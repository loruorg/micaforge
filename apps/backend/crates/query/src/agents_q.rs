//! The agent-traffic reports.
//!
//! These read `agent_events`, which holds one row per fetch Micaforge could
//! attribute to a **named** non-human client. Unnamed or merely suspected
//! non-human traffic lives in `bot_events` and is not reported here: a report
//! that mixed "GPTBot, verified against OpenAI's published ranges" with "a
//! datacentre address with no user agent" would be worth nothing, and the whole
//! claim being made is that the first kind can be named.
//!
//! `verified` is the column that carries that claim. A user-agent string is
//! something a client asserts; a match against the operator's published IP
//! ranges or reverse-DNS suffix is something Micaforge checked. Both are
//! reported, and the share that was actually verified is on every summary,
//! because "GPTBot fetched this 400 times" means something quite different when
//! none of it verifies.

use chrono::{DateTime, Utc};
use clickhouse::Row;
use micaforge_core::{time::previous_period, Dimension, QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, fetch_optional, ms_to_dt, ms_to_opt_dt, pct_delta_u64, round2};
use crate::sql::{bucket_millis, envelope_over, f64_of, millis, scope_predicate, Param};
use crate::stats::{Bucket, Series};

/// Operators and purposes listed on the overview.
const TOP_LIST_N: u32 = 8;

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// Site-level agent traffic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOverview {
    /// Fetches in the window.
    pub fetches: u64,
    /// Distinct named agents.
    pub unique_agents: u64,
    /// Percentage of fetches whose source address matched the operator's
    /// published ranges or reverse DNS.
    pub verified_pct: f64,
    /// Bytes served to agents. The most literal answer to "how much was taken".
    pub bytes: u64,
    /// Distinct paths fetched.
    pub unique_paths: u64,
    /// `(operator, fetches)`, busiest first.
    pub top_operators: Vec<(String, u64)>,
    /// `(purpose, fetches)`, busiest first.
    pub top_purposes: Vec<(String, u64)>,
    /// Change in fetches against the comparison window, as a percentage.
    /// Absent when no comparison was asked for, or the earlier window was empty.
    pub delta_pct: Option<f64>,
}

#[derive(Debug, Row, Deserialize)]
struct AgentTotalsRow {
    fetches: u64,
    unique_agents: u64,
    verified_pct: f64,
    bytes: u64,
    unique_paths: u64,
}

#[derive(Debug, Row, Deserialize)]
struct NamedCountRow {
    name: String,
    fetches: u64,
}

/// `GET /api/agents/overview`.
pub async fn overview(ch: &Ch, env: &QueryEnvelope) -> Result<AgentOverview> {
    let (scope, params) = scope_predicate(env, Table::AgentEvents)?;

    let totals_sql = format!(
        "SELECT
  toUInt64(count()) AS fetches,
  uniq(agent_id) AS unique_agents,
  {verified} AS verified_pct,
  toUInt64(sum(bytes)) AS bytes,
  uniq(pathname) AS unique_paths
FROM agent_events
WHERE {scope}",
        verified = f64_of("round(100 * countIf(verified = 1) / nullIf(count(), 0), 2)"),
    );

    let totals = fetch_optional::<AgentTotalsRow>(ch, "agents_overview", &totals_sql, &params)
        .await?
        .unwrap_or(AgentTotalsRow {
            fetches: 0,
            unique_agents: 0,
            verified_pct: 0.0,
            bytes: 0,
            unique_paths: 0,
        });

    let top = |column: &str| -> String {
        format!(
            "SELECT toString({column}) AS name, toUInt64(count()) AS fetches
FROM agent_events
WHERE {scope}
GROUP BY name
ORDER BY fetches DESC, name ASC
LIMIT {TOP_LIST_N}"
        )
    };

    let top_operators =
        fetch_all::<NamedCountRow>(ch, "agents_operators", &top("operator"), &params)
            .await?
            .into_iter()
            .map(|r| (r.name, r.fetches))
            .collect();

    let top_purposes = fetch_all::<NamedCountRow>(ch, "agents_purposes", &top("purpose"), &params)
        .await?
        .into_iter()
        .map(|r| (r.name, r.fetches))
        .collect();

    let delta_pct = match previous_period(&env.range, env.compare) {
        Some(prev) => {
            let earlier_env = envelope_over(env, &prev);
            let (earlier_scope, earlier_params) =
                scope_predicate(&earlier_env, Table::AgentEvents)?;
            let earlier = fetch_optional::<ScalarU64Row>(
                ch,
                "agents_overview_compare",
                &format!("SELECT toUInt64(count()) AS v FROM agent_events WHERE {earlier_scope}"),
                &earlier_params,
            )
            .await?
            .map(|r| r.v)
            .unwrap_or(0);
            pct_delta_u64(totals.fetches, earlier)
        }
        None => None,
    };

    Ok(AgentOverview {
        fetches: totals.fetches,
        unique_agents: totals.unique_agents,
        verified_pct: round2(totals.verified_pct),
        bytes: totals.bytes,
        unique_paths: totals.unique_paths,
        top_operators,
        top_purposes,
        delta_pct,
    })
}

#[derive(Debug, Row, Deserialize)]
struct ScalarU64Row {
    v: u64,
}

// ---------------------------------------------------------------------------
// Breakdown
// ---------------------------------------------------------------------------

/// One ranked row of agent traffic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBreakdownRow {
    /// The stored value.
    pub key: String,
    /// What to show. An unrecorded value is named as one rather than renamed.
    pub label: String,
    /// Fetches.
    pub fetches: u64,
    /// Distinct paths this row touched.
    pub unique_paths: u64,
    /// Bytes served.
    pub bytes: u64,
    /// Verified share of this row's fetches.
    pub verified_pct: f64,
    /// Most recent fetch in the window.
    pub last_seen: DateTime<Utc>,
    /// Share of all fetches across every group the query produced, 0 to 1.
    pub share: f64,
    /// The company behind the crawler, when this row is one crawler and the
    /// catalog names it. `None` when the row groups by something else, or when
    /// the agent is not in the catalog. It is left null rather than guessed:
    /// attributing a fetch to a company we cannot identify is exactly the
    /// mistake the policy report exists to avoid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    /// The operator's own display spelling of the company name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_label: Option<String>,
    /// What the fetch is collected for, from the catalog.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// That purpose in a reader's words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose_label: Option<String>,
    /// Where the operator documents this crawler, so a site owner can read the
    /// source instead of trusting us.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    /// Whether the operator *states* it obeys robots.txt. The claim, never the
    /// behaviour; what it actually did is measured separately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub respects_robots: Option<bool>,
}

/// Fill the catalog-derived columns for a row that groups by `agent_id`.
fn enrich_from_catalog(dimension: &str, row: &mut AgentBreakdownRow) {
    if !matches!(dimension, "agent_id" | "agent") {
        return;
    }
    let Some(def) = micaforge_agents::catalog::CATALOG
        .iter()
        .find(|d| d.slug == row.key)
    else {
        return;
    };
    row.label = def.display_name.to_string();
    row.operator = Some(def.operator.to_string());
    row.operator_label =
        Some(micaforge_agents::catalog::operator_display(def.operator).to_string());
    row.purpose = Some(def.purpose.as_str().to_string());
    row.purpose_label =
        Some(micaforge_agents::catalog::purpose_display(def.purpose.as_str()).to_string());
    row.docs_url = def.docs_url.map(str::to_string);
    row.respects_robots = Some(def.respects_robots);
}

#[derive(Debug, Row, Deserialize)]
struct AgentBreakdownRawRow {
    key: String,
    fetches: u64,
    unique_paths: u64,
    bytes: u64,
    verified_pct: f64,
    last_seen: i64,
    share: f64,
}

/// `GET /api/agents/breakdown`.
///
/// The dimension must be one the agent trace records — `agent_id`, `operator`,
/// `purpose`, `pathname`, `status`, `content_type`, `country`, `asn_org`,
/// `verified` or `robots_allowed`. Asking for `browser` here is refused rather
/// than answered with zeros: a crawler does not have one.
pub async fn breakdown(
    ch: &Ch,
    env: &QueryEnvelope,
    dimension: &Dimension,
) -> Result<Vec<AgentBreakdownRow>> {
    let (key_expr, key_params) = dimension.column_expr();
    // The dimension's own wire spelling, used to decide whether a row can carry
    // catalog facts (operator, purpose, docs link) or is grouping by something
    // the catalog says nothing about.
    let column = dimension.as_str();
    let column = column.as_str();
    let mut params: Vec<Param> = key_params.into_iter().map(Param::Str).collect();
    let (scope, scope_params) = scope_predicate(env, Table::AgentEvents)?;
    params.extend(scope_params);

    let sql = format!(
        "SELECT key, fetches, unique_paths, bytes, verified_pct, last_seen,
       toFloat64(ifNull(fetches / nullIf(sum(fetches) OVER (), 0), 0)) AS share
FROM (
  SELECT toString({key_expr}) AS key,
         toUInt64(count()) AS fetches,
         uniq(pathname) AS unique_paths,
         toUInt64(sum(bytes)) AS bytes,
         {verified} AS verified_pct,
         {last} AS last_seen
  FROM agent_events
  WHERE {scope}
  GROUP BY key
)
ORDER BY fetches DESC, key ASC
LIMIT {limit} OFFSET {offset}",
        verified = f64_of("round(100 * countIf(verified = 1) / nullIf(count(), 0), 2)"),
        last = millis("max(timestamp)"),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let rows = fetch_all::<AgentBreakdownRawRow>(ch, "agents_breakdown", &sql, &params).await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let mut row = AgentBreakdownRow {
                label: if r.key.is_empty() {
                    "(not recorded)".to_string()
                } else {
                    label_for_agent_key(column, &r.key)
                },
                // The window sums every group, so page two's shares are still
                // shares of the whole.
                share: round2(r.share * 100.0) / 100.0,
                key: r.key,
                fetches: r.fetches,
                unique_paths: r.unique_paths,
                bytes: r.bytes,
                verified_pct: round2(r.verified_pct),
                last_seen: ms_to_dt(r.last_seen),
                operator: None,
                operator_label: None,
                purpose: None,
                purpose_label: None,
                docs_url: None,
                respects_robots: None,
            };
            enrich_from_catalog(column, &mut row);
            row
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Time series
// ---------------------------------------------------------------------------

#[derive(Debug, Row, Deserialize)]
struct BucketRawRow {
    t: i64,
    v: f64,
}

/// `GET /api/agents/timeseries`.
///
/// `agent` narrows to one catalog slug and is bound, never interpolated. The
/// comparison series is produced when the envelope asks for one, so a crawl
/// spike can be read against the same days a week earlier.
pub async fn timeseries(ch: &Ch, env: &QueryEnvelope, agent: Option<&str>) -> Result<Series> {
    let buckets = agent_buckets(ch, env, agent).await?;
    let compare = match previous_period(&env.range, env.compare) {
        Some(prev) => Some(agent_buckets(ch, &envelope_over(env, &prev), agent).await?),
        None => None,
    };

    Ok(Series {
        metric: micaforge_core::Metric::AgentFetches.as_str().to_string(),
        unit: micaforge_core::Metric::AgentFetches.unit(),
        buckets,
        compare,
    })
}

/// One dimension value's trace across the window.
#[derive(Debug, Serialize)]
pub struct SplitSeries {
    /// The dimension value, e.g. an operator slug.
    pub key: String,
    /// The value as a reader should see it.
    pub label: String,
    /// One entry per bucket, aligned to [`SplitTimeseries::buckets`].
    pub values: Vec<f64>,
}

/// A crawl record split by a dimension, so each operator writes its own pen.
///
/// The flat [`timeseries`] answers "how much was fetched"; this answers "by
/// whom, over time", which is the question the agent screen is actually asking.
#[derive(Debug, Serialize)]
pub struct SplitTimeseries {
    /// The shared time axis, in epoch milliseconds.
    pub buckets: Vec<i64>,
    /// One series per dimension value, busiest first.
    pub series: Vec<SplitSeries>,
}

/// The crawl record, split by `agent_id`, `operator` or `purpose`.
///
/// Every series carries a value for every bucket, zero included: a pen that
/// stops writing has to draw a floor, not a gap, or the reader mistakes a quiet
/// day for a missing one.
pub async fn timeseries_split(
    ch: &Ch,
    env: &QueryEnvelope,
    dimension: &str,
) -> Result<SplitTimeseries> {
    let column = match dimension {
        "agent_id" | "agent" => "agent_id",
        "operator" => "operator",
        "purpose" => "purpose",
        other => {
            return Err(micaforge_core::Error::Validation {
                field: "dimension".into(),
                message: format!(
                    "invalid dimension: `{other}` cannot split the crawl record; \
                     it can be agent_id, operator or purpose"
                ),
            })
        }
    };

    let (scope, params) = scope_predicate(env, Table::AgentEvents)?;
    let bucket = bucket_millis(env.granularity, env.range.tz, "timestamp");
    let sql = format!(
        "SELECT {bucket} AS t, toString({column}) AS k, toFloat64(count()) AS v
FROM agent_events
WHERE {scope}
GROUP BY t, k
ORDER BY t ASC"
    );

    #[derive(Debug, clickhouse::Row, Deserialize)]
    struct SplitRawRow {
        t: i64,
        k: String,
        v: f64,
    }

    let rows = fetch_all::<SplitRawRow>(ch, "agents_timeseries_split", &sql, &params).await?;

    let mut buckets: Vec<i64> = rows.iter().map(|r| r.t).collect();
    buckets.sort_unstable();
    buckets.dedup();
    let index: std::collections::HashMap<i64, usize> =
        buckets.iter().enumerate().map(|(i, t)| (*t, i)).collect();

    let mut by_key: std::collections::HashMap<String, Vec<f64>> = std::collections::HashMap::new();
    for r in &rows {
        let slot = by_key
            .entry(r.k.clone())
            .or_insert_with(|| vec![0.0; buckets.len()]);
        if let Some(i) = index.get(&r.t) {
            slot[*i] += r.v;
        }
    }

    let mut series: Vec<SplitSeries> = by_key
        .into_iter()
        .map(|(key, values)| SplitSeries {
            label: label_for_agent_key(column, &key),
            key,
            values,
        })
        .collect();
    series.sort_by(|a, b| {
        let sa: f64 = a.values.iter().sum();
        let sb: f64 = b.values.iter().sum();
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(SplitTimeseries { buckets, series })
}

/// A dimension value as a reader should see it.
///
/// The catalog carries the operator's own spelling of its crawler; a slug in a
/// column is our spelling. Showing `gptbot` where the operator writes `GPTBot`
/// is a small thing that makes the whole report look machine-generated.
fn label_for_agent_key(column: &str, key: &str) -> String {
    if key.is_empty() {
        return "Unattributed".to_string();
    }
    match column {
        "agent_id" => micaforge_agents::catalog::CATALOG
            .iter()
            .find(|d| d.slug == key)
            .map(|d| d.display_name.to_string())
            .unwrap_or_else(|| key.to_string()),
        // The catalog is the authority on how a company spells its own name.
        "operator" => micaforge_agents::catalog::operator_display(key).to_string(),
        "purpose" => micaforge_agents::catalog::purpose_display(key).to_string(),
        _ => key.to_string(),
    }
}

/// The fetch buckets for one window.
async fn agent_buckets(ch: &Ch, env: &QueryEnvelope, agent: Option<&str>) -> Result<Vec<Bucket>> {
    let (scope, mut params) = scope_predicate(env, Table::AgentEvents)?;
    let agent_clause = match agent {
        Some(slug) if !slug.is_empty() => {
            params.push(Param::Str(slug.to_string()));
            " AND agent_id = ?"
        }
        _ => "",
    };

    let bucket = bucket_millis(env.granularity, env.range.tz, "timestamp");
    let sql = format!(
        "SELECT {bucket} AS t, toFloat64(count()) AS v
FROM agent_events
WHERE {scope}{agent_clause}
GROUP BY t
ORDER BY t ASC"
    );

    Ok(
        fetch_all::<BucketRawRow>(ch, "agents_timeseries", &sql, &params)
            .await?
            .into_iter()
            .map(|r| Bucket {
                t: r.t,
                v: round2(r.v),
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

/// One URL's crawl coverage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageRow {
    /// Path, from the content register.
    pub path: String,
    /// Title, from the content register.
    pub title: String,
    /// Fetches in the window. Zero is a real answer and the most interesting
    /// one: it means nothing has crawled this page.
    pub fetch_count: u64,
    /// First fetch in the window, absent when there was none.
    pub first_crawl: Option<DateTime<Utc>>,
    /// Most recent fetch in the window, absent when there was none.
    pub last_crawl: Option<DateTime<Utc>>,
    /// How stale the crawl is, in hours. See [`coverage`] for the definition.
    pub freshness_gap_hours: Option<f64>,
    /// Which agents fetched it.
    pub agents: Vec<String>,
    /// Whether the site's own sitemap lists it.
    pub in_sitemap: bool,
    /// Whether the site's own `llms.txt` lists it.
    pub in_llms_txt: bool,
    /// `1` allowed, `0` disallowed, `-1` unknown. See [`coverage`].
    pub robots_allowed: i8,
}

#[derive(Debug, Row, Deserialize)]
struct CoverageRawRow {
    path: String,
    title: String,
    fetch_count: u64,
    first_crawl: i64,
    last_crawl: i64,
    last_modified: i64,
    agents: Vec<String>,
    in_sitemap: u8,
    in_llms_txt: u8,
    robots_directive: String,
    observed_robots: i8,
}

/// `GET /api/agents/coverage`.
///
/// A `LEFT JOIN` from the content register, and the direction is the whole
/// point: the register is the list of pages that exist, and a page nothing has
/// crawled appears with a zero. Joining the other way would filter away exactly
/// the rows an operator opened this screen to find.
///
/// **Freshness gap** is the time between the page's last modification and its
/// most recent crawl: how long the current version of the page has been
/// unread by the machines. When the register has no modification date, it falls
/// back to the time since the last crawl. A page never crawled has no gap — it
/// has an absence, and gets `null`.
///
/// **`robots_allowed`** prefers what was observed: if agents fetched the page,
/// the value recorded on those fetches is what the site's own rules said at the
/// time. With no fetches to read it from, it is derived from the register's
/// `robots_directive`, and `-1` when the site has no `robots.txt` at all.
pub async fn coverage(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<CoverageRow>> {
    let bare = {
        let mut stripped = env.clone();
        // The register cannot answer a filter on an agent dimension, and the
        // report's subject is the register.
        stripped.filters = None;
        stripped
    };
    let (agent_scope, mut params) = scope_predicate(&bare, Table::AgentEvents)?;

    // The register branch is first in the statement, so its site parameter is
    // bound first.
    params.insert(0, Param::U16(env.site));

    let mut where_clause = String::new();
    if let Some(g) = env.filters.as_ref().filter(|g| !g.is_empty()) {
        if let Some((predicate, filter_params)) =
            crate::sql::compile_filter_group(g, Table::PageIndex)?
        {
            where_clause = format!("\nWHERE {predicate}");
            params.extend(filter_params);
        }
    }

    // Every alias below is a name neither source table uses, so nothing here is
    // a cyclic alias. `pathname` and `page_title` are introduced only at the
    // level the URL filter is applied to.
    let sql = format!(
        "SELECT pathname, page_title, fetch_count, first_crawl, last_crawl,
       last_modified, agents, in_sitemap, in_llms_txt, robots_directive, observed_robots
FROM (
  SELECT
    p.pn AS pathname,
    p.tt AS page_title,
    p.sm AS in_sitemap,
    p.lt AS in_llms_txt,
    p.rd AS robots_directive,
    p.lm AS last_modified,
    a.fc AS fetch_count,
    a.first_ms AS first_crawl,
    a.last_ms AS last_crawl,
    a.ag AS agents,
    a.orb AS observed_robots
  FROM (
    SELECT pathname AS pn,
           argMax(title, updated_at) AS tt,
           argMax(in_sitemap, updated_at) AS sm,
           argMax(in_llms_txt, updated_at) AS lt,
           argMax(robots_directive, updated_at) AS rd,
           toInt64(ifNull({modified}, 0)) AS lm
    FROM page_index
    WHERE site_id = ?
    GROUP BY pathname
  ) AS p
  LEFT JOIN (
    SELECT pathname AS pn,
           toUInt64(count()) AS fc,
           {first} AS first_ms,
           {last} AS last_ms,
           groupUniqArray(toString(agent_id)) AS ag,
           toInt8(max(robots_allowed)) AS orb
    FROM agent_events
    WHERE {agent_scope}
    GROUP BY pathname
  ) AS a ON a.pn = p.pn
){where_clause}
ORDER BY fetch_count DESC, pathname ASC
LIMIT {limit} OFFSET {offset}",
        modified = millis("max(last_modified)"),
        first = millis("min(timestamp)"),
        last = millis("max(timestamp)"),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let rows = fetch_all::<CoverageRawRow>(ch, "agents_coverage", &sql, &params).await?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let last_crawl = ms_to_opt_dt(r.last_crawl);
            let last_modified = ms_to_opt_dt(r.last_modified);
            let freshness_gap_hours = last_crawl.map(|crawled| {
                let reference = last_modified.unwrap_or_else(Utc::now);
                let gap = (reference - crawled).num_seconds() as f64 / 3_600.0;
                round2(gap.max(0.0))
            });

            CoverageRow {
                robots_allowed: if r.fetch_count > 0 {
                    r.observed_robots
                } else {
                    derive_robots(&r.robots_directive)
                },
                path: r.path,
                title: r.title,
                fetch_count: r.fetch_count,
                first_crawl: ms_to_opt_dt(r.first_crawl),
                last_crawl,
                freshness_gap_hours,
                agents: r.agents,
                in_sitemap: r.in_sitemap == 1,
                in_llms_txt: r.in_llms_txt == 1,
            }
        })
        .collect())
}

/// Read the register's own directive for a page nothing has fetched.
///
/// Conservative in one direction only: an empty directive is `unknown`, not
/// `allowed`. Reporting "allowed" for a site whose `robots.txt` has never been
/// read would be an assertion about the operator's policy that Micaforge has no
/// basis for.
fn derive_robots(directive: &str) -> i8 {
    let d = directive.trim().to_ascii_lowercase();
    if d.is_empty() {
        -1
    } else if d.contains("disallow") || d.contains("noindex") || d.contains("none") {
        0
    } else {
        1
    }
}
