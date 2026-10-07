//! `/api/alerts/*` and `/api/reports/*` — the two ways Micaforge reaches out.
//!
//! An **alert** is conditional and fires when something crosses a threshold. A
//! **report** is scheduled and goes out whether or not anything happened. They
//! share a `channels` document and nothing else.
//!
//! ## Configuration is completed here, not guessed later
//!
//! `alerts.config` is opaque jsonb, and the worker that evaluates it runs hours
//! later with nobody watching. A config that is missing a threshold would leave
//! the worker choosing one, and an alert whose threshold was invented by a
//! default buried in a job module is an alert nobody can reason about. So
//! [`complete_config`] fills every field this kind needs, at write time, and
//! stores the completed document. What the operator sees on the screen is
//! exactly what will be evaluated.
//!
//! ## Channels are validated against the same parser the worker uses
//!
//! [`micaforge_jobs::notify::Channels`] reads these documents leniently, because
//! a typo should cost one channel and not the whole alert. That leniency is
//! right at delivery time and wrong at write time: a webhook URL with a typo
//! would be silently dropped and the operator would believe it was configured.
//! So the same parser runs here, and anything it *cannot* see is refused.
//!
//! A webhook is additionally put through [`micaforge_jobs::fetch::guard_url`],
//! which refuses a private address. Without it, "send my alerts to this URL"
//! would be a request-forgery primitive pointed at whatever the host can reach.
//! The test send goes out through [`micaforge_jobs::outbound::post_json`], which
//! repeats that check on every redirect hop and every resolved address, and its
//! result says only what kind of failure happened: "could not connect", "timed
//! out", "non-2xx status 500". Handing back the client library's error text
//! would turn the test button into a port scanner for the server's network.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use micaforge_core::{new_id, Error, Result};
use micaforge_jobs::notify::Channels;
use micaforge_jobs::outbound::{self, FailureKind};
use micaforge_store::models::{alerts, goals};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{clean_email, require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Every alert kind, from `docs/CONTRACT.md` §2.
///
/// The last two are the wedge: a crawl surge, and a URL whose agent traffic
/// climbed while answer-engine arrivals did not.
const ALERT_KINDS: &[&str] = &[
    "spike",
    "drop",
    "goal",
    "error",
    "agent_surge",
    "citation_gap",
];

/// Every report cadence.
const CADENCES: &[&str] = &["daily", "weekly", "monthly"];

/// Shortest cooldown an alert may have, in minutes.
///
/// Five. An alert that can fire every minute is a pager nobody will keep
/// carrying, and muting it is how a real incident gets missed.
const MIN_COOLDOWN_MINUTES: i64 = 5;

/// Longest cooldown, in minutes. A week.
const MAX_COOLDOWN_MINUTES: i64 = 7 * 24 * 60;

/// Default cooldown when the operator does not choose one.
const DEFAULT_COOLDOWN_MINUTES: i64 = 60;

/// How many addresses one alert or report may name.
const MAX_RECIPIENTS: usize = 50;

/// How long a test webhook may take before it is called a failure.
const TEST_WEBHOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every alert and scheduled-report route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/alerts", get(list_alerts).post(create_alert))
        .route(
            "/api/alerts/{id}",
            get(get_alert).patch(update_alert).delete(delete_alert),
        )
        .route("/api/alerts/{id}/test", post(test_alert))
        .route("/api/reports", get(list_reports).post(create_report))
        .route(
            "/api/reports/{id}",
            get(get_report).patch(update_report).delete(delete_report),
        )
        .route("/api/reports/{id}/send-now", post(send_report_now))
}

// ---------------------------------------------------------------------------
// Alerts
// ---------------------------------------------------------------------------

/// `GET /api/alerts?site=…`
async fn list_alerts(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = alerts::list_alerts(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(alert_json).collect::<Vec<_>>()))
}

/// An alert, as the settings screen sends it.
#[derive(Debug, Deserialize)]
struct AlertBody {
    /// `spike` | `drop` | `goal` | `error` | `agent_surge` | `citation_gap`.
    kind: String,
    /// Thresholds and window. Missing fields are filled with this kind's
    /// documented defaults rather than left for the worker to invent.
    #[serde(default)]
    config: Option<Value>,
    /// Where a notification goes.
    channels: Value,
}

/// `POST /api/alerts?site=…`
async fn create_alert(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<AlertBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let kind = check_kind(&body.kind)?;
    let config = complete_config(kind, body.config.as_ref())?;
    check_goal_reference(&st, kind, &config, access.site.id).await?;
    let channels = check_channels(&body.channels, "channels")?;

    let alert =
        alerts::create_alert(&st.pg, new_id(), access.site.id, kind, &config, &channels).await?;
    Ok(ok(alert_json(&alert)))
}

/// `GET /api/alerts/{id}`
async fn get_alert(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let alert = alerts::get_alert(&st.pg, id).await?;
    reachable(&st, alert.site_id, auth.user.id, VIEWER, "alert").await?;
    Ok(ok(alert_json(&alert)))
}

/// `PATCH /api/alerts/{id}`
async fn update_alert(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AlertBody>,
) -> ApiResult<Json<Value>> {
    let alert = alerts::get_alert(&st.pg, id).await?;
    reachable(&st, alert.site_id, auth.user.id, MEMBER, "alert").await?;

    let kind = check_kind(&body.kind)?;
    let config = complete_config(kind, body.config.as_ref())?;
    check_goal_reference(&st, kind, &config, alert.site_id).await?;
    let channels = check_channels(&body.channels, "channels")?;

    let updated = alerts::update_alert(&st.pg, alert.id, kind, &config, &channels).await?;
    Ok(ok(alert_json(&updated)))
}

/// `DELETE /api/alerts/{id}`
async fn delete_alert(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let alert = alerts::get_alert(&st.pg, id).await?;
    reachable(&st, alert.site_id, auth.user.id, MEMBER, "alert").await?;
    alerts::delete_alert(&st.pg, alert.id).await?;
    Ok(ok(json!({ "deleted": alert.id })))
}

/// `POST /api/alerts/{id}/test` — prove the channels work.
///
/// Sends a message that is *visibly* a test, per `docs/CONTRACT.md` §7.4: every
/// number in a demo is labelled as one. Somebody who finds this in their inbox
/// at 3am must be able to tell in one glance that nothing is wrong.
///
/// Reports per-channel outcomes instead of failing on the first refusal. "The
/// email worked and Slack did not" is the answer the screen needs; a single
/// error would send the operator hunting through all three.
async fn test_alert(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let alert = alerts::get_alert(&st.pg, id).await?;
    let site = reachable(&st, alert.site_id, auth.user.id, MEMBER, "alert").await?;
    let channels = Channels::from_json(&alert.channels);

    let payload = micaforge_mail::AlertPayload {
        kind: alert.kind.clone(),
        headline: format!("Test alert for {} — nothing is wrong", site.name),
        detail: "This message was sent by hand from the alert settings screen to check that this \
             channel is reachable. The figures below are placeholders, not measurements."
            .to_string(),
        metric: "sample metric (not a measurement)".to_string(),
        observed: 0.0,
        expected: 0.0,
        link: format!(
            "{}/app/{}/settings/alerts",
            st.cfg.dashboard_url.trim_end_matches('/'),
            site.site_id_u16()
        ),
    };

    let mut results = Vec::new();
    for address in &channels.emails {
        let outcome = st.mailer.send_alert(address, &site.name, &payload).await;
        results.push(json!({
            "channel": "email",
            "target": address,
            "delivered": outcome.is_ok(),
            "error": outcome.err().map(|e| e.to_string()),
        }));
    }

    if let Some(url) = channels.webhook.as_deref() {
        results.push(
            test_webhook(
                &st,
                url,
                &json!({
                    "test": true,
                    "site": site.domain,
                    "kind": alert.kind,
                    "headline": payload.headline,
                }),
            )
            .await,
        );
    }
    if let Some(url) = channels.slack.as_deref() {
        results.push(test_webhook(&st, url, &json!({ "text": payload.headline })).await);
    }

    Ok(ok(json!({
        "alert_id": alert.id,
        "is_test": true,
        "mail_delivered": st.mailer.is_sending(),
        "results": results,
    })))
}

/// POST a body to a webhook and describe what happened.
///
/// The description is categorical on purpose; see the module documentation.
/// The full detail, host included and path left out, goes to the log for the
/// operator.
async fn test_webhook(st: &AppState, url: &str, body: &Value) -> Value {
    match outbound::post_json(&st.http, url, body, TEST_WEBHOOK_TIMEOUT).await {
        Ok(status) => json!({
            "channel": "webhook",
            "target": url,
            "delivered": true,
            "status": status,
        }),
        Err(failure) => {
            tracing::warn!(error = %failure, "test webhook failed");
            let mut result = json!({
                "channel": "webhook",
                "target": url,
                "delivered": false,
                "error": failure.public_message(),
            });
            if let FailureKind::Status(status) = failure.kind {
                result["status"] = json!(status);
            }
            result
        }
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

/// `GET /api/reports?site=…`
async fn list_reports(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = alerts::list_reports(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(report_json).collect::<Vec<_>>()))
}

/// A scheduled digest, as the settings screen sends it.
#[derive(Debug, Deserialize)]
struct ReportBody {
    /// `daily` | `weekly` | `monthly`.
    cadence: String,
    /// Who receives it.
    recipients: Value,
    /// How it is delivered. Merged with `recipients` at send time.
    #[serde(default)]
    channels: Option<Value>,
}

/// `POST /api/reports?site=…`
///
/// The first run is computed here, in the **site's** timezone, not the reader's:
/// a weekly report for a site in Lisbon should arrive on Monday morning in
/// Lisbon whoever configured it.
async fn create_report(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<ReportBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let cadence = check_cadence(&body.cadence)?;
    let recipients = check_channels(&body.recipients, "recipients")?;
    let channels = match body.channels.as_ref() {
        Some(value) => check_channels(value, "channels")?,
        None => json!({}),
    };
    let next_run = next_run(&access.site.timezone, cadence);

    let report = alerts::create_report(
        &st.pg,
        new_id(),
        access.site.id,
        cadence,
        &recipients,
        &channels,
        next_run,
    )
    .await?;
    Ok(ok(report_json(&report)))
}

/// `GET /api/reports/{id}`
async fn get_report(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let report = alerts::get_report(&st.pg, id).await?;
    reachable(&st, report.site_id, auth.user.id, VIEWER, "report").await?;
    Ok(ok(report_json(&report)))
}

/// `PATCH /api/reports/{id}`
///
/// Changing the cadence re-computes the next run. Leaving it where it was would
/// send tomorrow's daily digest to somebody who just asked for a monthly one.
async fn update_report(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ReportBody>,
) -> ApiResult<Json<Value>> {
    let report = alerts::get_report(&st.pg, id).await?;
    let site = reachable(&st, report.site_id, auth.user.id, MEMBER, "report").await?;

    let cadence = check_cadence(&body.cadence)?;
    let recipients = check_channels(&body.recipients, "recipients")?;
    let channels = match body.channels.as_ref() {
        Some(value) => check_channels(value, "channels")?,
        None => report.channels.clone(),
    };

    let updated = alerts::update_report(&st.pg, report.id, cadence, &recipients, &channels).await?;

    if cadence != report.cadence {
        if let Some(next) = next_run(&site.timezone, cadence) {
            alerts::set_next_run(&st.pg, report.id, next).await?;
            return Ok(ok(report_json(
                &alerts::get_report(&st.pg, report.id).await?,
            )));
        }
    }
    Ok(ok(report_json(&updated)))
}

/// `DELETE /api/reports/{id}`
async fn delete_report(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let report = alerts::get_report(&st.pg, id).await?;
    reachable(&st, report.site_id, auth.user.id, MEMBER, "report").await?;
    alerts::delete_report(&st.pg, report.id).await?;
    Ok(ok(json!({ "deleted": report.id })))
}

/// `POST /api/reports/{id}/send-now` — build and send it immediately.
///
/// Queued rather than run inline. Building a report reads a month of ClickHouse
/// and then talks to an SMTP relay, neither of which belongs inside a request a
/// browser is waiting on.
///
/// The schedule is untouched: an operator asking for a copy now must not thereby
/// skip tomorrow's.
async fn send_report_now(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let report = alerts::get_report(&st.pg, id).await?;
    reachable(&st, report.site_id, auth.user.id, MEMBER, "report").await?;

    let job = micaforge_jobs::enqueue_now(
        &st.pg,
        micaforge_jobs::Kind::SendReport,
        &micaforge_jobs::send_report::Payload {
            report: Some(report.id),
        },
    )
    .await?;

    Ok(ok(json!({
        "queued": true,
        "job_id": job,
        "report_id": report.id,
        "next_run_at": report.next_run_at,
    })))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate an alert kind.
fn check_kind(raw: &str) -> Result<&'static str> {
    let kind = raw.trim();
    ALERT_KINDS
        .iter()
        .copied()
        .find(|k| *k == kind)
        .ok_or_else(|| {
            Error::validation("kind", format!("must be one of {}", ALERT_KINDS.join(", ")))
        })
}

/// Validate a cadence.
fn check_cadence(raw: &str) -> Result<&'static str> {
    let cadence = raw.trim();
    CADENCES
        .iter()
        .copied()
        .find(|c| *c == cadence)
        .ok_or_else(|| {
            Error::validation("cadence", format!("must be one of {}", CADENCES.join(", ")))
        })
}

/// Fill an alert config out to everything its kind needs.
///
/// Every default here is a documented product decision rather than a fallback:
/// the worker reads these fields directly, so what is stored is what fires.
fn complete_config(kind: &str, supplied: Option<&Value>) -> Result<Value> {
    let mut config = match supplied {
        Some(Value::Object(map)) => map.clone(),
        Some(Value::Null) | None => Map::new(),
        Some(_) => return Err(Error::validation("config", "must be a JSON object")),
    };

    match kind {
        // A doubling within the hour. Spikes are about right now.
        "spike" => {
            default_str(&mut config, "metric", "visitors");
            default_number(&mut config, "threshold_pct", 100.0);
            default_number(&mut config, "window_hours", 1.0);
        }
        // Half the traffic, day on day. A drop is only real over a full day,
        // because an hour of quiet is a normal Tuesday afternoon.
        "drop" => {
            default_str(&mut config, "metric", "visitors");
            default_number(&mut config, "threshold_pct", 50.0);
            default_number(&mut config, "window_hours", 24.0);
        }
        "goal" => {
            if !config.contains_key("goal_id") {
                return Err(Error::validation(
                    "config.goal_id",
                    "a goal alert has to name the goal it watches",
                ));
            }
            default_str(&mut config, "direction", "below");
            default_number(&mut config, "threshold", 1.0);
            default_number(&mut config, "window_hours", 24.0);
        }
        "error" => {
            default_number(&mut config, "threshold", 10.0);
            default_number(&mut config, "window_hours", 1.0);
        }
        // Tripled crawling against the previous day. Crawl volume is spiky by
        // nature, so the bar is higher than for human traffic.
        "agent_surge" => {
            default_number(&mut config, "threshold_pct", 200.0);
            default_number(&mut config, "window_hours", 24.0);
        }
        // Fifty fetches per answer-engine arrival, over a week. The Citation
        // Gap needs a week to mean anything: a day of crawling with no arrivals
        // is normal, a week of it is the story.
        "citation_gap" => {
            default_number(&mut config, "ratio", 50.0);
            default_number(&mut config, "window_days", 7.0);
        }
        _ => {}
    }

    let cooldown = config
        .get("cooldown_minutes")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_COOLDOWN_MINUTES);
    if !(MIN_COOLDOWN_MINUTES..=MAX_COOLDOWN_MINUTES).contains(&cooldown) {
        return Err(Error::validation(
            "config.cooldown_minutes",
            format!("must be between {MIN_COOLDOWN_MINUTES} and {MAX_COOLDOWN_MINUTES}"),
        ));
    }
    config.insert("cooldown_minutes".to_string(), json!(cooldown));

    // Every threshold in every kind is a positive quantity, so one check covers
    // them all and a negative value cannot reach the worker.
    for field in [
        "threshold",
        "threshold_pct",
        "ratio",
        "window_hours",
        "window_days",
    ] {
        if let Some(value) = config.get(field) {
            let Some(number) = value.as_f64() else {
                return Err(Error::validation(
                    format!("config.{field}"),
                    "must be a number",
                ));
            };
            if !(number.is_finite() && number > 0.0) {
                return Err(Error::validation(
                    format!("config.{field}"),
                    "must be greater than zero",
                ));
            }
        }
    }

    Ok(Value::Object(config))
}

/// Insert a string default when the key is absent.
fn default_str(config: &mut Map<String, Value>, key: &str, value: &str) {
    config
        .entry(key.to_string())
        .or_insert_with(|| json!(value));
}

/// Insert a numeric default when the key is absent.
fn default_number(config: &mut Map<String, Value>, key: &str, value: f64) {
    config
        .entry(key.to_string())
        .or_insert_with(|| json!(value));
}

/// Refuse a goal alert pointed at another site's goal.
async fn check_goal_reference(
    st: &AppState,
    kind: &str,
    config: &Value,
    site_id: Uuid,
) -> Result<()> {
    if kind != "goal" {
        return Ok(());
    }
    let raw = config
        .get("goal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::validation("config.goal_id", "must be a goal id"))?;
    let goal_id = raw
        .parse::<Uuid>()
        .map_err(|_| Error::validation("config.goal_id", "is not a goal id"))?;

    let goal = goals::get(&st.pg, goal_id).await?;
    if goal.site_id != site_id {
        return Err(Error::validation(
            "config.goal_id",
            "belongs to another site",
        ));
    }
    Ok(())
}

/// Validate a channels or recipients document against the worker's own parser.
///
/// Read with [`Channels::from_json`], then checked: every address has to be an
/// address, and every webhook has to be a URL that does not point at the local
/// network. A document that parses to nothing at all is refused, because a
/// notification with nowhere to go is a notification that will not arrive.
fn check_channels(value: &Value, field: &str) -> Result<Value> {
    let parsed = Channels::from_json(value);
    if parsed.is_empty() {
        return Err(Error::validation(
            field,
            "needs at least one address or webhook; nothing here would be delivered",
        ));
    }
    if parsed.emails.len() > MAX_RECIPIENTS {
        return Err(Error::validation(
            field,
            format!("cannot name more than {MAX_RECIPIENTS} addresses"),
        ));
    }

    for address in &parsed.emails {
        clean_email(address).map_err(|_| {
            Error::validation(field, format!("`{address}` is not a valid email address"))
        })?;
    }
    for url in [parsed.webhook.as_deref(), parsed.slack.as_deref()]
        .into_iter()
        .flatten()
    {
        micaforge_jobs::fetch::guard_url(url)
            .map_err(|e| Error::validation(field, e.to_string()))?;
    }

    Ok(value.clone())
}

/// When a report of this cadence should next go out, in the site's own zone.
fn next_run(timezone: &str, cadence: &str) -> Option<chrono::DateTime<Utc>> {
    let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
    micaforge_jobs::schedule::next_report_run(Utc::now(), cadence, tz)
}

// ---------------------------------------------------------------------------
// Shaping and shared authorisation
// ---------------------------------------------------------------------------

/// One alert on the wire.
fn alert_json(alert: &alerts::Alert) -> Value {
    let channels = Channels::from_json(&alert.channels);
    json!({
        "id": alert.id,
        "site_id": alert.site_id,
        "kind": alert.kind,
        "config": alert.config,
        "channels": alert.channels,
        "last_fired_at": alert.last_fired_at,
        // Resolved so the screen can say "3 addresses, 1 webhook" without
        // re-implementing the lenient parser the worker uses.
        "resolved_channels": {
            "emails": channels.emails,
            "webhook": channels.webhook,
            "slack": channels.slack,
        },
    })
}

/// One report on the wire.
fn report_json(report: &alerts::Report) -> Value {
    let channels =
        Channels::from_json(&report.recipients).merge(Channels::from_json(&report.channels));
    json!({
        "id": report.id,
        "site_id": report.site_id,
        "cadence": report.cadence,
        "recipients": report.recipients,
        "channels": report.channels,
        "next_run_at": report.next_run_at,
        "resolved_channels": {
            "emails": channels.emails,
            "webhook": channels.webhook,
            "slack": channels.slack,
        },
    })
}

/// Assert the caller may act on an object belonging to `site_id`, and hand back
/// the site, which the alert test and the report schedule both need.
async fn reachable(
    st: &AppState,
    site_id: Uuid,
    user_id: Uuid,
    needed: &str,
    what: &str,
) -> Result<micaforge_store::models::sites::Site> {
    match authorize_site(&st.pg, site_id, user_id, needed).await {
        Ok(site) => Ok(site),
        Err(Error::NotFound(_)) => Err(Error::not_found(what.to_string())),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_contract_kind_is_accepted_and_nothing_else() {
        for kind in ALERT_KINDS {
            assert!(check_kind(kind).is_ok());
        }
        assert!(
            check_kind("agent_surge").is_ok(),
            "the wedge has its own alert"
        );
        assert!(check_kind("citation_gap").is_ok());
        assert!(check_kind("vibes").is_err());
    }

    #[test]
    fn a_config_leaves_the_endpoint_complete() {
        let config = complete_config("spike", None).unwrap();
        assert_eq!(config["metric"], json!("visitors"));
        assert_eq!(config["threshold_pct"], json!(100.0));
        assert_eq!(config["window_hours"], json!(1.0));
        assert_eq!(config["cooldown_minutes"], json!(DEFAULT_COOLDOWN_MINUTES));
    }

    #[test]
    fn a_supplied_value_is_never_overwritten_by_a_default() {
        let config = complete_config(
            "drop",
            Some(&json!({ "threshold_pct": 20, "metric": "sessions" })),
        )
        .unwrap();
        assert_eq!(config["threshold_pct"], json!(20));
        assert_eq!(config["metric"], json!("sessions"));
        // The field the operator did not set is still filled in.
        assert_eq!(config["window_hours"], json!(24.0));
    }

    #[test]
    fn a_goal_alert_must_name_its_goal() {
        assert_eq!(
            complete_config("goal", None).unwrap_err().field(),
            Some("config.goal_id")
        );
        assert!(complete_config(
            "goal",
            Some(&json!({ "goal_id": "018f8f00-0000-7000-8000-000000000000" }))
        )
        .is_ok());
    }

    #[test]
    fn thresholds_have_to_be_positive_numbers() {
        assert_eq!(
            complete_config("spike", Some(&json!({ "threshold_pct": -5 })))
                .unwrap_err()
                .field(),
            Some("config.threshold_pct")
        );
        assert_eq!(
            complete_config("spike", Some(&json!({ "threshold_pct": 0 })))
                .unwrap_err()
                .field(),
            Some("config.threshold_pct")
        );
        assert!(complete_config("spike", Some(&json!({ "window_hours": "soon" }))).is_err());
        assert!(complete_config("spike", Some(&json!(["not", "an", "object"]))).is_err());
    }

    #[test]
    fn the_cooldown_is_bounded_so_an_alert_cannot_become_a_pager() {
        assert!(complete_config("error", Some(&json!({ "cooldown_minutes": 1 }))).is_err());
        assert!(complete_config("error", Some(&json!({ "cooldown_minutes": 100_000 }))).is_err());
        let ok = complete_config("error", Some(&json!({ "cooldown_minutes": 15 }))).unwrap();
        assert_eq!(ok["cooldown_minutes"], json!(15));
    }

    #[test]
    fn the_citation_gap_alert_defaults_to_a_week() {
        let config = complete_config("citation_gap", None).unwrap();
        assert_eq!(config["window_days"], json!(7.0));
        assert_eq!(config["ratio"], json!(50.0));
    }

    #[test]
    fn channels_are_refused_when_nothing_would_be_delivered() {
        assert!(check_channels(&json!({}), "channels").is_err());
        assert!(check_channels(&json!([]), "channels").is_err());
        // A key the parser does not know contributes nothing, so it is refused
        // rather than silently stored as a channel that never fires.
        assert!(check_channels(&json!({ "sms": "+351000000000" }), "channels").is_err());
    }

    #[test]
    fn channels_accept_every_shape_the_worker_reads() {
        assert!(check_channels(&json!(["ops@example.com"]), "channels").is_ok());
        assert!(check_channels(&json!({ "email": "ops@example.com" }), "channels").is_ok());
        assert!(check_channels(
            &json!({ "emails": ["a@example.com", "b@example.com"] }),
            "channels"
        )
        .is_ok());
    }

    #[test]
    fn a_bad_address_is_named_rather_than_dropped() {
        let err = check_channels(&json!(["not-an-address"]), "recipients").unwrap_err();
        assert_eq!(err.field(), Some("recipients"));
    }

    #[test]
    fn a_webhook_cannot_point_at_the_local_network() {
        // Otherwise "send my alerts here" is a request-forgery primitive.
        assert!(
            check_channels(&json!({ "webhook": "http://127.0.0.1/admin" }), "channels").is_err()
        );
        assert!(check_channels(&json!({ "webhook": "file:///etc/passwd" }), "channels").is_err());
        assert!(check_channels(
            &json!({ "webhook": "https://hooks.example.com/abc" }),
            "channels"
        )
        .is_ok());
    }

    #[test]
    fn cadences_are_the_three_the_contract_names() {
        for cadence in CADENCES {
            assert!(check_cadence(cadence).is_ok());
        }
        assert!(check_cadence("hourly").is_err());
    }

    #[test]
    fn the_next_run_lands_in_the_future_in_the_sites_own_zone() {
        for cadence in CADENCES {
            let at = next_run("Europe/Lisbon", cadence).expect("a cadence must schedule");
            assert!(at > Utc::now(), "{cadence} scheduled in the past");
        }
        // A zone that will not parse falls back to UTC rather than failing.
        assert!(next_run("Mars/Olympus", "daily").is_some());
    }
}
