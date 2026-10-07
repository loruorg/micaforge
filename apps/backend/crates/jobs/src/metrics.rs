//! The one thing the alert sweep measures that no read endpoint does.
//!
//! Everything else this crate needs to know about an event stream comes from
//! `micaforge-query`: the digest reads [`micaforge_query::stats::overview`], the
//! Citation Gap alert reads [`micaforge_query::gap::by_page`], the decay job runs
//! [`micaforge_query::gap::decay`]. That is not politeness, it is the rule the
//! whole backend is arranged around — a metric has one definition, and a second
//! implementation of "bounce rate" living in the worker would eventually
//! disagree with the one behind the endpoint. Two numbers for the same question
//! is the one failure an analytics tool cannot recover from.
//!
//! What is left here is the piece the read API has no shape for: a **baseline**.
//! No endpoint answers "how many visitors does this site usually have at nine on
//! a Tuesday morning", because nothing on a dashboard asks it. An alert asks
//! nothing else.
//!
//! Two rules hold in the queries below, the same two the store crate holds
//! itself to.
//!
//! **Nothing is interpolated except the database name**, which
//! [`micaforge_store::ch::Ch`] validated as a bare identifier before it was
//! stored, and this module's own table and column literals. Every value — the
//! site, the window, the timezone, an agent slug — is a driver bind.
//!
//! **Timestamps cross as milliseconds**, wrapped in `fromUnixTimestamp64Milli`.
//! A formatted datetime literal would be parsed by ClickHouse in the *server's*
//! timezone, and a window that silently shifts by an hour on a server set to
//! `Europe/Berlin` is the kind of bug that makes every number subtly wrong and is
//! nearly impossible to see.

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use micaforge_core::{Error, Result};
use micaforge_store::ch::Ch;

/// A ClickHouse timestamp bind.
///
/// Milliseconds since the epoch, which is exactly what `DateTime64(3)` holds, so
/// the comparison is on the stored value and no parsing or zone conversion sits
/// between the bind and the column.
pub fn ms(at: DateTime<Utc>) -> i64 {
    at.timestamp_millis()
}

/// Flatten a driver error, keeping the failing query out of anything public.
fn ch_err(what: &str, e: clickhouse::error::Error) -> Error {
    Error::Database(format!("clickhouse {what}: {e}"))
}

/// The quantity an alert watches.
///
/// A small closed set rather than the full [`micaforge_core::Metric`] vocabulary,
/// because these are the four things an alert can be about and each one reads a
/// different table. Widening it is a deliberate act, not a matter of passing a
/// different string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pulse {
    /// Distinct human visitors. What `spike` and `drop` watch.
    Visitors,
    /// Pageviews, for a site that cares about volume rather than reach.
    Pageviews,
    /// Client-side exceptions. What `error` watches.
    Errors,
    /// Fetches by one named agent. What `agent_surge` watches.
    AgentFetches(String),
}

impl Pulse {
    /// The metric's own name, for an alert message.
    pub fn label(&self) -> String {
        match self {
            Pulse::Visitors => "visitors".to_string(),
            Pulse::Pageviews => "pageviews".to_string(),
            Pulse::Errors => "errors".to_string(),
            Pulse::AgentFetches(agent) => format!("{agent} fetches"),
        }
    }

    /// The aggregate, the table, and the extra predicate an agent-scoped pulse
    /// needs.
    ///
    /// The aggregates are [`micaforge_core::Metric`]'s own expressions wherever
    /// one exists, so a visitor counted by an alert is the same visitor the
    /// dashboard counts. An alert that fired on a number the dashboard does not
    /// show would be unarguable and useless.
    fn select(&self, db: &str) -> (&'static str, String, &'static str) {
        use micaforge_core::Metric;
        match self {
            Pulse::Visitors => (Metric::Visitors.select_expr(), format!("{db}.events"), ""),
            Pulse::Pageviews => (Metric::Pageviews.select_expr(), format!("{db}.events"), ""),
            // `errors` has no metric in core's vocabulary, because it is not a
            // column of the event stream: it is its own table, and the only
            // question anybody asks of it is how many.
            Pulse::Errors => ("count()", format!("{db}.errors"), ""),
            Pulse::AgentFetches(_) => (
                Metric::AgentFetches.select_expr(),
                format!("{db}.agent_events"),
                " AND agent_id = ?",
            ),
        }
    }

    /// The bind this pulse adds, if any.
    fn extra_bind(&self) -> Option<&str> {
        match self {
            Pulse::AgentFetches(agent) => Some(agent.as_str()),
            _ => None,
        }
    }
}

/// What a pulse measured in one window.
pub async fn pulse_in(
    ch: &Ch,
    site: u16,
    pulse: &Pulse,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<u64> {
    let (select, table, extra) = pulse.select(&ch.db);
    let sql = format!(
        "SELECT toUInt64({select})
         FROM {table}
         WHERE site_id = ?
           AND timestamp >= fromUnixTimestamp64Milli(?)
           AND timestamp < fromUnixTimestamp64Milli(?){extra}"
    );

    let mut query = ch
        .client()
        .query(&sql)
        .bind(site)
        .bind(ms(start))
        .bind(ms(end));
    if let Some(agent) = pulse.extra_bind() {
        query = query.bind(agent);
    }

    query.fetch_one().await.map_err(|e| ch_err("pulse", e))
}

/// The mean and spread of a quantity, over a set of comparable periods.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Baseline {
    /// The average.
    pub mean: f64,
    /// Population standard deviation. Population rather than sample because
    /// these *are* the whole set of comparable hours, not a draw from a larger
    /// one.
    pub stddev: f64,
    /// How many periods went into it.
    pub samples: usize,
}

impl Baseline {
    /// Build a baseline from observations.
    pub fn of(values: &[f64]) -> Baseline {
        if values.is_empty() {
            return Baseline {
                mean: 0.0,
                stddev: 0.0,
                samples: 0,
            };
        }

        let n = values.len() as f64;
        let mean = values.iter().sum::<f64>() / n;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n;

        Baseline {
            mean,
            stddev: variance.sqrt(),
            samples: values.len(),
        }
    }

    /// The upper edge of normal: `mean + k * stddev`.
    pub fn upper(&self, k: f64) -> f64 {
        self.mean + k * self.stddev
    }

    /// The lower edge of normal, never below zero.
    pub fn lower(&self, k: f64) -> f64 {
        (self.mean - k * self.stddev).max(0.0)
    }

    /// True when there is enough history for the answer to mean anything.
    ///
    /// Two comparable periods is the floor. A "spike" measured against one
    /// previous Tuesday is a coin toss dressed as a statistic, and firing it at
    /// a customer on their second day would be worse than not firing at all.
    pub fn is_usable(&self) -> bool {
        self.samples >= 2
    }
}

/// The same weekday and hour, over the preceding `weeks`.
///
/// Traffic is periodic on a weekly cycle far more than on a daily one: Tuesday
/// at 09:00 looks like last Tuesday at 09:00 and looks nothing like Tuesday at
/// 03:00 or like Sunday at 09:00. Comparing an hour with a rolling average of
/// *all* recent hours is what makes naive alerting fire every weekday morning
/// and stay silent through every weekend collapse.
///
/// Hours with no traffic produce no row, so the result is padded with zeros up to
/// `weeks`. Without that padding a site that is quiet three Tuesdays in four
/// would have a baseline computed only from the busy one.
pub async fn weekly_baseline(
    ch: &Ch,
    site: u16,
    pulse: &Pulse,
    tz: Tz,
    at: DateTime<Utc>,
    weeks: u32,
) -> Result<Baseline> {
    let weeks = weeks.clamp(1, 26);
    let local = at.with_timezone(&tz);
    // ClickHouse `toDayOfWeek` defaults to 1 = Monday … 7 = Sunday, which is what
    // `number_from_monday` returns.
    let weekday = local.weekday().number_from_monday();
    let hour = local.hour();

    // The window ends at the start of the hour being judged, so the observation
    // is never part of its own baseline. Truncated in the *site's* zone and then
    // converted back: several real zones are offset by thirty or forty-five
    // minutes, and truncating UTC would put their hour boundary in the wrong
    // place.
    let end = local
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(at);
    let start = end - chrono::Duration::weeks(i64::from(weeks));

    let (select, table, extra) = pulse.select(&ch.db);
    let sql = format!(
        "SELECT toUInt64({select}) AS v
         FROM {table}
         WHERE site_id = ?
           AND timestamp >= fromUnixTimestamp64Milli(?)
           AND timestamp < fromUnixTimestamp64Milli(?)
           AND toDayOfWeek(toTimeZone(timestamp, ?)) = ?
           AND toHour(toTimeZone(timestamp, ?)) = ?{extra}
         GROUP BY toStartOfHour(timestamp)"
    );

    let mut query = ch
        .client()
        .query(&sql)
        .bind(site)
        .bind(ms(start))
        .bind(ms(end))
        .bind(tz.name())
        .bind(weekday)
        .bind(tz.name())
        .bind(hour);
    if let Some(agent) = pulse.extra_bind() {
        query = query.bind(agent);
    }

    let observed: Vec<u64> = query
        .fetch_all()
        .await
        .map_err(|e| ch_err("weekly baseline", e))?;

    // Pad, never truncate: a daylight-saving fall-back genuinely produces two
    // matching hours in one week, and dropping the second would be inventing a
    // number rather than reading one.
    let mut values: Vec<f64> = observed.iter().map(|v| *v as f64).collect();
    if values.len() < weeks as usize {
        values.resize(weeks as usize, 0.0);
    }

    Ok(Baseline::of(&values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_baseline_of_a_flat_week_has_no_spread() {
        let flat = Baseline::of(&[100.0, 100.0, 100.0, 100.0]);
        assert_eq!(flat.mean, 100.0);
        assert_eq!(flat.stddev, 0.0);
        assert_eq!(flat.upper(3.0), 100.0);
        assert!(flat.is_usable());
    }

    #[test]
    fn a_baseline_spreads_with_its_data() {
        let noisy = Baseline::of(&[80.0, 100.0, 120.0, 100.0]);
        assert_eq!(noisy.mean, 100.0);
        assert!((noisy.stddev - 14.142).abs() < 0.01);
        assert!(noisy.upper(2.0) > 128.0);

        // The lower edge is clamped: a negative visitor count is not a threshold.
        let small = Baseline::of(&[1.0, 5.0]);
        assert_eq!(small.mean, 3.0);
        assert!(
            small.mean - 10.0 * small.stddev < 0.0,
            "the raw edge is negative"
        );
        assert_eq!(small.lower(10.0), 0.0);
    }

    #[test]
    fn one_sample_is_not_a_baseline() {
        assert!(!Baseline::of(&[500.0]).is_usable());
        assert!(!Baseline::of(&[]).is_usable());
        assert_eq!(Baseline::of(&[]).mean, 0.0);
    }

    #[test]
    fn a_pulse_names_itself_the_way_an_alert_would_say_it() {
        assert_eq!(Pulse::Visitors.label(), "visitors");
        assert_eq!(
            Pulse::AgentFetches("gptbot".into()).label(),
            "gptbot fetches"
        );
        // Only the agent-scoped pulse adds a predicate, and it adds a bind for it.
        assert!(Pulse::Visitors.select("db").2.is_empty());
        assert!(!Pulse::AgentFetches("x".into()).select("db").2.is_empty());
        assert_eq!(Pulse::AgentFetches("x".into()).extra_bind(), Some("x"));
        assert_eq!(Pulse::Errors.extra_bind(), None);
    }

    #[test]
    fn a_pulse_counts_what_the_dashboard_counts() {
        use micaforge_core::Metric;
        // Asserted rather than assumed: an alert that fired on a number the
        // dashboard does not show would be unarguable, and arguing with it is the
        // first thing an operator does.
        assert_eq!(
            Pulse::Visitors.select("db").0,
            Metric::Visitors.select_expr()
        );
        assert_eq!(
            Pulse::Pageviews.select("db").0,
            Metric::Pageviews.select_expr()
        );
        assert_eq!(
            Pulse::AgentFetches("x".into()).select("db").0,
            Metric::AgentFetches.select_expr()
        );
    }

    #[test]
    fn each_pulse_reads_its_own_table() {
        assert!(Pulse::Visitors.select("mf").1.ends_with(".events"));
        assert!(Pulse::Errors.select("mf").1.ends_with(".errors"));
        assert!(Pulse::AgentFetches("x".into())
            .select("mf")
            .1
            .ends_with(".agent_events"));
    }
}
