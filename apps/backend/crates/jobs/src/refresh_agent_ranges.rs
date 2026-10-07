//! `refresh_agent_ranges` — keep `agent_events.verified` meaning something.
//!
//! A user agent is a claim. Anything can send `GPTBot` and plenty of things do,
//! which is why `agent_events.verified` exists and why it has to be earned: a
//! fetch is verified when its source address is inside a range the operator
//! themselves publishes. This job is what keeps those ranges current.
//!
//! It runs every twelve hours. Operators change published ranges rarely — a new
//! region, a retired block — but they do change them without announcing it, and a
//! verification checked against a document from six months ago reports "not
//! verified" for perfectly genuine traffic, which is exactly the wrong way for
//! this number to be wrong.
//!
//! # One operator's outage is not a failure
//!
//! Each document is fetched independently and a failure is recorded per agent.
//! The job returns `Ok` as long as it could enumerate the catalog, because
//! failing the whole sweep because Perplexity's endpoint was down would also
//! throw away the eight documents that fetched perfectly, and would then retry
//! all nine.
//!
//! # A bad day never overwrites a good document
//!
//! An agent whose fetch failed is left entirely alone: its existing row keeps the
//! ranges it already had. Writing an empty list on a failed fetch would silently
//! turn every verified fetch by that agent into an unverified one, which reads on
//! the dashboard as "this crawler is now impersonating itself".
//!
//! # The self-hosted promise
//!
//! `PRODUCT.md` says a self-hosted build makes no outbound call at runtime. This
//! job is the exception that proves it: it is scheduled work an operator can
//! delete from the queue, not something the request path does, and nothing in the
//! product breaks without it — verification simply stays `unknown`, which is a
//! reportable answer and appears as one.

use micaforge_agents::{catalog as agents, verify};
use micaforge_core::{Error, Result};
use micaforge_store::models::catalog;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::fetch::{self, Pace, RANGES_MAX_BYTES};
use crate::JobCtx;

/// The Redis key stamped with the time of the last successful refresh.
///
/// Published so that a long-running process holding a
/// [`micaforge_agents::verify::RangeStore`] in memory can notice that Postgres
/// has newer ranges and reload, rather than polling the table or waiting for a
/// restart. It is a hint and nothing depends on it: a cold Redis costs one late
/// reload.
pub const REFRESHED_AT_KEY: &str = "mf:agents:ranges:refreshed_at";

/// How long that hint survives. Twice the refresh cadence, so it is always
/// present on a healthy install and absent on one where the job has stopped.
const REFRESHED_TTL_SECS: u32 = 24 * 60 * 60;

/// Nothing to configure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// Refresh every operator's published ranges, and reseed the catalog.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    let stored = catalog::list_agents(&ctx.pg).await?;
    let known: std::collections::HashSet<&str> =
        stored.iter().map(|row| row.slug.as_str()).collect();
    let existing: std::collections::HashMap<&str, &serde_json::Value> = stored
        .iter()
        .map(|row| (row.slug.as_str(), &row.ip_ranges))
        .collect();

    let mut pace = Pace::per_second();
    let mut refreshed = 0usize;
    let mut prefixes_total = 0usize;
    let mut failed = 0usize;
    let mut seeded = 0usize;

    for def in agents::CATALOG {
        // What this agent's ranges should be after this pass.
        let ranges = if def.publishes_ranges() {
            pace.tick().await;
            match fetch_ranges(ctx, def).await {
                Ok(nets) => {
                    refreshed += 1;
                    prefixes_total += nets.len();
                    tracing::debug!(agent = def.slug, prefixes = nets.len(), "ranges refreshed");
                    Some(json!(nets))
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(agent = def.slug, error = %e, "range refresh failed");
                    None
                }
            }
        } else {
            None
        };

        let ranges = match ranges {
            Some(fresh) => fresh,
            // No fetch, or a failed one. Keep whatever the row already holds; a
            // seed for an agent nobody has stored yet starts empty.
            None => match existing.get(def.slug) {
                Some(held) => (*held).clone(),
                None => json!([]),
            },
        };

        if !known.contains(def.slug) {
            seeded += 1;
        }

        catalog::upsert_agent(
            &ctx.pg,
            def.slug,
            def.display_name,
            def.operator,
            def.purpose.as_str(),
            &json!(def.ua_patterns),
            &ranges,
            &json!(def.rdns_suffixes),
            def.docs_url,
            def.respects_robots,
        )
        .await?;
    }

    // Best effort, and deliberately not `?`. The ranges are in Postgres, which is
    // where they matter; failing the job because a cache hint could not be
    // written would retry every one of those fetches for nothing.
    if let Err(e) = ctx
        .kv
        .set_json(
            REFRESHED_AT_KEY,
            &chrono::Utc::now(),
            Some(REFRESHED_TTL_SECS),
        )
        .await
    {
        tracing::warn!(error = %e, "could not stamp the range refresh time");
    }

    tracing::info!(
        agents = agents::CATALOG.len(),
        refreshed,
        prefixes = prefixes_total,
        failed,
        seeded,
        "agent range refresh finished"
    );
    Ok(())
}

/// Fetch and parse one operator's published range document.
///
/// Goes through [`fetch::get`] rather than
/// [`micaforge_agents::verify::fetch_published_ranges`], so the document is read
/// under a byte cap by the guarded client like every other fetch this crate
/// makes. The parse is the agents crate's, unchanged.
///
/// Returns the prefixes in their CIDR spelling, which is how the catalog row
/// stores them.
async fn fetch_ranges(ctx: &JobCtx, def: &agents::AgentDef) -> Result<Vec<String>> {
    let Some(url) = def.ip_ranges_url else {
        return Err(Error::NotFound(format!(
            "{} publishes no machine-readable IP range document",
            def.slug
        )));
    };

    let doc = fetch::get(&ctx.http, url, RANGES_MAX_BYTES)
        .await?
        .ok_or_else(|| Error::Upstream(format!("{url} is not there any more")))?;
    if doc.truncated {
        return Err(Error::Upstream(format!(
            "{url} is larger than {RANGES_MAX_BYTES} bytes"
        )));
    }

    let ranges = verify::parse_range_document(&doc.body)?;
    if ranges.is_empty() {
        return Err(Error::Upstream(format!(
            "{url} contained no usable prefixes"
        )));
    }
    Ok(ranges.iter().map(|net| net.to_string()).collect())
}
