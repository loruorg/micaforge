//! Performance and errors: the two reports that answer "is the site actually
//! working", rather than "who came".

use chrono::{DateTime, Utc};
use clickhouse::Row;
use micaforge_core::{Dimension, QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, ms_to_dt, round2};
use crate::sql::{f64_of, fmt_ts, millis, scope_predicate, Param};

/// Buckets in an error group's sparkline.
///
/// Twenty-four points read as a shape at any width a table cell can be, and it
/// divides evenly into a day, a week and a month, so the sparkline's resolution
/// changes with the range without the bucket count changing under the reader.
const SPARKLINE_BUCKETS: i64 = 24;

// ---------------------------------------------------------------------------
// Core Web Vitals thresholds
// ---------------------------------------------------------------------------

/// Largest Contentful Paint, in milliseconds, at or below which p75 is "good".
///
/// These four are Google's published Core Web Vitals thresholds, not a
/// judgement of ours. They are named as constants so that when the definitions
/// move — and they have moved: INP replaced FID in 2024 — the change is one
/// edit with a date attached, rather than a magic number in a `match`.
const LCP_GOOD_MS: f64 = 2_500.0;
/// LCP p75 above which the page is "poor".
const LCP_POOR_MS: f64 = 4_000.0;
/// Interaction to Next Paint, in milliseconds, at or below which p75 is "good".
const INP_GOOD_MS: f64 = 200.0;
/// INP p75 above which the page is "poor".
const INP_POOR_MS: f64 = 500.0;
/// Cumulative Layout Shift at or below which p75 is "good".
const CLS_GOOD: f64 = 0.1;
/// CLS p75 above which the page is "poor".
const CLS_POOR: f64 = 0.25;

/// How a page's Core Web Vitals read as a whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerfRating {
    /// Every measured vital is inside its "good" threshold.
    Good,
    /// At least one vital is between the thresholds.
    NeedsImprovement,
    /// At least one vital is past its "poor" threshold.
    Poor,
    /// Nothing was measured. Not a grade — an absence.
    Unknown,
}

/// One row of the performance report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfRow {
    /// The dimension value.
    pub key: String,
    /// How many hits carried at least one vital.
    pub samples: u64,
    /// LCP median, milliseconds.
    pub lcp_p50: f64,
    /// LCP 75th percentile, milliseconds. This is the one the rating uses.
    pub lcp_p75: f64,
    /// LCP 95th percentile, milliseconds.
    pub lcp_p95: f64,
    /// CLS 75th percentile, unitless.
    pub cls_p75: f64,
    /// INP 75th percentile, milliseconds.
    pub inp_p75: f64,
    /// FCP 75th percentile, milliseconds.
    pub fcp_p75: f64,
    /// TTFB 75th percentile, milliseconds.
    pub ttfb_p75: f64,
    /// The overall reading.
    pub rating: PerfRating,
}

#[derive(Debug, Row, Deserialize)]
struct PerfRawRow {
    key: String,
    samples: u64,
    lcp_p50: f64,
    lcp_p75: f64,
    lcp_p95: f64,
    cls_p75: f64,
    inp_p75: f64,
    fcp_p75: f64,
    ttfb_p75: f64,
}

/// Grade a row against the published thresholds.
///
/// A vital that was never reported does not drag the grade down: an unmeasured
/// metric is silence, and treating silence as a failure would grade every site
/// that has not shipped the vitals bundle as "poor".
fn rate(row: &PerfRawRow) -> PerfRating {
    let mut measured = false;
    let mut worst = PerfRating::Good;

    let mut consider = |value: f64, good: f64, poor: f64| {
        if value <= 0.0 {
            return;
        }
        measured = true;
        if value > poor {
            worst = PerfRating::Poor;
        } else if value > good && worst != PerfRating::Poor {
            worst = PerfRating::NeedsImprovement;
        }
    };

    consider(row.lcp_p75, LCP_GOOD_MS, LCP_POOR_MS);
    consider(row.inp_p75, INP_GOOD_MS, INP_POOR_MS);
    consider(row.cls_p75, CLS_GOOD, CLS_POOR);

    if measured {
        worst
    } else {
        PerfRating::Unknown
    }
}

/// `GET /api/stats/performance`.
///
/// Percentiles come from `quantileTDigest`, which is approximate and
/// deliberately so: an exact percentile over a month of vitals means holding
/// every sample in memory, and a p75 that is right to the millisecond is not a
/// number anyone acts on differently. The p75 is the one that carries the
/// rating, because that is the statistic the Core Web Vitals programme is
/// defined on.
pub async fn performance(
    ch: &Ch,
    env: &QueryEnvelope,
    dimension: &Dimension,
) -> Result<Vec<PerfRow>> {
    let (key_expr, key_params) = dimension.column_expr();
    let mut params: Vec<Param> = key_params.into_iter().map(Param::Str).collect();
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;
    params.extend(scope_params);

    let q =
        |quantile: &str, column: &str| f64_of(&format!("quantileTDigest({quantile})({column})"));

    let sql = format!(
        "SELECT toString({key_expr}) AS key,
       toUInt64(count()) AS samples,
       {lcp50} AS lcp_p50,
       {lcp75} AS lcp_p75,
       {lcp95} AS lcp_p95,
       {cls75} AS cls_p75,
       {inp75} AS inp_p75,
       {fcp75} AS fcp_p75,
       {ttfb75} AS ttfb_p75
FROM events
WHERE {scope}
  AND (isNotNull(lcp) OR isNotNull(cls) OR isNotNull(inp) OR isNotNull(fcp) OR isNotNull(ttfb))
GROUP BY key
ORDER BY samples DESC, key ASC
LIMIT {limit} OFFSET {offset}",
        lcp50 = q("0.5", "lcp"),
        lcp75 = q("0.75", "lcp"),
        lcp95 = q("0.95", "lcp"),
        cls75 = q("0.75", "cls"),
        inp75 = q("0.75", "inp"),
        fcp75 = q("0.75", "fcp"),
        ttfb75 = q("0.75", "ttfb"),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let rows = fetch_all::<PerfRawRow>(ch, "performance", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .map(|r| PerfRow {
            rating: rate(&r),
            key: r.key,
            samples: r.samples,
            lcp_p50: round2(r.lcp_p50),
            lcp_p75: round2(r.lcp_p75),
            lcp_p95: round2(r.lcp_p95),
            // CLS is a small unitless number; two decimals would round 0.083 to
            // 0.08 and lose the distinction the threshold is drawn on.
            cls_p75: (r.cls_p75 * 1000.0).round() / 1000.0,
            inp_p75: round2(r.inp_p75),
            fcp_p75: round2(r.fcp_p75),
            ttfb_p75: round2(r.ttfb_p75),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// One error, grouped by fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorGroup {
    /// The stable grouping key computed at ingest.
    pub fingerprint: String,
    /// Exception name, from the most recent occurrence.
    pub name: String,
    /// Message, from the most recent occurrence.
    pub message: String,
    /// How many times it happened.
    pub count: u64,
    /// How many distinct visitors hit it.
    pub users: u64,
    /// First occurrence inside the window.
    pub first_seen: DateTime<Utc>,
    /// Most recent occurrence inside the window.
    pub last_seen: DateTime<Utc>,
    /// Occurrences per equal slice of the window, oldest first, always
    /// [`SPARKLINE_BUCKETS`] long so every row's shape is drawn on one scale.
    pub sparkline: Vec<u64>,
}

#[derive(Debug, Row, Deserialize)]
struct ErrorRawRow {
    fingerprint: String,
    name: String,
    message: String,
    /// Spelled `hits` in SQL: `count` is a function name there.
    count: u64,
    users: u64,
    first_seen: i64,
    last_seen: i64,
}

#[derive(Debug, Row, Deserialize)]
struct SparkRawRow {
    fingerprint: String,
    slot: i64,
    hits: u64,
}

/// `GET /api/stats/errors`.
///
/// Two statements. The first ranks the groups; the second draws sparklines for
/// exactly the groups the first returned, with their fingerprints bound. Doing
/// it the other way round — one statement carrying a `groupArray` of every
/// occurrence — would build an array as long as the error count, which on the
/// one site that most needs this report is the largest array on the server.
pub async fn errors(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<ErrorGroup>> {
    let (scope, params) = scope_predicate(env, Table::Errors)?;

    let sql = format!(
        "SELECT fingerprint,
       argMax(name, timestamp) AS err_name,
       argMax(message, timestamp) AS err_message,
       toUInt64(count()) AS hits,
       uniq(visitor_id) AS users,
       {first} AS first_seen,
       {last} AS last_seen
FROM errors
WHERE {scope}
GROUP BY fingerprint
ORDER BY hits DESC, last_seen DESC
LIMIT {limit} OFFSET {offset}",
        first = millis("min(timestamp)"),
        last = millis("max(timestamp)"),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let groups = fetch_all::<ErrorRawRow>(ch, "errors", &sql, &params).await?;
    if groups.is_empty() {
        return Ok(Vec::new());
    }

    // One slice of the window per sparkline bucket, at least a second wide.
    let span_seconds = (env.range.end - env.range.start).num_seconds().max(1);
    let slot_seconds = (span_seconds / SPARKLINE_BUCKETS).max(1);

    let mut spark_params = vec![Param::Str(fmt_ts(env.range.start))];
    let (spark_scope, spark_scope_params) = scope_predicate(env, Table::Errors)?;
    spark_params.extend(spark_scope_params);
    let slots = vec!["?"; groups.len()].join(", ");
    for group in &groups {
        spark_params.push(Param::Str(group.fingerprint.clone()));
    }

    let spark_sql = format!(
        "SELECT fingerprint,
       toInt64(intDiv(dateDiff('second', toDateTime64(?, 3, 'UTC'), timestamp), {slot_seconds})) AS slot,
       toUInt64(count()) AS hits
FROM errors
WHERE {spark_scope} AND fingerprint IN ({slots})
GROUP BY fingerprint, slot"
    );

    let spark_rows =
        fetch_all::<SparkRawRow>(ch, "errors_sparkline", &spark_sql, &spark_params).await?;

    Ok(groups
        .into_iter()
        .map(|g| {
            let mut sparkline = vec![0u64; SPARKLINE_BUCKETS as usize];
            for row in spark_rows.iter().filter(|r| r.fingerprint == g.fingerprint) {
                if let Ok(index) = usize::try_from(row.slot) {
                    // The last slice can round past the final bucket; it belongs
                    // in the final bucket, not off the end of the chart.
                    let index = index.min(sparkline.len() - 1);
                    sparkline[index] += row.hits;
                }
            }
            ErrorGroup {
                fingerprint: g.fingerprint,
                name: g.name,
                message: g.message,
                count: g.count,
                users: g.users,
                first_seen: ms_to_dt(g.first_seen),
                last_seen: ms_to_dt(g.last_seen),
                sparkline,
            }
        })
        .collect())
}
