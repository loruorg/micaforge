//! `agent_catalog` and `site_policy` — the two halves of the policy audit.
//! `docs/CONTRACT.md` §2.
//!
//! The catalog is what Micaforge knows about named non-human clients: how to
//! recognise one from its user-agent, how to verify the claim against published
//! IP ranges or reverse DNS, and what the operator says the crawl is for. It is
//! install-wide rather than per-site, because GPTBot is GPTBot everywhere.
//!
//! `site_policy` is the other half: what a site *says* in its own `robots.txt`
//! and `llms.txt`. Holding both lets `GET /api/policy/violations` answer the
//! question no other analytics tool can: which agents fetched a path the site
//! told them not to.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A known non-human client.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentCatalogEntry {
    /// Stable slug, the primary key: `gptbot`, `claudebot`, `perplexitybot`.
    /// Written to `agent_events.agent_id`.
    pub slug: String,
    /// Human-readable name for the dashboard.
    pub display_name: String,
    /// The company behind it: `openai`, `anthropic`, `perplexity`.
    pub operator: String,
    /// `training` | `rag` | `search` | `preview` | `eval` | `unknown`.
    pub purpose: String,
    /// Substrings or regexes matched against the user-agent.
    pub ua_patterns: Json,
    /// CIDR ranges the operator publishes, for verification.
    pub ip_ranges: Json,
    /// Reverse-DNS suffixes, for operators who verify that way instead.
    pub rdns_suffixes: Json,
    /// Where the operator documents this crawler, shown in the dashboard so an
    /// operator can check the claim rather than take our word for it.
    pub docs_url: Option<String>,
    /// Whether the operator states that it honours `robots.txt`.
    ///
    /// A claim, recorded so it can be compared against
    /// `GET /api/policy/violations`. This column is what the operator said; the
    /// event stream is what they did.
    pub respects_robots: bool,
    /// When this entry was last refreshed.
    pub updated_at: DateTime<Utc>,
}

/// A site's own published crawl policy, as fetched.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SitePolicy {
    /// Site, by UUID.
    pub site_id: Uuid,
    /// `robots.txt` verbatim.
    pub robots_txt: Option<String>,
    /// `llms.txt` verbatim.
    pub llms_txt: Option<String>,
    /// When the two were last read.
    pub fetched_at: Option<DateTime<Utc>>,
    /// The parsed rules, so the audit does not re-parse on every request.
    pub parsed: Json,
}

const CATALOG_COLS: &str = "slug, display_name, operator, purpose, ua_patterns, ip_ranges, \
     rdns_suffixes, docs_url, respects_robots, updated_at";
const POLICY_COLS: &str = "site_id, robots_txt, llms_txt, fetched_at, parsed";

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Insert or refresh a catalog entry.
///
/// An upsert because the catalog ships with the build and is re-seeded on every
/// boot: a new release that adds a crawler or corrects an IP range has to reach
/// existing installs without an operator running anything.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_agent(
    pool: &PgPool,
    slug: &str,
    display_name: &str,
    operator: &str,
    purpose: &str,
    ua_patterns: &Json,
    ip_ranges: &Json,
    rdns_suffixes: &Json,
    docs_url: Option<&str>,
    respects_robots: bool,
) -> Result<AgentCatalogEntry> {
    let row = sqlx::query_as::<_, AgentCatalogEntry>(&format!(
        "INSERT INTO agent_catalog
           (slug, display_name, operator, purpose, ua_patterns, ip_ranges,
            rdns_suffixes, docs_url, respects_robots, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
         ON CONFLICT (slug) DO UPDATE SET
           display_name = EXCLUDED.display_name,
           operator = EXCLUDED.operator,
           purpose = EXCLUDED.purpose,
           ua_patterns = EXCLUDED.ua_patterns,
           ip_ranges = EXCLUDED.ip_ranges,
           rdns_suffixes = EXCLUDED.rdns_suffixes,
           docs_url = EXCLUDED.docs_url,
           respects_robots = EXCLUDED.respects_robots,
           updated_at = now()
         RETURNING {CATALOG_COLS}"
    ))
    .bind(slug)
    .bind(display_name)
    .bind(operator)
    .bind(purpose)
    .bind(ua_patterns)
    .bind(ip_ranges)
    .bind(rdns_suffixes)
    .bind(docs_url)
    .bind(respects_robots)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch one catalog entry.
pub async fn get_agent(pool: &PgPool, slug: &str) -> Result<AgentCatalogEntry> {
    let row = sqlx::query_as::<_, AgentCatalogEntry>(&format!(
        "SELECT {CATALOG_COLS} FROM agent_catalog WHERE slug = $1"
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "agent")
}

/// The whole catalog.
///
/// The classifier loads this once at boot and holds it in memory: matching a
/// user-agent against every pattern is a per-request operation on the ingest
/// path, and it cannot be a database round trip.
pub async fn list_agents(pool: &PgPool) -> Result<Vec<AgentCatalogEntry>> {
    let rows = sqlx::query_as::<_, AgentCatalogEntry>(&format!(
        "SELECT {CATALOG_COLS} FROM agent_catalog ORDER BY operator ASC, slug ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Every entry for one operator.
pub async fn list_agents_by_operator(
    pool: &PgPool,
    operator: &str,
) -> Result<Vec<AgentCatalogEntry>> {
    let rows = sqlx::query_as::<_, AgentCatalogEntry>(&format!(
        "SELECT {CATALOG_COLS} FROM agent_catalog WHERE operator = $1 ORDER BY slug ASC"
    ))
    .bind(operator)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Remove a catalog entry.
pub async fn delete_agent(pool: &PgPool, slug: &str) -> Result<()> {
    sqlx::query("DELETE FROM agent_catalog WHERE slug = $1")
        .bind(slug)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Site policy
// ---------------------------------------------------------------------------

/// Store a freshly fetched `robots.txt` and `llms.txt`.
pub async fn upsert_policy(
    pool: &PgPool,
    site_id: Uuid,
    robots_txt: Option<&str>,
    llms_txt: Option<&str>,
    parsed: &Json,
) -> Result<SitePolicy> {
    let row = sqlx::query_as::<_, SitePolicy>(&format!(
        "INSERT INTO site_policy (site_id, robots_txt, llms_txt, fetched_at, parsed)
         VALUES ($1, $2, $3, now(), $4)
         ON CONFLICT (site_id) DO UPDATE SET
           robots_txt = EXCLUDED.robots_txt,
           llms_txt = EXCLUDED.llms_txt,
           fetched_at = now(),
           parsed = EXCLUDED.parsed
         RETURNING {POLICY_COLS}"
    ))
    .bind(site_id)
    .bind(robots_txt)
    .bind(llms_txt)
    .bind(parsed)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// A site's policy, if it has ever been fetched.
///
/// `Option`, not a `404`: a site that has never been crawled has no policy row,
/// and that is a normal state the policy screen renders as "not fetched yet".
pub async fn get_policy(pool: &PgPool, site_id: Uuid) -> Result<Option<SitePolicy>> {
    let row = sqlx::query_as::<_, SitePolicy>(&format!(
        "SELECT {POLICY_COLS} FROM site_policy WHERE site_id = $1"
    ))
    .bind(site_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Policies that have not been refreshed since `before`, oldest first.
///
/// The crawler's work queue. A published policy changes without telling anyone,
/// so an audit against a stale copy reports violations that are not real.
pub async fn stale_policies(
    pool: &PgPool,
    before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<SitePolicy>> {
    let rows = sqlx::query_as::<_, SitePolicy>(&format!(
        "SELECT {POLICY_COLS} FROM site_policy
         WHERE fetched_at IS NULL OR fetched_at < $1
         ORDER BY fetched_at ASC NULLS FIRST
         LIMIT $2"
    ))
    .bind(before)
    .bind(limit.max(1))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Forget a site's policy.
pub async fn delete_policy(pool: &PgPool, site_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM site_policy WHERE site_id = $1")
        .bind(site_id)
        .execute(pool)
        .await?;
    Ok(())
}
