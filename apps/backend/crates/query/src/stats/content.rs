//! What was read, where the readers came from, and on what.
//!
//! Every report here is session-scoped, so it reads the rollup rebuilt from the
//! filtered event stream ([`crate::sql::sessions_subquery`]) rather than the
//! stored `sessions_mv`, which cannot answer a filtered question. `pages` is the
//! exception: it needs per-row ordering inside a session, so it works on the
//! event stream directly.

use clickhouse::Row;
use micaforge_core::{Audience, Dimension, Metric, QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, round2};
use crate::sql::{compile_union_filters, f64_of, scope_predicate, sessions_subquery, Param};

/// Rows on each list inside a composite report (sources, locations, devices).
///
/// A composite endpoint answers several questions at once and each answer is
/// read as a top list, not paged through; a caller who wants the long tail asks
/// `/api/stats/breakdown` for that one dimension.
const COMPOSITE_TOP_N: u32 = 24;

// ---------------------------------------------------------------------------
// Generic breakdown
// ---------------------------------------------------------------------------

/// One ranked row of a breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakdownRow {
    /// The raw value, exactly as stored. Empty string when the dimension was
    /// not recorded on those hits.
    pub key: String,
    /// What to show. Differs from `key` only where the raw value is unreadable
    /// on its own — an empty value becomes `(not set)`.
    pub label: String,
    /// Distinct visitors, or distinct agents when the audience is agents.
    pub visitors: u64,
    /// Distinct sessions, or distinct fetching hosts for agents.
    pub sessions: u64,
    /// Pageviews, or fetches for agents.
    pub pageviews: u64,
    /// Percentage of this row's sessions that were single-page. Absent for
    /// audiences with no sessions.
    pub bounce_rate: Option<f64>,
    /// Mean engagement per session in seconds. Absent for agents.
    pub avg_duration_s: Option<f64>,
    /// This row's share of the ranked metric across every row, 0 to 1.
    ///
    /// The denominator is every group the query produced, not just this page of
    /// them, so the shares on page two are still shares of the whole.
    pub share: f64,
}

#[derive(Debug, Row, Deserialize)]
struct BreakdownRawRow {
    key: String,
    visitors: u64,
    sessions: u64,
    pageviews: u64,
    bounce_rate: f64,
    avg_duration_s: f64,
    share: f64,
}

#[derive(Debug, Row, Deserialize)]
struct CountedRawRow {
    key: String,
    visitors: u64,
    sessions: u64,
    pageviews: u64,
    share: f64,
}

/// Turn a stored value into something readable without inventing one.
///
/// The only substitution made is for an empty string. A missing country is not
/// renamed to a country, and a missing campaign is not renamed to `direct`; it
/// is named as the absence it is.
fn label_for(key: &str) -> String {
    if key.is_empty() {
        "(not set)".to_string()
    } else {
        key.to_string()
    }
}

/// The same as [`label_for`], but knowing which dimension it is labelling.
///
/// An empty value is not always "unknown". For `source` and `referrer_host` an
/// empty value means the visitor arrived with no referrer at all, which is
/// Direct — a real, named channel. `/api/stats/sources` already coalesces it
/// that way, so labelling it "(not set)" here made two endpoints report the
/// same fact differently, which is the one thing an analytics tool may not do.
fn label_for_dim(dimension: &Dimension, key: &str) -> String {
    if !key.is_empty() {
        return key.to_string();
    }
    match dimension {
        Dimension::Source | Dimension::ReferrerHost => "Direct".to_string(),
        Dimension::Country | Dimension::Region | Dimension::City => "(unknown)".to_string(),
        _ => "(not set)".to_string(),
    }
}

/// The rollup column a metric ranks by.
///
/// A breakdown row always carries the same set of numbers; `metrics` chooses
/// which one the ranking is done on, because "top pages" and "slowest pages" are
/// the same table sorted differently.
fn rank_column(metrics: &[Metric]) -> &'static str {
    match metrics.first() {
        Some(Metric::Sessions) => "sessions",
        Some(Metric::Pageviews) | Some(Metric::Events) => "pageviews",
        Some(Metric::BounceRate) => "bounce_rate",
        Some(Metric::AvgDuration) => "avg_duration_s",
        _ => "visitors",
    }
}

/// `GET /api/stats/breakdown`.
///
/// A session is attributed to every value of the dimension it touched, which is
/// what makes "sessions by path" mean "sessions that saw this path". The shares
/// therefore describe the ranked column and not a partition of the visitors.
pub async fn breakdown(
    ch: &Ch,
    env: &QueryEnvelope,
    dimension: &Dimension,
    metrics: &[Metric],
) -> Result<Vec<BreakdownRow>> {
    let rank = rank_column(metrics);
    // The non-human traces carry no session quality, so a request to rank by it
    // falls back to visitors rather than naming a column that is not there.
    let counted_rank = match rank {
        "sessions" => "sessions",
        "pageviews" => "pageviews",
        _ => "visitors",
    };

    match env.audience {
        Audience::Human => {
            let (key_expr, key_params) = dimension.column_expr();
            let (sub, params) = sessions_subquery(
                env,
                Some((
                    &key_expr,
                    key_params.into_iter().map(Param::Str).collect::<Vec<_>>(),
                )),
                None,
            )?;
            let sql = format!(
                "SELECT key, visitors, sessions, pageviews_total AS pageviews, bounce_rate, avg_duration_s,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT
    toString(bkey) AS key,
    uniq(visitor_id) AS visitors,
    uniq(session_id) AS sessions,
    -- see the note in stats/overview.rs: aliasing this to `pageviews` would
    -- shadow the source column for the two expressions below.
    toUInt64(sum(pageviews)) AS pageviews_total,
    {bounce} AS bounce_rate,
    {duration} AS avg_duration_s
  FROM ({sub})
  GROUP BY key
)
ORDER BY {rank} DESC, key ASC
LIMIT {limit} OFFSET {offset}",
                bounce = f64_of(Metric::BounceRate.select_expr()),
                duration = f64_of(Metric::AvgDuration.select_expr()),
                limit = env.limit.clamp(1, 10_000),
                offset = env.offset(),
            );
            let rows = fetch_all::<BreakdownRawRow>(ch, "breakdown", &sql, &params).await?;
            Ok(rows
                .into_iter()
                .map(|r| BreakdownRow {
                    label: label_for_dim(dimension, &r.key),
                    key: r.key,
                    visitors: r.visitors,
                    sessions: r.sessions,
                    pageviews: r.pageviews,
                    bounce_rate: Some(round2(r.bounce_rate)),
                    avg_duration_s: Some(round2(r.avg_duration_s)),
                    share: round2(r.share * 100.0) / 100.0,
                })
                .collect())
        }

        // The agent trace. `visitors` is distinct agents and `sessions` is
        // distinct fetching hosts, exactly as on the overview, so the toggle
        // does not quietly redefine a column between two panels.
        Audience::Agent => {
            let (key_expr, key_params) = dimension.column_expr();
            let mut params: Vec<Param> = key_params.into_iter().map(Param::Str).collect();
            let (scope, scope_params) = scope_predicate(env, Table::AgentEvents)?;
            params.extend(scope_params);
            let sql = format!(
                "SELECT key, visitors, sessions, pageviews,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT toString({key_expr}) AS key,
         uniq(agent_id) AS visitors,
         uniq(ip_hash) AS sessions,
         count() AS pageviews
  FROM agent_events
  WHERE {scope}
  GROUP BY key
)
ORDER BY {counted_rank} DESC, key ASC
LIMIT {limit} OFFSET {offset}",
                limit = env.limit.clamp(1, 10_000),
                offset = env.offset(),
            );
            let rows = fetch_all::<CountedRawRow>(ch, "breakdown_agent", &sql, &params).await?;
            Ok(rows
                .into_iter()
                .map(|r| counted_row(dimension, r))
                .collect())
        }

        Audience::All => {
            let (key_expr, key_params) = dimension.column_expr();
            let (union, union_params) = crate::sql::union_projection(env);
            // Placeholder order follows the statement: the key expression is in
            // the inner SELECT list, the union is in its FROM, and the filter
            // predicate comes after both.
            let mut params: Vec<Param> = key_params.into_iter().map(Param::Str).collect();
            params.extend(union_params);
            let mut where_clause = String::new();
            if let Some((predicate, filter_params)) = compile_union_filters(env.filters.as_ref())? {
                where_clause = format!("\n  WHERE {predicate}");
                params.extend(filter_params);
            }
            let sql = format!(
                "SELECT key, visitors, sessions, pageviews,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT toString({key_expr}) AS key,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions,
         toUInt64(countIf(kind = 'pageview') + countIf(is_agent = 1)) AS pageviews
  FROM ({union}){where_clause}
  GROUP BY key
)
ORDER BY {counted_rank} DESC, key ASC
LIMIT {limit} OFFSET {offset}",
                limit = env.limit.clamp(1, 10_000),
                offset = env.offset(),
            );
            let rows = fetch_all::<CountedRawRow>(ch, "breakdown_all", &sql, &params).await?;
            Ok(rows
                .into_iter()
                .map(|r| counted_row(dimension, r))
                .collect())
        }
    }
}

/// A row from a trace with no session quality to report.
fn counted_row(dimension: &Dimension, r: CountedRawRow) -> BreakdownRow {
    BreakdownRow {
        label: label_for_dim(dimension, &r.key),
        key: r.key,
        visitors: r.visitors,
        sessions: r.sessions,
        pageviews: r.pageviews,
        bounce_rate: None,
        avg_duration_s: None,
        share: round2(r.share * 100.0) / 100.0,
    }
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// One page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRow {
    /// Path, without host or query string.
    pub path: String,
    /// The most recent `document.title` seen on it.
    pub title: String,
    /// Pageviews.
    pub pageviews: u64,
    /// Distinct visitors.
    pub visitors: u64,
    /// Sessions that began here.
    pub entries: u64,
    /// Sessions that ended here.
    pub exits: u64,
    /// Of the sessions that began here, the percentage that saw one page.
    pub bounce_rate: f64,
    /// Mean engagement time on this page, in seconds. See the note on
    /// attribution in [`pages`].
    pub avg_time_s: f64,
    /// Mean furthest scroll depth reached, as a percentage, over the hits that
    /// reported one.
    pub avg_scroll_pct: f64,
}

#[derive(Debug, Row, Deserialize)]
struct PageRawRow {
    path: String,
    title: String,
    pageviews: u64,
    visitors: u64,
    entries: u64,
    exits: u64,
    bounce_rate: f64,
    avg_time_s: f64,
    avg_scroll_pct: f64,
}

/// `GET /api/stats/pages`.
///
/// Always reads the human trace. What an agent did to a URL is a different
/// question with a different shape, and it is answered by
/// [`crate::agents_q::coverage`], not by adding crawler rows to a table of
/// entries, exits and scroll depth.
///
/// **Time attribution.** `docs/CONTRACT.md` §1.1 defines `duration_ms` on an
/// event as the engagement time on the *previous* page: the tracker measures a
/// page and reports it when the visitor leaves. Attributing that number to the
/// path on its own row would credit every page's time to the page after it. So
/// the query walks each session in order and attributes a pageview's
/// `duration_ms` to the path that preceded it, while an `engagement` event —
/// which names the page it measured — keeps its own.
///
/// **Bounce rate on a page** is measured over the sessions that *entered* on it.
/// A page cannot be bounced out of by a visitor who arrived somewhere else.
pub async fn pages(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<PageRow>> {
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;

    // One row per hit, carrying three facts the row itself does not have: which
    // path the previous hit was on, and the session's own entry and exit. The
    // block is used twice below — once to count, once to attribute time — so its
    // parameters are bound twice, in that order.
    let hits = format!(
        "    SELECT
      session_id,
      visitor_id,
      pathname,
      page_title,
      timestamp,
      kind,
      scroll_pct,
      duration_ms,
      if(kind = 'pageview',
         lagInFrame(pathname) OVER (PARTITION BY session_id ORDER BY timestamp ASC
                                    ROWS BETWEEN 1 PRECEDING AND CURRENT ROW),
         pathname) AS time_path,
      argMin(pathname, timestamp) OVER (PARTITION BY session_id) AS s_entry,
      argMax(pathname, timestamp) OVER (PARTITION BY session_id) AS s_exit,
      countIf(kind = 'pageview') OVER (PARTITION BY session_id) AS s_pv
    FROM events
    WHERE {scope}"
    );

    let mut params = scope_params.clone();
    params.extend(scope_params);

    let bounce = f64_of(
        "round(100 * uniqIf(session_id, pathname = s_entry AND s_pv <= 1) \
         / nullIf(uniqIf(session_id, pathname = s_entry), 0), 2)",
    );

    let sql = format!(
        "SELECT core.path, core.title, core.pageviews, core.visitors, core.entries, core.exits,
       core.bounce_rate, {time_s} AS avg_time_s, core.avg_scroll_pct
FROM (
  SELECT
    pathname AS path,
    argMax(page_title, timestamp) AS title,
    toUInt64(countIf(kind = 'pageview')) AS pageviews,
    uniq(visitor_id) AS visitors,
    uniqIf(session_id, pathname = s_entry) AS entries,
    uniqIf(session_id, pathname = s_exit) AS exits,
    {bounce} AS bounce_rate,
    {scroll} AS avg_scroll_pct
  FROM (
{hits}
  )
  GROUP BY pathname
) AS core
LEFT JOIN (
  SELECT time_path AS tpath, toUInt64(sum(duration_ms)) AS time_ms
  FROM (
{hits}
  )
  WHERE time_path != ''
  GROUP BY tpath
) AS timed ON timed.tpath = core.path
ORDER BY core.pageviews DESC, core.path ASC
LIMIT {limit} OFFSET {offset}",
        scroll = f64_of("round(avgIf(scroll_pct, scroll_pct > 0), 1)"),
        time_s = f64_of(
            "round(toFloat64(timed.time_ms) / 1000 / nullIf(toFloat64(core.pageviews), 0), 1)"
        ),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let rows = fetch_all::<PageRawRow>(ch, "pages", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .map(|r| PageRow {
            path: r.path,
            title: r.title,
            pageviews: r.pageviews,
            visitors: r.visitors,
            entries: r.entries,
            exits: r.exits,
            bounce_rate: round2(r.bounce_rate),
            avg_time_s: round2(r.avg_time_s),
            avg_scroll_pct: round2(r.avg_scroll_pct),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// One acquisition channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelRow {
    /// One of [`micaforge_core::Channel`]. `answer_engine` sits here beside
    /// `search`, which is the whole argument of the product.
    pub channel: String,
    /// Distinct visitors.
    pub visitors: u64,
    /// Distinct sessions.
    pub sessions: u64,
    /// Pageviews.
    pub pageviews: u64,
    /// Single-page share of this channel's sessions.
    pub bounce_rate: f64,
    /// Mean engagement per session, in seconds.
    pub avg_duration_s: f64,
    /// Share of all visitors, 0 to 1.
    pub share: f64,
}

/// One resolved source inside a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRow {
    /// The resolved source name: `google`, `chatgpt`, `github`, or the referring
    /// host when nothing resolved, or `direct`.
    pub source: String,
    /// The channel it was bucketed into.
    pub channel: String,
    /// The raw referring host, kept so a resolution can be audited.
    pub referrer_host: String,
    /// Distinct visitors.
    pub visitors: u64,
    /// Distinct sessions.
    pub sessions: u64,
    /// Pageviews.
    pub pageviews: u64,
    /// Share of all visitors, 0 to 1.
    pub share: f64,
}

/// One UTM campaign.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignRow {
    /// `utm_campaign`.
    pub campaign: String,
    /// `utm_source`.
    pub source: String,
    /// `utm_medium`.
    pub medium: String,
    /// Distinct visitors.
    pub visitors: u64,
    /// Distinct sessions.
    pub sessions: u64,
    /// Pageviews.
    pub pageviews: u64,
    /// Share of campaign visitors, 0 to 1.
    pub share: f64,
}

/// Channels, then sources, then campaigns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourcesTree {
    /// Top-level buckets.
    pub channels: Vec<ChannelRow>,
    /// Named sources.
    pub sources: Vec<SourceRow>,
    /// Tagged campaigns.
    pub campaigns: Vec<CampaignRow>,
}

#[derive(Debug, Row, Deserialize)]
struct SourceRawRow {
    source: String,
    channel: String,
    referrer_host: String,
    visitors: u64,
    sessions: u64,
    pageviews: u64,
    share: f64,
}

#[derive(Debug, Row, Deserialize)]
struct CampaignRawRow {
    campaign: String,
    source: String,
    medium: String,
    visitors: u64,
    sessions: u64,
    pageviews: u64,
    share: f64,
}

/// `GET /api/stats/sources`.
///
/// Three levels of the same question, answered together because they are read
/// together. `answer_engine` is an ordinary channel here — no special case, no
/// footnote — which is exactly the claim being made: an AI answer surface is an
/// acquisition channel like search, not an anomaly in the referrer list.
pub async fn sources(ch: &Ch, env: &QueryEnvelope) -> Result<SourcesTree> {
    let (sub, params) = sessions_subquery(env, None, None)?;

    let channels_sql = format!(
        "SELECT key AS channel, visitors, sessions, pageviews_total AS pageviews, bounce_rate, avg_duration_s,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT toString(channel) AS key,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions,
         -- see the note in stats/overview.rs.
         toUInt64(sum(pageviews)) AS pageviews_total,
         {bounce} AS bounce_rate,
         {duration} AS avg_duration_s
  FROM ({sub})
  GROUP BY key
)
ORDER BY visitors DESC, channel ASC
LIMIT {COMPOSITE_TOP_N}",
        bounce = f64_of(Metric::BounceRate.select_expr()),
        duration = f64_of(Metric::AvgDuration.select_expr()),
    );

    #[derive(Debug, Row, Deserialize)]
    struct ChannelRawRow {
        channel: String,
        visitors: u64,
        sessions: u64,
        pageviews: u64,
        bounce_rate: f64,
        avg_duration_s: f64,
        share: f64,
    }

    let channels = fetch_all::<ChannelRawRow>(ch, "sources_channels", &channels_sql, &params)
        .await?
        .into_iter()
        .map(|r| ChannelRow {
            channel: r.channel,
            visitors: r.visitors,
            sessions: r.sessions,
            pageviews: r.pageviews,
            bounce_rate: round2(r.bounce_rate),
            avg_duration_s: round2(r.avg_duration_s),
            share: round2(r.share * 100.0) / 100.0,
        })
        .collect();

    let sources_sql = format!(
        "SELECT src AS source, chan AS channel, refhost AS referrer_host,
       visitors, sessions, pageviews,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT if(source != '', toString(source),
            if(referrer_host != '', referrer_host, 'direct')) AS src,
         any(toString(channel)) AS chan,
         any(referrer_host) AS refhost,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions,
         toUInt64(sum(pageviews)) AS pageviews
  FROM ({sub})
  GROUP BY src
)
ORDER BY visitors DESC, source ASC
LIMIT {COMPOSITE_TOP_N}"
    );

    let sources = fetch_all::<SourceRawRow>(ch, "sources_sources", &sources_sql, &params)
        .await?
        .into_iter()
        .map(|r| SourceRow {
            source: r.source,
            channel: r.channel,
            referrer_host: r.referrer_host,
            visitors: r.visitors,
            sessions: r.sessions,
            pageviews: r.pageviews,
            share: round2(r.share * 100.0) / 100.0,
        })
        .collect();

    let campaigns_sql = format!(
        "SELECT camp AS campaign, src AS source, med AS medium,
       visitors, sessions, pageviews,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT utm_campaign AS camp,
         any(utm_source) AS src,
         any(utm_medium) AS med,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions,
         toUInt64(sum(pageviews)) AS pageviews
  FROM ({sub})
  WHERE utm_campaign != ''
  GROUP BY camp
)
ORDER BY visitors DESC, campaign ASC
LIMIT {COMPOSITE_TOP_N}"
    );

    let campaigns = fetch_all::<CampaignRawRow>(ch, "sources_campaigns", &campaigns_sql, &params)
        .await?
        .into_iter()
        .map(|r| CampaignRow {
            campaign: r.campaign,
            source: r.source,
            medium: r.medium,
            visitors: r.visitors,
            sessions: r.sessions,
            pageviews: r.pageviews,
            share: round2(r.share * 100.0) / 100.0,
        })
        .collect();

    Ok(SourcesTree {
        channels,
        sources,
        campaigns,
    })
}

// ---------------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------------

/// One place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoRow {
    /// A stable key a map can join on. For a country this is the ISO-3166
    /// alpha-2 code; for a region or a city it is the code, a hyphen, and the
    /// name, because city names repeat across countries.
    pub key: String,
    /// What to show.
    pub label: String,
    /// The country this row belongs to.
    pub country: String,
    /// Distinct visitors.
    pub visitors: u64,
    /// Distinct sessions.
    pub sessions: u64,
    /// Share of all visitors at this level, 0 to 1.
    pub share: f64,
}

/// Country, region and city, together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Locations {
    /// By country.
    pub countries: Vec<GeoRow>,
    /// By region or subdivision.
    pub regions: Vec<GeoRow>,
    /// By city.
    pub cities: Vec<GeoRow>,
}

#[derive(Debug, Row, Deserialize)]
struct GeoRawRow {
    key: String,
    label: String,
    country: String,
    visitors: u64,
    sessions: u64,
    share: f64,
}

/// `GET /api/stats/locations`.
///
/// Geography is derived from the address at ingest and the address itself is
/// never stored, so this is the finest resolution that exists: there is no row
/// to drill further into.
pub async fn locations(ch: &Ch, env: &QueryEnvelope) -> Result<Locations> {
    let (sub, params) = sessions_subquery(env, None, None)?;

    fn level(sub: &str, key: &str, label: &str) -> String {
        format!(
            "SELECT key, label, cc AS country, visitors, sessions,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT {key} AS key,
         {label} AS label,
         any(country) AS cc,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions
  FROM ({sub})
  WHERE country != ''
  GROUP BY key, label
)
ORDER BY visitors DESC, label ASC
LIMIT {COMPOSITE_TOP_N}"
        )
    }

    fn to_geo(rows: Vec<GeoRawRow>) -> Vec<GeoRow> {
        rows.into_iter()
            .map(|r| GeoRow {
                key: r.key,
                label: label_for(&r.label),
                country: r.country,
                visitors: r.visitors,
                sessions: r.sessions,
                share: round2(r.share * 100.0) / 100.0,
            })
            .collect()
    }

    let countries = to_geo(
        fetch_all::<GeoRawRow>(
            ch,
            "locations_countries",
            &level(&sub, "country", "country"),
            &params,
        )
        .await?,
    );
    let regions = to_geo(
        fetch_all::<GeoRawRow>(
            ch,
            "locations_regions",
            &level(&sub, "concat(country, '-', region)", "region"),
            &params,
        )
        .await?,
    );
    let cities = to_geo(
        fetch_all::<GeoRawRow>(
            ch,
            "locations_cities",
            &level(&sub, "concat(country, '-', city)", "city"),
            &params,
        )
        .await?,
    );

    Ok(Locations {
        countries,
        regions,
        cities,
    })
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// One device attribute value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRow {
    /// The stored value.
    pub key: String,
    /// What to show.
    pub label: String,
    /// Distinct visitors.
    pub visitors: u64,
    /// Distinct sessions.
    pub sessions: u64,
    /// Share of all visitors at this level, 0 to 1.
    pub share: f64,
}

/// Browser, operating system, device class and screen class.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Devices {
    /// By browser.
    pub browsers: Vec<DeviceRow>,
    /// By operating system.
    pub os: Vec<DeviceRow>,
    /// By device class.
    pub device_types: Vec<DeviceRow>,
    /// By screen class, bucketed from the reported screen width.
    pub screen_classes: Vec<DeviceRow>,
}

#[derive(Debug, Row, Deserialize)]
struct DeviceRawRow {
    key: String,
    visitors: u64,
    sessions: u64,
    share: f64,
}

/// `GET /api/stats/devices`.
///
/// The first three read the session rollup. Screen class reads the event stream,
/// because it is derived from `screen_w` and the rollup does not carry a screen
/// width — deliberately, so that a breakpoint table is never frozen into stored
/// data.
pub async fn devices(ch: &Ch, env: &QueryEnvelope) -> Result<Devices> {
    let (sub, params) = sessions_subquery(env, None, None)?;

    fn rollup_level(sub: &str, key: &str) -> String {
        format!(
            "SELECT key, visitors, sessions,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT toString({key}) AS key,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions
  FROM ({sub})
  GROUP BY key
)
ORDER BY visitors DESC, key ASC
LIMIT {COMPOSITE_TOP_N}"
        )
    }

    fn to_rows(rows: Vec<DeviceRawRow>) -> Vec<DeviceRow> {
        rows.into_iter()
            .map(|r| DeviceRow {
                label: label_for(&r.key),
                key: r.key,
                visitors: r.visitors,
                sessions: r.sessions,
                share: round2(r.share * 100.0) / 100.0,
            })
            .collect()
    }

    let browsers = to_rows(
        fetch_all::<DeviceRawRow>(
            ch,
            "devices_browsers",
            &rollup_level(&sub, "browser"),
            &params,
        )
        .await?,
    );
    let os = to_rows(
        fetch_all::<DeviceRawRow>(ch, "devices_os", &rollup_level(&sub, "os"), &params).await?,
    );
    let device_types = to_rows(
        fetch_all::<DeviceRawRow>(
            ch,
            "devices_types",
            &rollup_level(&sub, "device_type"),
            &params,
        )
        .await?,
    );

    let (screen_expr, _) = Dimension::ScreenClass.column_expr();
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;
    let screen_sql = format!(
        "SELECT key, visitors, sessions,
       toFloat64(ifNull(visitors / nullIf(sum(visitors) OVER (), 0), 0)) AS share
FROM (
  SELECT {screen_expr} AS key,
         uniq(visitor_id) AS visitors,
         uniq(session_id) AS sessions
  FROM events
  WHERE {scope}
  GROUP BY key
)
ORDER BY visitors DESC, key ASC
LIMIT {COMPOSITE_TOP_N}"
    );
    let screen_classes = to_rows(
        fetch_all::<DeviceRawRow>(ch, "devices_screens", &screen_sql, &scope_params).await?,
    );

    Ok(Devices {
        browsers,
        os,
        device_types,
        screen_classes,
    })
}
