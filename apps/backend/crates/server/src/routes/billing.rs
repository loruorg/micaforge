//! `/api/billing/*`: plans, usage, and Stripe, for the hosted service.
//!
//! Billing is off by default (`MICAFORGE_BILLING_ENABLED`). With it off, the
//! Billing screen still shows what each organisation recorded this month,
//! because "how much am I using" is a fair question on any install, and the
//! limits are the instance-wide `MICAFORGE_MAX_SITES_PER_ORG` they always
//! were. With it on, every organisation is on a configured plan
//! (`MICAFORGE_BILLING_PLANS`) whose site and member limits replace the
//! instance-wide ones, and whose event quota the `meter_usage` job measures
//! against. Stripe is a further step: with `MICAFORGE_STRIPE_SECRET_KEY` and
//! `MICAFORGE_STRIPE_WEBHOOK_SECRET` set, an owner can buy a plan through
//! Stripe Checkout and manage it in Stripe's customer portal, and Stripe's
//! webhook keeps the subscription here in step.
//!
//! No call to Stripe is made unless both Stripe variables are set and billing
//! is on; a self-hosted install never makes one.
//!
//! ## The webhook
//!
//! `POST /api/billing/webhook` is unauthenticated by necessity and
//! authenticated by signature: `Stripe-Signature: t=<unix>,v1=<hex>`, where
//! `v1` is HMAC-SHA256 keyed with the webhook secret over `"<t>.<raw body>"`,
//! exactly the construction `/api/log/edge` uses. A signature older than five
//! minutes is refused. Each event id is recorded before it is handled, so a
//! redelivery changes nothing, and forgotten again when handling fails, so
//! Stripe's retry gets a second try.

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use micaforge_core::config::{BillingPlan, StripeKeys};
use micaforge_core::{Error, Result};
use micaforge_jobs::meter_usage::{self, OVERAGE_POLICY};
use micaforge_store::models::billing::{self, OrgBilling, SubscriptionState};
use micaforge_store::models::{orgs, sites};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::audit::{self, Actor, Entry};
use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::orgs::{authorize_org, ADMIN, OWNER};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Stripe's API root.
const STRIPE_API: &str = "https://api.stripe.com";

/// How far a webhook's signed time may be from now, in seconds. Stripe's own
/// libraries use the same five minutes.
const SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// How stale the cached usage row may be before the screen measures again.
const USAGE_STALE_SECS: i64 = 15 * 60;

/// Every billing route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/billing", get(overview))
        .route("/api/billing/checkout", post(checkout))
        .route("/api/billing/portal", post(portal))
        .route("/api/billing/webhook", post(webhook))
}

// ---------------------------------------------------------------------------
// Limits, for the routes that create sites and invite people
// ---------------------------------------------------------------------------

/// The most sites an organisation may hold, `0` for no limit.
///
/// With billing on, the organisation's plan decides (a plan with no site
/// limit has none). With billing off, `MICAFORGE_MAX_SITES_PER_ORG` does, as it
/// always has. Instance admins are exempt in either case; the callers check.
pub(crate) async fn site_cap(st: &AppState, org_id: Uuid) -> Result<u32> {
    if !st.cfg.billing_enabled {
        return Ok(st.cfg.max_sites_per_org);
    }
    let row = billing::get(&st.pg, org_id).await?;
    Ok(billing::effective_plan(&st.cfg, row.as_ref())
        .and_then(|p| p.sites)
        .unwrap_or(0))
}

/// The most members (pending invitations included) an organisation may have,
/// `0` for no limit. Only a plan sets one.
pub(crate) async fn member_cap(st: &AppState, org_id: Uuid) -> Result<u32> {
    if !st.cfg.billing_enabled {
        return Ok(0);
    }
    let row = billing::get(&st.pg, org_id).await?;
    Ok(billing::effective_plan(&st.cfg, row.as_ref())
        .and_then(|p| p.members)
        .unwrap_or(0))
}

// ---------------------------------------------------------------------------
// The Billing screen
// ---------------------------------------------------------------------------

/// Which organisation.
#[derive(Debug, Deserialize)]
struct OrgQuery {
    org: Uuid,
}

/// `GET /api/billing?org=…` — the plan, the subscription, this month's usage
/// and the limits. `admin`, because the numbers describe the whole
/// organisation.
async fn overview(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<OrgQuery>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, q.org, auth.user.id, ADMIN).await?;
    let row = billing::get(&st.pg, org.id).await?;
    let plan = billing::effective_plan(&st.cfg, row.as_ref());
    let stripe = st.cfg.stripe_keys().is_some();

    let now = Utc::now();
    let (month, _, _) = meter_usage::month_bounds(now);
    let usage = match billing::get_usage(&st.pg, org.id, month).await? {
        Some(fresh) if (now - fresh.measured_at).num_seconds() < USAGE_STALE_SECS => fresh,
        _ => meter_usage::measure(&st.ch, &st.pg, &st.cfg, org.id, now).await?,
    };

    let sites_used = sites::list_for_org(&st.pg, org.id).await?.len();
    let members_used = orgs::list_members(&st.pg, org.id).await?.len();
    let site_max = site_cap(&st, org.id).await?;
    let member_max = member_cap(&st, org.id).await?;
    let quota = usage.quota;

    let plans: Vec<Value> = if st.cfg.billing_enabled {
        st.cfg
            .billing_plans
            .iter()
            .map(|p| plan_json(p, stripe, plan.is_some_and(|c| c.id == p.id)))
            .collect()
    } else {
        Vec::new()
    };

    Ok(ok(json!({
        "enabled": st.cfg.billing_enabled,
        "stripe": stripe,
        "plan": plan.map(|p| plan_json(p, stripe, true)),
        "subscription": row.as_ref().filter(|b| b.status != "none").map(subscription_json),
        "payment_failed_at": row.as_ref().and_then(|b| b.payment_failed_at),
        "plans": plans,
        "usage": {
            "month": month.format("%Y-%m").to_string(),
            "events": usage.events,
            "agent_events": usage.agent_events,
            "quota": quota,
            "overage": usage.overage_events,
            "percent": quota.filter(|q| *q > 0).map(|q| usage.events as f64 / q as f64 * 100.0),
            "updated_at": usage.measured_at,
        },
        "limits": {
            "sites": { "used": sites_used, "max": (site_max > 0).then_some(site_max) },
            "members": { "used": members_used, "max": (member_max > 0).then_some(member_max) },
        },
        "overage_policy": OVERAGE_POLICY,
    })))
}

/// One plan on the wire.
fn plan_json(plan: &BillingPlan, stripe: bool, current: bool) -> Value {
    json!({
        "id": plan.id,
        "name": plan.name,
        "events": plan.events,
        "sites": plan.sites,
        "members": plan.members,
        "price_label": plan.price_label,
        "purchasable": stripe && plan.price_id.is_some(),
        "current": current,
    })
}

/// A subscription on the wire. Stripe's ids stay on the server.
fn subscription_json(row: &OrgBilling) -> Value {
    json!({
        "status": row.status,
        "plan_id": row.plan_id,
        "current_period_end": row.current_period_end,
        "cancel_at_period_end": row.cancel_at_period_end,
    })
}

// ---------------------------------------------------------------------------
// Checkout and portal
// ---------------------------------------------------------------------------

/// Which plan to buy, for which organisation.
#[derive(Debug, Deserialize)]
struct CheckoutBody {
    org_id: Uuid,
    plan: String,
}

/// The organisation id, and Stripe, for an owner-only billing action.
async fn owner_and_stripe<'a>(
    st: &'a AppState,
    auth: &AuthUser,
    org_id: Uuid,
) -> Result<(orgs::Organization, &'a StripeKeys)> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, org_id, auth.user.id, OWNER).await?;
    let keys = st
        .cfg
        .stripe_keys()
        .ok_or_else(|| Error::Conflict("payments are not set up on this install".to_string()))?;
    Ok((org, keys))
}

/// `POST /api/billing/checkout` — start a Stripe Checkout for a plan, and
/// hand the browser the address to go to. Owner only.
async fn checkout(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CheckoutBody>,
) -> ApiResult<Json<Value>> {
    let (org, keys) = owner_and_stripe(&st, &auth, body.org_id).await?;
    let plan = st
        .cfg
        .plan(body.plan.trim())
        .filter(|p| p.price_id.is_some())
        .ok_or_else(|| Error::validation("plan", "is not a plan that can be bought"))?;
    let existing = billing::get(&st.pg, org.id).await?;

    let form = checkout_form(
        &st.cfg.dashboard_url,
        org.id,
        plan,
        existing
            .as_ref()
            .and_then(|b| b.stripe_customer_id.as_deref()),
        &auth.user.email,
    );
    let session = stripe_post(&st.http, keys, "/v1/checkout/sessions", &form).await?;
    let url = session
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Upstream("stripe: the checkout session has no url".into()))?;

    audit::record(
        &st,
        &auth,
        Entry::new("billing.checkout_started")
            .organisation(&org)
            .meta(json!({ "plan": plan.id })),
    )
    .await;
    Ok(ok(json!({ "url": url })))
}

/// The form a Checkout Session is created with.
///
/// The organisation id travels three ways (`client_reference_id`, the
/// session's metadata and the subscription's metadata) because the webhook
/// events that follow carry different objects, and each has to lead back to
/// the organisation without trusting anything the browser touched.
fn checkout_form(
    dashboard: &str,
    org_id: Uuid,
    plan: &BillingPlan,
    customer: Option<&str>,
    email: &str,
) -> Vec<(String, String)> {
    let back = format!("{dashboard}/app/settings/billing");
    let mut form = vec![
        ("mode".to_string(), "subscription".to_string()),
        (
            "line_items[0][price]".to_string(),
            plan.price_id.clone().unwrap_or_default(),
        ),
        ("line_items[0][quantity]".to_string(), "1".to_string()),
        ("client_reference_id".to_string(), org_id.to_string()),
        ("metadata[org_id]".to_string(), org_id.to_string()),
        (
            "subscription_data[metadata][org_id]".to_string(),
            org_id.to_string(),
        ),
        (
            "success_url".to_string(),
            format!("{back}?billing=success&org={org_id}"),
        ),
        (
            "cancel_url".to_string(),
            format!("{back}?billing=cancelled&org={org_id}"),
        ),
    ];
    match customer {
        Some(id) => form.push(("customer".to_string(), id.to_string())),
        None => form.push(("customer_email".to_string(), email.to_string())),
    }
    form
}

/// Which organisation's portal.
#[derive(Debug, Deserialize)]
struct PortalBody {
    org_id: Uuid,
}

/// `POST /api/billing/portal` — a Stripe customer portal session, where the
/// owner changes the card, the plan or cancels. Owner only.
async fn portal(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PortalBody>,
) -> ApiResult<Json<Value>> {
    let (org, keys) = owner_and_stripe(&st, &auth, body.org_id).await?;
    let customer = billing::get(&st.pg, org.id)
        .await?
        .and_then(|b| b.stripe_customer_id)
        .ok_or_else(|| {
            Error::Conflict("this organisation has not bought a plan yet".to_string())
        })?;
    let form = vec![
        ("customer".to_string(), customer),
        (
            "return_url".to_string(),
            format!("{}/app/settings/billing", st.cfg.dashboard_url),
        ),
    ];
    let session = stripe_post(&st.http, keys, "/v1/billing_portal/sessions", &form).await?;
    let url = session
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Upstream("stripe: the portal session has no url".into()))?;
    Ok(ok(json!({ "url": url })))
}

/// One form-encoded `POST` to Stripe.
///
/// Stripe's message is passed on in the error, never the key: the key is in a
/// header reqwest does not print, and nothing here formats it.
async fn stripe_post(
    http: &reqwest::Client,
    keys: &StripeKeys,
    path: &str,
    form: &[(String, String)],
) -> Result<Value> {
    let response = http
        .post(format!("{STRIPE_API}{path}"))
        .bearer_auth(&keys.secret_key)
        .form(form)
        .send()
        .await
        .map_err(|e| Error::Upstream(format!("stripe: {}", e.without_url())))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|e| Error::Upstream(format!("stripe: unreadable answer: {}", e.without_url())))?;
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("request refused");
        return Err(Error::Upstream(format!(
            "stripe answered {status}: {message}"
        )));
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// The webhook
// ---------------------------------------------------------------------------

/// Verify a `Stripe-Signature` header against the raw body.
///
/// The header carries `t=<unix seconds>` and one or more `v1=<hex>`; any `v1`
/// matching `HMAC-SHA256(secret, "<t>.<body>")` passes, compared in constant
/// time. A `t` more than [`SIGNATURE_TOLERANCE_SECS`] from `now` fails however
/// good the MAC, so a captured delivery cannot be replayed later.
pub(crate) fn verify_stripe_signature(secret: &str, header: &str, body: &[u8], now: i64) -> bool {
    let mut timestamp: Option<&str> = None;
    let mut candidates: Vec<&str> = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = Some(value),
            Some(("v1", value)) => candidates.push(value),
            _ => {}
        }
    }
    let Some(t) = timestamp else {
        return false;
    };
    let Ok(seconds) = t.parse::<i64>() else {
        return false;
    };
    if (now - seconds).abs() > SIGNATURE_TOLERANCE_SECS {
        return false;
    }

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(t.as_bytes());
    mac.update(b".");
    mac.update(body);
    let expected = hex::encode(mac.finalize().into_bytes());
    candidates
        .iter()
        .any(|v1| bool::from(v1.as_bytes().ct_eq(expected.as_bytes())))
}

/// `POST /api/billing/webhook` — Stripe telling us what changed.
async fn webhook(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let Some(keys) = st.cfg.stripe_keys() else {
        return Err(Error::not_found("billing").into());
    };
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !verify_stripe_signature(
        &keys.webhook_secret,
        signature,
        &body,
        Utc::now().timestamp(),
    ) {
        return Err(Error::Unauthorized("the Stripe signature does not verify".into()).into());
    }

    let event: Value = serde_json::from_slice(&body)
        .map_err(|_| Error::BadRequest("the event is not JSON".into()))?;
    let id = event.get("id").and_then(Value::as_str).unwrap_or("");
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    if id.is_empty() || kind.is_empty() {
        return Err(Error::BadRequest("the event has no id or type".into()).into());
    }
    if !billing::record_stripe_event(&st.pg, id, kind).await? {
        return Ok(ok(json!({ "received": true, "duplicate": true })));
    }

    let object = event
        .pointer("/data/object")
        .cloned()
        .unwrap_or(Value::Null);
    if let Err(e) = handle_event(&st, kind, &object).await {
        // Forget it, so Stripe's retry is handled rather than skipped.
        billing::forget_stripe_event(&st.pg, id).await?;
        tracing::error!(event = id, kind, error = %e, "a Stripe event could not be handled");
        return Err(e.into());
    }
    Ok(ok(json!({ "received": true })))
}

/// Apply one verified event.
async fn handle_event(st: &AppState, kind: &str, object: &Value) -> Result<()> {
    match kind {
        "checkout.session.completed" => {
            let org = object
                .get("client_reference_id")
                .and_then(Value::as_str)
                .or_else(|| object.pointer("/metadata/org_id").and_then(Value::as_str))
                .and_then(|s| s.parse::<Uuid>().ok());
            let customer = object.get("customer").and_then(Value::as_str);
            if let (Some(org), Some(customer)) = (org, customer) {
                // The organisation may have been deleted while its owner was at
                // Stripe; there is nothing to attach the customer to then.
                if orgs::get(&st.pg, org).await.is_ok() {
                    billing::set_customer(&st.pg, org, customer).await?;
                }
            }
            Ok(())
        }
        "customer.subscription.created"
        | "customer.subscription.updated"
        | "customer.subscription.deleted" => {
            let Some(org_id) = org_of(st, object).await? else {
                tracing::warn!(
                    kind,
                    "a subscription event names no organisation this install knows"
                );
                return Ok(());
            };
            let mut state = subscription_state(&st.cfg, object);
            if kind == "customer.subscription.deleted" {
                state.status = "canceled".to_string();
                state.plan_id = None;
            }
            let before = billing::get(&st.pg, org_id).await?;
            let after = billing::apply_subscription(&st.pg, org_id, &state).await?;
            audit::record_as(
                st,
                Actor::system("Stripe"),
                Entry::new("billing.subscription_changed")
                    .org(org_id)
                    .target("subscription", org_id, &after.status)
                    .before(
                        before
                            .as_ref()
                            .map(subscription_json)
                            .unwrap_or(Value::Null),
                    )
                    .after(subscription_json(&after)),
            )
            .await;
            Ok(())
        }
        "invoice.payment_failed" => {
            let Some(org_id) = org_of(st, object).await? else {
                return Ok(());
            };
            billing::mark_payment_failed(&st.pg, org_id).await?;
            audit::record_as(
                st,
                Actor::system("Stripe"),
                Entry::new("billing.payment_failed").org(org_id).target(
                    "subscription",
                    org_id,
                    "payment failed",
                ),
            )
            .await;
            notify_payment_failed(st, org_id).await;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The organisation an object belongs to: its own metadata first, then the
/// customer it names.
async fn org_of(st: &AppState, object: &Value) -> Result<Option<Uuid>> {
    let from_metadata = object
        .pointer("/metadata/org_id")
        .or_else(|| object.pointer("/subscription_details/metadata/org_id"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<Uuid>().ok());
    if let Some(org) = from_metadata {
        if orgs::get(&st.pg, org).await.is_ok() {
            return Ok(Some(org));
        }
    }
    match object.get("customer").and_then(Value::as_str) {
        Some(customer) => Ok(billing::find_by_customer(&st.pg, customer)
            .await?
            .map(|b| b.org_id)),
        None => Ok(None),
    }
}

/// A Stripe subscription object, read into what this install keeps.
///
/// The period end moved from the subscription to its items in newer API
/// versions, so both places are read.
fn subscription_state(cfg: &micaforge_core::Config, object: &Value) -> SubscriptionState {
    let price = object
        .pointer("/items/data/0/price/id")
        .and_then(Value::as_str);
    let period_end = object
        .get("current_period_end")
        .or_else(|| object.pointer("/items/data/0/current_period_end"))
        .and_then(Value::as_i64)
        .and_then(|s| DateTime::from_timestamp(s, 0));
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .filter(|s| {
            [
                "incomplete",
                "incomplete_expired",
                "trialing",
                "active",
                "past_due",
                "canceled",
                "unpaid",
                "paused",
            ]
            .contains(s)
        })
        .unwrap_or("incomplete");
    SubscriptionState {
        plan_id: price
            .and_then(|p| cfg.plan_for_price(p))
            .map(|p| p.id.clone()),
        customer_id: object
            .get("customer")
            .and_then(Value::as_str)
            .map(str::to_string),
        subscription_id: object.get("id").and_then(Value::as_str).map(str::to_string),
        status: status.to_string(),
        current_period_end: period_end,
        cancel_at_period_end: object
            .get("cancel_at_period_end")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Mail the owner about a failed payment. A mail failure is logged; the
/// webhook still succeeds, because the state change it records has happened.
async fn notify_payment_failed(st: &AppState, org_id: Uuid) {
    let Ok(org) = orgs::get(&st.pg, org_id).await else {
        return;
    };
    let Ok(owner) = micaforge_store::models::users::get(&st.pg, org.owner_id).await else {
        return;
    };
    let plan = billing::get(&st.pg, org_id)
        .await
        .ok()
        .flatten()
        .and_then(|b| b.plan_id)
        .and_then(|id| st.cfg.plan(&id).map(|p| p.name.clone()))
        .unwrap_or_else(|| "current".to_string());
    let notice = micaforge_mail::templates::PaymentFailed {
        org: org.name,
        plan,
        link: format!("{}/app/settings/billing", st.cfg.dashboard_url),
    };
    if let Err(e) = st.mailer.send_payment_failed(&owner.email, &notice).await {
        tracing::warn!(org = %org_id, error = %e, "the payment-failed email could not be sent");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Computed outside this code, with
    /// `printf '%s' '1700000000.{body}' | openssl dgst -sha256 -hmac 'whsec_micaforge_test_secret'`.
    const VECTOR_SECRET: &str = "whsec_micaforge_test_secret";
    const VECTOR_BODY: &str = r#"{"id":"evt_test_1","type":"invoice.payment_failed"}"#;
    const VECTOR_V1: &str = "7488a779035c23c0b81c3e44d51d150c8941bf48f746951a0fcbed6006045f80";

    #[test]
    fn a_signature_from_the_reference_tool_verifies() {
        let header = format!("t=1700000000,v1={VECTOR_V1}");
        assert!(verify_stripe_signature(
            VECTOR_SECRET,
            &header,
            VECTOR_BODY.as_bytes(),
            1_700_000_100
        ));
        // Stripe sends a v0 and several v1 entries during a secret roll; any
        // matching v1 is enough.
        let rolled = format!("t=1700000000,v0=deadbeef,v1=0000,v1={VECTOR_V1}");
        assert!(verify_stripe_signature(
            VECTOR_SECRET,
            &rolled,
            VECTOR_BODY.as_bytes(),
            1_700_000_000
        ));
    }

    #[test]
    fn a_tampered_stale_or_malformed_signature_fails() {
        let header = format!("t=1700000000,v1={VECTOR_V1}");
        let body = VECTOR_BODY.as_bytes();
        // Another body.
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            &header,
            b"{}",
            1_700_000_000
        ));
        // Another secret.
        assert!(!verify_stripe_signature(
            "whsec_other",
            &header,
            body,
            1_700_000_000
        ));
        // Too old, and too far in the future.
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            &header,
            body,
            1_700_000_301
        ));
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            &header,
            body,
            1_699_999_699
        ));
        // A different `t` than the one signed.
        let moved = format!("t=1700000001,v1={VECTOR_V1}");
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            &moved,
            body,
            1_700_000_001
        ));
        // Only the v0 scheme, or nothing usable at all.
        let v0 = format!("t=1700000000,v0={VECTOR_V1}");
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            &v0,
            body,
            1_700_000_000
        ));
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            "",
            body,
            1_700_000_000
        ));
        assert!(!verify_stripe_signature(
            VECTOR_SECRET,
            &format!("v1={VECTOR_V1}"),
            body,
            1_700_000_000
        ));
    }

    fn plan(price: Option<&str>) -> BillingPlan {
        BillingPlan {
            id: "team".into(),
            name: "Team".into(),
            events: Some(1_000_000),
            sites: Some(20),
            members: Some(25),
            price_id: price.map(str::to_string),
            price_label: Some("$19 a month".into()),
        }
    }

    #[test]
    fn checkout_carries_the_organisation_three_ways_and_returns_to_billing() {
        let org = Uuid::from_u128(42);
        let form = checkout_form(
            "https://app.example.com",
            org,
            &plan(Some("price_1")),
            None,
            "o@example.com",
        );
        let get = |k: &str| {
            form.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("mode"), Some("subscription"));
        assert_eq!(get("line_items[0][price]"), Some("price_1"));
        assert_eq!(get("client_reference_id"), Some(org.to_string().as_str()));
        assert_eq!(get("metadata[org_id]"), Some(org.to_string().as_str()));
        assert_eq!(
            get("subscription_data[metadata][org_id]"),
            Some(org.to_string().as_str())
        );
        assert_eq!(get("customer_email"), Some("o@example.com"));
        assert!(get("success_url")
            .unwrap()
            .starts_with("https://app.example.com/app/settings/billing?billing=success"));

        // A known customer is reused rather than a second one created.
        let again = checkout_form(
            "https://app.example.com",
            org,
            &plan(Some("price_1")),
            Some("cus_1"),
            "o@example.com",
        );
        assert!(again.iter().any(|(k, v)| k == "customer" && v == "cus_1"));
        assert!(!again.iter().any(|(k, _)| k == "customer_email"));
    }

    #[test]
    fn a_subscription_object_maps_to_its_plan_and_period() {
        let mut cfg_plans = vec![plan(Some("price_team"))];
        cfg_plans.push(BillingPlan {
            id: "free".into(),
            name: "Free".into(),
            events: Some(10_000),
            sites: Some(2),
            members: Some(3),
            price_id: None,
            price_label: None,
        });
        let cfg = micaforge_core::Config {
            billing_enabled: true,
            billing_plans: cfg_plans,
            ..crate::routes::tests::cfg()
        };
        let state = subscription_state(
            &cfg,
            &json!({
                "id": "sub_1",
                "customer": "cus_1",
                "status": "active",
                "cancel_at_period_end": true,
                "items": { "data": [{ "price": { "id": "price_team" }, "current_period_end": 1_800_000_000 }] }
            }),
        );
        assert_eq!(state.plan_id.as_deref(), Some("team"));
        assert_eq!(state.status, "active");
        assert!(state.cancel_at_period_end);
        assert_eq!(state.current_period_end.unwrap().timestamp(), 1_800_000_000);

        // An unknown price is no plan, and an unknown status is not trusted.
        let odd = subscription_state(
            &cfg,
            &json!({ "status": "glorious", "items": { "data": [{ "price": { "id": "price_x" } }] } }),
        );
        assert!(odd.plan_id.is_none());
        assert_eq!(odd.status, "incomplete");
    }
}
