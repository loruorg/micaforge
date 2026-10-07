//! `detect_decay` — pages losing readers while the machines keep reading.
//!
//! The question `GET /api/content/decay` answers, and one of the few questions
//! that needs both halves of this product to exist at once: **which pages did
//! people stop visiting, while agents carried on taking them?**
//!
//! A page whose human traffic fell and whose crawl traffic fell with it is
//! ordinary content ageing. A page whose human traffic fell while the crawlers
//! held steady is something else: the content is still being consumed, and the
//! consumption has moved somewhere the operator gets nothing back from.
//!
//! # The analysis is not written here
//!
//! [`micaforge_query::gap::decay`] computes it, and this job's whole role is to
//! run it on a schedule and cache the answer. That split is deliberate and it is
//! the same rule the rest of the backend follows: the Citation Gap is the metric
//! the product exists for, and a second implementation of it living in the worker
//! would eventually disagree with the one behind the endpoint. Two numbers for
//! the same question is the one failure an analytics tool cannot recover from.
//!
//! # Weekly, because the answer moves in weeks
//!
//! Decay is a four-week window against the four before it. Computing it hourly
//! would produce the same list a hundred and sixty-eight times, and each run is
//! a scan across two months of two tables.
//!
//! # Where the answer goes
//!
//! Into Redis, under [`decay_cache_key`], with a fortnight's TTL. The endpoint
//! reads that key rather than recomputing, because this is several seconds of
//! ClickHouse work and not something a dashboard request should do. The result is
//! derived and re-computable, which is exactly what Redis is for in this product:
//! a cold cache costs one recomputation and never a wrong number.
//!
//! [`DecayReport::computed_at`] travels with it so the screen can say when the
//! numbers are from. A weekly answer presented without its date reads as live,
//! and is not.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use micaforge_core::{Audience, QueryEnvelope, Result, TimeRange};
use micaforge_store::models::sites;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::JobCtx;

pub use micaforge_query::gap::DecayRow;

/// How long the stored answer survives without being recomputed.
///
/// A fortnight: twice the cadence, so the dashboard still has last week's answer
/// if one run is missed, and nothing older than two weeks is ever presented as
/// current.
const CACHE_TTL_SECS: u32 = 14 * 24 * 60 * 60;

/// Length of each half of the comparison, in days, when the payload does not say.
///
/// Four weeks. Long enough that a quiet fortnight is not a finding, short enough
/// that a page which died in March is not still being reported in August.
pub const DEFAULT_WINDOW_DAYS: i64 = 28;

/// Most decaying pages reported per site.
///
/// A hundred. This is a triage list somebody works through page by page; a
/// thousand rows is an export, and the export endpoint is where that belongs.
pub const MAX_ROWS: u32 = 100;

/// Which sites to examine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One site, by `sites.id`. Absent means every site.
    #[serde(default)]
    pub site: Option<Uuid>,
    /// Override the comparison window. Useful on a low-traffic site, where four
    /// weeks either side may not clear the analysis's own minimum-traffic floor.
    #[serde(default)]
    pub window_days: Option<i64>,
}

/// One site's decay analysis, as the dashboard reads it out of the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayReport {
    /// The numeric site id this is about.
    pub site: u16,
    /// When it was computed.
    pub computed_at: DateTime<Utc>,
    /// Length of each half of the comparison, in days.
    pub window_days: i64,
    /// Start of the recent half.
    pub window_start: DateTime<Utc>,
    /// The decaying pages, worst fall first.
    pub pages: Vec<DecayRow>,
}

/// Where one site's decay analysis is cached.
///
/// Public because the read endpoint fetches this key rather than recomputing.
pub fn decay_cache_key(site: u16) -> String {
    format!("mf:decay:{site}")
}

/// Compute and cache the decay analysis for one site, or for all of them.
///
/// A site whose analysis fails is logged and skipped. The sweep covers every site
/// on the install, and one site with no data yet must not stop the other forty
/// being measured.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let targets = match payload.site {
        Some(id) => vec![sites::get(&ctx.pg, id).await?],
        None => sites::list_all(&ctx.pg).await?,
    };

    let window_days = payload
        .window_days
        .unwrap_or(DEFAULT_WINDOW_DAYS)
        .clamp(7, 180);

    let mut decaying = 0usize;
    for site in &targets {
        match analyse(ctx, site, window_days).await {
            Ok(report) => {
                decaying += report.pages.len();
                ctx.kv
                    .set_json(&decay_cache_key(report.site), &report, Some(CACHE_TTL_SECS))
                    .await?;
                tracing::info!(
                    site = %site.domain,
                    window_days,
                    decaying = report.pages.len(),
                    "decay analysis stored"
                );
            }
            Err(e) => tracing::warn!(site = %site.domain, error = %e, "decay analysis failed"),
        }
    }

    tracing::info!(
        sites = targets.len(),
        pages = decaying,
        "decay sweep finished"
    );
    Ok(())
}

/// Measure one site.
///
/// The envelope carries only the *recent* half of the comparison;
/// [`micaforge_query::gap::decay`] derives the earlier half from it with
/// [`micaforge_core::time::previous_period`], so the two windows are the same
/// length by construction and cannot drift apart here.
pub async fn analyse(ctx: &JobCtx, site: &sites::Site, window_days: i64) -> Result<DecayReport> {
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let now = Utc::now();
    let start = now - Duration::days(window_days);

    let mut envelope = QueryEnvelope::new(site.site_id_u16(), TimeRange::new(start, now, tz));
    // Decay is about people leaving, so the human population is the one being
    // measured. The agent side of the comparison comes from `agent_events`
    // regardless of this, which is the whole point of the report.
    envelope.audience = Audience::Human;
    envelope.limit = MAX_ROWS;

    let pages = micaforge_query::gap::decay(&ctx.ch, &envelope).await?;

    Ok(DecayReport {
        site: envelope.site,
        computed_at: now,
        window_days,
        window_start: start,
        pages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_key_is_per_site_and_stable() {
        assert_eq!(decay_cache_key(1), "mf:decay:1");
        assert_ne!(decay_cache_key(1), decay_cache_key(2));
    }

    #[test]
    fn the_window_is_clamped_to_something_measurable() {
        // A one-day decay window would compare yesterday with the day before and
        // call every quiet Sunday a finding; a five-year one would never notice
        // anything. Both ends are held.
        let payload = Payload {
            site: None,
            window_days: Some(1),
        };
        assert_eq!(payload.window_days.unwrap().clamp(7, 180), 7);

        let long = Payload {
            site: None,
            window_days: Some(5_000),
        };
        assert_eq!(long.window_days.unwrap().clamp(7, 180), 180);
    }

    #[test]
    fn a_report_survives_a_round_trip_through_the_cache() {
        let report = DecayReport {
            site: 1,
            computed_at: Utc::now(),
            window_days: DEFAULT_WINDOW_DAYS,
            window_start: Utc::now(),
            pages: vec![DecayRow {
                path: "/docs/guide".into(),
                title: "Guide".into(),
                human_pageviews: 4,
                human_pageviews_prev: 120,
                human_change_pct: -96.67,
                agent_fetches: 310,
                agent_fetches_prev: 300,
                agent_change_pct: Some(3.33),
                answer_arrivals: 1,
            }],
        };

        let encoded = serde_json::to_string(&report).unwrap();
        let decoded: DecayReport = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.pages.len(), 1);
        assert_eq!(decoded.pages[0].path, "/docs/guide");
        assert_eq!(decoded.computed_at, report.computed_at);
    }
}
