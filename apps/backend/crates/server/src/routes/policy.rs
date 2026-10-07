//! `/api/policy/*` — what the site said, against what actually happened.
//!
//! Every server log holds the answer to "did that crawler do what I asked?" and
//! no analytics tool reads it. These three endpoints are the join: the site's
//! own `robots.txt` and `llms.txt` on one side (fetched by the
//! `crawl_site_policy` job and stored verbatim), the agent trace on the other.
//!
//! ## The report is an accusation, so it is built like one
//!
//! Three restraints run through this module and none of them is negotiable.
//!
//! - **A violation is a fetch of a disallowed path, and nothing else.** Not an
//!   agent the owner dislikes, not an operator who declines to promise it reads
//!   `robots.txt`, not an unverified request. `AgentDef::respects_robots` is
//!   reported next to the behaviour so a reader can see whether the operator
//!   ever made the promise, but it never decides the verdict.
//! - **An unverified address is never a violation.** The `verified` column
//!   separates "OpenAI fetched this" from "something sent OpenAI's product
//!   token". Only the first supports the word, and
//!   [`Compliance`](super::agents::Compliance) is where that rule lives.
//! - **Unknown is a real answer.** A site with no `robots.txt`, or one whose
//!   file never reaches a given agent, has expressed no policy. Reporting that
//!   as "allowed" would put words in the owner's mouth.
//!
//! ## Why the raw counts stay on every row
//!
//! `violations` counts every disallowed fetch and `verified_violations` counts
//! the subset Micaforge can stand behind. Both are on the wire because the
//! judgement between them is ours, and an operator who reads it differently is
//! entitled to the two numbers it was made from.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_agents::catalog::{AgentDef, CATALOG};
use micaforge_agents::{policy as agent_policy, LlmsTxt, PolicyReport, Robots};
use micaforge_core::{Error, QueryEnvelope, Table};
use micaforge_query::exec::{fetch_all, fetch_optional, ms_to_opt_dt};
use micaforge_query::sql::{millis, scope_predicate};
use micaforge_store::models::catalog as policy_store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::agents::{json_body, observations, parsed_policy, same_site, Compliance, Observation};
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::response::{ok, ok_meta};
use crate::routes::orgs::{require_at_least, MEMBER};
use crate::state::AppState;

/// How long an operator must wait between manual policy re-reads.
///
/// Long enough that a stuck client cannot turn "refresh" into a crawl loop
/// against somebody else's server, short enough that fixing a typo in
/// `robots.txt` and checking the result is not a chore. `PRODUCT.md` is explicit
/// that this tool must not crawl its customers rudely, and the refetch button is
/// the one place a person can make it do so.
const MIN_REFETCH_SECS: i64 = 60;

/// The three policy endpoints.
///
/// Absolute paths, merged at the root. See the note on [`super::agents::router`].
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/policy", get(policy))
        .route("/api/policy/violations", get(violations))
        .route("/api/policy/refetch", post(refetch))
}

// ---------------------------------------------------------------------------
// GET /api/policy
// ---------------------------------------------------------------------------

/// A site's `robots.txt` as fetched and as parsed.
#[derive(Debug, Clone, Serialize)]
struct RobotsView {
    /// False when the site has never been crawled, or served no file.
    present: bool,
    /// The file verbatim. Kept on the wire because a violation report has to be
    /// auditable against the exact bytes it was decided from.
    text: Option<String>,
    /// True when the file carried no group at all, so it restricts nobody.
    empty: bool,
    /// The parsed groups, in document order.
    groups: Value,
    /// Every `Sitemap:` line. File-global, not part of any group.
    sitemaps: Vec<String>,
    /// Tokens the file names that no known crawler sends. Usually a typo, and a
    /// rule written for a token nobody sends is a rule that does nothing while
    /// the owner believes it is protecting them.
    unrecognised_tokens: Vec<String>,
}

/// A site's `llms.txt` as fetched and as parsed.
#[derive(Debug, Clone, Serialize)]
struct LlmsView {
    /// False when the site serves no `llms.txt`. Distinct from an empty one:
    /// only the second is a decision the owner made.
    present: bool,
    /// The file verbatim.
    text: Option<String>,
    /// The `H1`.
    title: Option<String>,
    /// The blockquote summary under the title.
    summary: Option<String>,
    /// How many pages the file offers.
    link_count: usize,
    /// The parsed sections and their links.
    sections: Value,
}

/// One agent's row on the policy screen.
///
/// The four-tuple the contract names — `allowed`, `listed`, `observed_fetches`,
/// `violations` — is [`PolicyReport`], flattened in verbatim rather than
/// re-spelled, so this endpoint and the agents crate cannot drift about what
/// those four words mean. Everything else on the row is context for reading
/// them.
#[derive(Debug, Clone, Serialize)]
struct PolicyAgentRow {
    /// `agent`, `allowed`, `listed_in_llms`, `observed_fetches`, `violations`.
    #[serde(flatten)]
    report: PolicyReport,
    /// The operator's own spelling of the name.
    display_name: &'static str,
    /// The company behind it.
    operator: &'static str,
    /// What the fetch is for.
    purpose: &'static str,
    /// Where the operator documents it.
    docs_url: Option<&'static str>,
    /// Whether the operator *states* it honours `robots.txt`. A claim, recorded
    /// beside the behaviour and never used to decide it.
    respects_robots: bool,
    /// The token to write in `robots.txt` for this agent.
    robots_token: &'static str,
    /// True when the file names this token explicitly rather than covering it
    /// through the `*` group.
    robots_rule_named: bool,
    /// `Crawl-delay` in force for this agent, when the file sets one.
    crawl_delay: Option<f32>,
    /// Fetches that verified against the operator's published addresses.
    verified_fetches: u64,
    /// Disallowed fetches that verified. The only number that supports the word
    /// "violation", and the reason `violations` alone is not the verdict.
    verified_violations: u64,
    /// Fetches that hit a path the site's `llms.txt` offers.
    llms_listed_fetches: u64,
    /// Most recent fetch in the window.
    last_seen: Option<DateTime<Utc>>,
    /// The reading.
    compliance: Compliance,
}

/// The parsed policy, plus per-agent stated-versus-observed.
async fn policy(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;

    let stored = policy_store::get_policy(&st.pg, access.site.id).await?;
    let (robots, llms) = parsed_policy(stored.as_ref());
    let observed = observations(&st.ch, &env).await?;

    let mut agents: Vec<PolicyAgentRow> = CATALOG
        .iter()
        .map(|def| agent_row(def, &robots, llms.as_ref(), observed.get(def.slug)))
        .collect();

    // The rows worth reading first are the ones where stated and observed
    // disagree; then the agents that came at all; then the long tail nobody has
    // seen. Sorting by name would bury the finding under `anthropic-ai`.
    agents.sort_by(|a, b| {
        severity(a.compliance)
            .cmp(&severity(b.compliance))
            .then_with(|| b.report.observed_fetches.cmp(&a.report.observed_fetches))
            .then_with(|| a.report.agent.cmp(&b.report.agent))
    });

    let unaddressed: Vec<&'static str> = agent_policy::unaddressed_agents(&robots)
        .iter()
        .map(|def| def.slug)
        .collect();

    let body = json!({
        "fetched_at": stored.as_ref().and_then(|p| p.fetched_at),
        "robots": robots_view(stored.as_ref().and_then(|p| p.robots_txt.clone()), &robots)?,
        "llms": llms_view(stored.as_ref().and_then(|p| p.llms_txt.clone()), llms.as_ref())?,
        "agents": json_body(&agents)?,
        // Agents the owner has never written a named rule for. Not a fault: a
        // list of the decisions still open.
        "unaddressed_agents": unaddressed,
    });

    let meta = json!({
        "rows": agents.len(),
        "in_breach": count_of(&agents, Compliance::Violation),
        "unverified_breach": count_of(&agents, Compliance::Unverified),
        "policy_fetched": stored.as_ref().and_then(|p| p.fetched_at).is_some(),
    });

    Ok(ok_meta(body, meta))
}

/// Sort weight: the disagreements first.
fn severity(c: Compliance) -> u8 {
    match c {
        Compliance::Violation => 0,
        Compliance::Unverified => 1,
        Compliance::Compliant => 2,
        Compliance::Unstated => 3,
        Compliance::NotSeen => 4,
    }
}

/// How many rows read a given way, for the meta counts.
fn count_of(rows: &[PolicyAgentRow], want: Compliance) -> usize {
    rows.iter().filter(|r| r.compliance == want).count()
}

/// Build one agent's stated-versus-observed row.
fn agent_row(
    def: &'static AgentDef,
    robots: &Robots,
    llms: Option<&LlmsTxt>,
    observed: Option<&Observation>,
) -> PolicyAgentRow {
    let empty = Observation::default();
    let o = observed.unwrap_or(&empty);

    // The site root is the only path every site has, so it is where a
    // site-level "does this file allow this agent" question is asked. The
    // per-path truth is recorded on each fetch and summarised by the counts.
    let (allowed, _) = agent_policy::evaluate(robots, llms, def, "/");

    // `llms.txt` indexes pages, not agents, so the only per-agent reading of
    // "listed" that carries information is whether this agent read anything the
    // site actually offers.
    let listed = o.llms_listed_fetches > 0;

    PolicyAgentRow {
        report: PolicyReport::new(def.slug, allowed, listed)
            .with_observations(o.fetches, o.disallowed_fetches),
        display_name: def.display_name,
        operator: def.operator,
        purpose: def.purpose.as_str(),
        docs_url: def.docs_url,
        respects_robots: def.respects_robots,
        robots_token: def.robots_token(),
        robots_rule_named: robots.names(def.robots_token()),
        crawl_delay: robots.crawl_delay(def.robots_token()),
        verified_fetches: o.verified_fetches,
        verified_violations: o.verified_disallowed_fetches,
        llms_listed_fetches: o.llms_listed_fetches,
        last_seen: ms_to_opt_dt(o.last_seen_ms),
        compliance: Compliance::read(o),
    }
}

/// Present the `robots.txt` half.
fn robots_view(text: Option<String>, robots: &Robots) -> micaforge_core::Result<Value> {
    let view = RobotsView {
        present: text.is_some(),
        empty: robots.is_empty(),
        groups: json_body(robots.groups())?,
        sitemaps: robots.sitemaps.clone(),
        unrecognised_tokens: agent_policy::unrecognised_tokens(robots),
        text,
    };
    json_body(&view)
}

/// Present the `llms.txt` half.
fn llms_view(text: Option<String>, llms: Option<&LlmsTxt>) -> micaforge_core::Result<Value> {
    let view = LlmsView {
        present: llms.is_some(),
        title: llms.and_then(|l| l.title.clone()),
        summary: llms.and_then(|l| l.summary.clone()),
        link_count: llms.map(LlmsTxt::link_count).unwrap_or(0),
        sections: match llms {
            Some(l) => json_body(&l.sections)?,
            None => Value::Array(Vec::new()),
        },
        text,
    };
    json_body(&view)
}

// ---------------------------------------------------------------------------
// GET /api/policy/violations
// ---------------------------------------------------------------------------

/// One agent, one path it was told not to fetch, and what it did anyway.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ViolationRow {
    /// Catalog slug.
    pub agent: String,
    /// The operator's own spelling, when the slug is one Micaforge knows.
    pub display_name: String,
    /// The company behind it.
    pub operator: String,
    /// Where the operator documents this crawler.
    pub docs_url: Option<&'static str>,
    /// Whether the operator states it honours `robots.txt`. Context, not the
    /// verdict: a crawler whose operator never made the promise still shows up
    /// here, because the report is about what the site asked for.
    pub respects_robots: bool,
    /// The path that was fetched despite being disallowed.
    pub path: String,
    /// How many times.
    pub fetches: u64,
    /// Of those, how many came from an address the operator publishes.
    pub verified_fetches: u64,
    /// Distinct source addresses, as rotating salted digests. Never an address.
    pub unique_sources: u64,
    /// First offending fetch in the window.
    pub first_seen: Option<DateTime<Utc>>,
    /// Most recent offending fetch in the window.
    pub last_seen: Option<DateTime<Utc>>,
    /// [`Compliance::Violation`] when at least one of these fetches verified,
    /// [`Compliance::Unverified`] otherwise. Never anything else: every row here
    /// is already a disallowed fetch.
    pub compliance: Compliance,
}

/// Site-level totals across every offending fetch, not just the page returned.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(super) struct ViolationsSummary {
    /// Disallowed fetches in the window.
    pub fetches: u64,
    /// Of those, how many verified.
    pub verified_fetches: u64,
    /// Distinct agents involved.
    pub agents: u64,
    /// Distinct paths involved.
    pub paths: u64,
}

/// What `GET /api/policy/violations` answers, and what the MCP tool returns.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ViolationsReport {
    /// Totals over the whole window.
    pub summary: ViolationsSummary,
    /// One row per agent and path, busiest first.
    pub rows: Vec<ViolationRow>,
}

/// Raw row from ClickHouse. Field order is the `SELECT` list order and may not
/// be reordered: `RowBinary` decodes positionally.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct ViolationRawRow {
    agent_id: String,
    pathname: String,
    fetches: u64,
    verified_fetches: u64,
    unique_sources: u64,
    first_seen_ms: i64,
    last_seen_ms: i64,
}

/// Raw totals row. Same rule about field order.
#[derive(Debug, Default, clickhouse::Row, Deserialize)]
struct ViolationTotalsRow {
    fetches: u64,
    verified_fetches: u64,
    agents: u64,
    paths: u64,
}

/// Agents that fetched a path their own `robots.txt` rule disallowed.
///
/// Shared with the MCP tool of the same name, so an agent asking the question
/// and a person reading the screen get the same rows from the same statement.
///
/// The predicate is `robots_allowed = 0` — the value recorded on the fetch
/// itself, at the moment it happened, against the file that was in force then.
/// Re-deciding it now against today's `robots.txt` would report violations of a
/// rule that did not exist yet.
pub(super) async fn violations_report(
    st: &AppState,
    env: &QueryEnvelope,
) -> micaforge_core::Result<ViolationsReport> {
    let (scope, params) = scope_predicate(env, Table::AgentEvents)?;

    let totals_sql = format!(
        "SELECT
  toUInt64(count()) AS fetches,
  toUInt64(countIf(verified = 1)) AS verified_fetches,
  toUInt64(uniq(agent_id)) AS agents,
  toUInt64(uniq(pathname)) AS paths
FROM agent_events
WHERE {scope} AND robots_allowed = 0"
    );
    let summary = fetch_optional::<ViolationTotalsRow>(
        &st.ch,
        "policy_violation_totals",
        &totals_sql,
        &params,
    )
    .await?
    .unwrap_or_default();

    let rows_sql = format!(
        "SELECT
  toString(agent_id) AS agent_id,
  pathname,
  toUInt64(count()) AS fetches,
  toUInt64(countIf(verified = 1)) AS verified_fetches,
  toUInt64(uniq(ip_hash)) AS unique_sources,
  {first} AS first_seen_ms,
  {last} AS last_seen_ms
FROM agent_events
WHERE {scope} AND robots_allowed = 0
GROUP BY agent_id, pathname
ORDER BY fetches DESC, agent_id ASC, pathname ASC
LIMIT {limit} OFFSET {offset}",
        first = millis("min(timestamp)"),
        last = millis("max(timestamp)"),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let raw = fetch_all::<ViolationRawRow>(&st.ch, "policy_violations", &rows_sql, &params).await?;

    let rows = raw
        .into_iter()
        .map(|r| {
            let def = micaforge_agents::catalog::find_by_slug(&r.agent_id);
            ViolationRow {
                display_name: def
                    .map(|d| d.display_name.to_string())
                    // A slug with no catalog entry is an agent this build does
                    // not know rather than a bad row, so it is shown under the
                    // name the trace recorded instead of being dropped.
                    .unwrap_or_else(|| r.agent_id.clone()),
                operator: def.map(|d| d.operator.to_string()).unwrap_or_default(),
                docs_url: def.and_then(|d| d.docs_url),
                respects_robots: def.map(|d| d.respects_robots).unwrap_or(false),
                compliance: if r.verified_fetches > 0 {
                    Compliance::Violation
                } else {
                    Compliance::Unverified
                },
                agent: r.agent_id,
                path: r.pathname,
                fetches: r.fetches,
                verified_fetches: r.verified_fetches,
                unique_sources: r.unique_sources,
                first_seen: ms_to_opt_dt(r.first_seen_ms),
                last_seen: ms_to_opt_dt(r.last_seen_ms),
            }
        })
        .collect();

    Ok(ViolationsReport {
        summary: ViolationsSummary {
            fetches: summary.fetches,
            verified_fetches: summary.verified_fetches,
            agents: summary.agents,
            paths: summary.paths,
        },
        rows,
    })
}

/// The violations table.
async fn violations(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let report = violations_report(&st, &env).await?;

    let verified_rows = report
        .rows
        .iter()
        .filter(|r| r.compliance == Compliance::Violation)
        .count();
    let meta = json!({
        "rows": report.rows.len(),
        "verified_rows": verified_rows,
        "unverified_rows": report.rows.len() - verified_rows,
    });

    Ok(ok_meta(json_body(&report)?, meta))
}

// ---------------------------------------------------------------------------
// POST /api/policy/refetch
// ---------------------------------------------------------------------------

/// Re-read `robots.txt` and `llms.txt` now.
///
/// Answers `202` with the queue id rather than doing the work inline. The crawl
/// goes out to somebody else's server, and an HTTP handler that waits on a third
/// party's TLS handshake is a handler that can be held open by it.
///
/// Two guards, both about politeness rather than about security:
///
/// - a viewer may read the audit but not cause a request to leave the building;
/// - a site whose policy was read in the last [`MIN_REFETCH_SECS`] is answered
///   `429` with a `Retry-After`, so a stuck client cannot turn a button into a
///   crawl loop against a customer's origin.
async fn refetch(
    State(st): State<AppState>,
    access: SiteAccess,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    // A viewer may read the whole audit; only a member and up may cause a
    // request to leave the building.
    require_at_least(access.role.as_str(), MEMBER)?;

    let stored = policy_store::get_policy(&st.pg, access.site.id).await?;
    if let Some(fetched_at) = stored.as_ref().and_then(|p| p.fetched_at) {
        let age = (Utc::now() - fetched_at).num_seconds();
        if age < MIN_REFETCH_SECS {
            return Err(Error::RateLimited {
                retry_after: (MIN_REFETCH_SECS - age).max(1) as u64,
            }
            .into());
        }
    }

    let payload = micaforge_jobs::crawl_site_policy::Payload {
        site: Some(access.site.id),
    };
    let job = micaforge_jobs::enqueue_now(&st.pg, micaforge_jobs::Kind::CrawlSitePolicy, &payload)
        .await?;

    tracing::info!(
        site = %access.site.domain,
        job = %job,
        "policy refetch queued by an operator"
    );

    let body = ok(json!({
        "job_id": job,
        "kind": micaforge_jobs::Kind::CrawlSitePolicy.as_str(),
        "status": "queued",
    }));
    Ok((StatusCode::ACCEPTED, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disagreements_sort_to_the_top() {
        let mut order = [
            Compliance::NotSeen,
            Compliance::Compliant,
            Compliance::Violation,
            Compliance::Unstated,
            Compliance::Unverified,
        ];
        order.sort_by_key(|c| severity(*c));
        assert_eq!(order[0], Compliance::Violation);
        assert_eq!(order[1], Compliance::Unverified);
        assert_eq!(order[4], Compliance::NotSeen);
    }

    /// The four-tuple the contract names must keep its own spelling on the wire.
    #[test]
    fn the_contract_tuple_is_flattened_in_verbatim() {
        let robots = micaforge_agents::robots::parse("User-agent: GPTBot\nDisallow: /\n");
        let gptbot = micaforge_agents::catalog::find_by_slug("gptbot").unwrap();
        let observed = Observation {
            agent_id: "gptbot".to_string(),
            fetches: 41,
            disallowed_fetches: 41,
            verified_disallowed_fetches: 0,
            ..Observation::default()
        };

        let row = agent_row(gptbot, &robots, None, Some(&observed));
        let json = serde_json::to_value(&row).unwrap();

        assert_eq!(json["agent"], "gptbot");
        assert_eq!(json["allowed"], false);
        assert_eq!(json["observed_fetches"], 41);
        assert_eq!(json["violations"], 41);
        // Forty-one disallowed fetches, none of them verified: still not an
        // accusation.
        assert_eq!(json["compliance"], "unverified");
        assert_eq!(json["verified_violations"], 0);
    }
}
