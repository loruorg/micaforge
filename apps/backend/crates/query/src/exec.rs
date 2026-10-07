//! Running a built statement, and the small arithmetic every report shares.
//!
//! Reports build SQL with [`crate::sql`] and run it here, so there is exactly
//! one place that turns a driver failure into a [`micaforge_core::Error`] and
//! exactly one place that decides what gets logged. ClickHouse error text
//! carries the failing query, and a bound filter value is inside that query, so
//! it is logged and never returned: `Error::Database` is the variant the HTTP
//! layer replaces with a fixed sentence.

use chrono::{DateTime, Utc};
use clickhouse::Row;
use micaforge_core::{Error, Result};
use micaforge_store::Ch;
use serde::Deserialize;

use crate::sql::{bind_all, placeholder_count, Param};

/// Run a statement and collect every row.
///
/// `report` names the caller for the log line. It is a fixed string chosen in
/// this crate, never caller text.
pub async fn fetch_all<T>(ch: &Ch, report: &str, sql: &str, params: &[Param]) -> Result<Vec<T>>
where
    T: Row + for<'a> Deserialize<'a>,
{
    tracing::debug!(report, sql, params = params.len(), "clickhouse read");

    // Positional binding fails badly rather than loudly: one parameter too few
    // and every value after it lands in the wrong slot, which is a wrong answer
    // rather than an error. Checking here turns that class of bug into a named
    // internal failure with the report in the message, at the cost of one scan
    // of a few kilobytes.
    let slots = placeholder_count(sql);
    if slots != params.len() {
        tracing::error!(
            report,
            slots,
            params = params.len(),
            sql,
            "bind count mismatch"
        );
        return Err(Error::Internal(format!(
            "{report}: statement has {slots} bind slots but {} parameters",
            params.len()
        )));
    }

    let query = bind_all(ch.client().query(sql), params);
    match query.fetch_all::<T>().await {
        Ok(rows) => {
            tracing::trace!(report, rows = rows.len(), "clickhouse read complete");
            Ok(rows)
        }
        Err(e) => {
            tracing::error!(report, error = %e, "clickhouse read failed");
            Err(Error::Database(format!("{report}: {e}")))
        }
    }
}

/// Run a statement expected to produce at most one row.
///
/// An aggregate with no `GROUP BY` always produces one row in ClickHouse, so a
/// `None` here means the statement had a grouping and matched nothing — an empty
/// result, not a failure.
pub async fn fetch_optional<T>(
    ch: &Ch,
    report: &str,
    sql: &str,
    params: &[Param],
) -> Result<Option<T>>
where
    T: Row + for<'a> Deserialize<'a>,
{
    Ok(fetch_all::<T>(ch, report, sql, params)
        .await?
        .into_iter()
        .next())
}

// ---------------------------------------------------------------------------
// Shared arithmetic
// ---------------------------------------------------------------------------

/// One row's share of a total, as a fraction between 0 and 1.
///
/// A zero total gives zero, not a division by zero and not a null: "nothing
/// happened, so nothing has a share of it" is the honest reading, and every
/// caller here has already established that the total came from the same query.
pub fn share(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

/// Period-over-period change as a percentage.
///
/// `None` when the earlier period was zero. A jump from nothing to something is
/// not "infinite growth" and is not "+100%"; it has no percentage, and saying so
/// is better than inventing one. Clients render the absent case as "new".
pub fn pct_delta(current: f64, previous: f64) -> Option<f64> {
    if previous == 0.0 {
        return None;
    }
    Some(((current - previous) / previous * 100.0 * 100.0).round() / 100.0)
}

/// The same, for counts.
pub fn pct_delta_u64(current: u64, previous: u64) -> Option<f64> {
    pct_delta(current as f64, previous as f64)
}

/// Turn epoch milliseconds from ClickHouse into a record timestamp.
///
/// Out-of-range milliseconds cannot come from a `DateTime64(3)` column, so the
/// fallback is the Unix epoch rather than an error: a report must not fail
/// because one row carried a nonsense date.
pub fn ms_to_dt(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or_else(|| DateTime::from_timestamp_nanos(0))
}

/// The same, but an instant of zero means "never happened".
pub fn ms_to_opt_dt(ms: i64) -> Option<DateTime<Utc>> {
    if ms == 0 {
        None
    } else {
        Some(ms_to_dt(ms))
    }
}

/// Round to two decimals, which is the precision every rate and ratio on the
/// wire carries. Doing it once here keeps `12.340000000000001` out of JSON.
///
/// Non-finite input becomes zero. ClickHouse's `avg` over an empty set is a
/// `NaN`, not a null, so `ifNull` in the SQL cannot catch it; JSON has no way to
/// spell `NaN` and `serde_json` would quietly write `null`. Zero is the honest
/// answer for "no rows contributed to this average", and it is chosen here
/// rather than left to a serialiser's fallback.
pub fn round2(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    (v * 100.0).round() / 100.0
}
