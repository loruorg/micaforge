//! `/api/audit/*`: reading an organisation's audit log.
//!
//! The log is written by [`crate::audit::record`] from every mutating handler
//! that matters; this module only reads it back, as pages for the Organisation
//! screen and as a CSV for whoever has to answer "who changed this, and when".
//!
//! ## Who may read it
//!
//! A person needs the `admin` role in the organisation: the log names every
//! member, every key and every change, which is operational detail a `viewer`
//! has no claim to. An API key needs the `audit:read` scope and a grant on the
//! organisation itself (a site-bound key reaches no organisation-level object).
//! The scope, not a role, is what authorises a key here, because keys are
//! capped below `admin` everywhere else on purpose; a key minted to feed a
//! security tool the log should read the log and nothing more.

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use micaforge_store::models::audit::{self as audit_model, AuditEvent, AuditQuery};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::audit::ACTIONS;
use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::orgs::{authorize_org, ADMIN};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The scope an API key needs to read the log.
pub const AUDIT_SCOPE: &str = "audit:read";

/// Default page size on the screen.
const PAGE: i64 = 50;

/// Largest page.
const MAX_PAGE: i64 = 200;

/// Most rows one CSV export carries.
const MAX_EXPORT: usize = 50_000;

/// Rows read per round trip while exporting.
const EXPORT_BATCH: i64 = 1_000;

/// Every audit route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/audit", get(list))
        .route("/api/audit/actions", get(actions))
        .route("/api/audit/export.csv", get(export))
}

/// The filters, as the query string carries them.
#[derive(Debug, Default, Deserialize)]
struct Filters {
    org: Uuid,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    target_type: Option<String>,
    #[serde(default)]
    target_id: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    before: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

impl Filters {
    /// The repository query these filters ask for, each one validated.
    fn query(&self, limit: i64) -> Result<AuditQuery> {
        let text = |raw: &Option<String>| -> Option<String> {
            raw.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let uuid = |raw: &Option<String>, field: &'static str| -> Result<Option<Uuid>> {
            match text(raw) {
                None => Ok(None),
                Some(s) => s
                    .parse::<Uuid>()
                    .map(Some)
                    .map_err(|_| Error::validation(field, "must be an id")),
            }
        };
        let time = |raw: &Option<String>, field: &'static str| -> Result<Option<DateTime<Utc>>> {
            match text(raw) {
                None => Ok(None),
                Some(s) => DateTime::parse_from_rfc3339(&s)
                    .map(|t| Some(t.with_timezone(&Utc)))
                    .map_err(|_| Error::validation(field, "must be an ISO-8601 timestamp")),
            }
        };

        let action = text(&self.action);
        if let Some(a) = action.as_deref() {
            let usable = a.len() <= 64
                && a.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_');
            if !usable {
                return Err(Error::validation(
                    "action",
                    "must be an action name, or a prefix ending in a dot",
                ));
            }
        }

        Ok(AuditQuery {
            org_id: self.org,
            actor_id: uuid(&self.actor, "actor")?,
            action,
            target_type: text(&self.target_type),
            target_id: text(&self.target_id),
            from: time(&self.from, "from")?,
            to: time(&self.to, "to")?,
            before: uuid(&self.before, "before")?,
            limit,
        })
    }
}

/// Refuse anybody who may not read this organisation's log.
async fn authorize(st: &AppState, auth: &AuthUser, org: Uuid) -> Result<()> {
    match auth.key.as_ref() {
        Some(grant) => {
            if !grant.permits_org(org) {
                return Err(Error::not_found("organization"));
            }
            auth.require_scope(AUDIT_SCOPE)
        }
        None => authorize_org(&st.pg, org, auth.user.id, ADMIN)
            .await
            .map(|_| ()),
    }
}

/// `GET /api/audit?org=…` — one page, newest first.
async fn list(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(filters): Query<Filters>,
) -> ApiResult<Json<Value>> {
    authorize(&st, &auth, filters.org).await?;
    let limit = filters.limit.unwrap_or(PAGE).clamp(1, MAX_PAGE);
    let rows = audit_model::list(&st.pg, &filters.query(limit)?).await?;
    let next = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| r.id))
        .flatten();
    Ok(ok(json!({
        "events": rows.iter().map(event_json).collect::<Vec<_>>(),
        "next": next,
    })))
}

/// `GET /api/audit/actions` — every action the server records, for the
/// screen's filter.
async fn actions(_auth: AuthUser) -> ApiResult<Json<Value>> {
    Ok(ok(ACTIONS
        .iter()
        .map(|(action, label)| json!({ "action": action, "label": label }))
        .collect::<Vec<_>>()))
}

/// One row of the CSV. Every column is text, so the export never refuses a
/// row for the shape of its JSON.
#[derive(Debug, Serialize)]
struct CsvRow {
    time: String,
    actor_kind: String,
    actor_id: String,
    actor: String,
    action: String,
    target_type: String,
    target_id: String,
    target: String,
    site_id: String,
    ip_hash: String,
    user_agent: String,
    before: String,
    after: String,
    metadata: String,
}

/// `GET /api/audit/export.csv?org=…` — the filtered log as a spreadsheet.
///
/// Read in pages of [`EXPORT_BATCH`] down the same cursor the screen uses, up
/// to [`MAX_EXPORT`] rows. Cells pass through the query engine's CSV writer,
/// which neutralises anything a spreadsheet would run as a formula: a target
/// label is text an admin typed, and an address is text a stranger chose.
async fn export(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(filters): Query<Filters>,
) -> ApiResult<Response<Body>> {
    authorize(&st, &auth, filters.org).await?;
    let mut query = filters.query(EXPORT_BATCH)?;
    let mut rows: Vec<CsvRow> = Vec::new();
    loop {
        let page = audit_model::list(&st.pg, &query).await?;
        let last = page.last().map(|r| r.id);
        let full = page.len() as i64 == EXPORT_BATCH;
        rows.extend(page.iter().map(csv_row));
        if !full || rows.len() >= MAX_EXPORT {
            break;
        }
        query.before = last;
    }
    rows.truncate(MAX_EXPORT);

    let csv = if rows.is_empty() {
        "time,actor_kind,actor_id,actor,action,target_type,target_id,target,site_id,ip_hash,user_agent,before,after,metadata\n".to_string()
    } else {
        micaforge_query::to_csv(&rows)?
    };

    let mut response = Response::new(Body::from(csv));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    let filename = format!("micaforge-audit-{}.csv", Utc::now().format("%Y%m%d"));
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );
    Ok(response)
}

/// A JSON column as one CSV cell.
fn cell(value: &Option<Value>) -> String {
    value.as_ref().map(Value::to_string).unwrap_or_default()
}

/// One event as a CSV row.
fn csv_row(event: &AuditEvent) -> CsvRow {
    CsvRow {
        time: event.created_at.to_rfc3339(),
        actor_kind: event.actor_kind.clone(),
        actor_id: event.actor_id.map(|id| id.to_string()).unwrap_or_default(),
        actor: event.actor_label.clone(),
        action: event.action.clone(),
        target_type: event.target_type.clone(),
        target_id: event.target_id.clone().unwrap_or_default(),
        target: event.target_label.clone(),
        site_id: event.site_id.map(|id| id.to_string()).unwrap_or_default(),
        ip_hash: event.ip_hash.clone().unwrap_or_default(),
        user_agent: event.user_agent.clone().unwrap_or_default(),
        before: cell(&event.before),
        after: cell(&event.after),
        metadata: cell(&event.metadata),
    }
}

/// One event on the wire. The address digest is shortened to twelve
/// characters: enough to tell two places apart, nothing more.
fn event_json(event: &AuditEvent) -> Value {
    json!({
        "id": event.id,
        "org_id": event.org_id,
        "created_at": event.created_at,
        "actor": {
            "kind": event.actor_kind,
            "id": event.actor_id,
            "label": event.actor_label,
        },
        "action": event.action,
        "target": {
            "type": event.target_type,
            "id": event.target_id,
            "label": event.target_label,
        },
        "site_id": event.site_id,
        "before": event.before,
        "after": event.after,
        "metadata": event.metadata,
        "ip_hash": event.ip_hash.as_deref().map(|h| h.chars().take(12).collect::<String>()),
        "user_agent": event.user_agent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filters(raw: &str) -> Filters {
        let uri: axum::http::Uri = format!("/api/audit?{raw}").parse().unwrap();
        Query::<Filters>::try_from_uri(&uri).unwrap().0
    }

    #[test]
    fn filters_are_validated_into_a_query() {
        let org = Uuid::from_u128(1);
        let q = filters(&format!(
            "org={org}&action=site.&from=2026-10-01T00:00:00Z&actor=&target_type=site"
        ))
        .query(10)
        .unwrap();
        assert_eq!(q.org_id, org);
        assert_eq!(q.action.as_deref(), Some("site."));
        assert!(q.actor_id.is_none(), "an empty filter is no filter");
        assert_eq!(q.target_type.as_deref(), Some("site"));
        assert!(q.from.is_some());

        let bad = [
            format!("org={org}&action=SITE"),
            format!("org={org}&action=site%25"),
            format!("org={org}&actor=someone"),
            format!("org={org}&from=yesterday"),
        ];
        for raw in bad {
            assert!(filters(&raw).query(10).is_err(), "{raw}");
        }
    }

    #[test]
    fn a_csv_row_flattens_every_column_to_text() {
        let event = AuditEvent {
            id: Uuid::from_u128(5),
            org_id: Some(Uuid::from_u128(1)),
            actor_kind: "user".into(),
            actor_id: Some(Uuid::from_u128(7)),
            actor_label: "ada@example.com".into(),
            action: "site.updated".into(),
            target_type: "site".into(),
            target_id: Some("abc".into()),
            target_label: "=HYPERLINK(\"x\")".into(),
            site_id: None,
            before: Some(json!({ "name": "Old" })),
            after: Some(json!({ "name": "New" })),
            metadata: None,
            ip_hash: Some("0123456789abcdef".into()),
            user_agent: None,
            created_at: Utc::now(),
        };
        let csv = micaforge_query::to_csv(&[csv_row(&event)]).unwrap();
        assert!(csv.starts_with("time,actor_kind"));
        // A label a person typed is never run by a spreadsheet.
        assert!(csv.contains("'=HYPERLINK"));
        assert!(csv.contains("\"{\"\"name\"\":\"\"Old\"\"}\""));
        assert_eq!(event_json(&event)["ip_hash"], json!("0123456789ab"));
    }
}
