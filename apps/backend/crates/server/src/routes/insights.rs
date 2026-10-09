//! `/api/investigations/*` and `/api/insights/*` — what changed, and who hears.
//!
//! The detector and the life-cycle rules live in the worker
//! (`micaforge_jobs::investigate`); this module reads what it stored, lets a
//! member move an investigation by hand, queues a scan of one site on demand,
//! and keeps each person's choice about the weekly digest.
//!
//! A manual status change follows the same schedule rules as the detector's:
//! reopening puts the investigation back on the daily re-check, resolving takes
//! it off. The change is written to the timeline with who made it, so the
//! history never shows a status nobody can account for.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use micaforge_core::{new_id, Error};
use micaforge_store::models::investigations::{self, Event, Investigation};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The statuses a person may set.
const STATUSES: &[&str] = &["open", "monitoring", "resolved"];

/// Most investigations listed at once.
const MAX_LISTED: i64 = 200;

/// Every insights route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/investigations", get(list))
        .route("/api/investigations/scan", post(scan))
        .route("/api/investigations/{id}", get(detail).patch(set_status))
        .route(
            "/api/insights/subscription",
            get(subscription).put(set_subscription),
        )
}

/// `?status=` for the list.
#[derive(Debug, Deserialize)]
struct ListQuery {
    #[serde(default)]
    status: Option<String>,
}

/// `GET /api/investigations?site=&status=`.
async fn list(
    State(st): State<AppState>,
    access: SiteAccess,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let status = match q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("all") => None,
        Some(raw) => Some(
            STATUSES
                .iter()
                .copied()
                .find(|s| *s == raw)
                .ok_or_else(|| {
                    Error::validation("status", "must be open, monitoring, resolved or all")
                })?,
        ),
    };
    let rows = investigations::list(&st.pg, access.site.id, status, MAX_LISTED).await?;
    Ok(ok(rows
        .iter()
        .map(|i| investigation_json(i, access.site_id))
        .collect::<Vec<_>>()))
}

/// Load an investigation the caller may act on with `needed`.
async fn reachable(
    st: &AppState,
    auth: &AuthUser,
    id: Uuid,
    needed: &str,
) -> Result<(Investigation, u16), Error> {
    let inv = investigations::get(&st.pg, id).await?;
    match authorize_site(&st.pg, inv.site_id, auth.user.id, needed).await {
        Ok(site) => Ok((inv, site.site_id_u16())),
        Err(Error::NotFound(_)) => Err(Error::not_found("investigation")),
        Err(e) => Err(e),
    }
}

/// `GET /api/investigations/{id}` — with its timeline.
async fn detail(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (inv, site) = reachable(&st, &auth, id, VIEWER).await?;
    let events = investigations::list_events(&st.pg, inv.id).await?;
    let mut body = investigation_json(&inv, site);
    body["events"] = Value::Array(events.iter().map(event_json).collect());
    Ok(ok(body))
}

/// A status change.
#[derive(Debug, Deserialize)]
struct StatusBody {
    status: String,
}

/// `PATCH /api/investigations/{id}`.
async fn set_status(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> ApiResult<Json<Value>> {
    let (inv, site) = reachable(&st, &auth, id, MEMBER).await?;
    let status = STATUSES
        .iter()
        .copied()
        .find(|s| *s == body.status.trim())
        .ok_or_else(|| Error::validation("status", "must be open, monitoring or resolved"))?;
    if status == inv.status {
        return Ok(ok(investigation_json(&inv, site)));
    }
    // Only one investigation of a change may be live. Reopening an old one
    // while the detector has opened a newer one would make two.
    if inv.status == "resolved" {
        if let Some(newer) = investigations::find_live(&st.pg, inv.site_id, &inv.signal_key).await?
        {
            return Err(Error::Conflict(format!(
                "a newer investigation of the same change is already {}; open that one instead ({})",
                newer.status, newer.id
            ))
            .into());
        }
    }
    // A live investigation goes back on the daily re-check, with its calm
    // count reset; a resolved one leaves the schedule.
    let next_check = (status != "resolved").then(|| Utc::now() + Duration::hours(24));
    let updated = investigations::set_status(&st.pg, inv.id, status, 0, next_check).await?;
    investigations::add_event(
        &st.pg,
        new_id(),
        inv.id,
        "status",
        &format!("{} set it to {status}", auth.user.name),
        &json!({ "status": status, "by": auth.user.id }),
    )
    .await?;
    Ok(ok(investigation_json(&updated, site)))
}

/// `POST /api/investigations/scan?site=` — scan this site now.
async fn scan(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let job = micaforge_jobs::enqueue_now(
        &st.pg,
        micaforge_jobs::Kind::Investigate,
        &micaforge_jobs::investigate::Payload {
            site: Some(access.site.id),
        },
    )
    .await?;
    Ok(ok(json!({ "queued": true, "job_id": job })))
}

/// `GET /api/insights/subscription?site=`.
async fn subscription(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let wanted = investigations::weekly_digest(&st.pg, access.site.id, auth.user.id).await?;
    Ok(ok(json!({
        "weekly_digest": wanted,
        "mail_enabled": st.mailer.is_sending(),
    })))
}

/// The choice.
#[derive(Debug, Deserialize)]
struct SubscriptionBody {
    weekly_digest: bool,
}

/// `PUT /api/insights/subscription?site=`.
///
/// A viewer may subscribe: the digest says nothing a viewer cannot already
/// read on the Insights screen.
async fn set_subscription(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(body): Json<SubscriptionBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    investigations::set_weekly_digest(&st.pg, access.site.id, auth.user.id, body.weekly_digest)
        .await?;
    Ok(ok(json!({
        "weekly_digest": body.weekly_digest,
        "mail_enabled": st.mailer.is_sending(),
    })))
}

/// One investigation on the wire.
pub fn investigation_json(i: &Investigation, site: u16) -> Value {
    json!({
        "id": i.id,
        "site": site,
        "signal_key": i.signal_key,
        "title": i.title,
        "status": i.status,
        "severity": i.severity,
        "metric": i.metric,
        "dimension": i.dimension,
        "dimension_value": i.dimension_value,
        "direction": i.direction,
        "current": i.current_value,
        "baseline": i.baseline_value,
        "change_pct": i.change_pct,
        "window": i.window_label,
        "findings": i.findings,
        "evidence": i.evidence,
        "explanation": i.explanation,
        "explanation_status": i.explanation_status,
        "created_at": i.created_at,
        "updated_at": i.updated_at,
        "next_check_at": i.next_check_at,
        "resolved_at": i.resolved_at,
    })
}

/// One timeline entry on the wire.
fn event_json(e: &Event) -> Value {
    json!({
        "id": e.id,
        "kind": e.kind,
        "summary": e.summary,
        "data": e.data,
        "created_at": e.created_at,
    })
}

/// The store rules the detector relies on, against a real Postgres. See
/// [`crate::routes::oauth::harness`] for how to run these.
#[cfg(test)]
mod live {
    use axum::body::Body;
    use axum::http::Request;
    use chrono::{Duration, Utc};
    use micaforge_core::new_id;
    use micaforge_store::models::investigations::{self, Measure};
    use micaforge_store::models::{annotations, sites};
    use serde_json::json;

    use crate::routes::oauth::harness::Harness;

    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn one_live_investigation_per_signal_and_a_timeline_that_follows_it() {
        let h = Harness::new().await;
        let ada = h.person("ada@example.com").await;
        let site = sites::create(
            &h.st.pg,
            new_id(),
            ada.org,
            "docs.example.com",
            "Docs",
            "UTC",
        )
        .await
        .unwrap();
        let findings = json!([]);
        let evidence = json!([]);
        let measure = Measure {
            title: "Visitors fell",
            severity: "warning",
            direction: "down",
            current: 120.0,
            baseline: 300.0,
            change_pct: Some(-60.0),
            window_label: "7 days against the previous 7",
            findings: &findings,
            evidence: &evidence,
        };
        let open = |id| {
            investigations::open(
                &h.st.pg,
                id,
                site.id,
                "wow:visitors",
                "visitors",
                None,
                None,
                &measure,
                "not_configured",
                Utc::now() + Duration::hours(24),
            )
        };
        let first = open(new_id()).await.unwrap().expect("the first opens");
        // The same signal again, while the first is live: refused, not doubled.
        assert!(open(new_id()).await.unwrap().is_none());
        assert_eq!(
            investigations::find_live(&h.st.pg, site.id, "wow:visitors")
                .await
                .unwrap()
                .unwrap()
                .id,
            first.id
        );

        // Monitoring is still live; resolving frees the signal for a recurrence.
        investigations::set_status(&h.st.pg, first.id, "monitoring", 1, Some(Utc::now()))
            .await
            .unwrap();
        assert!(open(new_id()).await.unwrap().is_none());
        let resolved = investigations::set_status(&h.st.pg, first.id, "resolved", 2, None)
            .await
            .unwrap();
        assert!(resolved.resolved_at.is_some());
        assert!(resolved.next_check_at.is_none());

        // Reopening clears the resolution time.
        let reopened = investigations::set_status(&h.st.pg, first.id, "open", 0, Some(Utc::now()))
            .await
            .unwrap();
        assert!(reopened.resolved_at.is_none());
        investigations::set_status(&h.st.pg, first.id, "resolved", 2, None)
            .await
            .unwrap();
        assert!(
            open(new_id()).await.unwrap().is_some(),
            "a recurrence is a new case"
        );

        // With the recurrence live, the old case cannot be reopened beside it.
        let (status, _, refused) = h
            .send(
                Request::patch(format!("/api/investigations/{}", first.id))
                    .header("cookie", &ada.cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "status": "open" }).to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 409, "{refused}");

        // The detail endpoint shows it, with its timeline, to the site's people.
        investigations::add_event(&h.st.pg, new_id(), first.id, "opened", "Opened", &json!({}))
            .await
            .unwrap();
        let (status, _, body) = h
            .send(
                Request::get(format!("/api/investigations/{}", first.id))
                    .header("cookie", &ada.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["data"]["events"][0]["kind"], "opened");
        assert_eq!(body["data"]["site"], site.site_id);

        // ... and to nobody else.
        let bob = h.person("bob@example.com").await;
        let (status, _, _) = h
            .send(
                Request::get(format!("/api/investigations/{}", first.id))
                    .header("cookie", &bob.cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 404);

        // An annotation overlapping a window is listed even when it began before it.
        let start = Utc::now() - Duration::days(3);
        annotations::create(
            &h.st.pg,
            new_id(),
            site.id,
            &annotations::Fields {
                starts_at: start - Duration::days(2),
                ends_at: Some(start + Duration::hours(5)),
                text: "Outage",
                color: "red",
                category: Some("incident"),
            },
            "dashboard",
            None,
        )
        .await
        .unwrap();
        let listed = annotations::list_in(&h.st.pg, site.id, start, Utc::now(), 10)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        let none = annotations::list_in(
            &h.st.pg,
            site.id,
            Utc::now() - Duration::hours(1),
            Utc::now(),
            10,
        )
        .await
        .unwrap();
        assert!(none.is_empty());

        h.finish().await;
    }
}
