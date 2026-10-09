//! `/api/admin/*` — the self-hosted operator's view of the whole install.
//!
//! Everything here crosses tenant boundaries, so the gate is not a role on an
//! organisation but a property of the *instance*.
//!
//! ## Who is an instance admin
//!
//! Either of two things, and nothing else:
//!
//! 1. Their address is listed in `MICAFORGE_ADMIN_EMAILS`, a comma-separated
//!    list read once at boot, **and** they have verified it. Every sign-in is
//!    by a code sent to the address, so an account that has signed in has
//!    verified it; the second half still matters for an account carried over
//!    from a build that had passwords and has not signed in since, which proves
//!    only that somebody once typed the address into a form.
//! 2. They are the **first registered account**. On a `docker compose up`
//!    install the person who signs up first is the operator, and requiring them
//!    to also set an environment variable before they could see their own
//!    server's disk usage would be ceremony with no security value: they already
//!    hold the machine.
//!
//! API keys never reach this module: every route here refuses one, whatever
//! account it resolves against.
//!
//! Rule 2 is why registration closes. An install that leaves signup open has
//! given away nothing — the *second* account is an ordinary user — but the first
//! registration on a fresh, publicly reachable install claims the instance, and
//! `MICAFORGE_DISABLE_REGISTRATION` is how an operator shuts that door behind
//! them.
//!
//! ## Counting without counting
//!
//! `SELECT count() FROM events` on an install with billions of rows is a real
//! query with a real cost, and an admin dashboard that hurts the server it is
//! reporting on is a bad dashboard. Row counts and disk usage both come from
//! `system.parts`, which ClickHouse maintains as metadata: the numbers are exact
//! for row counts, current for bytes, and free.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use micaforge_store::models::{jobs, orgs, sites, users};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Environment variable holding the extra admin addresses.
const ADMIN_EMAILS_VAR: &str = "MICAFORGE_ADMIN_EMAILS";

/// How many failed jobs the queue view lists.
const FAILED_JOB_SAMPLE: i64 = 25;

/// Default page size for the admin lists.
const DEFAULT_PAGE: i64 = 50;

/// Addresses named by `MICAFORGE_ADMIN_EMAILS`, lower-cased, read once.
///
/// Read once rather than per request for the same reason `Config` is: the
/// environment does not change under a running process, and an authorisation
/// check that re-read it would invite the belief that it could be changed
/// without a restart.
static ADMIN_EMAILS: Lazy<Vec<String>> = Lazy::new(|| {
    std::env::var(ADMIN_EMAILS_VAR)
        .unwrap_or_default()
        .split(',')
        .map(|entry| entry.trim().to_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
});

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Is this account an instance admin?
///
/// Public because `/api/auth/me` reports it, so the dashboard knows whether to
/// render the admin section at all. The check is repeated on every admin route
/// regardless: a hidden menu item is not an access control.
pub async fn is_instance_admin(st: &AppState, user: &users::User) -> Result<bool> {
    if listed_and_verified(&ADMIN_EMAILS, user) {
        return Ok(true);
    }

    // Oldest account by creation, ties broken by id so the answer is stable
    // even for two rows written in the same millisecond.
    let first: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users ORDER BY created_at ASC, id ASC LIMIT 1")
            .fetch_optional(&st.pg)
            .await?;

    Ok(first == Some(user.id))
}

/// The addresses of every instance admin, for a notice meant for them.
///
/// The same two rules as [`is_instance_admin`]: every listed address whose
/// account has verified it, and the first account. Deduplicated, because the
/// first account is often also listed.
pub async fn instance_admin_addresses(st: &AppState) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for address in ADMIN_EMAILS.iter() {
        if let Some(user) = users::find_by_email(&st.pg, address).await? {
            if listed_and_verified(&ADMIN_EMAILS, &user) {
                out.push(user.email.to_lowercase());
            }
        }
    }
    let first: Option<String> =
        sqlx::query_scalar("SELECT email::text FROM users ORDER BY created_at ASC, id ASC LIMIT 1")
            .fetch_optional(&st.pg)
            .await?;
    if let Some(email) = first {
        let email = email.to_lowercase();
        if !out.contains(&email) {
            out.push(email);
        }
    }
    Ok(out)
}

/// The `MICAFORGE_ADMIN_EMAILS` rule: listed, and the address proven.
///
/// A code sign-in proves the address, but an account carried over from a
/// password build may never have done so, and a listed but unverified account
/// proves only that somebody typed the address into a form.
fn listed_and_verified(listed: &[String], user: &users::User) -> bool {
    let email = user.email.trim().to_lowercase();
    user.email_verified_at.is_some() && listed.contains(&email)
}

/// Refuse anybody who is not an instance admin, and every API key.
///
/// Reports `403`, not `404`: unlike an organisation id, the existence of the
/// admin surface is not a secret — every copy of this open-source product has
/// one — and a `404` here would only make a misconfigured install harder to
/// diagnose.
async fn require_admin(st: &AppState, auth: &AuthUser) -> Result<()> {
    auth.require_session()?;
    if is_instance_admin(st, &auth.user).await? {
        return Ok(());
    }
    Err(Error::Forbidden(
        "this needs an instance administrator; set MICAFORGE_ADMIN_EMAILS to grant it".into(),
    ))
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every instance-administration route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/stats", get(stats))
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users/{id}", axum::routing::delete(delete_user))
        .route("/api/admin/orgs", get(list_orgs))
        .route("/api/admin/sites", get(list_sites))
        .route("/api/admin/clickhouse", get(clickhouse))
        .route("/api/admin/jobs", get(job_health))
        .route("/api/admin/jobs/{id}", axum::routing::delete(cancel_job))
        .route("/api/admin/jobs/{id}/retry", post(retry_job))
}

// ---------------------------------------------------------------------------
// Instance overview
// ---------------------------------------------------------------------------

/// `GET /api/admin/stats` — one screen's worth of "is this install healthy".
async fn stats(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;

    let tables = table_sizes(&st).await.unwrap_or_default();
    let rows_in = |name: &str| {
        tables
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.rows)
            .unwrap_or(0)
    };
    let disk: u64 = tables.iter().map(|t| t.bytes).sum();

    let statuses = jobs::counts_by_status(&st.pg).await?;
    let all_sites = sites::list_all(&st.pg).await?;

    Ok(ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "self_hosted": st.cfg.self_hosted,
        "registration_open": !st.cfg.disable_registration,
        "mail_enabled": st.cfg.mail_enabled(),
        "retention_days": st.cfg.retention_days,
        "replay_retention_days": st.cfg.replay_retention_days,
        "counts": {
            "users": users::count(&st.pg).await?,
            "organizations": org_count(&st).await?,
            "sites": all_sites.len(),
            "public_sites": all_sites.iter().filter(|s| s.public).count(),
        },
        "events": {
            // From `system.parts` metadata, so this costs nothing however
            // large the install is.
            "human": rows_in("events"),
            "agent": rows_in("agent_events"),
            "bot": rows_in("bot_events"),
            "replay": rows_in("replay_events"),
            "errors": rows_in("errors"),
            "pages_indexed": rows_in("page_index"),
        },
        "clickhouse": {
            "database": st.ch.db,
            "bytes_on_disk": disk,
            "reachable": st.ch.ping().await.is_ok(),
        },
        "stores": {
            "postgres": micaforge_store::pg::ping(&st.pg).await.is_ok(),
            "redis": st.kv.ping().await.is_ok(),
        },
        "jobs": statuses
            .iter()
            .map(|(status, count)| json!({ "status": status, "count": count }))
            .collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// People and tenants
// ---------------------------------------------------------------------------

/// Paging for the admin lists.
#[derive(Debug, Deserialize)]
struct Paging {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    page: Option<i64>,
}

impl Paging {
    /// Rows to return, clamped by the repository's own ceiling.
    fn limit(&self) -> i64 {
        micaforge_store::models::clamp_limit(Some(self.limit.unwrap_or(DEFAULT_PAGE)))
    }

    /// Rows to skip. A page below one is page one, not a negative offset.
    fn offset(&self) -> i64 {
        (self.page.unwrap_or(1).max(1) - 1) * self.limit()
    }
}

/// `GET /api/admin/users` — every account on the install.
async fn list_users(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(paging): Query<Paging>,
) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;
    let rows = users::list(&st.pg, Some(paging.limit()), paging.offset()).await?;

    let mut out = Vec::with_capacity(rows.len());
    for user in rows {
        let memberships = orgs::list_for_user(&st.pg, user.id).await?;
        out.push(json!({
            "id": user.id,
            "email": user.email,
            "name": user.name,
            "email_verified": user.email_verified_at.is_some(),
            "totp_enabled": user.has_totp(),
            "created_at": user.created_at,
            "organizations": memberships.len(),
            "owns": memberships.iter().filter(|o| o.owner_id == user.id).count(),
            "instance_admin": is_instance_admin(&st, &user).await?,
        }));
    }

    Ok(ok(json!({
        "users": out,
        "total": users::count(&st.pg).await?,
    })))
}

/// `DELETE /api/admin/users/{id}` — remove an account.
///
/// Refused for anybody who still owns an organisation. Cascading the delete
/// would take that organisation's sites and configuration with it, which is
/// never what "remove this person" means; the organisation is transferred first,
/// deliberately, by somebody who knows what it holds.
///
/// Their events are untouched, because no event ever referenced them.
async fn delete_user(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;

    if id == auth.user.id {
        return Err(
            Error::Conflict("you cannot delete the account you are signed in as".into()).into(),
        );
    }

    let user = users::get(&st.pg, id).await?;
    let owned: Vec<_> = orgs::list_for_user(&st.pg, user.id)
        .await?
        .into_iter()
        .filter(|org| org.owner_id == user.id)
        .collect();

    if !owned.is_empty() {
        return Err(Error::Conflict(format!(
            "this account still owns {} organisation(s); transfer them first",
            owned.len()
        ))
        .into());
    }

    users::delete(&st.pg, user.id).await?;
    tracing::warn!(deleted = %user.id, actor = %auth.user.id, "account deleted by an instance admin");

    Ok(ok(json!({ "deleted": user.id, "events_removed": false })))
}

/// `GET /api/admin/orgs` — every organisation.
async fn list_orgs(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(paging): Query<Paging>,
) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;

    let rows = sqlx::query_as::<_, orgs::Organization>(
        "SELECT id, name, slug, owner_id, created_at FROM organizations
         ORDER BY created_at DESC, id DESC LIMIT $1 OFFSET $2",
    )
    .bind(paging.limit())
    .bind(paging.offset())
    .fetch_all(&st.pg)
    .await
    .map_err(Error::from)?;

    let mut out = Vec::with_capacity(rows.len());
    for org in rows {
        out.push(json!({
            "id": org.id,
            "name": org.name,
            "slug": org.slug,
            "owner_id": org.owner_id,
            "created_at": org.created_at,
            "members": orgs::list_members(&st.pg, org.id).await?.len(),
            "sites": sites::list_for_org(&st.pg, org.id).await?.len(),
        }));
    }

    Ok(ok(
        json!({ "organizations": out, "total": org_count(&st).await? }),
    ))
}

/// `GET /api/admin/sites` — every tracked property, in numeric-id order.
async fn list_sites(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;
    let rows = sites::list_all(&st.pg).await?;

    let out: Vec<Value> = rows
        .iter()
        .map(|site| {
            json!({
                "id": site.id,
                "site_id": site.site_id_u16(),
                "org_id": site.org_id,
                "domain": site.domain,
                "name": site.name,
                "timezone": site.timezone,
                "public": site.public,
                "created_at": site.created_at,
            })
        })
        .collect();

    Ok(ok(json!({ "sites": out, "total": rows.len() })))
}

/// How many organisations exist.
async fn org_count(st: &AppState) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM organizations")
        .fetch_one(&st.pg)
        .await?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// ClickHouse
// ---------------------------------------------------------------------------

/// One table's footprint, from `system.parts`.
#[derive(Debug, Clone, Default, clickhouse::Row, serde::Deserialize)]
struct TableSize {
    name: String,
    rows: u64,
    bytes: u64,
    uncompressed: u64,
    parts: u64,
}

/// `GET /api/admin/clickhouse` — what the event store is actually costing.
async fn clickhouse(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;
    let tables = table_sizes(&st).await?;

    let total_bytes: u64 = tables.iter().map(|t| t.bytes).sum();
    let total_rows: u64 = tables.iter().map(|t| t.rows).sum();

    let out: Vec<Value> = tables
        .iter()
        .map(|t| {
            json!({
                "table": t.name,
                "rows": t.rows,
                "bytes_on_disk": t.bytes,
                "uncompressed_bytes": t.uncompressed,
                "parts": t.parts,
                // The reason ClickHouse is the store: this is usually between
                // five and fifteen on an event table.
                "compression_ratio": ratio(t.uncompressed, t.bytes),
            })
        })
        .collect();

    Ok(ok(json!({
        "database": st.ch.db,
        "tables": out,
        "total_rows": total_rows,
        "total_bytes_on_disk": total_bytes,
        "retention_days": st.cfg.retention_days,
        "replay_retention_days": st.cfg.replay_retention_days,
    })))
}

/// Read every table's footprint from ClickHouse's own metadata.
///
/// `active = 1` matters: an inactive part is a merged-away duplicate that
/// ClickHouse has not yet deleted, and counting it would double-report both the
/// rows and the disk.
async fn table_sizes(st: &AppState) -> Result<Vec<TableSize>> {
    st.ch
        .client()
        .query(
            "SELECT
                 table AS name,
                 sum(rows) AS rows,
                 sum(bytes_on_disk) AS bytes,
                 sum(data_uncompressed_bytes) AS uncompressed,
                 count() AS parts
             FROM system.parts
             WHERE active = 1 AND database = ?
             GROUP BY table
             ORDER BY bytes DESC",
        )
        .bind(st.ch.db.as_str())
        .fetch_all::<TableSize>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse table sizes: {e}")))
}

/// Compression ratio, to two decimal places, or `null` when nothing is stored.
fn ratio(uncompressed: u64, on_disk: u64) -> Option<f64> {
    if on_disk == 0 {
        return None;
    }
    Some(((uncompressed as f64 / on_disk as f64) * 100.0).round() / 100.0)
}

// ---------------------------------------------------------------------------
// The job queue
// ---------------------------------------------------------------------------

/// `GET /api/admin/jobs` — is the worker keeping up?
///
/// Two numbers matter and neither is a total. **Backlog lag** is how long the
/// oldest *due* job has been waiting: a healthy queue answers in seconds, and a
/// lag of hours means the worker is stopped or wedged. **Parked failures** are
/// jobs that exhausted their retries and will never run again unless somebody
/// looks, which is exactly why they are kept rather than deleted.
async fn job_health(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;

    let statuses = jobs::counts_by_status(&st.pg).await?;
    let failed = jobs::list_by_status(&st.pg, "failed", FAILED_JOB_SAMPLE).await?;
    let running = jobs::list_by_status(&st.pg, "running", FAILED_JOB_SAMPLE).await?;

    let oldest_due: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT min(run_at) FROM jobs WHERE status = 'pending' AND run_at <= now()",
    )
    .fetch_one(&st.pg)
    .await
    .map_err(Error::from)?;
    let lag_seconds = oldest_due.map(|at| (Utc::now() - at).num_seconds().max(0));

    Ok(ok(json!({
        "counts": statuses
            .iter()
            .map(|(status, count)| json!({ "status": status, "count": count }))
            .collect::<Vec<_>>(),
        "backlog": {
            "oldest_due_at": oldest_due,
            "lag_seconds": lag_seconds,
        },
        "kinds": micaforge_jobs::Kind::ALL
            .iter()
            .map(|kind| json!({
                "kind": kind.as_str(),
                "timeout_seconds": kind.timeout().as_secs(),
                "failure_is_loud": kind.failure_is_loud(),
            }))
            .collect::<Vec<_>>(),
        "running": running.iter().map(job_json).collect::<Vec<_>>(),
        "failed": failed.iter().map(job_json).collect::<Vec<_>>(),
        "max_attempts": micaforge_store::models::jobs::MAX_ATTEMPTS,
    })))
}

/// `DELETE /api/admin/jobs/{id}` — drop a job that has not started.
///
/// Will not touch a `running` row. Deleting a job out from under the worker
/// executing it would leave the work half done with nothing recording that it
/// happened.
async fn cancel_job(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;

    if !jobs::cancel(&st.pg, id).await? {
        return Err(Error::Conflict(
            "only a pending job can be cancelled; a running one has to finish or time out".into(),
        )
        .into());
    }
    Ok(ok(json!({ "cancelled": id })))
}

/// `POST /api/admin/jobs/{id}/retry` — put a parked job back on the queue.
///
/// The attempt counter is reset, so the job gets a full run of retries again.
/// Without this a job parked by a transient outage — a relay that was down, a
/// site that was offline all night — could only be recovered by hand in SQL.
async fn retry_job(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;
    let job = jobs::get(&st.pg, id).await?;

    if job.status != "failed" {
        return Err(Error::Conflict(format!(
            "only a failed job can be retried; this one is `{}`",
            job.status
        ))
        .into());
    }

    // A recurring job that was parked has already queued its next occurrence,
    // and only one occurrence of a kind may be queued at a time (migration
    // 0005). Retrying it means running that occurrence now, not reviving this
    // row, which the unique index would refuse.
    if job.payload.get("repeat_secs").is_some() {
        let pulled = sqlx::query(
            "UPDATE jobs SET run_at = now()
             WHERE kind = $1 AND status = 'pending' AND (payload -> 'repeat_secs') IS NOT NULL",
        )
        .bind(&job.kind)
        .execute(&st.pg)
        .await
        .map_err(Error::from)?
        .rows_affected();
        if pulled > 0 {
            tracing::info!(job = %job.id, kind = %job.kind, actor = %auth.user.id, "next occurrence of a parked job run now");
            return Ok(ok(
                json!({ "requeued": job.id, "kind": job.kind, "next_occurrence_now": true }),
            ));
        }
    }

    sqlx::query(
        "UPDATE jobs SET status = 'pending', attempts = 0, run_at = now(),
                         locked_by = NULL, locked_at = NULL
         WHERE id = $1 AND status = 'failed'",
    )
    .bind(job.id)
    .execute(&st.pg)
    .await
    .map_err(Error::from)?;

    tracing::info!(job = %job.id, kind = %job.kind, actor = %auth.user.id, "parked job requeued");
    Ok(ok(json!({ "requeued": job.id, "kind": job.kind })))
}

/// One job on the wire. The payload is included: it is the whole reason a failed
/// job is worth looking at.
fn job_json(job: &jobs::Job) -> Value {
    json!({
        "id": job.id,
        "kind": job.kind,
        "payload": job.payload,
        "run_at": job.run_at,
        "attempts": job.attempts,
        "status": job.status,
        "locked_by": job.locked_by,
        "locked_at": job.locked_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_address_counts_only_once_it_is_verified() {
        let listed = vec!["ops@example.com".to_string()];
        let mut user = users::User {
            id: Uuid::nil(),
            email: "  Ops@Example.com ".into(),
            name: "Ops".into(),
            avatar_url: None,
            email_verified_at: None,
            totp_secret_sealed: None,
            created_at: Utc::now(),
        };
        // Holds the listed address, never proved it.
        assert!(!listed_and_verified(&listed, &user));

        user.email_verified_at = Some(Utc::now());
        assert!(listed_and_verified(&listed, &user));

        user.email = "someone@example.com".into();
        assert!(!listed_and_verified(&listed, &user));
    }

    #[test]
    fn paging_never_produces_a_negative_offset() {
        let first = Paging {
            limit: Some(10),
            page: Some(1),
        };
        assert_eq!(first.offset(), 0);

        let third = Paging {
            limit: Some(10),
            page: Some(3),
        };
        assert_eq!(third.offset(), 20);

        // Page zero and page minus one are both page one, not a query that
        // Postgres would refuse.
        for page in [Some(0), Some(-5), None] {
            let paging = Paging {
                limit: Some(10),
                page,
            };
            assert_eq!(paging.offset(), 0);
        }
    }

    #[test]
    fn the_page_size_respects_the_repositorys_ceiling() {
        let huge = Paging {
            limit: Some(100_000),
            page: Some(1),
        };
        assert!(huge.limit() <= micaforge_store::models::MAX_LIMIT);

        let none = Paging {
            limit: None,
            page: None,
        };
        assert_eq!(none.limit(), DEFAULT_PAGE);

        // A nonsense page size becomes a usable one rather than an empty page.
        let zero = Paging {
            limit: Some(0),
            page: Some(1),
        };
        assert!(zero.limit() >= 1);
    }

    #[test]
    fn the_compression_ratio_is_honest_about_an_empty_table() {
        assert_eq!(ratio(0, 0), None);
        assert_eq!(ratio(1_000, 100), Some(10.0));
        assert_eq!(ratio(1_050, 100), Some(10.5));
    }
}
