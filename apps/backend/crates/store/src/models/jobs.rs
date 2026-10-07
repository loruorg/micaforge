//! `jobs` — the work queue. `docs/CONTRACT.md` §2.
//!
//! Postgres is the queue, deliberately. The product ships as one binary plus
//! Postgres, ClickHouse and Redis, and adding a fourth service so that crawls
//! and reports could be scheduled would break the `docker compose up` promise
//! for no gain: this queue moves crawls, imports, alert sweeps and emails, which
//! is hundreds of jobs a day, not thousands a second.
//!
//! Claiming is `SELECT ... FOR UPDATE SKIP LOCKED`, which is what makes several
//! workers safe against each other without a lock table or a leader. Each worker
//! takes the oldest due row nobody else has locked, in one statement, and rows
//! nobody claimed are simply left for the next poll.
//!
//! Failure is retried with exponential backoff and then parked. A job that has
//! failed [`MAX_ATTEMPTS`] times is marked `failed` and left in the table rather
//! than deleted: a queue that quietly drops the work it could not do is a queue
//! that lies about what happened.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// How many times a job is tried before it is parked as `failed`.
///
/// The one ceiling for the whole queue: the worker's own retry path, the
/// generic [`fail`], the janitor's [`reap_stale`] and the admin queue view all
/// read this constant, so a row cannot be parked by one of them at a count the
/// others would still retry. Eight attempts on the worker's retry curve span a
/// little over three hours.
pub const MAX_ATTEMPTS: i32 = 8;

/// First retry delay, in seconds. Doubles each attempt.
const BASE_BACKOFF_SECS: i64 = 30;

/// Longest a retry is ever deferred, in seconds.
///
/// One hour. Uncapped doubling would push the fifth retry of a long-lived job
/// past the point anyone is still watching for it.
const MAX_BACKOFF_SECS: i64 = 3_600;

/// Anything a worker has been asked to do.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// What kind of work this is. The worker dispatches on it.
    pub kind: String,
    /// Arguments, shaped by the kind.
    pub payload: Json,
    /// Earliest time this may run. Also the retry time after a failure.
    pub run_at: DateTime<Utc>,
    /// How many times it has been tried.
    pub attempts: i32,
    /// Which worker holds it.
    pub locked_by: Option<String>,
    /// When that worker took it, so a dead worker's jobs can be reclaimed.
    pub locked_at: Option<DateTime<Utc>>,
    /// `pending` | `running` | `done` | `failed`.
    pub status: String,
}

impl Job {
    /// Decode the payload into a typed argument struct.
    ///
    /// A malformed payload is a `bad_request`, not an internal error: it was
    /// written by whoever enqueued the job, and that is the caller.
    pub fn payload_as<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.payload.clone()).map_err(Into::into)
    }
}

const COLS: &str = "id, kind, payload, run_at, attempts, locked_by, locked_at, status";

/// Put a job on the queue.
///
/// `run_at` in the past means "as soon as a worker looks", which is how an
/// immediate job is expressed. There is no separate "now" path.
pub async fn enqueue(
    pool: &PgPool,
    kind: &str,
    payload: &Json,
    run_at: DateTime<Utc>,
) -> Result<Job> {
    let row = sqlx::query_as::<_, Job>(&format!(
        "INSERT INTO jobs (id, kind, payload, run_at, attempts, status)
         VALUES ($1, $2, $3, $4, 0, 'pending') RETURNING {COLS}"
    ))
    .bind(micaforge_core::new_id())
    .bind(kind)
    .bind(payload)
    .bind(run_at)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Queue one occurrence of a recurring job, unless one is already queued.
///
/// "Already queued" means a `pending` or `running` row of the same kind that
/// carries a `repeat_secs` cadence. The unique partial index
/// `jobs_one_recurring_per_kind` (migration 0005) makes that a guarantee rather
/// than a check: two replicas seeding at the same instant, or a janitor and a
/// worker rescheduling the same kind, produce one row between them, and the
/// loser's insert is a no-op rather than an error.
///
/// Returns whether a row was inserted.
pub async fn enqueue_recurring(
    pool: &PgPool,
    kind: &str,
    payload: &Json,
    run_at: DateTime<Utc>,
) -> Result<bool> {
    let result = sqlx::query(
        "INSERT INTO jobs (id, kind, payload, run_at, attempts, status)
         VALUES ($1, $2, $3, $4, 0, 'pending')
         ON CONFLICT (kind)
             WHERE status IN ('pending', 'running') AND (payload -> 'repeat_secs') IS NOT NULL
         DO NOTHING",
    )
    .bind(micaforge_core::new_id())
    .bind(kind)
    .bind(payload)
    .bind(run_at)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Take the next due job of one of `kinds`, or `None` if there is nothing to do.
///
/// The whole claim is one statement. `FOR UPDATE SKIP LOCKED` inside the
/// subquery is what makes it safe: two workers polling at the same instant lock
/// different rows instead of blocking on each other, and neither ever sees a row
/// the other has taken.
///
/// `kinds` is bound as an array rather than interpolated, so a worker's
/// configured kind list can never become SQL.
///
/// An empty `kinds` claims nothing, which is the honest reading of "this worker
/// handles no kinds" and stops a misconfigured worker from draining the queue.
pub async fn claim_next(pool: &PgPool, worker_id: &str, kinds: &[String]) -> Result<Option<Job>> {
    if kinds.is_empty() {
        return Ok(None);
    }

    let row = sqlx::query_as::<_, Job>(&format!(
        "UPDATE jobs SET status = 'running', locked_by = $1, locked_at = now(), attempts = attempts + 1
         WHERE id = (
             SELECT id FROM jobs
             WHERE status = 'pending' AND run_at <= now() AND kind = ANY($2)
             ORDER BY run_at ASC, id ASC
             FOR UPDATE SKIP LOCKED
             LIMIT 1
         )
         RETURNING {COLS}"
    ))
    .bind(worker_id)
    .bind(kinds)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Fetch one job.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Job> {
    let row = sqlx::query_as::<_, Job>(&format!("SELECT {COLS} FROM jobs WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "job")
}

/// Prove that the worker holding a job is still alive.
///
/// A running job's lane calls this every thirty seconds. It moves `locked_at`
/// forward only while the row is still `running` under this lane's name, so a
/// lane whose row was reclaimed in the meantime learns that from the `false`
/// and does not quietly steal it back.
pub async fn heartbeat(pool: &PgPool, id: Uuid, worker_id: &str) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE jobs SET locked_at = now()
         WHERE id = $1 AND status = 'running' AND locked_by = $2",
    )
    .bind(id)
    .bind(worker_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Mark a job finished.
///
/// The row is kept, not deleted, so the admin surface can show what ran.
/// [`purge_finished`] is what removes it later.
pub async fn complete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query(
        "UPDATE jobs SET status = 'done', locked_by = NULL, locked_at = NULL WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a failure, and either schedule a retry or park the job.
///
/// The decision is made in SQL against the row's own `attempts`, which
/// [`claim_next`] already incremented. Reading the count and then writing a
/// decision based on it would be a race; doing both in one statement is not.
///
/// Backoff doubles from [`BASE_BACKOFF_SECS`] and is capped at
/// [`MAX_BACKOFF_SECS`]. The error text is not stored: `jobs` has no column for
/// it in `docs/CONTRACT.md` §2, so it goes to the log, where the stack that
/// produced it is anyway.
pub async fn fail(pool: &PgPool, id: Uuid, error: &str) -> Result<()> {
    tracing::warn!(job = %id, error, "job failed");

    sqlx::query(
        "UPDATE jobs SET
            status = CASE WHEN attempts >= $2 THEN 'failed' ELSE 'pending' END,
            run_at = now() + make_interval(secs => LEAST($3 * power(2, GREATEST(attempts - 1, 0)), $4)),
            locked_by = NULL,
            locked_at = NULL
         WHERE id = $1",
    )
    .bind(id)
    .bind(MAX_ATTEMPTS)
    .bind(BASE_BACKOFF_SECS as f64)
    .bind(MAX_BACKOFF_SECS as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Return jobs whose worker died back to the queue.
///
/// A worker that is killed mid-job leaves the row `running` with a lock nobody
/// holds, and nothing would ever pick it up again. A live worker moves
/// `locked_at` forward with [`heartbeat`] while the job runs, so a row whose
/// `locked_at` is older than `stale_after_secs` has lost its worker and is made
/// claimable, subject to the same attempt ceiling as a normal failure.
///
/// Returns the rows it touched, with their new status, so the caller can
/// reschedule a recurring job that this parked as `failed`.
pub async fn reap_stale(pool: &PgPool, stale_after_secs: i64) -> Result<Vec<Job>> {
    let rows = sqlx::query_as::<_, Job>(&format!(
        "UPDATE jobs SET
            status = CASE WHEN attempts >= $2 THEN 'failed' ELSE 'pending' END,
            locked_by = NULL,
            locked_at = NULL
         WHERE status = 'running'
           AND locked_at IS NOT NULL
           AND locked_at < now() - make_interval(secs => $1::double precision)
         RETURNING {COLS}"
    ))
    .bind(stale_after_secs.max(1) as f64)
    .bind(MAX_ATTEMPTS)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many jobs are due and waiting for a lane. The readiness route's queue
/// depth.
pub async fn due_count(pool: &PgPool) -> Result<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM jobs WHERE status = 'pending' AND run_at <= now()")
            .fetch_one(pool)
            .await?;
    Ok(count)
}

/// Jobs in one status, newest first. The admin queue view.
pub async fn list_by_status(pool: &PgPool, status: &str, limit: i64) -> Result<Vec<Job>> {
    let rows = sqlx::query_as::<_, Job>(&format!(
        "SELECT {COLS} FROM jobs WHERE status = $1 ORDER BY run_at DESC LIMIT $2"
    ))
    .bind(status)
    .bind(crate::models::clamp_limit(Some(limit)))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// How many jobs sit in each status.
pub async fn counts_by_status(pool: &PgPool) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, count(*) FROM jobs GROUP BY status ORDER BY status")
            .fetch_all(pool)
            .await?;
    Ok(rows)
}

/// Cancel a pending job.
///
/// Deliberately will not touch a `running` row: deleting a job out from under
/// the worker executing it would leave the work half done with nothing recording
/// that it happened.
pub async fn cancel(pool: &PgPool, id: Uuid) -> Result<bool> {
    let result = sqlx::query("DELETE FROM jobs WHERE id = $1 AND status = 'pending'")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Delete completed jobs older than `before`. Housekeeping.
///
/// Only `done` rows. A parked `failed` job stays until somebody looks at it,
/// which is the point of parking it.
pub async fn purge_finished(pool: &PgPool, before: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query("DELETE FROM jobs WHERE status = 'done' AND run_at < $1")
        .bind(before)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
