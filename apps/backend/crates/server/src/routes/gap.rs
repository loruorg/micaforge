//! The Citation Gap, and the decay report that follows from it.
//!
//! `PRODUCT.md` names one signature metric, and this is the module that serves
//! it: for every URL, how much AI agents crawled it beside how many people
//! arrived at it from an answer engine. Plausible cannot report it, not because
//! the arithmetic is hard but because it does not keep the two facts in the
//! same place.
//!
//! The engine is [`micaforge_query::gap`]; these handlers resolve the envelope
//! and wrap the result. What they add is the **meta**, and it is not decoration.
//! Every verdict on the wire — `extracted`, `compounding`, `invisible`, `quiet`
//! — is a product judgement made against three constants, so those constants
//! travel with the answer. A reader who disagrees with where the line was drawn
//! can see exactly where it was drawn, and every row carries the two raw numbers
//! the judgement was made from.
//!
//! ## Three endpoints, one universe of URLs
//!
//! All three are built over a union of the content register, the agent trace and
//! the human trace, never a join from one of them. A page that agents crawl and
//! nobody reads has to appear, and so does a page that exists and nothing has
//! touched at all. Those absences are the finding.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use micaforge_query::gap;
use serde_json::{json, Value};

use super::agents::{json_body, same_site};
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess, SiteOrShare};
use crate::response::ok_meta;
use crate::state::AppState;

/// The Citation Gap and content decay.
///
/// Two prefixes in one router — `/api/citation-gap` and `/api/content/decay` —
/// which is why every router in this group is written with absolute paths and
/// merged at the root rather than nested.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/citation-gap", get(by_page))
        .route("/api/citation-gap/summary", get(summary))
        .route("/api/content/decay", get(decay))
}

/// The lines the verdicts are drawn at, echoed with every answer.
///
/// A number a reader cannot interrogate is a number they have to take on faith,
/// and this product's whole claim is that it does not ask for faith. Sending the
/// thresholds costs three integers and makes `extracted` an argument rather than
/// a pronouncement.
fn thresholds() -> Value {
    json!({
        "heavy_crawl_fetches": gap::HEAVY_CRAWL_FETCHES,
        "cited_arrivals": gap::CITED_ARRIVALS,
        "extracted_ratio": gap::EXTRACTED_RATIO,
    })
}

// ---------------------------------------------------------------------------
// GET /api/citation-gap
// ---------------------------------------------------------------------------

/// Per URL: agent fetches, answer-engine arrivals, the ratio, the verdict.
async fn by_page(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let rows = gap::by_page(&st.ch, &env).await?;

    // Counted over the page that was returned, and labelled as such: the
    // site-level counts are `/summary`'s job and are measured over every URL.
    let mut counts = gap::VerdictCounts::default();
    for row in &rows {
        match row.verdict {
            gap::Verdict::Extracted => counts.extracted += 1,
            gap::Verdict::Compounding => counts.compounding += 1,
            gap::Verdict::Invisible => counts.invisible += 1,
            gap::Verdict::Quiet => counts.quiet += 1,
        }
    }

    let meta = json!({
        "rows": rows.len(),
        "verdict_counts_on_page": json_body(&counts)?,
        "thresholds": thresholds(),
    });

    Ok(ok_meta(json_body(&rows)?, meta))
}

// ---------------------------------------------------------------------------
// GET /api/citation-gap/summary
// ---------------------------------------------------------------------------

/// The site-level gap: taken, returned, the ratio, the trend, the verdict split.
async fn summary(
    State(st): State<AppState>,
    SiteOrShare(access): SiteOrShare,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let report = gap::summary(&st.ch, &env).await?;

    let meta = json!({
        "rows": report.trend.len(),
        "granularity": env.granularity.as_str(),
        "thresholds": thresholds(),
    });

    Ok(ok_meta(json_body(&report)?, meta))
}

// ---------------------------------------------------------------------------
// GET /api/content/decay
// ---------------------------------------------------------------------------

/// Pages whose human traffic fell while the machines kept reading them.
///
/// The comparison is always against the immediately preceding window of the same
/// length, whatever the envelope's `compare` says. Decay is defined as a change
/// over time, so "compare: none" would leave the report with nothing to measure;
/// an endpoint that answered an empty list to that would look like "no decay"
/// rather than "no comparison", and those are different sentences.
async fn decay(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let rows = gap::decay(&st.ch, &env).await?;

    let meta = json!({
        "rows": rows.len(),
        "compared_to": "the preceding window of the same length",
        "window_start": env.range.start,
        "window_end": env.range.end,
    });

    Ok(ok_meta(json_body(&rows)?, meta))
}
