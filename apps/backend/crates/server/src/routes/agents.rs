//! `GET /api/agents/*` — the account of who read this site that was not a
//! person.
//!
//! Five reads over `agent_events` (`docs/CONTRACT.md` §1.2), the table that
//! holds one row per fetch Micaforge could *name*. Four of them are thin: they
//! resolve the envelope, hand it to [`micaforge_query::agents_q`], and wrap the
//! result. The fifth, `/catalog`, is the one that does real work here, because
//! it is the only report in the product that needs three sources at once — the
//! compiled-in agent register, the site's own published policy, and the event
//! stream — and none of those three knows about the other two.
//!
//! ## Why this module owns the compliance verdict
//!
//! `agent_events.robots_allowed` records what the site's rules said about the
//! path at the moment it was fetched, and `agent_events.verified` records
//! whether the source address matched what the operator publishes. Reading a
//! disallowed fetch as a *violation* requires both columns, and getting that
//! wrong is the most damaging thing this product could do: it would put an
//! operator's name next to an accusation Micaforge cannot support. So the
//! verdict is computed in one place, [`Compliance::read`], from one struct, and
//! it never says `violation` for a fetch nobody verified. An unverified fetch of
//! a disallowed path is [`Compliance::Unverified`] — somebody sent that product
//! token, and Micaforge is not willing to say who.
//!
//! ## Shared with the neighbouring modules
//!
//! `policy.rs`, `gap.rs` and `mcp.rs` all read this module rather than growing
//! their own copy of the observation query, the verdict, or the policy parse.
//! `/api/agents/catalog` and `/api/policy` are two presentations of the same
//! join and they must never disagree about whether an agent is in breach.

use std::collections::BTreeMap;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_agents::catalog::{AgentDef, CATALOG};
use micaforge_agents::{llms_txt, policy as agent_policy, robots, LlmsTxt, Robots};
use micaforge_core::{Audience, Dimension, Error, QueryEnvelope, Table};
use micaforge_query::agents_q;
use micaforge_query::exec::{fetch_all, ms_to_opt_dt};
use micaforge_query::sql::{millis, scope_predicate};
use micaforge_store::models::catalog::SitePolicy;
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess, SiteOrShare};
use crate::response::{ok, ok_meta};
use crate::state::AppState;

/// The five agent reads.
///
/// Paths are absolute because the wedge does not sit under one prefix —
/// `gap.rs` serves both `/api/citation-gap` and `/api/content/decay` — so every
/// router in this group is written to be `merge`d at the root rather than
/// `nest`ed under a shared mount point.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/agents/overview", get(overview))
        .route("/api/agents/breakdown", get(breakdown))
        .route("/api/agents/timeseries", get(timeseries))
        .route("/api/agents/coverage", get(coverage))
        .route("/api/agents/catalog", get(catalog))
}

// ---------------------------------------------------------------------------
// Shared helpers, used by the other wedge modules too
// ---------------------------------------------------------------------------

/// Serialise a report body into the value the response envelope wraps.
///
/// A dedicated error rather than `?` through `From<serde_json::Error>`: that
/// conversion reads a failure as the caller's malformed JSON and answers `400`,
/// which is exactly wrong here. Nothing this crate serialises comes off the
/// wire, so a failure is ours and is reported as ours.
pub(super) fn json_body<T: Serialize + ?Sized>(value: &T) -> micaforge_core::Result<Value> {
    serde_json::to_value(value).map_err(|e| Error::internal(format!("serialising report: {e}")))
}

/// Refuse a request whose envelope names a different site from the one the
/// caller was authorised for.
///
/// Both extractors read `?site=` today, so this cannot fire. It is here because
/// it guards a tenant boundary: if the envelope ever learns a second way to
/// select a site (a saved view, a default), the authorisation check must not
/// silently stop covering it. A cross-tenant read is the one bug worth spending
/// a branch on.
pub(super) fn same_site(access: &SiteAccess, env: &QueryEnvelope) -> micaforge_core::Result<()> {
    if access.site_id != env.site {
        return Err(Error::Forbidden(
            "the query envelope names a different site from the one authorised".to_string(),
        ));
    }
    Ok(())
}

/// The site's stored `robots.txt` and `llms.txt`, re-parsed from the text.
///
/// The stored `parsed` jsonb is not read. `site_policy` keeps both files
/// verbatim precisely so that an audit can be repeated against the exact bytes
/// that were in force, and re-parsing costs microseconds on a document that is
/// capped at a few kilobytes. It also means the report cannot drift from the
/// parser as the parser improves.
///
/// A site that has never been crawled parses as an empty `robots.txt`, which
/// expresses no rule about anybody — not "allowed".
pub(super) fn parsed_policy(policy: Option<&SitePolicy>) -> (Robots, Option<LlmsTxt>) {
    let robots_text = policy.and_then(|p| p.robots_txt.as_deref()).unwrap_or("");
    let llms = policy
        .and_then(|p| p.llms_txt.as_deref())
        .map(llms_txt::parse);
    (robots::parse(robots_text), llms)
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

/// What one agent actually did in the window.
///
/// The field order is the `SELECT` list order in [`observations`] and may not be
/// reordered: the driver decodes `RowBinary` positionally, so a swapped pair of
/// `u64` columns would decode cleanly into the wrong numbers.
#[derive(Debug, Clone, Default, clickhouse::Row, Serialize, Deserialize)]
pub(super) struct Observation {
    /// The catalog slug the fetches were attributed to.
    pub agent_id: String,
    /// Fetches in the window.
    pub fetches: u64,
    /// Of those, how many came from an address that matched the operator's
    /// published ranges or reverse DNS.
    pub verified_fetches: u64,
    /// Distinct paths touched.
    pub unique_paths: u64,
    /// Bytes served. The most literal answer to "how much was taken".
    pub bytes: u64,
    /// Fetches of a path the site's rules allowed this agent.
    pub allowed_fetches: u64,
    /// Fetches of a path the site's rules disallowed this agent.
    pub disallowed_fetches: u64,
    /// Of those, how many were from a verified address. This is the only number
    /// that may become a violation.
    pub verified_disallowed_fetches: u64,
    /// Fetches the site's rules said nothing about.
    pub unstated_fetches: u64,
    /// Fetches of a path the site's own `llms.txt` offers.
    pub llms_listed_fetches: u64,
    /// Conditional requests (`If-None-Match` / `If-Modified-Since`): re-crawl
    /// checks rather than fresh reads.
    pub conditional_fetches: u64,
    /// First fetch in the window, epoch milliseconds.
    pub first_seen_ms: i64,
    /// Most recent fetch in the window, epoch milliseconds.
    pub last_seen_ms: i64,
}

/// Every named agent seen in the window, keyed by catalog slug.
///
/// One pass over `agent_events`. The alternative — a filtered aggregate per
/// question — would scan the same partitions four times to answer one screen,
/// and the four answers would be taken from four different moments of a table
/// that is being appended to while they run.
pub(super) async fn observations(
    ch: &Ch,
    env: &QueryEnvelope,
) -> micaforge_core::Result<BTreeMap<String, Observation>> {
    let (scope, params) = scope_predicate(env, Table::AgentEvents)?;

    let sql = format!(
        "SELECT
  toString(agent_id) AS agent_id,
  toUInt64(count()) AS fetches,
  toUInt64(countIf(verified = 1)) AS verified_fetches,
  toUInt64(uniq(pathname)) AS unique_paths,
  toUInt64(sum(bytes)) AS bytes,
  toUInt64(countIf(robots_allowed = 1)) AS allowed_fetches,
  toUInt64(countIf(robots_allowed = 0)) AS disallowed_fetches,
  toUInt64(countIf(robots_allowed = 0 AND verified = 1)) AS verified_disallowed_fetches,
  toUInt64(countIf(robots_allowed = -1)) AS unstated_fetches,
  toUInt64(countIf(llms_listed = 1)) AS llms_listed_fetches,
  toUInt64(countIf(conditional = 1)) AS conditional_fetches,
  {first} AS first_seen_ms,
  {last} AS last_seen_ms
FROM agent_events
WHERE {scope}
GROUP BY agent_id",
        first = millis("min(timestamp)"),
        last = millis("max(timestamp)"),
    );

    let rows = fetch_all::<Observation>(ch, "agents_observations", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.agent_id.clone(), row))
        .collect())
}

// ---------------------------------------------------------------------------
// The verdict
// ---------------------------------------------------------------------------

/// What the observed fetches say about an agent against the site's own rules.
///
/// The ordering of the arms in [`Compliance::read`] is the product judgement,
/// and it is deliberately conservative in one direction: verification gates the
/// accusation, and nothing else does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Compliance {
    /// Nothing from this agent was seen in the window. Not a pass and not a
    /// failure: an absence.
    NotSeen,
    /// Fetches were seen, and the site's files express no rule that reaches this
    /// agent. The site has not made a decision, so there is nothing to comply
    /// with.
    Unstated,
    /// Every fetch that the site had a rule for hit a path it allows.
    Compliant,
    /// A disallowed path was fetched, but not once from an address the operator
    /// publishes. Somebody sent this product token; Micaforge will not say who.
    Unverified,
    /// A verified fetch hit a disallowed path. This is the only reading that is
    /// an accusation, and it is the only one backed by both columns.
    Violation,
}

impl Compliance {
    /// Read one agent's window.
    ///
    /// Takes the whole [`Observation`] rather than four counts, because four
    /// `u64` arguments in a row is a transposition waiting to happen and the
    /// transposition would swap "verified breach" for "unverified claim".
    pub(super) fn read(o: &Observation) -> Compliance {
        if o.fetches == 0 {
            Compliance::NotSeen
        } else if o.verified_disallowed_fetches > 0 {
            Compliance::Violation
        } else if o.disallowed_fetches > 0 {
            Compliance::Unverified
        } else if o.allowed_fetches > 0 {
            Compliance::Compliant
        } else {
            Compliance::Unstated
        }
    }
}

// ---------------------------------------------------------------------------
// GET /api/agents/overview
// ---------------------------------------------------------------------------

/// Fetches, unique agents, verified share, bytes, top operators.
async fn overview(
    State(st): State<AppState>,
    SiteOrShare(access): SiteOrShare,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let env = agent_envelope(env);
    let report = agents_q::overview(&st.ch, &env).await?;
    Ok(ok(json_body(&report)?))
}

/// The envelope an agent report is answered over.
///
/// The audience is forced rather than trusted. Every query in this module reads
/// `agent_events` by name, so a caller that passed `audience=human` would get
/// agent numbers under a human label; pinning it keeps the echoed meta honest
/// about which population the answer describes.
pub(super) fn agent_envelope(mut env: QueryEnvelope) -> QueryEnvelope {
    env.audience = Audience::Agent;
    env
}

// ---------------------------------------------------------------------------
// GET /api/agents/breakdown
// ---------------------------------------------------------------------------

/// `?dimension=` for the agent breakdown.
#[derive(Debug, Deserialize)]
struct BreakdownParams {
    /// Which column to rank by. Defaults to the agent itself, which is the
    /// question the screen opens with.
    #[serde(default)]
    dimension: Option<String>,
}

/// Ranked agent traffic by one dimension of the agent trace.
async fn breakdown(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<BreakdownParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let env = agent_envelope(env);
    let dimension = agent_dimension(params.dimension.as_deref())?;
    let rows = agents_q::breakdown(&st.ch, &env, &dimension).await?;
    let meta = json!({ "rows": rows.len(), "dimension": dimension.as_str() });
    Ok(ok_meta(json_body(&rows)?, meta))
}

/// Resolve a breakdown dimension the agent trace actually records.
///
/// Refused here rather than at ClickHouse. The driver's answer to an unknown
/// identifier is a paragraph of query text; the honest answer to "break agent
/// traffic down by browser" is that a crawler does not have one. The accepted
/// set is exactly the columns `agent_events` carries.
fn agent_dimension(raw: Option<&str>) -> micaforge_core::Result<Dimension> {
    let raw = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("agent_id");
    let dimension = Dimension::parse(raw)?;

    let recorded = matches!(
        dimension,
        Dimension::AgentId
            | Dimension::Operator
            | Dimension::Purpose
            | Dimension::Verified
            | Dimension::RobotsAllowed
            | Dimension::Status
            | Dimension::ContentType
            | Dimension::Hostname
            | Dimension::Pathname
            | Dimension::Querystring
            | Dimension::Country
            | Dimension::AsnOrg
    );

    if !recorded {
        return Err(Error::validation(
            "dimension",
            format!(
                "`{raw}` is not recorded on the agent trace; \
                 use agent_id, operator, purpose, verified, robots_allowed, status, \
                 content_type, hostname, pathname, querystring, country or asn_org"
            ),
        ));
    }
    Ok(dimension)
}

// ---------------------------------------------------------------------------
// GET /api/agents/timeseries
// ---------------------------------------------------------------------------

/// `?agent=` for the crawl-volume series.
#[derive(Debug, Deserialize)]
struct TimeseriesParams {
    /// One catalog slug, to narrow the series to a single crawler. Bound as a
    /// parameter downstream, never interpolated.
    #[serde(default)]
    agent: Option<String>,
    /// Split the record into one trace per value of this dimension:
    /// `agent_id`, `operator` or `purpose`. Mutually exclusive with `agent`.
    #[serde(default)]
    dimension: Option<String>,
}

/// Crawl volume over time, optionally for one agent.
async fn timeseries(
    State(st): State<AppState>,
    SiteOrShare(access): SiteOrShare,
    Envelope(env): Envelope,
    Query(params): Query<TimeseriesParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let env = agent_envelope(env);

    // An unknown slug is refused rather than answered with a flat zero line: a
    // chart of nothing looks identical to a chart of an agent that never came,
    // and only one of those is a finding.
    let agent = params
        .agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(slug) = agent {
        if micaforge_agents::catalog::find_by_slug(slug).is_none() {
            return Err(Error::validation(
                "agent",
                format!("`{slug}` is not a known agent; see /api/agents/catalog"),
            )
            .into());
        }
    }

    // With a dimension, the record is split so each operator writes its own pen;
    // without one it is a single trace of everything. The agent screen asks for
    // the split, because "who is crawling me, over time" is the question — a
    // single merged line cannot answer it.
    let dimension = params
        .dimension
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(dim) = dimension {
        if agent.is_some() {
            return Err(Error::validation(
                "dimension",
                "a dimension splits the record across agents, so it cannot be combined with `agent`",
            )
            .into());
        }
        let split = agents_q::timeseries_split(&st.ch, &env, dim).await?;
        return Ok(ok(json_body(&split)?));
    }

    let series = agents_q::timeseries(&st.ch, &env, agent).await?;
    Ok(ok(json_body(&series)?))
}

// ---------------------------------------------------------------------------
// GET /api/agents/coverage
// ---------------------------------------------------------------------------

/// Per URL: first crawl, last crawl, fetch count, freshness gap, which agents.
async fn coverage(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let env = agent_envelope(env);
    let rows = agents_q::coverage(&st.ch, &env).await?;

    // Uncrawled pages are the reason this report exists, so the count is on the
    // envelope rather than left for the client to derive from the page it got.
    let uncrawled = rows.iter().filter(|r| r.fetch_count == 0).count();
    let meta = json!({ "rows": rows.len(), "uncrawled": uncrawled });
    Ok(ok_meta(json_body(&rows)?, meta))
}

// ---------------------------------------------------------------------------
// GET /api/agents/catalog
// ---------------------------------------------------------------------------

/// One catalog entry, joined to this site's policy and this site's traffic.
#[derive(Debug, Clone, Serialize)]
struct CatalogRow {
    /// Stable identifier, and the value `agent_events.agent_id` carries.
    slug: &'static str,
    /// The operator's own spelling.
    display_name: &'static str,
    /// The company behind it.
    operator: &'static str,
    /// `training` | `rag` | `search` | `preview` | `eval` | `unknown`.
    purpose: &'static str,
    /// Where the operator documents this crawler, so a reader can check the
    /// claim rather than take ours.
    docs_url: Option<&'static str>,
    /// Whether the operator *states* it honours `robots.txt`. A recorded claim,
    /// never a measurement; `compliance` is the measurement.
    respects_robots: bool,
    /// Whether the operator publishes a machine-readable range document, which
    /// is what makes a fetch verifiable at all.
    publishes_ranges: bool,
    /// The token to write in `robots.txt` for this agent.
    robots_token: &'static str,
    /// True for the entries that exist only to be written in `robots.txt` and
    /// will never appear in a request (`Google-Extended` and its kin).
    robots_token_only: bool,
    /// Whether anything from this agent was seen in the window.
    seen: bool,
    /// Fetches in the window.
    fetches: u64,
    /// Of those, how many verified against the operator's published addresses.
    verified_fetches: u64,
    /// Distinct paths touched.
    unique_paths: u64,
    /// Bytes served.
    bytes: u64,
    /// Re-crawl checks rather than fresh reads.
    conditional_fetches: u64,
    /// First fetch in the window.
    first_seen: Option<DateTime<Utc>>,
    /// Most recent fetch in the window.
    last_seen: Option<DateTime<Utc>>,
    /// What this site's `robots.txt` says about this agent **at the site root**.
    /// `null` when the file expresses no rule that reaches it. Per-path answers
    /// are recorded on every fetch and summarised by the counts below.
    robots_allowed: Option<bool>,
    /// True when the file names this token explicitly rather than covering it
    /// through the `*` group. The difference is a decision the owner made.
    robots_rule_named: bool,
    /// `Crawl-delay` in force for this agent, when the file sets one.
    crawl_delay: Option<f32>,
    /// True when at least one of this agent's fetches hit a path the site's own
    /// `llms.txt` offers. `llms.txt` is an index of pages, not of agents, so
    /// this is the only reading of "listed" that means anything per agent.
    listed_in_llms: bool,
    /// Fetches that hit a path `llms.txt` offers.
    llms_listed_fetches: u64,
    /// Fetches of a path the site allows this agent.
    allowed_fetches: u64,
    /// Fetches of a path the site disallows this agent.
    disallowed_fetches: u64,
    /// Of those, how many came from a verified address. The column nobody else
    /// ships, and the only one that supports the word "violation".
    verified_disallowed_fetches: u64,
    /// The reading. See [`Compliance`].
    compliance: Compliance,
}

/// The known-agent catalog, merged with this site's policy and observations.
///
/// Three sources, joined here because no one of them can see the others: the
/// register is compiled in, the policy is a document the site publishes, and the
/// behaviour is in ClickHouse. The row an operator opens this screen for is the
/// one where the three disagree.
async fn catalog(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let env = agent_envelope(env);

    let observed = observations(&st.ch, &env).await?;
    let policy = micaforge_store::models::catalog::get_policy(&st.pg, access.site.id).await?;
    let (robots, llms) = parsed_policy(policy.as_ref());

    let mut rows: Vec<CatalogRow> = CATALOG
        .iter()
        .map(|def| catalog_row(def, &robots, llms.as_ref(), observed.get(def.slug)))
        .collect();

    // Busiest first, then alphabetically inside the long tail of agents that
    // have never come. A table sorted by a name nobody is looking for buries the
    // one row that matters.
    rows.sort_by(|a, b| {
        b.fetches
            .cmp(&a.fetches)
            .then_with(|| a.operator.cmp(b.operator))
            .then_with(|| a.slug.cmp(b.slug))
    });

    let meta = json!({
        "rows": rows.len(),
        "seen": rows.iter().filter(|r| r.seen).count(),
        "in_breach": rows
            .iter()
            .filter(|r| r.compliance == Compliance::Violation)
            .count(),
        "unverified_breach": rows
            .iter()
            .filter(|r| r.compliance == Compliance::Unverified)
            .count(),
        "policy_fetched_at": policy.as_ref().and_then(|p| p.fetched_at),
    });

    Ok(ok_meta(json_body(&rows)?, meta))
}

/// Build one row of the catalog table.
fn catalog_row(
    def: &'static AgentDef,
    robots: &Robots,
    llms: Option<&LlmsTxt>,
    observed: Option<&Observation>,
) -> CatalogRow {
    let empty = Observation::default();
    let o = observed.unwrap_or(&empty);

    // The site root is the only path that exists for every site, so it is where
    // a site-level "does this file allow this agent" question is asked. The
    // per-path truth is on the fetches themselves.
    let (robots_allowed, _) = agent_policy::evaluate(robots, llms, def, "/");

    CatalogRow {
        slug: def.slug,
        display_name: def.display_name,
        operator: def.operator,
        purpose: def.purpose.as_str(),
        docs_url: def.docs_url,
        respects_robots: def.respects_robots,
        publishes_ranges: def.publishes_ranges(),
        robots_token: def.robots_token(),
        robots_token_only: def.is_robots_token_only(),
        seen: o.fetches > 0,
        fetches: o.fetches,
        verified_fetches: o.verified_fetches,
        unique_paths: o.unique_paths,
        bytes: o.bytes,
        conditional_fetches: o.conditional_fetches,
        first_seen: ms_to_opt_dt(o.first_seen_ms),
        last_seen: ms_to_opt_dt(o.last_seen_ms),
        robots_allowed,
        robots_rule_named: robots.names(def.robots_token()),
        crawl_delay: robots.crawl_delay(def.robots_token()),
        listed_in_llms: o.llms_listed_fetches > 0,
        llms_listed_fetches: o.llms_listed_fetches,
        allowed_fetches: o.allowed_fetches,
        disallowed_fetches: o.disallowed_fetches,
        verified_disallowed_fetches: o.verified_disallowed_fetches,
        compliance: Compliance::read(o),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one rule this module exists to hold: an unverified fetch of a
    /// disallowed path is never an accusation.
    #[test]
    fn a_breach_needs_a_verified_address() {
        let unverified = Observation {
            fetches: 40,
            disallowed_fetches: 12,
            verified_disallowed_fetches: 0,
            ..Observation::default()
        };
        assert_eq!(Compliance::read(&unverified), Compliance::Unverified);

        let verified = Observation {
            verified_disallowed_fetches: 1,
            ..unverified.clone()
        };
        assert_eq!(Compliance::read(&verified), Compliance::Violation);
    }

    #[test]
    fn silence_and_absence_are_told_apart() {
        assert_eq!(
            Compliance::read(&Observation::default()),
            Compliance::NotSeen
        );

        // Seen, but the site's files never reach this agent.
        let unstated = Observation {
            fetches: 9,
            unstated_fetches: 9,
            ..Observation::default()
        };
        assert_eq!(Compliance::read(&unstated), Compliance::Unstated);

        let clean = Observation {
            fetches: 9,
            allowed_fetches: 9,
            ..Observation::default()
        };
        assert_eq!(Compliance::read(&clean), Compliance::Compliant);
    }

    #[test]
    fn only_agent_trace_dimensions_are_accepted() {
        assert!(agent_dimension(None).is_ok());
        assert!(agent_dimension(Some("operator")).is_ok());
        assert!(agent_dimension(Some("pathname")).is_ok());
        // A crawler has no browser, and answering with zeros would imply it did.
        let refused = agent_dimension(Some("browser")).unwrap_err();
        assert_eq!(refused.status(), 422);
        assert_eq!(refused.field(), Some("dimension"));
    }

    #[test]
    fn an_unfetched_policy_expresses_no_opinion() {
        let (robots, llms) = parsed_policy(None);
        assert!(robots.is_empty());
        assert!(llms.is_none());

        let gptbot = micaforge_agents::catalog::find_by_slug("gptbot").unwrap();
        let row = catalog_row(gptbot, &robots, None, None);
        assert_eq!(row.robots_allowed, None);
        assert_eq!(row.compliance, Compliance::NotSeen);
        assert!(!row.seen);
    }
}
