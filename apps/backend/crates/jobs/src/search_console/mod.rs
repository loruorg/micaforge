//! `sync_search_console`: bring each connected site's Google Search Console
//! data into ClickHouse.
//!
//! Off unless configured. With no `MICAFORGE_SEARCH_CONSOLE_CLIENT_ID` the job
//! returns at once and no request leaves the process; with one, it reads only
//! the sites whose admin connected Search Console and chose a property.
//!
//! ## One run, one site at a time
//!
//! The daily sweep (and a site admin's "Sync now", and the follow-ups a
//! backfill queues for itself) take each site's lease in Postgres, so two
//! workers never sync one site at once, then:
//!
//! 1. open the sealed refresh token ([`open_token`]) and mint an access token
//!    from it. Google refusing the refresh token (`invalid_grant`) means the
//!    person revoked access or it expired: the token is deleted, the
//!    connection marked `revoked`, and the settings screen says to connect
//!    again. Nothing is retried, because nothing would help.
//! 2. plan the days ([`plan`]): the provisional days again, anything new, and
//!    a slice of the sixteen-month backfill, within one run's budget.
//! 3. for each chunk of days, fetch every grain (see
//!    `micaforge_store::ch::SearchConsoleRow`): the coarse ones (totals,
//!    countries, devices) once over the chunk, the fine ones (pages, and
//!    search terms by page, country and device) one day at a time, each paged
//!    through Google's 25 000-row pages up to its daily ceiling. Each chunk's
//!    rows are written before the chunk counts as done, so a run that fails
//!    half way resumes at the first chunk it did not finish.
//! 4. record how far the window moved, and queue a follow-up when the plan
//!    left work over.
//!
//! A day fetched again replaces the earlier copy in every read, because reads
//! keep only the newest fetch of each day (`micaforge_query::search`).
//!
//! ## What is retried
//!
//! A Google outage, a quota refusal or a ClickHouse hiccup fails the run, the
//! queue retries it, and the error is shown to admins meanwhile. A property
//! the account can no longer read is recorded and not retried; the next
//! day's sweep tries again, in case access came back.

mod countries;
pub mod google;
pub mod plan;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use micaforge_core::config::OAuthClient;
use micaforge_core::{crypto, Config, Error, Result};
use micaforge_enrich::url_norm;
use micaforge_store::ch::{insert_rows, SearchConsoleRow};
use micaforge_store::models::search_console::{self as store, Progress};
use micaforge_store::models::sites::{self, Site};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use google::{Endpoints, GoogleError, Property, SCOPE};

use crate::{enqueue_at, JobCtx, Kind};
use google::{AnalyticsRequest, ApiRow, ROW_LIMIT};
use plan::{Chunk, Window};

/// The key-derivation purpose the refresh token is sealed under.
pub const TOKEN_PURPOSE: &str = "search-console-token";

/// How long one run holds a site. Shorter than the job's timeout, so a worker
/// that died mid-run frees the site before the next sweep.
const LEASE_SECS: i64 = 25 * 60;

/// How soon a backfill's follow-up runs.
const FOLLOW_UP_SECS: i64 = 60;

/// Rows per ClickHouse insert.
const INSERT_BATCH: usize = 10_000;

/// Which sites to sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One site, from "Sync now" or a backfill follow-up. Absent is every
    /// connected site, which is what the daily sweep queues.
    #[serde(default)]
    pub site: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// The sealed token
// ---------------------------------------------------------------------------

/// The additional data a site's token is sealed with: the blob opens for this
/// site's row and no other.
fn token_aad(site: Uuid) -> Vec<u8> {
    format!("search-console:{site}").into_bytes()
}

/// Seal a refresh token for `search_console_connections.refresh_token_sealed`.
pub fn seal_token(cfg: &Config, site: Uuid, refresh_token: &str) -> Result<Vec<u8>> {
    seal_with(&cfg.derive_key(TOKEN_PURPOSE), site, refresh_token)
}

/// Open what [`seal_token`] sealed, for the same site.
pub fn open_token(cfg: &Config, site: Uuid, sealed: &[u8]) -> Result<String> {
    open_with(&cfg.derive_key(TOKEN_PURPOSE), site, sealed)
}

fn seal_with(key: &[u8; 32], site: Uuid, refresh_token: &str) -> Result<Vec<u8>> {
    crypto::seal(key, &token_aad(site), refresh_token.as_bytes())
}

fn open_with(key: &[u8; 32], site: Uuid, sealed: &[u8]) -> Result<String> {
    let plain = crypto::open(key, &token_aad(site), sealed)?;
    String::from_utf8(plain)
        .map_err(|_| Error::internal("the stored Search Console token is not text"))
}

// ---------------------------------------------------------------------------
// Grains
// ---------------------------------------------------------------------------

/// One set of dimensions the sync asks Google for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grain {
    /// By date: the property's daily totals.
    Total,
    /// By date, query, page, country and device.
    Query,
    /// By date and page.
    Page,
    /// By date and country.
    Country,
    /// By date and device.
    Device,
}

impl Grain {
    /// The `grain` column value.
    pub fn as_str(self) -> &'static str {
        match self {
            Grain::Total => micaforge_query::search::GRAIN_TOTAL,
            Grain::Query => micaforge_query::search::GRAIN_QUERY,
            Grain::Page => micaforge_query::search::GRAIN_PAGE,
            Grain::Country => micaforge_query::search::GRAIN_COUNTRY,
            Grain::Device => micaforge_query::search::GRAIN_DEVICE,
        }
    }

    /// The dimensions asked for, in the order an answer's `keys` follow.
    pub fn dimensions(self) -> &'static [&'static str] {
        match self {
            Grain::Total => &["date"],
            Grain::Query => &["date", "query", "page", "country", "device"],
            Grain::Page => &["date", "page"],
            Grain::Country => &["date", "country"],
            Grain::Device => &["date", "device"],
        }
    }
}

/// Fetched once over a whole chunk: a few rows a day.
const COARSE: [Grain; 3] = [Grain::Total, Grain::Country, Grain::Device];

/// Fetched one day at a time: these can reach Google's daily ceiling.
const FINE: [Grain; 2] = [Grain::Page, Grain::Query];

/// Place one answer row in its grain's columns.
///
/// `None` for a row whose date will not parse, which Google never sends; the
/// caller counts it rather than failing a whole day over it.
pub fn to_row(
    site: u16,
    property: &str,
    grain: Grain,
    trailing_slash: bool,
    row: &ApiRow,
    synced_at: DateTime<Utc>,
) -> Option<SearchConsoleRow> {
    let key = |name: &str| -> &str {
        grain
            .dimensions()
            .iter()
            .position(|d| *d == name)
            .and_then(|i| row.keys.get(i))
            .map(String::as_str)
            .unwrap_or("")
    };
    let date: NaiveDate = key("date").parse().ok()?;
    let page = key("page");
    let (hostname, pathname) = split_page(page, trailing_slash);
    Some(SearchConsoleRow {
        site_id: site,
        property: property.to_string(),
        grain: grain.as_str().to_string(),
        date,
        query: key("query").to_string(),
        page: page.to_string(),
        hostname,
        pathname,
        country: countries::alpha2(key("country")),
        device: key("device").to_ascii_lowercase(),
        clicks: count(row.clicks),
        impressions: count(row.impressions),
        position: if row.position.is_finite() {
            row.position
        } else {
            0.0
        },
        synced_at,
    })
}

/// A page address as its host and its path, the path normalised exactly as
/// ingest normalises a pageview's, so the two join. Empty for no page.
fn split_page(page: &str, trailing_slash: bool) -> (String, String) {
    if page.is_empty() {
        return (String::new(), String::new());
    }
    match url::Url::parse(page) {
        Ok(url) => (
            url.host_str().unwrap_or("").to_ascii_lowercase(),
            url_norm::normalise_path(url.path(), trailing_slash),
        ),
        Err(_) => (String::new(), String::new()),
    }
}

/// Google's double as a count.
fn count(v: f64) -> u32 {
    if v.is_finite() {
        v.round().clamp(0.0, f64::from(u32::MAX)) as u32
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// The job
// ---------------------------------------------------------------------------

/// How one site's run ended, short of a failure worth retrying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Up to date.
    Done,
    /// Wrote what it planned; the backfill has more.
    More,
    /// Another worker holds the site, or it is no longer connected.
    Skipped,
    /// Google refused the grant; the connection is marked revoked.
    Revoked,
    /// A failure retrying will not fix, recorded for the admin.
    Refused,
}

/// Why a run stopped.
enum SyncError {
    Google(GoogleError),
    Store(Error),
}

impl From<GoogleError> for SyncError {
    fn from(e: GoogleError) -> Self {
        SyncError::Google(e)
    }
}

impl From<Error> for SyncError {
    fn from(e: Error) -> Self {
        SyncError::Store(e)
    }
}

/// Run the job. See the module notes.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let Some(client) = ctx.cfg.search_console.clone() else {
        tracing::debug!("search console is not configured; nothing to sync");
        return Ok(());
    };
    let endpoints = Endpoints::google();
    let targets: Vec<Uuid> = match payload.site {
        Some(id) => vec![id],
        None => store::list_syncable(&ctx.pg)
            .await?
            .into_iter()
            .map(|c| c.site_id)
            .collect(),
    };

    let mut failed = 0usize;
    for site in &targets {
        match sync_site(ctx, &client, &endpoints, *site).await {
            Ok(Outcome::More) => {
                let at = Utc::now() + Duration::seconds(FOLLOW_UP_SECS);
                enqueue_at(
                    &ctx.pg,
                    Kind::SyncSearchConsole,
                    &Payload { site: Some(*site) },
                    at,
                )
                .await?;
            }
            Ok(outcome) => tracing::debug!(site = %site, ?outcome, "search console sync finished"),
            Err(e) => {
                failed += 1;
                tracing::warn!(site = %site, error = %e, "search console sync failed");
            }
        }
    }

    // Every site failing is Google or ClickHouse being away, and worth the
    // queue's retry. A few failing is a few sites with a problem of their own,
    // recorded on each connection.
    if !targets.is_empty() && failed == targets.len() {
        return Err(Error::Upstream(format!(
            "no Search Console sync succeeded ({failed} failed)"
        )));
    }
    Ok(())
}

/// Sync one site.
async fn sync_site(
    ctx: &JobCtx,
    client: &OAuthClient,
    endpoints: &Endpoints,
    site_id: Uuid,
) -> Result<Outcome> {
    let Some(conn) = store::take_lease(&ctx.pg, site_id, LEASE_SECS).await? else {
        return Ok(Outcome::Skipped);
    };
    let (Some(property), Some(sealed)) = (conn.property.clone(), conn.refresh_token_sealed.clone())
    else {
        return Ok(Outcome::Skipped);
    };

    let refresh_token = match open_token(&ctx.cfg, site_id, &sealed) {
        Ok(token) => token,
        Err(_) => {
            // The server's secret changed since the token was sealed. The
            // token is unreadable for good, which is a revocation in effect.
            store::mark_revoked(
                &ctx.pg,
                site_id,
                "The stored connection can no longer be opened because the server's secret key \
                 changed. Connect Search Console again.",
            )
            .await?;
            return Ok(Outcome::Revoked);
        }
    };

    let today = plan::google_today(Utc::now());
    let start = Window {
        backfilled_from: conn.backfilled_from,
        synced_through: conn.synced_through,
        backfill_until: conn
            .backfill_until
            .unwrap_or_else(|| plan::backfill_until(today)),
    };
    let the_plan = plan::plan(today, start, plan::DAYS_PER_RUN);

    let mut session = Session {
        http: &ctx.http,
        endpoints,
        client,
        refresh_token,
        access: None,
    };
    let mut window = start;
    let mut written = 0i64;
    let outcome: std::result::Result<(), SyncError> = async {
        let site = sites::get(&ctx.pg, site_id).await?;
        for chunk in &the_plan.chunks {
            written += fetch_chunk(ctx, &mut session, &site, &property, chunk).await?;
            window = window.after(chunk);
        }
        Ok(())
    }
    .await;

    let progress = Progress {
        backfilled_from: window.backfilled_from,
        synced_through: window.synced_through,
        rows: written,
    };
    match outcome {
        Ok(()) => {
            store::finish_ok(&ctx.pg, site_id, &property, &progress).await?;
            Ok(if the_plan.more {
                Outcome::More
            } else {
                Outcome::Done
            })
        }
        Err(SyncError::Google(GoogleError::Revoked)) => {
            store::mark_revoked(&ctx.pg, site_id, &GoogleError::Revoked.sentence()).await?;
            Ok(Outcome::Revoked)
        }
        Err(SyncError::Google(e @ GoogleError::Forbidden)) => {
            store::finish_failed(&ctx.pg, site_id, &property, &progress, &e.sentence()).await?;
            Ok(Outcome::Refused)
        }
        Err(SyncError::Google(e)) => {
            store::finish_failed(&ctx.pg, site_id, &property, &progress, &e.sentence()).await?;
            Err(e.into())
        }
        Err(SyncError::Store(e)) => {
            let said = "Micaforge could not store the data Google sent. The next sync tries again.";
            store::finish_failed(&ctx.pg, site_id, &property, &progress, said).await?;
            Err(e)
        }
    }
}

/// The Google side of one run: the client and the current access token.
struct Session<'a> {
    http: &'a reqwest::Client,
    endpoints: &'a Endpoints,
    client: &'a OAuthClient,
    refresh_token: String,
    access: Option<String>,
}

impl Session<'_> {
    /// The access token, minted on first use.
    async fn access(&mut self) -> std::result::Result<String, GoogleError> {
        if let Some(token) = &self.access {
            return Ok(token.clone());
        }
        let token =
            google::refresh(self.http, self.endpoints, self.client, &self.refresh_token).await?;
        self.access = Some(token.clone());
        Ok(token)
    }

    /// Every row of one grain over `start..=end`. An access token Google
    /// refuses mid-run (it lives an hour) is minted again once.
    async fn fetch(
        &mut self,
        property: &str,
        grain: Grain,
        start: &str,
        end: &str,
    ) -> std::result::Result<(Vec<ApiRow>, bool), GoogleError> {
        let mut retried = false;
        loop {
            let token = self.access().await?;
            let (http, endpoints) = (self.http, self.endpoints);
            let read = plan::page_through(ROW_LIMIT, |start_row| {
                let request = AnalyticsRequest {
                    start,
                    end,
                    dimensions: grain.dimensions(),
                    start_row,
                    row_limit: ROW_LIMIT,
                };
                let token = token.as_str();
                async move { google::query(http, endpoints, token, property, &request).await }
            })
            .await;
            match read {
                Err(GoogleError::Unauthorized) if !retried => {
                    retried = true;
                    self.access = None;
                }
                other => return other,
            }
        }
    }
}

/// What a run writes for: the site, its property, and how the site's paths
/// are normalised.
struct Target<'a> {
    site: &'a Site,
    property: &'a str,
    trailing_slash: bool,
}

/// Fetch and write one chunk. Returns the rows written.
async fn fetch_chunk(
    ctx: &JobCtx,
    session: &mut Session<'_>,
    site: &Site,
    property: &str,
    chunk: &Chunk,
) -> std::result::Result<i64, SyncError> {
    let target = Target {
        site,
        property,
        // The same default ingest applies: `/docs` and `/docs/` stay apart
        // unless the site said otherwise.
        trailing_slash: site
            .settings
            .get("trailing_slash")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
    };
    let mut written = 0i64;
    let (start, end) = (chunk.start.to_string(), chunk.end.to_string());
    for grain in COARSE {
        written += fetch_grain(ctx, session, &target, grain, (&start, &end)).await?;
    }
    for day in chunk.days() {
        let day = day.to_string();
        for grain in FINE {
            written += fetch_grain(ctx, session, &target, grain, (&day, &day)).await?;
        }
    }
    Ok(written)
}

/// Fetch one grain over one window (first and last day) and write it, all
/// under one `synced_at`.
async fn fetch_grain(
    ctx: &JobCtx,
    session: &mut Session<'_>,
    target: &Target<'_>,
    grain: Grain,
    (start, end): (&str, &str),
) -> std::result::Result<i64, SyncError> {
    let synced_at = Utc::now();
    let site = target.site;
    let (rows, capped) = session.fetch(target.property, grain, start, end).await?;
    if capped {
        tracing::info!(
            site = site.site_id,
            grain = grain.as_str(),
            start,
            end,
            "search console returned its row ceiling; the least-clicked rows are not included"
        );
    }
    let total = rows.len();
    let placed: Vec<SearchConsoleRow> = rows
        .iter()
        .filter_map(|r| {
            to_row(
                site.site_id_u16(),
                target.property,
                grain,
                target.trailing_slash,
                r,
                synced_at,
            )
        })
        .collect();
    if placed.len() < total {
        tracing::warn!(
            site = site.site_id,
            skipped = total - placed.len(),
            "search console rows without a readable date were skipped"
        );
    }
    for batch in placed.chunks(INSERT_BATCH) {
        insert_rows(&ctx.ch, "search_console", batch).await?;
    }
    Ok(placed.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_token_opens_only_for_its_own_site() {
        let key = crypto::derive_key(&"s".repeat(48), TOKEN_PURPOSE);
        let site = Uuid::from_u128(7);
        let sealed = seal_with(&key, site, "1//0g-refresh").unwrap();
        assert!(
            !sealed.windows(5).any(|w| w == b"1//0g"),
            "the token is not in the clear"
        );
        assert_eq!(open_with(&key, site, &sealed).unwrap(), "1//0g-refresh");
        // Moved to another site's row, it does not open.
        assert!(open_with(&key, Uuid::from_u128(8), &sealed).is_err());
        // Under another server secret, it does not open either.
        let other = crypto::derive_key(&"t".repeat(48), TOKEN_PURPOSE);
        assert!(open_with(&other, site, &sealed).is_err());
        // Sealing twice gives two different blobs (a fresh nonce each time).
        assert_ne!(seal_with(&key, site, "1//0g-refresh").unwrap(), sealed);
    }

    #[test]
    fn each_grain_places_its_keys_in_their_columns() {
        let now = Utc::now();
        let full = ApiRow {
            keys: vec![
                "2026-10-01".into(),
                "micaforge pricing".into(),
                "https://WWW.Example.com/pricing/index.html?ref=x#plans".into(),
                "deu".into(),
                "MOBILE".into(),
            ],
            clicks: 3.0,
            impressions: 41.6,
            position: 2.25,
        };
        let row = to_row(9, "sc-domain:example.com", Grain::Query, false, &full, now).unwrap();
        assert_eq!(row.grain, "query");
        assert_eq!(row.date.to_string(), "2026-10-01");
        assert_eq!(row.query, "micaforge pricing");
        assert_eq!(row.hostname, "www.example.com");
        assert_eq!(
            row.pathname, "/pricing",
            "normalised as ingest normalises it"
        );
        assert_eq!(row.country, "DE");
        assert_eq!(row.device, "mobile");
        assert_eq!((row.clicks, row.impressions), (3, 42));

        let total = ApiRow {
            keys: vec!["2026-10-01".into()],
            clicks: 10.0,
            impressions: 200.0,
            position: 5.5,
        };
        let row = to_row(9, "sc-domain:example.com", Grain::Total, true, &total, now).unwrap();
        assert_eq!(
            (row.query.as_str(), row.page.as_str(), row.country.as_str()),
            ("", "", "")
        );

        let device = ApiRow {
            keys: vec!["2026-10-01".into(), "TABLET".into()],
            clicks: 1.0,
            impressions: 2.0,
            position: 1.0,
        };
        let row = to_row(9, "p", Grain::Device, true, &device, now).unwrap();
        assert_eq!(row.device, "tablet");
        assert_eq!(row.country, "");

        let unknown = ApiRow {
            keys: vec!["2026-10-01".into(), "zzz".into()],
            clicks: 1.0,
            impressions: 2.0,
            position: 1.0,
        };
        assert_eq!(
            to_row(9, "p", Grain::Country, true, &unknown, now)
                .unwrap()
                .country,
            ""
        );

        let bad_date = ApiRow {
            keys: vec!["yesterday".into()],
            clicks: 1.0,
            impressions: 1.0,
            position: f64::NAN,
        };
        assert!(to_row(9, "p", Grain::Total, true, &bad_date, now).is_none());
    }

    #[test]
    fn every_grain_starts_with_the_date() {
        for grain in COARSE.iter().chain(FINE.iter()) {
            assert_eq!(grain.dimensions()[0], "date", "{grain:?}");
        }
        assert_eq!(count(-3.0), 0);
        assert_eq!(count(f64::INFINITY), 0);
        assert_eq!(count(2.5), 3);
    }
}
