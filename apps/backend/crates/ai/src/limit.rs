//! The per-organisation daily question limit.
//!
//! Every surface that asks the model a question (the dashboard, MCP `ask`, the
//! chat apps, the investigations' explanations) takes one unit here first, so
//! one busy channel cannot spend an organisation's whole allowance and the
//! operator's bill is bounded by a number they chose. The window is the UTC
//! day, counted in Valkey with the same fixed-window counter the request rate
//! limiter uses.

use micaforge_core::config::LlmConfig;
use micaforge_core::{Error, Result};
use micaforge_store::Kv;
use uuid::Uuid;

/// One day, in seconds: the counter's window.
const DAY: u32 = 24 * 60 * 60;

/// Take one question from `org_id`'s allowance for today.
///
/// A limit of zero means none. Over the limit is `429` with the seconds until
/// the next UTC day, which is when the counter resets.
pub async fn take(kv: &Kv, cfg: &LlmConfig, org_id: Uuid) -> Result<()> {
    if cfg.daily_limit == 0 {
        return Ok(());
    }
    let key = format!("ai:day:{org_id}");
    if kv.rate_limit(&key, cfg.daily_limit, DAY).await? {
        return Ok(());
    }
    Err(Error::RateLimited {
        retry_after: seconds_to_midnight(chrono::Utc::now().timestamp()),
    })
}

/// Seconds from `now` (Unix seconds) until the next UTC midnight.
fn seconds_to_midnight(now: i64) -> u64 {
    let day = i64::from(DAY);
    (day - now.rem_euclid(day)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wait_runs_to_the_next_utc_day() {
        assert_eq!(seconds_to_midnight(0), 86_400);
        assert_eq!(seconds_to_midnight(86_400 - 10), 10);
        assert_eq!(seconds_to_midnight(86_400 + 3_600), 82_800);
    }
}
