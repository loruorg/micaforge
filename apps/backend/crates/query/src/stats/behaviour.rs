//! What people did: custom events, goals, funnels, journeys and retention.

use clickhouse::Row;
use micaforge_core::{Error, Granularity, QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::exec::{fetch_all, fetch_optional, round2, share};
use crate::sql::{bucket_of, millis, scope_predicate, Param};
use crate::stats::{FunnelStepDef, GoalDef};

/// Most steps a funnel may have.
///
/// `windowFunnel` takes the conditions as arguments, so the bound is a real one:
/// past a dozen the chart is unreadable and the query is a wall of predicates.
const MAX_FUNNEL_STEPS: usize = 12;

/// Longest funnel window, in hours. One year.
const MAX_FUNNEL_WINDOW_HOURS: u32 = 24 * 365;

/// Deepest journey. Beyond ten hops a Sankey is a hairball.
const MAX_JOURNEY_DEPTH: u32 = 10;

/// Most links a journey returns.
const MAX_JOURNEY_LINKS: u32 = 250;

/// Most `(key, value)` pairs a property breakdown returns.
const MAX_PROPERTY_ROWS: u32 = 500;

/// Most cohorts a retention grid returns.
const MAX_RETENTION_COHORTS: u32 = 24;

// ---------------------------------------------------------------------------
// Custom events
// ---------------------------------------------------------------------------

/// One custom event name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRowAgg {
    /// The name passed to `track()`.
    pub name: String,
    /// How many times it fired.
    pub count: u64,
    /// How many distinct visitors fired it.
    pub visitors: u64,
}

#[derive(Debug, Row, Deserialize)]
struct EventRawRow {
    name: String,
    hits: u64,
    visitors: u64,
}

/// `GET /api/stats/events`.
pub async fn events(ch: &Ch, env: &QueryEnvelope) -> Result<Vec<EventRowAgg>> {
    let (scope, params) = scope_predicate(env, Table::Events)?;
    let sql = format!(
        "SELECT event_name AS name, toUInt64(count()) AS hits, uniq(visitor_id) AS visitors
FROM events
WHERE {scope} AND kind = 'custom' AND event_name != ''
GROUP BY name
ORDER BY hits DESC, name ASC
LIMIT {limit} OFFSET {offset}",
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );
    Ok(fetch_all::<EventRawRow>(ch, "events", &sql, &params)
        .await?
        .into_iter()
        .map(|r| EventRowAgg {
            name: r.name,
            count: r.hits,
            visitors: r.visitors,
        })
        .collect())
}

/// One value a property took.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropValue {
    /// The value, as text. A JSON string is unquoted; a number, boolean or
    /// nested object keeps its JSON spelling.
    pub value: String,
    /// How many events carried it.
    pub count: u64,
    /// How many distinct visitors sent it.
    pub visitors: u64,
}

/// One property key, with its values ranked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventProperty {
    /// The key inside the event's `props` object.
    pub key: String,
    /// Its values, most frequent first.
    pub values: Vec<PropValue>,
}

#[derive(Debug, Row, Deserialize)]
struct PropRawRow {
    key: String,
    value: String,
    hits: u64,
    visitors: u64,
}

/// `GET /api/stats/events` property drill-down.
///
/// The keys belong to the caller's own tracking code, so they are discovered
/// from the data rather than declared anywhere: the query walks the `props`
/// object of each matching event. `name` is bound, never interpolated.
pub async fn event_properties(
    ch: &Ch,
    env: &QueryEnvelope,
    name: &str,
) -> Result<Vec<EventProperty>> {
    let (scope, mut params) = scope_predicate(env, Table::Events)?;
    params.push(Param::Str(name.to_string()));

    let sql = format!(
        "SELECT k AS key, v AS value, toUInt64(count()) AS hits, uniq(visitor_id) AS visitors
FROM (
  SELECT visitor_id,
         arrayJoin(JSONExtractKeysAndValuesRaw(toJSONString(props))) AS kv,
         tupleElement(kv, 1) AS k,
         if(startsWith(tupleElement(kv, 2), '\"'),
            JSONExtractString(tupleElement(kv, 2)),
            tupleElement(kv, 2)) AS v
  FROM events
  WHERE {scope} AND kind = 'custom' AND event_name = ?
)
GROUP BY key, value
ORDER BY hits DESC, key ASC, value ASC
LIMIT {MAX_PROPERTY_ROWS}"
    );

    let rows = fetch_all::<PropRawRow>(ch, "event_properties", &sql, &params).await?;

    // Grouped in Rust rather than with a nested aggregate: the row count is
    // already bounded, and a map here is clearer than a `groupArray` of tuples
    // that would then have to be decoded positionally.
    let mut grouped: Vec<EventProperty> = Vec::new();
    for r in rows {
        match grouped.iter_mut().find(|p| p.key == r.key) {
            Some(entry) => entry.values.push(PropValue {
                value: r.value,
                count: r.hits,
                visitors: r.visitors,
            }),
            None => grouped.push(EventProperty {
                key: r.key,
                values: vec![PropValue {
                    value: r.value,
                    count: r.hits,
                    visitors: r.visitors,
                }],
            }),
        }
    }
    Ok(grouped)
}

// ---------------------------------------------------------------------------
// Goals
// ---------------------------------------------------------------------------

/// One goal's performance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalRow {
    /// The goal's own id, echoed back.
    pub id: String,
    /// Display name.
    pub name: String,
    /// How many events satisfied it.
    pub conversions: u64,
    /// How many distinct visitors converted at least once.
    pub visitors: u64,
    /// Converting visitors as a percentage of all visitors in the window.
    pub rate: f64,
    /// Revenue attributed to the goal, in major units.
    ///
    /// Taken from the events themselves when they carry revenue. When they do
    /// not, the goal's configured `value_cents` is multiplied by the conversion
    /// count — the operator's own valuation, applied consistently, rather than a
    /// zero that would read as "this goal earned nothing".
    pub revenue: f64,
}

#[derive(Debug, Row, Deserialize)]
struct GoalRawRow {
    conversions: u64,
    visitors: u64,
    revenue: f64,
}

#[derive(Debug, Row, Deserialize)]
struct ScalarU64Row {
    v: u64,
}

/// `GET /api/stats/goals`.
///
/// One statement per goal rather than one wide statement with a column per
/// goal: the number of goals is small, the queries are cheap and independent,
/// and a wide statement would have to be decoded positionally against a shape
/// that changes with the site's configuration.
pub async fn goals(ch: &Ch, env: &QueryEnvelope, defs: &[GoalDef]) -> Result<Vec<GoalRow>> {
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;

    let base_visitors = fetch_optional::<ScalarU64Row>(
        ch,
        "goals_base",
        &format!("SELECT uniq(visitor_id) AS v FROM events WHERE {scope}"),
        &scope_params,
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let mut out = Vec::with_capacity(defs.len());
    for def in defs {
        let (pred, pred_params) = def.predicate()?;

        // The predicate appears three times in the statement, so its parameters
        // are bound three times. Positional placeholders are unforgiving about
        // this and it is the one place in the crate where a repeat happens.
        let mut params = Vec::new();
        for _ in 0..3 {
            params.extend(pred_params.iter().cloned());
        }
        params.extend(scope_params.iter().cloned());

        let sql = format!(
            "SELECT
  toUInt64(countIf({pred})) AS conversions,
  uniqIf(visitor_id, {pred}) AS visitors,
  toFloat64(sumIf(ifNull(revenue, 0), {pred})) AS revenue
FROM events
WHERE {scope}"
        );

        let row = fetch_optional::<GoalRawRow>(ch, "goals", &sql, &params)
            .await?
            .unwrap_or(GoalRawRow {
                conversions: 0,
                visitors: 0,
                revenue: 0.0,
            });

        let revenue = if row.revenue > 0.0 {
            row.revenue
        } else {
            row.conversions as f64 * (def.value_cents as f64 / 100.0)
        };

        out.push(GoalRow {
            id: def.id.clone(),
            name: def.name.clone(),
            conversions: row.conversions,
            visitors: row.visitors,
            rate: round2(share(row.visitors, base_visitors) * 100.0),
            revenue: round2(revenue),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Funnel
// ---------------------------------------------------------------------------

/// One step of a funnel result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunnelStep {
    /// The step's label, as configured.
    pub label: String,
    /// Visitors who reached this step within the window.
    pub visitors: u64,
    /// Those visitors as a percentage of everyone who entered the funnel.
    pub rate: f64,
    /// How many were lost between the previous step and this one. Zero on the
    /// first step, which nobody can drop out of before reaching.
    pub drop_off: u64,
}

/// A funnel result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Funnel {
    /// The steps, in order.
    pub steps: Vec<FunnelStep>,
    /// Visitors who reached step one.
    pub total_entered: u64,
    /// Visitors who reached the last step.
    pub completed: u64,
}

#[derive(Debug, Row, Deserialize)]
struct FunnelLevelRow {
    level: u8,
    visitors: u64,
}

/// The funnel query text.
///
/// `windowFunnel` refuses `DateTime64` (ClickHouse code 43), so the timestamp
/// goes in as unsigned epoch milliseconds and the window in milliseconds too.
/// `toDateTime` would also be accepted, but it drops the milliseconds, and two
/// steps inside one second would then count in either order.
fn funnel_sql(window_ms: u64, conditions: &[String], scope: &str) -> String {
    format!(
        "SELECT level, toUInt64(count()) AS visitors
FROM (
  SELECT visitor_id,
         windowFunnel({window_ms})(toUInt64(toUnixTimestamp64Milli(timestamp)), {conditions}) AS level
  FROM events
  WHERE {scope}
  GROUP BY visitor_id
)
GROUP BY level
ORDER BY level ASC",
        conditions = conditions.join(", "),
    )
}

/// `GET /api/stats/funnel/:id`.
///
/// Built on ClickHouse's `windowFunnel`, which is the reason this is one pass
/// rather than one self-join per step: it walks each visitor's events in time
/// order and returns the deepest prefix of the conditions that was satisfied
/// within `window_hours`. Steps must therefore happen **in order** — a visitor
/// who did step two before step one has completed one step, not two, which is
/// what a funnel means.
pub async fn funnel(
    ch: &Ch,
    env: &QueryEnvelope,
    steps: &[FunnelStepDef],
    window_hours: u32,
) -> Result<Funnel> {
    if steps.len() < 2 {
        return Err(Error::validation(
            "steps",
            "a funnel needs at least two steps",
        ));
    }
    if steps.len() > MAX_FUNNEL_STEPS {
        return Err(Error::validation(
            "steps",
            format!("a funnel takes at most {MAX_FUNNEL_STEPS} steps"),
        ));
    }

    let window_ms = u64::from(window_hours.clamp(1, MAX_FUNNEL_WINDOW_HOURS)) * 3_600_000;

    let mut conditions = Vec::with_capacity(steps.len());
    let mut params: Vec<Param> = Vec::new();
    for step in steps {
        let (sql, step_params) = step.predicate()?;
        conditions.push(sql);
        params.extend(step_params);
    }

    let (scope, scope_params) = scope_predicate(env, Table::Events)?;
    params.extend(scope_params);

    let sql = funnel_sql(window_ms, &conditions, &scope);

    let levels = fetch_all::<FunnelLevelRow>(ch, "funnel", &sql, &params).await?;

    // `windowFunnel` reports the deepest step reached, so a visitor counted at
    // level 3 also reached levels 1 and 2. The cumulative sum is what turns
    // that into "how many got this far".
    let mut reached = vec![0u64; steps.len() + 1];
    for row in levels {
        let level = usize::from(row.level);
        if level < reached.len() {
            reached[level] = row.visitors;
        }
    }
    for i in (1..reached.len() - 1).rev() {
        reached[i] += reached[i + 1];
    }

    let total_entered = reached.get(1).copied().unwrap_or(0);
    let mut out = Vec::with_capacity(steps.len());
    let mut previous = total_entered;
    for (i, step) in steps.iter().enumerate() {
        let visitors = reached.get(i + 1).copied().unwrap_or(0);
        out.push(FunnelStep {
            label: step.label.clone(),
            visitors,
            rate: round2(share(visitors, total_entered) * 100.0),
            drop_off: if i == 0 {
                0
            } else {
                previous.saturating_sub(visitors)
            },
        });
        previous = visitors;
    }

    Ok(Funnel {
        completed: out.last().map(|s| s.visitors).unwrap_or(0),
        total_entered,
        steps: out,
    })
}

// ---------------------------------------------------------------------------
// Journeys
// ---------------------------------------------------------------------------

/// One node of the Sankey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// `<depth>:<path>`, unique across the graph. The same path at two depths is
    /// two nodes, which is what makes a Sankey readable.
    pub id: String,
    /// The path.
    pub label: String,
    /// How many hops from the start, 1-based.
    pub depth: u32,
    /// Sessions passing through.
    pub visitors: u64,
}

/// One link of the Sankey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    /// Source [`Node::id`].
    pub source: String,
    /// Target [`Node::id`].
    pub target: String,
    /// Sessions taking this hop.
    pub visitors: u64,
}

/// A user-journey graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Journey {
    /// Every node reached.
    pub nodes: Vec<Node>,
    /// Every hop taken.
    pub links: Vec<Link>,
}

#[derive(Debug, Row, Deserialize)]
struct HopRow {
    depth: u32,
    src: String,
    dst: String,
    visitors: u64,
}

/// `GET /api/stats/journeys`.
///
/// Each session's pageviews are sorted by time into a path, and each adjacent
/// pair becomes one hop. The sort happens inside the aggregate rather than in an
/// `ORDER BY` in a subquery, because a subquery's ordering is not guaranteed to
/// survive into `groupArray` when the scan is parallel — and a journey built
/// from mis-ordered pages is worse than no journey at all.
pub async fn journeys(
    ch: &Ch,
    env: &QueryEnvelope,
    depth: u32,
    start_path: Option<&str>,
) -> Result<Journey> {
    let depth = depth.clamp(2, MAX_JOURNEY_DEPTH);
    let (scope, mut params) = scope_predicate(env, Table::Events)?;

    let start_clause = match start_path {
        Some(path) if !path.is_empty() => {
            params.push(Param::Str(path.to_string()));
            " AND paths[1] = ?"
        }
        _ => "",
    };

    let sql = format!(
        "SELECT tupleElement(h, 1) AS depth, tupleElement(h, 2) AS src,
       tupleElement(h, 3) AS dst, uniq(session_id) AS visitors
FROM (
  SELECT session_id,
         arrayJoin(arrayMap(i -> (toUInt32(i), paths[i], paths[i + 1]),
                            range(1, least(length(paths), toUInt64({depth}))))) AS h
  FROM (
    SELECT session_id,
           arrayMap(x -> tupleElement(x, 2),
                    arraySort(x -> tupleElement(x, 1),
                              groupArray((timestamp, pathname)))) AS paths
    FROM events
    WHERE {scope} AND kind = 'pageview'
    GROUP BY session_id
  )
  WHERE length(paths) > 1{start_clause}
)
GROUP BY depth, src, dst
ORDER BY visitors DESC, depth ASC
LIMIT {MAX_JOURNEY_LINKS}"
    );

    let hops = fetch_all::<HopRow>(ch, "journeys", &sql, &params).await?;

    // A node's value is the larger of what flows in and what flows out. Summing
    // both would count the same session twice for every node in the middle of a
    // path, which is the classic way a Sankey ends up wider than its input.
    let mut outgoing: BTreeMap<(u32, String), u64> = BTreeMap::new();
    let mut incoming: BTreeMap<(u32, String), u64> = BTreeMap::new();
    let mut links = Vec::with_capacity(hops.len());

    for hop in &hops {
        *outgoing.entry((hop.depth, hop.src.clone())).or_insert(0) += hop.visitors;
        *incoming
            .entry((hop.depth + 1, hop.dst.clone()))
            .or_insert(0) += hop.visitors;
        links.push(Link {
            source: format!("{}:{}", hop.depth, hop.src),
            target: format!("{}:{}", hop.depth + 1, hop.dst),
            visitors: hop.visitors,
        });
    }

    let mut nodes: BTreeMap<(u32, String), u64> = BTreeMap::new();
    for (key, value) in outgoing {
        let slot = nodes.entry(key).or_insert(0);
        *slot = (*slot).max(value);
    }
    for (key, value) in incoming {
        let slot = nodes.entry(key).or_insert(0);
        *slot = (*slot).max(value);
    }

    Ok(Journey {
        nodes: nodes
            .into_iter()
            .map(|((depth, label), visitors)| Node {
                id: format!("{depth}:{label}"),
                label,
                depth,
                visitors,
            })
            .collect(),
        links,
    })
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// One cohort row of the retention grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cohort {
    /// When the cohort starts, epoch milliseconds, cut in the site's own zone.
    pub start: i64,
    /// How many visitors were first seen in this period.
    pub size: u64,
    /// Visitors still returning, by period offset. `cells[0]` is the cohort
    /// itself and equals `size`.
    pub cells: Vec<u64>,
}

/// A retention grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Retention {
    /// The period each column represents.
    pub period: Granularity,
    /// Cohorts, oldest first.
    pub cohorts: Vec<Cohort>,
}

#[derive(Debug, Row, Deserialize)]
struct RetentionRawRow {
    cohort: i64,
    /// Periods elapsed since the cohort's own period. Spelled `period_offset`
    /// in SQL, because `OFFSET` is a keyword there.
    period_offset: i64,
    visitors: u64,
}

/// The `dateDiff` unit for a period.
fn diff_unit(g: Granularity) -> &'static str {
    match g {
        Granularity::Minute => "minute",
        Granularity::Hour => "hour",
        Granularity::Day => "day",
        Granularity::Week => "week",
        Granularity::Month => "month",
    }
}

/// `GET /api/stats/retention`.
///
/// A visitor's cohort is the period their **first** hit in the window falls
/// into, which is why this is a join and not a single scan: the first-seen
/// instant has to be known before any row can be placed in a column.
///
/// The window matters. "First seen" means first seen *inside the selected
/// range*, not first seen ever, because the event store may not go back far
/// enough to know the difference and pretending otherwise would put long-time
/// readers into this week's cohort.
pub async fn retention(
    ch: &Ch,
    env: &QueryEnvelope,
    period: Granularity,
    cohorts: u32,
) -> Result<Retention> {
    let cohorts = cohorts.clamp(1, MAX_RETENTION_COHORTS) as usize;

    let (scope, scope_params) = scope_predicate(env, Table::Events)?;
    let cohort_bucket = bucket_of(period, env.range.tz, "f.first_ts");
    let row_bucket = bucket_of(period, env.range.tz, "e.timestamp");
    let cohort_ms = millis(&cohort_bucket);
    let unit = diff_unit(period);

    // The join subquery comes first in the text, so its parameters are bound
    // first.
    let mut params = scope_params.clone();
    params.extend(scope_params);

    // `f` deliberately exposes `vid` rather than `visitor_id`: the outer scope
    // predicate is written with unqualified column names, and a join side that
    // repeated one of them would make it ambiguous the moment somebody filtered
    // on it.
    let sql = format!(
        "SELECT {cohort_ms} AS cohort,
       toInt64(dateDiff('{unit}', {cohort_bucket}, {row_bucket})) AS period_offset,
       uniq(e.visitor_id) AS visitors
FROM events AS e
INNER JOIN (
  SELECT visitor_id AS vid, min(timestamp) AS first_ts
  FROM events
  WHERE {scope}
  GROUP BY visitor_id
) AS f ON f.vid = e.visitor_id
WHERE {scope}
GROUP BY cohort, period_offset
HAVING period_offset >= 0
ORDER BY cohort ASC, period_offset ASC"
    );

    let rows = fetch_all::<RetentionRawRow>(ch, "retention", &sql, &params).await?;

    let mut grid: BTreeMap<i64, Vec<u64>> = BTreeMap::new();
    for row in rows {
        let cells = grid.entry(row.cohort).or_insert_with(|| vec![0; cohorts]);
        if let Ok(index) = usize::try_from(row.period_offset) {
            if index < cohorts {
                cells[index] = row.visitors;
            }
        }
    }

    // Most recent cohorts, returned oldest first: a grid is read top-down and
    // the interesting rows are the recent ones.
    let mut all: Vec<Cohort> = grid
        .into_iter()
        .map(|(start, cells)| Cohort {
            start,
            size: cells.first().copied().unwrap_or(0),
            cells,
        })
        .collect();
    if all.len() > cohorts {
        all.drain(..all.len() - cohorts);
    }

    Ok(Retention {
        period,
        cohorts: all,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_funnel_never_hands_window_funnel_a_datetime64() {
        let sql = funnel_sql(3_600_000, &["kind = 'pageview'".into(), "1".into()], "1");
        assert!(sql.contains(
            "windowFunnel(3600000)(toUInt64(toUnixTimestamp64Milli(timestamp)), kind = 'pageview', 1)"
        ));
        assert!(
            !sql.contains("(timestamp,"),
            "the raw DateTime64 column is refused"
        );
    }
}
