//! `rollup_sessions` — hourly maintenance on the session aggregate.
//!
//! `sessions_mv` is an `AggregatingMergeTree` fed by a materialised view from
//! `events`. Every insert block that touches a session writes a *partial* row,
//! and the table only becomes one row per session when those parts merge.
//! ClickHouse merges on its own schedule; this job asks it to merge the parts
//! that matter, on a schedule the product can rely on.
//!
//! # Why it is worth a job at all
//!
//! Every read of the rollup uses `FINAL`, which merges on the fly and is correct
//! whether or not this job ever runs. What it is not, on a table with thousands
//! of unmerged parts, is fast. So this is a *performance* job, not a correctness
//! one, and that is the reason it is safe: skipping it, failing it, or running it
//! twice changes no number anybody reads.
//!
//! # Only the partitions that are moving
//!
//! `OPTIMIZE TABLE ... FINAL` with no partition rewrites the whole table, which
//! on a year of data is hours of I/O to merge parts that were merged in January
//! and have not been touched since. Only the current month receives new
//! sessions all month; the previous one receives the tail of sessions that
//! straddled midnight on the last day, so it is asked for only in the first
//! [`PREVIOUS_MONTH_DAYS`] days of a month.
//!
//! Every statement carries `optimize_skip_merged_partitions = 1`. Without it,
//! `FINAL` rewrites a partition that is already one merged part, which on a
//! quiet install is the whole month being copied every hour for nothing, on a
//! two-core machine that is also serving ingest.
//!
//! # `page_index` comes along, once a day
//!
//! The content register is a `ReplacingMergeTree` that three jobs write to and
//! several read with `FINAL`. It is unpartitioned, so `FINAL` rewrites all of
//! it, and the crawls that feed it run daily. Merging it more often than they
//! write to it is wasted I/O, so a Valkey marker limits it to once every
//! [`REGISTER_EVERY_SECS`].

use chrono::{Datelike, Months, Utc};
use micaforge_core::Result;
use serde::{Deserialize, Serialize};

use crate::JobCtx;

/// The settings clause every `OPTIMIZE` here carries.
const SKIP_MERGED: &str = "SETTINGS optimize_skip_merged_partitions = 1";

/// For how many days into a month the previous month's partition is merged.
const PREVIOUS_MONTH_DAYS: u32 = 2;

/// Valkey key marking that the register was merged recently.
const REGISTER_KEY: &str = "mf:rollup:page_index_merged";

/// The least time between two merges of the register.
///
/// Twenty-three hours rather than twenty-four, so an hourly job that drifts by
/// a few seconds a run still merges once a day instead of skipping a day.
const REGISTER_EVERY_SECS: u32 = 23 * 60 * 60;

/// Nothing to configure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// Merge the active partitions of the session rollup, and the register.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    let now = Utc::now();
    let mut merged = 0usize;

    for partition in &partitions_to_merge(now) {
        let sql = format!(
            "OPTIMIZE TABLE {}.sessions_mv PARTITION ID '{partition}' FINAL {SKIP_MERGED}",
            ctx.ch.db
        );
        match ctx.ch.exec(&sql).await {
            Ok(()) => {
                merged += 1;
                tracing::debug!(partition, "session rollup merged");
            }
            // A partition that does not exist yet is the normal state of a fresh
            // install and of the first day of a month, and ClickHouse reports it
            // as an error. It is not one.
            Err(e) => tracing::debug!(partition, error = %e, "nothing to merge"),
        }
    }

    // An unreadable Valkey skips the register this hour: this is a performance
    // job, and merging when unsure would be the expensive way to be wrong.
    let register_due = matches!(ctx.kv.get_json::<i64>(REGISTER_KEY).await, Ok(None));
    if register_due {
        let register = format!(
            "OPTIMIZE TABLE {}.page_index FINAL {SKIP_MERGED}",
            ctx.ch.db
        );
        match ctx.ch.exec(&register).await {
            Ok(()) => {
                let _ = ctx
                    .kv
                    .set_json(REGISTER_KEY, &now.timestamp(), Some(REGISTER_EVERY_SECS))
                    .await;
            }
            Err(e) => tracing::debug!(error = %e, "content register did not need merging"),
        }
    }

    tracing::info!(
        partitions = merged,
        register = register_due,
        "session rollup maintenance finished"
    );
    Ok(())
}

/// The `sessions_mv` partitions worth merging at `now`.
///
/// The current month always. The previous one only in the first
/// [`PREVIOUS_MONTH_DAYS`] days: a session that started at 23:58 on the last of
/// the month is still receiving events in the new one, and after that the
/// partition is closed and `optimize_skip_merged_partitions` would skip it
/// anyway, once it has been merged.
fn partitions_to_merge(now: chrono::DateTime<Utc>) -> Vec<String> {
    let mut partitions = vec![partition_id(now)];
    if now.day() <= PREVIOUS_MONTH_DAYS {
        if let Some(previous) = now.checked_sub_months(Months::new(1)) {
            partitions.push(partition_id(previous));
        }
    }
    partitions
}

/// The `PARTITION ID` for a month, matching `PARTITION BY toYYYYMM(started)`.
///
/// Built from the date rather than taken from a caller, and always six digits, so
/// the only thing this crate ever formats into a ClickHouse statement is a string
/// it generated itself from a number.
fn partition_id(at: chrono::DateTime<Utc>) -> String {
    format!("{:04}{:02}", at.year(), at.month())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_partition_id_is_always_six_digits() {
        let at = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        assert_eq!(partition_id(at), "202608");
        let january = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(partition_id(january), "202601");
        assert_eq!(partition_id(january).len(), 6);
        // Nothing a caller supplies reaches the statement, so the only shape that
        // has to hold is this one.
        assert!(partition_id(at).chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn the_previous_month_is_merged_only_while_it_can_still_change() {
        let first = Utc.with_ymd_and_hms(2026, 9, 1, 3, 0, 0).unwrap();
        assert_eq!(partitions_to_merge(first), ["202609", "202608"]);
        let second = Utc.with_ymd_and_hms(2026, 9, 2, 23, 0, 0).unwrap();
        assert_eq!(partitions_to_merge(second).len(), 2);
        let mid = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap();
        assert_eq!(partitions_to_merge(mid), ["202609"]);
    }

    #[test]
    fn every_merge_skips_partitions_that_are_already_merged() {
        assert!(SKIP_MERGED.contains("optimize_skip_merged_partitions = 1"));
        // Once a day for a register that is crawled daily, not once an hour.
        const { assert!(REGISTER_EVERY_SECS >= 12 * 60 * 60) };
    }

    #[test]
    fn the_month_before_january_is_the_previous_december() {
        let january = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let previous = january.checked_sub_months(Months::new(1)).unwrap();
        assert_eq!(partition_id(previous), "202512");
    }
}
