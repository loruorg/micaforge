//! Google Search Console reports: what people searched for before they
//! arrived, and how the site showed up in the results.
//!
//! The rows come from the `search_console` table, which the sync job
//! (`micaforge_jobs::search_console`) fills from Google's Search Analytics
//! API. Three things make these reads different from every other report in
//! this crate, and each one is handled in exactly one place below:
//!
//! - **Days, not instants.** Google reports whole days in its own calendar
//!   (Pacific time). A window is turned into a range of dates once, by
//!   [`DateWindow::of_range`], reading the caller's start and end in the
//!   caller's own zone; nothing here pretends to an hour's precision.
//! - **The newest fetch of each day wins.** Google revises the last three
//!   days, so the same day is fetched more than once and the table holds
//!   several copies until ClickHouse merges them. [`latest`] keeps only the
//!   rows of the newest fetch of each day at the grain being read, which is
//!   right before a merge, after it, and when a later fetch no longer reports
//!   a row an earlier one did.
//! - **Each figure from the grain that was asked for it.** Totals come from
//!   the `total` grain, pages from `page`, countries from `country`, devices
//!   from `device`, and only search terms from the five-dimension `query`
//!   grain. See `micaforge_store::ch::SearchConsoleRow` for why summing the
//!   `query` grain would under-report.
//!
//! Click-through rate is clicks over impressions, computed from the sums.
//! Average position is weighted by impressions, the way Google aggregates it,
//! never a plain average of averages.

use std::collections::BTreeMap;

use chrono::{Duration, NaiveDate};
use clickhouse::Row;
use micaforge_core::{time, Compare, Error, Result, TimeRange};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, fetch_optional, pct_delta, round2};
use crate::sql::{fmt_ts, Param};

/// The grain of the property's own daily totals.
pub const GRAIN_TOTAL: &str = "total";
/// The five-dimension grain search terms are read from.
pub const GRAIN_QUERY: &str = "query";
/// By page.
pub const GRAIN_PAGE: &str = "page";
/// By country.
pub const GRAIN_COUNTRY: &str = "country";
/// By device.
pub const GRAIN_DEVICE: &str = "device";

/// Most rows one ranked report returns.
pub const MAX_ROWS: u32 = 1_000;

/// Which site and property a report reads.
#[derive(Debug, Clone, Copy)]
pub struct SearchScope<'a> {
    /// Numeric site id.
    pub site: u16,
    /// The Search Console property, exactly as Google names it.
    pub property: &'a str,
}

/// An inclusive range of Google's days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateWindow {
    /// The first day.
    pub start: NaiveDate,
    /// The last day, included.
    pub end: NaiveDate,
}

impl DateWindow {
    /// The days a window covers, read in the window's own zone.
    ///
    /// The end is exclusive on a [`TimeRange`], so the last day is the one
    /// holding the instant just before it. A window shorter than a day is
    /// that one day.
    pub fn of_range(range: &TimeRange) -> DateWindow {
        let start = range.start.with_timezone(&range.tz).date_naive();
        let last = (range.end - Duration::milliseconds(1))
            .with_timezone(&range.tz)
            .date_naive();
        DateWindow {
            start,
            end: last.max(start),
        }
    }

    /// How many days it covers.
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days() + 1
    }

    /// Every day in it, in order.
    pub fn each_day(&self) -> Vec<NaiveDate> {
        (0..self.days())
            .filter_map(|i| self.start.checked_add_signed(Duration::days(i)))
            .collect()
    }
}

/// The comparison window for `range`, as days, or `None` for no comparison.
pub fn previous_window(range: &TimeRange, compare: Compare) -> Option<DateWindow> {
    time::previous_period(range, compare).map(|r| DateWindow::of_range(&r))
}

/// What to rank a list by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortBy {
    /// Most clicks first.
    #[default]
    Clicks,
    /// Most impressions first.
    Impressions,
    /// Highest click-through rate first.
    Ctr,
    /// Best (lowest) average position first.
    Position,
}

impl SortBy {
    /// Parse the wire name; absent is clicks.
    pub fn parse(raw: Option<&str>) -> Result<SortBy> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("clicks") => Ok(SortBy::Clicks),
            Some("impressions") => Ok(SortBy::Impressions),
            Some("ctr") => Ok(SortBy::Ctr),
            Some("position") => Ok(SortBy::Position),
            Some(other) => Err(Error::validation(
                "sort",
                format!("`{other}` is not one of clicks, impressions, ctr, position"),
            )),
        }
    }

    /// The `ORDER BY` for it. Each is this module's own text.
    fn order(self) -> &'static str {
        match self {
            SortBy::Clicks => "n_clicks DESC, n_impressions DESC, key",
            SortBy::Impressions => "n_impressions DESC, n_clicks DESC, key",
            SortBy::Ctr => {
                "if(n_impressions = 0, 0, n_clicks / n_impressions) DESC, n_impressions DESC, key"
            }
            SortBy::Position => {
                "if(n_impressions = 0, 1e9, weighted / n_impressions) ASC, n_impressions DESC, key"
            }
        }
    }
}

/// Narrowing for the search-terms report. Every condition must hold; each is
/// matched without regard to case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryFilter {
    /// Search terms containing this text.
    pub query: Option<String>,
    /// Pages whose path or address contains this text.
    pub page: Option<String>,
    /// One country, ISO 3166-1 alpha-2.
    pub country: Option<String>,
    /// One device: `desktop`, `mobile` or `tablet`.
    pub device: Option<String>,
}

impl QueryFilter {
    /// Check the values and put them in the stored spelling.
    pub fn checked(self) -> Result<QueryFilter> {
        let text = |v: Option<String>, field: &str| -> Result<Option<String>> {
            match v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
                Some(s) if s.chars().count() > 200 => {
                    Err(Error::validation(field, "must be at most 200 characters"))
                }
                other => Ok(other),
            }
        };
        let country = match text(self.country, "country")? {
            Some(c) if c.len() == 2 && c.chars().all(|ch| ch.is_ascii_alphabetic()) => {
                Some(c.to_ascii_uppercase())
            }
            Some(_) => {
                return Err(Error::validation(
                    "country",
                    "must be a two-letter country code, such as US or DE",
                ))
            }
            None => None,
        };
        let device = match text(self.device, "device")?.map(|d| d.to_ascii_lowercase()) {
            Some(d) if ["desktop", "mobile", "tablet"].contains(&d.as_str()) => Some(d),
            Some(_) => {
                return Err(Error::validation(
                    "device",
                    "must be one of desktop, mobile, tablet",
                ))
            }
            None => None,
        };
        Ok(QueryFilter {
            query: text(self.query, "query")?,
            page: text(self.page, "page")?,
            country,
            device,
        })
    }

    /// The predicate and its binds, or `None` for no narrowing.
    fn predicate(&self) -> Option<(String, Vec<Param>)> {
        let mut parts = Vec::new();
        let mut params = Vec::new();
        if let Some(q) = &self.query {
            parts.push("positionCaseInsensitiveUTF8(query, ?) > 0");
            params.push(Param::Str(q.clone()));
        }
        if let Some(p) = &self.page {
            parts.push("(positionCaseInsensitiveUTF8(pathname, ?) > 0 OR positionCaseInsensitiveUTF8(page, ?) > 0)");
            params.push(Param::Str(p.clone()));
            params.push(Param::Str(p.clone()));
        }
        if let Some(c) = &self.country {
            parts.push("country = ?");
            params.push(Param::Str(c.clone()));
        }
        if let Some(d) = &self.device {
            parts.push("device = ?");
            params.push(Param::Str(d.clone()));
        }
        (!parts.is_empty()).then(|| (parts.join(" AND "), params))
    }
}

// ---------------------------------------------------------------------------
// The shared predicate
// ---------------------------------------------------------------------------

/// The rows of one grain in a window, newest fetch of each day only.
///
/// The `(date, synced_at) IN (…)` pair is what makes a revised day read once:
/// see the module notes. The scope is bound twice, once for the rows and once
/// for the subquery, because positional binds cannot be shared.
fn latest(scope: SearchScope<'_>, grain: &str, window: DateWindow) -> (String, Vec<Param>) {
    let one = || {
        vec![
            Param::U16(scope.site),
            Param::Str(scope.property.to_string()),
            Param::Str(grain.to_string()),
            Param::Str(window.start.to_string()),
            Param::Str(window.end.to_string()),
        ]
    };
    let mut params = one();
    params.extend(one());
    (
        "site_id = ? AND property = ? AND grain = ? AND date >= toDate(?) AND date <= toDate(?) \
         AND (date, synced_at) IN (SELECT date, max(synced_at) FROM search_console \
         WHERE site_id = ? AND property = ? AND grain = ? AND date >= toDate(?) AND date <= toDate(?) \
         GROUP BY date)"
            .to_string(),
        params,
    )
}

/// Clicks over impressions, as a fraction; zero with no impressions.
fn ctr(clicks: u64, impressions: u64) -> f64 {
    if impressions == 0 {
        0.0
    } else {
        clicks as f64 / impressions as f64
    }
}

/// The impression-weighted average position; zero with no impressions.
fn position(weighted: f64, impressions: u64) -> f64 {
    if impressions == 0 {
        0.0
    } else {
        round2(weighted / impressions as f64)
    }
}

// ---------------------------------------------------------------------------
// Totals
// ---------------------------------------------------------------------------

/// The four headline figures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchTotals {
    /// Clicks from Google Search.
    pub clicks: u64,
    /// Times the site was shown in the results.
    pub impressions: u64,
    /// Clicks over impressions, a fraction between 0 and 1.
    pub ctr: f64,
    /// Average position, weighted by impressions (1 is the top).
    pub position: f64,
}

/// Each figure's change against the comparison window, in percent. `None`
/// where the earlier figure was zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchChange {
    /// Change in clicks.
    pub clicks: Option<f64>,
    /// Change in impressions.
    pub impressions: Option<f64>,
    /// Change in click-through rate.
    pub ctr: Option<f64>,
    /// Change in average position. A fall is an improvement.
    pub position: Option<f64>,
}

/// `GET /api/stats/search/overview`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchOverview {
    /// The days read.
    pub window: DateWindow,
    /// The figures over them.
    pub totals: SearchTotals,
    /// The comparison window, when one was asked for.
    pub previous_window: Option<DateWindow>,
    /// The figures over it.
    pub previous: Option<SearchTotals>,
    /// The change between the two.
    pub change: Option<SearchChange>,
    /// The first and last day in the window with any data. `None` with none.
    pub first_day: Option<NaiveDate>,
    /// See `first_day`.
    pub last_day: Option<NaiveDate>,
}

#[derive(Debug, Row, Deserialize)]
struct TotalsRow {
    n_clicks: u64,
    n_impressions: u64,
    weighted: f64,
    first: String,
    last: String,
    days: u64,
}

async fn totals(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
) -> Result<(SearchTotals, Option<NaiveDate>, Option<NaiveDate>)> {
    let (pred, params) = latest(scope, GRAIN_TOTAL, window);
    let sql = format!(
        "SELECT toUInt64(sum(clicks)) AS n_clicks, toUInt64(sum(impressions)) AS n_impressions,
                toFloat64(sum(position * impressions)) AS weighted,
                toString(min(date)) AS first, toString(max(date)) AS last,
                toUInt64(count()) AS days
         FROM search_console
         WHERE {pred}"
    );
    let row = fetch_optional::<TotalsRow>(ch, "search_totals", &sql, &params).await?;
    Ok(match row {
        Some(r) if r.days > 0 => (
            SearchTotals {
                clicks: r.n_clicks,
                impressions: r.n_impressions,
                ctr: ctr(r.n_clicks, r.n_impressions),
                position: position(r.weighted, r.n_impressions),
            },
            r.first.parse().ok(),
            r.last.parse().ok(),
        ),
        _ => (SearchTotals::default(), None, None),
    })
}

/// The headline figures for a window, with the comparison when asked for.
pub async fn overview(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
    previous_window: Option<DateWindow>,
) -> Result<SearchOverview> {
    let (now, first_day, last_day) = totals(ch, scope, window).await?;
    let previous = match previous_window {
        Some(w) => Some(totals(ch, scope, w).await?.0),
        None => None,
    };
    let change = previous.map(|p| SearchChange {
        clicks: pct_delta(now.clicks as f64, p.clicks as f64),
        impressions: pct_delta(now.impressions as f64, p.impressions as f64),
        ctr: pct_delta(now.ctr, p.ctr),
        position: pct_delta(now.position, p.position),
    });
    Ok(SearchOverview {
        window,
        totals: now,
        previous_window,
        previous,
        change,
        first_day,
        last_day,
    })
}

// ---------------------------------------------------------------------------
// The daily series
// ---------------------------------------------------------------------------

/// One day of the series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPoint {
    /// The day.
    pub date: NaiveDate,
    /// Clicks that day.
    pub clicks: u64,
    /// Impressions that day.
    pub impressions: u64,
    /// Click-through rate that day.
    pub ctr: f64,
    /// Average position that day; zero on a day with no impressions.
    pub position: f64,
}

#[derive(Debug, Row, Deserialize)]
struct DayRow {
    day: String,
    n_clicks: u64,
    n_impressions: u64,
    weighted: f64,
}

/// One point per day of the window, zero-filled, from the `total` grain.
///
/// Zero-filled so a day Google has nothing for draws as zero rather than as a
/// line across a gap, the same rule every series in this product keeps.
pub async fn timeseries(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
) -> Result<Vec<SearchPoint>> {
    let (pred, params) = latest(scope, GRAIN_TOTAL, window);
    let sql = format!(
        "SELECT toString(date) AS day, toUInt64(sum(clicks)) AS n_clicks,
                toUInt64(sum(impressions)) AS n_impressions,
                toFloat64(sum(position * impressions)) AS weighted
         FROM search_console
         WHERE {pred}
         GROUP BY date
         ORDER BY date"
    );
    let rows = fetch_all::<DayRow>(ch, "search_timeseries", &sql, &params).await?;
    Ok(zero_fill(window, &rows))
}

fn zero_fill(window: DateWindow, rows: &[DayRow]) -> Vec<SearchPoint> {
    let by_day: BTreeMap<&str, &DayRow> = rows.iter().map(|r| (r.day.as_str(), r)).collect();
    window
        .each_day()
        .into_iter()
        .map(|date| {
            let key = date.to_string();
            match by_day.get(key.as_str()) {
                Some(r) => SearchPoint {
                    date,
                    clicks: r.n_clicks,
                    impressions: r.n_impressions,
                    ctr: ctr(r.n_clicks, r.n_impressions),
                    position: position(r.weighted, r.n_impressions),
                },
                None => SearchPoint {
                    date,
                    clicks: 0,
                    impressions: 0,
                    ctr: 0.0,
                    position: 0.0,
                },
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Ranked lists
// ---------------------------------------------------------------------------

/// One ranked row: a search term, a page, a country or a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchRow {
    /// The search terms, the page's address, the country code or the device.
    pub key: String,
    /// For a page: its host. Empty otherwise.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hostname: String,
    /// For a page: its normalised path. Empty otherwise.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pathname: String,
    /// Clicks.
    pub clicks: u64,
    /// Impressions.
    pub impressions: u64,
    /// Click-through rate, a fraction.
    pub ctr: f64,
    /// Average position.
    pub position: f64,
}

#[derive(Debug, Row, Deserialize)]
struct RankedRow {
    key: String,
    host: String,
    path: String,
    n_clicks: u64,
    n_impressions: u64,
    weighted: f64,
}

/// What a ranked list is asked for.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    /// Narrowing. Only the search-terms report reads more than its own column.
    pub filter: QueryFilter,
    /// What to rank by.
    pub sort: SortBy,
    /// Rows per page, at most [`MAX_ROWS`].
    pub limit: u32,
    /// 1-based page.
    pub page: u32,
}

/// One ranked report: its name for the log, its grain, the column it ranks,
/// and any narrowing. All four are this module's own text.
struct Ranking {
    report: &'static str,
    grain: &'static str,
    key: &'static str,
    filter: Option<(String, Vec<Param>)>,
}

async fn ranked(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
    ranking: Ranking,
    list: &ListQuery,
) -> Result<Vec<SearchRow>> {
    let Ranking {
        report,
        grain,
        key,
        filter,
    } = ranking;
    let (mut pred, mut params) = latest(scope, grain, window);
    if let Some((extra, binds)) = filter {
        pred.push_str(" AND ");
        pred.push_str(&extra);
        params.extend(binds);
    }
    let limit = list.limit.clamp(1, MAX_ROWS);
    let offset = u64::from(list.page.max(1) - 1) * u64::from(limit);
    let sql = format!(
        "SELECT {key} AS key, any(hostname) AS host, any(pathname) AS path,
                toUInt64(sum(clicks)) AS n_clicks, toUInt64(sum(impressions)) AS n_impressions,
                toFloat64(sum(position * impressions)) AS weighted
         FROM search_console
         WHERE {pred}
         GROUP BY key
         ORDER BY {order}
         LIMIT {limit} OFFSET {offset}",
        order = list.sort.order(),
    );
    let rows = fetch_all::<RankedRow>(ch, report, &sql, &params).await?;
    let is_page = grain == GRAIN_PAGE;
    Ok(rows
        .into_iter()
        .map(|r| SearchRow {
            ctr: ctr(r.n_clicks, r.n_impressions),
            position: position(r.weighted, r.n_impressions),
            hostname: if is_page { r.host } else { String::new() },
            pathname: if is_page { r.path } else { String::new() },
            key: r.key,
            clicks: r.n_clicks,
            impressions: r.n_impressions,
        })
        .collect())
}

/// The search terms people used, from the five-dimension grain, narrowed by
/// any of the four filters.
pub async fn queries(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
    list: &ListQuery,
) -> Result<Vec<SearchRow>> {
    let ranking = Ranking {
        report: "search_queries",
        grain: GRAIN_QUERY,
        key: "query",
        filter: list.filter.predicate(),
    };
    ranked(ch, scope, window, ranking, list).await
}

/// The pages that showed up, from the page grain. Only `filter.page` narrows.
pub async fn pages(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
    list: &ListQuery,
) -> Result<Vec<SearchRow>> {
    let own = QueryFilter {
        page: list.filter.page.clone(),
        ..QueryFilter::default()
    };
    let ranking = Ranking {
        report: "search_pages",
        grain: GRAIN_PAGE,
        key: "page",
        filter: own.predicate(),
    };
    ranked(ch, scope, window, ranking, list).await
}

/// Where searchers were, from the country grain.
pub async fn countries(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
    list: &ListQuery,
) -> Result<Vec<SearchRow>> {
    let ranking = Ranking {
        report: "search_countries",
        grain: GRAIN_COUNTRY,
        key: "country",
        filter: None,
    };
    ranked(ch, scope, window, ranking, list).await
}

/// What searchers used, from the device grain.
pub async fn devices(
    ch: &Ch,
    scope: SearchScope<'_>,
    window: DateWindow,
    list: &ListQuery,
) -> Result<Vec<SearchRow>> {
    let ranking = Ranking {
        report: "search_devices",
        grain: GRAIN_DEVICE,
        key: "device",
        filter: None,
    };
    ranked(ch, scope, window, ranking, list).await
}

// ---------------------------------------------------------------------------
// Micaforge's own count for the same pages
// ---------------------------------------------------------------------------

/// What Micaforge itself counted on one page over the same window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageTraffic {
    /// The host, lower-cased, without a leading `www.`.
    pub host: String,
    /// The path.
    pub pathname: String,
    /// Visitors who viewed it.
    pub visitors: u64,
    /// Times it was viewed.
    pub pageviews: u64,
    /// Visitors whose view of it came from Google Search.
    pub from_google: u64,
}

#[derive(Debug, Row, Deserialize)]
struct TrafficRow {
    host: String,
    pathname: String,
    visitors: u64,
    pageviews: u64,
    from_google: u64,
}

/// A host as the join compares it: lower case, `www.` dropped.
///
/// Google reports the canonical address and the browser the address it
/// loaded; on most sites one of the two carries `www.` and the other does
/// not, while a site that serves different pages at `www.` and the bare
/// domain is rare enough to be the exception this accepts.
pub fn join_host(host: &str) -> String {
    let lower = host.trim().to_ascii_lowercase();
    lower
        .strip_prefix("www.")
        .map(str::to_string)
        .unwrap_or(lower)
}

/// Pageviews and visitors for a short list of paths over `range`.
///
/// Cheap by construction: one scan of the site's pageviews in the window,
/// narrowed to the listed paths, which the caller takes from one page of a
/// ranked list (at most [`MAX_JOINED`]). People only, because Google's clicks
/// are people.
pub async fn page_traffic(
    ch: &Ch,
    site: u16,
    range: &TimeRange,
    paths: &[String],
) -> Result<Vec<PageTraffic>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut unique: Vec<&String> = paths.iter().collect();
    unique.sort_unstable();
    unique.dedup();
    unique.truncate(MAX_JOINED);
    let slots = vec!["?"; unique.len()].join(", ");
    let sql = format!(
        "SELECT replaceRegexpOne(lower(hostname), '^www\\\\.', '') AS host, pathname,
                toUInt64(uniq(visitor_id)) AS visitors, toUInt64(count()) AS pageviews,
                toUInt64(uniqIf(visitor_id, channel = 'search' AND source = 'google')) AS from_google
         FROM events
         WHERE site_id = ? AND kind = 'pageview'
           AND timestamp >= toDateTime64(?, 3, 'UTC') AND timestamp < toDateTime64(?, 3, 'UTC')
           AND pathname IN ({slots})
         GROUP BY host, pathname"
    );
    let mut params = vec![
        Param::U16(site),
        Param::Str(fmt_ts(range.start)),
        Param::Str(fmt_ts(range.end)),
    ];
    params.extend(unique.into_iter().map(|p| Param::Str(p.clone())));
    let rows = fetch_all::<TrafficRow>(ch, "search_page_traffic", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .map(|r| PageTraffic {
            host: r.host,
            pathname: r.pathname,
            visitors: r.visitors,
            pageviews: r.pageviews,
            from_google: r.from_google,
        })
        .collect())
}

/// Most paths one [`page_traffic`] call reads.
pub const MAX_JOINED: usize = 200;

/// A page row with Micaforge's own count beside Google's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinedPage {
    /// Google's figures for the page.
    #[serde(flatten)]
    pub search: SearchRow,
    /// Micaforge's visitors on the same page, `None` when the page had no
    /// pageview in the window (or is outside the first [`MAX_JOINED`]).
    pub visitors: Option<u64>,
    /// Micaforge's pageviews.
    pub pageviews: Option<u64>,
    /// Micaforge's visitors who came from Google Search.
    pub from_google: Option<u64>,
}

/// Lay Micaforge's count beside each page Google reported.
pub fn join_pages(pages: Vec<SearchRow>, traffic: &[PageTraffic]) -> Vec<JoinedPage> {
    let index: BTreeMap<(String, &str), &PageTraffic> = traffic
        .iter()
        .map(|t| ((t.host.clone(), t.pathname.as_str()), t))
        .collect();
    pages
        .into_iter()
        .map(|search| {
            let hit = index
                .get(&(join_host(&search.hostname), search.pathname.as_str()))
                .copied();
            JoinedPage {
                visitors: hit.map(|t| t.visitors),
                pageviews: hit.map(|t| t.pageviews),
                from_google: hit.map(|t| t.from_google),
                search,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::placeholder_count;
    use chrono::TimeZone;

    fn range(start: &str, end: &str, tz: chrono_tz::Tz) -> TimeRange {
        time::resolve_range(None, Some(start), Some(end), tz).unwrap()
    }

    #[test]
    fn a_window_is_the_days_it_touches_in_its_own_zone() {
        let w = DateWindow::of_range(&range("2026-09-01", "2026-09-07", chrono_tz::UTC));
        assert_eq!(w.start.to_string(), "2026-09-01");
        assert_eq!(
            w.end.to_string(),
            "2026-09-07",
            "a bare end date includes that day"
        );
        assert_eq!(w.days(), 7);
        assert_eq!(w.each_day().len(), 7);

        // Read in the window's zone, not in UTC: midnight in Tokyo is the
        // previous afternoon in UTC.
        let tokyo: chrono_tz::Tz = "Asia/Tokyo".parse().unwrap();
        let w = DateWindow::of_range(&range("2026-09-01", "2026-09-01", tokyo));
        assert_eq!(
            (w.start.to_string(), w.days()),
            ("2026-09-01".to_string(), 1)
        );

        // A window under a day is that day.
        let start = chrono::Utc.with_ymd_and_hms(2026, 9, 3, 10, 0, 0).unwrap();
        let short = TimeRange::new(start, start + Duration::minutes(5), chrono_tz::UTC);
        assert_eq!(DateWindow::of_range(&short).days(), 1);
    }

    #[test]
    fn the_comparison_window_has_the_same_length_and_abuts() {
        let r = range("2026-09-08", "2026-09-14", chrono_tz::UTC);
        let prev = previous_window(&r, Compare::Previous).unwrap();
        assert_eq!(prev.start.to_string(), "2026-09-01");
        assert_eq!(prev.end.to_string(), "2026-09-07");
        assert!(previous_window(&r, Compare::None).is_none());
    }

    #[test]
    fn the_newest_fetch_predicate_binds_every_slot() {
        let scope = SearchScope {
            site: 3,
            property: "sc-domain:example.com",
        };
        let w = DateWindow::of_range(&range("2026-09-01", "2026-09-07", chrono_tz::UTC));
        let (pred, params) = latest(scope, GRAIN_TOTAL, w);
        assert_eq!(placeholder_count(&pred), params.len());
        assert!(pred.contains("max(synced_at)"));

        let filter = QueryFilter {
            query: Some("pricing".into()),
            page: Some("/docs".into()),
            country: Some("de".into()),
            device: Some("Mobile".into()),
        }
        .checked()
        .unwrap();
        assert_eq!(filter.country.as_deref(), Some("DE"));
        assert_eq!(filter.device.as_deref(), Some("mobile"));
        let (extra, binds) = filter.predicate().unwrap();
        assert_eq!(placeholder_count(&extra), binds.len());
    }

    #[test]
    fn filters_refuse_what_the_table_cannot_hold() {
        let bad_country = QueryFilter {
            country: Some("USA".into()),
            ..QueryFilter::default()
        };
        assert_eq!(bad_country.checked().unwrap_err().field(), Some("country"));
        let bad_device = QueryFilter {
            device: Some("watch".into()),
            ..QueryFilter::default()
        };
        assert_eq!(bad_device.checked().unwrap_err().field(), Some("device"));
        assert!(QueryFilter::default()
            .checked()
            .unwrap()
            .predicate()
            .is_none());
        assert!(SortBy::parse(Some("bounce")).is_err());
        assert_eq!(SortBy::parse(None).unwrap(), SortBy::Clicks);
    }

    #[test]
    fn rates_come_from_sums_and_positions_are_weighted() {
        assert_eq!(ctr(5, 0), 0.0);
        assert!((ctr(5, 100) - 0.05).abs() < 1e-9);
        // Two rows: position 1 on 100 impressions, position 10 on 900. The
        // plain average would be 5.5; Google's weighted figure is 9.1.
        assert_eq!(position(1.0 * 100.0 + 10.0 * 900.0, 1_000), 9.1);
        assert_eq!(position(0.0, 0), 0.0);
    }

    #[test]
    fn a_series_has_one_point_per_day_with_gaps_as_zero() {
        let w = DateWindow::of_range(&range("2026-09-01", "2026-09-03", chrono_tz::UTC));
        let rows = vec![DayRow {
            day: "2026-09-02".into(),
            n_clicks: 4,
            n_impressions: 80,
            weighted: 240.0,
        }];
        let points = zero_fill(w, &rows);
        assert_eq!(points.len(), 3);
        assert_eq!(points[0].clicks, 0);
        assert_eq!(points[1].clicks, 4);
        assert_eq!(points[1].position, 3.0);
        assert_eq!(points[2].impressions, 0);
    }

    #[test]
    fn pages_are_joined_on_host_without_www_and_path() {
        let row = |host: &str, path: &str| SearchRow {
            key: format!("https://{host}{path}"),
            hostname: host.into(),
            pathname: path.into(),
            clicks: 1,
            impressions: 10,
            ctr: 0.1,
            position: 2.0,
        };
        let traffic = vec![PageTraffic {
            host: "example.com".into(),
            pathname: "/docs".into(),
            visitors: 12,
            pageviews: 20,
            from_google: 7,
        }];
        let joined = join_pages(
            vec![row("www.example.com", "/docs"), row("example.com", "/blog")],
            &traffic,
        );
        assert_eq!(joined[0].visitors, Some(12));
        assert_eq!(joined[0].from_google, Some(7));
        assert_eq!(
            joined[1].visitors, None,
            "no pageview is not zero pageviews"
        );
        assert_eq!(join_host("WWW.Example.COM"), "example.com");
    }
}
