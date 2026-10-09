//! What the tracker's optional parts report: the frustration signals, and
//! whether a window was recorded from a sample.
//!
//! `GET /api/stats/frustration` reads the rage clicks, dead clicks and abandoned
//! forms the frustration plugin sends. `GET /api/stats/sampling` says whether
//! the pageviews of a window came from a `data-sample` tracker, so a screen can
//! print "sampled" beside numbers that are a sample rather than leave a reader
//! to believe they are a census.
//!
//! Both are ordinary analytics reads: the envelope, `SiteAccess`, and the
//! `stats:read` scope an API key needs for any `GET` under `/api/stats`. Neither
//! is open to a share link.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use micaforge_core::Audience;
use micaforge_query::stats;

use super::agents::{json_body, same_site};
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::response::{ok, ok_meta};
use crate::state::AppState;

/// The two reads.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/frustration", get(frustration))
        .route("/api/stats/sampling", get(sampling))
}

/// `GET /api/stats/frustration` — rage clicks, dead clicks and abandoned forms.
///
/// The rows exist only for people: no crawler clicks. With the audience set to
/// AI crawlers the answer is the empty report and a note saying why, rather
/// than the people's figures under a heading that says otherwise.
async fn frustration(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    if env.audience == Audience::Agent {
        return Ok(ok_meta(
            json_body(&stats::Frustration::default())?,
            json!({ "note": "frustration signals are recorded for people only" }),
        ));
    }
    let report = stats::frustration(&st.ch, &env).await?;
    let meta = json!({
        "rows": report.pages.len() + report.elements.len() + report.forms.len(),
    });
    Ok(ok_meta(json_body(&report)?, meta))
}

/// `GET /api/stats/sampling` — whether the window's pageviews are a sample.
async fn sampling(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    Ok(ok(json_body(&stats::sampling(&st.ch, &env).await?)?))
}
