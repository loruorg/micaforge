//! `/api/annotations` — notes people write on a site's charts.
//!
//! Reading is a `viewer`'s right, like every chart on the site. Writing,
//! editing and deleting need `member`, the role that may change what a site
//! measures; an API key needs `manage:write` for the same reason. The MCP
//! tools call [`create`] and [`delete`] here rather than the repository, so an
//! annotation written by an agent passes the same checks as one written in the
//! dashboard.

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use micaforge_core::{new_id, Error, Result};
use micaforge_store::models::annotations::{self, Annotation, Fields};
use micaforge_store::models::sites::Site;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthMethod, AuthUser, Envelope, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The colours an annotation may take. Never blue (people) or orange (AI).
pub const COLORS: &[&str] = &["neutral", "green", "red"];
/// Longest note, in characters.
const MAX_TEXT: usize = 280;
/// Longest category, in characters.
const MAX_CATEGORY: usize = 40;
/// Most annotations one chart is sent.
const MAX_LISTED: i64 = 500;

/// Every annotation route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/annotations", get(list).post(create_route))
        .route(
            "/api/annotations/{id}",
            patch(update_route).delete(delete_route),
        )
}

/// An annotation as a create or an edit sends it. Every field is optional so
/// an edit can send only what changed; a create checks the required ones.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Input {
    /// ISO-8601 instant, or a bare date read as midnight in the site's zone.
    #[serde(default)]
    pub starts_at: Option<String>,
    /// The end of a range. `null` clears it on an edit.
    #[serde(default, deserialize_with = "present")]
    pub ends_at: Option<Option<String>>,
    /// The note.
    #[serde(default)]
    pub text: Option<String>,
    /// `neutral` | `green` | `red`.
    #[serde(default)]
    pub color: Option<String>,
    /// A free label. `null` clears it on an edit.
    #[serde(default, deserialize_with = "present")]
    pub category: Option<Option<String>>,
}

/// Tell an absent field from an explicit `null`.
fn present<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

/// Read an instant: RFC 3339, or a date at midnight in the site's zone.
pub fn parse_instant(field: &str, raw: &str, tz: Tz) -> Result<DateTime<Utc>> {
    let raw = raw.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&Utc));
    }
    if let Ok(day) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        if let Some(local) = day
            .and_hms_opt(0, 0, 0)
            .and_then(|t| t.and_local_timezone(tz).earliest())
        {
            return Ok(local.with_timezone(&Utc));
        }
    }
    Err(Error::validation(
        field,
        "must be an ISO-8601 date or timestamp",
    ))
}

/// The fields an input settles to, over `current` for an edit.
pub fn settle(site: &Site, input: &Input, current: Option<&Annotation>) -> Result<OwnedFields> {
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let starts_at = match (input.starts_at.as_deref(), current) {
        (Some(raw), _) => parse_instant("starts_at", raw, tz)?,
        (None, Some(a)) => a.starts_at,
        (None, None) => return Err(Error::validation("starts_at", "is required")),
    };
    let ends_at = match (&input.ends_at, current) {
        (Some(Some(raw)), _) if !raw.trim().is_empty() => Some(parse_instant("ends_at", raw, tz)?),
        (Some(_), _) => None,
        (None, Some(a)) => a.ends_at,
        (None, None) => None,
    };
    if ends_at.is_some_and(|end| end < starts_at) {
        return Err(Error::validation("ends_at", "must not be before starts_at"));
    }
    let text = match (input.text.as_deref(), current) {
        (Some(raw), _) => raw.trim().to_string(),
        (None, Some(a)) => a.text.clone(),
        (None, None) => String::new(),
    };
    let count = text.chars().count();
    if count == 0 || count > MAX_TEXT {
        return Err(Error::validation(
            "text",
            format!("must be 1 to {MAX_TEXT} characters"),
        ));
    }
    let color = match (input.color.as_deref(), current) {
        (Some(raw), _) => {
            let wanted = raw.trim().to_lowercase();
            COLORS
                .iter()
                .find(|c| **c == wanted)
                .map(|c| (*c).to_string())
                .ok_or_else(|| {
                    Error::validation("color", format!("must be one of {}", COLORS.join(", ")))
                })?
        }
        (None, Some(a)) => a.color.clone(),
        (None, None) => "neutral".to_string(),
    };
    let category = match (&input.category, current) {
        (Some(Some(raw)), _) if !raw.trim().is_empty() => {
            let trimmed = raw.trim();
            if trimmed.chars().count() > MAX_CATEGORY {
                return Err(Error::validation(
                    "category",
                    format!("must be at most {MAX_CATEGORY} characters"),
                ));
            }
            Some(trimmed.to_string())
        }
        (Some(_), _) => None,
        (None, Some(a)) => a.category.clone(),
        (None, None) => None,
    };
    Ok(OwnedFields {
        starts_at,
        ends_at,
        text,
        color,
        category,
    })
}

/// [`Fields`], owned, so it can outlive the input it was built from.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedFields {
    starts_at: DateTime<Utc>,
    ends_at: Option<DateTime<Utc>>,
    text: String,
    color: String,
    category: Option<String>,
}

impl OwnedFields {
    fn borrow(&self) -> Fields<'_> {
        Fields {
            starts_at: self.starts_at,
            ends_at: self.ends_at,
            text: &self.text,
            color: &self.color,
            category: self.category.as_deref(),
        }
    }
}

/// Write an annotation on a site the caller has already been checked against.
///
/// `source` is `dashboard`, `api` or `mcp`. The author is the person for a
/// session or an OAuth token, and nobody for an API key: a key is not its
/// owner.
pub async fn create(
    st: &AppState,
    site: &Site,
    auth: &AuthUser,
    input: &Input,
    source: &str,
) -> Result<Annotation> {
    let fields = settle(site, input, None)?;
    let author = match auth.method {
        AuthMethod::ApiKey(_) => None,
        AuthMethod::Session(_) | AuthMethod::OAuth(_) => Some(auth.user.id),
    };
    annotations::create(&st.pg, new_id(), site.id, &fields.borrow(), source, author).await
}

/// Delete an annotation the caller may change; returns its site.
pub async fn delete(st: &AppState, auth: &AuthUser, id: Uuid) -> Result<Site> {
    let note = annotations::get(&st.pg, id).await?;
    let site = reachable(st, note.site_id, auth.user.id, MEMBER).await?;
    annotations::delete(&st.pg, note.id).await?;
    Ok(site)
}

/// The site of an annotation, if the caller holds `needed` on it.
async fn reachable(st: &AppState, site_id: Uuid, user: Uuid, needed: &str) -> Result<Site> {
    match authorize_site(&st.pg, site_id, user, needed).await {
        Ok(site) => Ok(site),
        Err(Error::NotFound(_)) => Err(Error::not_found("annotation")),
        Err(e) => Err(e),
    }
}

/// The source a request writes under.
fn source_of(auth: &AuthUser) -> &'static str {
    match auth.method {
        AuthMethod::Session(_) => "dashboard",
        AuthMethod::ApiKey(_) => "api",
        AuthMethod::OAuth(_) => "mcp",
    }
}

/// `GET /api/annotations?site=…&range=…`
async fn list(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = annotations::list_in(
        &st.pg,
        access.site.id,
        env.range.start,
        env.range.end,
        MAX_LISTED,
    )
    .await?;
    Ok(ok(rows
        .iter()
        .map(|a| annotation_json(a, access.site_id))
        .collect::<Vec<_>>()))
}

/// `POST /api/annotations?site=…`
async fn create_route(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(input): Json<Input>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let note = create(&st, &access.site, &auth, &input, source_of(&auth)).await?;
    Ok(ok(annotation_json(&note, access.site_id)))
}

/// `PATCH /api/annotations/{id}`
async fn update_route(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(input): Json<Input>,
) -> ApiResult<Json<Value>> {
    let note = annotations::get(&st.pg, id).await?;
    let site = reachable(&st, note.site_id, auth.user.id, MEMBER).await?;
    let fields = settle(&site, &input, Some(&note))?;
    let updated = annotations::update(&st.pg, note.id, &fields.borrow()).await?;
    Ok(ok(annotation_json(&updated, site.site_id_u16())))
}

/// `DELETE /api/annotations/{id}`
async fn delete_route(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    delete(&st, &auth, id).await?;
    Ok(ok(json!({ "deleted": id })))
}

/// One annotation on the wire.
pub fn annotation_json(a: &Annotation, site: u16) -> Value {
    json!({
        "id": a.id,
        "site": site,
        "starts_at": a.starts_at,
        "ends_at": a.ends_at,
        "text": a.text,
        "color": a.color,
        "category": a.category,
        "source": a.source,
        "url": a.url,
        "author": a.author_id.map(|id| json!({ "user_id": id, "name": a.author_name })),
        "created_at": a.created_at,
        "updated_at": a.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> Site {
        Site {
            id: Uuid::from_u128(1),
            site_id: 3,
            org_id: Uuid::from_u128(2),
            domain: "example.com".into(),
            name: "Example".into(),
            timezone: "Europe/Lisbon".into(),
            public: false,
            salt_rotated_at: None,
            created_at: Utc::now(),
            settings: json!({}),
        }
    }

    #[test]
    fn a_bare_date_is_midnight_in_the_sites_zone() {
        let tz: Tz = "Europe/Lisbon".parse().unwrap();
        let at = parse_instant("starts_at", "2026-07-01", tz).unwrap();
        // Lisbon is UTC+1 in July.
        assert_eq!(at.to_rfc3339(), "2026-06-30T23:00:00+00:00");
        assert!(parse_instant("starts_at", "yesterday", tz).is_err());
        assert_eq!(
            parse_instant("starts_at", "2026-07-01T10:00:00Z", tz)
                .unwrap()
                .to_rfc3339(),
            "2026-07-01T10:00:00+00:00"
        );
    }

    #[test]
    fn a_create_needs_a_start_and_a_text_and_takes_safe_colours() {
        let s = site();
        let ok = settle(
            &s,
            &Input {
                starts_at: Some("2026-07-01T10:00:00Z".into()),
                text: Some(" Launched pricing ".into()),
                ..Input::default()
            },
            None,
        )
        .unwrap();
        assert_eq!(ok.text, "Launched pricing");
        assert_eq!(ok.color, "neutral");

        assert_eq!(
            settle(
                &s,
                &Input {
                    text: Some("x".into()),
                    ..Input::default()
                },
                None
            )
            .unwrap_err()
            .field(),
            Some("starts_at")
        );
        let blue = Input {
            starts_at: Some("2026-07-01".into()),
            text: Some("x".into()),
            color: Some("blue".into()),
            ..Input::default()
        };
        assert_eq!(settle(&s, &blue, None).unwrap_err().field(), Some("color"));
        let backwards = Input {
            starts_at: Some("2026-07-02".into()),
            ends_at: Some(Some("2026-07-01".into())),
            text: Some("x".into()),
            ..Input::default()
        };
        assert_eq!(
            settle(&s, &backwards, None).unwrap_err().field(),
            Some("ends_at")
        );
    }

    #[test]
    fn an_edit_keeps_what_it_does_not_send_and_clears_what_it_nulls() {
        let s = site();
        let current = Annotation {
            id: Uuid::from_u128(9),
            site_id: s.id,
            starts_at: Utc::now(),
            ends_at: Some(Utc::now() + chrono::Duration::hours(2)),
            text: "Outage".into(),
            color: "red".into(),
            category: Some("incident".into()),
            source: "dashboard".into(),
            url: None,
            author_id: None,
            author_name: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let edit: Input =
            serde_json::from_value(json!({ "text": "Outage, resolved", "ends_at": null })).unwrap();
        let settled = settle(&s, &edit, Some(&current)).unwrap();
        assert_eq!(settled.text, "Outage, resolved");
        assert_eq!(settled.ends_at, None, "an explicit null clears the range");
        assert_eq!(settled.color, "red");
        assert_eq!(settled.category.as_deref(), Some("incident"));
    }
}
