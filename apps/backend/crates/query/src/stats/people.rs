//! Sessions and visitors, as lists and as records.
//!
//! Both lists are built from the event stream rather than from `sessions_mv`,
//! and that is a deliberate cost. The stored rollup is faster, but it carries
//! none of the event-level columns a filter can name, so a filtered session list
//! read from it would quietly ignore half the filters. Grouping the filtered
//! stream is slower and correct.
//!
//! Nothing here is a person. A `visitor_id` is a salted hash that rotates every
//! day, so a "visitor record" is one day of one browser's activity and the
//! `identified_id` beside it exists only when a site opted in by calling
//! `identify()`.

use chrono::{DateTime, Utc};
use clickhouse::Row;
use micaforge_core::{Error, Paged, QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::{fetch_all, fetch_optional, ms_to_dt, round2};
use crate::sql::{millis, scope_predicate, Param};

/// Most hits returned for one session or one visitor.
///
/// A record view is read, not scrolled forever. A session with more hits than
/// this is a bot or a stuck tab, and either way the first thousand hits answer
/// the question.
const MAX_RECORD_HITS: u32 = 1_000;

/// Most sessions listed inside one visitor record.
const MAX_VISITOR_SESSIONS: u32 = 100;

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    /// The session id.
    pub id: String,
    /// The daily visitor hash behind it.
    pub visitor_id: String,
    /// The id supplied by `identify()`, empty unless the site opted in.
    pub identified_id: String,
    /// First hit.
    pub started: DateTime<Utc>,
    /// Last hit.
    pub ended: DateTime<Utc>,
    /// Total engagement time across the session, in seconds.
    pub duration_s: f64,
    /// Pageviews.
    pub pageviews: u64,
    /// All events, pageviews included.
    pub events: u64,
    /// First path.
    pub entry_path: String,
    /// Last path.
    pub exit_path: String,
    /// Acquisition channel.
    pub channel: String,
    /// Resolved source.
    pub source: String,
    /// Referring host.
    pub referrer_host: String,
    /// ISO-3166 alpha-2.
    pub country: String,
    /// Region or subdivision.
    pub region: String,
    /// City.
    pub city: String,
    /// Device class.
    pub device_type: String,
    /// Browser.
    pub browser: String,
    /// Operating system.
    pub os: String,
    /// True when the session saw one page or fewer.
    pub is_bounce: bool,
    /// Revenue attached to the session, in major units.
    pub revenue: f64,
}

#[derive(Debug, Row, Deserialize)]
struct SessionRawRow {
    id: String,
    vid: String,
    ident: String,
    started: i64,
    ended: i64,
    pv: u64,
    ev: u64,
    entry_p: String,
    exit_p: String,
    chan: String,
    src: String,
    refhost: String,
    cc: String,
    reg: String,
    cty: String,
    dev: String,
    brw: String,
    osn: String,
    dur: u64,
    rev: f64,
}

impl From<SessionRawRow> for SessionRow {
    fn from(r: SessionRawRow) -> Self {
        SessionRow {
            id: r.id,
            visitor_id: r.vid,
            identified_id: r.ident,
            started: ms_to_dt(r.started),
            ended: ms_to_dt(r.ended),
            duration_s: round2(r.dur as f64 / 1000.0),
            is_bounce: r.pv <= 1,
            pageviews: r.pv,
            events: r.ev,
            entry_path: r.entry_p,
            exit_path: r.exit_p,
            channel: r.chan,
            source: r.src,
            referrer_host: r.refhost,
            country: r.cc,
            region: r.reg,
            city: r.cty,
            device_type: r.dev,
            browser: r.brw,
            os: r.osn,
            revenue: round2(r.rev),
        }
    }
}

/// One hit inside a record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    /// When, epoch milliseconds.
    pub t: i64,
    /// Event kind.
    pub kind: String,
    /// Custom event name, empty for a pageview.
    pub event_name: String,
    /// Path.
    pub pathname: String,
    /// Title at the time of the hit.
    pub page_title: String,
    /// Full referrer.
    pub referrer: String,
    /// Engagement time carried on this hit, in milliseconds. Per
    /// `docs/CONTRACT.md` §1.1 this measures the *previous* page.
    pub duration_ms: u32,
    /// Furthest scroll depth reported, as a percentage.
    pub scroll_pct: u8,
    /// The event's `props` object, as JSON text. Kept opaque: the keys belong to
    /// the site's own tracking code and this crate does not have a schema for
    /// them.
    pub props: String,
}

#[derive(Debug, Row, Deserialize)]
struct HitRawRow {
    t: i64,
    kind: String,
    event_name: String,
    pathname: String,
    page_title: String,
    referrer: String,
    duration_ms: u32,
    scroll_pct: u8,
    props: String,
}

impl From<HitRawRow> for Hit {
    fn from(r: HitRawRow) -> Self {
        Hit {
            t: r.t,
            kind: r.kind,
            event_name: r.event_name,
            pathname: r.pathname,
            page_title: r.page_title,
            referrer: r.referrer,
            duration_ms: r.duration_ms,
            scroll_pct: r.scroll_pct,
            props: r.props,
        }
    }
}

/// One session, in full.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    /// The session's summary row.
    pub meta: SessionRow,
    /// Every hit, oldest first.
    pub hits: Vec<Hit>,
}

/// One visitor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisitorRow {
    /// The daily visitor hash.
    pub visitor_id: String,
    /// The id supplied by `identify()`, empty unless the site opted in.
    pub identified_id: String,
    /// First hit inside the window.
    pub first_seen: DateTime<Utc>,
    /// Last hit inside the window.
    pub last_seen: DateTime<Utc>,
    /// Sessions inside the window.
    pub sessions: u64,
    /// Pageviews.
    pub pageviews: u64,
    /// All events.
    pub events: u64,
    /// ISO-3166 alpha-2.
    pub country: String,
    /// Device class.
    pub device_type: String,
    /// Browser.
    pub browser: String,
    /// Operating system.
    pub os: String,
    /// Acquisition channel of the first session seen.
    pub channel: String,
    /// Resolved source.
    pub source: String,
    /// Revenue attributed, in major units.
    pub revenue: f64,
}

#[derive(Debug, Row, Deserialize)]
struct VisitorRawRow {
    vid: String,
    ident: String,
    first_seen: i64,
    last_seen: i64,
    sessions: u64,
    pv: u64,
    ev: u64,
    cc: String,
    dev: String,
    brw: String,
    osn: String,
    chan: String,
    src: String,
    rev: f64,
}

impl From<VisitorRawRow> for VisitorRow {
    fn from(r: VisitorRawRow) -> Self {
        VisitorRow {
            visitor_id: r.vid,
            identified_id: r.ident,
            first_seen: ms_to_dt(r.first_seen),
            last_seen: ms_to_dt(r.last_seen),
            sessions: r.sessions,
            pageviews: r.pv,
            events: r.ev,
            country: r.cc,
            device_type: r.dev,
            browser: r.brw,
            os: r.osn,
            channel: r.chan,
            source: r.src,
            revenue: round2(r.rev),
        }
    }
}

/// One visitor, in full.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisitorDetail {
    /// The visitor's summary row.
    pub profile: VisitorRow,
    /// Their sessions, newest first.
    pub sessions: Vec<SessionRow>,
    /// Their most recent hits, oldest first.
    pub recent: Vec<Hit>,
}

#[derive(Debug, Row, Deserialize)]
struct ScalarU64Row {
    v: u64,
}

// ---------------------------------------------------------------------------
// The session projection
// ---------------------------------------------------------------------------

/// The `SELECT` list every session row is built from.
///
/// Every alias here is a name no `events` column uses. ClickHouse resolves an
/// alias that shadows a column of the same name by looking at the alias, which
/// turns `any(city) AS city` into a cycle; renaming is cheaper than debugging
/// that once.
fn session_projection() -> String {
    format!(
        "  session_id AS id,
  any(visitor_id) AS vid,
  argMax(identified_id, timestamp) AS ident,
  {started} AS started,
  {ended} AS ended,
  toUInt64(countIf(kind = 'pageview')) AS pv,
  toUInt64(count()) AS ev,
  argMin(pathname, timestamp) AS entry_p,
  argMax(pathname, timestamp) AS exit_p,
  any(toString(channel)) AS chan,
  any(toString(source)) AS src,
  any(referrer_host) AS refhost,
  any(trimBoth(toString(country))) AS cc,
  any(toString(region)) AS reg,
  any(city) AS cty,
  any(toString(device_type)) AS dev,
  any(toString(browser)) AS brw,
  any(toString(os)) AS osn,
  toUInt64(sum(duration_ms)) AS dur,
  toFloat64(max(ifNull(revenue, 0))) AS rev",
        started = millis("min(timestamp)"),
        ended = millis("max(timestamp)"),
    )
}

/// The `SELECT` list for one hit.
fn hit_projection() -> String {
    format!(
        "  {t} AS t,
  kind AS hit_kind,
  event_name AS ev_name,
  pathname AS path,
  page_title AS title,
  referrer AS ref,
  duration_ms AS dur_ms,
  scroll_pct AS scroll,
  toJSONString(props) AS props_json",
        t = millis("timestamp"),
    )
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/// `GET /api/stats/sessions`.
pub async fn sessions(ch: &Ch, env: &QueryEnvelope) -> Result<Paged<SessionRow>> {
    let (scope, params) = scope_predicate(env, Table::Events)?;

    let total = fetch_optional::<ScalarU64Row>(
        ch,
        "sessions_total",
        &format!("SELECT uniq(session_id) AS v FROM events WHERE {scope}"),
        &params,
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope}
GROUP BY session_id
ORDER BY started DESC
LIMIT {limit} OFFSET {offset}",
        projection = session_projection(),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let rows = fetch_all::<SessionRawRow>(ch, "sessions", &sql, &params).await?;
    Ok(Paged::new(
        rows.into_iter().map(SessionRow::from).collect(),
        total,
    ))
}

/// `GET /api/stats/sessions/:id`.
///
/// Scoped to the envelope's window as well as the id, so the read still prunes
/// partitions. A caller who followed a link from the session list is inside that
/// window by construction.
pub async fn session_detail(
    ch: &Ch,
    env: &QueryEnvelope,
    session_id: &str,
) -> Result<SessionDetail> {
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;

    let mut params = scope_params.clone();
    params.push(Param::Str(session_id.to_string()));

    let meta_sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope} AND session_id = ?
GROUP BY session_id",
        projection = session_projection(),
    );

    let meta = fetch_optional::<SessionRawRow>(ch, "session_detail", &meta_sql, &params)
        .await?
        .map(SessionRow::from)
        .ok_or_else(|| Error::not_found(format!("session {session_id}")))?;

    let hits_sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope} AND session_id = ?
ORDER BY timestamp ASC
LIMIT {MAX_RECORD_HITS}",
        projection = hit_projection(),
    );

    let hits = fetch_all::<HitRawRow>(ch, "session_hits", &hits_sql, &params)
        .await?
        .into_iter()
        .map(Hit::from)
        .collect();

    Ok(SessionDetail { meta, hits })
}

/// `GET /api/stats/visitors`.
pub async fn visitors(ch: &Ch, env: &QueryEnvelope) -> Result<Paged<VisitorRow>> {
    let (scope, params) = scope_predicate(env, Table::Events)?;

    let total = fetch_optional::<ScalarU64Row>(
        ch,
        "visitors_total",
        &format!("SELECT uniq(visitor_id) AS v FROM events WHERE {scope}"),
        &params,
    )
    .await?
    .map(|r| r.v)
    .unwrap_or(0);

    let sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope}
GROUP BY visitor_id
ORDER BY last_seen DESC
LIMIT {limit} OFFSET {offset}",
        projection = visitor_projection(),
        limit = env.limit.clamp(1, 10_000),
        offset = env.offset(),
    );

    let rows = fetch_all::<VisitorRawRow>(ch, "visitors", &sql, &params).await?;
    Ok(Paged::new(
        rows.into_iter().map(VisitorRow::from).collect(),
        total,
    ))
}

/// The `SELECT` list every visitor row is built from.
fn visitor_projection() -> String {
    format!(
        "  visitor_id AS vid,
  argMax(identified_id, timestamp) AS ident,
  {first} AS first_seen,
  {last} AS last_seen,
  uniq(session_id) AS sessions,
  toUInt64(countIf(kind = 'pageview')) AS pv,
  toUInt64(count()) AS ev,
  any(trimBoth(toString(country))) AS cc,
  any(toString(device_type)) AS dev,
  any(toString(browser)) AS brw,
  any(toString(os)) AS osn,
  any(toString(channel)) AS chan,
  any(toString(source)) AS src,
  toFloat64(sum(ifNull(revenue, 0))) AS rev",
        first = millis("min(timestamp)"),
        last = millis("max(timestamp)"),
    )
}

/// `GET /api/stats/visitors/:id`.
///
/// `id` matches either the daily visitor hash or an `identify()` id, because a
/// caller following a link from a list has one of the two and should not have to
/// know which.
pub async fn visitor_detail(ch: &Ch, env: &QueryEnvelope, id: &str) -> Result<VisitorDetail> {
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;

    // The id is compared against two columns, so it is bound twice.
    let mut params = scope_params.clone();
    params.push(Param::Str(id.to_string()));
    params.push(Param::Str(id.to_string()));
    let matches_id = "(visitor_id = ? OR identified_id = ?)";

    let profile_sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope} AND {matches_id}
GROUP BY visitor_id",
        projection = visitor_projection(),
    );

    let profile = fetch_optional::<VisitorRawRow>(ch, "visitor_detail", &profile_sql, &params)
        .await?
        .map(VisitorRow::from)
        .ok_or_else(|| Error::not_found(format!("visitor {id}")))?;

    let sessions_sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope} AND {matches_id}
GROUP BY session_id
ORDER BY started DESC
LIMIT {MAX_VISITOR_SESSIONS}",
        projection = session_projection(),
    );

    let sessions = fetch_all::<SessionRawRow>(ch, "visitor_sessions", &sessions_sql, &params)
        .await?
        .into_iter()
        .map(SessionRow::from)
        .collect();

    let hits_sql = format!(
        "SELECT
{projection}
FROM events
WHERE {scope} AND {matches_id}
ORDER BY timestamp ASC
LIMIT {MAX_RECORD_HITS}",
        projection = hit_projection(),
    );

    let recent = fetch_all::<HitRawRow>(ch, "visitor_hits", &hits_sql, &params)
        .await?
        .into_iter()
        .map(Hit::from)
        .collect();

    Ok(VisitorDetail {
        profile,
        sessions,
        recent,
    })
}
