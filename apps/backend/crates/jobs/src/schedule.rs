//! Recurring work, without a scheduler process.
//!
//! Micaforge has no cron container and elects no leader. A recurring job is an
//! ordinary queue row that carries its own cadence in its payload, under the key
//! [`REPEAT_KEY`]. When the worker finishes such a row it queues the next
//! occurrence before it moves on, so the schedule is a property of the queue
//! rather than of a running process.
//!
//! Three things follow from that, and all three are why it is built this way:
//!
//! - **A restart costs nothing.** There is no in-memory timer wheel to rebuild,
//!   because the next occurrence is already a row with a `run_at`.
//! - **Several replicas cooperate for free.** `SKIP LOCKED` means exactly one
//!   worker runs each occurrence, whichever one gets there first.
//! - **Changing a cadence in code does not strand queued rows.** The row keeps
//!   the cadence it was created with until it next runs, and picks up the new one
//!   from [`RECURRING`] after that.
//!
//! A one-off run of the same kind — an operator pressing "refresh now" — is the
//! same row *without* the repeat key, so it happens once and stops. That
//! distinction is the whole of the scheduler.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use micaforge_core::Result;
use micaforge_store::pg::PgPool;
use serde_json::{json, Value as Json};

use crate::Kind;

/// The payload key that marks a row as one occurrence of a repeating job, and
/// holds the interval to the next one, in seconds.
///
/// It lives in the payload rather than in a column because `jobs` has no column
/// for it in `docs/CONTRACT.md` §2 and inventing one would be a schema change
/// for something the payload already carries perfectly well. Every job's typed
/// `Payload` ignores unknown keys, so this rides along invisibly.
pub const REPEAT_KEY: &str = "repeat_secs";

/// One recurring job.
#[derive(Debug, Clone, Copy)]
pub struct Recurring {
    /// What runs.
    pub kind: Kind,
    /// How long between the end of one occurrence and the start of the next.
    pub every: Duration,
    /// How long after seeding the first occurrence runs.
    ///
    /// Staggered per kind so that a fresh install does not fire ten jobs in the
    /// same second, one of which is a full retention sweep, while the operator is
    /// still watching the first pageview arrive.
    pub first_after: Duration,
}

/// Everything that runs on its own, and how often.
///
/// The cadences are chosen against what the data actually does, not against a
/// tidy set of round numbers:
///
/// - `check_alerts` every five minutes, because an alert an hour late is a
///   notification about history.
/// - `rollup_sessions` hourly, matching the merge cadence the session table
///   wants.
/// - `refresh_agent_ranges` every twelve hours: operators change published
///   ranges rarely, and every fetch is somebody else's endpoint.
/// - `crawl_site_policy`, `index_sitemap`, `crawl_page_meta` and
///   `prune_retention` daily. A site's `robots.txt` and sitemap are edited in
///   days, not minutes, and crawling a customer more often than that is rude.
/// - `detect_decay` weekly, because decay is a four-week comparison and running
///   it hourly would report the same answer 168 times.
pub const RECURRING: &[Recurring] = &[
    Recurring {
        kind: Kind::CheckAlerts,
        every: Duration::from_secs(5 * 60),
        first_after: Duration::from_secs(60),
    },
    Recurring {
        kind: Kind::SendReport,
        every: Duration::from_secs(15 * 60),
        first_after: Duration::from_secs(120),
    },
    Recurring {
        kind: Kind::RollupSessions,
        every: Duration::from_secs(60 * 60),
        first_after: Duration::from_secs(5 * 60),
    },
    Recurring {
        kind: Kind::RefreshAgentRanges,
        every: Duration::from_secs(12 * 60 * 60),
        first_after: Duration::from_secs(10 * 60),
    },
    Recurring {
        kind: Kind::CrawlSitePolicy,
        every: Duration::from_secs(24 * 60 * 60),
        first_after: Duration::from_secs(15 * 60),
    },
    Recurring {
        kind: Kind::IndexSitemap,
        every: Duration::from_secs(24 * 60 * 60),
        first_after: Duration::from_secs(25 * 60),
    },
    Recurring {
        kind: Kind::CrawlPageMeta,
        every: Duration::from_secs(24 * 60 * 60),
        first_after: Duration::from_secs(35 * 60),
    },
    Recurring {
        kind: Kind::PruneRetention,
        every: Duration::from_secs(24 * 60 * 60),
        first_after: Duration::from_secs(45 * 60),
    },
    Recurring {
        kind: Kind::DetectDecay,
        every: Duration::from_secs(7 * 24 * 60 * 60),
        first_after: Duration::from_secs(55 * 60),
    },
];

/// Seed the recurring jobs, idempotently.
///
/// Call it on every boot. For each entry in [`RECURRING`] it inserts one row
/// only if that kind has no repeating occurrence already queued or running, so
/// running it a hundred times leaves a hundred-times-unchanged queue.
///
/// The insert goes through
/// [`micaforge_store::models::jobs::enqueue_recurring`], whose
/// `ON CONFLICT DO NOTHING` against a unique partial index makes "one per kind"
/// hold even when two replicas boot in the same instant.
///
/// A row left `running` by a process that was killed mid-job still counts as
/// queued here. That is correct rather than a gap: it stops heartbeating, the
/// janitor returns it to the queue about two minutes later, and it runs again.
///
/// A one-off row of the same kind (no [`REPEAT_KEY`]) is invisible here, so an
/// operator's "refresh now" never suppresses the daily schedule.
pub async fn schedule_recurring(pg: &PgPool) -> Result<()> {
    let now = Utc::now();
    let mut seeded = 0usize;

    for entry in RECURRING {
        let payload = sweep_payload(entry.every);
        let run_at = now + chrono_from(entry.first_after);

        let inserted = micaforge_store::models::jobs::enqueue_recurring(
            pg,
            entry.kind.as_str(),
            &payload,
            run_at,
        )
        .await?;

        if inserted {
            seeded += 1;
            tracing::info!(
                kind = %entry.kind,
                every_secs = entry.every.as_secs(),
                first_run = %run_at,
                "recurring job seeded"
            );
        }
    }

    tracing::info!(
        seeded,
        already_queued = RECURRING.len() - seeded,
        "recurring schedule ready"
    );
    Ok(())
}

/// The payload one occurrence of a recurring sweep carries.
///
/// An empty object plus the cadence. Every sweep job reads its own target list
/// from Postgres rather than from the payload, which is what lets a site added
/// this afternoon be crawled tonight without anyone re-seeding the schedule.
pub fn sweep_payload(every: Duration) -> Json {
    json!({ REPEAT_KEY: every.as_secs().max(1) })
}

/// The cadence a payload carries, if it carries one.
///
/// `None` means "run this once", which is what an operator-triggered job and a
/// one-off import both are.
pub fn repeat_secs(payload: &Json) -> Option<u64> {
    payload.get(REPEAT_KEY)?.as_u64().filter(|s| *s > 0)
}

/// Queue one job to run as soon as a worker looks.
///
/// The typed way in for everything outside this crate: an operator pressing
/// "refresh policy now", an upload handler starting an import, a test. The
/// payload is the job module's own `Payload` struct, so a caller cannot enqueue a
/// `run_import` with a `crawl_site_policy` payload and find out about it eight
/// retries later.
///
/// A job queued this way carries no [`REPEAT_KEY`], so it happens once and stops.
pub async fn enqueue_now<T: serde::Serialize>(
    pg: &PgPool,
    kind: Kind,
    payload: &T,
) -> Result<uuid::Uuid> {
    enqueue_at(pg, kind, payload, Utc::now()).await
}

/// Queue one job for a particular time.
pub async fn enqueue_at<T: serde::Serialize>(
    pg: &PgPool,
    kind: Kind,
    payload: &T,
    run_at: DateTime<Utc>,
) -> Result<uuid::Uuid> {
    let payload = crate::encode(payload)?;
    let job = micaforge_store::models::jobs::enqueue(pg, kind.as_str(), &payload, run_at).await?;
    tracing::debug!(kind = %kind, job = %job.id, run_at = %run_at, "job queued");
    Ok(job.id)
}

/// Queue the next occurrence of a repeating job.
///
/// Called by the worker after an occurrence finishes, successfully or
/// terminally. Rescheduling after a *terminal* failure is the important half: a
/// site that was down for a whole day must not silently stop being crawled, and
/// a queue whose recurring rows quietly disappear on the first bad night is a
/// queue nobody can trust.
///
/// The cadence is re-read from [`RECURRING`] when the kind is still listed
/// there, so a cadence changed in code takes effect on the next occurrence
/// rather than only on the next `schedule_recurring`. A kind that has been
/// removed from the table keeps the interval its own payload carries, and then
/// stops when the operator deletes the row.
///
/// Also called by the janitor for a recurring row it parks as `failed`, which
/// is what stops a job that kept killing its worker from ending its schedule.
/// When an occurrence of the kind is already queued, this is a no-op.
pub async fn enqueue_next(pg: &PgPool, kind: Kind, payload: &Json) -> Result<()> {
    let Some(carried) = repeat_secs(payload) else {
        return Ok(());
    };

    let every = RECURRING
        .iter()
        .find(|r| r.kind == kind)
        .map(|r| r.every.as_secs().max(1))
        .unwrap_or(carried);

    let run_at = Utc::now() + ChronoDuration::seconds(every as i64);
    let next = merge_repeat(payload, every);

    if micaforge_store::models::jobs::enqueue_recurring(pg, kind.as_str(), &next, run_at).await? {
        tracing::debug!(kind = %kind, next_run = %run_at, "next occurrence queued");
    } else {
        tracing::debug!(kind = %kind, "next occurrence already queued");
    }
    Ok(())
}

/// Copy a payload, replacing its cadence.
///
/// Preserves any other keys, so a recurring job that was seeded with a target
/// (a single site, say) keeps that target across occurrences.
fn merge_repeat(payload: &Json, every: u64) -> Json {
    let mut map: BTreeMap<String, Json> = match payload {
        Json::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => BTreeMap::new(),
    };
    map.insert(REPEAT_KEY.to_string(), json!(every));
    Json::Object(map.into_iter().collect())
}

/// `std::time::Duration` to `chrono::Duration`, saturating.
///
/// The conversion is fallible in `chrono` because a `std` duration can exceed
/// what a `chrono` one holds. Nothing in [`RECURRING`] comes close, so the
/// saturating arm is unreachable in practice and is written as a week rather
/// than as a panic.
fn chrono_from(d: Duration) -> ChronoDuration {
    ChronoDuration::from_std(d).unwrap_or_else(|_| ChronoDuration::days(7))
}

/// When a job that has just failed should be tried again.
///
/// `attempts` squared, in minutes, capped at six hours: 1, 4, 9, 16, 25, 36
/// minutes, then 49 and 64, at which point [`MAX_ATTEMPTS`] has been reached.
///
/// The shape is chosen for what actually fails here. These jobs fail because
/// somebody else's server is down, an operator's range document moved, or an
/// SMTP relay is refusing connections — outages measured in minutes and hours,
/// not in milliseconds. A tighter schedule would spend eight retries inside the
/// first minute of a two-hour outage and then give up.
pub fn retry_delay(attempts: i32) -> Duration {
    /// Six hours. Longer than this and the retry lands in a different working
    /// day from the failure that caused it.
    const CAP_SECS: u64 = 6 * 60 * 60;

    let n = attempts.max(1) as u64;
    let secs = n.saturating_mul(n).saturating_mul(60);
    Duration::from_secs(secs.min(CAP_SECS))
}

/// How many times a job is tried before the worker parks it.
///
/// Eight attempts on the [`retry_delay`] curve spans a little over three hours,
/// which covers an ordinary outage and stops well short of retrying a genuinely
/// broken job forever. It is the store's constant rather than a second copy, so
/// the janitor's [`micaforge_store::models::jobs::reap_stale`] parks a row at
/// exactly the count this crate does.
pub const MAX_ATTEMPTS: i32 = micaforge_store::models::jobs::MAX_ATTEMPTS;

/// Record a failure and set the retry time, or park the job.
///
/// This is the one place the worker writes to `jobs` itself rather than going
/// through [`micaforge_store::models::jobs::fail`]. The store's version is the
/// generic queue backoff, thirty seconds doubling to an hour, which is right for
/// the small transactional work anything else might queue and wrong for these
/// jobs, whose failures are other people's outages. Both park at the same
/// [`MAX_ATTEMPTS`]. The decision
/// is still made in SQL against the row's own `attempts`, for the same reason the
/// store makes it there: reading the count and then writing a decision based on
/// it is a race, and doing both in one statement is not.
///
/// Returns `true` when the job was parked, so the caller can log it as a
/// dead letter and reschedule the cadence.
pub(crate) async fn fail_with_backoff(pg: &PgPool, job: &JobRef<'_>, error: &str) -> Result<bool> {
    let dead = job.attempts >= MAX_ATTEMPTS;
    let delay = retry_delay(job.attempts).as_secs() as f64;

    sqlx::query(
        "UPDATE jobs SET
            status = CASE WHEN attempts >= $2 THEN 'failed' ELSE 'pending' END,
            run_at = now() + make_interval(secs => $3::double precision),
            locked_by = NULL,
            locked_at = NULL
         WHERE id = $1",
    )
    .bind(job.id)
    .bind(MAX_ATTEMPTS)
    .bind(delay)
    .execute(pg)
    .await?;

    if dead {
        tracing::error!(job = %job.id, kind = job.kind, attempts = job.attempts, error, "job dead-lettered");
    } else {
        tracing::warn!(
            job = %job.id,
            kind = job.kind,
            attempts = job.attempts,
            retry_in_secs = delay,
            error,
            "job failed; will retry"
        );
    }

    Ok(dead)
}

/// The little bit of a claimed job that [`fail_with_backoff`] needs.
///
/// A narrow view rather than the whole [`micaforge_store::models::jobs::Job`],
/// so the failure path cannot accidentally depend on a payload that may be the
/// thing that broke. `kind` is the raw column value rather than a [`Kind`],
/// because the one failure that has no valid kind — a row written by a newer
/// build — still has to be recorded, and recording it under a wrong-but-valid
/// kind would put a lie in the log.
pub(crate) struct JobRef<'a> {
    /// The queue row.
    pub id: uuid::Uuid,
    /// What it was, as written in `jobs.kind`.
    pub kind: &'a str,
    /// Attempts *including* the one that just failed; `claim_next` increments it
    /// when it hands the row over.
    pub attempts: i32,
}

/// The next time a report of this cadence should go out, in the site's zone.
///
/// Reports leave at 08:00 local so that a daily digest is on the reader's screen
/// when they sit down, not at whatever hour UTC midnight happens to be where they
/// live. Weekly means Monday, monthly means the first.
///
/// Returns `None` only when the site's zone has no valid 08:00 on the chosen day,
/// which a daylight-saving spring-forward can produce; the caller falls back to
/// twenty-four hours from now rather than skipping a week.
pub fn next_report_run(
    after: DateTime<Utc>,
    cadence: &str,
    tz: chrono_tz::Tz,
) -> Option<DateTime<Utc>> {
    use chrono::{Datelike, Days, Months, NaiveTime, TimeZone};

    /// The hour a digest is worth reading.
    const SEND_HOUR: u32 = 8;

    let local = after.with_timezone(&tz);
    let send_at = NaiveTime::from_hms_opt(SEND_HOUR, 0, 0)?;

    let mut day = local.date_naive();
    day = match cadence {
        "weekly" => {
            // Monday of the following week.
            let ahead = 7 - (day.weekday().num_days_from_monday() as u64);
            day.checked_add_days(Days::new(ahead.max(1)))?
        }
        "monthly" => day.with_day(1)?.checked_add_months(Months::new(1))?,
        // Daily, and anything unrecognised: a cadence this build does not know
        // still has to send something rather than stop forever.
        _ => day.checked_add_days(Days::new(1))?,
    };

    let naive = day.and_time(send_at);
    tz.from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_retry_curve_climbs_and_then_stops_climbing() {
        assert_eq!(retry_delay(1), Duration::from_secs(60));
        assert_eq!(retry_delay(2), Duration::from_secs(4 * 60));
        assert_eq!(retry_delay(8), Duration::from_secs(64 * 60));
        // The cap, and proof it cannot be walked past by a corrupt attempt count.
        assert_eq!(retry_delay(1_000), Duration::from_secs(6 * 60 * 60));
        assert_eq!(retry_delay(i32::MAX), Duration::from_secs(6 * 60 * 60));
        // A zero or negative attempt count must still produce a real delay.
        assert_eq!(retry_delay(0), Duration::from_secs(60));
        assert_eq!(retry_delay(-5), Duration::from_secs(60));
    }

    #[test]
    fn a_sweep_payload_repeats_and_a_bare_payload_does_not() {
        let sweep = sweep_payload(Duration::from_secs(300));
        assert_eq!(repeat_secs(&sweep), Some(300));
        assert_eq!(repeat_secs(&json!({})), None);
        assert_eq!(repeat_secs(&json!({ "site": "x" })), None);
        // A zero cadence would queue the next occurrence in the same instant,
        // forever, so it is read as "not repeating".
        assert_eq!(repeat_secs(&json!({ REPEAT_KEY: 0 })), None);
    }

    #[test]
    fn rescheduling_keeps_the_rest_of_the_payload() {
        let payload = json!({ "site": "abc", REPEAT_KEY: 60 });
        let next = merge_repeat(&payload, 900);
        assert_eq!(next["site"], json!("abc"));
        assert_eq!(repeat_secs(&next), Some(900));
    }

    #[test]
    fn every_recurring_kind_is_a_real_kind_and_runs_at_a_distinct_time() {
        let mut offsets: Vec<u64> = RECURRING.iter().map(|r| r.first_after.as_secs()).collect();
        let before = offsets.len();
        offsets.sort_unstable();
        offsets.dedup();
        assert_eq!(
            offsets.len(),
            before,
            "two recurring jobs share a first run"
        );
        for r in RECURRING {
            assert!(Kind::parse(r.kind.as_str()).is_ok());
            assert!(!r.every.is_zero());
        }
        // An import is always explicit; it must never be seeded.
        assert!(!RECURRING.iter().any(|r| r.kind == Kind::RunImport));
    }

    #[test]
    fn the_worker_and_the_janitor_park_at_the_same_count() {
        assert_eq!(MAX_ATTEMPTS, micaforge_store::models::jobs::MAX_ATTEMPTS);
        // The retry curve still has to be climbing when the ceiling is hit, or
        // the last attempts are wasted at the cap.
        assert!(retry_delay(MAX_ATTEMPTS) < Duration::from_secs(6 * 60 * 60));
    }

    #[test]
    fn report_cadences_land_on_the_right_local_morning() {
        let tz: chrono_tz::Tz = "Europe/Lisbon".parse().unwrap();
        // A Thursday.
        let after = Utc.with_ymd_and_hms(2026, 8, 20, 14, 0, 0).unwrap();

        let daily = next_report_run(after, "daily", tz).unwrap();
        assert_eq!(
            daily.with_timezone(&tz).format("%Y-%m-%d %H").to_string(),
            "2026-08-21 08"
        );

        let weekly = next_report_run(after, "weekly", tz).unwrap();
        assert_eq!(
            weekly.with_timezone(&tz).format("%Y-%m-%d %H").to_string(),
            "2026-08-24 08"
        );

        let monthly = next_report_run(after, "monthly", tz).unwrap();
        assert_eq!(
            monthly.with_timezone(&tz).format("%Y-%m-%d %H").to_string(),
            "2026-09-01 08"
        );

        // An unknown cadence still schedules rather than stopping the report.
        assert!(next_report_run(after, "fortnightly", tz).is_some());
    }
}
