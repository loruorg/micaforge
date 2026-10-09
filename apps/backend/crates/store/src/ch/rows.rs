//! One Rust struct per ClickHouse table, in the column order the schema
//! declares.
//!
//! These are wire types, not domain types. Field names are the ClickHouse
//! column names exactly, because `#[derive(clickhouse::Row)]` turns them into
//! the `INSERT INTO t(...)` column list and into the `?fields` expansion on the
//! read side. Renaming a field here silently renames a column, so nothing in
//! this file is free to be tidied for Rust taste.
//!
//! Three encoding decisions are worth stating once, because they are not
//! obvious and they are load-bearing for every row below.
//!
//! **`Map(String, String)` is `Vec<(String, String)>`.** The driver's RowBinary
//! serialiser panics on a serde map and says so: a ClickHouse map is a pair of
//! parallel arrays on the wire, which is a sequence of tuples in serde terms.
//! `url_params` and `flags` are therefore vectors of pairs, not `HashMap`s.
//!
//! **`FixedString(2)` is `[u8; 2]`.** A Rust `String` would be length-prefixed,
//! and a `FixedString` is not: it is exactly N raw bytes. A two-byte array is
//! the only type that lands on the wire correctly in both directions. Use
//! [`country_code`] to build one and [`country_str`] to read one back; a country
//! that is not known is two spaces, matching the column default.
//!
//! **`JSON` is a `String`.** ClickHouse 24.10+ can read and write a `JSON`
//! column as a JSON *string* over the binary formats, which is what
//! [`super::Ch::connect`] switches on. That keeps `props` opaque to this crate:
//! the ingest path hands over a serialised object and the query engine extracts
//! keys server-side with `JSONExtract*`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The scale of every `Decimal64` money column in the schema.
const DECIMAL_SCALE: f64 = 10_000.0;

/// Pack a two-letter country code into the `FixedString(2)` representation.
///
/// Anything that is not exactly two ASCII letters becomes two spaces, which is
/// the column default and the value the `country` dimension trims back to an
/// empty string. Unknown is a real answer here and must not be guessed at.
pub fn country_code(cc: &str) -> [u8; 2] {
    let b = cc.trim().as_bytes();
    if b.len() == 2 && b[0].is_ascii_alphabetic() && b[1].is_ascii_alphabetic() {
        [b[0].to_ascii_uppercase(), b[1].to_ascii_uppercase()]
    } else {
        *b"  "
    }
}

/// Read a `FixedString(2)` country back as a trimmed string.
pub fn country_str(cc: [u8; 2]) -> String {
    String::from_utf8_lossy(&cc).trim().to_string()
}

/// Convert a money amount in major units to the `Decimal64(4)` integer
/// ClickHouse stores.
///
/// Revenue arrives from a tracker as a JSON number, which is a float and cannot
/// be trusted to the cent. Rounding once, here, at a fixed scale of four decimal
/// places, means two ingests of the same value always produce the same row.
pub fn decimal64_4(amount: f64) -> i64 {
    (amount * DECIMAL_SCALE).round() as i64
}

/// Convert a stored `Decimal64(4)` back to major units for display.
pub fn from_decimal64_4(units: i64) -> f64 {
    units as f64 / DECIMAL_SCALE
}

/// A row in `events`: one human or custom hit.
///
/// See `docs/CONTRACT.md` §1.1. Nothing derived from an IP address survives on
/// this row except the coarse geography and the ASN; the address itself is used
/// to compute `visitor_id` and is then dropped, which is non-negotiable §7.1.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct EventRow {
    /// Numeric site id, the leading column of the sort key.
    pub site_id: u16,
    /// When the hit happened, UTC, millisecond precision.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// UUIDv7 assigned at ingest, so a retried beacon can be de-duplicated.
    #[serde(with = "clickhouse::serde::uuid")]
    pub event_id: Uuid,
    /// The 30-minute session window this hit belongs to.
    pub session_id: String,
    /// Daily-rotating salted hash. Never a person, never reversible.
    pub visitor_id: String,
    /// The id a site supplied through `identify()`, when it opted in.
    pub identified_id: String,
    /// `pageview` | `custom` | `outbound` | `file_download` | `form` | `error` |
    /// `web_vitals` | `engagement`.
    ///
    /// A `String` rather than [`micaforge_core::EventKind`] on purpose: the
    /// column is `LowCardinality(String)`, and RowBinary encodes a serde enum as
    /// a variant index, which would write a number where ClickHouse expects a
    /// name. Build it with `EventKind::as_str()`.
    pub kind: String,
    /// Name of a custom event; empty for a pageview.
    pub event_name: String,
    /// Host the hit was served from.
    pub hostname: String,
    /// Path without host or query string.
    pub pathname: String,
    /// `document.title` at the time of the hit.
    pub page_title: String,
    /// Raw query string, retained so a campaign audit can see what was actually
    /// sent rather than only what parsed.
    pub querystring: String,
    /// Parsed query parameters.
    pub url_params: Vec<(String, String)>,
    /// URL fragment, which is a real route in a hash-router SPA.
    pub hash: String,
    /// Full referrer URL.
    pub referrer: String,
    /// Referrer host, which is what reports group by.
    pub referrer_host: String,
    /// Acquisition channel. `answer_engine` is a sibling of `search`, not a kind
    /// of referral: that distinction is the product.
    pub channel: String,
    /// Resolved source name: `google`, `chatgpt`, `github`.
    pub source: String,
    /// `utm_source`.
    pub utm_source: String,
    /// `utm_medium`.
    pub utm_medium: String,
    /// `utm_campaign`.
    pub utm_campaign: String,
    /// `utm_term`.
    pub utm_term: String,
    /// `utm_content`.
    pub utm_content: String,
    /// `gclid` / `fbclid` / `msclkid` / `ttclid`, whichever was present.
    pub click_id: String,
    /// Browser name, from the request's user-agent header.
    pub browser: String,
    /// Browser major version.
    pub browser_version: String,
    /// Operating system name.
    pub os: String,
    /// Operating system version.
    pub os_version: String,
    /// `desktop` | `mobile` | `tablet` | `tv` | `wearable` | `console`.
    pub device_type: String,
    /// Screen width in CSS pixels.
    pub screen_w: u16,
    /// Screen height in CSS pixels.
    pub screen_h: u16,
    /// Viewport width in CSS pixels.
    pub viewport_w: u16,
    /// Viewport height in CSS pixels.
    pub viewport_h: u16,
    /// Browser language tag.
    pub language: String,
    /// IANA timezone the browser reported.
    pub timezone: String,
    /// ISO-3166 alpha-2, two spaces when unknown. See [`country_code`].
    pub country: [u8; 2],
    /// Region or subdivision name.
    pub region: String,
    /// City name.
    pub city: String,
    /// Latitude of the city centroid, never of the visitor.
    pub lat: f32,
    /// Longitude of the city centroid, never of the visitor.
    pub lon: f32,
    /// Autonomous system number.
    pub asn: u32,
    /// Autonomous system organisation.
    pub asn_org: String,
    /// `1` when the address belongs to a hosting provider. The strongest single
    /// signal that a hit counted as human is not one.
    pub is_datacenter: u8,
    /// First path of the session, copied onto every row so the session rollup
    /// can take it without a self-join.
    pub entry_path: String,
    /// Path this hit was on, used by the rollup to resolve the session's exit.
    pub exit_path: String,
    /// Engagement time on the *previous* page, in milliseconds.
    pub duration_ms: u32,
    /// Deepest scroll reached on the previous page, as a percentage.
    pub scroll_pct: u8,
    /// Custom properties, as a serialised JSON object. See the module note.
    pub props: String,
    /// Revenue in `Decimal64(4)` minor units. See [`decimal64_4`].
    pub revenue: Option<i64>,
    /// ISO-4217 currency of `revenue`.
    pub currency: String,
    /// Feature-flag exposures active for this hit.
    pub flags: Vec<(String, String)>,
    /// Experiment key, when the hit was inside one.
    pub experiment: String,
    /// Assigned variant.
    pub variant: String,
    /// Largest Contentful Paint, milliseconds.
    pub lcp: Option<f32>,
    /// Cumulative Layout Shift, unitless.
    pub cls: Option<f32>,
    /// Interaction to Next Paint, milliseconds.
    pub inp: Option<f32>,
    /// First Contentful Paint, milliseconds.
    pub fcp: Option<f32>,
    /// Time To First Byte, milliseconds.
    pub ttfb: Option<f32>,
    /// Set when the row came from a GA4/Plausible/Umami import rather than from
    /// live collection, so an import can be rolled back as a unit.
    #[serde(with = "clickhouse::serde::uuid::option")]
    pub import_id: Option<Uuid>,
    /// Version of the tracker that sent the hit.
    pub sdk_version: String,
    /// The percentage of visitors the site's tracker keeps (`data-sample`),
    /// 1-100. A report over rows below 100 is a sample and says so; the rate is
    /// on the row rather than on the site because a site can change it, and a
    /// window that spans the change holds rows at both rates.
    pub sample_rate: u8,
}

impl Default for EventRow {
    /// An empty pageview at the epoch.
    ///
    /// Every column in `events` has a ClickHouse default, and building a row
    /// field by field from a tracker payload means most of them stay at it. A
    /// `Default` here keeps the ingest path to the fields it actually knows.
    fn default() -> Self {
        Self {
            site_id: 0,
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default(),
            event_id: Uuid::nil(),
            session_id: String::new(),
            visitor_id: String::new(),
            identified_id: String::new(),
            kind: "pageview".to_string(),
            event_name: String::new(),
            hostname: String::new(),
            pathname: String::new(),
            page_title: String::new(),
            querystring: String::new(),
            url_params: Vec::new(),
            hash: String::new(),
            referrer: String::new(),
            referrer_host: String::new(),
            channel: "direct".to_string(),
            source: String::new(),
            utm_source: String::new(),
            utm_medium: String::new(),
            utm_campaign: String::new(),
            utm_term: String::new(),
            utm_content: String::new(),
            click_id: String::new(),
            browser: String::new(),
            browser_version: String::new(),
            os: String::new(),
            os_version: String::new(),
            device_type: String::new(),
            screen_w: 0,
            screen_h: 0,
            viewport_w: 0,
            viewport_h: 0,
            language: String::new(),
            timezone: String::new(),
            country: *b"  ",
            region: String::new(),
            city: String::new(),
            lat: 0.0,
            lon: 0.0,
            asn: 0,
            asn_org: String::new(),
            is_datacenter: 0,
            entry_path: String::new(),
            exit_path: String::new(),
            duration_ms: 0,
            scroll_pct: 0,
            // An empty JSON object, not an empty string: the column is `JSON`
            // and an empty string is not a parseable document.
            props: "{}".to_string(),
            revenue: None,
            currency: String::new(),
            flags: Vec::new(),
            experiment: String::new(),
            variant: String::new(),
            lcp: None,
            cls: None,
            inp: None,
            fcp: None,
            ttfb: None,
            import_id: None,
            sdk_version: String::new(),
            // Everyone: only a hit that says otherwise is a sample.
            sample_rate: 100,
        }
    }
}

/// A row in `agent_events`: one fetch by a client Micaforge could name.
///
/// See `docs/CONTRACT.md` §1.2. Written by the server-side ingest path, because
/// agents do not run JavaScript. This table is the wedge: `robots_allowed` and
/// `llms_listed` are recorded at fetch time so a later policy edit cannot
/// rewrite what was true when the fetch happened.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct AgentEventRow {
    /// Numeric site id.
    pub site_id: u16,
    /// When the fetch happened, UTC, millisecond precision.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// Catalog slug: `gptbot`, `claudebot`, `perplexitybot`.
    pub agent_id: String,
    /// The company behind it: `openai`, `anthropic`, `perplexity`.
    pub operator: String,
    /// `training` | `rag` | `search` | `preview` | `eval` | `unknown`.
    pub purpose: String,
    /// `1` when the source address matched the operator's published ranges or
    /// reverse-DNS suffix. An unverified `GPTBot` string is a claim, not a fact.
    pub verified: u8,
    /// The raw user-agent, kept whole: it is the evidence behind the
    /// classification and an operator has to be able to audit it.
    pub user_agent: String,
    /// HTTP method.
    pub method: String,
    /// Host that was fetched.
    pub hostname: String,
    /// Path that was fetched.
    pub pathname: String,
    /// Query string that was fetched.
    pub querystring: String,
    /// Status the fetch received.
    pub status: u16,
    /// Response size in bytes, which is what "how much was taken" is measured
    /// in.
    pub bytes: u32,
    /// Server-side response time in milliseconds.
    pub duration_ms: u32,
    /// Response content type.
    pub content_type: String,
    /// Autonomous system number of the fetching address.
    pub asn: u32,
    /// Autonomous system organisation.
    pub asn_org: String,
    /// ISO-3166 alpha-2 of the fetching address. See [`country_code`].
    pub country: [u8; 2],
    /// Salted, rotating digest of the address. Never the address.
    pub ip_hash: String,
    /// `-1` unknown, `0` disallowed by `robots.txt`, `1` allowed.
    pub robots_allowed: i8,
    /// `-1` no `llms.txt`, `0` absent from it, `1` listed in it.
    pub llms_listed: i8,
    /// `1` when the request carried `If-None-Match` or `If-Modified-Since`, so a
    /// re-crawl check can be told apart from a fresh read.
    pub conditional: u8,
}

impl Default for AgentEventRow {
    fn default() -> Self {
        Self {
            site_id: 0,
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default(),
            agent_id: String::new(),
            operator: String::new(),
            purpose: "unknown".to_string(),
            verified: 0,
            user_agent: String::new(),
            method: "GET".to_string(),
            hostname: String::new(),
            pathname: String::new(),
            querystring: String::new(),
            status: 200,
            bytes: 0,
            duration_ms: 0,
            content_type: String::new(),
            asn: 0,
            asn_org: String::new(),
            country: *b"  ",
            ip_hash: String::new(),
            // Unknown, not allowed. A policy question with no answer must never
            // default to the permissive one.
            robots_allowed: -1,
            llms_listed: -1,
            conditional: 0,
        }
    }
}

/// A row in `bot_events`: traffic that is not human and could not be named.
///
/// See `docs/CONTRACT.md` §1.3. The column set is `agent_events` minus the three
/// fields that only exist once a client has been identified, plus the evidence
/// that made Micaforge suspect it. Kept for three months and no longer: this is
/// working material for tuning detection, not a report anyone reads.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct BotEventRow {
    /// Numeric site id.
    pub site_id: u16,
    /// When the fetch happened, UTC, millisecond precision.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// Catalog slug when a weak match existed, empty otherwise.
    pub agent_id: String,
    /// The raw user-agent.
    pub user_agent: String,
    /// HTTP method.
    pub method: String,
    /// Host that was fetched.
    pub hostname: String,
    /// Path that was fetched.
    pub pathname: String,
    /// Query string that was fetched.
    pub querystring: String,
    /// Status the fetch received.
    pub status: u16,
    /// Response size in bytes.
    pub bytes: u32,
    /// Server-side response time in milliseconds.
    pub duration_ms: u32,
    /// Response content type.
    pub content_type: String,
    /// Autonomous system number.
    pub asn: u32,
    /// Autonomous system organisation.
    pub asn_org: String,
    /// ISO-3166 alpha-2. See [`country_code`].
    pub country: [u8; 2],
    /// Salted, rotating digest of the address.
    pub ip_hash: String,
    /// `-1` unknown, `0` disallowed, `1` allowed.
    pub robots_allowed: i8,
    /// `-1` no `llms.txt`, `0` absent, `1` listed.
    pub llms_listed: i8,
    /// `1` for a conditional request.
    pub conditional: u8,
    /// Comma-separated detector names that fired, so a false positive can be
    /// traced back to the rule that caused it.
    pub reasons: String,
    /// Confidence that this was not a person, 0-100.
    pub score: u8,
}

impl Default for BotEventRow {
    fn default() -> Self {
        Self {
            site_id: 0,
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default(),
            agent_id: String::new(),
            user_agent: String::new(),
            method: "GET".to_string(),
            hostname: String::new(),
            pathname: String::new(),
            querystring: String::new(),
            status: 200,
            bytes: 0,
            duration_ms: 0,
            content_type: String::new(),
            asn: 0,
            asn_org: String::new(),
            country: *b"  ",
            ip_hash: String::new(),
            robots_allowed: -1,
            llms_listed: -1,
            conditional: 0,
            reasons: String::new(),
            score: 0,
        }
    }
}

/// A row in `replay_events`: one rrweb event in a recorded session.
///
/// See `docs/CONTRACT.md` §1.5. `data` is the rrweb payload verbatim and is
/// stored `ZSTD(3)`, because a full-snapshot event is large, highly repetitive
/// DOM text and compresses by roughly an order of magnitude.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct ReplayRow {
    /// Numeric site id.
    pub site_id: u16,
    /// Session being replayed. Leads the sort key after `site_id`, because a
    /// replay is always read as one whole session and never scanned across
    /// sessions.
    pub session_id: String,
    /// When the event was recorded, UTC, millisecond precision.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// Monotonic sequence within the session, which resolves ties when several
    /// events share a millisecond.
    pub seq: u32,
    /// rrweb event type.
    #[serde(rename = "type")]
    pub event_type: u8,
    /// The rrweb payload, serialised.
    pub data: String,
}

/// A row in `errors`: one client-side exception.
///
/// See `docs/CONTRACT.md` §1.6. `fingerprint` is computed by the ingest path
/// rather than at query time, so grouping stays stable when the stack text
/// changes between releases.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct ErrorRow {
    /// Numeric site id.
    pub site_id: u16,
    /// When the error was thrown, UTC, millisecond precision.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// Session the error happened in, so it can be replayed.
    pub session_id: String,
    /// Visitor hash, for "how many people hit this".
    pub visitor_id: String,
    /// Error constructor name: `TypeError`, `ChunkLoadError`.
    pub name: String,
    /// Error message.
    pub message: String,
    /// Stack trace as reported.
    pub stack: String,
    /// Source file the error came from.
    pub filename: String,
    /// Line number.
    pub lineno: u32,
    /// Column number.
    pub colno: u32,
    /// Page the error happened on.
    pub pathname: String,
    /// Browser name.
    pub browser: String,
    /// Operating system name.
    pub os: String,
    /// Release identifier the site reported, so a regression can be pinned to a
    /// deploy.
    pub release: String,
    /// `1` when the site caught it itself, `0` when it reached `window.onerror`.
    pub handled: u8,
    /// Stable grouping key across releases.
    pub fingerprint: String,
}

/// A row in `page_index`: one known URL on the site.
///
/// See `docs/CONTRACT.md` §1.7. This is the register the Citation Gap joins both
/// traces against, and the reason it exists: a page that agents crawled and no
/// person ever visited has no row in `events`, so without this table it would be
/// invisible in exactly the report it matters most to.
///
/// The engine is `ReplacingMergeTree(updated_at)`, so re-registering a page is
/// an insert and the newest `updated_at` wins.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct PageIndexRow {
    /// Numeric site id.
    pub site_id: u16,
    /// The path this row describes.
    pub pathname: String,
    /// Page title, from the crawl.
    pub title: String,
    /// When the crawler first saw this path.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub first_seen: DateTime<Utc>,
    /// `Last-Modified` as served, when the site sends one. Absent is a real
    /// answer and is stored as null rather than as the epoch.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis::option")]
    pub last_modified: Option<DateTime<Utc>>,
    /// Word count of the extracted main content, which is the crude proxy for
    /// "how much of this page an agent actually took".
    pub word_count: u32,
    /// `1` when the path appears in the site's sitemap.
    pub in_sitemap: u8,
    /// `1` when the path appears in the site's `llms.txt`.
    pub in_llms_txt: u8,
    /// The `robots.txt` rule that applies to this path, as written.
    pub robots_directive: String,
    /// Version column for the replacing merge.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub updated_at: DateTime<Utc>,
}

/// A row in `imports_raw`: one untouched record from a third-party export.
///
/// See `docs/CONTRACT.md` §1.8. The importer stages here first and normalises
/// into `events` afterwards. Keeping the original payload means a mapping bug is
/// re-runnable instead of being a re-upload, and it is the only place a GA4 row
/// survives in the shape Google wrote it.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct ImportRawRow {
    /// The `imports` job this record belongs to.
    #[serde(with = "clickhouse::serde::uuid")]
    pub import_id: Uuid,
    /// Numeric site id the import targets.
    pub site_id: u16,
    /// Timestamp carried by the source record, UTC.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// `ga4` | `plausible` | `umami`.
    pub source: String,
    /// Position in the source file, so a partial import can resume.
    pub seq: u64,
    /// The source record, verbatim.
    pub payload: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_codes_round_trip_and_reject_junk() {
        assert_eq!(country_code("pt"), *b"PT");
        assert_eq!(country_str(country_code("pt")), "PT");
        // Unknown must stay unknown rather than become a plausible-looking code.
        assert_eq!(country_code(""), *b"  ");
        assert_eq!(country_code("PRT"), *b"  ");
        assert_eq!(country_code("1"), *b"  ");
        assert_eq!(country_str(*b"  "), "");
    }

    #[test]
    fn decimal_scale_survives_a_round_trip() {
        assert_eq!(decimal64_4(19.99), 199_900);
        assert!((from_decimal64_4(decimal64_4(19.99)) - 19.99).abs() < f64::EPSILON);
        // The classic float trap: 0.1 + 0.2 must still store as 0.3000.
        assert_eq!(decimal64_4(0.1 + 0.2), 3_000);
    }

    #[test]
    fn column_names_match_the_contract() {
        use clickhouse::Row;
        // The derive turns field names into the INSERT column list, so this is
        // the check that a rename in Rust cannot quietly rename a column.
        assert_eq!(EventRow::COLUMN_NAMES[0], "site_id");
        assert_eq!(EventRow::COLUMN_NAMES[1], "timestamp");
        assert_eq!(EventRow::COLUMN_NAMES.len(), 61);
        assert_eq!(EventRow::COLUMN_NAMES[60], "sample_rate");
        assert_eq!(AgentEventRow::COLUMN_NAMES.len(), 22);
        assert_eq!(BotEventRow::COLUMN_NAMES.len(), 21);
        // `type` is a Rust keyword, so the field is renamed for serde and the
        // column name has to survive that.
        assert!(ReplayRow::COLUMN_NAMES.contains(&"type"));
    }

    #[test]
    fn a_default_event_is_a_valid_pageview() {
        let row = EventRow::default();
        assert_eq!(row.kind, "pageview");
        assert_eq!(row.channel, "direct");
        // `props` is a JSON column; an empty string would not parse.
        assert_eq!(row.props, "{}");
    }
}
