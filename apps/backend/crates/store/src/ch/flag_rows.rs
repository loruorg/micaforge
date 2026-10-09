//! The row of the `flag_events` table: feature-flag evaluations and exposures,
//! counted by the SDKs and sent in batches.
//!
//! One row is a *count*, not one evaluation. An SDK aggregates what it decided
//! over a few seconds per (flag, variant, outcome, kind) and sends the totals,
//! so a site with ten flags and a million pageviews writes thousands of rows,
//! not ten million. The flag detail screen sums `count` per variant per bucket.
//!
//! Kept in its own file rather than at the end of `rows.rs` so that the four
//! agents adding tables in parallel do not edit the same lines.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A row in `flag_events`.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct FlagEventRow {
    /// Numeric site id.
    pub site_id: u16,
    /// When the server received the batch. A client clock is never trusted for
    /// an unauthenticated write.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// The flag key.
    pub flag_key: String,
    /// The variant label: a variant key, `on` for an on/off flag that was on,
    /// `off` for any decision that was off.
    pub variant: String,
    /// 1 when the decision was on.
    pub enabled: u8,
    /// `evaluation` | `exposure`.
    pub kind: String,
    /// `browser` | `server`.
    pub source: String,
    /// The environment the SDK named, lower-case, or empty.
    pub environment: String,
    /// How many evaluations or exposures this row stands for.
    pub count: u32,
}
