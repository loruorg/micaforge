//! The three reports the dashboard opens with: the totals, the series behind
//! them, and the live view.

use chrono::{Duration, Utc};
use clickhouse::Row;
use micaforge_core::{
    time::previous_period, Audience, Metric, MetricUnit, QueryEnvelope, Result, Table,
};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, fetch_optional, pct_delta_u64, round2};
use crate::sql::{
    bucket_millis, envelope_over, f64_of, fmt_ts, millis, scope_predicate, sessions_subquery, Param,
};
use crate::stats::GoalDef;

/// How far back "now" reaches on the realtime view, in minutes.
///
/// `docs/CONTRACT.md` §3.3 says five, and five is what a person means by "who is
/// on the site right now" — long enough to survive a page being read, short
/// enough that a number falling to zero means the traffic stopped.
const REALTIME_WINDOW_MINUTES: i64 = 5;

/// How much history the realtime sparkline shows, in minutes.
///
/// The live counter answers "now"; the sparkline beside it has to answer
/// "compared with what", and half an hour is the shortest window in which a
/// launch post is visibly a launch post.
const REALTIME_TREND_MINUTES: i64 = 30;

/// Rows on the realtime side lists.
const REALTIME_TOP_N: u32 = 8;

/// Hits on the realtime feed.
const REALTIME_FEED_N: u32 = 25;

// ---------------------------------------------------------------------------
// Overview
// ---------------------------------------------------------------------------

/// Site totals for one window.
///
/// The three quality metrics are `Option` rather than zero on purpose. A crawler
/// has no session to bounce out of and no time on page, so for
/// [`Audience::Agent`] and [`Audience::All`] those fields are absent. A zero
/// there would read as "nothing bounced", which is a measurement, not an
/// absence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overview {
    /// Distinct visitors, or distinct named agents when the audience is agents.
    pub visitors: u64,
    /// Distinct sessions, or distinct fetching hosts for agents.
    pub sessions: u64,
    /// Pageviews, or fetches for agents.
    pub pageviews: u64,
    /// Percentage of sessions with one pageview or fewer.
    pub bounce_rate: Option<f64>,
    /// Mean engagement time per session, in seconds.
    pub avg_duration_s: Option<f64>,
    /// Mean pageviews per session.
    pub views_per_session: Option<f64>,
    /// The same totals over the comparison window.
    ///
    /// Boxed because a struct cannot contain itself by value. The JSON is
    /// identical either way: `serde` sees straight through a `Box`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare: Option<Box<Overview>>,
    /// Percentage change against [`Overview::compare`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<OverviewDelta>,
}

/// Percentage change per metric, against the comparison window.
///
/// Every field is optional for one reason: a metric whose earlier value was zero
/// has no percentage change. See [`crate::exec::pct_delta`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverviewDelta {
    /// Change in visitors.
    pub visitors: Option<f64>,
    /// Change in sessions.
    pub sessions: Option<f64>,
    /// Change in pageviews.
    pub pageviews: Option<f64>,
    /// Change in bounce rate, in percentage points of the original rate.
    pub bounce_rate: Option<f64>,
    /// Change in mean session duration.
    pub avg_duration_s: Option<f64>,
    /// Change in pageviews per session.
    pub views_per_session: Option<f64>,
}

#[derive(Debug, Row, Deserialize)]
struct HumanTotalsRow {
    visitors: u64,
    sessions: u64,
    pageviews: u64,
    bounce_rate: f64,
    avg_duration_s: f64,
    views_per_session: f64,
}

#[derive(Debug, Row, Deserialize)]
struct CountsRow {
    visitors: u64,
    sessions: u64,
    pageviews: u64,
}

/// `GET /api/stats/overview`.
///
/// Runs the comparison window as a second pass when the envelope asks for one,
/// with the filters and the audience held identical, so a delta can only ever be
/// a change in the data and never a change in the question.
pub async fn overview(ch: &Ch, env: &QueryEnvelope) -> Result<Overview> {
    let mut current = totals(ch, env).await?;

    if let Some(prev) = previous_period(&env.range, env.compare) {
        let earlier = totals(ch, &envelope_over(env, &prev)).await?;
        current.delta = Some(OverviewDelta {
            visitors: pct_delta_u64(current.visitors, earlier.visitors),
            sessions: pct_delta_u64(current.sessions, earlier.sessions),
            pageviews: pct_delta_u64(current.pageviews, earlier.pageviews),
            bounce_rate: opt_delta(current.bounce_rate, earlier.bounce_rate),
            avg_duration_s: opt_delta(current.avg_duration_s, earlier.avg_duration_s),
            views_per_session: opt_delta(current.views_per_session, earlier.views_per_session),
        });
        current.compare = Some(Box::new(earlier));
    }

    Ok(current)
}

/// A delta only exists when both halves were measured.
fn opt_delta(current: Option<f64>, earlier: Option<f64>) -> Option<f64> {
    match (current, earlier) {
        (Some(c), Some(e)) => crate::exec::pct_delta(c, e),
        _ => None,
    }
}

/// One window's totals, with no comparison attached.
async fn totals(ch: &Ch, env: &QueryEnvelope) -> Result<Overview> {
    match env.audience {
        // The human trace: rebuild the session rollup from the filtered stream
        // and read core's own metric expressions off it.
        Audience::Human => {
            let (sub, params) = sessions_subquery(env, None, None)?;
            let sql = format!(
                "SELECT
  uniq(visitor_id) AS visitors,
  uniq(session_id) AS sessions,
  -- Not aliased to `pageviews`: ClickHouse resolves a bare name against a
  -- sibling alias first, so that would make the two expressions below read
  -- countIf(sum(pageviews) <= 1) and avg(sum(pageviews)) -- an aggregate
  -- inside an aggregate (ILLEGAL_AGGREGATION), and only at run time.
  toUInt64(sum(pageviews)) AS pageviews_total,
  {bounce} AS bounce_rate,
  {duration} AS avg_duration_s,
  {vps} AS views_per_session
FROM ({sub})",
                bounce = f64_of(Metric::BounceRate.select_expr()),
                duration = f64_of(Metric::AvgDuration.select_expr()),
                vps = f64_of(Metric::ViewsPerSession.select_expr()),
            );
            let row = fetch_optional::<HumanTotalsRow>(ch, "overview", &sql, &params)
                .await?
                .unwrap_or(HumanTotalsRow {
                    visitors: 0,
                    sessions: 0,
                    pageviews: 0,
                    bounce_rate: 0.0,
                    avg_duration_s: 0.0,
                    views_per_session: 0.0,
                });
            Ok(Overview {
                visitors: row.visitors,
                sessions: row.sessions,
                pageviews: row.pageviews,
                bounce_rate: Some(round2(row.bounce_rate)),
                avg_duration_s: Some(round2(row.avg_duration_s)),
                views_per_session: Some(round2(row.views_per_session)),
                compare: None,
                delta: None,
            })
        }

        // The agent trace. "Visitors" is distinct agents, because a crawler is
        // one program however many machines it runs on; "sessions" is distinct
        // fetching hosts, which is the closest honest analogue.
        Audience::Agent => {
            let (scope, params) = scope_predicate(env, Table::AgentEvents)?;
            let sql = format!(
                "SELECT
  uniq(agent_id) AS visitors,
  uniq(ip_hash) AS sessions,
  count() AS pageviews
FROM agent_events
WHERE {scope}"
            );
            let row = fetch_optional::<CountsRow>(ch, "overview_agent", &sql, &params)
                .await?
                .unwrap_or(CountsRow {
                    visitors: 0,
                    sessions: 0,
                    pageviews: 0,
                });
            Ok(Overview {
                visitors: row.visitors,
                sessions: row.sessions,
                pageviews: row.pageviews,
                bounce_rate: None,
                avg_duration_s: None,
                views_per_session: None,
                compare: None,
                delta: None,
            })
        }

        // Both traces at once. The quality metrics are absent rather than
        // human-only: a bounce rate printed beside a visitor count that
        // includes crawlers would be two populations in one row.
        Audience::All => {
            let (union, mut params) = crate::sql::union_projection(env);
            let mut sql = format!(
                "SELECT
  uniq(visitor_id) AS visitors,
  uniq(session_id) AS sessions,
  toUInt64(countIf(kind = 'pageview') + countIf(is_agent = 1)) AS pageviews
FROM ({union})"
            );
            if let Some((predicate, filter_params)) =
                crate::sql::compile_union_filters(env.filters.as_ref())?
            {
                sql.push_str(&format!("\nWHERE {predicate}"));
                params.extend(filter_params);
            }
            let row = fetch_optional::<CountsRow>(ch, "overview_all", &sql, &params)
                .await?
                .unwrap_or(CountsRow {
                    visitors: 0,
                    sessions: 0,
                    pageviews: 0,
                });
            Ok(Overview {
                visitors: row.visitors,
                sessions: row.sessions,
                pageviews: row.pageviews,
                bounce_rate: None,
                avg_duration_s: None,
                views_per_session: None,
                compare: None,
                delta: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Time series
// ---------------------------------------------------------------------------

/// One point on a series.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Bucket {
    /// Bucket start, epoch milliseconds, cut in the site's own zone.
    pub t: i64,
    /// The metric's value in this bucket.
    pub v: f64,
}

/// A bucketed series, with the comparison series beside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    /// Which metric this is, so a client does not have to remember what it
    /// asked for.
    pub metric: String,
    /// How to format the values.
    pub unit: MetricUnit,
    /// The buckets, ascending.
    pub buckets: Vec<Bucket>,
    /// The same metric over the comparison window, ascending.
    ///
    /// Aligned by position, not by timestamp: the two windows have different
    /// dates by definition, and a client draws them on one axis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare: Option<Vec<Bucket>>,
}

#[derive(Debug, Row, Deserialize)]
struct BucketRow {
    t: i64,
    v: f64,
}

/// `GET /api/stats/timeseries`.
///
/// **The metric chooses the trace, not the envelope's audience.** `visitors` is
/// always the human trace and `agent_fetches` is always the agent trace, so a
/// chart showing both lines asks for both metrics rather than flipping a toggle
/// between two things it wants side by side. `gap_ratio` is the one metric that
/// reads both at once.
///
/// `goal` is required only for [`Metric::Conversions`] and
/// [`Metric::ConversionRate`], which have no meaning without one. Passing it for
/// any other metric is harmless and ignored.
pub async fn timeseries(
    ch: &Ch,
    env: &QueryEnvelope,
    metric: Metric,
    goal: Option<&GoalDef>,
) -> Result<Series> {
    let buckets = series_for(ch, env, metric, goal).await?;

    let compare = match previous_period(&env.range, env.compare) {
        Some(prev) => Some(series_for(ch, &envelope_over(env, &prev), metric, goal).await?),
        None => None,
    };

    Ok(Series {
        metric: metric.as_str().to_string(),
        unit: metric.unit(),
        buckets,
        compare,
    })
}

/// The buckets for one window.
async fn series_for(
    ch: &Ch,
    env: &QueryEnvelope,
    metric: Metric,
    goal: Option<&GoalDef>,
) -> Result<Vec<Bucket>> {
    // The Citation Gap has no single table behind it. It is the whole point of
    // the product, and it is assembled from both traces.
    if metric == Metric::GapRatio {
        let trend = crate::gap::trend_buckets(ch, env).await?;
        return Ok(trend
            .into_iter()
            .map(|(t, fetches, arrivals)| Bucket {
                t,
                v: round2(crate::gap::ratio(fetches, arrivals)),
            })
            .collect());
    }

    // The session rollup's time column is `started`; every other source's is
    // `timestamp`. The rollup deliberately does not alias one to the other,
    // because an alias that shadows a real column name is a cyclic alias inside
    // a subquery.
    let time_column = if metric.table() == Table::Sessions {
        "started"
    } else {
        "timestamp"
    };
    let bucket = bucket_millis(env.granularity, env.range.tz, time_column);
    let value = f64_of(metric.select_expr());

    let (from, params) = match metric.table() {
        // Session-scoped: rebuild the rollup from the filtered stream. The
        // rollup aliases `timestamp` to the session's start, so the bucket
        // expression above reads it unchanged.
        Table::Sessions => {
            let conversion = conversion_column(goal, metric)?;
            let (sub, params) = sessions_subquery(
                env,
                None,
                conversion
                    .as_ref()
                    .map(|(sql, p)| (sql.as_str(), p.clone())),
            )?;
            (format!("({sub})"), params)
        }
        Table::AgentEvents => {
            let (scope, params) = scope_predicate(env, Table::AgentEvents)?;
            (format!("agent_events\nWHERE {scope}"), params)
        }
        // Event-scoped. A conversion metric needs a per-row `is_conversion`, so
        // the rows are projected through a subquery rather than aliased inside
        // an aggregate, where alias resolution would be doing load-bearing work.
        _ => match conversion_column(goal, metric)? {
            Some((pred, mut params)) => {
                let (scope, scope_params) = scope_predicate(env, Table::Events)?;
                params.extend(scope_params);
                (
                    format!(
                        "(SELECT timestamp, visitor_id, session_id, channel, revenue, kind, \
                         {pred} AS is_conversion FROM events WHERE {scope})"
                    ),
                    params,
                )
            }
            None => {
                let (scope, params) = scope_predicate(env, Table::Events)?;
                (format!("events\nWHERE {scope}"), params)
            }
        },
    };

    let sql =
        format!("SELECT {bucket} AS t, {value} AS v\nFROM {from}\nGROUP BY t\nORDER BY t ASC");
    let rows = fetch_all::<BucketRow>(ch, "timeseries", &sql, &params).await?;
    Ok(rows
        .into_iter()
        .map(|r| Bucket {
            t: r.t,
            v: round2(r.v),
        })
        .collect())
}

/// The `is_conversion` expression a conversion metric needs, if this is one.
fn conversion_column(
    goal: Option<&GoalDef>,
    metric: Metric,
) -> Result<Option<(String, Vec<Param>)>> {
    if !matches!(metric, Metric::Conversions | Metric::ConversionRate) {
        return Ok(None);
    }
    let goal = goal.ok_or_else(|| {
        micaforge_core::Error::validation(
            "metric",
            format!("`{}` needs a goal to be counted against", metric.as_str()),
        )
    })?;
    Ok(Some(goal.predicate()?))
}

// ---------------------------------------------------------------------------
// Realtime
// ---------------------------------------------------------------------------

/// One entry on a realtime side list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRow {
    /// The path, or the resolved source name.
    pub key: String,
    /// Distinct visitors on it in the live window.
    pub visitors: u64,
}

/// One hit on the live feed.
///
/// The feed shows both traces. A crawler fetching a page is a thing that just
/// happened on the site, and hiding it would be the exact editorial choice this
/// product exists to reverse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentHit {
    /// When, epoch milliseconds.
    pub t: i64,
    /// Event kind, or `agent_fetch`.
    pub kind: String,
    /// Path that was hit.
    pub pathname: String,
    /// Referring host, empty for agents.
    pub referrer_host: String,
    /// ISO-3166 alpha-2, empty when unknown.
    pub country: String,
    /// City, empty for agents and when unknown.
    pub city: String,
    /// Browser, empty for agents.
    pub browser: String,
    /// Operating system, empty for agents.
    pub os: String,
    /// Device class, empty for agents.
    pub device_type: String,
    /// True when this row came from the agent trace.
    pub is_agent: bool,
    /// Catalog slug, when this was an agent.
    pub agent_id: String,
}

/// The live view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Realtime {
    /// Distinct visitors in the last five minutes.
    pub visitors_now: u64,
    /// Distinct named agents in the last five minutes.
    pub agents_now: u64,
    /// `(bucket start in epoch ms, visitors, agent fetches)` per minute over
    /// the last half hour, ascending, with empty minutes omitted.
    pub per_minute: Vec<(i64, u64, u64)>,
    /// Busiest paths right now.
    pub top_pages: Vec<LiveRow>,
    /// Busiest sources right now.
    pub top_sources: Vec<LiveRow>,
    /// The most recent hits, newest first.
    pub recent: Vec<RecentHit>,
}

#[derive(Debug, Row, Deserialize)]
struct ScalarRow {
    v: u64,
}

#[derive(Debug, Row, Deserialize)]
struct MinuteRow {
    t: i64,
    visitors: u64,
    agents: u64,
}

#[derive(Debug, Row, Deserialize)]
struct LiveKeyRow {
    key: String,
    visitors: u64,
}

#[derive(Debug, Row, Deserialize)]
struct RecentRow {
    t: i64,
    kind: String,
    pathname: String,
    referrer_host: String,
    country: String,
    city: String,
    browser: String,
    os: String,
    device_type: String,
    is_agent: u8,
    agent_id: String,
}

/// `GET /api/stats/realtime`.
///
/// Deliberately not filtered by the envelope's range: "realtime" is its own
/// window, and honouring a `range=30d` here would answer a different question
/// from the one the panel asks. The site and the filters still apply.
pub async fn realtime(ch: &Ch, env: &QueryEnvelope) -> Result<Realtime> {
    let now = Utc::now();
    let live_from = fmt_ts(now - Duration::minutes(REALTIME_WINDOW_MINUTES));
    let trend_from = fmt_ts(now - Duration::minutes(REALTIME_TREND_MINUTES));

    let site = Param::U16(env.site);

    let visitors_now = fetch_optional::<ScalarRow>(
        ch,
        "realtime_visitors",
        "SELECT uniq(visitor_id) AS v FROM events \
         WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')",
        &[site.clone(), Param::Str(live_from.clone())],
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let agents_now = fetch_optional::<ScalarRow>(
        ch,
        "realtime_agents",
        "SELECT uniq(agent_id) AS v FROM agent_events \
         WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')",
        &[site.clone(), Param::Str(live_from.clone())],
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let minute = millis("toStartOfMinute(timestamp)");
    let per_minute_sql = format!(
        "SELECT t, toUInt64(sum(visitors)) AS visitors, toUInt64(sum(agents)) AS agents
FROM (
  SELECT {minute} AS t, uniq(visitor_id) AS visitors, toUInt64(0) AS agents
  FROM events WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')
  GROUP BY t
  UNION ALL
  SELECT {minute} AS t, toUInt64(0) AS visitors, count() AS agents
  FROM agent_events WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')
  GROUP BY t
)
GROUP BY t
ORDER BY t ASC"
    );
    let per_minute = fetch_all::<MinuteRow>(
        ch,
        "realtime_per_minute",
        &per_minute_sql,
        &[
            site.clone(),
            Param::Str(trend_from.clone()),
            site.clone(),
            Param::Str(trend_from),
        ],
    )
    .await?
    .into_iter()
    .map(|r| (r.t, r.visitors, r.agents))
    .collect();

    let top_pages = fetch_all::<LiveKeyRow>(
        ch,
        "realtime_pages",
        &format!(
            "SELECT pathname AS key, uniq(visitor_id) AS visitors FROM events
WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC') AND kind = 'pageview'
GROUP BY key ORDER BY visitors DESC, key ASC LIMIT {REALTIME_TOP_N}"
        ),
        &[site.clone(), Param::Str(live_from.clone())],
    )
    .await?
    .into_iter()
    .map(|r| LiveRow {
        key: r.key,
        visitors: r.visitors,
    })
    .collect();

    let top_sources = fetch_all::<LiveKeyRow>(
        ch,
        "realtime_sources",
        &format!(
            "SELECT if(source != '', toString(source), if(referrer_host != '', referrer_host, 'direct')) AS key,
       uniq(visitor_id) AS visitors
FROM events
WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')
GROUP BY key ORDER BY visitors DESC, key ASC LIMIT {REALTIME_TOP_N}"
        ),
        &[site.clone(), Param::Str(live_from.clone())],
    )
    .await?
    .into_iter()
    .map(|r| LiveRow {
        key: r.key,
        visitors: r.visitors,
    })
    .collect();

    let hit_ts = millis("timestamp");
    let recent_sql = format!(
        "SELECT t, hit_kind, path, ref_host, cc, city_name, brw, osn, dev, is_agent, agent
FROM (
  SELECT {hit_ts} AS t, kind AS hit_kind, pathname AS path, referrer_host AS ref_host,
         trimBoth(toString(country)) AS cc, city AS city_name,
         browser AS brw, os AS osn, device_type AS dev,
         toUInt8(0) AS is_agent, '' AS agent
  FROM events WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')
  UNION ALL
  SELECT {hit_ts} AS t, 'agent_fetch' AS hit_kind, pathname AS path, '' AS ref_host,
         trimBoth(toString(country)) AS cc, '' AS city_name,
         '' AS brw, '' AS osn, '' AS dev,
         toUInt8(1) AS is_agent, agent_id AS agent
  FROM agent_events WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC')
)
ORDER BY t DESC
LIMIT {REALTIME_FEED_N}"
    );
    let recent = fetch_all::<RecentRow>(
        ch,
        "realtime_feed",
        &recent_sql,
        &[
            site.clone(),
            Param::Str(live_from.clone()),
            site,
            Param::Str(live_from),
        ],
    )
    .await?
    .into_iter()
    .map(|r| RecentHit {
        t: r.t,
        kind: r.kind,
        pathname: r.pathname,
        referrer_host: r.referrer_host,
        country: r.country,
        city: r.city,
        browser: r.browser,
        os: r.os,
        device_type: r.device_type,
        is_agent: r.is_agent == 1,
        agent_id: r.agent_id,
    })
    .collect();

    Ok(Realtime {
        visitors_now,
        agents_now,
        per_minute,
        top_pages,
        top_sources,
        recent,
    })
}
