//! `meter_usage` — how many events each organisation recorded this month, and
//! the usage emails at 80% and 100% of its plan's quota.
//!
//! Only runs while billing is on (`MICAFORGE_BILLING_ENABLED`); a self-hosted
//! install never pays for it. The Billing screen measures on demand too
//! ([`measure`]), so usage is shown with billing off, read from the same
//! cached row this job writes.
//!
//! # What is metered
//!
//! Rows in `events` (pageviews, custom events, errors, web vitals) in the
//! calendar month, UTC, across every site the organisation holds when the
//! meter runs. `agent_events` (AI crawler visits recorded from a server) are
//! counted and shown but never measured against the quota: they are what this
//! product exists to make visible, and a quota on them would be a reason to
//! stop looking.
//!
//! # Past the quota
//!
//! Nothing is dropped. Ingest never consults the meter, so an organisation
//! over its quota keeps collecting exactly as before; the events above the
//! quota are recorded as overage on the month's row, shown on the Billing
//! screen, and named in the 100% email. [`OVERAGE_POLICY`] says so in the
//! words the screen and the emails print.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use micaforge_core::{Config, Error, Result};
use micaforge_mail::templates::UsageNotice;
use micaforge_store::models::billing::{self, Threshold, Usage};
use micaforge_store::models::{orgs, sites, users};
use micaforge_store::{ch::Ch, pg::PgPool};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::JobCtx;

/// What happens past the quota, as the Billing screen and the usage emails
/// print it.
pub const OVERAGE_POLICY: &str = "Micaforge keeps recording every event past the quota: \
     nothing is dropped and no tracking stops. Events above the quota are counted as overage \
     on the Billing screen, and the owner and admins get an email at 80% and at 100%. Choose a \
     larger plan to raise the quota; overage is not charged automatically.";

/// Nothing to configure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// How far along its quota an organisation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Below 80%, or no quota at all.
    Under,
    /// 80% or more.
    Warn,
    /// The whole quota or more.
    Reached,
}

/// Where `events` stands against `quota`. A quota of zero or none is no quota.
pub fn level(events: u64, quota: Option<u64>) -> Level {
    match quota {
        Some(quota) if quota > 0 => {
            if events >= quota {
                Level::Reached
            } else if u128::from(events) * 100 >= u128::from(quota) * 80 {
                Level::Warn
            } else {
                Level::Under
            }
        }
        _ => Level::Under,
    }
}

/// Events past the quota. Zero with no quota.
pub fn overage(events: u64, quota: Option<u64>) -> u64 {
    match quota {
        Some(quota) if quota > 0 => events.saturating_sub(quota),
        _ => 0,
    }
}

/// The calendar month (UTC) `now` falls in: its first day, and its bounds.
pub fn month_bounds(now: DateTime<Utc>) -> (NaiveDate, DateTime<Utc>, DateTime<Utc>) {
    let first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).unwrap_or(now.date_naive());
    let next = if now.month() == 12 {
        NaiveDate::from_ymd_opt(now.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(now.year(), now.month() + 1, 1)
    }
    .unwrap_or(first);
    let start = Utc.from_utc_datetime(&first.and_time(chrono::NaiveTime::MIN));
    let end = Utc.from_utc_datetime(&next.and_time(chrono::NaiveTime::MIN));
    (first, start, end)
}

/// The two counts the meter reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    events: u64,
    agent_events: u64,
}

/// Count one organisation's rows in a window, across its sites.
///
/// Two plain statements rather than one with two scalar subqueries: newer
/// ClickHouse releases type a scalar subquery as `Nullable`, which the typed
/// row reader would read one byte out of line.
async fn count(
    ch: &Ch,
    site_ids: &[u16],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Counts> {
    if site_ids.is_empty() {
        return Ok(Counts {
            events: 0,
            agent_events: 0,
        });
    }
    Ok(Counts {
        events: count_table(ch, "events", site_ids, start, end).await?,
        agent_events: count_table(ch, "agent_events", site_ids, start, end).await?,
    })
}

/// Rows of one table for some sites in a window. The table name is one of the
/// two literals [`count`] passes, never caller text.
async fn count_table(
    ch: &Ch,
    table: &'static str,
    site_ids: &[u16],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<u64> {
    let sql = format!(
        "SELECT count() FROM {table}
         WHERE has(?, site_id)
           AND timestamp >= fromUnixTimestamp64Milli(?)
           AND timestamp < fromUnixTimestamp64Milli(?)"
    );
    ch.client()
        .query(&sql)
        .bind(site_ids)
        .bind(crate::metrics::ms(start))
        .bind(crate::metrics::ms(end))
        .fetch_one::<u64>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse usage meter: {e}")))
}

/// Measure one organisation for the month `now` falls in, and store it.
///
/// The quota written beside the counts is the one in force now, so the row
/// says what the organisation was measured against.
pub async fn measure(
    ch: &Ch,
    pg: &PgPool,
    cfg: &Config,
    org_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Usage> {
    let site_ids: Vec<u16> = sites::list_for_org(pg, org_id)
        .await?
        .iter()
        .map(|s| s.site_id_u16())
        .collect();
    let (month, start, end) = month_bounds(now);
    let counts = count(ch, &site_ids, start, end).await?;
    let row = billing::get(pg, org_id).await?;
    let quota = billing::effective_plan(cfg, row.as_ref()).and_then(|p| p.events);
    billing::upsert_usage(
        pg,
        org_id,
        month,
        clamp_i64(counts.events),
        clamp_i64(counts.agent_events),
        quota.map(clamp_i64),
        clamp_i64(overage(counts.events, quota)),
    )
    .await
}

/// A ClickHouse count as a Postgres `bigint`.
fn clamp_i64(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// Measure every organisation that holds a site, and send the usage emails
/// that are due.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    if !ctx.cfg.billing_enabled {
        return Ok(());
    }
    let now = Utc::now();
    let mut by_org: BTreeMap<Uuid, usize> = BTreeMap::new();
    for site in sites::list_all(&ctx.pg).await? {
        *by_org.entry(site.org_id).or_default() += 1;
    }

    let mut measured = 0usize;
    let mut mailed = 0usize;
    for org_id in by_org.keys() {
        let usage = match measure(&ctx.ch, &ctx.pg, &ctx.cfg, *org_id, now).await {
            Ok(usage) => usage,
            Err(e) => {
                tracing::warn!(org = %org_id, error = %e, "usage could not be measured");
                continue;
            }
        };
        measured += 1;
        mailed += notify(ctx, *org_id, &usage).await?;
    }
    tracing::info!(measured, mailed, "usage metered");
    Ok(())
}

/// Send whichever usage email this month's row has newly crossed. Returns how
/// many messages went out.
///
/// Each threshold is claimed once per month in Postgres before anything is
/// sent, so two workers cannot both mail; a claim whose mail could not be
/// sent to anybody is given back, so the next sweep tries again. An
/// organisation that jumps straight past 100% gets the 100% email only, and
/// its 80% claim is spent so a later sweep does not send a stale warning.
async fn notify(ctx: &JobCtx, org_id: Uuid, usage: &Usage) -> Result<usize> {
    let events = u64::try_from(usage.events).unwrap_or(0);
    let quota = usage.quota.and_then(|q| u64::try_from(q).ok());
    let (threshold, reached) = match level(events, quota) {
        Level::Under => return Ok(0),
        Level::Warn => (Threshold::Warn, false),
        Level::Reached => {
            billing::claim_warning(&ctx.pg, org_id, usage.month, Threshold::Warn).await?;
            (Threshold::Reached, true)
        }
    };
    if !billing::claim_warning(&ctx.pg, org_id, usage.month, threshold).await? {
        return Ok(0);
    }

    let org = orgs::get(&ctx.pg, org_id).await?;
    let plan = billing::get(&ctx.pg, org_id)
        .await?
        .and_then(|b| billing::effective_plan(&ctx.cfg, Some(&b)).map(|p| p.name.clone()))
        .or_else(|| ctx.cfg.default_plan().map(|p| p.name.clone()))
        .unwrap_or_default();
    let notice = UsageNotice {
        org: org.name.clone(),
        plan,
        month: usage.month.format("%B %Y").to_string(),
        events,
        quota: quota.unwrap_or(0),
        reached,
        overage_policy: OVERAGE_POLICY.to_string(),
        link: format!("{}/app/settings/billing", ctx.cfg.dashboard_url),
    };

    let mut sent = 0usize;
    for address in org_admin_emails(&ctx.pg, org_id).await? {
        match ctx.mailer.send_usage(&address, &notice).await {
            Ok(()) => sent += 1,
            Err(e) => tracing::warn!(org = %org_id, error = %e, "a usage email could not be sent"),
        }
    }
    if sent == 0 {
        billing::release_warning(&ctx.pg, org_id, usage.month, threshold).await?;
    }
    Ok(sent)
}

/// The addresses of an organisation's owner and admins.
pub async fn org_admin_emails(pg: &PgPool, org_id: Uuid) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for member in orgs::list_members(pg, org_id).await? {
        if member.role != "owner" && member.role != "admin" {
            continue;
        }
        if let Ok(user) = users::get(pg, member.user_id).await {
            out.push(user.email);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_are_80_and_100_percent() {
        assert_eq!(level(0, Some(1_000)), Level::Under);
        assert_eq!(level(799, Some(1_000)), Level::Under);
        assert_eq!(level(800, Some(1_000)), Level::Warn);
        assert_eq!(level(999, Some(1_000)), Level::Warn);
        assert_eq!(level(1_000, Some(1_000)), Level::Reached);
        assert_eq!(level(5_000, Some(1_000)), Level::Reached);
        // No quota, or a zero one, is never a warning.
        assert_eq!(level(u64::MAX, None), Level::Under);
        assert_eq!(level(10, Some(0)), Level::Under);
        // No overflow at the top of the range.
        assert_eq!(level(u64::MAX, Some(u64::MAX)), Level::Reached);
        assert_eq!(level(u64::MAX - 1, Some(u64::MAX)), Level::Warn);
    }

    #[test]
    fn overage_is_what_is_past_the_quota_and_nothing_is_dropped() {
        assert_eq!(overage(1_200, Some(1_000)), 200);
        assert_eq!(overage(900, Some(1_000)), 0);
        assert_eq!(overage(900, None), 0);
    }

    #[test]
    fn a_month_runs_from_its_first_day_to_the_next_months() {
        let now = Utc.with_ymd_and_hms(2026, 12, 31, 23, 59, 59).unwrap();
        let (first, start, end) = month_bounds(now);
        assert_eq!(first, NaiveDate::from_ymd_opt(2026, 12, 1).unwrap());
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());

        let (_, _, feb_end) = month_bounds(Utc.with_ymd_and_hms(2028, 2, 10, 0, 0, 0).unwrap());
        assert_eq!(feb_end, Utc.with_ymd_and_hms(2028, 3, 1, 0, 0, 0).unwrap());
    }

    #[test]
    fn the_policy_says_nothing_is_dropped() {
        assert!(OVERAGE_POLICY.contains("nothing is dropped"));
    }

    /// The meter's two counts against a real ClickHouse: this month's rows of
    /// the organisation's sites, people and AI crawlers apart, and nothing
    /// from another site or another month. Opt in with
    /// `MICAFORGE_TEST_CLICKHOUSE_URL=http://user:password@host:8123` and
    /// `cargo test -p micaforge-jobs -- --ignored`.
    #[tokio::test]
    #[ignore = "needs a ClickHouse server"]
    async fn the_meter_counts_one_months_rows_of_the_organisations_sites() {
        use micaforge_store::ch::{AgentEventRow, EventRow};

        let raw = std::env::var("MICAFORGE_TEST_CLICKHOUSE_URL")
            .expect("set MICAFORGE_TEST_CLICKHOUSE_URL to run this test");
        let url = url::Url::parse(&raw).expect("a URL");
        let base = format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap(),
            url.port().unwrap_or(8123)
        );
        let db = format!("mf_test_{}", micaforge_core::crypto::numeric_code(9));
        let ch = Ch::connect(&base, &db, url.username(), url.password().unwrap_or("")).unwrap();
        ch.init_schema().await.unwrap();

        let now = Utc::now();
        let (_, start, end) = month_bounds(now);
        let last_month = start - chrono::Duration::days(2);

        let mut events = ch.client().insert::<EventRow>("events").unwrap();
        for (site_id, timestamp) in [(3u16, now), (3, now), (4, now), (3, last_month), (9, now)] {
            events
                .write(&EventRow {
                    site_id,
                    timestamp,
                    ..EventRow::default()
                })
                .await
                .unwrap();
        }
        events.end().await.unwrap();
        let mut agents = ch.client().insert::<AgentEventRow>("agent_events").unwrap();
        agents
            .write(&AgentEventRow {
                site_id: 4,
                timestamp: now,
                ..AgentEventRow::default()
            })
            .await
            .unwrap();
        agents.end().await.unwrap();

        let counts = count(&ch, &[3, 4], start, end).await.unwrap();
        assert_eq!(counts.events, 3);
        assert_eq!(counts.agent_events, 1);
        let none = count(&ch, &[], start, end).await.unwrap();
        assert_eq!(none.events, 0);

        let _ = ch.exec(&format!("DROP DATABASE IF EXISTS {db}")).await;
    }
}
