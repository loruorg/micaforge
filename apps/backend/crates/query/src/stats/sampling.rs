//! Is this window a sample?
//!
//! A site can tell its tracker to keep only some visitors (`data-sample`). The
//! tracker sends the rate with every hit and the ingest path stores it in
//! `events.sample_rate` (100 for everyone, and for every row from before
//! sampling existed). No report scales its figures up: a sampled count is
//! shown as counted. What a report must do instead is say so, and this is the
//! reading it says it from.
//!
//! The rate is per row rather than per site because a site can change it, and
//! a window that spans the change holds rows at both rates; the answer then
//! gives both ends and the share of pageviews that were sampled.

use clickhouse::Row;
use micaforge_core::{QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, round2};
use crate::sql::scope_predicate;

/// How the pageviews of a window were sampled.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Sampling {
    /// `true` when any pageview in the window came from a sample.
    pub sampled: bool,
    /// The lowest rate seen, percent. 100 when nothing was sampled.
    pub min_rate: u8,
    /// The highest rate seen, percent.
    pub max_rate: u8,
    /// The share of the window's pageviews recorded under a rate below 100,
    /// 0-1.
    pub sampled_share: f64,
}

#[derive(Debug, Row, Deserialize)]
struct SamplingRaw {
    lo: u8,
    hi: u8,
    sampled: u64,
    total: u64,
}

/// `GET /api/stats/sampling`.
pub async fn sampling(ch: &Ch, env: &QueryEnvelope) -> Result<Sampling> {
    let (scope, params) = scope_predicate(env, Table::Events)?;
    let sql = format!(
        "SELECT
  if(count() = 0, 100, min(sample_rate)) AS lo,
  if(count() = 0, 100, max(sample_rate)) AS hi,
  toUInt64(countIf(sample_rate < 100)) AS s_rows,
  toUInt64(count()) AS total
FROM events
WHERE {scope} AND kind = 'pageview'"
    );
    let row = fetch_all::<SamplingRaw>(ch, "sampling", &sql, &params)
        .await?
        .into_iter()
        .next();
    Ok(row.map(from_raw).unwrap_or_else(everyone))
}

/// The answer for a window with nothing sampled.
fn everyone() -> Sampling {
    Sampling {
        sampled: false,
        min_rate: 100,
        max_rate: 100,
        sampled_share: 0.0,
    }
}

fn from_raw(r: SamplingRaw) -> Sampling {
    if r.sampled == 0 || r.total == 0 {
        return everyone();
    }
    Sampling {
        sampled: true,
        min_rate: r.lo,
        max_rate: r.hi,
        sampled_share: round2(r.sampled as f64 / r.total as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_a_sample_only_when_a_row_says_so() {
        let raw = |lo, hi, sampled, total| SamplingRaw {
            lo,
            hi,
            sampled,
            total,
        };
        assert_eq!(from_raw(raw(100, 100, 0, 500)), everyone());
        assert_eq!(from_raw(raw(100, 100, 0, 0)), everyone());
        let half = from_raw(raw(25, 100, 250, 500));
        assert!(half.sampled);
        assert_eq!((half.min_rate, half.max_rate), (25, 100));
        assert!((half.sampled_share - 0.5).abs() < f64::EPSILON);
    }
}
