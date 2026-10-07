//! `prune_retention` — delete what the operator said to delete, and nothing else.
//!
//! Two things happen here, and the order matters.
//!
//! First the configured retention is re-applied as a TTL on every time-series
//! table, so a `MICAFORGE_RETENTION_DAYS` changed in the environment takes effect
//! without waiting for a restart.
//!
//! Then the partitions that are entirely older than the cutoff are dropped
//! outright. A TTL alone would get there eventually, by rewriting parts during
//! ordinary merges; a `DROP PARTITION` is a directory unlink. On a table
//! partitioned by month, which every one of these is, that is the difference
//! between hours of I/O and a few milliseconds — and, more to the point, between
//! data that is gone when the operator said it should be gone and data that is
//! gone at some point afterwards, which is not the same promise.
//!
//! # Forever is the default, and it is deliberate
//!
//! `MICAFORGE_RETENTION_DAYS` defaults to `0`, meaning keep everything. An
//! analytics tool that quietly deleted an operator's history because a variable
//! was unset would be indefensible, so zero does nothing here at all — not even a
//! TTL.
//!
//! # Only whole partitions
//!
//! A partition is dropped only when its *newest* row is older than the cutoff.
//! Nothing is ever partially deleted, so no report can end up reading a month
//! that is half there. Rows inside a still-live partition are removed by the TTL
//! on their next merge, which is the slow path and the correct one.
//!
//! # `page_index` is never pruned
//!
//! The content register is a list of URLs, not a stream of hits.
//! [`micaforge_store::ch::TableDef::time_column`] is `None` for it, which is how
//! that decision is recorded, and expiring it would hide exactly the pages the
//! Citation Gap exists to show: the ones agents crawl and no person ever visits.

use chrono::{Duration, Utc};
use micaforge_core::{Error, Result};
use micaforge_store::ch::TABLES;
use serde::{Deserialize, Serialize};

use crate::JobCtx;

/// The ceiling `bot_events` carries from `docs/CONTRACT.md` §1.3, whatever the
/// configured retention says.
const BOT_MAX_DAYS: u32 = 90;

/// How long a finished job row survives. Matches the worker's own janitor.
const KEEP_FINISHED_DAYS: i64 = 7;

/// Nothing to configure. Retention comes from the environment, and a job payload
/// that could override it would be a way to delete an operator's data by
/// enqueueing a row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// Apply retention and drop what has expired.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    let days = ctx.cfg.retention_days;
    let replay_days = ctx.cfg.replay_retention_days;

    // Keep the declared TTLs matching the configuration even when nothing is due
    // to be dropped: the setting may have changed since the last boot.
    ctx.ch.apply_retention(days, replay_days).await?;

    let now = Utc::now();
    let mut dropped = 0usize;

    for table in TABLES {
        // `None` means the table is not time-series and retention does not reach
        // it. That is the register, and it stays.
        if table.time_column.is_none() {
            continue;
        }

        let effective = effective_days(table.name, days, replay_days);
        if effective == 0 {
            continue;
        }

        let cutoff = now - Duration::days(i64::from(effective));
        let expired = expired_partitions(ctx, table.name, cutoff).await?;

        for partition in expired {
            match drop_partition(ctx, table.name, &partition).await {
                Ok(()) => {
                    dropped += 1;
                    tracing::info!(
                        table = table.name,
                        partition,
                        keep_days = effective,
                        "expired partition dropped"
                    );
                }
                Err(e) => {
                    // A partition that another process merged away between the
                    // read and the drop is not a failure worth retrying the whole
                    // sweep for.
                    tracing::warn!(table = table.name, partition, error = %e, "could not drop partition");
                }
            }
        }
    }

    let before = now - Duration::days(KEEP_FINISHED_DAYS);
    let purged = micaforge_store::models::jobs::purge_finished(&ctx.pg, before).await?;

    tracing::info!(
        retention_days = days,
        replay_retention_days = replay_days,
        partitions_dropped = dropped,
        job_rows_purged = purged,
        "retention sweep finished"
    );
    Ok(())
}

/// How long one table's rows are kept.
///
/// Mirrors [`micaforge_store::ch::Ch::apply_retention`] exactly, because a
/// partition dropped on a different schedule from the TTL that governs the same
/// table would make the two disagree about what "expired" means.
fn effective_days(table: &str, days: u32, replay_days: u32) -> u32 {
    match table {
        "replay_events" => replay_days,
        // Suspected-bot traffic is working material for tuning detection. It has
        // a three-month ceiling that a stricter retention shortens and a looser
        // one, including "forever", does not lengthen.
        "bot_events" if days == 0 => BOT_MAX_DAYS,
        "bot_events" => days.min(BOT_MAX_DAYS),
        _ => days,
    }
}

/// Partitions of `table` whose newest row is older than `cutoff`.
///
/// `max_time` comes from ClickHouse's own part metadata, so this is a read of a
/// system table rather than a scan of the data.
async fn expired_partitions(
    ctx: &JobCtx,
    table: &str,
    cutoff: chrono::DateTime<Utc>,
) -> Result<Vec<String>> {
    let sql = "SELECT partition_id
               FROM system.parts
               WHERE database = ? AND table = ? AND active
               GROUP BY partition_id
               HAVING max(max_time) < fromUnixTimestamp64Milli(?)
               ORDER BY partition_id";

    let rows: Vec<String> = ctx
        .ch
        .client()
        .query(sql)
        .bind(ctx.ch.db.as_str())
        .bind(table)
        .bind(crate::metrics::ms(cutoff))
        .fetch_all()
        .await
        .map_err(|e| Error::Database(format!("clickhouse read system.parts: {e}")))?;

    Ok(rows)
}

/// Drop one partition.
///
/// ClickHouse has no bind form for a partition id, so it is formatted. The value
/// came from `system.parts` and is checked against `[A-Za-z0-9_-]` before it goes
/// anywhere near a statement — not because ClickHouse is expected to hand back an
/// injection, but because a `DROP` built by string concatenation should be
/// provably safe from reading the function alone.
async fn drop_partition(ctx: &JobCtx, table: &str, partition: &str) -> Result<()> {
    if partition.is_empty()
        || !partition
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::Internal(format!(
            "refusing to drop a partition named `{partition}`"
        )));
    }

    let sql = format!(
        "ALTER TABLE {}.{table} DROP PARTITION ID '{partition}'",
        ctx.ch.db
    );
    ctx.ch.exec(&sql).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forever_is_the_default_and_deletes_nothing() {
        assert_eq!(effective_days("events", 0, 0), 0);
        assert_eq!(effective_days("replay_events", 0, 0), 0);
        // Except for suspected bots, which are working material with their own
        // ceiling from the contract.
        assert_eq!(effective_days("bot_events", 0, 0), BOT_MAX_DAYS);
    }

    #[test]
    fn bot_traffic_is_capped_but_never_extended() {
        assert_eq!(effective_days("bot_events", 30, 30), 30);
        assert_eq!(effective_days("bot_events", 365, 365), BOT_MAX_DAYS);
    }

    #[test]
    fn replay_has_its_own_shorter_clock() {
        assert_eq!(effective_days("replay_events", 365, 30), 30);
        assert_eq!(effective_days("events", 365, 30), 365);
    }

    #[test]
    fn the_register_is_never_pruned() {
        // Recorded in the schema rather than here, so the two cannot drift.
        let register = TABLES
            .iter()
            .find(|t| t.name == "page_index")
            .expect("the content register is part of the schema");
        assert!(register.time_column.is_none());
    }
}
