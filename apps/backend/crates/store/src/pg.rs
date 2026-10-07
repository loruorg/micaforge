//! The Postgres pool, and the migration runner.
//!
//! Postgres is the system of record for identity and configuration only. It is
//! deliberately kept off the ingest path: a `POST /api/track` resolves a site
//! from a cache and a Redis key, never from a row here, so a Postgres stall
//! degrades the dashboard and leaves collection running.

use std::time::Duration;

use micaforge_core::{Error, Result};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::Connection;

/// The pool type every repository function takes.
///
/// Aliased rather than wrapped so a caller can still reach for a transaction or
/// a raw `sqlx` query when a repository function would be the wrong shape.
pub type PgPool = sqlx::PgPool;

/// Upper bound on pooled connections.
///
/// One deployed binary serves the API and runs the embedded workers, and the
/// target install is a single VPS whose Postgres ships with `max_connections =
/// 100`. Sixteen leaves generous headroom for `psql`, a backup and a second
/// replica of the binary during a rolling restart.
const MAX_CONNECTIONS: u32 = 16;

/// Keep a small warm floor so the first dashboard request after an idle night
/// does not pay for a TLS handshake.
const MIN_CONNECTIONS: u32 = 2;

/// How long a caller waits for a free connection before it gives up.
///
/// Shorter than any sensible HTTP timeout on purpose: if the pool is saturated
/// the honest answer is a fast 500 with a log line, not a request that hangs
/// until the client's own timeout fires and leaves us holding the work.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Recycle connections that have gone quiet, so a restarted Postgres or a
/// connection-killing proxy never leaves the pool full of dead sockets.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

/// The longest any one statement may run on a pooled connection.
///
/// Every query this product sends is an indexed lookup or a small write, and
/// the slowest legitimate one finishes in well under a second. Thirty seconds
/// is a ceiling for a runaway query or a lock wait, which would otherwise hold
/// a connection out of a pool of sixteen until the client gave up. Migrations
/// are exempt; see [`migrate`].
const STATEMENT_TIMEOUT: &str = "30s";

/// How long a connection may sit idle inside an open transaction.
///
/// A transaction left open holds its row locks, and the job queue's claim
/// query would queue behind them. Nothing here keeps a transaction open while
/// it waits on something else, so a minute means a bug or a stuck task, and
/// Postgres ending that session is the right outcome.
const IDLE_IN_TRANSACTION_TIMEOUT: &str = "60s";

/// Open the pool and prove the credentials work before returning.
///
/// The connection is verified eagerly. A misconfigured `MICAFORGE_DATABASE_URL`
/// should fail at boot with a readable message, not on the first request an hour
/// later.
pub async fn connect(url: &str) -> Result<PgPool> {
    if url.trim().is_empty() {
        return Err(Error::validation(
            "database_url",
            "MICAFORGE_DATABASE_URL is empty",
        ));
    }

    // Set in the startup packet rather than by a `SET` after connecting, so the
    // limits are in force from the first statement and cost no round trip.
    let options: PgConnectOptions = url
        .parse::<PgConnectOptions>()
        .map_err(|e| Error::validation("database_url", format!("not a Postgres URL: {e}")))?
        .options([
            ("statement_timeout", STATEMENT_TIMEOUT),
            (
                "idle_in_transaction_session_timeout",
                IDLE_IN_TRANSACTION_TIMEOUT,
            ),
        ]);

    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .min_connections(MIN_CONNECTIONS)
        .acquire_timeout(ACQUIRE_TIMEOUT)
        .idle_timeout(Some(IDLE_TIMEOUT))
        .test_before_acquire(true)
        .connect_with(options)
        .await
        .map_err(|e| Error::Database(format!("postgres connect: {e}")))
}

/// Run every pending migration in `apps/backend/migrations`.
///
/// The directory is embedded at compile time, which is what lets the self-hosted
/// image be a single binary: `docker compose up` migrates itself with no SQL
/// files mounted and no `sqlx-cli` in the image.
///
/// This is the one macro this crate uses. It reads a directory, not a database,
/// so the build still never needs a live Postgres or an offline query cache.
///
/// Migrations run on one connection with the statement timeout switched off.
/// Building an index on a large table is allowed to take longer than any
/// request is, and a migration killed halfway by [`STATEMENT_TIMEOUT`] would
/// leave a boot that fails the same way every time. The connection is closed
/// afterwards rather than returned to the pool, so the exemption never leaks
/// into a request.
pub async fn migrate(pool: &PgPool) -> Result<()> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|e| Error::Database(format!("migrate: {e}")))?
        .detach();

    sqlx::query("SET statement_timeout = 0")
        .execute(&mut conn)
        .await
        .map_err(|e| Error::Database(format!("migrate: {e}")))?;

    let outcome = sqlx::migrate!("../../migrations")
        .run(&mut conn)
        .await
        .map_err(|e| Error::Database(format!("migrate: {e}")));

    // Closing is best effort: the outcome that matters is the migration's.
    let _ = conn.close().await;
    outcome
}

/// A cheap readiness probe for `GET /api/health/ready`.
///
/// Deliberately `SELECT 1` and not a table read: health should answer "the
/// socket is up and the pool has a connection", and nothing about whether one
/// particular table happens to have rows in it.
pub async fn ping(pool: &PgPool) -> Result<()> {
    sqlx::query("SELECT 1").execute(pool).await?;
    Ok(())
}
