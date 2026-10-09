//! API imports: reading history straight from PostHog or Simple Analytics.
//!
//! A file import makes no outbound request at all: the operator exports from
//! the old tool and uploads the file. An API import is the one way an import
//! reaches out, and it does so only because an operator asked for it by typing
//! an API key into the Imports screen. Nothing in this module runs unless an
//! `imports` row was created with a key, so an install that never does that
//! never makes the request.
//!
//! # The API writes a file, and the file is imported like any other
//!
//! The provider's answer is written to the import's scratch file, one record
//! per line, and from there it is staged and normalised by exactly the code an
//! uploaded export goes through ([`crate::run_import`]). There is no second
//! pipeline to drift from the first. A retry downloads again and staging
//! resumes at the record it had reached, which is sound because both providers
//! answer a window in a fixed order: PostHog by `(timestamp, uuid)`, which this
//! module asks for, and Simple Analytics in its export order.
//!
//! # The key is sealed from the moment it arrives
//!
//! The create route hands the key to [`seal`] before anything else happens to
//! it: XChaCha20-Poly1305 under a key derived from `MICAFORGE_SECRET_KEY_BASE`
//! for the `imports` purpose, with the import's own id as additional
//! authenticated data, so the blob is worthless in any other row. It is stored
//! in `imports.credentials_sealed`, opened only here, inside the job, sent only
//! in a request header to the provider, and cleared when the import is done or
//! given up for good. It is never in the job payload, never logged, never
//! returned by a route, and [`Credentials`]' `Debug` prints nothing of it.
//!
//! # No redirect is followed with a key attached
//!
//! The shared client follows redirects. `reqwest` drops an `Authorization`
//! header when a redirect leaves the host, which would cover PostHog, but
//! Simple Analytics takes its key in custom `Api-Key` and `User-Id` headers,
//! and those would be carried to wherever a redirect pointed. So these requests
//! go through their own client, which follows no redirect at all and otherwise
//! keeps every protection [`crate::outbound`] documents: the guarded resolver,
//! no proxy, the URL check before each request, the timeouts and the body caps.
//! A provider that answers with a redirect gets an error that says so.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use micaforge_core::{crypto, Config, Error, Result};
use micaforge_store::models::imports;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

use crate::outbound::{self, BodyCap, Failure};
use crate::run_import::{parse_instant, MAX_DOWNLOAD_BYTES};
use crate::JobCtx;

/// Key-derivation purpose for sealed import credentials. Changing it makes
/// every stored key unreadable, which fails the imports still waiting to run.
const CREDENTIALS_PURPOSE: &str = "imports";

/// PostHog Cloud's US region, the host an API import uses when none is typed.
pub const POSTHOG_DEFAULT_HOST: &str = "https://us.posthog.com";

/// Simple Analytics' export endpoint. Simple Analytics is a hosted service
/// only, so the address is fixed and nothing an operator types changes it.
const SIMPLE_ANALYTICS_EXPORT: &str = "https://simpleanalytics.com/api/export/datapoints";

/// Events asked of PostHog per query.
///
/// PostHog's query API returns at most 50 000 rows, and a page of events with
/// their property bags runs to a few kilobytes a row. Five thousand keeps one
/// answer to some tens of megabytes, which [`PAGE_MAX_BYTES`] allows for.
const PAGE_ROWS: usize = 5_000;

/// The smallest page tried after a page came back too large to read.
const MIN_PAGE_ROWS: usize = 250;

/// Most bytes one PostHog answer may carry.
const PAGE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Longest one PostHog query may take. PostHog bounds a query's own execution
/// well inside this; the rest is transfer.
const PAGE_TIMEOUT: Duration = Duration::from_secs(90);

/// Longest a Simple Analytics export may stream. It sends the whole window in
/// one answer, so this is sized like the import's own download timeout.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How many times one request is retried after the provider asked for a pause
/// (`429`) or was briefly unavailable (`502`, `503`, `504`).
const PAUSES: u32 = 6;

/// The pause when the provider did not say how long to wait.
const DEFAULT_PAUSE: Duration = Duration::from_secs(10);

/// The longest pause honoured. A provider asking for more than a minute is
/// better answered by the queue's own retry, an hour later.
const MAX_PAUSE: Duration = Duration::from_secs(60);

/// The fields asked of Simple Analytics for page views.
///
/// Everything the importer maps, and nothing it does not: the user agent is
/// left out because nothing reads it and a cookie-free product should not
/// hold a copy of it.
const SA_PAGEVIEW_FIELDS: &[&str] = &[
    "added_iso",
    "hostname",
    "path",
    "query",
    "is_unique",
    "is_robot",
    "document_referrer",
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_content",
    "utm_term",
    "duration_seconds",
    "scrolled_percentage",
    "viewport_width",
    "viewport_height",
    "screen_width",
    "screen_height",
    "device_type",
    "country_code",
    "browser_name",
    "browser_version",
    "os_name",
    "os_version",
    "lang_language",
    "session_id",
    "uuid",
];

/// The fields asked of Simple Analytics for events. `datapoint` is the event's
/// name; Simple Analytics does not time or measure scroll on an event.
const SA_EVENT_FIELDS: &[&str] = &[
    "added_iso",
    "datapoint",
    "hostname",
    "path",
    "query",
    "is_robot",
    "document_referrer",
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_content",
    "utm_term",
    "viewport_width",
    "viewport_height",
    "screen_width",
    "screen_height",
    "device_type",
    "country_code",
    "browser_name",
    "browser_version",
    "os_name",
    "os_version",
    "lang_language",
    "session_id",
    "uuid",
];

// ---------------------------------------------------------------------------
// What an API import is
// ---------------------------------------------------------------------------

/// Which API to read, and which days. Travels in the job payload, so nothing
/// in it is secret; the key is in [`Credentials`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ApiSource {
    /// PostHog's query API.
    Posthog {
        /// `scheme://host[:port]`, already validated by [`posthog_host`].
        host: String,
        /// The numeric project id.
        project: String,
        /// First day, UTC, inclusive.
        from: NaiveDate,
        /// Last day, UTC, inclusive.
        to: NaiveDate,
    },
    /// Simple Analytics' export API.
    SimpleAnalytics {
        /// The website as Simple Analytics names it.
        hostname: String,
        /// First day, UTC, inclusive.
        from: NaiveDate,
        /// Last day, UTC, inclusive.
        to: NaiveDate,
    },
}

impl ApiSource {
    /// The provider's name, as a sentence about it would use it.
    pub fn provider(&self) -> &'static str {
        match self {
            ApiSource::Posthog { .. } => "PostHog",
            ApiSource::SimpleAnalytics { .. } => "Simple Analytics",
        }
    }
}

/// The secret half of an API import.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    /// PostHog's personal API key, or Simple Analytics' `sa_api_key_…`.
    pub api_key: String,
    /// Simple Analytics' `sa_user_id_…`, which its API requires beside the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

impl std::fmt::Debug for Credentials {
    /// Prints nothing of either value: a `Debug` that showed the key would put
    /// it in the first log line or panic message that formatted one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

/// What the create route collected for an API import, as typed.
#[derive(Default, Clone)]
pub struct ApiForm {
    /// The API key.
    pub api_key: Option<String>,
    /// PostHog's address.
    pub api_host: Option<String>,
    /// PostHog's project id.
    pub project: Option<String>,
    /// Simple Analytics' user id.
    pub user_id: Option<String>,
    /// Simple Analytics' website.
    pub hostname: Option<String>,
    /// First day, `YYYY-MM-DD`.
    pub from: Option<String>,
    /// Last day, `YYYY-MM-DD`.
    pub to: Option<String>,
}

impl std::fmt::Debug for ApiForm {
    /// The key and the user id are left out, for the reason [`Credentials`]
    /// gives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiForm")
            .field("api_host", &self.api_host)
            .field("project", &self.project)
            .field("hostname", &self.hostname)
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

impl ApiForm {
    /// True when the form carries an API key, which is what makes an import
    /// an API import.
    pub fn has_key(&self) -> bool {
        self.api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
    }
}

/// Validate an API import's form and split it into what may travel in the job
/// payload and what must be sealed.
///
/// `site_domain` is the Micaforge site's domain, the website a Simple Analytics
/// import reads when none is typed. Every refusal names its field, so the
/// Imports screen can say which box is wrong.
pub fn prepare(
    source: &str,
    form: &ApiForm,
    site_domain: &str,
    today: NaiveDate,
) -> Result<(ApiSource, Credentials)> {
    let (from, to) = window(form.from.as_deref(), form.to.as_deref(), today)?;
    let api_key = single_line("api_key", form.api_key.as_deref())?;

    match source {
        "posthog" => {
            if api_key.starts_with("phc_") {
                return Err(Error::validation(
                    "api_key",
                    "is the project token PostHog uses to receive events, and it cannot read them; \
                     create a personal API key (it starts with phx_) with the Query Read permission",
                ));
            }
            let source = ApiSource::Posthog {
                host: posthog_host(form.api_host.as_deref().unwrap_or_default())?,
                project: posthog_project(form.project.as_deref())?,
                from,
                to,
            };
            Ok((
                source,
                Credentials {
                    api_key,
                    user_id: None,
                },
            ))
        }
        "simple_analytics" => {
            if !api_key.starts_with("sa_api_key_") {
                return Err(Error::validation(
                    "api_key",
                    "is not a Simple Analytics API key; those start with sa_api_key_",
                ));
            }
            let user_id = single_line("user_id", form.user_id.as_deref())?;
            if !user_id.starts_with("sa_user_id_") {
                return Err(Error::validation(
                    "user_id",
                    "is not a Simple Analytics user id; those start with sa_user_id_",
                ));
            }
            let source = ApiSource::SimpleAnalytics {
                hostname: website(form.hostname.as_deref(), site_domain)?,
                from,
                to,
            };
            Ok((
                source,
                Credentials {
                    api_key,
                    user_id: Some(user_id),
                },
            ))
        }
        other => Err(Error::validation(
            "source",
            format!("`{other}` has no API import; export a file from it and upload that"),
        )),
    }
}

/// A key or id: required, one line, no spaces, of a sane length.
fn single_line(field: &str, raw: Option<&str>) -> Result<String> {
    let value = raw.map(str::trim).unwrap_or_default();
    if value.is_empty() {
        return Err(Error::validation(field, "is required for an API import"));
    }
    if value.len() > 512 {
        return Err(Error::validation(
            field,
            "is longer than any key the provider issues",
        ));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::validation(field, "must be one line with no spaces"));
    }
    Ok(value.to_string())
}

/// The days to import: both required, in order, and not past today.
pub fn window(
    from: Option<&str>,
    to: Option<&str>,
    today: NaiveDate,
) -> Result<(NaiveDate, NaiveDate)> {
    let from = day("from", from)?;
    let to = day("to", to)?;
    if from > to {
        return Err(Error::validation(
            "from",
            "is after the last day; swap the two dates",
        ));
    }
    if to > today {
        return Err(Error::validation(
            "to",
            "is in the future; the last day can be today at the latest",
        ));
    }
    Ok((from, to))
}

/// One `YYYY-MM-DD` date.
fn day(field: &str, raw: Option<&str>) -> Result<NaiveDate> {
    let raw = raw.map(str::trim).unwrap_or_default();
    if raw.is_empty() {
        return Err(Error::validation(field, "is required for an API import"));
    }
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| Error::validation(field, "is not a date in the form 2026-08-21"))
}

/// Validate PostHog's address and reduce it to `scheme://host[:port]`.
///
/// Empty means PostHog Cloud US. A bare host gets `https://`. PostHog Cloud is
/// only ever reached over `https`; plain `http` is accepted only for a
/// self-hosted PostHog, whose address the operator typed and whose network
/// they know. The address must be PostHog itself, with no path, query or user
/// name, so a key is never sent somewhere a path quietly redirected. The
/// ingestion hosts and the old `app.` host are rewritten to the API host they
/// stand for, because a key sent to an ingestion host is simply refused.
///
/// The result has passed [`outbound::guard_url`], so a literal local or private
/// address is refused here with a sentence, and a name that resolves to one is
/// refused again when the job connects.
pub fn posthog_host(raw: &str) -> Result<String> {
    let raw = raw.trim().trim_end_matches('/');
    let raw = if raw.is_empty() {
        POSTHOG_DEFAULT_HOST
    } else {
        raw
    };
    let typed = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };

    let url = Url::parse(&typed).map_err(|_| {
        Error::validation(
            "api_host",
            "is not an address; PostHog Cloud's is https://us.posthog.com",
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::validation(
            "api_host",
            "must not carry a user name or password",
        ));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(Error::validation(
            "api_host",
            "must be PostHog's own address, with nothing after the host name",
        ));
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return Err(Error::validation("api_host", "has no host name"));
    };

    let cloud = host == "posthog.com" || host.ends_with(".posthog.com");
    match url.scheme() {
        "https" => {}
        "http" if !cloud => {}
        "http" => {
            return Err(Error::validation(
                "api_host",
                "PostHog Cloud is only served over https",
            ))
        }
        other => {
            return Err(Error::validation(
                "api_host",
                format!("`{other}` is not an address Micaforge can call; use https"),
            ))
        }
    }

    let host = match host.as_str() {
        "app.posthog.com" | "us.i.posthog.com" => "us.posthog.com",
        "eu.i.posthog.com" => "eu.posthog.com",
        other => other,
    };
    let origin = match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    };

    outbound::guard_url(&origin).map_err(|_| {
        Error::validation(
            "api_host",
            "names a local or private address, which Micaforge does not call; a PostHog on \
             your own network needs MICAFORGE_ALLOW_PRIVATE_OUTBOUND",
        )
    })?;
    Ok(origin)
}

/// PostHog's project id: the number in the project's settings and URLs.
fn posthog_project(raw: Option<&str>) -> Result<String> {
    let value = raw.map(str::trim).unwrap_or_default();
    if value.is_empty() {
        return Err(Error::validation(
            "project",
            "is required for a PostHog import",
        ));
    }
    if value.len() > 12 || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::validation(
            "project",
            "is the project's number, as in /project/12345 in PostHog's address bar",
        ));
    }
    Ok(value.to_string())
}

/// The website a Simple Analytics import reads: what was typed, or the site's
/// own domain. A pasted address is reduced to its host.
fn website(raw: Option<&str>, site_domain: &str) -> Result<String> {
    let raw = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(site_domain)
        .trim();
    let host = match Url::parse(raw) {
        Ok(url) if url.has_host() => url.host_str().unwrap_or_default().to_string(),
        _ => raw
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .to_string(),
    };
    let host = host.to_ascii_lowercase();

    let plausible = !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && !host.starts_with(['.', '-'])
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !plausible {
        return Err(Error::validation(
            "hostname",
            "is the website as Simple Analytics lists it, for example example.com",
        ));
    }
    Ok(host)
}

// ---------------------------------------------------------------------------
// Sealing
// ---------------------------------------------------------------------------

/// Seal an import's credentials for `imports.credentials_sealed`.
pub fn seal(cfg: &Config, import: Uuid, credentials: &Credentials) -> Result<Vec<u8>> {
    seal_with(&cfg.derive_key(CREDENTIALS_PURPOSE), import, credentials)
}

/// Open what [`seal`] produced, for the same import.
pub fn open(cfg: &Config, import: Uuid, sealed: &[u8]) -> Result<Credentials> {
    open_with(&cfg.derive_key(CREDENTIALS_PURPOSE), import, sealed)
}

fn seal_with(key: &[u8; 32], import: Uuid, credentials: &Credentials) -> Result<Vec<u8>> {
    let plain = serde_json::to_vec(credentials)?;
    crypto::seal(key, import.as_bytes(), &plain)
}

fn open_with(key: &[u8; 32], import: Uuid, sealed: &[u8]) -> Result<Credentials> {
    let plain = crypto::open(key, import.as_bytes(), sealed)?;
    serde_json::from_slice(&plain)
        .map_err(|_| Error::internal("the stored import credentials are not readable"))
}

// ---------------------------------------------------------------------------
// The download
// ---------------------------------------------------------------------------

/// Read the whole window from the provider into `path`, one record per line.
/// Returns how many records were written.
///
/// Fails when the import no longer holds a key, and when the provider has
/// nothing at all for the window: an import of nothing is far more often a
/// wrong project, website or date than an empty history, and saying so beats
/// a finished import with no rows in it. A partial file is the caller's to
/// remove.
pub async fn download(ctx: &JobCtx, import: Uuid, source: &ApiSource, path: &Path) -> Result<u64> {
    let sealed = imports::credentials(&ctx.pg, import)
        .await?
        .ok_or_else(|| {
            Error::validation(
                "api_key",
                "this import no longer holds an API key, because it finished or was given up; \
             start a new import",
            )
        })?;
    let credentials = open(&ctx.cfg, import, &sealed)?;
    let client = client()?;

    let file = tokio::fs::File::create(path)
        .await
        .map_err(|e| Error::Internal(format!("could not open {}: {e}", path.display())))?;
    let mut scratch = Scratch::new(file);

    match source {
        ApiSource::Posthog {
            host,
            project,
            from,
            to,
        } => {
            posthog(
                &client,
                &mut scratch,
                host,
                project,
                *from,
                *to,
                &credentials,
            )
            .await?
        }
        ApiSource::SimpleAnalytics { hostname, from, to } => {
            simple_analytics(&client, &mut scratch, hostname, *from, *to, &credentials).await?
        }
    }
    let records = scratch.finish(path).await?;

    if records == 0 {
        let (from, to) = match source {
            ApiSource::Posthog { from, to, .. } | ApiSource::SimpleAnalytics { from, to, .. } => {
                (from, to)
            }
        };
        return Err(Error::validation(
            "from",
            format!(
                "{} has nothing to import between {from} and {to}; check the project or website \
                 and the dates, then start a new import",
                source.provider()
            ),
        ));
    }

    tracing::info!(import = %import, provider = source.provider(), records, "API export downloaded");
    Ok(records)
}

/// The client every API import request goes out through.
///
/// Built per import rather than shared, because it differs from the shared
/// client in exactly one way, the one that matters for a request carrying a
/// key: it follows no redirect. See the module documentation.
fn client() -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(crate::fetch::USER_AGENT)
        .timeout(EXPORT_TIMEOUT)
        .connect_timeout(outbound::CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none());
    if !outbound::allow_private() {
        builder = builder
            .dns_resolver(Arc::new(outbound::GuardedResolver))
            .no_proxy();
    }
    builder
        .build()
        .map_err(|e| Error::Internal(format!("could not build the import API client: {e}")))
}

/// The scratch file, with the import's size ceiling enforced as it grows.
struct Scratch {
    file: tokio::fs::File,
    cap: BodyCap,
    records: u64,
}

impl Scratch {
    fn new(file: tokio::fs::File) -> Scratch {
        Scratch {
            file,
            cap: BodyCap::new(MAX_DOWNLOAD_BYTES),
            records: 0,
        }
    }

    /// Append records, one JSON object per line.
    async fn records(&mut self, rows: &[Map<String, Json>]) -> Result<()> {
        let mut page = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut page, row)?;
            page.push(b'\n');
        }
        self.bytes(&page).await?;
        self.records += rows.len() as u64;
        Ok(())
    }

    /// Append raw bytes the provider already wrote one record per line.
    async fn bytes(&mut self, chunk: &[u8]) -> Result<()> {
        self.cap.admit(chunk.len());
        if self.cap.exceeded() {
            return Err(Error::validation(
                "from",
                format!(
                    "the export is larger than {} GB; import a shorter range of days at a time",
                    MAX_DOWNLOAD_BYTES / (1024 * 1024 * 1024)
                ),
            ));
        }
        self.file
            .write_all(chunk)
            .await
            .map_err(|e| Error::Internal(format!("could not write the API export: {e}")))
    }

    async fn finish(mut self, path: &Path) -> Result<u64> {
        self.file
            .flush()
            .await
            .map_err(|e| Error::Internal(format!("could not flush {}: {e}", path.display())))?;
        Ok(self.records)
    }
}

/// Send a request, pausing and repeating it when the provider asks for that.
///
/// `build` makes a fresh request each time, because a sent request is spent.
/// `secrets` are cut out of anything logged from the provider's answer.
async fn send(
    provider: &str,
    target: &Url,
    secrets: &[&str],
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response> {
    let mut pauses = 0;
    loop {
        let response = build()
            .send()
            .await
            .map_err(|e| unreachable_provider(provider, target, &e))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let transient = matches!(status.as_u16(), 429 | 502 | 503 | 504);
        if transient && pauses < PAUSES {
            pauses += 1;
            let pause = retry_after(&response)
                .unwrap_or(DEFAULT_PAUSE)
                .min(MAX_PAUSE);
            tracing::info!(
                provider,
                status = status.as_u16(),
                pause_secs = pause.as_secs(),
                "the provider asked the import to wait"
            );
            drop(response);
            tokio::time::sleep(pause).await;
            continue;
        }
        return Err(refused(provider, response, secrets).await);
    }
}

/// `Retry-After`, when it is a number of seconds.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// A non-success answer, as the import row will show it.
///
/// The row gets the status and what to do about it, and never the provider's
/// body: the same rule the export download follows, so an import cannot be
/// used to read what some server says. The provider's own explanation goes to
/// the log, with the key cut out in case a provider ever echoes it.
async fn refused(provider: &str, response: reqwest::Response, secrets: &[&str]) -> Error {
    let status = response.status().as_u16();
    if (300..400).contains(&status) {
        return Error::Upstream(format!(
            "{provider} answered with a redirect (HTTP {status}), and Micaforge does not send an \
             API key on to another address; use the address it redirects to"
        ));
    }

    if let Ok((body, _)) = outbound::read_capped(response, 4096).await {
        let mut said = serde_json::from_slice::<Json>(&body)
            .ok()
            .and_then(|b| {
                ["detail", "error", "message"]
                    .iter()
                    .find_map(|k| b.get(*k).and_then(Json::as_str).map(str::to_string))
            })
            .unwrap_or_default();
        for secret in secrets.iter().filter(|s| !s.is_empty()) {
            said = said.replace(secret, "[key]");
        }
        said.truncate(300);
        tracing::warn!(provider, status, detail = %said, "the provider refused an import request");
    }

    match status {
        401 | 403 => Error::Upstream(format!(
            "{provider} refused the API key (HTTP {status}); check the key and its permissions, \
             then start a new import"
        )),
        404 => Error::Upstream(format!(
            "{provider} answered HTTP 404; check the project or website, then start a new import"
        )),
        _ => Error::Upstream(format!("{provider} answered HTTP {status}")),
    }
}

/// A request that never got an answer. The coarse kind for the import row, the
/// detail, which names the host but never the path or query, for the log.
fn unreachable_provider(provider: &str, target: &Url, e: &reqwest::Error) -> Error {
    let failure = Failure::from_reqwest(target, e);
    tracing::warn!(provider, error = %failure, "an import API request failed");
    Error::Upstream(format!(
        "could not reach {provider}: {}",
        failure.public_message()
    ))
}

// ---------------------------------------------------------------------------
// PostHog
// ---------------------------------------------------------------------------

/// Where the last page ended. Rows are ordered by timestamp and then by uuid,
/// and the next page starts strictly after this pair, so two events that share
/// a timestamp across a page boundary are neither lost nor read twice.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cursor {
    at: DateTime<Utc>,
    uuid: Uuid,
}

/// Page through PostHog's query API for the window, writing every row.
///
/// Uses `POST /api/projects/{id}/query/` with a HogQL query, which is what
/// PostHog documents for reading events with a personal API key (scope: Query
/// Read). PostHog refuses `OFFSET` paging from API keys and asks for keyset
/// paging on `timestamp`, which is what [`posthog_query`] builds.
async fn posthog(
    client: &reqwest::Client,
    scratch: &mut Scratch,
    host: &str,
    project: &str,
    from: NaiveDate,
    to: NaiveDate,
    credentials: &Credentials,
) -> Result<()> {
    let target = outbound::guard_url(&format!("{host}/api/projects/{project}/query/"))?;
    let start = from.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
    let end = to
        .succ_opt()
        .unwrap_or(to)
        .and_hms_opt(0, 0, 0)
        .unwrap_or_default()
        .and_utc();

    let mut cursor: Option<Cursor> = None;
    let mut limit = PAGE_ROWS;
    loop {
        let body = json!({
            "query": {
                "kind": "HogQLQuery",
                "query": posthog_query(start, end, cursor.as_ref(), limit),
            },
            "refresh": "blocking",
            "name": "micaforge-import",
        });
        let response = send("PostHog", &target, &[&credentials.api_key], || {
            client
                .post(target.clone())
                .bearer_auth(&credentials.api_key)
                .json(&body)
                .timeout(PAGE_TIMEOUT)
        })
        .await?;

        let (bytes, truncated) = outbound::read_capped(response, PAGE_MAX_BYTES).await?;
        if truncated {
            // Events with very large property bags. A smaller page, from the
            // same cursor, until one fits.
            if limit <= MIN_PAGE_ROWS {
                return Err(Error::Upstream(format!(
                    "PostHog's events are larger than {PAGE_MAX_BYTES} bytes per {MIN_PAGE_ROWS}; \
                     export them to a file instead"
                )));
            }
            limit /= 2;
            continue;
        }

        let rows = posthog_rows(&bytes)?;
        scratch.records(&rows).await?;
        if rows.len() < limit {
            return Ok(());
        }

        let next = rows.last().map(cursor_of).transpose()?.ok_or_else(|| {
            Error::Upstream("PostHog's answer ended without a last event".to_string())
        })?;
        if cursor
            .as_ref()
            .is_some_and(|c| next == *c || next.at < c.at)
        {
            return Err(Error::Upstream(
                "PostHog answered the same page twice; the import stopped rather than loop"
                    .to_string(),
            ));
        }
        cursor = Some(next);
    }
}

/// The HogQL for one page.
///
/// The window is half-open, `[start, end)`, in UTC, written as
/// `toDateTime('…', 'UTC')` because PostHog reads a bare datetime literal in
/// the project's own time zone. With the zone given, HogQL parses the literal
/// at microsecond precision, which is the precision of `events.timestamp`, so
/// the cursor comparison is exact.
///
/// Only the events the importer keeps are asked for: `$pageview`,
/// `$web_vitals`, and every event the site named itself. PostHog's other
/// internal events (`$autocapture`, `$pageleave`, `$identify`, `$set`,
/// `$feature_flag_called`, …) have no Micaforge equivalent and would be most of
/// the transfer. Every value interpolated here is a date this module formatted
/// or a [`Uuid`] it parsed, never text from a form or a response.
fn posthog_query(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    cursor: Option<&Cursor>,
    limit: usize,
) -> String {
    let mut query = format!(
        "SELECT uuid, event, timestamp, distinct_id, person_id, properties \
         FROM events \
         WHERE timestamp >= toDateTime('{}', 'UTC') \
         AND timestamp < toDateTime('{}', 'UTC') \
         AND (event IN ('$pageview', '$web_vitals') OR event NOT LIKE '$%')",
        hogql_time(start),
        hogql_time(end),
    );
    if let Some(cursor) = cursor {
        let at = hogql_time(cursor.at);
        query.push_str(&format!(
            " AND (timestamp > toDateTime('{at}', 'UTC') \
             OR (timestamp = toDateTime('{at}', 'UTC') AND uuid > toUUID('{}')))",
            cursor.uuid
        ));
    }
    query.push_str(&format!(" ORDER BY timestamp ASC, uuid ASC LIMIT {limit}"));
    query
}

/// A UTC instant as HogQL reads it at full precision.
fn hogql_time(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S%.6f").to_string()
}

/// PostHog's answer to a HogQL query: column names once, then rows as arrays.
#[derive(Deserialize)]
struct QueryPage {
    columns: Option<Vec<String>>,
    results: Option<Vec<Vec<Json>>>,
}

/// The rows of one answer, each as an object keyed by column name, which is
/// the shape the JSON reader stages.
fn posthog_rows(body: &[u8]) -> Result<Vec<Map<String, Json>>> {
    let page: QueryPage = serde_json::from_slice(body).map_err(|e| {
        Error::Upstream(format!(
            "PostHog answered with something that is not a query result: {e}"
        ))
    })?;
    let (Some(columns), Some(results)) = (page.columns, page.results) else {
        return Err(Error::Upstream(
            "PostHog's answer had no rows and no columns; the query may still be running there"
                .to_string(),
        ));
    };

    results
        .into_iter()
        .map(|row| {
            if row.len() != columns.len() {
                return Err(Error::Upstream(format!(
                    "PostHog answered a row of {} values for {} columns",
                    row.len(),
                    columns.len()
                )));
            }
            Ok(columns.iter().cloned().zip(row).collect())
        })
        .collect()
}

/// Where a row sits in the `(timestamp, uuid)` order.
fn cursor_of(row: &Map<String, Json>) -> Result<Cursor> {
    let at = row
        .get("timestamp")
        .and_then(Json::as_str)
        .and_then(parse_instant);
    let uuid = row
        .get("uuid")
        .and_then(Json::as_str)
        .and_then(|u| Uuid::parse_str(u).ok());
    match (at, uuid) {
        (Some(at), Some(uuid)) => Ok(Cursor { at, uuid }),
        _ => Err(Error::Upstream(
            "PostHog answered an event without a readable timestamp or uuid".to_string(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Simple Analytics
// ---------------------------------------------------------------------------

/// Read page views and then events from Simple Analytics' export API.
///
/// Page views first and then events, each kind in the order Simple Analytics
/// exports it. Both carry Simple Analytics' `session_id`, so an event lands in
/// the same imported session as the page views of that visit.
async fn simple_analytics(
    client: &reqwest::Client,
    scratch: &mut Scratch,
    hostname: &str,
    from: NaiveDate,
    to: NaiveDate,
    credentials: &Credentials,
) -> Result<()> {
    let user_id = credentials.user_id.as_deref().unwrap_or_default();
    for (kind, fields) in [
        ("pageviews", SA_PAGEVIEW_FIELDS),
        ("events", SA_EVENT_FIELDS),
    ] {
        let target = simple_analytics_url(hostname, from, to, kind, fields)?;
        let mut response = send(
            "Simple Analytics",
            &target,
            &[&credentials.api_key, user_id],
            || {
                client
                    .get(target.clone())
                    .header("Api-Key", &credentials.api_key)
                    .header("User-Id", user_id)
                    .header(reqwest::header::CONTENT_TYPE, "application/x-ndjson")
                    .timeout(EXPORT_TIMEOUT)
            },
        )
        .await?;

        // Streamed straight to the file, counting complete lines as records.
        let mut open_line = false;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| unreachable_provider("Simple Analytics", &target, &e))?
        {
            scratch.bytes(&chunk).await?;
            for byte in chunk.iter() {
                if *byte == b'\n' {
                    if open_line {
                        scratch.records += 1;
                    }
                    open_line = false;
                } else if !byte.is_ascii_whitespace() {
                    open_line = true;
                }
            }
        }
        if open_line {
            scratch.bytes(b"\n").await?;
            scratch.records += 1;
        }
    }
    Ok(())
}

/// The export URL for one kind of data point.
///
/// `timezone=UTC`, so a day here is the same day a PostHog import or the
/// dashboard's own range means. `robots=false` leaves robots out at the source;
/// the normaliser drops any `is_robot` row that arrives anyway.
fn simple_analytics_url(
    hostname: &str,
    from: NaiveDate,
    to: NaiveDate,
    kind: &str,
    fields: &[&str],
) -> Result<Url> {
    let mut url = Url::parse(SIMPLE_ANALYTICS_EXPORT)
        .map_err(|e| Error::Internal(format!("the Simple Analytics address is not a URL: {e}")))?;
    url.query_pairs_mut()
        .append_pair("version", "6")
        .append_pair("format", "ndjson")
        .append_pair("hostname", hostname)
        .append_pair("start", &from.format("%Y-%m-%d").to_string())
        .append_pair("end", &to.format("%Y-%m-%d").to_string())
        .append_pair("timezone", "UTC")
        .append_pair("robots", "false")
        .append_pair("type", kind)
        .append_pair("fields", &fields.join(","));
    outbound::guard_url(url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, 8).unwrap()
    }

    fn form(key: &str) -> ApiForm {
        ApiForm {
            api_key: Some(key.into()),
            api_host: None,
            project: Some("12345".into()),
            user_id: Some("sa_user_id_abc".into()),
            hostname: None,
            from: Some("2026-09-01".into()),
            to: Some("2026-09-30".into()),
        }
    }

    #[test]
    fn a_posthog_query_page_becomes_records_keyed_by_column() {
        let rows =
            posthog_rows(include_bytes!("../tests/fixtures/posthog_query_page.json")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["event"], "$pageview");
        assert_eq!(rows[1]["event"], "checkout");
        // The bag stays text; the shaper reads text and objects alike.
        assert!(rows[0]["properties"].as_str().unwrap().contains("/docs"));

        let cursor = cursor_of(&rows[1]).unwrap();
        assert_eq!(
            cursor.uuid.to_string(),
            "018f2a10-0000-7000-8000-000000000032"
        );

        assert!(posthog_rows(br#"{"columns":["a"],"results":[[1,2]]}"#).is_err());
        assert!(posthog_rows(br#"{"detail":"still running"}"#).is_err());
        assert!(posthog_rows(b"<html>").is_err());
    }

    #[test]
    fn the_hogql_reads_a_half_open_window_by_keyset_and_skips_bookkeeping() {
        let start = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 9, 2, 0, 0, 0).unwrap();
        let first = posthog_query(start, end, None, 5000);
        assert!(first.contains("timestamp >= toDateTime('2026-09-01 00:00:00.000000', 'UTC')"));
        assert!(first.contains("timestamp < toDateTime('2026-09-02 00:00:00.000000', 'UTC')"));
        assert!(first.contains("event IN ('$pageview', '$web_vitals') OR event NOT LIKE '$%'"));
        assert!(first.ends_with("ORDER BY timestamp ASC, uuid ASC LIMIT 5000"));
        assert!(!first.contains("uuid >"));

        let cursor = Cursor {
            at: Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap(),
            uuid: Uuid::nil(),
        };
        let next = posthog_query(start, end, Some(&cursor), 250);
        assert!(next.contains("uuid > toUUID('00000000-0000-0000-0000-000000000000')"));
        assert!(next.ends_with("LIMIT 250"));
    }

    #[test]
    fn posthog_addresses_are_reduced_to_the_api_host_and_checked() {
        assert_eq!(posthog_host("").unwrap(), "https://us.posthog.com");
        assert_eq!(
            posthog_host("eu.posthog.com/").unwrap(),
            "https://eu.posthog.com"
        );
        assert_eq!(
            posthog_host("https://app.posthog.com").unwrap(),
            "https://us.posthog.com"
        );
        assert_eq!(
            posthog_host("https://eu.i.posthog.com").unwrap(),
            "https://eu.posthog.com"
        );
        assert!(
            posthog_host("http://us.posthog.com").is_err(),
            "the cloud is https only"
        );
        assert!(
            posthog_host("https://us.posthog.com/api").is_err(),
            "nothing after the host"
        );
        assert!(posthog_host("https://user:pw@posthog.example.org").is_err());
        assert!(posthog_host("ftp://posthog.example.org").is_err());
        assert!(
            posthog_host("http://127.0.0.1:8000").is_err(),
            "no local address by default"
        );
    }

    #[test]
    fn a_form_is_checked_box_by_box_before_anything_is_stored() {
        let (source, credentials) =
            prepare("posthog", &form("phx_personal"), "example.com", today()).unwrap();
        assert!(matches!(source, ApiSource::Posthog { ref project, .. } if project == "12345"));
        assert_eq!(credentials.user_id, None);

        let refused = prepare(
            "posthog",
            &form("phc_project_token"),
            "example.com",
            today(),
        );
        assert!(refused
            .unwrap_err()
            .to_string()
            .contains("personal API key"));

        let (source, credentials) = prepare(
            "simple_analytics",
            &form("sa_api_key_xyz"),
            "example.com",
            today(),
        )
        .unwrap();
        assert!(
            matches!(source, ApiSource::SimpleAnalytics { ref hostname, .. } if hostname == "example.com")
        );
        assert_eq!(credentials.user_id.as_deref(), Some("sa_user_id_abc"));
        assert!(prepare(
            "simple_analytics",
            &form("phx_personal"),
            "example.com",
            today()
        )
        .is_err());

        let mut late = form("phx_personal");
        late.to = Some("2026-10-09".into());
        assert!(
            prepare("posthog", &late, "example.com", today()).is_err(),
            "no future days"
        );
        let mut backwards = form("phx_personal");
        backwards.from = Some("2026-10-01".into());
        assert!(prepare("posthog", &backwards, "example.com", today()).is_err());
        let mut spaced = form("phx personal");
        spaced.api_key = Some("phx personal".into());
        assert!(prepare("posthog", &spaced, "example.com", today()).is_err());
        assert!(prepare("ga4", &form("x"), "example.com", today()).is_err());
    }

    #[test]
    fn credentials_are_sealed_to_one_import_and_never_printed() {
        let key = [3u8; 32];
        let import = Uuid::from_u128(7);
        let credentials = Credentials {
            api_key: "phx_secret".into(),
            user_id: None,
        };
        let sealed = seal_with(&key, import, &credentials).unwrap();
        assert!(!sealed.windows(10).any(|w| w == b"phx_secret"));
        assert_eq!(open_with(&key, import, &sealed).unwrap(), credentials);
        assert!(
            open_with(&key, Uuid::from_u128(8), &sealed).is_err(),
            "bound to its import"
        );
        assert!(!format!("{credentials:?}").contains("phx_secret"));
        let typed = form("sa_api_key_secret");
        assert!(!format!("{typed:?}").contains("secret"));
    }

    #[test]
    fn the_simple_analytics_url_asks_for_one_kind_in_utc_without_robots() {
        let url = simple_analytics_url(
            "example.com",
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap(),
            "events",
            SA_EVENT_FIELDS,
        )
        .unwrap();
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.host_str(), Some("simpleanalytics.com"));
        assert_eq!(pairs["hostname"], "example.com");
        assert_eq!(pairs["start"], "2026-09-01");
        assert_eq!(pairs["end"], "2026-09-30");
        assert_eq!(pairs["timezone"], "UTC");
        assert_eq!(pairs["robots"], "false");
        assert_eq!(pairs["type"], "events");
        assert!(pairs["fields"].contains("datapoint"));
        assert!(
            !pairs["fields"].contains("user_agent"),
            "nothing is asked that is not mapped"
        );
    }
}
