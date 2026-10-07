//! `micaforge-jobs`: everything Micaforge does when nobody asked.
//!
//! Ingest is synchronous and reads are synchronous. Everything else — reading a
//! site's `robots.txt`, walking its sitemap, refreshing an operator's published
//! IP ranges, sweeping alerts, mailing a weekly report, importing three years of
//! GA4 — happens here, on a worker that shares the process with the API.
//!
//! # One binary, one queue
//!
//! `PRODUCT.md` promises `docker compose up` on a single VPS with Postgres,
//! ClickHouse and Redis. That budget has no room for a fourth service, so the
//! queue is a Postgres table ([`micaforge_store::models::jobs`]) claimed with
//! `SELECT ... FOR UPDATE SKIP LOCKED`, and the worker is [`Worker::spawn`]
//! called from the same `main` that serves HTTP. Several replicas of the binary
//! are safe against each other because the claim is one statement and the lock
//! is the row.
//!
//! The load this has to carry is hundreds of jobs a day, not thousands a second.
//! A crawl of one site is one row. A weekly report is one row. The only job that
//! moves real volume is [`run_import`], and it moves it in batches straight into
//! ClickHouse rather than through the queue.
//!
//! # Recurring work without a scheduler
//!
//! There is no cron process and no leader election. [`schedule_recurring`] seeds
//! one row per recurring kind, and each of those rows carries its own cadence in
//! its payload (`repeat_secs`). When the worker finishes a job that carries one,
//! it queues the next occurrence before it moves on. The schedule therefore
//! lives in the queue rather than in a running process, which is what makes a
//! restart, a redeploy or a crash cost one late crawl instead of a stopped
//! product.
//!
//! # What a job may assume
//!
//! Very little. Every job here is retried, so every job is written to be safe to
//! run twice: the crawlers upsert, the importer replaces its own rows by
//! `import_id`, the alert sweep claims each alert through
//! [`micaforge_store::models::alerts::try_fire_alert`] so two workers cannot both
//! send, and retention is expressed as a partition drop that is a no-op the
//! second time. A job that could not be made idempotent would not belong in this
//! crate.
//!
//! # Politeness
//!
//! Four of these jobs fetch somebody else's server. They all go out as
//! [`fetch::USER_AGENT`], they honour the site's own `robots.txt` when they read
//! the site's own pages, they are rate limited to one request a second per site,
//! and every request this crate makes, crawls, webhooks and import downloads
//! alike, refuses a private address in the URL, in each redirect hop and in each
//! DNS answer ([`outbound`]). An analytics tool that crawled its customers
//! rudely, or that could be pointed at a metadata endpoint by whoever typed the
//! domain into the settings screen, would be indefensible.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use micaforge_core::{Config, Error, Result};
use micaforge_mail::Mailer;
use micaforge_store::{ch::Ch, kv::Kv, pg::PgPool};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub mod check_alerts;
pub mod crawl_page_meta;
pub mod crawl_site_policy;
pub mod detect_decay;
pub mod fetch;
pub mod index_sitemap;
pub mod metrics;
pub mod notify;
pub mod outbound;
pub mod pages;
pub mod prune_retention;
pub mod refresh_agent_ranges;
pub mod rollup_sessions;
pub mod run_import;
pub mod schedule;
pub mod send_report;
pub mod worker;

pub use detect_decay::{decay_cache_key, DecayReport, DecayRow};
pub use schedule::{enqueue_at, enqueue_now, schedule_recurring, RECURRING};
pub use worker::Worker;

/// Everything a job is handed.
///
/// Built once at boot and shared by every worker lane behind an `Arc`, so the
/// fields are the process-wide handles rather than per-job state: `pg`, `ch` and
/// `kv` are all cheap clones over pooled or multiplexed connections, and
/// [`reqwest::Client`] is explicitly documented as something to build once and
/// reuse.
///
/// The fields are public because a job module is not a stranger to this struct —
/// they are two halves of the same crate, and a set of accessors that only ever
/// returned `&self.field` would be ceremony.
pub struct JobCtx {
    /// Postgres: sites, alerts, reports, imports, the catalog, the queue itself.
    pub pg: PgPool,
    /// ClickHouse: the event stream every report and alert is measured against.
    pub ch: Ch,
    /// Redis: the computed caches a dashboard reads, and nothing that may not be
    /// lost.
    pub kv: Kv,
    /// Outgoing mail. On an install with no relay this is the logging mailer,
    /// which is a supported configuration and not a broken one.
    pub mailer: Mailer,
    /// The one HTTP client every crawl and webhook goes out through. Build it
    /// with [`fetch::client`] so the user agent, the timeouts, the redirect
    /// policy and the guarded resolver are the ones [`outbound`] documents.
    pub http: reqwest::Client,
    /// Process configuration. `Arc` because the server holds the same one.
    pub cfg: Arc<Config>,
}

impl std::fmt::Debug for JobCtx {
    /// Hand-written: `Config` carries `secret_key_base` and the SMTP URL carries
    /// a password, and neither belongs in a log line that a job span might emit.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCtx")
            .field("clickhouse_db", &self.ch.db)
            .field("mailer", &self.mailer)
            .finish_non_exhaustive()
    }
}

/// Every kind of work the queue carries.
///
/// The wire spelling is `snake_case` and it is the `jobs.kind` column, so it is
/// as much a schema as the table is: renaming a variant here strands every row
/// already queued under the old name. [`Kind::parse`] is deliberately fallible
/// for the same reason — a row whose kind this build does not know is a job from
/// a newer release, and the honest response is to fail it loudly rather than to
/// silently drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Read a site's `robots.txt` and `llms.txt`. See [`crawl_site_policy`].
    CrawlSitePolicy,
    /// Discover and walk a site's sitemap. See [`index_sitemap`].
    IndexSitemap,
    /// Refresh operators' published IP ranges. See [`refresh_agent_ranges`].
    RefreshAgentRanges,
    /// Merge the session rollup. See [`rollup_sessions`].
    RollupSessions,
    /// Apply retention and drop expired partitions. See [`prune_retention`].
    PruneRetention,
    /// Evaluate every alert. See [`check_alerts`].
    CheckAlerts,
    /// Build and send a scheduled report. See [`send_report`].
    SendReport,
    /// Import GA4, Plausible or Umami data. See [`run_import`].
    RunImport,
    /// Find pages losing readers while agents keep crawling. See [`detect_decay`].
    DetectDecay,
    /// Fill in title, word count and last-modified. See [`crawl_page_meta`].
    CrawlPageMeta,
}

impl Kind {
    /// Every kind, in the order the admin queue view lists them.
    pub const ALL: &'static [Kind] = &[
        Kind::CrawlSitePolicy,
        Kind::IndexSitemap,
        Kind::RefreshAgentRanges,
        Kind::RollupSessions,
        Kind::PruneRetention,
        Kind::CheckAlerts,
        Kind::SendReport,
        Kind::RunImport,
        Kind::DetectDecay,
        Kind::CrawlPageMeta,
    ];

    /// The `jobs.kind` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::CrawlSitePolicy => "crawl_site_policy",
            Kind::IndexSitemap => "index_sitemap",
            Kind::RefreshAgentRanges => "refresh_agent_ranges",
            Kind::RollupSessions => "rollup_sessions",
            Kind::PruneRetention => "prune_retention",
            Kind::CheckAlerts => "check_alerts",
            Kind::SendReport => "send_report",
            Kind::RunImport => "run_import",
            Kind::DetectDecay => "detect_decay",
            Kind::CrawlPageMeta => "crawl_page_meta",
        }
    }

    /// Parse a `jobs.kind` value.
    pub fn parse(s: &str) -> Result<Kind> {
        Kind::ALL
            .iter()
            .copied()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| Error::validation("kind", format!("unknown job kind `{s}`")))
    }

    /// How long this kind may run before the worker gives up on it.
    ///
    /// Per-kind rather than global, because the range is three orders of
    /// magnitude: reading one `robots.txt` should take under a second, and
    /// importing a decade of GA4 legitimately takes an hour. One timeout that
    /// suited both would either kill the import or let a hung socket hold a
    /// worker lane all afternoon.
    pub fn timeout(&self) -> std::time::Duration {
        use std::time::Duration;
        match self {
            Kind::CrawlSitePolicy => Duration::from_secs(120),
            Kind::IndexSitemap => Duration::from_secs(15 * 60),
            Kind::RefreshAgentRanges => Duration::from_secs(10 * 60),
            Kind::RollupSessions => Duration::from_secs(30 * 60),
            Kind::PruneRetention => Duration::from_secs(30 * 60),
            Kind::CheckAlerts => Duration::from_secs(5 * 60),
            Kind::SendReport => Duration::from_secs(10 * 60),
            Kind::RunImport => Duration::from_secs(2 * 60 * 60),
            Kind::DetectDecay => Duration::from_secs(15 * 60),
            Kind::CrawlPageMeta => Duration::from_secs(20 * 60),
        }
    }

    /// True when a failure of this kind is worth waking somebody for.
    ///
    /// A failed crawl is a warning: the site was down, the next run will pick it
    /// up. A failed import or a failed report is an error, because a person is
    /// waiting for a result that will not arrive on its own.
    pub fn failure_is_loud(&self) -> bool {
        matches!(self, Kind::RunImport | Kind::SendReport)
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Decode a job payload into the argument struct its module expects.
///
/// A payload that will not decode is a [`Error::BadRequest`] rather than an
/// internal error: it was written by whoever enqueued the job, so it is the
/// caller's mistake, and reporting it as ours would send the operator looking in
/// the wrong place.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(kind: Kind, payload: &Json) -> Result<T> {
    serde_json::from_value(payload.clone())
        .map_err(|e| Error::BadRequest(format!("{kind} payload is not usable: {e}")))
}

/// Turn a job payload struct back into the jsonb the queue stores.
pub(crate) fn encode<T: Serialize>(payload: &T) -> Result<Json> {
    serde_json::to_value(payload).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_wire_name() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()).unwrap(), *kind);
            // The `jobs.kind` value and the serde spelling must agree, because
            // the queue writes one and typed payloads read the other.
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn an_unknown_kind_is_refused_rather_than_guessed() {
        let e = Kind::parse("delete_everything").unwrap_err();
        assert_eq!(e.status(), 422);
    }

    #[test]
    fn timeouts_are_ordered_by_how_much_work_the_kind_does() {
        assert!(Kind::RunImport.timeout() > Kind::CrawlSitePolicy.timeout());
        // Nothing may be un-timed: a job with no ceiling holds a lane forever.
        assert!(Kind::ALL.iter().all(|k| !k.timeout().is_zero()));
    }
}
