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

/// Most values listed per facet: browsers, systems, pages, releases.
const FACET_ROWS: u32 = 20;

/// Most sessions listed on one error's detail.
const DETAIL_SESSIONS: u32 = 50;

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
    /// How many distinct sessions it fired in.
    pub sessions: u64,
    /// First occurrence inside the window.
    pub first_seen: DateTime<Utc>,
    /// Most recent occurrence inside the window.
    pub last_seen: DateTime<Utc>,
    /// `false` when any occurrence reached the page uncaught (`window.onerror`
    /// or an unhandled rejection): one uncaught copy makes the group a crash.
    pub handled: bool,
    /// The site's release on the most recent occurrence, empty when it sent
    /// none.
    pub release: String,
    /// The path it fired on most often.
    pub top_path: String,
    /// The browsers it fired in most often, at most three.
    pub browsers: Vec<String>,
    /// Occurrences per equal slice of the window, oldest first, always
    /// [`SPARKLINE_BUCKETS`] long so every row's shape is drawn on one scale.
    pub sparkline: Vec<u64>,
}

/// What narrows the error report beyond the envelope.
///
/// `release` is a column of `errors` and not one of the filter dimensions of
/// `docs/CONTRACT.md` §3.2, so it travels beside the envelope rather than in
/// its filter tree. `fingerprint` narrows to one group, which is how the detail
/// screen reads the same numbers the list showed for that row.
#[derive(Debug, Clone, Default)]
pub struct ErrorNarrow {
    /// Only occurrences reported under this release.
    pub release: Option<String>,
    /// Only this group.
    pub fingerprint: Option<String>,
}

impl ErrorNarrow {
    /// The envelope's scope over `errors`, narrowed.
    fn scope(&self, env: &QueryEnvelope) -> Result<(String, Vec<Param>)> {
        let (mut scope, mut params) = scope_predicate(env, Table::Errors)?;
        if let Some(release) = self.release.as_deref().filter(|r| !r.is_empty()) {
            scope.push_str(" AND release = ?");
            params.push(Param::Str(release.to_string()));
        }
        if let Some(fingerprint) = self.fingerprint.as_deref() {
            scope.push_str(" AND fingerprint = ?");
            params.push(Param::Str(fingerprint.to_string()));
        }
        Ok((scope, params))
    }
}

#[derive(Debug, Row, Deserialize)]
struct ErrorRawRow {
    fingerprint: String,
    name: String,
    message: String,
    /// Spelled `hits` in SQL: `count` is a function name there.
    count: u64,
    users: u64,
    sessions: u64,
    first_seen: i64,
    last_seen: i64,
    all_handled: u8,
    release: String,
    top_path: String,
    browsers: Vec<String>,
}

#[derive(Debug, Row, Deserialize)]
struct SparkRawRow {
    fingerprint: String,
    slot: i64,
    hits: u64,
}

/// `GET /api/stats/errors`.
pub async fn errors(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<ErrorGroup>> {
    error_groups(ch, env, &ErrorNarrow::default()).await
}

/// The error groups, narrowed.
///
/// Two statements. The first ranks the groups; the second draws sparklines for
/// exactly the groups the first returned, with their fingerprints bound. Doing
/// it the other way round — one statement carrying a `groupArray` of every
/// occurrence — would build an array as long as the error count, which on the
/// one site that most needs this report is the largest array on the server.
pub async fn error_groups(
    ch: &Ch,
    env: &QueryEnvelope,
    narrow: &ErrorNarrow,
) -> Result<Vec<ErrorGroup>> {
    let (scope, params) = narrow.scope(env)?;

    let sql = format!(
        "SELECT fingerprint,
       argMax(name, timestamp) AS err_name,
       argMax(message, timestamp) AS err_message,
       toUInt64(count()) AS hits,
       uniq(visitor_id) AS users,
       uniq(session_id) AS err_sessions,
       {first} AS first_seen,
       {last} AS last_seen,
       min(handled) AS all_handled,
       toString(argMax(release, timestamp)) AS err_release,
       topK(1)(pathname)[1] AS err_top_path,
       arrayFilter(b -> b != '', topK(3)(toString(browser))) AS err_browsers
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
    let (spark_scope, spark_scope_params) = narrow.scope(env)?;
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
                sessions: g.sessions,
                first_seen: ms_to_dt(g.first_seen),
                last_seen: ms_to_dt(g.last_seen),
                handled: g.all_handled != 0,
                release: g.release,
                top_path: g.top_path,
                browsers: g.browsers,
                sparkline,
            }
        })
        .collect())
}

/// One value of a facet and how many occurrences carried it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorFacet {
    /// The browser, system, path or release.
    pub value: String,
    /// Occurrences with that value.
    pub count: u64,
}

/// The four facets the error register can be narrowed by, each ranked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorFacets {
    /// Browsers, by occurrences.
    pub browser: Vec<ErrorFacet>,
    /// Operating systems, by occurrences.
    pub os: Vec<ErrorFacet>,
    /// Paths, by occurrences.
    pub pathname: Vec<ErrorFacet>,
    /// The site's releases, by occurrences.
    pub release: Vec<ErrorFacet>,
}

#[derive(Debug, Row, Deserialize)]
struct FacetRawRow {
    facet: String,
    value: String,
    hits: u64,
}

/// The browsers, systems, pages and releases the errors in scope carried.
///
/// One statement: every occurrence is unfolded into its four `(facet, value)`
/// pairs and the top [`FACET_ROWS`] of each facet are kept with `LIMIT ... BY`,
/// so four ranked lists cost one scan of the window.
pub async fn error_facets(
    ch: &Ch,
    env: &QueryEnvelope,
    narrow: &ErrorNarrow,
) -> Result<ErrorFacets> {
    let (scope, params) = narrow.scope(env)?;
    let sql = format!(
        "SELECT pair.1 AS facet, pair.2 AS value, toUInt64(count()) AS hits
FROM (
  SELECT arrayJoin([
    ('browser', toString(browser)),
    ('os', toString(os)),
    ('pathname', pathname),
    ('release', toString(release))
  ]) AS pair
  FROM errors
  WHERE {scope}
)
WHERE value != ''
GROUP BY facet, value
ORDER BY facet ASC, hits DESC, value ASC
LIMIT {FACET_ROWS} BY facet"
    );
    let rows = fetch_all::<FacetRawRow>(ch, "errors_facets", &sql, &params).await?;
    Ok(facets_from(rows))
}

/// Sort the unfolded rows back into the four lists.
fn facets_from(rows: Vec<FacetRawRow>) -> ErrorFacets {
    let mut out = ErrorFacets::default();
    for row in rows {
        let list = match row.facet.as_str() {
            "browser" => &mut out.browser,
            "os" => &mut out.os,
            "pathname" => &mut out.pathname,
            "release" => &mut out.release,
            _ => continue,
        };
        list.push(ErrorFacet {
            value: row.value,
            count: row.hits,
        });
    }
    out
}

/// Totals over every group in scope, not only the page of groups listed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorTotals {
    /// Distinct fingerprints.
    pub groups: u64,
    /// Occurrences.
    pub occurrences: u64,
    /// Distinct visitors who hit any error.
    pub visitors: u64,
    /// Groups with at least one uncaught occurrence.
    pub unhandled: u64,
}

#[derive(Debug, Row, Deserialize)]
struct TotalsRawRow {
    groups: u64,
    occurrences: u64,
    visitors: u64,
    unhandled: u64,
}

/// [`ErrorTotals`] for the scope.
pub async fn error_totals(
    ch: &Ch,
    env: &QueryEnvelope,
    narrow: &ErrorNarrow,
) -> Result<ErrorTotals> {
    let (scope, params) = narrow.scope(env)?;
    let sql = format!(
        "SELECT uniq(fingerprint) AS groups,
       toUInt64(count()) AS occurrences,
       uniq(visitor_id) AS visitors,
       uniqIf(fingerprint, handled = 0) AS unhandled
FROM errors
WHERE {scope}"
    );
    let rows = fetch_all::<TotalsRawRow>(ch, "errors_totals", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .next()
        .map(|r| ErrorTotals {
            groups: r.groups,
            occurrences: r.occurrences,
            visitors: r.visitors,
            unhandled: r.unhandled,
        })
        .unwrap_or_default())
}

/// One session an error fired in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorSession {
    /// The session, as the session list and the replay address it.
    pub session_id: String,
    /// The last time it fired in that session.
    pub t: DateTime<Utc>,
    /// The page it last fired on in that session.
    pub pathname: String,
    /// The session's browser.
    pub browser: String,
    /// The session's operating system.
    pub os: String,
}

/// One error group, in full.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// The group's row, exactly as the list shows it.
    pub group: ErrorGroup,
    /// The stack of the most recent occurrence, as the browser reported it.
    pub stack: String,
    /// The source file of the most recent occurrence.
    pub filename: String,
    /// Its line.
    pub lineno: u32,
    /// Its column.
    pub colno: u32,
    /// The pages it fired on, ranked.
    pub by_page: Vec<ErrorFacet>,
    /// The browsers it fired in, ranked.
    pub by_browser: Vec<ErrorFacet>,
    /// The systems it fired on, ranked.
    pub by_os: Vec<ErrorFacet>,
    /// Occurrences per bucket at the envelope's granularity.
    pub series: Vec<super::Bucket>,
    /// The most recent sessions it fired in, newest first.
    pub sessions: Vec<ErrorSession>,
}

#[derive(Debug, Row, Deserialize)]
struct LatestRawRow {
    stack: String,
    filename: String,
    lineno: u32,
    colno: u32,
}

#[derive(Debug, Row, Deserialize)]
struct SeriesRawRow {
    t: i64,
    v: f64,
}

#[derive(Debug, Row, Deserialize)]
struct SessionRawRow {
    session_id: String,
    t: i64,
    pathname: String,
    browser: String,
    os: String,
}

/// `GET /api/stats/errors?fingerprint=…`: one group with its stack, where it
/// fired, when, and the sessions it fired in. `None` when the group has no
/// occurrence in scope.
pub async fn error_detail(
    ch: &Ch,
    env: &QueryEnvelope,
    fingerprint: &str,
    release: Option<String>,
) -> Result<Option<ErrorDetail>> {
    let narrow = ErrorNarrow {
        release,
        fingerprint: Some(fingerprint.to_string()),
    };
    let Some(group) = error_groups(ch, env, &narrow).await?.into_iter().next() else {
        return Ok(None);
    };
    let (scope, params) = narrow.scope(env)?;

    let latest = fetch_all::<LatestRawRow>(
        ch,
        "error_latest",
        &format!(
            "SELECT stack, filename, lineno, colno FROM errors WHERE {scope}
ORDER BY timestamp DESC LIMIT 1"
        ),
        &params,
    )
    .await?
    .into_iter()
    .next();

    let facets = error_facets(ch, env, &narrow).await?;

    let bucket = crate::sql::bucket_millis(env.granularity, env.range.tz, "timestamp");
    let series = fetch_all::<SeriesRawRow>(
        ch,
        "error_series",
        &format!(
            "SELECT {bucket} AS t, toFloat64(count()) AS v FROM errors WHERE {scope}
GROUP BY t ORDER BY t ASC"
        ),
        &params,
    )
    .await?
    .into_iter()
    .map(|r| super::Bucket { t: r.t, v: r.v })
    .collect();

    let sessions = fetch_all::<SessionRawRow>(
        ch,
        "error_sessions",
        &format!(
            "SELECT session_id, {last} AS t,
       argMax(pathname, timestamp) AS err_path,
       toString(argMax(browser, timestamp)) AS err_browser,
       toString(argMax(os, timestamp)) AS err_os
FROM errors
WHERE {scope}
GROUP BY session_id
ORDER BY t DESC
LIMIT {DETAIL_SESSIONS}",
            last = millis("max(timestamp)"),
        ),
        &params,
    )
    .await?
    .into_iter()
    .map(|r| ErrorSession {
        session_id: r.session_id,
        t: ms_to_dt(r.t),
        pathname: r.pathname,
        browser: r.browser,
        os: r.os,
    })
    .collect();

    let latest = latest.unwrap_or(LatestRawRow {
        stack: String::new(),
        filename: String::new(),
        lineno: 0,
        colno: 0,
    });
    Ok(Some(ErrorDetail {
        group,
        stack: latest.stack,
        filename: latest.filename,
        lineno: latest.lineno,
        colno: latest.colno,
        by_page: facets.pathname,
        by_browser: facets.browser,
        by_os: facets.os,
        series,
        sessions,
    }))
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use micaforge_core::{Audience, Compare, Granularity, TimeRange};

    fn env() -> QueryEnvelope {
        let end = Utc::now();
        QueryEnvelope {
            site: 3,
            range: TimeRange::new(end - chrono::Duration::days(7), end, chrono_tz::UTC),
            compare: Compare::None,
            granularity: Granularity::Day,
            filters: None,
            audience: Audience::Human,
            limit: 50,
            page: 1,
        }
    }

    #[test]
    fn narrowing_appends_bound_predicates_only() {
        let (plain, plain_params) = ErrorNarrow::default().scope(&env()).unwrap();
        let narrow = ErrorNarrow {
            release: Some("2026.10.1' OR 1=1 --".into()),
            fingerprint: Some("abcd".into()),
        };
        let (scope, params) = narrow.scope(&env()).unwrap();
        assert!(scope.starts_with(&plain));
        assert!(scope.ends_with(" AND release = ? AND fingerprint = ?"));
        assert_eq!(params.len(), plain_params.len() + 2);
        // The caller's text is a parameter, never part of the statement.
        assert!(!scope.contains("2026.10.1"));

        // An empty release narrows nothing.
        let empty = ErrorNarrow {
            release: Some(String::new()),
            fingerprint: None,
        };
        assert_eq!(empty.scope(&env()).unwrap().0, plain);
    }

    #[test]
    fn facet_rows_sort_into_their_lists_and_unknown_ones_are_dropped() {
        let row = |facet: &str, value: &str, hits: u64| FacetRawRow {
            facet: facet.into(),
            value: value.into(),
            hits,
        };
        let facets = facets_from(vec![
            row("browser", "Chrome", 9),
            row("browser", "Firefox", 2),
            row("os", "macOS", 5),
            row("pathname", "/checkout", 7),
            row("release", "2026.10.1", 11),
            row("country", "PT", 1),
        ]);
        assert_eq!(facets.browser.len(), 2);
        assert_eq!(
            facets.browser[0],
            ErrorFacet {
                value: "Chrome".into(),
                count: 9
            }
        );
        assert_eq!(facets.os[0].value, "macOS");
        assert_eq!(facets.pathname[0].count, 7);
        assert_eq!(facets.release[0].value, "2026.10.1");
    }
}
