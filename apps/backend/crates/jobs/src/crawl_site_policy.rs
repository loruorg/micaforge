//! `crawl_site_policy` — read what a site tells crawlers.
//!
//! Fetches `robots.txt` and `llms.txt` from a site's own domain, parses both
//! with `micaforge-agents`, and stores the text and the parse into `site_policy`.
//! That stored copy is one half of the product's policy audit: the other half is
//! `agent_events`, and the report nobody else can produce is the join between
//! them — which agents fetched a path this file told them not to.
//!
//! # The text is stored, not just the parse
//!
//! `site_policy.robots_txt` keeps the file verbatim alongside the parsed form.
//! A violation report is an accusation, and an accusation has to be auditable: a
//! site owner looking at "GPTBot fetched 41 disallowed paths" must be able to
//! read the exact file that was in force when we decided that, not our reading
//! of a file that has since been edited.
//!
//! # A missing file is an answer
//!
//! Most sites have no `llms.txt` and many have no `robots.txt`. Both are stored
//! as `NULL` with a status recorded in `parsed`, and neither is an error. The
//! three-way distinction runs all the way through the product: a site with no
//! `llms.txt` (`-1`), a site whose `llms.txt` omits this page (`0`), and a site
//! that lists it (`1`) are three different statements, and only the second and
//! third are decisions the owner made.
//!
//! # It also registers the pages `llms.txt` names
//!
//! Every link in an `llms.txt` is a page the owner is *offering* to agents. Those
//! paths are written into `page_index` whether or not a person has ever visited
//! them, because the whole point of the register is that a page with no human
//! traffic is still a page.

use chrono::Utc;
use micaforge_agents::{llms_txt, policy, robots};
use micaforge_core::Result;
use micaforge_store::models::{catalog, sites};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::fetch::{self, LLMS_MAX_BYTES, ROBOTS_MAX_BYTES};
use crate::pages::{self, Patch};
use crate::JobCtx;

/// Which sites to read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One site, by its Postgres `sites.id`. Absent means every site, which is
    /// what the daily sweep queues.
    #[serde(default)]
    pub site: Option<Uuid>,
}

/// Fetch and store the policy for one site, or for all of them.
///
/// A site that cannot be reached is logged and skipped, never fatal: the sweep
/// covers every site on the install, and one customer's expired certificate must
/// not stop the other forty from being read. The job only fails if it could not
/// enumerate the sites at all.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let targets = match payload.site {
        Some(id) => vec![sites::get(&ctx.pg, id).await?],
        None => sites::list_all(&ctx.pg).await?,
    };

    let mut read = 0usize;
    let mut failed = 0usize;

    for site in &targets {
        match crawl_one(ctx, site).await {
            Ok(()) => read += 1,
            Err(e) => {
                failed += 1;
                tracing::warn!(site = %site.domain, error = %e, "could not read site policy");
            }
        }
    }

    tracing::info!(
        sites = targets.len(),
        read,
        failed,
        "site policy sweep finished"
    );
    Ok(())
}

/// Read one site.
async fn crawl_one(ctx: &JobCtx, site: &sites::Site) -> Result<()> {
    let robots_url = fetch::site_url(&site.domain, "/robots.txt")?;
    let llms_url = fetch::site_url(&site.domain, "/llms.txt")?;

    let (robots_doc, robots_status) = try_fetch(ctx, robots_url.as_str(), ROBOTS_MAX_BYTES).await;
    // Deliberately not paced against the `robots.txt` fetch above: two requests a
    // day to one host is not a load, and making the policy crawl take an extra
    // second per site would add a minute to a fifty-site sweep for nothing.
    let (llms_doc, llms_status) = try_fetch(ctx, llms_url.as_str(), LLMS_MAX_BYTES).await;

    let robots_text = robots_doc.as_ref().map(|d| d.body.clone());
    let llms_text = llms_doc.as_ref().map(|d| d.body.clone());

    let parsed_robots = robots::parse(robots_text.as_deref().unwrap_or_default());
    let parsed_llms = llms_text.as_deref().map(llms_txt::parse);

    let parsed = json!({
        "robots": parsed_robots,
        "llms": parsed_llms,
        "sitemaps": parsed_robots.sitemaps,
        // The whole catalog judged against the site root. Per-path answers are
        // recorded on each `agent_events` row at fetch time; this is the summary
        // the policy screen opens with.
        "policy": policy::evaluate_all(&parsed_robots, parsed_llms.as_ref(), "/"),
        "unrecognised_tokens": policy::unrecognised_tokens(&parsed_robots),
        "unaddressed_agents": policy::unaddressed_agents(&parsed_robots)
            .iter()
            .map(|def| def.slug)
            .collect::<Vec<_>>(),
        "robots_status": robots_status,
        "llms_status": llms_status,
        "fetched_at": Utc::now(),
    });

    catalog::upsert_policy(
        &ctx.pg,
        site.id,
        robots_text.as_deref(),
        llms_text.as_deref(),
        &parsed,
    )
    .await?;

    if let Some(llms) = &parsed_llms {
        register_offered_pages(ctx, site, llms).await?;
    }

    tracing::info!(
        site = %site.domain,
        robots = robots_status,
        llms = llms_status,
        sitemaps = parsed_robots.sitemaps.len(),
        groups = parsed_robots.groups().len(),
        "site policy stored"
    );
    Ok(())
}

/// Fetch a policy file, reducing every outcome to a document and a word.
///
/// `absent` and `error` are both stored rather than one being turned into the
/// other. "This site has no `llms.txt`" and "we could not reach this site today"
/// are different facts, and a policy screen that showed the second as the first
/// would tell an owner their file is missing when it is not.
async fn try_fetch(
    ctx: &JobCtx,
    url: &str,
    max_bytes: usize,
) -> (Option<fetch::Fetched>, &'static str) {
    match fetch::get(&ctx.http, url, max_bytes).await {
        Ok(Some(doc)) => (Some(doc), "ok"),
        Ok(None) => (None, "absent"),
        Err(e) => {
            tracing::debug!(url, error = %e, "policy file not readable");
            (None, "error")
        }
    }
}

/// Put every page an `llms.txt` names into the content register.
///
/// These are the pages a site is explicitly offering to language models, which
/// makes them the pages most worth having a Citation Gap number for. Many of them
/// have no human traffic at all and would otherwise never appear.
async fn register_offered_pages(
    ctx: &JobCtx,
    site: &sites::Site,
    llms: &llms_txt::LlmsTxt,
) -> Result<()> {
    let site_id = site.site_id_u16();
    let existing = pages::load(&ctx.ch, site_id).await?;
    let now = Utc::now();

    let patch = Patch {
        in_llms_txt: Some(true),
        ..Patch::default()
    };

    let mut rows = Vec::new();
    for link in llms.links() {
        let path = micaforge_enrich::url_norm::normalise_path(&link.path(), false);
        rows.push(patch.apply(existing.get(&path), site_id, &path, now));
    }

    // A page that has been dropped from `llms.txt` has to stop claiming it is in
    // it, so every row that used to be flagged and is no longer listed is
    // written back with the flag cleared.
    let listed: std::collections::HashSet<String> =
        rows.iter().map(|row| row.pathname.clone()).collect();
    let cleared = Patch {
        in_llms_txt: Some(false),
        ..Patch::default()
    };
    for (path, row) in &existing {
        if row.in_llms_txt == 1 && !listed.contains(path) {
            rows.push(cleared.apply(Some(row), site_id, path, now));
        }
    }

    let written = pages::write(&ctx.ch, &rows).await?;
    tracing::debug!(site = %site.domain, pages = written, "llms.txt pages registered");
    Ok(())
}
