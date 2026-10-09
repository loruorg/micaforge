//! `revenue_integrations` and the bookkeeping a webhook needs to be safe to
//! receive twice.
//!
//! An integration is one provider (Stripe or Paddle) wired to one site, with
//! the signing secret sealed at rest. The payments themselves go to ClickHouse;
//! the three small tables here are what makes that write exactly-once in
//! practice:
//!
//! - [`claim_event`] records a provider event id before its rows are written,
//!   so a redelivery finds it and writes nothing. [`release_event`] hands the
//!   claim back when the write failed, so the provider's retry is processed.
//! - [`customer_ref`] remembers the checkout reference a customer's first
//!   checkout carried, for the invoices and refunds that follow without one.
//! - [`advance_refund_total`] turns Stripe's cumulative "amount refunded so
//!   far" into the amount refunded by this event.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// One provider wired to one site.
#[derive(Clone, sqlx::FromRow)]
pub struct Integration {
    /// UUIDv7 primary key. Also the last segment of the webhook URL.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// `stripe` | `paddle`.
    pub provider: String,
    /// The signing secret, sealed with this row's id as associated data.
    /// `None` until the operator pastes it.
    pub secret_sealed: Option<Vec<u8>>,
    /// `false` refuses deliveries without deleting the secret.
    pub enabled: bool,
    /// When the last event was recorded.
    pub last_event_at: Option<DateTime<Utc>>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

impl std::fmt::Debug for Integration {
    /// Hand-written so the sealed secret never reaches a log line, sealed or
    /// not.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Integration")
            .field("id", &self.id)
            .field("site_id", &self.site_id)
            .field("provider", &self.provider)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}

const COLS: &str = "id, site_id, provider, secret_sealed, enabled, last_event_at, created_at";

/// Create an integration, with its secret when the operator already has it.
pub async fn create(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    provider: &str,
    secret_sealed: Option<&[u8]>,
) -> Result<Integration> {
    let row = sqlx::query_as::<_, Integration>(&format!(
        "INSERT INTO revenue_integrations (id, site_id, provider, secret_sealed)
         VALUES ($1, $2, $3, $4) RETURNING {COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(provider)
    .bind(secret_sealed)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Integration> {
    let row = sqlx::query_as::<_, Integration>(&format!(
        "SELECT {COLS} FROM revenue_integrations WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "integration")
}

/// A site's integration for one provider, if it has one.
pub async fn find(pool: &PgPool, site_id: Uuid, provider: &str) -> Result<Option<Integration>> {
    let row = sqlx::query_as::<_, Integration>(&format!(
        "SELECT {COLS} FROM revenue_integrations WHERE site_id = $1 AND provider = $2"
    ))
    .bind(site_id)
    .bind(provider)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Every integration of a site.
pub async fn list_for_site(pool: &PgPool, site_id: Uuid) -> Result<Vec<Integration>> {
    let rows = sqlx::query_as::<_, Integration>(&format!(
        "SELECT {COLS} FROM revenue_integrations WHERE site_id = $1 ORDER BY provider"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Replace the sealed secret and, when given, the enabled switch.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    secret_sealed: Option<&[u8]>,
    enabled: Option<bool>,
) -> Result<Integration> {
    let row = sqlx::query_as::<_, Integration>(&format!(
        "UPDATE revenue_integrations
         SET secret_sealed = coalesce($2, secret_sealed), enabled = coalesce($3, enabled)
         WHERE id = $1 RETURNING {COLS}"
    ))
    .bind(id)
    .bind(secret_sealed)
    .bind(enabled)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "integration")
}

/// Delete an integration and its bookkeeping.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM revenue_integrations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record that an event arrived.
pub async fn touch(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE revenue_integrations SET last_event_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Event ids
// ---------------------------------------------------------------------------

/// How long a processed event id is remembered. Stripe retries a failed
/// delivery for three days and Paddle for about as long; a month is far past
/// either.
const EVENT_MEMORY_DAYS: f64 = 35.0;

/// Claim an event id. `false` means it was already processed (or is being
/// processed right now by another request), and the caller writes nothing.
pub async fn claim_event(pool: &PgPool, integration_id: Uuid, event_id: &str) -> Result<bool> {
    let claimed: Option<(String,)> = sqlx::query_as(
        "INSERT INTO revenue_events_seen (integration_id, event_id) VALUES ($1, $2)
         ON CONFLICT (integration_id, event_id) DO NOTHING
         RETURNING event_id",
    )
    .bind(integration_id)
    .bind(event_id)
    .fetch_optional(pool)
    .await?;
    Ok(claimed.is_some())
}

/// Hand a claim back after its rows failed to land, so a redelivery is
/// processed.
pub async fn release_event(pool: &PgPool, integration_id: Uuid, event_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM revenue_events_seen WHERE integration_id = $1 AND event_id = $2")
        .bind(integration_id)
        .bind(event_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Forget event ids older than any provider retries for.
pub async fn prune_events(pool: &PgPool, integration_id: Uuid) -> Result<()> {
    sqlx::query(
        "DELETE FROM revenue_events_seen
         WHERE integration_id = $1
           AND received_at < now() - make_interval(days => $2::integer)",
    )
    .bind(integration_id)
    .bind(EVENT_MEMORY_DAYS as i32)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Checkout references per customer
// ---------------------------------------------------------------------------

/// Remember the checkout reference a customer arrived with. The newest wins.
pub async fn remember_customer_ref(
    pool: &PgPool,
    integration_id: Uuid,
    customer_id: &str,
    checkout_ref: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO revenue_customer_refs (integration_id, customer_id, checkout_ref)
         VALUES ($1, $2, $3)
         ON CONFLICT (integration_id, customer_id)
         DO UPDATE SET checkout_ref = EXCLUDED.checkout_ref, updated_at = now()",
    )
    .bind(integration_id)
    .bind(customer_id)
    .bind(checkout_ref)
    .execute(pool)
    .await?;
    Ok(())
}

/// The checkout reference remembered for a customer.
pub async fn customer_ref(
    pool: &PgPool,
    integration_id: Uuid,
    customer_id: &str,
) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT checkout_ref FROM revenue_customer_refs
         WHERE integration_id = $1 AND customer_id = $2",
    )
    .bind(integration_id)
    .bind(customer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(r,)| r))
}

// ---------------------------------------------------------------------------
// Refund totals
// ---------------------------------------------------------------------------

/// Raise the recorded refund total of one charge or adjustment to `total`.
///
/// Returns `(before, after)`: the newly refunded amount is `after - before`,
/// zero when this event reports nothing new. The row is locked for the length
/// of the transaction, so two refund events for one charge arriving together
/// cannot both claim the same difference.
pub async fn advance_refund_total(
    pool: &PgPool,
    integration_id: Uuid,
    object_id: &str,
    total: i64,
) -> Result<(i64, i64)> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO revenue_refund_totals (integration_id, object_id, recorded_minor)
         VALUES ($1, $2, 0) ON CONFLICT (integration_id, object_id) DO NOTHING",
    )
    .bind(integration_id)
    .bind(object_id)
    .execute(&mut *tx)
    .await?;
    let (before,): (i64,) = sqlx::query_as(
        "SELECT recorded_minor FROM revenue_refund_totals
         WHERE integration_id = $1 AND object_id = $2 FOR UPDATE",
    )
    .bind(integration_id)
    .bind(object_id)
    .fetch_one(&mut *tx)
    .await?;
    let after = before.max(total);
    sqlx::query(
        "UPDATE revenue_refund_totals SET recorded_minor = $3
         WHERE integration_id = $1 AND object_id = $2",
    )
    .bind(integration_id)
    .bind(object_id)
    .bind(after)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((before, after))
}

/// Undo [`advance_refund_total`] after the refund row failed to land, unless
/// something has moved the total since.
pub async fn rewind_refund_total(
    pool: &PgPool,
    integration_id: Uuid,
    object_id: &str,
    before: i64,
    after: i64,
) -> Result<()> {
    sqlx::query(
        "UPDATE revenue_refund_totals SET recorded_minor = $3
         WHERE integration_id = $1 AND object_id = $2 AND recorded_minor = $4",
    )
    .bind(integration_id)
    .bind(object_id)
    .bind(before)
    .bind(after)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_sealed_secret() {
        let integration = Integration {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            provider: "stripe".into(),
            secret_sealed: Some(vec![0xAB; 48]),
            enabled: true,
            last_event_at: None,
            created_at: Utc::now(),
        };
        let printed = format!("{integration:?}");
        assert!(!printed.contains("171"), "{printed}");
        assert!(!printed.contains("secret"), "{printed}");
        assert!(printed.contains("stripe"));
    }
}
