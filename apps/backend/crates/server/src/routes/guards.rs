//! `/api/sites/{site}/ingest-guards`: the settings screen for a site's address
//! allow and deny lists, its allowed hostnames, and what the guards blocked.
//!
//! The rules themselves, and the check every ingest path runs, are in
//! [`crate::ingest_guard`]. This module only reads and writes the settings,
//! validating every entry, and reads the blocked counts back for the last
//! week.
//!
//! The allowed hostnames are the same `settings.domains` the site settings
//! screen has always written; they live on this screen too because they are
//! the other half of the same question (what may write into this site), and
//! a hit refused for its hostname is counted here beside the address blocks.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use micaforge_core::{Error, Result};
use micaforge_store::models::sites::{self, Site};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::audit::{self, Entry};
use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::ingest_guard::{self, Blocked, SETTINGS_KEY};
use crate::response::ok;
use crate::routes::orgs::ADMIN;
use crate::routes::sites::{authorize_site_ref, clean_domain};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Most extra hostnames a site may allow (the ingest path reads 32).
const MAX_DOMAINS: usize = 32;

/// Days of blocked counts the screen shows.
const DAYS_SHOWN: u32 = 7;

/// Highest alert threshold accepted: a billion blocked requests a day is not a
/// threshold, it is a typo.
const MAX_THRESHOLD: u64 = 1_000_000_000;

/// The guard routes.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/sites/{site}/ingest-guards",
        get(read_guards).patch(write_guards),
    )
}

/// `GET /api/sites/{site}/ingest-guards`
async fn read_guards(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    Ok(ok(guards_json(&st, &site).await))
}

/// What the screen sends. Absent means unchanged.
#[derive(Debug, Deserialize)]
struct GuardsPatch {
    #[serde(default)]
    allow: Option<Value>,
    #[serde(default)]
    deny: Option<Value>,
    #[serde(default)]
    alert_threshold: Option<u64>,
    #[serde(default)]
    domains: Option<Value>,
}

/// `PATCH /api/sites/{site}/ingest-guards`
///
/// Takes effect on every ingest path within a minute, the life of the ingest
/// path's cached site.
async fn write_guards(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
    Json(body): Json<GuardsPatch>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let before = settings_view(&site.settings);
    let settings = apply(&site, body)?;
    let updated = sites::update_settings(&st.pg, site.id, &settings).await?;

    audit::record(
        &st,
        &auth,
        Entry::new("site.guards_updated")
            .site(&updated)
            .before(before)
            .after(settings_view(&updated.settings)),
    )
    .await;
    Ok(ok(guards_json(&st, &updated).await))
}

/// The settings document with a patch applied, every entry validated.
fn apply(site: &Site, body: GuardsPatch) -> Result<Value> {
    let mut settings = match &site.settings {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    let current = settings
        .get(SETTINGS_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let allow = match &body.allow {
        Some(raw) => ingest_guard::clean_list(raw, "allow")?,
        None => current.get("allow").cloned().unwrap_or_else(|| json!([])),
    };
    let deny = match &body.deny {
        Some(raw) => ingest_guard::clean_list(raw, "deny")?,
        None => current.get("deny").cloned().unwrap_or_else(|| json!([])),
    };
    let threshold = match body.alert_threshold {
        Some(n) if n > MAX_THRESHOLD => {
            return Err(Error::validation(
                "alert_threshold",
                format!("must be at most {MAX_THRESHOLD}"),
            ))
        }
        Some(n) => n,
        None => current
            .get("alert_threshold")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    };
    settings.insert(
        SETTINGS_KEY.to_string(),
        json!({ "allow": allow, "deny": deny, "alert_threshold": threshold }),
    );

    if let Some(raw) = &body.domains {
        settings.insert("domains".to_string(), clean_domains(raw, &site.domain)?);
    }
    Ok(Value::Object(settings))
}

/// Validate the extra hostnames: real hostnames, no repeats, not the primary.
fn clean_domains(raw: &Value, primary: &str) -> Result<Value> {
    let list = match raw {
        Value::Null => return Ok(json!([])),
        Value::Array(list) => list,
        _ => return Err(Error::validation("domains", "must be a list of hostnames")),
    };
    if list.len() > MAX_DOMAINS {
        return Err(Error::validation(
            "domains",
            format!("a site can allow at most {MAX_DOMAINS} extra hostnames"),
        ));
    }
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for entry in list {
        let host = entry
            .as_str()
            .ok_or_else(|| Error::validation("domains", "every entry must be a hostname"))
            .and_then(|s| {
                clean_domain(s).map_err(|_| {
                    Error::validation(
                        "domains",
                        format!("`{}` is not a hostname such as docs.example.com", s.trim()),
                    )
                })
            })?;
        if host != primary && !out.contains(&host) {
            out.push(host);
        }
    }
    Ok(json!(out))
}

/// The part of the settings this screen owns, for the audit log.
fn settings_view(settings: &Value) -> Value {
    json!({
        SETTINGS_KEY: settings.get(SETTINGS_KEY).cloned().unwrap_or(Value::Null),
        "domains": settings.get("domains").cloned().unwrap_or_else(|| json!([])),
    })
}

/// The screen's whole answer.
async fn guards_json(st: &AppState, site: &Site) -> Value {
    let section = site.settings.get(SETTINGS_KEY);
    let list = |name: &str| {
        section
            .and_then(|s| s.get(name))
            .cloned()
            .unwrap_or_else(|| json!([]))
    };
    let days = ingest_guard::blocked_days(st, site.site_id_u16(), DAYS_SHOWN).await;
    let mut totals = [0u64; 3];
    let days_json: Vec<Value> = days
        .iter()
        .map(|d| {
            for (sum, count) in totals.iter_mut().zip(d.counts) {
                *sum += count;
            }
            let mut row = Map::new();
            row.insert("day".into(), json!(d.day));
            for (reason, count) in Blocked::ALL.iter().zip(d.counts) {
                row.insert(reason.as_str().into(), json!(count));
            }
            Value::Object(row)
        })
        .collect();
    let mut totals_json = Map::new();
    for (reason, count) in Blocked::ALL.iter().zip(totals) {
        totals_json.insert(reason.as_str().into(), json!(count));
    }

    json!({
        "allow": list("allow"),
        "deny": list("deny"),
        "alert_threshold": ingest_guard::alert_threshold(&site.settings),
        "primary_domain": site.domain,
        "domains": site.settings.get("domains").cloned().unwrap_or_else(|| json!([])),
        "blocked": { "days": days_json, "totals": totals_json },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn site(settings: Value) -> Site {
        Site {
            id: Uuid::nil(),
            site_id: 3,
            org_id: Uuid::nil(),
            domain: "example.com".into(),
            name: "Example".into(),
            timezone: "UTC".into(),
            public: false,
            salt_rotated_at: None,
            created_at: Utc::now(),
            settings,
        }
    }

    fn patch(raw: Value) -> GuardsPatch {
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn a_patch_keeps_what_it_does_not_name_and_the_rest_of_the_settings() {
        let stored = site(json!({
            "ingest_key_sealed": "sealed",
            "domains": ["docs.example.com"],
            SETTINGS_KEY: { "allow": [], "deny": [{ "cidr": "10.0.0.0/8", "note": "" }], "alert_threshold": 50 }
        }));
        let next = apply(&stored, patch(json!({ "allow": ["192.0.2.0/24"] }))).unwrap();
        assert_eq!(next["ingest_key_sealed"], json!("sealed"));
        assert_eq!(next["domains"], json!(["docs.example.com"]));
        assert_eq!(next[SETTINGS_KEY]["deny"][0]["cidr"], json!("10.0.0.0/8"));
        assert_eq!(
            next[SETTINGS_KEY]["allow"][0]["cidr"],
            json!("192.0.2.0/24")
        );
        assert_eq!(next[SETTINGS_KEY]["alert_threshold"], json!(50));
    }

    #[test]
    fn hostnames_are_cleaned_and_the_primary_is_not_repeated() {
        let next = apply(
            &site(json!({})),
            patch(json!({ "domains": ["https://Docs.Example.com/", "example.com", "docs.example.com"] })),
        )
        .unwrap();
        assert_eq!(next["domains"], json!(["docs.example.com"]));

        let refused = apply(&site(json!({})), patch(json!({ "domains": ["localhost"] })));
        assert_eq!(refused.unwrap_err().field(), Some("domains"));
    }

    #[test]
    fn a_bad_entry_or_threshold_is_refused_by_field() {
        let s = site(json!({}));
        assert_eq!(
            apply(&s, patch(json!({ "deny": ["nope"] })))
                .unwrap_err()
                .field(),
            Some("deny")
        );
        assert_eq!(
            apply(&s, patch(json!({ "alert_threshold": MAX_THRESHOLD + 1 })))
                .unwrap_err()
                .field(),
            Some("alert_threshold")
        );
    }
}
