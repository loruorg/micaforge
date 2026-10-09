//! `org_billing`, `billing_usage`, `stripe_events`: the hosted service's
//! billing state.
//!
//! Plans are configuration, so nothing here names a quota except the snapshot
//! the meter writes beside what it measured. See `migrations/0044_billing.sql`.

use chrono::{DateTime, NaiveDate, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::pg::PgPool;

/// What an organisation is subscribed to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OrgBilling {
    /// The organisation.
    pub org_id: Uuid,
    /// A configured plan id, `None` for the default plan.
    pub plan_id: Option<String>,
    /// Stripe's customer, once one exists.
    pub stripe_customer_id: Option<String>,
    /// Stripe's subscription, while one exists.
    pub stripe_subscription_id: Option<String>,
    /// Stripe's subscription status, or `none`.
    pub status: String,
    /// When the paid period ends.
    pub current_period_end: Option<DateTime<Utc>>,
    /// Whether the subscription ends at that point rather than renewing.
    pub cancel_at_period_end: bool,
    /// The last failed invoice payment, cleared by the next good one.
    pub payment_failed_at: Option<DateTime<Utc>>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
}

/// What the meter measured for one organisation in one month.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Usage {
    /// The organisation.
    pub org_id: Uuid,
    /// The first day of the calendar month (UTC).
    pub month: NaiveDate,
    /// `events` rows: the metered unit.
    pub events: i64,
    /// `agent_events` rows: shown, never counted.
    pub agent_events: i64,
    /// The quota in force when measured.
    pub quota: Option<i64>,
    /// Events past the quota.
    pub overage_events: i64,
    /// When the 80% email was sent this month.
    pub warned_80_at: Option<DateTime<Utc>>,
    /// When the 100% email was sent this month.
    pub warned_100_at: Option<DateTime<Utc>>,
    /// When this was measured.
    pub measured_at: DateTime<Utc>,
}

/// A subscription as Stripe last described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionState {
    /// The configured plan the subscription's price maps to, when one does.
    pub plan_id: Option<String>,
    /// Stripe's customer.
    pub customer_id: Option<String>,
    /// Stripe's subscription.
    pub subscription_id: Option<String>,
    /// Stripe's status.
    pub status: String,
    /// End of the paid period.
    pub current_period_end: Option<DateTime<Utc>>,
    /// Ends rather than renews.
    pub cancel_at_period_end: bool,
}

/// The subscription statuses under which a paid plan applies. `past_due`
/// keeps the plan while Stripe retries the payment, which is Stripe's own
/// grace period; anything else falls back to the free plan.
pub const PAID_STATUSES: &[&str] = &["active", "trialing", "past_due"];

/// The plan an organisation is on: its subscription's plan while the
/// subscription is in good standing, otherwise the configured free plan.
/// `None` when billing is off.
pub fn effective_plan<'a>(
    cfg: &'a micaforge_core::Config,
    billing: Option<&OrgBilling>,
) -> Option<&'a micaforge_core::config::BillingPlan> {
    let paid = billing
        .filter(|b| PAID_STATUSES.contains(&b.status.as_str()))
        .and_then(|b| b.plan_id.as_deref())
        .and_then(|id| cfg.plan(id));
    paid.or_else(|| cfg.default_plan())
}

const BILLING_COLS: &str = "org_id, plan_id, stripe_customer_id, stripe_subscription_id, status, \
     current_period_end, cancel_at_period_end, payment_failed_at, updated_at";
const USAGE_COLS: &str = "org_id, month, events, agent_events, quota, overage_events, \
     warned_80_at, warned_100_at, measured_at";

/// An organisation's billing row, if it has one.
pub async fn get(pool: &PgPool, org_id: Uuid) -> Result<Option<OrgBilling>> {
    let row = sqlx::query_as::<_, OrgBilling>(&format!(
        "SELECT {BILLING_COLS} FROM org_billing WHERE org_id = $1"
    ))
    .bind(org_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The organisation a Stripe customer belongs to.
pub async fn find_by_customer(pool: &PgPool, customer_id: &str) -> Result<Option<OrgBilling>> {
    let row = sqlx::query_as::<_, OrgBilling>(&format!(
        "SELECT {BILLING_COLS} FROM org_billing WHERE stripe_customer_id = $1"
    ))
    .bind(customer_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Remember an organisation's Stripe customer, creating its row if needed.
pub async fn set_customer(pool: &PgPool, org_id: Uuid, customer_id: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO org_billing (org_id, stripe_customer_id) VALUES ($1, $2)
         ON CONFLICT (org_id) DO UPDATE SET stripe_customer_id = EXCLUDED.stripe_customer_id,
                                            updated_at = now()",
    )
    .bind(org_id)
    .bind(customer_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Store what Stripe said about a subscription.
///
/// A good status (`active`, `trialing`) clears a recorded payment failure: the
/// invoice that failed has since been paid, or the subscription has moved on.
pub async fn apply_subscription(
    pool: &PgPool,
    org_id: Uuid,
    state: &SubscriptionState,
) -> Result<OrgBilling> {
    let row = sqlx::query_as::<_, OrgBilling>(&format!(
        "INSERT INTO org_billing
             (org_id, plan_id, stripe_customer_id, stripe_subscription_id, status,
              current_period_end, cancel_at_period_end, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, now())
         ON CONFLICT (org_id) DO UPDATE SET
             plan_id = EXCLUDED.plan_id,
             stripe_customer_id = COALESCE(EXCLUDED.stripe_customer_id, org_billing.stripe_customer_id),
             stripe_subscription_id = EXCLUDED.stripe_subscription_id,
             status = EXCLUDED.status,
             current_period_end = EXCLUDED.current_period_end,
             cancel_at_period_end = EXCLUDED.cancel_at_period_end,
             payment_failed_at = CASE WHEN EXCLUDED.status IN ('active', 'trialing')
                                      THEN NULL ELSE org_billing.payment_failed_at END,
             updated_at = now()
         RETURNING {BILLING_COLS}"
    ))
    .bind(org_id)
    .bind(&state.plan_id)
    .bind(&state.customer_id)
    .bind(&state.subscription_id)
    .bind(&state.status)
    .bind(state.current_period_end)
    .bind(state.cancel_at_period_end)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Record a failed invoice payment.
pub async fn mark_payment_failed(pool: &PgPool, org_id: Uuid) -> Result<()> {
    sqlx::query(
        "INSERT INTO org_billing (org_id, payment_failed_at) VALUES ($1, now())
         ON CONFLICT (org_id) DO UPDATE SET payment_failed_at = now(), updated_at = now()",
    )
    .bind(org_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Write what the meter measured, keeping the warning stamps already set.
pub async fn upsert_usage(
    pool: &PgPool,
    org_id: Uuid,
    month: NaiveDate,
    events: i64,
    agent_events: i64,
    quota: Option<i64>,
    overage_events: i64,
) -> Result<Usage> {
    let row = sqlx::query_as::<_, Usage>(&format!(
        "INSERT INTO billing_usage
             (org_id, month, events, agent_events, quota, overage_events, measured_at)
         VALUES ($1, $2, $3, $4, $5, $6, now())
         ON CONFLICT (org_id, month) DO UPDATE SET
             events = EXCLUDED.events,
             agent_events = EXCLUDED.agent_events,
             quota = EXCLUDED.quota,
             overage_events = EXCLUDED.overage_events,
             measured_at = now()
         RETURNING {USAGE_COLS}"
    ))
    .bind(org_id)
    .bind(month)
    .bind(events)
    .bind(agent_events)
    .bind(quota)
    .bind(overage_events)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// One organisation's usage row for one month.
pub async fn get_usage(pool: &PgPool, org_id: Uuid, month: NaiveDate) -> Result<Option<Usage>> {
    let row = sqlx::query_as::<_, Usage>(&format!(
        "SELECT {USAGE_COLS} FROM billing_usage WHERE org_id = $1 AND month = $2"
    ))
    .bind(org_id)
    .bind(month)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Which usage email to claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Threshold {
    /// 80% of the quota.
    Warn,
    /// The whole quota.
    Reached,
}

/// Claim the right to send one threshold's email for one month. `true` for
/// exactly one caller, however many workers try at once; the stamp it sets is
/// what stops the email going out twice.
pub async fn claim_warning(
    pool: &PgPool,
    org_id: Uuid,
    month: NaiveDate,
    threshold: Threshold,
) -> Result<bool> {
    let sql = match threshold {
        Threshold::Warn => {
            "UPDATE billing_usage SET warned_80_at = now()
             WHERE org_id = $1 AND month = $2 AND warned_80_at IS NULL"
        }
        Threshold::Reached => {
            "UPDATE billing_usage SET warned_100_at = now()
             WHERE org_id = $1 AND month = $2 AND warned_100_at IS NULL"
        }
    };
    let done = sqlx::query(sql)
        .bind(org_id)
        .bind(month)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Give back a claim whose email could not be sent, so the next sweep tries
/// again rather than the warning being lost.
pub async fn release_warning(
    pool: &PgPool,
    org_id: Uuid,
    month: NaiveDate,
    threshold: Threshold,
) -> Result<()> {
    let sql = match threshold {
        Threshold::Warn => {
            "UPDATE billing_usage SET warned_80_at = NULL WHERE org_id = $1 AND month = $2"
        }
        Threshold::Reached => {
            "UPDATE billing_usage SET warned_100_at = NULL WHERE org_id = $1 AND month = $2"
        }
    };
    sqlx::query(sql)
        .bind(org_id)
        .bind(month)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a Stripe event id. `true` the first time, `false` for a redelivery.
pub async fn record_stripe_event(pool: &PgPool, id: &str, kind: &str) -> Result<bool> {
    let done = sqlx::query(
        "INSERT INTO stripe_events (id, type) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(kind)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Forget a Stripe event id, so a delivery whose handling failed is handled
/// again when Stripe retries it.
pub async fn forget_stripe_event(pool: &PgPool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM stripe_events WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
