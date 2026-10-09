//! `/api/status-pages/*`, `/api/status-incidents/*` and the public
//! `/api/status/{slug}`.
//!
//! A status page publishes chosen uptime monitors of one site at
//! `/status/<slug>` on the dashboard origin, read with no account. The page
//! itself is a static document in the web app; everything on it comes from
//! [`public_status`], which is the one unauthenticated route here.
//!
//! ## What the public may read
//!
//! A monitor's public name, its state and ninety daily counts of checks and
//! failures. Never its URL, its assertions or its channels: an internal health
//! endpoint is exactly the kind of address a status page must not leak. The
//! incidents shown are the page's own, written for the public.
//!
//! ## Who may change what
//!
//! Creating, editing and deleting a page needs `admin` on the site, the role
//! that publishes a site's data (the same rule as share links). Posting an
//! incident needs `member`: during an outage the person on call should be able
//! to say so without first finding an admin.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use micaforge_core::{new_id, Error, Result};
use micaforge_query::exec::fetch_all;
use micaforge_query::sql::{fmt_ts, Param};
use micaforge_store::models::status_pages::{
    self as pages, IncidentFields, NewIncident, PageFields, StatusIncident, StatusPage,
};
use micaforge_store::models::{sites, uptime};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, ADMIN, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// How many days of daily bars a public page draws.
const PUBLIC_DAYS: i64 = 90;

/// How far back a public page lists closed incidents.
const HISTORY_DAYS: i64 = 14;

/// Most monitors one page may show.
const MAX_PAGE_MONITORS: usize = 50;

/// How long a browser or a CDN may reuse a public answer. Short: a status page
/// is read hardest during an outage, when it changes every minute.
const PUBLIC_CACHE: &str = "public, max-age=30";

/// The statuses of an incident.
const INCIDENT_STATUSES: &[&str] = &["investigating", "identified", "monitoring", "resolved"];

/// The statuses of a maintenance window.
const MAINTENANCE_STATUSES: &[&str] = &["scheduled", "in_progress", "completed"];

/// The impacts an incident may declare.
const IMPACTS: &[&str] = &["none", "minor", "major", "critical"];

/// Every status page route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/status-pages", get(list_pages).post(create_page))
        .route(
            "/api/status-pages/{id}",
            get(get_page).patch(update_page).delete(delete_page),
        )
        .route(
            "/api/status-pages/{id}/incidents",
            get(list_incidents).post(create_incident),
        )
        .route(
            "/api/status-incidents/{id}",
            patch(update_incident).delete(delete_incident),
        )
        .route("/api/status-incidents/{id}/updates", post(post_update))
        .route("/api/status/{slug}", get(public_status))
}

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// A page as the dashboard sends it.
#[derive(Debug, Deserialize)]
struct PageBody {
    slug: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    logo_url: Option<String>,
    #[serde(default)]
    accent_color: Option<String>,
    #[serde(default)]
    footer_label: Option<String>,
    #[serde(default)]
    footer_url: Option<String>,
    #[serde(default = "yes")]
    published: bool,
    #[serde(default)]
    monitors: Vec<PageMonitorBody>,
}

fn yes() -> bool {
    true
}

/// One monitor in a page body.
#[derive(Debug, Deserialize)]
struct PageMonitorBody {
    monitor_id: Uuid,
    #[serde(default)]
    display_name: Option<String>,
}

/// Validate a page body. `monitors_of_site` are the ids the page may show.
fn page_fields(body: &PageBody, monitors_of_site: &[Uuid]) -> Result<PageFields> {
    let slug = body.slug.trim().to_ascii_lowercase();
    check_slug(&slug)?;

    let title = body.title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(Error::validation("title", "must be 1 to 120 characters"));
    }
    let description = body.description.as_deref().unwrap_or_default().trim();
    if description.chars().count() > 1000 {
        return Err(Error::validation(
            "description",
            "must be at most 1000 characters",
        ));
    }

    let logo_url = match blank_to_none(body.logo_url.as_deref()) {
        None => None,
        Some(raw) => Some(web_url(raw, "logo_url", true)?),
    };

    let accent_color = match blank_to_none(body.accent_color.as_deref()) {
        None => "#2f6bff".to_string(),
        Some(raw) => {
            let ok = raw.len() == 7
                && raw.starts_with('#')
                && raw[1..].bytes().all(|b| b.is_ascii_hexdigit());
            if !ok {
                return Err(Error::validation(
                    "accent_color",
                    "must be a colour written #rrggbb",
                ));
            }
            raw.to_ascii_lowercase()
        }
    };

    let label = blank_to_none(body.footer_label.as_deref());
    let link = blank_to_none(body.footer_url.as_deref());
    let (footer_label, footer_url) = match (label, link) {
        (None, None) => (None, None),
        (Some(label), Some(link)) => {
            if label.chars().count() > 80 {
                return Err(Error::validation(
                    "footer_label",
                    "must be at most 80 characters",
                ));
            }
            (
                Some(label.to_string()),
                Some(web_url(link, "footer_url", false)?),
            )
        }
        (Some(_), None) => {
            return Err(Error::validation(
                "footer_url",
                "is needed when the footer link has a label",
            ))
        }
        (None, Some(_)) => {
            return Err(Error::validation(
                "footer_label",
                "is needed when the footer link has an address",
            ))
        }
    };

    if body.monitors.len() > MAX_PAGE_MONITORS {
        return Err(Error::validation(
            "monitors",
            format!("a page can show at most {MAX_PAGE_MONITORS} monitors"),
        ));
    }
    let mut monitors = Vec::with_capacity(body.monitors.len());
    for entry in &body.monitors {
        if !monitors_of_site.contains(&entry.monitor_id) {
            return Err(Error::validation(
                "monitors",
                "names a monitor that is not on this site",
            ));
        }
        if monitors.iter().any(|(id, _)| *id == entry.monitor_id) {
            return Err(Error::validation("monitors", "names a monitor twice"));
        }
        let display = blank_to_none(entry.display_name.as_deref());
        if display.is_some_and(|d| d.chars().count() > 120) {
            return Err(Error::validation(
                "monitors",
                "a display name must be at most 120 characters",
            ));
        }
        monitors.push((entry.monitor_id, display.map(str::to_string)));
    }

    Ok(PageFields {
        slug,
        title: title.to_string(),
        description: description.to_string(),
        logo_url,
        accent_color,
        footer_label,
        footer_url,
        published: body.published,
        monitors,
    })
}

/// A slug is 3 to 64 of `[a-z0-9-]`, and starts and ends with a letter or digit.
fn check_slug(slug: &str) -> Result<()> {
    let ok = (3..=64).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !slug.starts_with('-')
        && !slug.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(Error::validation(
            "slug",
            "must be 3 to 64 lowercase letters, digits or hyphens, not starting or ending with a hyphen",
        ))
    }
}

/// An absolute `https` URL (or `http` too, when `https_only` is false), as text.
///
/// The browser of whoever reads the status page is what fetches these, never
/// this server, so they are not put through the outbound guard; they only have
/// to be web addresses and not, say, `javascript:`.
fn web_url(raw: &str, field: &str, https_only: bool) -> Result<String> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| Error::validation(field, "is not an absolute URL"))?;
    let allowed = if https_only {
        url.scheme() == "https"
    } else {
        matches!(url.scheme(), "http" | "https")
    };
    if !allowed || url.host_str().is_none() || raw.len() > 2048 {
        return Err(Error::validation(
            field,
            if https_only {
                "must be an https address"
            } else {
                "must be an http or https address"
            },
        ));
    }
    Ok(url.to_string())
}

/// A trimmed value, with empty read as absent.
fn blank_to_none(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// The ids of the monitors on a site.
async fn monitor_ids(st: &AppState, site: Uuid) -> Result<Vec<Uuid>> {
    Ok(uptime::list_for_site(&st.pg, site)
        .await?
        .into_iter()
        .map(|m| m.id)
        .collect())
}

/// Pages on the wire, with their monitors.
async fn pages_json(st: &AppState, list: &[StatusPage]) -> Result<Vec<Value>> {
    let ids: Vec<Uuid> = list.iter().map(|p| p.id).collect();
    let monitors = pages::monitors_of(&st.pg, &ids).await?;
    let base = st.cfg.dashboard_url.trim_end_matches('/');
    Ok(list
        .iter()
        .map(|p| {
            let shown: Vec<Value> = monitors
                .iter()
                .filter(|m| m.page_id == p.id)
                .map(|m| {
                    json!({
                        "monitor_id": m.monitor_id,
                        "display_name": m.display_name,
                        "position": m.position,
                        "name": m.name,
                    })
                })
                .collect();
            json!({
                "id": p.id,
                "site_id": p.site_id,
                "slug": p.slug,
                "title": p.title,
                "description": p.description,
                "logo_url": p.logo_url,
                "accent_color": p.accent_color,
                "footer_label": p.footer_label,
                "footer_url": p.footer_url,
                "published": p.published,
                "public_url": format!("{base}/status/{}", p.slug),
                "monitors": shown,
                "created_at": p.created_at,
            })
        })
        .collect())
}

/// One page on the wire.
async fn page_json(st: &AppState, page: &StatusPage) -> Result<Value> {
    Ok(pages_json(st, std::slice::from_ref(page))
        .await?
        .into_iter()
        .next()
        .unwrap_or(Value::Null))
}

/// `GET /api/status-pages?site=`
async fn list_pages(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, VIEWER)?;
    let list = pages::list_pages(&st.pg, access.site.id).await?;
    Ok(ok(pages_json(&st, &list).await?))
}

/// `POST /api/status-pages?site=`
async fn create_page(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<PageBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let fields = page_fields(&body, &monitor_ids(&st, access.site.id).await?)?;
    let page = pages::create_page(&st.pg, new_id(), access.site.id, &fields)
        .await
        .map_err(slug_conflict)?;
    Ok(ok(page_json(&st, &page).await?))
}

/// `GET /api/status-pages/{id}`
async fn get_page(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let page = reachable_page(&st, id, auth.user.id, VIEWER).await?;
    Ok(ok(page_json(&st, &page).await?))
}

/// `PATCH /api/status-pages/{id}`
async fn update_page(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PageBody>,
) -> ApiResult<Json<Value>> {
    let page = reachable_page(&st, id, auth.user.id, ADMIN).await?;
    let fields = page_fields(&body, &monitor_ids(&st, page.site_id).await?)?;
    let updated = pages::update_page(&st.pg, page.id, &fields)
        .await
        .map_err(slug_conflict)?;
    Ok(ok(page_json(&st, &updated).await?))
}

/// `DELETE /api/status-pages/{id}`
async fn delete_page(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let page = reachable_page(&st, id, auth.user.id, ADMIN).await?;
    pages::delete_page(&st.pg, page.id).await?;
    Ok(ok(json!({ "deleted": page.id })))
}

/// A taken slug, said as the field it is.
fn slug_conflict(e: Error) -> Error {
    match e {
        Error::Conflict(_) => Error::Conflict("that status page address is taken".to_string()),
        other => other,
    }
}

/// Load a page and assert the caller's role on its site.
async fn reachable_page(st: &AppState, id: Uuid, user: Uuid, needed: &str) -> Result<StatusPage> {
    let page = pages::get_page(&st.pg, id).await?;
    match authorize_site(&st.pg, page.site_id, user, needed).await {
        Ok(_) => Ok(page),
        Err(Error::NotFound(_)) => Err(Error::not_found("status page")),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// Incidents
// ---------------------------------------------------------------------------

/// A new incident or maintenance window.
#[derive(Debug, Deserialize)]
struct IncidentBody {
    #[serde(default = "default_kind")]
    kind: String,
    title: String,
    status: String,
    #[serde(default = "default_impact")]
    impact: String,
    body: String,
    #[serde(default)]
    monitor_ids: Vec<Uuid>,
    #[serde(default)]
    scheduled_start: Option<DateTime<Utc>>,
    #[serde(default)]
    scheduled_end: Option<DateTime<Utc>>,
}

fn default_kind() -> String {
    "incident".to_string()
}
fn default_impact() -> String {
    "minor".to_string()
}

/// Changes to an incident after it was posted.
#[derive(Debug, Deserialize)]
struct IncidentPatch {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    impact: Option<String>,
    #[serde(default)]
    monitor_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    scheduled_start: Option<DateTime<Utc>>,
    #[serde(default)]
    scheduled_end: Option<DateTime<Utc>>,
}

/// One timeline entry.
#[derive(Debug, Deserialize)]
struct UpdateBody {
    status: String,
    body: String,
}

/// The status vocabulary of an incident kind.
fn statuses_for(kind: &str) -> Result<&'static [&'static str]> {
    match kind {
        "incident" => Ok(INCIDENT_STATUSES),
        "maintenance" => Ok(MAINTENANCE_STATUSES),
        _ => Err(Error::validation(
            "kind",
            "must be `incident` or `maintenance`",
        )),
    }
}

/// A status valid for the kind.
fn check_status(kind: &str, status: &str) -> Result<()> {
    let allowed = statuses_for(kind)?;
    if allowed.contains(&status) {
        Ok(())
    } else {
        Err(Error::validation(
            "status",
            format!("must be one of: {}", allowed.join(", ")),
        ))
    }
}

fn check_impact(impact: &str) -> Result<()> {
    if IMPACTS.contains(&impact) {
        Ok(())
    } else {
        Err(Error::validation(
            "impact",
            "must be none, minor, major or critical",
        ))
    }
}

fn check_title(title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(Error::validation("title", "must be 1 to 200 characters"));
    }
    Ok(title.to_string())
}

fn check_body(body: &str) -> Result<String> {
    let body = body.trim();
    if body.is_empty() || body.chars().count() > 5000 {
        return Err(Error::validation("body", "must be 1 to 5000 characters"));
    }
    Ok(body.to_string())
}

/// A maintenance window ends after it starts.
fn check_window(start: Option<DateTime<Utc>>, end: Option<DateTime<Utc>>) -> Result<()> {
    if let (Some(start), Some(end)) = (start, end) {
        if end <= start {
            return Err(Error::validation(
                "scheduled_end",
                "must be after the start",
            ));
        }
    }
    Ok(())
}

/// The affected monitors, each on the page.
async fn affected(st: &AppState, page: &StatusPage, ids: &[Uuid]) -> Result<Value> {
    let shown: Vec<Uuid> = pages::monitors_of(&st.pg, &[page.id])
        .await?
        .into_iter()
        .map(|m| m.monitor_id)
        .collect();
    let mut out = Vec::new();
    for id in ids {
        if !shown.contains(id) {
            return Err(Error::validation(
                "monitor_ids",
                "names a monitor this page does not show",
            ));
        }
        if !out.contains(id) {
            out.push(*id);
        }
    }
    Ok(json!(out))
}

/// Incidents on the wire, each with its timeline. `public` leaves out what the
/// public page has no use for.
async fn incidents_json(
    st: &AppState,
    list: &[StatusIncident],
    public: bool,
) -> Result<Vec<Value>> {
    let ids: Vec<Uuid> = list.iter().map(|i| i.id).collect();
    let updates = pages::updates_of(&st.pg, &ids).await?;
    Ok(list
        .iter()
        .map(|i| {
            let timeline: Vec<Value> = updates
                .iter()
                .filter(|u| u.incident_id == i.id)
                .map(|u| {
                    if public {
                        json!({ "status": u.status, "body": u.body, "created_at": u.created_at })
                    } else {
                        json!({
                            "id": u.id,
                            "status": u.status,
                            "body": u.body,
                            "automatic": u.automatic,
                            "created_at": u.created_at,
                        })
                    }
                })
                .collect();
            let mut v = json!({
                "id": i.id,
                "kind": i.kind,
                "title": i.title,
                "status": i.status,
                "impact": i.impact,
                "monitor_ids": i.monitor_ids,
                "scheduled_start": i.scheduled_start,
                "scheduled_end": i.scheduled_end,
                "created_at": i.created_at,
                "resolved_at": i.resolved_at,
                "updates": timeline,
            });
            if !public {
                v["page_id"] = json!(i.page_id);
                v["automatic"] = json!(i.automatic);
            }
            v
        })
        .collect())
}

/// `?limit=`
#[derive(Debug, Deserialize)]
struct LimitQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /api/status-pages/{id}/incidents`
async fn list_incidents(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Value>> {
    let page = reachable_page(&st, id, auth.user.id, VIEWER).await?;
    let list = pages::list_incidents(&st.pg, page.id, q.limit.unwrap_or(50)).await?;
    Ok(ok(incidents_json(&st, &list, false).await?))
}

/// `POST /api/status-pages/{id}/incidents`
async fn create_incident(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<IncidentBody>,
) -> ApiResult<Json<Value>> {
    let page = reachable_page(&st, id, auth.user.id, MEMBER).await?;
    let kind = body.kind.trim();
    check_status(kind, body.status.trim())?;
    check_impact(body.impact.trim())?;
    let title = check_title(&body.title)?;
    let text = check_body(&body.body)?;
    check_window(body.scheduled_start, body.scheduled_end)?;
    if kind == "incident" && (body.scheduled_start.is_some() || body.scheduled_end.is_some()) {
        return Err(Error::validation(
            "scheduled_start",
            "only a maintenance window has a schedule",
        )
        .into());
    }
    let monitors = affected(&st, &page, &body.monitor_ids).await?;

    let incident = pages::create_incident(
        &st.pg,
        &NewIncident {
            page_id: page.id,
            kind,
            title: &title,
            status: body.status.trim(),
            impact: body.impact.trim(),
            body: &text,
            monitor_ids: monitors,
            scheduled_start: body.scheduled_start,
            scheduled_end: body.scheduled_end,
            uptime_incident_id: None,
            automatic: false,
        },
    )
    .await?;
    let out = incidents_json(&st, std::slice::from_ref(&incident), false).await?;
    Ok(ok(out.into_iter().next().unwrap_or(Value::Null)))
}

/// Load an incident and its page, asserting the caller's role.
async fn reachable_incident(
    st: &AppState,
    id: Uuid,
    user: Uuid,
    needed: &str,
) -> Result<(StatusPage, StatusIncident)> {
    let incident = pages::get_incident(&st.pg, id).await?;
    let page = reachable_page(st, incident.page_id, user, needed)
        .await
        .map_err(|e| match e {
            Error::NotFound(_) => Error::not_found("incident"),
            other => other,
        })?;
    Ok((page, incident))
}

/// `PATCH /api/status-incidents/{id}`
async fn update_incident(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<IncidentPatch>,
) -> ApiResult<Json<Value>> {
    let (page, incident) = reachable_incident(&st, id, auth.user.id, MEMBER).await?;
    let title = match body.title.as_deref() {
        Some(t) => check_title(t)?,
        None => incident.title.clone(),
    };
    let impact = body.impact.unwrap_or_else(|| incident.impact.clone());
    check_impact(&impact)?;
    let monitor_ids = match body.monitor_ids.as_deref() {
        Some(ids) => affected(&st, &page, ids).await?,
        None => incident.monitor_ids.clone(),
    };
    let (start, end) = if incident.kind == "maintenance" {
        (
            body.scheduled_start.or(incident.scheduled_start),
            body.scheduled_end.or(incident.scheduled_end),
        )
    } else {
        (None, None)
    };
    check_window(start, end)?;

    let updated = pages::update_incident(
        &st.pg,
        incident.id,
        &IncidentFields {
            title,
            impact,
            monitor_ids,
            scheduled_start: start,
            scheduled_end: end,
        },
    )
    .await?;
    let out = incidents_json(&st, std::slice::from_ref(&updated), false).await?;
    Ok(ok(out.into_iter().next().unwrap_or(Value::Null)))
}

/// `POST /api/status-incidents/{id}/updates`
async fn post_update(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let (_, incident) = reachable_incident(&st, id, auth.user.id, MEMBER).await?;
    check_status(&incident.kind, body.status.trim())?;
    let text = check_body(&body.body)?;
    let updated = pages::add_update(&st.pg, incident.id, body.status.trim(), &text, false).await?;
    let out = incidents_json(&st, std::slice::from_ref(&updated), false).await?;
    Ok(ok(out.into_iter().next().unwrap_or(Value::Null)))
}

/// `DELETE /api/status-incidents/{id}`
async fn delete_incident(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (_, incident) = reachable_incident(&st, id, auth.user.id, MEMBER).await?;
    pages::delete_incident(&st.pg, incident.id).await?;
    Ok(ok(json!({ "deleted": incident.id })))
}

// ---------------------------------------------------------------------------
// The public page
// ---------------------------------------------------------------------------

/// One monitor's checks on one day.
#[derive(Debug, Clone, clickhouse::Row, Deserialize)]
struct DayCount {
    #[serde(with = "clickhouse::serde::uuid")]
    monitor_id: Uuid,
    day: String,
    checks: u64,
    failures: u64,
}

/// `GET /api/status/{slug}` — what `/status/<slug>` draws. No account.
///
/// An unknown slug and an unpublished page are the same `404`: telling a
/// stranger that a page exists but is hidden is information they have no claim
/// to.
async fn public_status(
    State(st): State<AppState>,
    Path(slug): Path<String>,
) -> ApiResult<(HeaderMap, Json<Value>)> {
    let slug = slug.trim().to_ascii_lowercase();
    if check_slug(&slug).is_err() {
        return Err(Error::not_found("status page").into());
    }
    let page = pages::get_published_by_slug(&st.pg, &slug).await?;
    let site = sites::get(&st.pg, page.site_id).await?;
    let shown = pages::monitors_of(&st.pg, &[page.id]).await?;

    let now = Utc::now();
    let first_day = (now - Duration::days(PUBLIC_DAYS - 1)).date_naive();
    let counts = if shown.is_empty() {
        Vec::new()
    } else {
        let slots = vec!["toUUID(?)"; shown.len()].join(", ");
        let sql = format!(
            "SELECT monitor_id, toString(toDate(timestamp, 'UTC')) AS day,
                count() AS checks, countIf(ok = 0) AS failures
             FROM uptime_checks
             WHERE site_id = ? AND monitor_id IN ({slots})
               AND timestamp >= toDateTime64(?, 3, 'UTC')
             GROUP BY monitor_id, day"
        );
        let mut params = vec![Param::U16(site.site_id_u16())];
        params.extend(shown.iter().map(|m| Param::Str(m.monitor_id.to_string())));
        params.push(Param::Str(fmt_ts(
            first_day.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc(),
        )));
        fetch_all::<DayCount>(&st.ch, "status_public_days", &sql, &params).await?
    };

    let mut by_monitor: HashMap<Uuid, HashMap<String, (u64, u64)>> = HashMap::new();
    for c in &counts {
        by_monitor
            .entry(c.monitor_id)
            .or_default()
            .insert(c.day.clone(), (c.checks, c.failures));
    }

    let monitors: Vec<Value> = shown
        .iter()
        .map(|m| {
            let days = by_monitor.get(&m.monitor_id);
            let (bars, checks, failures) = daily_bars(first_day, PUBLIC_DAYS, days);
            json!({
                "id": m.monitor_id,
                "name": m.public_name(),
                "state": if m.enabled { m.state.as_str() } else { "unknown" },
                "uptime_90d": (checks > 0).then(|| checks.saturating_sub(failures) as f64 / checks as f64),
                "days": bars,
            })
        })
        .collect();

    let incidents =
        pages::public_incidents(&st.pg, page.id, now - Duration::days(HISTORY_DAYS)).await?;
    let open: Vec<StatusIncident> = incidents
        .iter()
        .filter(|i| i.resolved_at.is_none())
        .cloned()
        .collect();
    let active: Vec<StatusIncident> = open
        .iter()
        .filter(|i| i.status != "scheduled")
        .cloned()
        .collect();
    let scheduled: Vec<StatusIncident> = open
        .iter()
        .filter(|i| i.status == "scheduled")
        .cloned()
        .collect();
    let history: Vec<StatusIncident> = incidents
        .iter()
        .filter(|i| i.resolved_at.is_some())
        .cloned()
        .collect();

    let states: Vec<&str> = shown
        .iter()
        .map(|m| {
            if m.enabled {
                m.state.as_str()
            } else {
                "unknown"
            }
        })
        .collect();
    let overall = overall_state(&states, &active);

    let footer = match (&page.footer_label, &page.footer_url) {
        (Some(label), Some(url)) => json!({ "label": label, "url": url }),
        _ => Value::Null,
    };

    let body = json!({
        "slug": page.slug,
        "title": page.title,
        "description": page.description,
        "logo_url": page.logo_url,
        "accent_color": page.accent_color,
        "footer": footer,
        "overall": overall,
        "monitors": monitors,
        "active": incidents_json(&st, &active, true).await?,
        "scheduled": incidents_json(&st, &scheduled, true).await?,
        "history": incidents_json(&st, &history, true).await?,
        "generated_at": now,
    });

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(PUBLIC_CACHE),
    );
    Ok((headers, ok(body)))
}

/// One bar per day from `first` for `days` days, oldest first, and the
/// window's totals.
fn daily_bars(
    first: NaiveDate,
    days: i64,
    counts: Option<&HashMap<String, (u64, u64)>>,
) -> (Vec<Value>, u64, u64) {
    let mut bars = Vec::with_capacity(usize::try_from(days).unwrap_or(0));
    let (mut checks, mut failures) = (0u64, 0u64);
    for offset in 0..days {
        let day = first + Duration::days(offset);
        let key = day.format("%Y-%m-%d").to_string();
        let (c, f) = counts.and_then(|m| m.get(&key)).copied().unwrap_or((0, 0));
        checks += c;
        failures += f;
        bars.push(json!({ "date": key, "checks": c, "failures": f }));
    }
    (bars, checks, failures)
}

/// The one-line state at the top of a public page.
///
/// An outage is a critical incident, or every shown monitor down. Degraded is
/// any monitor down or any other open incident. Maintenance is a window in
/// progress with nothing worse. Unknown is monitors that have never been
/// checked, which is said rather than dressed up as operational.
fn overall_state(states: &[&str], active: &[StatusIncident]) -> &'static str {
    let incidents: Vec<&StatusIncident> = active.iter().filter(|i| i.kind == "incident").collect();
    let down = states.iter().filter(|s| **s == "down").count();
    if incidents.iter().any(|i| i.impact == "critical") || (down > 0 && down == states.len()) {
        return "outage";
    }
    if down > 0 || incidents.iter().any(|i| i.impact != "none") {
        return "degraded";
    }
    if active
        .iter()
        .any(|i| i.kind == "maintenance" && i.status == "in_progress")
    {
        return "maintenance";
    }
    if !states.is_empty() && states.iter().all(|s| *s == "unknown") {
        return "unknown";
    }
    "operational"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(v: Value) -> PageBody {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn slugs_are_short_lowercase_words() {
        assert!(check_slug("acme-status").is_ok());
        assert!(check_slug("abc").is_ok());
        for bad in [
            "ab",
            "-acme",
            "acme-",
            "Acme",
            "acme status",
            "acme_status",
            &"a".repeat(65),
        ] {
            assert!(check_slug(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_page_is_validated_and_completed() {
        let monitor = Uuid::now_v7();
        let f = page_fields(
            &body(json!({
                "slug": " Acme ",
                "title": "Acme status",
                "monitors": [{ "monitor_id": monitor, "display_name": "  " }]
            })),
            &[monitor],
        )
        .unwrap();
        assert_eq!(f.slug, "acme");
        assert_eq!(f.accent_color, "#2f6bff");
        assert_eq!(f.monitors, vec![(monitor, None)]);
        assert!(f.published);
    }

    #[test]
    fn a_page_cannot_show_another_sites_monitor_or_a_script_link() {
        let mine = Uuid::now_v7();
        let theirs = Uuid::now_v7();
        let e = page_fields(
            &body(json!({ "slug": "acme", "title": "x", "monitors": [{ "monitor_id": theirs }] })),
            &[mine],
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("monitors"));

        let e = page_fields(
            &body(
                json!({ "slug": "acme", "title": "x", "logo_url": "http://cdn.example.com/a.png" }),
            ),
            &[],
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("logo_url"));

        let e = page_fields(
            &body(json!({
                "slug": "acme", "title": "x",
                "footer_label": "Home", "footer_url": "javascript:alert(1)"
            })),
            &[],
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("footer_url"));

        let e = page_fields(
            &body(json!({ "slug": "acme", "title": "x", "footer_label": "Home" })),
            &[],
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("footer_url"));

        let e = page_fields(
            &body(json!({ "slug": "acme", "title": "x", "accent_color": "blue" })),
            &[],
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("accent_color"));
    }

    #[test]
    fn statuses_belong_to_their_kind() {
        assert!(check_status("incident", "identified").is_ok());
        assert!(check_status("maintenance", "in_progress").is_ok());
        assert!(check_status("incident", "scheduled").is_err());
        assert!(check_status("maintenance", "resolved").is_err());
        assert_eq!(
            check_status("outage", "resolved").unwrap_err().field(),
            Some("kind")
        );
    }

    fn incident(kind: &str, status: &str, impact: &str) -> StatusIncident {
        StatusIncident {
            id: Uuid::nil(),
            page_id: Uuid::nil(),
            kind: kind.into(),
            title: "t".into(),
            status: status.into(),
            impact: impact.into(),
            uptime_incident_id: None,
            automatic: false,
            monitor_ids: json!([]),
            scheduled_start: None,
            scheduled_end: None,
            created_at: Utc::now(),
            resolved_at: None,
        }
    }

    #[test]
    fn the_overall_state_says_the_worst_thing_true() {
        assert_eq!(overall_state(&["up", "up"], &[]), "operational");
        assert_eq!(overall_state(&[], &[]), "operational");
        assert_eq!(overall_state(&["unknown"], &[]), "unknown");
        assert_eq!(overall_state(&["up", "down"], &[]), "degraded");
        assert_eq!(overall_state(&["down", "down"], &[]), "outage");
        assert_eq!(
            overall_state(
                &["up"],
                &[incident("incident", "investigating", "critical")]
            ),
            "outage"
        );
        assert_eq!(
            overall_state(&["up"], &[incident("incident", "monitoring", "minor")]),
            "degraded"
        );
        assert_eq!(
            overall_state(&["up"], &[incident("maintenance", "in_progress", "minor")]),
            "maintenance"
        );
    }

    #[test]
    fn the_daily_bars_cover_every_day_and_add_up() {
        let first = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let mut counts = HashMap::new();
        counts.insert("2026-10-02".to_string(), (288u64, 3u64));
        let (bars, checks, failures) = daily_bars(first, 3, Some(&counts));
        assert_eq!(bars.len(), 3);
        assert_eq!(bars[0]["date"], "2026-10-01");
        assert_eq!(bars[0]["checks"], 0);
        assert_eq!(bars[1]["failures"], 3);
        assert_eq!((checks, failures), (288, 3));
    }
}
