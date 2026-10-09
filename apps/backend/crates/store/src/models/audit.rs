//! `audit_events`: who changed what, and when.
//!
//! Written by one function in the server (`crate::audit::record`), read by the
//! Organisation audit screen and its CSV export. Rows are never updated: an
//! audit log that could be edited would only record what somebody was willing
//! to leave in it.
//!
//! An organisation's log is its own rows plus the account events (`org_id`
//! NULL: signing in, ending sessions) of the people who are members of it
//! now. See `migrations/0040_audit_events.sql` for why account events are not
//! copied into every organisation instead.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::pg::PgPool;

/// One recorded action.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuditEvent {
    /// UUIDv7 primary key; orders by time and pages the screen.
    pub id: Uuid,
    /// The organisation the change belongs to, `None` for an account event.
    pub org_id: Option<Uuid>,
    /// `user` | `api_key` | `system`.
    pub actor_kind: String,
    /// The user or key.
    pub actor_id: Option<Uuid>,
    /// Who, as it read at the time.
    pub actor_label: String,
    /// What happened, in dot form: `site.created`.
    pub action: String,
    /// What it happened to: `site`, `member`, `flag`, ...
    pub target_type: String,
    /// The target's id, as text (some targets are keyed by a slug or a
    /// fingerprint rather than a UUID).
    pub target_id: Option<String>,
    /// The target, as it read at the time.
    pub target_label: String,
    /// The site involved, when one was.
    pub site_id: Option<Uuid>,
    /// The target's relevant state before.
    pub before: Option<Json>,
    /// And after.
    pub after: Option<Json>,
    /// Anything else worth keeping.
    pub metadata: Option<Json>,
    /// A keyed digest of the caller's address.
    pub ip_hash: Option<String>,
    /// The caller's user agent, bounded.
    pub user_agent: Option<String>,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// What the screen narrows a read by. Every field but `org_id` is optional.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// The organisation whose log this is.
    pub org_id: Uuid,
    /// One user or key.
    pub actor_id: Option<Uuid>,
    /// An exact action, or a prefix ending in `.` (`site.`).
    pub action: Option<String>,
    /// One kind of target.
    pub target_type: Option<String>,
    /// One target.
    pub target_id: Option<String>,
    /// Inclusive lower bound.
    pub from: Option<DateTime<Utc>>,
    /// Exclusive upper bound.
    pub to: Option<DateTime<Utc>>,
    /// The cursor: only rows older than this id.
    pub before: Option<Uuid>,
    /// Page size.
    pub limit: i64,
}

const COLS: &str = "a.id, a.org_id, a.actor_kind, a.actor_id, a.actor_label, a.action, \
     a.target_type, a.target_id, a.target_label, a.site_id, a.before, a.after, a.metadata, \
     a.ip_hash, a.user_agent, a.created_at";

/// Write one event.
pub async fn insert(pool: &PgPool, event: &AuditEvent) -> Result<()> {
    sqlx::query(
        "INSERT INTO audit_events
             (id, org_id, actor_kind, actor_id, actor_label, action, target_type, target_id,
              target_label, site_id, before, after, metadata, ip_hash, user_agent, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(event.id)
    .bind(event.org_id)
    .bind(&event.actor_kind)
    .bind(event.actor_id)
    .bind(&event.actor_label)
    .bind(&event.action)
    .bind(&event.target_type)
    .bind(&event.target_id)
    .bind(&event.target_label)
    .bind(event.site_id)
    .bind(&event.before)
    .bind(&event.after)
    .bind(&event.metadata)
    .bind(&event.ip_hash)
    .bind(&event.user_agent)
    .bind(event.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// One page of an organisation's log, newest first.
///
/// The action filter is `starts_with` for a prefix ending in `.`, not `LIKE`,
/// so a `%` or `_` in the caller's text is a character and never a wildcard.
pub async fn list(pool: &PgPool, q: &AuditQuery) -> Result<Vec<AuditEvent>> {
    let rows = sqlx::query_as::<_, AuditEvent>(&format!(
        "SELECT {COLS} FROM audit_events a
         WHERE (a.org_id = $1
                OR (a.org_id IS NULL AND a.actor_kind = 'user'
                    AND a.actor_id IN (SELECT user_id FROM org_members WHERE org_id = $1)))
           AND ($2::uuid IS NULL OR a.actor_id = $2)
           AND ($3::text IS NULL
                OR a.action = $3
                OR (right($3, 1) = '.' AND starts_with(a.action, $3)))
           AND ($4::text IS NULL OR a.target_type = $4)
           AND ($5::text IS NULL OR a.target_id = $5)
           AND ($6::timestamptz IS NULL OR a.created_at >= $6)
           AND ($7::timestamptz IS NULL OR a.created_at < $7)
           AND ($8::uuid IS NULL OR a.id < $8)
         ORDER BY a.id DESC
         LIMIT $9"
    ))
    .bind(q.org_id)
    .bind(q.actor_id)
    .bind(&q.action)
    .bind(&q.target_type)
    .bind(&q.target_id)
    .bind(q.from)
    .bind(q.to)
    .bind(q.before)
    .bind(q.limit.max(1))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
