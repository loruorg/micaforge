//! Revenue from Stripe and Paddle: `/api/revenue/*`.
//!
//! Micaforge never calls Stripe or Paddle. A site's operator points the
//! provider's webhooks at this server, and each payment or refund the provider
//! reports becomes one row in ClickHouse (`revenue`), attributed, when it can
//! be, to the visit that led to it.
//!
//! ## Trust comes from the signature
//!
//! The webhook routes take no account and no key: the provider cannot hold
//! one. Each delivery is verified against the integration's signing secret,
//! sealed at rest, exactly as the provider documents:
//!
//! - **Stripe**: `Stripe-Signature: t=<unix>,v1=<hex>[,v1=<hex>]`, where `v1`
//!   is HMAC-SHA256 keyed with the secret over `"<t>.<raw body>"`.
//! - **Paddle Billing**: `Paddle-Signature: ts=<unix>;h1=<hex>[;h1=<hex>]`,
//!   HMAC-SHA256 over `"<ts>:<raw body>"`.
//!
//! Both are refused more than [`SIGNATURE_TOLERANCE_SECS`] from the server's
//! clock, both accept any one of several signatures (a provider sends two
//! while a secret is being rotated), and both compare in constant time.
//!
//! ## Exactly once, in practice
//!
//! The provider retries a delivery until it gets a `2xx`, so the same event
//! can arrive several times. Its id is claimed in Postgres before any row is
//! written; a repeat finds the claim and records nothing. If the ClickHouse
//! write fails, the claim is handed back and the answer is a `5xx`, so the
//! provider's next retry is processed rather than lost.
//!
//! ## Attribution without a cookie
//!
//! A page asks this server for a checkout reference (`GET
//! /api/revenue/ref?s=<site>`). The reference is the visitor's current
//! session id and visitor hash, sealed so nobody but this server can read it,
//! and the page passes it to the checkout. When the payment arrives, the
//! reference names the session, and the session names the channel, the
//! referrer, the campaign, the country, the device and the first page. Without
//! one, a customer email that matches an id sent with `identify()` is used;
//! failing that, the payment is counted as unattributed. A raw email is never
//! stored: a customer without a provider id is a keyed hash.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use hmac::{Hmac, Mac};
use micaforge_core::{crypto, new_id, Error, Result};
use micaforge_query::exec::fetch_all;
use micaforge_query::sql::{bucket_millis, fmt_ts, Param};
use micaforge_store::ch::{country_code, insert_rows, RevenueRow};
use micaforge_store::models::revenue::{self, Integration};
use micaforge_store::models::sites::{self, Site};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr, Envelope, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, ADMIN, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// How far a signed timestamp may be from the server's clock. Five minutes,
/// Stripe's own default tolerance.
pub const SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// Key-derivation purpose for sealing signing secrets.
const SECRET_PURPOSE: &str = "revenue";

/// Key-derivation purpose for sealing checkout references.
const REF_PURPOSE: &str = "revenue-ref";

/// Associated data every checkout reference is sealed with.
const REF_AAD: &[u8] = b"micaforge/revenue-ref/v1";

/// What every checkout reference starts with.
pub const REF_PREFIX: &str = "mfr_";

/// The layout version inside a reference.
const REF_VERSION: u8 = 1;

/// Key-derivation purpose for the hash of a customer email.
const EMAIL_PURPOSE: &str = "revenue-customer";

/// How far back an email is matched against `identify()` ids.
const EMAIL_LOOKBACK_DAYS: i64 = 90;

/// Longest signing secret accepted. Both providers' are well under a hundred.
const MAX_SECRET: usize = 256;

/// Every revenue route that answers the dashboard or a provider.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/revenue/integrations",
            get(list_integrations).post(create_integration),
        )
        .route(
            "/api/revenue/integrations/{id}",
            axum::routing::patch(update_integration).delete(delete_integration),
        )
        .route("/api/revenue/webhooks/stripe/{id}", post(stripe_webhook))
        .route("/api/revenue/webhooks/paddle/{id}", post(paddle_webhook))
        .route("/api/revenue/overview", get(overview))
        .route("/api/revenue/timeseries", get(timeseries))
        .route("/api/revenue/breakdown", get(breakdown))
        .route("/api/revenue/customers", get(customers))
        .route("/api/revenue/payments", get(payments))
}

/// The checkout-reference route. Merged with the collection group: a
/// visitor's browser on the customer's own site calls it, so it is open to
/// every origin and writes its own CORS header.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/api/revenue/ref", get(checkout_ref))
}

// ---------------------------------------------------------------------------
// Signatures
// ---------------------------------------------------------------------------

/// Why a delivery's signature was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    /// No usable header.
    Missing,
    /// The timestamp is too far from now.
    Stale,
    /// No signature in the header matches.
    Mismatch,
}

/// Verify a `Stripe-Signature` header over `body` at `now`.
pub fn verify_stripe(
    secret: &str,
    header: &str,
    body: &[u8],
    now: i64,
) -> std::result::Result<(), SignatureError> {
    verify(secret, header, body, now, ',', "t", "v1", b'.')
}

/// Verify a `Paddle-Signature` header over `body` at `now`.
pub fn verify_paddle(
    secret: &str,
    header: &str,
    body: &[u8],
    now: i64,
) -> std::result::Result<(), SignatureError> {
    verify(secret, header, body, now, ';', "ts", "h1", b':')
}

/// The scheme both providers share: a timestamp, one or more hex HMACs, and a
/// signed payload of the timestamp, a separator and the body.
#[allow(clippy::too_many_arguments)]
fn verify(
    secret: &str,
    header: &str,
    body: &[u8],
    now: i64,
    pair_sep: char,
    time_key: &str,
    sig_key: &str,
    joiner: u8,
) -> std::result::Result<(), SignatureError> {
    let mut stamp: Option<(&str, i64)> = None;
    let mut candidates: Vec<Vec<u8>> = Vec::new();
    for part in header.split(pair_sep) {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name == time_key {
            stamp = value.parse::<i64>().ok().map(|t| (value, t));
        } else if name == sig_key {
            if let Ok(bytes) = hex::decode(value) {
                candidates.push(bytes);
            }
        }
    }
    let Some((raw_time, time)) = stamp else {
        return Err(SignatureError::Missing);
    };
    if candidates.is_empty() {
        return Err(SignatureError::Missing);
    }
    if (now - time).abs() > SIGNATURE_TOLERANCE_SECS {
        return Err(SignatureError::Stale);
    }

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes())
        .map_err(|_| SignatureError::Mismatch)?;
    mac.update(raw_time.as_bytes());
    mac.update(&[joiner]);
    mac.update(body);
    let expected = mac.finalize().into_bytes();

    let matched = candidates
        .iter()
        .any(|c| c.len() == expected.len() && bool::from(expected.as_slice().ct_eq(c)));
    if matched {
        Ok(())
    } else {
        Err(SignatureError::Mismatch)
    }
}

// ---------------------------------------------------------------------------
// Checkout references
// ---------------------------------------------------------------------------

/// What a checkout reference names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutRef {
    /// The site it was minted for.
    pub site_id: u16,
    /// When, in unix seconds.
    pub issued: u32,
    /// The visitor hash, sixteen hex characters.
    pub visitor: String,
    /// The session id at that moment; empty when the visitor had none open.
    pub session: String,
}

/// Seal a reference.
///
/// The plaintext is a few bytes (`version, site, issued, visitor, session`)
/// rather than JSON so the whole token stays well under Stripe's 200-character
/// limit on `client_reference_id`.
pub fn mint_ref(key: &[u8; 32], r: &CheckoutRef) -> Result<String> {
    let visitor = hex::decode(&r.visitor)
        .ok()
        .filter(|v| v.len() == 8)
        .ok_or_else(|| Error::internal("a visitor hash is sixteen hex characters"))?;
    let mut plain = Vec::with_capacity(15 + r.session.len());
    plain.push(REF_VERSION);
    plain.extend_from_slice(&r.site_id.to_be_bytes());
    plain.extend_from_slice(&r.issued.to_be_bytes());
    plain.extend_from_slice(&visitor);
    plain.extend_from_slice(r.session.as_bytes());
    let sealed = crypto::seal(key, REF_AAD, &plain)?;
    Ok(format!("{REF_PREFIX}{}", URL_SAFE_NO_PAD.encode(sealed)))
}

/// Open a reference. `None` for anything this server did not seal.
pub fn read_ref(key: &[u8; 32], token: &str) -> Option<CheckoutRef> {
    let sealed = URL_SAFE_NO_PAD
        .decode(token.trim().strip_prefix(REF_PREFIX)?)
        .ok()?;
    let plain = crypto::open(key, REF_AAD, &sealed).ok()?;
    if plain.len() < 15 || plain[0] != REF_VERSION {
        return None;
    }
    Some(CheckoutRef {
        site_id: u16::from_be_bytes([plain[1], plain[2]]),
        issued: u32::from_be_bytes([plain[3], plain[4], plain[5], plain[6]]),
        visitor: hex::encode(&plain[7..15]),
        session: String::from_utf8(plain[15..].to_vec()).ok()?,
    })
}

/// `?s=`
#[derive(Debug, Deserialize)]
struct RefQuery {
    #[serde(default)]
    s: Option<String>,
}

/// `GET /api/revenue/ref?s=<site id>` — mint a checkout reference for the
/// visitor making this request.
///
/// The visitor hash is computed exactly as ingest computes it (the site's
/// salt for today, the address, the user agent), and the session is the one
/// ingest has open for that visitor. Nothing is stored and no cookie is set:
/// the reference exists only in the page that asked for it and in the
/// checkout the page hands it to.
async fn checkout_ref(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
    Query(q): Query<RefQuery>,
) -> Response {
    let answer = async {
        let site_id =
            q.s.as_deref()
                .and_then(|s| s.trim().parse::<u16>().ok())
                .filter(|s| *s > 0)
                .ok_or_else(|| Error::validation("s", "must be a numeric site id"))?;
        let site = sites::get_by_site_id(&st.pg, site_id).await?;
        let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
        let today = Utc::now().with_timezone(&tz).date_naive();
        let salt = st.kv.daily_salt_for(site_id, today).await?;
        let user_agent = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let visitor = micaforge_enrich::visitor_id(&salt, site_id, &ip.to_string(), user_agent);
        let session = st
            .kv
            .session_for(site_id, &visitor)
            .await?
            .unwrap_or_default();
        mint_ref(
            &st.cfg.derive_key(REF_PURPOSE),
            &CheckoutRef {
                site_id,
                issued: u32::try_from(Utc::now().timestamp()).unwrap_or(u32::MAX),
                visitor,
                session,
            },
        )
    }
    .await;

    let mut response = match answer {
        Ok(token) => ok(json!({ "ref": token })).into_response(),
        Err(e) => ApiError(e).into_response(),
    };
    let h = response.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

// ---------------------------------------------------------------------------
// Reading provider events
// ---------------------------------------------------------------------------

/// What one provider event says happened, before attribution.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    /// `sale` | `subscription` | `refund`.
    pub kind: &'static str,
    /// Minor units. For a refund, see [`Draft::refund_total`].
    pub amount: i64,
    /// ISO 4217, upper case.
    pub currency: String,
    /// The provider's customer id.
    pub customer_id: Option<String>,
    /// The customer's email, used for matching and then dropped.
    pub email: Option<String>,
    /// The provider object the row is about.
    pub transaction_id: String,
    /// When the provider says it happened.
    pub at: DateTime<Utc>,
    /// A checkout reference carried by the event.
    pub checkout_ref: Option<String>,
    /// For a refund: the object whose cumulative refunded total this event
    /// reports, and that total. The row's amount is the part not yet recorded.
    pub refund_total: Option<(String, i64)>,
}

/// What a Stripe or Paddle event asks of the handler.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The provider's event id.
    pub event_id: String,
    /// Rows to record (zero or one here).
    pub drafts: Vec<Draft>,
    /// A checkout reference to remember for a customer, with no row of its
    /// own: a subscription checkout, whose payments arrive as invoices.
    pub remember: Option<(String, String)>,
}

/// The trimmed, non-empty string at `path`.
fn text_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(v, |node, key| node.get(*key))?
        .as_str()
        .map(str::trim)
        .filter(|x| !x.is_empty())
}

/// The integer at `path`, written as a number or as a string of digits.
fn int_at(v: &Value, path: &[&str]) -> Option<i64> {
    let node = path.iter().try_fold(v, |node, key| node.get(*key))?;
    // Paddle sends amounts as strings of minor units; Stripe as integers.
    node.as_i64()
        .or_else(|| node.as_str().and_then(|t| t.trim().parse::<i64>().ok()))
}

/// The ISO currency, upper case, or `None` for anything that is not three
/// letters.
fn currency(raw: Option<&str>) -> Option<String> {
    raw.filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase)
}

/// Read a Stripe event.
///
/// - `checkout.session.completed`, mode `payment`, `payment_status` `paid`:
///   a sale of `amount_total`.
/// - `checkout.session.completed`, mode `subscription`: no row; the
///   checkout reference is remembered for the customer, because each payment
///   arrives as an `invoice.paid`.
/// - `invoice.paid` for an invoice that belongs to a subscription: a
///   subscription payment of `amount_paid`. A one-off invoice is not counted,
///   because a one-off Checkout payment already was.
/// - `charge.refunded`: the charge's cumulative `amount_refunded`.
///
/// Anything else reads as nothing, which the handler answers with a `200`:
/// an endpoint subscribed to more events than this needs is not an error.
pub fn read_stripe(event: &Value) -> Result<Reading> {
    let event_id = text_at(event, &["id"])
        .ok_or_else(|| Error::BadRequest("the event has no id".into()))?
        .to_string();
    let kind = text_at(event, &["type"]).unwrap_or_default();
    let at = event
        .get("created")
        .and_then(Value::as_i64)
        .and_then(|t| Utc.timestamp_opt(t, 0).single())
        .unwrap_or_else(Utc::now);
    let obj = event
        .get("data")
        .and_then(|d| d.get("object"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut reading = Reading {
        event_id,
        drafts: Vec::new(),
        remember: None,
    };
    let reference = |paths: &[&[&str]]| {
        paths
            .iter()
            .find_map(|p| text_at(&obj, p))
            .filter(|r| r.starts_with(REF_PREFIX))
            .map(str::to_string)
    };

    match kind {
        "checkout.session.completed" => {
            let checkout_ref = reference(&[
                &["client_reference_id"],
                &["metadata", "mf_ref"],
                &["subscription_data", "metadata", "mf_ref"],
            ]);
            let customer_id = text_at(&obj, &["customer"]).map(str::to_string);
            match (text_at(&obj, &["mode"]), text_at(&obj, &["payment_status"])) {
                (Some("payment"), Some("paid")) => {
                    if let (Some(amount), Some(currency)) = (
                        int_at(&obj, &["amount_total"]),
                        currency(text_at(&obj, &["currency"])),
                    ) {
                        reading.drafts.push(Draft {
                            kind: "sale",
                            amount,
                            currency,
                            customer_id,
                            email: text_at(&obj, &["customer_details", "email"])
                                .map(str::to_string),
                            transaction_id: text_at(&obj, &["id"]).unwrap_or_default().to_string(),
                            at,
                            checkout_ref,
                            refund_total: None,
                        });
                    }
                }
                (Some("subscription"), _) => {
                    if let (Some(customer), Some(r)) = (customer_id, checkout_ref) {
                        reading.remember = Some((customer, r));
                    }
                }
                _ => {}
            }
        }
        "invoice.paid" => {
            let subscription = text_at(&obj, &["subscription"])
                .or_else(|| text_at(&obj, &["parent", "subscription_details", "subscription"]))
                .is_some()
                || text_at(&obj, &["billing_reason"])
                    .is_some_and(|r| r.starts_with("subscription"));
            let amount = int_at(&obj, &["amount_paid"]).unwrap_or(0);
            if subscription && amount > 0 {
                if let Some(currency) = currency(text_at(&obj, &["currency"])) {
                    reading.drafts.push(Draft {
                        kind: "subscription",
                        amount,
                        currency,
                        customer_id: text_at(&obj, &["customer"]).map(str::to_string),
                        email: text_at(&obj, &["customer_email"]).map(str::to_string),
                        transaction_id: text_at(&obj, &["id"]).unwrap_or_default().to_string(),
                        at,
                        checkout_ref: reference(&[
                            &["subscription_details", "metadata", "mf_ref"],
                            &["parent", "subscription_details", "metadata", "mf_ref"],
                            &["metadata", "mf_ref"],
                        ]),
                        refund_total: None,
                    });
                }
            }
        }
        "charge.refunded" => {
            let charge = text_at(&obj, &["id"]).unwrap_or_default().to_string();
            if let (Some(total), Some(currency)) = (
                int_at(&obj, &["amount_refunded"]),
                currency(text_at(&obj, &["currency"])),
            ) {
                reading.drafts.push(Draft {
                    kind: "refund",
                    amount: 0,
                    currency,
                    customer_id: text_at(&obj, &["customer"]).map(str::to_string),
                    email: text_at(&obj, &["billing_details", "email"]).map(str::to_string),
                    transaction_id: charge.clone(),
                    at,
                    checkout_ref: reference(&[&["metadata", "mf_ref"]]),
                    refund_total: Some((charge, total)),
                });
            }
        }
        _ => {}
    }
    Ok(reading)
}

/// Read a Paddle Billing event.
///
/// - `transaction.completed`: `details.totals.total`, a subscription payment
///   when the transaction has a `subscription_id`, a sale otherwise.
/// - `adjustment.created` / `adjustment.updated` with action `refund` or
///   `chargeback` and status `approved`: the adjustment's total, once per
///   adjustment (an adjustment is created pending and approved later).
pub fn read_paddle(event: &Value) -> Result<Reading> {
    let event_id = text_at(event, &["event_id"])
        .ok_or_else(|| Error::BadRequest("the event has no event_id".into()))?
        .to_string();
    let kind = text_at(event, &["event_type"]).unwrap_or_default();
    let at = text_at(event, &["occurred_at"])
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    let mut reading = Reading {
        event_id,
        drafts: Vec::new(),
        remember: None,
    };

    match kind {
        "transaction.completed" => {
            if let (Some(amount), Some(currency)) = (
                int_at(&data, &["details", "totals", "total"]),
                currency(text_at(&data, &["currency_code"])),
            ) {
                reading.drafts.push(Draft {
                    kind: if text_at(&data, &["subscription_id"]).is_some() {
                        "subscription"
                    } else {
                        "sale"
                    },
                    amount,
                    currency,
                    customer_id: text_at(&data, &["customer_id"]).map(str::to_string),
                    email: None,
                    transaction_id: text_at(&data, &["id"]).unwrap_or_default().to_string(),
                    at,
                    checkout_ref: text_at(&data, &["custom_data", "mf_ref"])
                        .filter(|r| r.starts_with(REF_PREFIX))
                        .map(str::to_string),
                    refund_total: None,
                });
            }
        }
        "adjustment.created" | "adjustment.updated" => {
            let counts = matches!(text_at(&data, &["action"]), Some("refund" | "chargeback"))
                && text_at(&data, &["status"]) == Some("approved");
            if counts {
                let adjustment = text_at(&data, &["id"]).unwrap_or_default().to_string();
                if let (Some(total), Some(currency)) = (
                    int_at(&data, &["totals", "total"]),
                    currency(text_at(&data, &["currency_code"])),
                ) {
                    reading.drafts.push(Draft {
                        kind: "refund",
                        amount: 0,
                        currency,
                        customer_id: text_at(&data, &["customer_id"]).map(str::to_string),
                        email: None,
                        transaction_id: text_at(&data, &["transaction_id"])
                            .unwrap_or(&adjustment)
                            .to_string(),
                        at,
                        checkout_ref: None,
                        refund_total: Some((adjustment, total)),
                    });
                }
            }
        }
        _ => {}
    }
    Ok(reading)
}

// ---------------------------------------------------------------------------
// The webhooks
// ---------------------------------------------------------------------------

/// `POST /api/revenue/webhooks/stripe/{id}`
async fn stripe_webhook(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    webhook(&st, id, "stripe", &headers, &body).await
}

/// `POST /api/revenue/webhooks/paddle/{id}`
async fn paddle_webhook(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    webhook(&st, id, "paddle", &headers, &body).await
}

/// A plain JSON answer for a provider.
fn answer(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// Verify, claim, read, attribute and record one delivery.
async fn webhook(
    st: &AppState,
    id: Uuid,
    provider: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let integration = match revenue::get(&st.pg, id).await {
        Ok(i) if i.provider == provider => i,
        Ok(_) | Err(Error::NotFound(_)) => {
            return ApiError(Error::not_found("integration")).into_response()
        }
        Err(e) => return ApiError(e).into_response(),
    };
    if !integration.enabled {
        // Paused on purpose: accepted, so the provider stops retrying, and
        // recorded nowhere.
        return answer(
            StatusCode::OK,
            json!({ "received": true, "recorded": 0, "paused": true }),
        );
    }
    let secret = match open_secret(st, &integration) {
        Ok(Some(secret)) => secret,
        // No secret yet: a 503 makes the provider retry, and the retries that
        // arrive after the operator pastes the secret are recorded.
        Ok(None) => {
            return answer(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": { "code": "not_ready", "message": "this integration has no signing secret yet" } }),
            )
        }
        Err(e) => return ApiError(e).into_response(),
    };

    let header_name = if provider == "stripe" {
        "stripe-signature"
    } else {
        "paddle-signature"
    };
    let signature = headers
        .get(header_name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let now = Utc::now().timestamp();
    let verified = if provider == "stripe" {
        verify_stripe(&secret, signature, body, now)
    } else {
        verify_paddle(&secret, signature, body, now)
    };
    if let Err(reason) = verified {
        tracing::warn!(integration = %integration.id, provider, ?reason, "revenue webhook refused");
        return ApiError(Error::BadRequest(
            "the signature did not verify with this integration's signing secret".into(),
        ))
        .into_response();
    }

    let event: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return ApiError(Error::BadRequest("the body is not JSON".into())).into_response()
        }
    };
    let reading = match if provider == "stripe" {
        read_stripe(&event)
    } else {
        read_paddle(&event)
    } {
        Ok(r) => r,
        Err(e) => return ApiError(e).into_response(),
    };

    match record(st, &integration, provider, &reading).await {
        Ok(Some(n)) => answer(StatusCode::OK, json!({ "received": true, "recorded": n })),
        Ok(None) => answer(
            StatusCode::OK,
            json!({ "received": true, "duplicate": true }),
        ),
        Err(e) => {
            tracing::error!(integration = %integration.id, provider, error = %e, "revenue event not recorded");
            ApiError(e).into_response()
        }
    }
}

/// The integration's signing secret, opened.
fn open_secret(st: &AppState, integration: &Integration) -> Result<Option<String>> {
    let Some(sealed) = integration.secret_sealed.as_deref() else {
        return Ok(None);
    };
    let plain = crypto::open(
        &st.cfg.derive_key(SECRET_PURPOSE),
        integration.id.as_bytes(),
        sealed,
    )?;
    String::from_utf8(plain)
        .map(Some)
        .map_err(|_| Error::internal("the stored signing secret is not text"))
}

/// Claim the event and write its rows. `None` when it was already processed.
async fn record(
    st: &AppState,
    integration: &Integration,
    provider: &str,
    reading: &Reading,
) -> Result<Option<usize>> {
    if !revenue::claim_event(&st.pg, integration.id, &reading.event_id).await? {
        return Ok(None);
    }
    let outcome = write_rows(st, integration, provider, reading).await;
    match outcome {
        Ok(n) => {
            let _ = revenue::touch(&st.pg, integration.id).await;
            if let Err(e) = revenue::prune_events(&st.pg, integration.id).await {
                tracing::warn!(error = %e, "could not prune processed revenue events");
            }
            Ok(Some(n))
        }
        Err(e) => {
            // Hand the claim back so the provider's retry is processed.
            if let Err(release) =
                revenue::release_event(&st.pg, integration.id, &reading.event_id).await
            {
                tracing::error!(error = %release, "could not release a revenue event claim");
            }
            Err(e)
        }
    }
}

/// Attribute and insert the rows of one reading.
async fn write_rows(
    st: &AppState,
    integration: &Integration,
    provider: &str,
    reading: &Reading,
) -> Result<usize> {
    let site = sites::get(&st.pg, integration.site_id).await?;
    let ref_key = st.cfg.derive_key(REF_PURPOSE);

    if let Some((customer, reference)) = &reading.remember {
        revenue::remember_customer_ref(&st.pg, integration.id, customer, reference).await?;
    }

    let mut rows = Vec::new();
    let mut advanced: Vec<(String, i64, i64)> = Vec::new();
    for draft in &reading.drafts {
        let amount = match &draft.refund_total {
            Some((object, total)) => {
                let (before, after) =
                    revenue::advance_refund_total(&st.pg, integration.id, object, *total).await?;
                if after == before {
                    continue;
                }
                advanced.push((object.clone(), before, after));
                -(after - before)
            }
            None => draft.amount,
        };

        if let (Some(customer), Some(reference)) = (&draft.customer_id, &draft.checkout_ref) {
            revenue::remember_customer_ref(&st.pg, integration.id, customer, reference).await?;
        }
        let reference = match (&draft.checkout_ref, &draft.customer_id) {
            (Some(r), _) => Some(r.clone()),
            (None, Some(customer)) => {
                revenue::customer_ref(&st.pg, integration.id, customer).await?
            }
            (None, None) => None,
        };
        let visit = attribute(
            st,
            &site,
            &ref_key,
            reference.as_deref(),
            draft.email.as_deref(),
        )
        .await?;

        rows.push(RevenueRow {
            site_id: site.site_id_u16(),
            timestamp: draft.at,
            provider: provider.to_string(),
            event_id: reading.event_id.clone(),
            transaction_id: draft.transaction_id.clone(),
            kind: draft.kind.to_string(),
            amount,
            currency: draft.currency.clone(),
            customer: customer_label(st, draft),
            session_id: visit.session_id,
            visitor_id: visit.visitor_id,
            attribution: visit.method.to_string(),
            channel: visit.channel,
            source: visit.source,
            referrer_host: visit.referrer_host,
            utm_source: visit.utm_source,
            utm_medium: visit.utm_medium,
            utm_campaign: visit.utm_campaign,
            country: country_code(&visit.country),
            device_type: visit.device_type,
            browser: visit.browser,
            entry_path: visit.entry_path,
        });
    }

    if let Err(e) = insert_rows(&st.ch, "revenue", &rows).await {
        for (object, before, after) in advanced {
            let _ =
                revenue::rewind_refund_total(&st.pg, integration.id, &object, before, after).await;
        }
        return Err(e);
    }
    Ok(rows.len())
}

/// The customer as stored: the provider's id, or a keyed hash of the email,
/// never the email itself.
fn customer_label(st: &AppState, draft: &Draft) -> String {
    if let Some(id) = &draft.customer_id {
        return id.clone();
    }
    match draft
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        Some(email) => {
            let digest = crypto::keyed_digest(
                &st.cfg.derive_key(EMAIL_PURPOSE),
                &[&email.to_ascii_lowercase()],
            );
            format!("email:{}", &digest[..16])
        }
        None => String::new(),
    }
}

/// The visit a payment is attributed to.
#[derive(Debug, Clone, Default, clickhouse::Row, Deserialize)]
struct Visit {
    session_id: String,
    visitor_id: String,
    channel: String,
    source: String,
    referrer_host: String,
    utm_source: String,
    utm_medium: String,
    utm_campaign: String,
    country: String,
    device_type: String,
    browser: String,
    entry_path: String,
}

/// A visit and how it was found.
#[derive(Debug, Clone, Default)]
struct Attributed {
    method: &'static str,
    session_id: String,
    visitor_id: String,
    channel: String,
    source: String,
    referrer_host: String,
    utm_source: String,
    utm_medium: String,
    utm_campaign: String,
    country: String,
    device_type: String,
    browser: String,
    entry_path: String,
}

impl Attributed {
    fn from_visit(method: &'static str, v: Visit) -> Attributed {
        Attributed {
            method,
            session_id: v.session_id,
            visitor_id: v.visitor_id,
            channel: v.channel,
            source: v.source,
            referrer_host: v.referrer_host,
            utm_source: v.utm_source,
            utm_medium: v.utm_medium,
            utm_campaign: v.utm_campaign,
            country: v.country,
            device_type: v.device_type,
            browser: v.browser,
            entry_path: v.entry_path,
        }
    }
}

/// The columns a visit is read with, from the session rollup.
const VISIT_COLS: &str = "session_id,
    any(visitor_id) AS visitor_id, any(channel) AS channel, any(source) AS source,
    any(referrer_host) AS referrer_host, any(utm_source) AS utm_source,
    any(utm_medium) AS utm_medium, any(utm_campaign) AS utm_campaign,
    any(country) AS country, any(device_type) AS device_type, any(browser) AS browser,
    any(entry_path) AS entry_path";

/// Find the visit: by reference (its session, else its visitor's latest
/// session), else by email, else none.
async fn attribute(
    st: &AppState,
    site: &Site,
    key: &[u8; 32],
    reference: Option<&str>,
    email: Option<&str>,
) -> Result<Attributed> {
    let site_id = site.site_id_u16();
    if let Some(r) = reference
        .and_then(|r| read_ref(key, r))
        .filter(|r| r.site_id == site_id)
    {
        if !r.session.is_empty() {
            let sql = format!(
                "SELECT {VISIT_COLS} FROM sessions_mv
                 WHERE site_id = ? AND session_id = ? GROUP BY session_id LIMIT 1"
            );
            let found = fetch_all::<Visit>(
                &st.ch,
                "revenue_visit_session",
                &sql,
                &[Param::U16(site_id), Param::Str(r.session.clone())],
            )
            .await?;
            if let Some(v) = found.into_iter().next() {
                return Ok(Attributed::from_visit("session", v));
            }
        }
        let sql = format!(
            "SELECT {VISIT_COLS} FROM sessions_mv
             WHERE site_id = ? AND visitor_id = ?
             GROUP BY session_id ORDER BY min(started) DESC LIMIT 1"
        );
        let found = fetch_all::<Visit>(
            &st.ch,
            "revenue_visit_visitor",
            &sql,
            &[Param::U16(site_id), Param::Str(r.visitor.clone())],
        )
        .await?;
        if let Some(v) = found.into_iter().next() {
            return Ok(Attributed::from_visit("visitor", v));
        }
    }

    if let Some(email) = email.map(str::trim).filter(|e| e.contains('@')) {
        let sql = format!(
            "SELECT {VISIT_COLS} FROM sessions_mv
             WHERE site_id = ? AND lower(identified_id) = ?
               AND started >= toDateTime64(?, 3, 'UTC')
             GROUP BY session_id ORDER BY min(started) ASC LIMIT 1"
        );
        let since = fmt_ts(Utc::now() - chrono::Duration::days(EMAIL_LOOKBACK_DAYS));
        let found = fetch_all::<Visit>(
            &st.ch,
            "revenue_visit_email",
            &sql,
            &[
                Param::U16(site_id),
                Param::Str(email.to_ascii_lowercase()),
                Param::Str(since),
            ],
        )
        .await?;
        if let Some(v) = found.into_iter().next() {
            return Ok(Attributed::from_visit("email", v));
        }
    }

    Ok(Attributed {
        method: "none",
        ..Attributed::default()
    })
}

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

/// One integration on the wire. The secret never is.
fn integration_json(st: &AppState, i: &Integration) -> Value {
    json!({
        "id": i.id,
        "site_id": i.site_id,
        "provider": i.provider,
        "webhook_url": format!(
            "{}/api/revenue/webhooks/{}/{}",
            st.cfg.public_url.trim_end_matches('/'),
            i.provider,
            i.id
        ),
        "has_secret": i.secret_sealed.is_some(),
        "enabled": i.enabled,
        "last_event_at": i.last_event_at,
        "created_at": i.created_at,
    })
}

/// Seal a secret for one integration row.
fn seal_secret(st: &AppState, id: Uuid, secret: &str) -> Result<Vec<u8>> {
    let secret = secret.trim();
    if secret.is_empty() || secret.len() > MAX_SECRET || secret.chars().any(char::is_whitespace) {
        return Err(Error::validation(
            "secret",
            "must be the signing secret exactly as the provider shows it",
        ));
    }
    crypto::seal(
        &st.cfg.derive_key(SECRET_PURPOSE),
        id.as_bytes(),
        secret.as_bytes(),
    )
}

/// `GET /api/revenue/integrations?site=`
async fn list_integrations(
    State(st): State<AppState>,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    let rows = revenue::list_for_site(&st.pg, access.site.id).await?;
    Ok(ok(rows
        .iter()
        .map(|i| integration_json(&st, i))
        .collect::<Vec<_>>()))
}

/// A new integration.
#[derive(Debug, Deserialize)]
struct CreateBody {
    provider: String,
    #[serde(default)]
    secret: Option<String>,
}

/// `POST /api/revenue/integrations?site=`
///
/// The secret is optional, because both providers show it only after they
/// have the webhook URL, and the URL names this row. When the site already has
/// an integration for the provider, this replaces its secret instead.
async fn create_integration(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let provider = body.provider.trim().to_ascii_lowercase();
    if !matches!(provider.as_str(), "stripe" | "paddle") {
        return Err(Error::validation("provider", "must be stripe or paddle").into());
    }
    let secret = body
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let integration = match revenue::find(&st.pg, access.site.id, &provider).await? {
        Some(existing) => match secret {
            Some(secret) => {
                let sealed = seal_secret(&st, existing.id, secret)?;
                revenue::update(&st.pg, existing.id, Some(&sealed), None).await?
            }
            None => existing,
        },
        None => {
            let id = new_id();
            let sealed = secret.map(|s| seal_secret(&st, id, s)).transpose()?;
            revenue::create(&st.pg, id, access.site.id, &provider, sealed.as_deref()).await?
        }
    };
    Ok(ok(integration_json(&st, &integration)))
}

/// Changes to an integration.
#[derive(Debug, Deserialize)]
struct UpdateBody {
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// Load an integration and assert the caller's role on its site.
async fn reachable(st: &AppState, id: Uuid, user: Uuid, needed: &str) -> Result<Integration> {
    let integration = revenue::get(&st.pg, id).await?;
    match authorize_site(&st.pg, integration.site_id, user, needed).await {
        Ok(_) => Ok(integration),
        Err(Error::NotFound(_)) => Err(Error::not_found("integration")),
        Err(e) => Err(e),
    }
}

/// `PATCH /api/revenue/integrations/{id}`
async fn update_integration(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let integration = reachable(&st, id, auth.user.id, ADMIN).await?;
    let sealed = match body
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(secret) => Some(seal_secret(&st, integration.id, secret)?),
        None => None,
    };
    let updated = revenue::update(&st.pg, integration.id, sealed.as_deref(), body.enabled).await?;
    Ok(ok(integration_json(&st, &updated)))
}

/// `DELETE /api/revenue/integrations/{id}`
async fn delete_integration(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let integration = reachable(&st, id, auth.user.id, ADMIN).await?;
    revenue::delete(&st.pg, integration.id).await?;
    Ok(ok(json!({ "deleted": integration.id })))
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// The extra parameters the revenue reports read beside the envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct ReportQuery {
    /// Only this currency, when set.
    #[serde(default)]
    pub(crate) currency: Option<String>,
    /// The breakdown dimension.
    #[serde(default)]
    pub(crate) dimension: Option<String>,
}

/// The `WHERE` every report shares, and its parameters: the site, the window,
/// and the currency when one was asked for.
fn scope(env: &micaforge_core::QueryEnvelope, q: &ReportQuery) -> Result<(String, Vec<Param>)> {
    let mut sql = "site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC') \
                   AND timestamp < toDateTime64(?, 3, 'UTC')"
        .to_string();
    let mut params = vec![
        Param::U16(env.site),
        Param::Str(fmt_ts(env.range.start)),
        Param::Str(fmt_ts(env.range.end)),
    ];
    if let Some(raw) = q
        .currency
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        let code = currency(Some(raw))
            .ok_or_else(|| Error::validation("currency", "must be a three-letter currency code"))?;
        sql.push_str(" AND currency = ?");
        params.push(Param::Str(code));
    }
    Ok((sql, params))
}

/// Totals for one currency.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct Totals {
    currency: String,
    gross: i64,
    refunds: i64,
    net: i64,
    payments: u64,
    refund_count: u64,
    customers: u64,
    attributed: u64,
}

/// `GET /api/revenue/overview`
async fn overview(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    Ok(ok(overview_value(&st, access.site.id, &env, &q).await?))
}

/// Totals per currency, for the route and the MCP tool.
pub(crate) async fn overview_value(
    st: &AppState,
    site: Uuid,
    env: &micaforge_core::QueryEnvelope,
    q: &ReportQuery,
) -> Result<Value> {
    let (scope, params) = scope(env, q)?;
    let sql = format!(
        "SELECT toString(currency) AS currency,
            sumIf(amount, amount > 0) AS gross,
            -sumIf(amount, amount < 0) AS refunds,
            sum(amount) AS net,
            countIf(amount > 0) AS payments,
            countIf(amount < 0) AS refund_count,
            uniqExactIf(customer, customer != '') AS customers,
            countIf(amount > 0 AND attribution != 'none') AS attributed
         FROM revenue WHERE {scope}
         GROUP BY currency ORDER BY gross DESC, currency"
    );
    let rows = fetch_all::<Totals>(&st.ch, "revenue_overview", &sql, &params).await?;
    let integrations = revenue::list_for_site(&st.pg, site).await?.len();
    let currencies: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "currency": r.currency,
                "gross": r.gross,
                "refunds": r.refunds,
                "net": r.net,
                "payments": r.payments,
                "refund_count": r.refund_count,
                "customers": r.customers,
                "attributed": if r.payments > 0 { r.attributed as f64 / r.payments as f64 } else { 0.0 },
            })
        })
        .collect();
    Ok(json!({ "currencies": currencies, "integrations": integrations }))
}

/// One bucket of one currency.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct SeriesRow {
    t: i64,
    currency: String,
    gross: i64,
    refunds: i64,
    net: i64,
}

/// `GET /api/revenue/timeseries`
async fn timeseries(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    let (scope, params) = scope(&env, &q)?;
    let sql = format!(
        "SELECT {t} AS t, toString(currency) AS currency,
            sumIf(amount, amount > 0) AS gross,
            -sumIf(amount, amount < 0) AS refunds,
            sum(amount) AS net
         FROM revenue WHERE {scope}
         GROUP BY t, currency ORDER BY currency, t",
        t = bucket_millis(env.granularity, env.range.tz, "timestamp"),
    );
    let rows = fetch_all::<SeriesRow>(&st.ch, "revenue_timeseries", &sql, &params).await?;
    let starts = crate::routes::links::bucket_starts(
        env.range.start,
        env.range.end - chrono::Duration::milliseconds(1),
        env.granularity,
        env.range.tz,
    );
    let mut currencies: Vec<String> = rows.iter().map(|r| r.currency.clone()).collect();
    currencies.dedup();
    let out: Vec<Value> = currencies
        .iter()
        .map(|c| {
            let points: Vec<Value> = starts
                .iter()
                .map(|t| {
                    let r = rows.iter().find(|r| &r.currency == c && r.t == *t);
                    json!({
                        "t": t,
                        "gross": r.map(|r| r.gross).unwrap_or(0),
                        "refunds": r.map(|r| r.refunds).unwrap_or(0),
                        "net": r.map(|r| r.net).unwrap_or(0),
                    })
                })
                .collect();
            json!({ "currency": c, "points": points })
        })
        .collect();
    Ok(ok(out))
}

/// The breakdown dimensions, and the column each reads.
const DIMENSIONS: &[(&str, &str)] = &[
    ("channel", "toString(channel)"),
    ("source", "toString(source)"),
    ("referrer_host", "referrer_host"),
    ("utm_source", "utm_source"),
    ("utm_medium", "utm_medium"),
    ("utm_campaign", "utm_campaign"),
    ("country", "trimBoth(toString(country))"),
    ("device_type", "toString(device_type)"),
    ("browser", "toString(browser)"),
    ("entry_path", "entry_path"),
    ("attribution", "toString(attribution)"),
];

/// The column for a dimension name, from the fixed list only.
fn dimension_column(name: Option<&str>) -> Result<&'static str> {
    let name = name.map(str::trim).unwrap_or("channel");
    DIMENSIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
        .ok_or_else(|| {
            Error::validation(
                "dimension",
                format!(
                    "must be one of: {}",
                    DIMENSIONS
                        .iter()
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
}

/// One breakdown row.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct BreakdownRow {
    value: String,
    currency: String,
    net: i64,
    payments: u64,
    customers: u64,
}

/// `GET /api/revenue/breakdown?dimension=`
async fn breakdown(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    Ok(ok(breakdown_value(&st, &env, &q).await?))
}

/// Net revenue by one dimension, for the route and the MCP tool.
pub(crate) async fn breakdown_value(
    st: &AppState,
    env: &micaforge_core::QueryEnvelope,
    q: &ReportQuery,
) -> Result<Value> {
    let column = dimension_column(q.dimension.as_deref())?;
    let (scope, mut params) = scope(env, q)?;
    params.push(Param::U64(u64::from(env.limit)));
    let sql = format!(
        "SELECT {column} AS value, toString(currency) AS currency, sum(amount) AS net,
            countIf(amount > 0) AS payments, uniqExactIf(customer, customer != '') AS customers
         FROM revenue WHERE {scope}
         GROUP BY value, currency ORDER BY net DESC, value LIMIT ?"
    );
    let rows = fetch_all::<BreakdownRow>(&st.ch, "revenue_breakdown", &sql, &params).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "value": r.value,
                "currency": r.currency,
                "net": r.net,
                "payments": r.payments,
                "customers": r.customers,
            })
        })
        .collect();
    Ok(Value::Array(out))
}

/// One customer.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct CustomerRow {
    customer: String,
    currency: String,
    net: i64,
    payments: u64,
    refunds: i64,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    first_at: DateTime<Utc>,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    last_at: DateTime<Utc>,
    channel: String,
}

/// `GET /api/revenue/customers`
async fn customers(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    let (scope, mut params) = scope(&env, &q)?;
    params.push(Param::U64(u64::from(env.limit.min(500))));
    let sql = format!(
        "SELECT customer, toString(currency) AS currency, sum(amount) AS net,
            countIf(amount > 0) AS payments, -sumIf(amount, amount < 0) AS refunds,
            min(timestamp) AS first_at, max(timestamp) AS last_at,
            argMin(toString(channel), timestamp) AS channel
         FROM revenue WHERE {scope} AND customer != ''
         GROUP BY customer, currency ORDER BY net DESC, customer LIMIT ?"
    );
    let rows = fetch_all::<CustomerRow>(&st.ch, "revenue_customers", &sql, &params).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "customer": r.customer,
                "currency": r.currency,
                "net": r.net,
                "payments": r.payments,
                "refunds": r.refunds,
                "first_at": r.first_at,
                "last_at": r.last_at,
                "channel": r.channel,
            })
        })
        .collect();
    Ok(ok(out))
}

/// One payment.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct PaymentRow {
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    timestamp: DateTime<Utc>,
    provider: String,
    kind: String,
    amount: i64,
    currency: String,
    customer: String,
    channel: String,
    country: String,
    attribution: String,
}

/// `GET /api/revenue/payments`
async fn payments(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    let (scope, mut params) = scope(&env, &q)?;
    params.push(Param::U64(u64::from(env.limit.min(500))));
    let sql = format!(
        "SELECT timestamp, toString(provider) AS provider, toString(type) AS kind, amount,
            toString(currency) AS currency, customer, toString(channel) AS channel,
            trimBoth(toString(country)) AS country, toString(attribution) AS attribution
         FROM revenue WHERE {scope}
         ORDER BY timestamp DESC LIMIT ?"
    );
    let rows = fetch_all::<PaymentRow>(&st.ch, "revenue_payments", &sql, &params).await?;
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "at": r.timestamp,
                "provider": r.provider,
                "type": r.kind,
                "amount": r.amount,
                "currency": r.currency,
                "customer": r.customer,
                "channel": r.channel,
                "country": r.country,
                "attribution": r.attribution,
            })
        })
        .collect();
    Ok(ok(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sign the way Stripe documents: hex HMAC-SHA256 over `"<t>.<body>"`.
    fn stripe_header(secret: &str, t: i64, body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{t}.").as_bytes());
        mac.update(body);
        format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    /// Sign the way Paddle documents: hex HMAC-SHA256 over `"<ts>:<body>"`.
    fn paddle_header(secret: &str, ts: i64, body: &[u8]) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{ts}:").as_bytes());
        mac.update(body);
        format!("ts={ts};h1={}", hex::encode(mac.finalize().into_bytes()))
    }

    const NOW: i64 = 1_760_000_000;

    #[test]
    fn fixed_vectors_computed_outside_this_code_verify() {
        // Computed with openssl, not with this module, so a mistake shared by
        // the signer and the verifier here cannot hide:
        //   printf '%s' '1760000000.{"id":"evt_1"}' \
        //     | openssl dgst -sha256 -hmac whsec_test_secret
        //   printf '%s' '1760000000:{"event_id":"evt_01"}' \
        //     | openssl dgst -sha256 -hmac pdl_ntfset_test
        let stripe =
            "t=1760000000,v1=8c032c6b5729dcb3cc1f3a2c4ec17af66b2885bdbac65e2108d19a14282d243d";
        assert_eq!(
            verify_stripe("whsec_test_secret", stripe, br#"{"id":"evt_1"}"#, NOW),
            Ok(())
        );
        let paddle =
            "ts=1760000000;h1=0c6fb344b9391b17ed77fb77c7637f2d8995de1b29580019848cf8093472394a";
        assert_eq!(
            verify_paddle("pdl_ntfset_test", paddle, br#"{"event_id":"evt_01"}"#, NOW),
            Ok(())
        );
        // The helpers below sign the same way.
        assert_eq!(
            stripe_header("whsec_test_secret", NOW, br#"{"id":"evt_1"}"#),
            stripe
        );
        assert_eq!(
            paddle_header("pdl_ntfset_test", NOW, br#"{"event_id":"evt_01"}"#),
            paddle
        );
    }

    #[test]
    fn stripe_signatures_check_the_secret_the_body_and_the_clock() {
        let body = br#"{"id":"evt_1","type":"invoice.paid"}"#;
        let good = stripe_header("whsec_abc", NOW, body);
        assert_eq!(verify_stripe("whsec_abc", &good, body, NOW), Ok(()));
        assert_eq!(verify_stripe("whsec_abc", &good, body, NOW + 299), Ok(()));
        assert_eq!(
            verify_stripe("whsec_abc", &good, body, NOW + 301),
            Err(SignatureError::Stale)
        );
        assert_eq!(
            verify_stripe("whsec_other", &good, body, NOW),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_stripe("whsec_abc", &good, br#"{"id":"evt_2"}"#, NOW),
            Err(SignatureError::Mismatch)
        );
        assert_eq!(
            verify_stripe("whsec_abc", "", body, NOW),
            Err(SignatureError::Missing)
        );
        // During rotation Stripe sends one v1 per secret; any match is enough,
        // and an unknown scheme (v0) is ignored.
        let rotated = format!("{},v1={},v0=deadbeef", good, hex::encode([0u8; 32]));
        assert_eq!(verify_stripe("whsec_abc", &rotated, body, NOW), Ok(()));
    }

    #[test]
    fn paddle_signatures_use_a_colon_and_semicolons() {
        let body = br#"{"event_id":"evt_01","event_type":"transaction.completed"}"#;
        let good = paddle_header("pdl_ntfset_secret", NOW, body);
        assert_eq!(verify_paddle("pdl_ntfset_secret", &good, body, NOW), Ok(()));
        // A Stripe-shaped signature over the same bytes must not pass.
        let stripe_shaped = stripe_header("pdl_ntfset_secret", NOW, body);
        assert!(verify_paddle("pdl_ntfset_secret", &stripe_shaped, body, NOW).is_err());
        assert_eq!(
            verify_paddle("pdl_ntfset_secret", &good, body, NOW - 3600),
            Err(SignatureError::Stale)
        );
        let two = format!(
            "ts={NOW};h1={};h1={}",
            hex::encode([1u8; 32]),
            &good[good.find("h1=").unwrap() + 3..]
        );
        assert_eq!(verify_paddle("pdl_ntfset_secret", &two, body, NOW), Ok(()));
    }

    #[test]
    fn a_checkout_reference_round_trips_and_is_sealed() {
        let key = crypto::derive_key("a-secret-key-base-of-adequate-length", REF_PURPOSE);
        let r = CheckoutRef {
            site_id: 7,
            issued: 1_760_000_000,
            visitor: "0123456789abcdef".into(),
            session: "01JABCDEFGHJKMNPQRSTVWXYZ0".into(),
        };
        let token = mint_ref(&key, &r).unwrap();
        assert!(token.starts_with(REF_PREFIX));
        assert!(
            token.len() < 200,
            "Stripe's client_reference_id limit: {}",
            token.len()
        );
        assert!(!token.contains("0123456789abcdef"));
        assert_eq!(read_ref(&key, &token), Some(r));
        let other = crypto::derive_key("a-secret-key-base-of-adequate-length", "other");
        assert_eq!(read_ref(&other, &token), None);
        assert_eq!(read_ref(&key, "mfr_not-a-token"), None);
        assert_eq!(read_ref(&key, "cs_test_123"), None);
    }

    #[test]
    fn stripe_checkout_payment_mode_is_a_sale_with_its_reference() {
        let event = json!({
            "id": "evt_1", "type": "checkout.session.completed", "created": NOW,
            "data": { "object": {
                "id": "cs_test_1", "mode": "payment", "payment_status": "paid",
                "amount_total": 4900, "currency": "eur", "customer": "cus_1",
                "customer_details": { "email": "Ada@Example.com" },
                "client_reference_id": "mfr_abc"
            }}
        });
        let r = read_stripe(&event).unwrap();
        assert_eq!(r.event_id, "evt_1");
        assert_eq!(r.drafts.len(), 1);
        let d = &r.drafts[0];
        assert_eq!(
            (d.kind, d.amount, d.currency.as_str()),
            ("sale", 4900, "EUR")
        );
        assert_eq!(d.customer_id.as_deref(), Some("cus_1"));
        assert_eq!(d.checkout_ref.as_deref(), Some("mfr_abc"));
        assert_eq!(d.at.timestamp(), NOW);
    }

    #[test]
    fn a_subscription_checkout_remembers_its_reference_and_records_nothing() {
        let event = json!({
            "id": "evt_2", "type": "checkout.session.completed", "created": NOW,
            "data": { "object": {
                "id": "cs_2", "mode": "subscription", "payment_status": "paid",
                "amount_total": 1200, "currency": "usd", "customer": "cus_2",
                "metadata": { "mf_ref": "mfr_sub" }
            }}
        });
        let r = read_stripe(&event).unwrap();
        assert!(r.drafts.is_empty());
        assert_eq!(
            r.remember,
            Some(("cus_2".to_string(), "mfr_sub".to_string()))
        );
        // An unpaid one-off session records nothing either.
        let unpaid = json!({
            "id": "evt_3", "type": "checkout.session.completed",
            "data": { "object": { "mode": "payment", "payment_status": "unpaid", "amount_total": 10, "currency": "usd" } }
        });
        assert!(read_stripe(&unpaid).unwrap().drafts.is_empty());
    }

    #[test]
    fn only_subscription_invoices_count() {
        let invoice = |extra: Value| {
            let mut obj = json!({ "id": "in_1", "amount_paid": 1200, "currency": "usd", "customer": "cus_2" });
            for (k, v) in extra.as_object().unwrap() {
                obj[k] = v.clone();
            }
            json!({ "id": "evt_4", "type": "invoice.paid", "created": NOW, "data": { "object": obj } })
        };
        let sub = read_stripe(&invoice(json!({ "subscription": "sub_1" }))).unwrap();
        assert_eq!(sub.drafts[0].kind, "subscription");
        let newer_api = read_stripe(&invoice(json!({
            "parent": { "subscription_details": { "subscription": "sub_1", "metadata": { "mf_ref": "mfr_x" } } }
        })))
        .unwrap();
        assert_eq!(newer_api.drafts[0].checkout_ref.as_deref(), Some("mfr_x"));
        assert!(read_stripe(&invoice(json!({ "billing_reason": "manual" })))
            .unwrap()
            .drafts
            .is_empty());
        // A trial's zero invoice is not a payment.
        assert!(read_stripe(&invoice(
            json!({ "subscription": "sub_1", "amount_paid": 0 })
        ))
        .unwrap()
        .drafts
        .is_empty());
    }

    #[test]
    fn a_stripe_refund_reports_the_charges_cumulative_total() {
        let event = json!({
            "id": "evt_5", "type": "charge.refunded", "created": NOW,
            "data": { "object": { "id": "ch_1", "amount_refunded": 700, "currency": "eur", "customer": "cus_1" } }
        });
        let d = &read_stripe(&event).unwrap().drafts[0];
        assert_eq!(d.kind, "refund");
        assert_eq!(d.refund_total, Some(("ch_1".to_string(), 700)));
    }

    #[test]
    fn paddle_transactions_and_approved_adjustments_are_read() {
        let txn = json!({
            "event_id": "evt_01", "event_type": "transaction.completed",
            "occurred_at": "2026-10-08T10:00:00.000Z",
            "data": {
                "id": "txn_1", "customer_id": "ctm_1", "currency_code": "GBP",
                "subscription_id": "sub_01", "custom_data": { "mf_ref": "mfr_p" },
                "details": { "totals": { "total": "2400" } }
            }
        });
        let r = read_paddle(&txn).unwrap();
        let d = &r.drafts[0];
        assert_eq!(
            (d.kind, d.amount, d.currency.as_str()),
            ("subscription", 2400, "GBP")
        );
        assert_eq!(d.checkout_ref.as_deref(), Some("mfr_p"));
        assert_eq!(d.at.to_rfc3339(), "2026-10-08T10:00:00+00:00");

        let adjustment = |status: &str, action: &str| {
            json!({
                "event_id": "evt_02", "event_type": "adjustment.updated",
                "data": {
                    "id": "adj_1", "action": action, "status": status, "transaction_id": "txn_1",
                    "customer_id": "ctm_1", "currency_code": "GBP", "totals": { "total": "1000" }
                }
            })
        };
        let approved = read_paddle(&adjustment("approved", "refund")).unwrap();
        assert_eq!(
            approved.drafts[0].refund_total,
            Some(("adj_1".to_string(), 1000))
        );
        assert_eq!(approved.drafts[0].transaction_id, "txn_1");
        assert!(read_paddle(&adjustment("pending_approval", "refund"))
            .unwrap()
            .drafts
            .is_empty());
        assert!(read_paddle(&adjustment("approved", "credit"))
            .unwrap()
            .drafts
            .is_empty());
        assert!(read_paddle(&json!({ "event_type": "transaction.completed" })).is_err());
    }

    #[test]
    fn dimensions_come_from_the_fixed_list_only() {
        assert_eq!(dimension_column(None).unwrap(), "toString(channel)");
        assert_eq!(
            dimension_column(Some("utm_campaign")).unwrap(),
            "utm_campaign"
        );
        assert_eq!(
            dimension_column(Some("amount; DROP TABLE revenue"))
                .unwrap_err()
                .field(),
            Some("dimension")
        );
    }

    #[test]
    fn currencies_are_three_letters_upper_case() {
        assert_eq!(currency(Some("eur")).as_deref(), Some("EUR"));
        assert_eq!(currency(Some("euro")), None);
        assert_eq!(currency(Some("e1r")), None);
    }
}
