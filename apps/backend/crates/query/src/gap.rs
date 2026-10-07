//! The Citation Gap: the metric this product exists for.
//!
//! For one URL, over one window, Micaforge holds two numbers that no other
//! analytics tool keeps in the same place:
//!
//! - **`agent_fetches`** — how many times a named non-human client fetched it.
//! - **`answer_arrivals`** — how many sessions landed on it from an answer
//!   engine (`channel = 'answer_engine'`).
//!
//! The ratio between them is what the page is *getting back* for what is being
//! *taken*. A documentation page crawled two thousand times a month that has
//! never once produced a reader arriving from an answer is not "doing well in
//! AI search"; it is being consumed.
//!
//! ## The universe of URLs
//!
//! Every report here is built over a union of three traces, not a join from one
//! of them: the content register (`page_index`), the agent trace and the human
//! trace. A page that agents crawl and no person has ever visited must appear,
//! and so must a page that exists and nothing has touched at all. Those absences
//! *are* the finding. A `LEFT JOIN` from the human trace would hide both.
//!
//! ## Why filters do not narrow the traces
//!
//! The envelope's filters select which **URLs** are reported — `pathname` and
//! `page_title` — and they are applied once, to the assembled result. They do
//! not narrow either trace. Filtering the human half by browser while the agent
//! half stayed whole would produce a ratio between two different populations,
//! and a ratio between two different populations is not a measurement. A filter
//! naming any other dimension is refused rather than quietly ignored.

use micaforge_core::{Channel, QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, fetch_optional, round2};
use crate::sql::{
    bucket_millis, compile_filter_group, fmt_ts, scope_predicate, scope_predicate_over, Param,
};

// ---------------------------------------------------------------------------
// The thresholds
// ---------------------------------------------------------------------------

/// Fetches, in the window, above which a page counts as **heavily crawled**.
///
/// Below this there is not enough trace to call the page anything. A handful of
/// fetches is a sitemap sweep, a link check, a single agent following one
/// reference: it is not evidence that the page is feeding anything. Twenty is
/// chosen so that a page crawled roughly daily over a month clears it, and a
/// page a crawler glanced at once does not.
pub const HEAVY_CRAWL_FETCHES: u64 = 20;

/// Answer-engine arrivals, in the window, above which a page counts as
/// **cited**.
///
/// One arrival is a coincidence and two is a rounding error on a small site;
/// three is the smallest number that reads as a pattern rather than an accident.
/// It is deliberately low, because the failure this report is looking for is
/// pages with *zero* return, and setting the bar high would hide the pages that
/// are only just starting to work.
pub const CITED_ARRIVALS: u64 = 3;

/// Fetches per arrival above which a cited page is still being **extracted**.
///
/// A page can be both crawled and cited and still be a bad trade. Twenty-five
/// fetches for every reader returned is the point at which the exchange stops
/// looking like distribution and starts looking like a feed. The number is a
/// product judgement, not a measurement: it is the line this product is willing
/// to defend, and an operator who disagrees can read the raw two numbers, which
/// are on every row for exactly that reason.
pub const EXTRACTED_RATIO: f64 = 25.0;

/// Pages scanned when counting verdicts for the site-level summary.
///
/// The verdicts are decided in Rust, from the constants above, so that the
/// summary and the per-page table can never disagree about what "extracted"
/// means. That requires reading the rows rather than counting them in SQL, so
/// the scan is bounded. Fifty thousand distinct URLs in one window is far past
/// any documentation site; a larger one gets a summary over its fifty thousand
/// most-crawled pages, which is the same answer.
const VERDICT_SCAN_LIMIT: u32 = 50_000;

/// How far human traffic must fall, period over period, to count as decay.
const DECAY_DROP: f64 = 0.30;

/// The share of its earlier agent traffic a page must keep for that traffic to
/// count as having "held".
///
/// Not 1.0: crawl volume is lumpy, and demanding a strict increase would drop
/// exactly the pages where the crawlers stayed and the readers left, which is
/// the case the report exists to surface.
const DECAY_AGENT_HELD: f64 = 0.90;

/// Pageviews a page needed in the earlier period before a fall means anything.
///
/// A page that went from three views to one has not decayed; it has noise.
const DECAY_MIN_PREV_PAGEVIEWS: u64 = 20;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What the two numbers say about one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Heavily crawled, and either never cited or cited far less than it is
    /// taken. The page is feeding the machines and getting nothing back.
    Extracted,
    /// Crawled and cited. The exchange is working.
    Compounding,
    /// Cited but barely crawled. The page is earning readers on a trace so thin
    /// that it is probably under-indexed rather than well-optimised.
    Invisible,
    /// Neither crawled nor cited enough to say anything. Not a failure — no
    /// signal yet.
    Quiet,
}

/// One page's Citation Gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapRow {
    /// Path, without host or query string.
    pub path: String,
    /// Title from the content register, empty when the crawler has not indexed
    /// the page yet.
    pub title: String,
    /// Fetches by named non-human clients in the window.
    pub agent_fetches: u64,
    /// Sessions that arrived here from an answer engine in the window.
    pub answer_arrivals: u64,
    /// Pageviews by people in the window, for context. Not part of the ratio.
    pub human_pageviews: u64,
    /// Fetches per arrival. See [`ratio`] for what a zero denominator means.
    pub ratio: f64,
    /// The reading.
    pub verdict: Verdict,
}

/// How many verdicts of each kind a site has.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct VerdictCounts {
    /// Pages being taken with nothing coming back.
    pub extracted: u64,
    /// Pages where the exchange is working.
    pub compounding: u64,
    /// Pages earning readers on a thin crawl trace.
    pub invisible: u64,
    /// Pages with no signal yet.
    pub quiet: u64,
}

/// The site-level Citation Gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapSummary {
    /// Total fetches by named agents.
    pub agent_fetches: u64,
    /// Total sessions arriving from an answer engine.
    pub answer_arrivals: u64,
    /// Fetches per arrival across the whole site.
    pub ratio: f64,
    /// `(bucket start in epoch ms, fetches, arrivals)`, ascending.
    pub trend: Vec<(i64, u64, u64)>,
    /// How the site's pages break down.
    pub verdict_counts: VerdictCounts,
}

/// One page whose human traffic fell while the machines kept reading it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayRow {
    /// Path.
    pub path: String,
    /// Title from the content register.
    pub title: String,
    /// Human pageviews this period.
    pub human_pageviews: u64,
    /// Human pageviews last period.
    pub human_pageviews_prev: u64,
    /// Percentage change in human pageviews. Negative by construction.
    pub human_change_pct: f64,
    /// Agent fetches this period.
    pub agent_fetches: u64,
    /// Agent fetches last period.
    pub agent_fetches_prev: u64,
    /// Percentage change in agent fetches.
    pub agent_change_pct: Option<f64>,
    /// Answer-engine arrivals this period, so a reader can see whether the lost
    /// traffic came back through an answer instead.
    pub answer_arrivals: u64,
}

// ---------------------------------------------------------------------------
// The arithmetic
// ---------------------------------------------------------------------------

/// Fetches per arrival.
///
/// With no arrivals the ratio is the fetch count itself. That is not a fudge: it
/// is the reading a person makes anyway — "two thousand taken, none returned" —
/// and it keeps the number finite, which matters because JSON cannot spell
/// infinity and a serialiser would quietly turn it into `null`.
pub fn ratio(agent_fetches: u64, answer_arrivals: u64) -> f64 {
    if answer_arrivals == 0 {
        agent_fetches as f64
    } else {
        agent_fetches as f64 / answer_arrivals as f64
    }
}

/// The reading for one page.
///
/// Read top to bottom: being cited at all is the first question, because a page
/// that returns readers is not being extracted however hard it is crawled —
/// unless the exchange is lopsided enough to say otherwise.
pub fn verdict_for(agent_fetches: u64, answer_arrivals: u64) -> Verdict {
    let cited = answer_arrivals >= CITED_ARRIVALS;
    let heavily_crawled = agent_fetches >= HEAVY_CRAWL_FETCHES;

    if cited {
        if !heavily_crawled {
            Verdict::Invisible
        } else if ratio(agent_fetches, answer_arrivals) > EXTRACTED_RATIO {
            Verdict::Extracted
        } else {
            Verdict::Compounding
        }
    } else if heavily_crawled {
        Verdict::Extracted
    } else {
        Verdict::Quiet
    }
}

// ---------------------------------------------------------------------------
// Queries
// ---------------------------------------------------------------------------

/// The envelope with its filters removed.
///
/// Used for the two traces. See the module note on why the filters belong on the
/// assembled result and not on either half.
fn unfiltered(env: &QueryEnvelope) -> QueryEnvelope {
    let mut stripped = env.clone();
    stripped.filters = None;
    stripped
}

/// The union of the three traces, grouped by path.
///
/// The register branch contributes the URL and its title and no counts; the two
/// event branches contribute counts and no title. Summing them is what makes a
/// page appear whether it is in the register, in the crawl trace, in the human
/// trace, or in only one of the three.
fn universe_sql(env: &QueryEnvelope) -> Result<(String, Vec<Param>)> {
    let bare = unfiltered(env);
    let (agent_scope, agent_params) = scope_predicate(&bare, Table::AgentEvents)?;
    let (human_scope, human_params) = scope_predicate(&bare, Table::Events)?;
    let answer = Channel::AnswerEngine.as_str();

    let mut params = vec![Param::U16(env.site)];
    params.extend(agent_params);
    params.extend(human_params);

    let sql = format!(
        "SELECT
    path AS pathname,
    argMax(title, length(title)) AS page_title,
    toUInt64(sum(fetches)) AS agent_fetches,
    toUInt64(sum(arrivals)) AS answer_arrivals,
    toUInt64(sum(pageviews)) AS human_pageviews
  FROM (
    SELECT pn AS path, tt AS title, toUInt64(0) AS fetches, toUInt64(0) AS arrivals,
           toUInt64(0) AS pageviews
    FROM (
      SELECT pathname AS pn, argMax(title, updated_at) AS tt
      FROM page_index WHERE site_id = ? GROUP BY pathname
    )
    UNION ALL
    SELECT pathname AS path, '' AS title, toUInt64(count()) AS fetches, toUInt64(0) AS arrivals,
           toUInt64(0) AS pageviews
    FROM agent_events WHERE {agent_scope} GROUP BY pathname
    UNION ALL
    SELECT pathname AS path, '' AS title, toUInt64(0) AS fetches,
           toUInt64(uniqIf(session_id, channel = '{answer}')) AS arrivals,
           toUInt64(countIf(kind = 'pageview')) AS pageviews
    FROM events WHERE {human_scope} GROUP BY pathname
  )
  GROUP BY path"
    );

    Ok((sql, params))
}

#[derive(Debug, clickhouse::Row, Deserialize)]
struct GapRawRow {
    pathname: String,
    page_title: String,
    agent_fetches: u64,
    answer_arrivals: u64,
    human_pageviews: u64,
}

/// `GET /api/citation-gap`.
pub async fn by_page(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<GapRow>> {
    gap_rows(ch, env, env.limit.clamp(1, 10_000), env.offset()).await
}

/// The per-page rows, with the paging decided by the caller.
async fn gap_rows(ch: &Ch, env: &QueryEnvelope, limit: u32, offset: u64) -> Result<Vec<GapRow>> {
    let (universe, mut params) = universe_sql(env)?;

    // Filters select URLs, applied once to the assembled result. `page_index` is
    // the availability set here, which is why only `pathname` and `page_title`
    // are accepted: they are the two things a URL has on every trace.
    let mut where_clause = String::new();
    if let Some(g) = env.filters.as_ref().filter(|g| !g.is_empty()) {
        if let Some((predicate, filter_params)) = compile_filter_group(g, Table::PageIndex)? {
            where_clause = format!("\nWHERE {predicate}");
            params.extend(filter_params);
        }
    }

    let sql = format!(
        "SELECT pathname, page_title, agent_fetches, answer_arrivals, human_pageviews
FROM (
  {universe}
){where_clause}
ORDER BY agent_fetches DESC, human_pageviews DESC, pathname ASC
LIMIT {limit} OFFSET {offset}"
    );

    let rows = fetch_all::<GapRawRow>(ch, "citation_gap", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .map(|r| GapRow {
            ratio: round2(ratio(r.agent_fetches, r.answer_arrivals)),
            verdict: verdict_for(r.agent_fetches, r.answer_arrivals),
            path: r.pathname,
            title: r.page_title,
            agent_fetches: r.agent_fetches,
            answer_arrivals: r.answer_arrivals,
            human_pageviews: r.human_pageviews,
        })
        .collect())
}

#[derive(Debug, clickhouse::Row, Deserialize)]
struct TrendRawRow {
    t: i64,
    fetches: u64,
    arrivals: u64,
}

/// The two traces bucketed side by side.
///
/// Shared with `timeseries(gap_ratio)` so the headline number on the Citation
/// Gap screen and the line on the overview chart are the same query.
pub(crate) async fn trend_buckets(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<(i64, u64, u64)>> {
    let bare = unfiltered(env);
    let (agent_scope, mut params) = scope_predicate(&bare, Table::AgentEvents)?;
    let (human_scope, human_params) = scope_predicate(&bare, Table::Events)?;
    params.extend(human_params);

    let bucket = bucket_millis(env.granularity, env.range.tz, "timestamp");
    let answer = Channel::AnswerEngine.as_str();

    let sql = format!(
        "SELECT t, toUInt64(sum(fetches)) AS fetches, toUInt64(sum(arrivals)) AS arrivals
FROM (
  SELECT {bucket} AS t, toUInt64(count()) AS fetches, toUInt64(0) AS arrivals
  FROM agent_events WHERE {agent_scope} GROUP BY t
  UNION ALL
  SELECT {bucket} AS t, toUInt64(0) AS fetches,
         toUInt64(uniqIf(session_id, channel = '{answer}')) AS arrivals
  FROM events WHERE {human_scope} GROUP BY t
)
GROUP BY t
ORDER BY t ASC"
    );

    Ok(
        fetch_all::<TrendRawRow>(ch, "citation_gap_trend", &sql, &params)
            .await?
            .into_iter()
            .map(|r| (r.t, r.fetches, r.arrivals))
            .collect(),
    )
}

#[derive(Debug, clickhouse::Row, Deserialize)]
struct ScalarU64Row {
    v: u64,
}

/// `GET /api/citation-gap/summary`.
pub async fn summary(ch: &Ch, env: &QueryEnvelope) -> Result<GapSummary> {
    let bare = unfiltered(env);

    let (agent_scope, agent_params) = scope_predicate(&bare, Table::AgentEvents)?;
    let agent_fetches = fetch_optional::<ScalarU64Row>(
        ch,
        "citation_gap_fetches",
        &format!("SELECT toUInt64(count()) AS v FROM agent_events WHERE {agent_scope}"),
        &agent_params,
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let (human_scope, human_params) = scope_predicate(&bare, Table::Events)?;
    let answer_arrivals = fetch_optional::<ScalarU64Row>(
        ch,
        "citation_gap_arrivals",
        &format!(
            "SELECT toUInt64(uniqIf(session_id, channel = '{answer}')) AS v FROM events WHERE {human_scope}",
            answer = Channel::AnswerEngine.as_str(),
        ),
        &human_params,
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let trend = trend_buckets(ch, env).await?;

    let mut verdict_counts = VerdictCounts::default();
    for row in gap_rows(ch, env, VERDICT_SCAN_LIMIT, 0).await? {
        match row.verdict {
            Verdict::Extracted => verdict_counts.extracted += 1,
            Verdict::Compounding => verdict_counts.compounding += 1,
            Verdict::Invisible => verdict_counts.invisible += 1,
            Verdict::Quiet => verdict_counts.quiet += 1,
        }
    }

    Ok(GapSummary {
        agent_fetches,
        answer_arrivals,
        ratio: round2(ratio(agent_fetches, answer_arrivals)),
        trend,
        verdict_counts,
    })
}

#[derive(Debug, clickhouse::Row, Deserialize)]
struct DecayRawRow {
    pathname: String,
    page_title: String,
    human_now: u64,
    human_prev: u64,
    agent_now: u64,
    agent_prev: u64,
    arrivals_now: u64,
}

/// `GET /api/content/decay`.
///
/// The pages people stopped reading while the machines did not. Both halves are
/// measured over the same statement, with a window that spans the previous
/// period and this one, so the two numbers cannot be taken from two different
/// scans of a moving table.
///
/// The thresholds — [`DECAY_DROP`], [`DECAY_AGENT_HELD`] and
/// [`DECAY_MIN_PREV_PAGEVIEWS`] — are applied in Rust rather than in a `HAVING`,
/// so that the report and the constants that document it stay in one place.
pub async fn decay(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<DecayRow>> {
    let Some(previous) =
        micaforge_core::time::previous_period(&env.range, micaforge_core::Compare::Previous)
    else {
        return Ok(Vec::new());
    };

    let bare = unfiltered(env);
    let span = micaforge_core::TimeRange::new(previous.start, env.range.end, env.range.tz);
    let (agent_scope, agent_params) = scope_predicate_over(&bare, Table::AgentEvents, &span)?;
    let (human_scope, human_params) = scope_predicate_over(&bare, Table::Events, &span)?;

    let cut = fmt_ts(env.range.start);
    let answer = Channel::AnswerEngine.as_str();

    // Parameter order follows the statement exactly, and in each branch the
    // conditional aggregates in the SELECT list come before that branch's own
    // WHERE clause: register site, then two cut instants and the agent scope,
    // then three cut instants and the human scope.
    let mut params = vec![Param::U16(env.site)];
    params.push(Param::Str(cut.clone()));
    params.push(Param::Str(cut.clone()));
    params.extend(agent_params);
    params.push(Param::Str(cut.clone()));
    params.push(Param::Str(cut.clone()));
    params.push(Param::Str(cut));
    params.extend(human_params);

    let sql = format!(
        "SELECT pathname, page_title, human_now, human_prev, agent_now, agent_prev, arrivals_now
FROM (
  SELECT
    path AS pathname,
    argMax(title, length(title)) AS page_title,
    toUInt64(sum(h_now)) AS human_now,
    toUInt64(sum(h_prev)) AS human_prev,
    toUInt64(sum(a_now)) AS agent_now,
    toUInt64(sum(a_prev)) AS agent_prev,
    toUInt64(sum(arr_now)) AS arrivals_now
  FROM (
    SELECT pn AS path, tt AS title, toUInt64(0) AS h_now, toUInt64(0) AS h_prev,
           toUInt64(0) AS a_now, toUInt64(0) AS a_prev, toUInt64(0) AS arr_now
    FROM (
      SELECT pathname AS pn, argMax(title, updated_at) AS tt
      FROM page_index WHERE site_id = ? GROUP BY pathname
    )
    UNION ALL
    SELECT pathname AS path, '' AS title, toUInt64(0) AS h_now, toUInt64(0) AS h_prev,
           toUInt64(countIf(timestamp >= toDateTime64(?, 3, 'UTC'))) AS a_now,
           toUInt64(countIf(timestamp < toDateTime64(?, 3, 'UTC'))) AS a_prev,
           toUInt64(0) AS arr_now
    FROM agent_events WHERE {agent_scope} GROUP BY pathname
    UNION ALL
    SELECT pathname AS path, '' AS title,
           toUInt64(countIf(kind = 'pageview' AND timestamp >= toDateTime64(?, 3, 'UTC'))) AS h_now,
           toUInt64(countIf(kind = 'pageview' AND timestamp < toDateTime64(?, 3, 'UTC'))) AS h_prev,
           toUInt64(0) AS a_now, toUInt64(0) AS a_prev,
           toUInt64(uniqIf(session_id, channel = '{answer}'
                           AND timestamp >= toDateTime64(?, 3, 'UTC'))) AS arr_now
    FROM events WHERE {human_scope} GROUP BY pathname
  )
  GROUP BY path
)
WHERE human_prev >= {DECAY_MIN_PREV_PAGEVIEWS}
ORDER BY human_prev DESC
LIMIT {VERDICT_SCAN_LIMIT}"
    );

    let rows = fetch_all::<DecayRawRow>(ch, "content_decay", &sql, &params).await?;

    let mut out: Vec<DecayRow> = rows
        .into_iter()
        .filter(|r| {
            let fell = (r.human_now as f64) < (r.human_prev as f64) * (1.0 - DECAY_DROP);
            let agents_held =
                (r.agent_now as f64) >= (r.agent_prev as f64) * DECAY_AGENT_HELD && r.agent_now > 0;
            fell && agents_held
        })
        .map(|r| DecayRow {
            path: r.pathname,
            title: r.page_title,
            human_change_pct: round2(
                (r.human_now as f64 - r.human_prev as f64) / r.human_prev as f64 * 100.0,
            ),
            agent_change_pct: crate::exec::pct_delta_u64(r.agent_now, r.agent_prev),
            human_pageviews: r.human_now,
            human_pageviews_prev: r.human_prev,
            agent_fetches: r.agent_now,
            agent_fetches_prev: r.agent_prev,
            answer_arrivals: r.arrivals_now,
        })
        .collect();

    // Worst fall first: this is a triage list, not a table to browse.
    out.sort_by(|a, b| {
        a.human_change_pct
            .partial_cmp(&b.human_change_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(env.limit.clamp(1, 10_000) as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verdicts are a product judgement, so they get a test that reads like
    /// the sentence each one is supposed to mean.
    #[test]
    fn verdicts_say_what_they_mean() {
        // Two thousand fetches, nobody ever arrived.
        assert_eq!(verdict_for(2_000, 0), Verdict::Extracted);
        // Crawled hard, cited once in a while, still lopsided.
        assert_eq!(verdict_for(2_000, 10), Verdict::Extracted);
        // Crawled and genuinely returning readers.
        assert_eq!(verdict_for(200, 40), Verdict::Compounding);
        // Returning readers off almost no crawl trace.
        assert_eq!(verdict_for(4, 12), Verdict::Invisible);
        // Nothing has happened yet.
        assert_eq!(verdict_for(3, 1), Verdict::Quiet);
        assert_eq!(verdict_for(0, 0), Verdict::Quiet);
    }

    /// A zero denominator is answered, not divided by.
    #[test]
    fn the_ratio_stays_finite() {
        assert_eq!(ratio(500, 0), 500.0);
        assert_eq!(ratio(0, 0), 0.0);
        assert_eq!(ratio(100, 4), 25.0);
        assert!(ratio(1, 3).is_finite());
    }
}
