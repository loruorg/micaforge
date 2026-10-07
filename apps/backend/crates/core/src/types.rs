//! The shared domain vocabulary.
//!
//! These are the nouns in `docs/CONTRACT.md`: what a site is, what a dimension
//! is, which metrics exist, and the shape of a filter. Everything serialises to
//! `snake_case` JSON, because the same words appear in the query string, in the
//! saved-segment JSON in Postgres, and in the MCP tool schema, and they have to
//! be spelled the same way in all three.
//!
//! The one rule that matters here: **no method on any of these types ever
//! interpolates caller text into SQL.** Where a value has to reach ClickHouse,
//! it comes back as a bind parameter alongside the expression. See
//! [`Dimension::column_expr`].

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Site
// ---------------------------------------------------------------------------

/// A site's numeric id.
///
/// ClickHouse stores this as `UInt16` in every table, so it is a `u16` here as
/// well rather than the Postgres UUID. Two consequences worth stating: an
/// install is capped at 65,535 sites, and the id is small enough that it is a
/// cheap leading column in every `ORDER BY` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SiteId(pub u16);

impl SiteId {
    /// The underlying number.
    pub fn get(self) -> u16 {
        self.0
    }
}

impl fmt::Display for SiteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for SiteId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        s.trim()
            .parse::<u16>()
            .map(SiteId)
            .map_err(|_| Error::validation("site", "must be a numeric site id"))
    }
}

impl From<u16> for SiteId {
    fn from(v: u16) -> Self {
        SiteId(v)
    }
}

// ---------------------------------------------------------------------------
// Enumerations that appear as column values
// ---------------------------------------------------------------------------

/// What kind of hit an event row records.
///
/// Matches the `kind` column in `events`. `Pageview` is the default because the
/// tracker omits the field for the common case to keep the beacon small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A page was viewed.
    #[default]
    Pageview,
    /// A named custom event, with optional properties.
    Custom,
    /// A click on a link leaving the site.
    Outbound,
    /// A click on a link to a file.
    FileDownload,
    /// A form submission.
    Form,
    /// A client-side error, mirrored into the `errors` table.
    Error,
    /// A Core Web Vitals sample.
    WebVitals,
    /// An engagement heartbeat carrying time and scroll depth.
    Engagement,
}

impl EventKind {
    /// The value as it is written to ClickHouse and to JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Pageview => "pageview",
            EventKind::Custom => "custom",
            EventKind::Outbound => "outbound",
            EventKind::FileDownload => "file_download",
            EventKind::Form => "form",
            EventKind::Error => "error",
            EventKind::WebVitals => "web_vitals",
            EventKind::Engagement => "engagement",
        }
    }

    /// Parse the wire value. An unknown kind is rejected rather than coerced to
    /// `Pageview`, so a mistyped `t` in a payload does not quietly inflate the
    /// headline number.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "pageview" => Ok(EventKind::Pageview),
            "custom" => Ok(EventKind::Custom),
            "outbound" => Ok(EventKind::Outbound),
            "file_download" => Ok(EventKind::FileDownload),
            "form" => Ok(EventKind::Form),
            "error" => Ok(EventKind::Error),
            "web_vitals" => Ok(EventKind::WebVitals),
            "engagement" => Ok(EventKind::Engagement),
            other => Err(Error::validation(
                "kind",
                format!("unknown event kind `{other}`"),
            )),
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The acquisition bucket a visit is attributed to.
///
/// `AnswerEngine` sits beside `Search` rather than inside `Referral`. That
/// placement is the product's whole argument: an arrival from an AI answer is a
/// different act from a click on a search result, and folding the two together
/// is what every other tool does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// No referrer and no campaign.
    #[default]
    Direct,
    /// A traditional search engine.
    Search,
    /// An AI answer surface: ChatGPT, Perplexity, Claude, Gemini, Copilot.
    AnswerEngine,
    /// A social network.
    Social,
    /// Another site.
    Referral,
    /// An email client or a tagged email campaign.
    Email,
    /// A paid campaign, identified by medium or by a click id.
    Paid,
    /// Another page on the same site. Kept rather than dropped so that a
    /// self-referral is visible instead of silently becoming `Direct`.
    Internal,
}

impl Channel {
    /// The value as it is written to ClickHouse and to JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::Direct => "direct",
            Channel::Search => "search",
            Channel::AnswerEngine => "answer_engine",
            Channel::Social => "social",
            Channel::Referral => "referral",
            Channel::Email => "email",
            Channel::Paid => "paid",
            Channel::Internal => "internal",
        }
    }

    /// Parse the wire value.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "direct" => Ok(Channel::Direct),
            "search" => Ok(Channel::Search),
            "answer_engine" => Ok(Channel::AnswerEngine),
            "social" => Ok(Channel::Social),
            "referral" => Ok(Channel::Referral),
            "email" => Ok(Channel::Email),
            "paid" => Ok(Channel::Paid),
            "internal" => Ok(Channel::Internal),
            other => Err(Error::validation(
                "channel",
                format!("unknown channel `{other}`"),
            )),
        }
    }

    /// Every channel, in the order reports list them.
    pub fn all() -> &'static [Channel] {
        &[
            Channel::Direct,
            Channel::Search,
            Channel::AnswerEngine,
            Channel::Social,
            Channel::Referral,
            Channel::Email,
            Channel::Paid,
            Channel::Internal,
        ]
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which population a query is about.
///
/// This is the toggle that makes the wedge visible. It defaults to `Human` so
/// that headline numbers stay comparable with every other analytics tool, and
/// `Agent` switches the same report onto the `agent_events` table rather than
/// filtering the human one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// People in browsers. The default.
    #[default]
    Human,
    /// Named non-human clients: crawlers, fetchers, AI agents.
    Agent,
    /// Both, unioned.
    All,
}

impl Audience {
    /// The value as it appears in the query string and in JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Audience::Human => "human",
            Audience::Agent => "agent",
            Audience::All => "all",
        }
    }

    /// Parse the wire value.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "human" => Ok(Audience::Human),
            "agent" => Ok(Audience::Agent),
            "all" => Ok(Audience::All),
            other => Err(Error::validation(
                "audience",
                format!("unknown audience `{other}`, expected human, agent or all"),
            )),
        }
    }

    /// The table this audience reads from.
    ///
    /// `All` reports `Events`: the union is assembled by the query engine, which
    /// is the only layer that can decide how to merge two different row shapes.
    pub fn table(&self) -> Table {
        match self {
            Audience::Human | Audience::All => Table::Events,
            Audience::Agent => Table::AgentEvents,
        }
    }
}

impl fmt::Display for Audience {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The width of one bucket in a time series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    /// One minute. Realtime and short ranges only.
    Minute,
    /// One hour.
    Hour,
    /// One day. The default for a week-or-longer range.
    #[default]
    Day,
    /// One week, starting Monday.
    Week,
    /// One calendar month.
    Month,
}

impl Granularity {
    /// The value as it appears in the query string and in JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Granularity::Minute => "minute",
            Granularity::Hour => "hour",
            Granularity::Day => "day",
            Granularity::Week => "week",
            Granularity::Month => "month",
        }
    }

    /// Parse the wire value.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "minute" => Ok(Granularity::Minute),
            "hour" => Ok(Granularity::Hour),
            "day" => Ok(Granularity::Day),
            "week" => Ok(Granularity::Week),
            "month" => Ok(Granularity::Month),
            other => Err(Error::validation(
                "granularity",
                format!("unknown granularity `{other}`"),
            )),
        }
    }

    /// Nominal bucket width. Months are not a fixed length, so this is the
    /// planning approximation used for sizing a series, never for placing a
    /// bucket boundary. Boundaries come from
    /// [`time::bucket_expr`](crate::time::bucket_expr), which ClickHouse
    /// evaluates in the site's own zone.
    pub fn approx_duration(&self) -> Duration {
        match self {
            Granularity::Minute => Duration::minutes(1),
            Granularity::Hour => Duration::hours(1),
            Granularity::Day => Duration::days(1),
            Granularity::Week => Duration::days(7),
            Granularity::Month => Duration::days(30),
        }
    }
}

impl fmt::Display for Granularity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A ClickHouse table from `docs/CONTRACT.md` §1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Table {
    /// Human and custom events.
    Events,
    /// Named non-human fetches: the wedge.
    AgentEvents,
    /// Unnamed or suspected non-human traffic.
    BotEvents,
    /// The session rollup.
    Sessions,
    /// Client-side errors, grouped by fingerprint.
    Errors,
    /// The content register the Citation Gap joins both traces against.
    PageIndex,
}

impl Table {
    /// The table's name in ClickHouse.
    ///
    /// The session rollup is `sessions_mv`, spelled as the contract spells it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Table::Events => "events",
            Table::AgentEvents => "agent_events",
            Table::BotEvents => "bot_events",
            Table::Sessions => "sessions_mv",
            Table::Errors => "errors",
            Table::PageIndex => "page_index",
        }
    }
}

impl fmt::Display for Table {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Dimensions
// ---------------------------------------------------------------------------

/// Longest accepted property or flag key.
///
/// Keys arrive from a caller's own tracking code, so they are bounded before
/// they are ever bound into a query: a megabyte-long key in a filter is not a
/// use case, it is a load-generator.
const MAX_KEY_LEN: usize = 128;

/// Something a report can group or filter by.
///
/// The named variants are exactly the list in `docs/CONTRACT.md` §3.2. Two are
/// parametric, because their key space belongs to the site rather than to us:
/// `props.<key>` for custom event properties, and `flag.<key>` for feature-flag
/// exposures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// Path of the page, without host or query string.
    Pathname,
    /// First path of the session.
    EntryPath,
    /// Last path of the session.
    ExitPath,
    /// Host the hit was served from.
    Hostname,
    /// `document.title` at the time of the hit.
    PageTitle,
    /// Raw query string.
    Querystring,
    /// URL fragment.
    Hash,
    /// Full referrer URL.
    Referrer,
    /// Referrer host, which is what reports actually group by.
    ReferrerHost,
    /// Acquisition channel. See [`Channel`].
    Channel,
    /// Resolved source name: `google`, `chatgpt`, `github`.
    Source,
    /// `utm_source`.
    UtmSource,
    /// `utm_medium`.
    UtmMedium,
    /// `utm_campaign`.
    UtmCampaign,
    /// `utm_term`.
    UtmTerm,
    /// `utm_content`.
    UtmContent,
    /// Browser name.
    Browser,
    /// Browser major version.
    BrowserVersion,
    /// Operating system name.
    Os,
    /// Operating system version.
    OsVersion,
    /// `desktop`, `mobile`, `tablet`, `tv`, `wearable`, `console`.
    DeviceType,
    /// Screen width bucketed into a readable class. Derived, not stored.
    ScreenClass,
    /// Browser language tag.
    Language,
    /// Reported IANA timezone.
    Timezone,
    /// ISO-3166 alpha-2 country.
    Country,
    /// Region or subdivision.
    Region,
    /// City.
    City,
    /// Autonomous-system organisation, from the ASN database.
    AsnOrg,
    /// Whether the address belongs to a hosting provider. The strongest single
    /// signal that a "human" hit is not one.
    IsDatacenter,
    /// Custom event name.
    EventName,
    /// A configured goal. Resolved against Postgres, not a column: see
    /// [`Dimension::column_expr`].
    Goal,
    /// The cookieless daily visitor hash.
    VisitorId,
    /// The id supplied by `identify()`, when a site opts in.
    IdentifiedId,
    /// Experiment key.
    Experiment,
    /// Experiment variant.
    Variant,
    /// Revenue attached to the event. Filterable with the numeric operators.
    Revenue,
    /// Catalog slug of a named agent: `gptbot`, `claudebot`, `perplexitybot`.
    AgentId,
    /// The company behind the agent: `openai`, `anthropic`, `perplexity`.
    Operator,
    /// Why the agent fetched: `training`, `rag`, `search`, `preview`, `eval`.
    Purpose,
    /// Whether the source address matched the operator's published ranges.
    Verified,
    /// Whether the site's own `robots.txt` allowed the path that was fetched.
    RobotsAllowed,
    /// HTTP status the fetch received.
    Status,
    /// Response content type.
    ContentType,
    /// A custom event property, written `props.<key>` on the wire.
    Prop(String),
    /// A feature-flag exposure, written `flag.<key>` on the wire.
    Flag(String),
}

impl Dimension {
    /// Parse the wire spelling, including the `props.<key>` and `flag.<key>`
    /// forms.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();

        if let Some(key) = s.strip_prefix("props.") {
            return Ok(Dimension::Prop(check_key("props", key)?));
        }
        // Both spellings are accepted for flags because the contract writes
        // `flag.<key>` while the ClickHouse column is `flags`, and operators
        // reliably type the column name.
        if let Some(key) = s.strip_prefix("flag.").or_else(|| s.strip_prefix("flags.")) {
            return Ok(Dimension::Flag(check_key("flag", key)?));
        }

        let d = match s {
            "pathname" => Dimension::Pathname,
            "entry_path" => Dimension::EntryPath,
            "exit_path" => Dimension::ExitPath,
            "hostname" => Dimension::Hostname,
            "page_title" => Dimension::PageTitle,
            "querystring" => Dimension::Querystring,
            "hash" => Dimension::Hash,
            "referrer" => Dimension::Referrer,
            "referrer_host" => Dimension::ReferrerHost,
            "channel" => Dimension::Channel,
            "source" => Dimension::Source,
            "utm_source" => Dimension::UtmSource,
            "utm_medium" => Dimension::UtmMedium,
            "utm_campaign" => Dimension::UtmCampaign,
            "utm_term" => Dimension::UtmTerm,
            "utm_content" => Dimension::UtmContent,
            "browser" => Dimension::Browser,
            "browser_version" => Dimension::BrowserVersion,
            "os" => Dimension::Os,
            "os_version" => Dimension::OsVersion,
            "device_type" => Dimension::DeviceType,
            "screen_class" => Dimension::ScreenClass,
            "language" => Dimension::Language,
            "timezone" => Dimension::Timezone,
            "country" => Dimension::Country,
            "region" => Dimension::Region,
            "city" => Dimension::City,
            "asn_org" => Dimension::AsnOrg,
            "is_datacenter" => Dimension::IsDatacenter,
            "event_name" => Dimension::EventName,
            "goal" => Dimension::Goal,
            "visitor_id" => Dimension::VisitorId,
            "identified_id" => Dimension::IdentifiedId,
            "experiment" => Dimension::Experiment,
            "variant" => Dimension::Variant,
            "revenue" => Dimension::Revenue,
            "agent_id" => Dimension::AgentId,
            "operator" => Dimension::Operator,
            "purpose" => Dimension::Purpose,
            "verified" => Dimension::Verified,
            "robots_allowed" => Dimension::RobotsAllowed,
            "status" => Dimension::Status,
            "content_type" => Dimension::ContentType,
            other => {
                return Err(Error::validation(
                    "dimension",
                    format!("unknown dimension `{other}`"),
                ))
            }
        };
        Ok(d)
    }

    /// The wire spelling. Round-trips with [`Dimension::parse`].
    pub fn as_str(&self) -> String {
        let fixed = match self {
            Dimension::Pathname => "pathname",
            Dimension::EntryPath => "entry_path",
            Dimension::ExitPath => "exit_path",
            Dimension::Hostname => "hostname",
            Dimension::PageTitle => "page_title",
            Dimension::Querystring => "querystring",
            Dimension::Hash => "hash",
            Dimension::Referrer => "referrer",
            Dimension::ReferrerHost => "referrer_host",
            Dimension::Channel => "channel",
            Dimension::Source => "source",
            Dimension::UtmSource => "utm_source",
            Dimension::UtmMedium => "utm_medium",
            Dimension::UtmCampaign => "utm_campaign",
            Dimension::UtmTerm => "utm_term",
            Dimension::UtmContent => "utm_content",
            Dimension::Browser => "browser",
            Dimension::BrowserVersion => "browser_version",
            Dimension::Os => "os",
            Dimension::OsVersion => "os_version",
            Dimension::DeviceType => "device_type",
            Dimension::ScreenClass => "screen_class",
            Dimension::Language => "language",
            Dimension::Timezone => "timezone",
            Dimension::Country => "country",
            Dimension::Region => "region",
            Dimension::City => "city",
            Dimension::AsnOrg => "asn_org",
            Dimension::IsDatacenter => "is_datacenter",
            Dimension::EventName => "event_name",
            Dimension::Goal => "goal",
            Dimension::VisitorId => "visitor_id",
            Dimension::IdentifiedId => "identified_id",
            Dimension::Experiment => "experiment",
            Dimension::Variant => "variant",
            Dimension::Revenue => "revenue",
            Dimension::AgentId => "agent_id",
            Dimension::Operator => "operator",
            Dimension::Purpose => "purpose",
            Dimension::Verified => "verified",
            Dimension::RobotsAllowed => "robots_allowed",
            Dimension::Status => "status",
            Dimension::ContentType => "content_type",
            Dimension::Prop(key) => return format!("props.{key}"),
            Dimension::Flag(key) => return format!("flag.{key}"),
        };
        fixed.to_string()
    }

    /// The ClickHouse expression that selects this dimension, plus the bind
    /// parameters it needs, in order.
    ///
    /// The pair is the whole point. A property key is caller-controlled text; it
    /// is never pasted into SQL. It comes back as a `?` in the expression and a
    /// `String` in the vector, and the caller binds it the same way it binds a
    /// filter value. Callers must bind the returned parameters **before** any
    /// parameter of their own that appears later in the statement, because
    /// placeholders in this driver are positional.
    ///
    /// Two expressions are not columns:
    ///
    /// - `screen_class` is derived from `screen_w`, so that a report can group
    ///   by "mobile" without the ingest path having to freeze a breakpoint
    ///   table into stored data.
    /// - `goal` has no column at all. Goals live in Postgres with their own
    ///   conditions, so the query engine resolves a goal into a predicate and
    ///   projects it; the expression returned here is the closest underlying
    ///   column and is only correct for event-name goals.
    pub fn column_expr(&self) -> (String, Vec<String>) {
        match self {
            // Custom properties are a JSON column. `toJSONString` gives the
            // extractor a String to work on whichever ClickHouse JSON
            // implementation the install has, and the key stays a bind param.
            Dimension::Prop(key) => (
                "JSONExtractString(toJSONString(props), ?)".to_string(),
                vec![key.clone()],
            ),
            // Flags are a Map(String, String); a missing key yields '' rather
            // than a null, which is what the `is_not_set` operator tests for.
            Dimension::Flag(key) => ("flags[?]".to_string(), vec![key.clone()]),

            Dimension::ScreenClass => (
                concat!(
                    "multiIf(screen_w = 0, 'unknown',",
                    " screen_w < 576, 'mobile',",
                    " screen_w < 992, 'tablet',",
                    " screen_w < 1440, 'laptop',",
                    " 'desktop')"
                )
                .to_string(),
                Vec::new(),
            ),

            // Fixed-width country codes pad with spaces, and a report grouping
            // on the padded value shows a blank row that looks like a bug.
            Dimension::Country => ("trimBoth(toString(country))".to_string(), Vec::new()),

            Dimension::Goal => ("event_name".to_string(), Vec::new()),

            // Everything else is a column whose name is this dimension's own
            // wire spelling, which is not an accident: the contract chose the
            // wire names to match the schema so no mapping table can drift.
            other => (other.as_str(), Vec::new()),
        }
    }

    /// The table this dimension is native to.
    ///
    /// Several names exist on both traces (`hostname`, `pathname`, `country`,
    /// `asn_org`), and they report `Events` here. The query engine already knows
    /// which table it is reading from [`Audience`]; this method only answers
    /// which one *owns* the dimension, which is how a filter that can never
    /// match the selected audience is caught before it reaches ClickHouse.
    pub fn table(&self) -> Table {
        match self {
            Dimension::AgentId
            | Dimension::Operator
            | Dimension::Purpose
            | Dimension::Verified
            | Dimension::RobotsAllowed
            | Dimension::Status
            | Dimension::ContentType => Table::AgentEvents,
            _ => Table::Events,
        }
    }

    /// True when the dimension holds a number, so the numeric operators apply
    /// and quoting the bound value would be wrong.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Dimension::Revenue
                | Dimension::Status
                | Dimension::Verified
                | Dimension::RobotsAllowed
                | Dimension::IsDatacenter
        )
    }
}

/// Reject an empty or oversized property/flag key with the field named.
fn check_key(kind: &str, key: &str) -> Result<String> {
    let key = key.trim();
    if key.is_empty() {
        return Err(Error::validation(
            "dimension",
            format!("`{kind}.` needs a key after the dot"),
        ));
    }
    if key.len() > MAX_KEY_LEN {
        return Err(Error::validation(
            "dimension",
            format!("`{kind}` key is longer than {MAX_KEY_LEN} characters"),
        ));
    }
    Ok(key.to_string())
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl FromStr for Dimension {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Dimension::parse(s)
    }
}

impl Serialize for Dimension {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Dimension {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Dimension::parse(&raw).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Something a report counts.
///
/// The aggregate expressions are held here, not in the query engine, so that the
/// overview card, the time series, the breakdown table and the CSV export cannot
/// each grow their own slightly different definition of "bounce rate". That
/// divergence is the classic way an analytics tool starts contradicting itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// Distinct daily visitor hashes.
    Visitors,
    /// Distinct sessions.
    Sessions,
    /// Pageview events.
    Pageviews,
    /// Share of sessions with a single pageview, as a percentage.
    BounceRate,
    /// Mean session length in seconds.
    AvgDuration,
    /// Mean pageviews per session.
    ViewsPerSession,
    /// All events, of any kind.
    Events,
    /// Events matching the selected goal.
    Conversions,
    /// Converting visitors as a share of all visitors, as a percentage.
    ConversionRate,
    /// Summed revenue.
    Revenue,
    /// Fetches by named agents. The wedge's headline count.
    AgentFetches,
    /// Bytes served to named agents.
    AgentBytes,
    /// Distinct named agents seen.
    UniqueAgents,
    /// Sessions that arrived from an answer engine.
    AnswerArrivals,
    /// Agent fetches per answer-engine arrival. The Citation Gap ratio.
    GapRatio,
}

impl Metric {
    /// The wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Metric::Visitors => "visitors",
            Metric::Sessions => "sessions",
            Metric::Pageviews => "pageviews",
            Metric::BounceRate => "bounce_rate",
            Metric::AvgDuration => "avg_duration",
            Metric::ViewsPerSession => "views_per_session",
            Metric::Events => "events",
            Metric::Conversions => "conversions",
            Metric::ConversionRate => "conversion_rate",
            Metric::Revenue => "revenue",
            Metric::AgentFetches => "agent_fetches",
            Metric::AgentBytes => "agent_bytes",
            Metric::UniqueAgents => "unique_agents",
            Metric::AnswerArrivals => "answer_arrivals",
            Metric::GapRatio => "gap_ratio",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "visitors" => Ok(Metric::Visitors),
            "sessions" => Ok(Metric::Sessions),
            "pageviews" => Ok(Metric::Pageviews),
            "bounce_rate" => Ok(Metric::BounceRate),
            "avg_duration" => Ok(Metric::AvgDuration),
            "views_per_session" => Ok(Metric::ViewsPerSession),
            "events" => Ok(Metric::Events),
            "conversions" => Ok(Metric::Conversions),
            "conversion_rate" => Ok(Metric::ConversionRate),
            "revenue" => Ok(Metric::Revenue),
            "agent_fetches" => Ok(Metric::AgentFetches),
            "agent_bytes" => Ok(Metric::AgentBytes),
            "unique_agents" => Ok(Metric::UniqueAgents),
            "answer_arrivals" => Ok(Metric::AnswerArrivals),
            "gap_ratio" => Ok(Metric::GapRatio),
            other => Err(Error::validation(
                "metric",
                format!("unknown metric `{other}`"),
            )),
        }
    }

    /// The ClickHouse aggregate that computes this metric.
    ///
    /// No caller text is involved, so this is a plain `&'static str`. Three
    /// expressions depend on something the query engine has to project first,
    /// and they are stated here rather than hidden:
    ///
    /// - `conversions` and `conversion_rate` read a boolean column
    ///   `is_conversion`, which the engine builds from the selected goal's
    ///   conditions.
    /// - `gap_ratio` reads `agent_fetches` and `answer_arrivals` from the
    ///   Citation Gap join, because no single table holds both halves. That is
    ///   exactly why no other tool reports this number.
    ///
    /// The session-quality metrics run against [`Table::Sessions`], where one
    /// row is one session; the rest run against the event stream. See
    /// [`Metric::table`].
    pub fn select_expr(&self) -> &'static str {
        match self {
            Metric::Visitors => "uniq(visitor_id)",
            Metric::Sessions => "uniq(session_id)",
            Metric::Pageviews => "countIf(kind = 'pageview')",
            Metric::Events => "count()",
            // Guarding the divisor with nullIf turns an empty range into a null
            // the client renders as "no data", instead of a NaN or a zero that
            // reads as a real measurement.
            Metric::BounceRate => "round(100 * countIf(pageviews <= 1) / nullIf(count(), 0), 2)",
            Metric::AvgDuration => "round(avg(duration_ms) / 1000, 1)",
            Metric::ViewsPerSession => "round(avg(pageviews), 2)",
            Metric::Conversions => "countIf(is_conversion)",
            Metric::ConversionRate => {
                "round(100 * uniqIf(visitor_id, is_conversion) / nullIf(uniq(visitor_id), 0), 2)"
            }
            // Revenue is Decimal64(4) in storage; the sum is widened to a float
            // on the way out so the JSON number is a plain one.
            Metric::Revenue => "toFloat64(sum(ifNull(revenue, 0)))",
            Metric::AgentFetches => "count()",
            Metric::AgentBytes => "sum(bytes)",
            Metric::UniqueAgents => "uniq(agent_id)",
            Metric::AnswerArrivals => "uniqIf(session_id, channel = 'answer_engine')",
            Metric::GapRatio => "round(agent_fetches / nullIf(answer_arrivals, 0), 2)",
        }
    }

    /// The table [`Metric::select_expr`] is written against.
    pub fn table(&self) -> Table {
        match self {
            Metric::BounceRate | Metric::AvgDuration | Metric::ViewsPerSession => Table::Sessions,
            Metric::AgentFetches | Metric::AgentBytes | Metric::UniqueAgents => Table::AgentEvents,
            // The gap is a join keyed on the content register, so that a page
            // with agent traffic and no human traffic still appears.
            Metric::GapRatio => Table::PageIndex,
            _ => Table::Events,
        }
    }

    /// How a client should format the value: a count, a percentage, seconds,
    /// bytes, money, or a bare ratio.
    ///
    /// The unit belongs with the definition. Sending it in `meta` means the
    /// dashboard, the CSV export and the emailed report agree on whether 42
    /// means 42 people or 42 percent.
    pub fn unit(&self) -> MetricUnit {
        match self {
            Metric::BounceRate | Metric::ConversionRate => MetricUnit::Percent,
            Metric::AvgDuration => MetricUnit::Seconds,
            Metric::AgentBytes => MetricUnit::Bytes,
            Metric::Revenue => MetricUnit::Currency,
            Metric::ViewsPerSession | Metric::GapRatio => MetricUnit::Ratio,
            _ => MetricUnit::Count,
        }
    }
}

/// How to render a [`Metric`]'s value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnit {
    /// A whole number of things.
    Count,
    /// Already multiplied by 100.
    Percent,
    /// Seconds.
    Seconds,
    /// Bytes.
    Bytes,
    /// Money, in the site's currency.
    Currency,
    /// A bare ratio.
    Ratio,
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Metric {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Metric::parse(s)
    }
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// How a filter compares its dimension to its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    /// Exact match. With a list value this is "in".
    Is,
    /// Exact non-match. With a list value this is "not in".
    IsNot,
    /// Substring.
    Contains,
    /// Not a substring.
    NotContains,
    /// Prefix.
    StartsWith,
    /// Suffix.
    EndsWith,
    /// Greater than.
    Gt,
    /// Less than.
    Lt,
    /// Greater than or equal.
    Gte,
    /// Less than or equal.
    Lte,
    /// Regular expression, evaluated by ClickHouse (RE2).
    Matches,
    /// The dimension has a non-empty value.
    IsSet,
    /// The dimension is empty or absent.
    IsNotSet,
}

impl FilterOp {
    /// The wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterOp::Is => "is",
            FilterOp::IsNot => "is_not",
            FilterOp::Contains => "contains",
            FilterOp::NotContains => "not_contains",
            FilterOp::StartsWith => "starts_with",
            FilterOp::EndsWith => "ends_with",
            FilterOp::Gt => "gt",
            FilterOp::Lt => "lt",
            FilterOp::Gte => "gte",
            FilterOp::Lte => "lte",
            FilterOp::Matches => "matches",
            FilterOp::IsSet => "is_set",
            FilterOp::IsNotSet => "is_not_set",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "is" => Ok(FilterOp::Is),
            "is_not" => Ok(FilterOp::IsNot),
            "contains" => Ok(FilterOp::Contains),
            "not_contains" => Ok(FilterOp::NotContains),
            "starts_with" => Ok(FilterOp::StartsWith),
            "ends_with" => Ok(FilterOp::EndsWith),
            "gt" => Ok(FilterOp::Gt),
            "lt" => Ok(FilterOp::Lt),
            "gte" => Ok(FilterOp::Gte),
            "lte" => Ok(FilterOp::Lte),
            "matches" => Ok(FilterOp::Matches),
            "is_set" => Ok(FilterOp::IsSet),
            "is_not_set" => Ok(FilterOp::IsNotSet),
            other => Err(Error::validation(
                "filters",
                format!("unknown filter operator `{other}`"),
            )),
        }
    }

    /// True when the operator needs a value to compare against.
    ///
    /// `is_set` and `is_not_set` are the two that do not, and a client that
    /// omits `value` for them is correct rather than malformed.
    pub fn needs_value(&self) -> bool {
        !matches!(self, FilterOp::IsSet | FilterOp::IsNotSet)
    }

    /// True when the operator is an inequality, so the compared value has to be
    /// a number.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            FilterOp::Gt | FilterOp::Lt | FilterOp::Gte | FilterOp::Lte
        )
    }
}

impl fmt::Display for FilterOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The right-hand side of a filter.
///
/// The contract allows `string | number | (string | number)[]`, so this accepts
/// all three shapes and normalises mixed arrays to strings. The distinction that
/// survives is the one the query engine actually needs: is this one value, a
/// set, or a number to compare with an inequality.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    /// A single string.
    One(String),
    /// A set of strings, for `is` and `is_not`.
    Many(Vec<String>),
    /// A number, for the inequality operators.
    Num(f64),
}

impl Default for FilterValue {
    fn default() -> Self {
        FilterValue::One(String::new())
    }
}

impl FilterValue {
    /// Every value as a string, whatever shape it arrived in. This is what the
    /// query engine binds.
    pub fn as_strings(&self) -> Vec<String> {
        match self {
            FilterValue::One(v) => vec![v.clone()],
            FilterValue::Many(v) => v.clone(),
            FilterValue::Num(n) => vec![num_to_string(*n)],
        }
    }

    /// The value as a number, when it is one. A numeric string counts, because
    /// query strings have no types.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            FilterValue::Num(n) => Some(*n),
            FilterValue::One(v) => v.trim().parse::<f64>().ok(),
            FilterValue::Many(_) => None,
        }
    }

    /// True when there is nothing to compare against.
    pub fn is_empty(&self) -> bool {
        match self {
            FilterValue::One(v) => v.is_empty(),
            FilterValue::Many(v) => v.is_empty(),
            FilterValue::Num(_) => false,
        }
    }
}

/// Render a float the way a person would type it, so `5.0` binds as `5`.
fn num_to_string(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

impl Serialize for FilterValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            FilterValue::One(v) => s.serialize_str(v),
            FilterValue::Many(v) => v.serialize(s),
            FilterValue::Num(n) => s.serialize_f64(*n),
        }
    }
}

impl<'de> Deserialize<'de> for FilterValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        /// One element of an array value. Arrays may mix strings and numbers,
        /// which a `Vec<String>` alone would reject.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Scalar {
            S(String),
            N(f64),
            B(bool),
        }

        impl Scalar {
            fn into_string(self) -> String {
                match self {
                    Scalar::S(v) => v,
                    Scalar::N(n) => num_to_string(n),
                    Scalar::B(b) => b.to_string(),
                }
            }
        }

        struct V;

        impl<'de> serde::de::Visitor<'de> for V {
            type Value = FilterValue;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string, a number, or an array of strings and numbers")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                v: &str,
            ) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::One(v.to_string()))
            }

            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::One(v))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::Num(v as f64))
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::Num(v as f64))
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::Num(v))
            }

            fn visit_bool<E: serde::de::Error>(
                self,
                v: bool,
            ) -> std::result::Result<FilterValue, E> {
                // `is_datacenter` and `verified` are UInt8 columns, and clients
                // send them as booleans often enough to be worth accepting.
                Ok(FilterValue::Num(if v { 1.0 } else { 0.0 }))
            }

            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::default())
            }

            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<FilterValue, E> {
                Ok(FilterValue::default())
            }

            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                d: D,
            ) -> std::result::Result<FilterValue, D::Error> {
                FilterValue::deserialize(d)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<FilterValue, A::Error> {
                let mut out = Vec::new();
                while let Some(item) = seq.next_element::<Scalar>()? {
                    out.push(item.into_string());
                }
                Ok(FilterValue::Many(out))
            }
        }

        d.deserialize_any(V)
    }
}

/// One condition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    /// What to compare.
    pub dimension: Dimension,
    /// How to compare it.
    pub op: FilterOp,
    /// What to compare it against. Absent for `is_set` and `is_not_set`.
    #[serde(default)]
    pub value: FilterValue,
}

impl Filter {
    /// Build a filter.
    pub fn new(dimension: Dimension, op: FilterOp, value: FilterValue) -> Self {
        Filter {
            dimension,
            op,
            value,
        }
    }

    /// Check that the operator and the value agree.
    ///
    /// Worth doing before the query is built: ClickHouse would reject
    /// `revenue > 'abc'` with a message about type conversion that means nothing
    /// to the person who typed it into a filter box.
    pub fn validate(&self) -> Result<()> {
        if !self.op.needs_value() {
            return Ok(());
        }
        if self.value.is_empty() {
            return Err(Error::validation(
                "filters",
                format!("`{}` needs a value", self.op),
            ));
        }
        if self.op.is_numeric() && self.value.as_f64().is_none() {
            return Err(Error::validation(
                "filters",
                format!("`{}` needs a number to compare against", self.op),
            ));
        }
        Ok(())
    }
}

/// A filter tree node: either a condition or a nested group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterNode {
    /// A single condition.
    Leaf(Filter),
    /// A nested group with its own logic.
    Group(FilterGroup),
}

/// How the members of a [`FilterGroup`] combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Logic {
    /// All members must match.
    #[default]
    And,
    /// Any member may match.
    Or,
}

impl Logic {
    /// The SQL keyword.
    pub fn as_sql(&self) -> &'static str {
        match self {
            Logic::And => "AND",
            Logic::Or => "OR",
        }
    }

    /// The wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Logic::And => "and",
            Logic::Or => "or",
        }
    }
}

/// A set of filters combined with one logic operator.
///
/// Groups nest, so `(a AND b) OR c` is expressible, which is what
/// `docs/CONTRACT.md` §3.2 requires of a saved segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
// Without this, an untagged `FilterNode` would quietly accept a malformed leaf
// as an empty group: every field here has a default, so any object at all would
// match. A filter the caller got wrong must fail loudly, not filter nothing.
#[serde(deny_unknown_fields)]
pub struct FilterGroup {
    /// How the members combine. Defaults to `and`, matching the flat array form.
    #[serde(default)]
    pub logic: Logic,
    /// The members.
    #[serde(default)]
    pub filters: Vec<FilterNode>,
}

impl FilterGroup {
    /// An `AND` group over the given nodes.
    pub fn and(filters: Vec<FilterNode>) -> Self {
        FilterGroup {
            logic: Logic::And,
            filters,
        }
    }

    /// An `OR` group over the given nodes.
    pub fn or(filters: Vec<FilterNode>) -> Self {
        FilterGroup {
            logic: Logic::Or,
            filters,
        }
    }

    /// True when the group constrains nothing.
    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Validate every condition in the tree.
    pub fn validate(&self) -> Result<()> {
        for node in &self.filters {
            match node {
                FilterNode::Leaf(f) => f.validate()?,
                FilterNode::Group(g) => g.validate()?,
            }
        }
        Ok(())
    }

    /// Every dimension used anywhere in the tree.
    ///
    /// The query engine uses this to decide which tables a query has to touch
    /// before it builds anything.
    pub fn dimensions(&self) -> Vec<Dimension> {
        let mut out = Vec::new();
        self.collect_dimensions(&mut out);
        out
    }

    fn collect_dimensions(&self, out: &mut Vec<Dimension>) {
        for node in &self.filters {
            match node {
                FilterNode::Leaf(f) => out.push(f.dimension.clone()),
                FilterNode::Group(g) => g.collect_dimensions(out),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Time range and the query envelope
// ---------------------------------------------------------------------------

/// Serialise a timezone as its IANA name.
///
/// `chrono-tz`'s own serde support is an optional feature the workspace does not
/// enable, and the name is the only representation worth putting on the wire
/// anyway: it is what the client sent and what ClickHouse expects.
mod tz_serde {
    use chrono_tz::Tz;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::str::FromStr;

    pub fn serialize<S: Serializer>(tz: &Tz, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(tz.name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Tz, D::Error> {
        let raw = String::deserialize(d)?;
        Tz::from_str(&raw).map_err(serde::de::Error::custom)
    }
}

/// A half-open window `[start, end)` and the zone it was expressed in.
///
/// The bounds are UTC because that is what the event store holds. The zone is
/// carried alongside rather than folded in, because bucketing a day, a week or a
/// month is a *local* operation: "yesterday" in Lisbon is not "yesterday" in
/// Auckland, and a daily bar chart that silently uses UTC boundaries is wrong
/// for every operator outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    /// Inclusive start, UTC.
    pub start: DateTime<Utc>,
    /// Exclusive end, UTC.
    pub end: DateTime<Utc>,
    /// The zone the range was expressed in, and the zone buckets are cut in.
    #[serde(with = "tz_serde")]
    pub tz: Tz,
}

impl TimeRange {
    /// Build a range. Bounds are not reordered: a caller that passes them the
    /// wrong way round has a bug, and silently swapping them hides it.
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>, tz: Tz) -> Self {
        TimeRange { start, end, tz }
    }

    /// How long the window is.
    pub fn duration(&self) -> Duration {
        self.end - self.start
    }

    /// Whole days in the window, rounded up. Used for sizing a series and for
    /// choosing a retention warning, never for placing a bucket boundary.
    pub fn days(&self) -> i64 {
        let secs = self.duration().num_seconds().max(0);
        (secs + 86_399) / 86_400
    }

    /// True when the window is empty or inverted.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Which earlier window to compare against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Compare {
    /// No comparison. The default: a delta nobody asked for is a number that
    /// invites a story.
    #[default]
    None,
    /// The window of equal length immediately before this one.
    Previous,
    /// The same calendar window one year earlier.
    Year,
}

impl Compare {
    /// The wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Compare::None => "none",
            Compare::Previous => "previous",
            Compare::Year => "year",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "" | "none" => Ok(Compare::None),
            "previous" => Ok(Compare::Previous),
            "year" => Ok(Compare::Year),
            other => Err(Error::validation(
                "compare",
                format!("unknown comparison `{other}`, expected none, previous or year"),
            )),
        }
    }
}

/// Everything an analytics read is scoped by.
///
/// This is the standard query envelope from `docs/CONTRACT.md` §3.1, already
/// resolved: the string `range=7d` has become real timestamps, the saved segment
/// has been merged into `filters`, and `granularity` has been chosen if the
/// caller left it out. Handlers receive this, not the raw query string, so no
/// endpoint can invent its own interpretation of `7d`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryEnvelope {
    /// The site being read.
    pub site: u16,
    /// The resolved window.
    pub range: TimeRange,
    /// Which earlier window to put beside it.
    #[serde(default)]
    pub compare: Compare,
    /// Bucket width for series endpoints.
    #[serde(default)]
    pub granularity: Granularity,
    /// The filter tree, already merged with any named segment.
    #[serde(default)]
    pub filters: Option<FilterGroup>,
    /// Which population to report on.
    #[serde(default)]
    pub audience: Audience,
    /// Rows per page for list endpoints.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// 1-based page number.
    #[serde(default = "default_page")]
    pub page: u32,
}

/// Enough rows to fill a breakdown table without a second request, small enough
/// that a runaway `limit` cannot be the default.
fn default_limit() -> u32 {
    100
}

fn default_page() -> u32 {
    1
}

impl QueryEnvelope {
    /// A default envelope for one site and window.
    pub fn new(site: u16, range: TimeRange) -> Self {
        QueryEnvelope {
            site,
            range,
            compare: Compare::None,
            granularity: crate::time::auto_granularity(&range),
            filters: None,
            audience: Audience::Human,
            limit: default_limit(),
            page: 1,
        }
    }

    /// Rows to skip for the requested page. Page 0 and page 1 both mean the
    /// first page, because clients disagree about which is which.
    pub fn offset(&self) -> u64 {
        u64::from(self.page.saturating_sub(1)) * u64::from(self.limit)
    }

    /// The table this query reads from.
    pub fn table(&self) -> Table {
        self.audience.table()
    }
}

/// One page of rows, with the total the page was cut from.
///
/// `total` is what lets a client render "1-100 of 4,312" rather than an endless
/// "next" button that eventually returns nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paged<T> {
    /// The rows on this page.
    pub rows: Vec<T>,
    /// How many rows exist across every page.
    pub total: u64,
}

impl<T> Paged<T> {
    /// Build a page.
    pub fn new(rows: Vec<T>, total: u64) -> Self {
        Paged { rows, total }
    }

    /// An empty page. Used for an empty state, never to stand in for a failure.
    pub fn empty() -> Self {
        Paged {
            rows: Vec::new(),
            total: 0,
        }
    }
}

impl<T> Default for Paged<T> {
    fn default() -> Self {
        Paged::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every dimension the contract lists must parse, round-trip, and produce a
    /// bindable expression. This is the test that catches a rename in §3.2.
    #[test]
    fn every_contract_dimension_round_trips() {
        const CONTRACT: &[&str] = &[
            "pathname",
            "entry_path",
            "exit_path",
            "hostname",
            "page_title",
            "querystring",
            "hash",
            "referrer",
            "referrer_host",
            "channel",
            "source",
            "utm_source",
            "utm_medium",
            "utm_campaign",
            "utm_term",
            "utm_content",
            "browser",
            "browser_version",
            "os",
            "os_version",
            "device_type",
            "screen_class",
            "language",
            "timezone",
            "country",
            "region",
            "city",
            "asn_org",
            "is_datacenter",
            "event_name",
            "goal",
            "visitor_id",
            "identified_id",
            "experiment",
            "variant",
            "revenue",
            "agent_id",
            "operator",
            "purpose",
            "verified",
            "robots_allowed",
            "status",
            "content_type",
        ];

        assert!(
            CONTRACT.len() >= 15,
            "the contract promises 15+ filter dimensions"
        );

        for name in CONTRACT {
            let d = Dimension::parse(name).unwrap_or_else(|_| panic!("`{name}` must parse"));
            assert_eq!(&d.as_str(), name, "`{name}` must round-trip");

            let (expr, params) = d.column_expr();
            assert!(!expr.is_empty(), "`{name}` needs an expression");
            assert!(
                params.is_empty(),
                "`{name}` is a fixed column and needs no bind parameters"
            );
        }
    }

    #[test]
    fn property_and_flag_dimensions_bind_their_key() {
        let prop = Dimension::parse("props.plan").unwrap();
        assert_eq!(prop, Dimension::Prop("plan".into()));
        assert_eq!(prop.as_str(), "props.plan");

        let (expr, params) = prop.column_expr();
        assert_eq!(params, vec!["plan".to_string()]);
        assert_eq!(expr.matches('?').count(), 1);

        let flag = Dimension::parse("flag.new_nav").unwrap();
        assert_eq!(flag, Dimension::Flag("new_nav".into()));
        assert_eq!(
            flag.column_expr(),
            ("flags[?]".to_string(), vec!["new_nav".to_string()])
        );
        // The column spelling is accepted too, and normalises to the wire form.
        assert_eq!(Dimension::parse("flags.new_nav").unwrap(), flag);
    }

    /// The property key is caller text. It has to arrive as a bind parameter and
    /// never as part of the expression, or the filter box is an injection point.
    #[test]
    fn a_hostile_property_key_never_reaches_the_expression() {
        let hostile = "x') OR 1=1 --";
        let d = Dimension::parse(&format!("props.{hostile}")).unwrap();
        let (expr, params) = d.column_expr();

        assert!(!expr.contains("OR 1=1"), "the key must not be interpolated");
        assert!(!expr.contains('\''), "the expression must carry no quotes");
        assert_eq!(params, vec![hostile.to_string()]);
    }

    #[test]
    fn malformed_dimensions_are_refused() {
        assert!(Dimension::parse("").is_err());
        assert!(Dimension::parse("props.").is_err());
        assert!(Dimension::parse("flag.").is_err());
        assert!(Dimension::parse("pathnam").is_err());
        assert!(Dimension::parse(&format!("props.{}", "k".repeat(MAX_KEY_LEN + 1))).is_err());
        assert_eq!(
            Dimension::parse("nope").unwrap_err().field(),
            Some("dimension")
        );
    }

    #[test]
    fn agent_dimensions_point_at_the_agent_table() {
        for name in [
            "agent_id",
            "operator",
            "purpose",
            "verified",
            "robots_allowed",
            "status",
            "content_type",
        ] {
            assert_eq!(
                Dimension::parse(name).unwrap().table(),
                Table::AgentEvents,
                "`{name}` lives on the agent trace"
            );
        }
        assert_eq!(Dimension::Pathname.table(), Table::Events);
        assert_eq!(Table::Sessions.as_str(), "sessions_mv");
    }

    #[test]
    fn every_metric_round_trips_and_has_an_aggregate() {
        const ALL: &[Metric] = &[
            Metric::Visitors,
            Metric::Sessions,
            Metric::Pageviews,
            Metric::BounceRate,
            Metric::AvgDuration,
            Metric::ViewsPerSession,
            Metric::Events,
            Metric::Conversions,
            Metric::ConversionRate,
            Metric::Revenue,
            Metric::AgentFetches,
            Metric::AgentBytes,
            Metric::UniqueAgents,
            Metric::AnswerArrivals,
            Metric::GapRatio,
        ];

        for m in ALL {
            assert_eq!(Metric::parse(m.as_str()).unwrap(), *m);
            assert!(m.select_expr().contains('('), "{m} needs an aggregate");
        }

        // A rate is guarded so an empty window reports "no data", not zero.
        assert!(Metric::BounceRate.select_expr().contains("nullIf"));
        assert!(Metric::ConversionRate.select_expr().contains("nullIf"));
        assert_eq!(Metric::BounceRate.unit(), MetricUnit::Percent);
        assert_eq!(Metric::AvgDuration.table(), Table::Sessions);
        assert_eq!(Metric::AgentFetches.table(), Table::AgentEvents);
        assert!(Metric::parse("clicks").is_err());
    }

    #[test]
    fn enum_wire_spellings_match_the_schema() {
        assert_eq!(EventKind::default(), EventKind::Pageview);
        assert_eq!(EventKind::FileDownload.as_str(), "file_download");
        assert_eq!(EventKind::WebVitals.as_str(), "web_vitals");
        assert_eq!(
            EventKind::parse("engagement").unwrap(),
            EventKind::Engagement
        );
        assert!(EventKind::parse("pageviews").is_err());

        assert_eq!(Channel::default(), Channel::Direct);
        assert_eq!(Channel::AnswerEngine.as_str(), "answer_engine");
        assert_eq!(Channel::all().len(), 8);

        assert_eq!(Audience::default(), Audience::Human);
        assert_eq!(Audience::Agent.table(), Table::AgentEvents);
        assert_eq!(Audience::Human.table(), Table::Events);
        assert!(Audience::parse("robots").is_err());

        assert_eq!(Granularity::default(), Granularity::Day);
        assert_eq!(Compare::default(), Compare::None);
        assert_eq!(Logic::default(), Logic::And);
        assert_eq!(Logic::Or.as_sql(), "OR");
    }

    #[test]
    fn json_uses_the_snake_case_wire_spellings() {
        assert_eq!(
            serde_json::to_string(&Channel::AnswerEngine).unwrap(),
            "\"answer_engine\""
        );
        assert_eq!(serde_json::to_string(&SiteId(7)).unwrap(), "7");
        assert_eq!(
            serde_json::to_string(&Dimension::Prop("plan".into())).unwrap(),
            "\"props.plan\""
        );
        assert_eq!(
            serde_json::to_string(&Metric::ViewsPerSession).unwrap(),
            "\"views_per_session\""
        );

        let f: Filter =
            serde_json::from_str(r#"{"dimension":"props.plan","op":"is","value":["pro","team"]}"#)
                .unwrap();
        assert_eq!(f.dimension, Dimension::Prop("plan".into()));
        assert_eq!(f.value.as_strings(), vec!["pro", "team"]);
    }

    #[test]
    fn site_ids_parse_and_display() {
        assert_eq!("42".parse::<SiteId>().unwrap(), SiteId(42));
        assert_eq!(SiteId(42).to_string(), "42");
        assert_eq!(SiteId::from(1).get(), 1);
        assert_eq!("0".parse::<SiteId>().unwrap(), SiteId(0));
        assert!("65536".parse::<SiteId>().is_err(), "a site id is a UInt16");
        assert_eq!("abc".parse::<SiteId>().unwrap_err().field(), Some("site"));
    }

    #[test]
    fn filter_validation_matches_the_operator() {
        let numeric = Filter::new(
            Dimension::Revenue,
            FilterOp::Gt,
            FilterValue::One("ten".into()),
        );
        assert!(numeric.validate().is_err());

        let ok = Filter::new(Dimension::Revenue, FilterOp::Gt, FilterValue::Num(10.0));
        assert!(ok.validate().is_ok());

        let no_value = Filter::new(
            Dimension::UtmSource,
            FilterOp::IsSet,
            FilterValue::default(),
        );
        assert!(no_value.validate().is_ok(), "`is_set` needs no value");

        let missing = Filter::new(Dimension::UtmSource, FilterOp::Is, FilterValue::default());
        assert!(missing.validate().is_err());
    }

    #[test]
    fn the_envelope_paginates_from_page_one() {
        let range = TimeRange::new(
            DateTime::<Utc>::UNIX_EPOCH,
            DateTime::<Utc>::UNIX_EPOCH + Duration::days(7),
            chrono_tz::Tz::UTC,
        );
        let mut q = QueryEnvelope::new(1, range);
        assert_eq!(q.limit, 100);
        assert_eq!(q.page, 1);
        assert_eq!(q.offset(), 0);
        assert_eq!(q.audience, Audience::Human);
        assert_eq!(q.table(), Table::Events);

        q.page = 3;
        assert_eq!(q.offset(), 200);
        // Page 0 and page 1 both mean the first page.
        q.page = 0;
        assert_eq!(q.offset(), 0);

        q.audience = Audience::Agent;
        assert_eq!(q.table(), Table::AgentEvents);
    }

    #[test]
    fn a_time_range_keeps_its_zone_through_json() {
        let range = TimeRange::new(
            DateTime::<Utc>::UNIX_EPOCH,
            DateTime::<Utc>::UNIX_EPOCH + Duration::days(1),
            chrono_tz::Tz::Europe__Lisbon,
        );
        let json = serde_json::to_string(&range).unwrap();
        assert!(json.contains("Europe/Lisbon"));

        let back: TimeRange = serde_json::from_str(&json).unwrap();
        assert_eq!(back, range);
        assert_eq!(back.days(), 1);
        assert!(!back.is_empty());

        assert!(serde_json::from_str::<TimeRange>(
            r#"{"start":"1970-01-01T00:00:00Z","end":"1970-01-02T00:00:00Z","tz":"Mars/Olympus"}"#
        )
        .is_err());
    }

    #[test]
    fn paged_defaults_to_empty_rather_than_to_a_fake_total() {
        let p: Paged<u8> = Paged::default();
        assert!(p.rows.is_empty());
        assert_eq!(p.total, 0);
        assert_eq!(Paged::new(vec![1, 2], 9).total, 9);
    }
}
