//! Profiles: the people a site identified, found by any trait and read in full.
//!
//! `GET /api/stats/profiles?q=` finds the people whose id or traits contain the
//! text (an email address, a company); `GET /api/stats/profiles/:id` returns one
//! person's traits, the history of every change to them, the daily visitor
//! hashes the site linked to the id, and the anonymous sessions recorded under
//! those hashes before the site said who it was.
//!
//! All of it exists only because the site called `identify()` or `setTraits()`:
//! Micaforge itself identifies nobody (`micaforge_query::stats::profile`). Like
//! a visitor record, a profile is addressed by its id rather than by a window,
//! so both reads widen the envelope to the site's whole retained history; the
//! filters and the site still apply.
//!
//! Profiles hold personal data a site chose to send, so they are `viewer`
//! reads like the visitor list, never open to a share link, and an API key
//! needs `stats:read`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use micaforge_core::Error;
use micaforge_query::stats;

use super::agents::{json_body, same_site};
use super::people::whole_history;
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::response::{ok, ok_meta};
use crate::state::AppState;

/// Longest search text and longest id read. An identify id is at most 128
/// characters on the row; a search longer than that cannot match one.
const MAX_TEXT: usize = 128;

/// The two reads.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/profiles", get(search))
        .route("/api/stats/profiles/{id}", get(detail))
}

/// `?q=`, the search text.
#[derive(Debug, Default, Deserialize)]
struct SearchParams {
    q: Option<String>,
}

/// The search text, trimmed and checked, or the reason it cannot be one.
fn clean_query(raw: Option<&str>) -> Result<String, Error> {
    let q = raw.map(str::trim).unwrap_or_default();
    let chars = q.chars().count();
    if chars < stats::MIN_QUERY {
        return Err(Error::validation(
            "q",
            format!("type at least {} characters", stats::MIN_QUERY),
        ));
    }
    if chars > MAX_TEXT {
        return Err(Error::validation(
            "q",
            format!("search for at most {MAX_TEXT} characters"),
        ));
    }
    Ok(q.to_string())
}

/// `GET /api/stats/profiles?q=` — people whose id or traits contain `q`.
async fn search(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let q = clean_query(params.q.as_deref())?;
    let env = whole_history(&env, st.cfg.retention_days);
    let hits = stats::search_profiles(&st.ch, &env, &q).await?;
    let meta = json!({ "rows": hits.len() });
    Ok(ok_meta(json!({ "rows": json_body(&hits)? }), meta))
}

/// `GET /api/stats/profiles/:id` — one person, as the site's calls built them.
///
/// `data` is `null` for an id the site never identified, so the visitor screen
/// can ask for every visitor and show the profile panel only where one exists.
async fn detail(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let id = id.trim();
    if id.is_empty() || id.chars().count() > MAX_TEXT {
        return Err(Error::validation("id", "name the person to read").into());
    }
    let env = whole_history(&env, st.cfg.retention_days);
    match stats::profile(&st.ch, &env, id).await? {
        Some(profile) => Ok(ok(json_body(&profile)?)),
        None => Ok(ok(Value::Null)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_search_is_long_enough_to_mean_something_and_no_longer_than_an_id() {
        assert!(clean_query(None).is_err());
        assert!(clean_query(Some(" a ")).is_err());
        assert_eq!(
            clean_query(Some("  ann@example.com ")).unwrap(),
            "ann@example.com"
        );
        assert!(clean_query(Some(&"x".repeat(MAX_TEXT + 1))).is_err());
    }
}
