//! `GET /api/export/raw`: the recorded rows themselves, as a ZIP.
//! `docs/CONTRACT.md` §3.3.
//!
//! The two exports in `routes::export` answer a question: a report, a
//! breakdown, the screen that was open. This one hands over the rows every
//! answer is computed from: each event, error and web vital the site recorded
//! in a window, one line per hit. It is what an owner takes when they leave,
//! when they want to check a number by hand, or when they load the data into a
//! warehouse of their own. Taking your data with you is part of what an
//! open-source analytics product promises, and an export that needed somebody
//! with ClickHouse access would not keep that promise.
//!
//! ## The archive
//!
//! | file | rows | columns |
//! |---|---|---|
//! | `events.csv` or `events.ndjson` | every row of `events` | every column of `events` (§1.1), in schema order |
//! | `errors.csv` or `errors.ndjson` | every row of `errors` | every column of `errors` (§1.6), in schema order |
//! | `web_vitals.csv` or `web_vitals.ndjson` | the `events` rows with `kind = 'web_vitals'` | time, session, visitor, path, browser, OS, device, country, and the five vitals |
//! | `README.txt` | | the site, the window, the row cap, and which files stopped short |
//!
//! `?format=csv` (the default) or `?format=json`, and `?tables=events,errors,vitals`
//! for a subset. The files always come in that order, whatever order was asked.
//!
//! The column lists are written out below rather than read from the schema at
//! run time, so the format is a contract: a column added to `events` cannot
//! silently shift every column after it in somebody's import script. The tests
//! hold the lists to [`micaforge_store::ch::TABLES`], so a new column cannot be
//! forgotten either; it has to be appended here, on purpose.
//!
//! Only the site and the window apply. Filters, a saved segment and the
//! audience are parsed by the envelope extractor and then not used, because a
//! raw export that quietly dropped rows would not be raw. The README and the
//! dashboard both say so in words.
//!
//! ## Streamed, never held
//!
//! Each table is read through a ClickHouse cursor one row at a time, formatted
//! into a chunk of [`CHUNK_BYTES`], deflated into the archive and handed to the
//! connection. Memory per download is one chunk, one row and the compressor's
//! window, whatever the size of the window asked for, and the read advances
//! only as fast as the client takes bytes. A client that goes away drops the
//! body, which drops the cursor, which ends the ClickHouse query.
//!
//! Every value is turned into text inside ClickHouse and read as one
//! `Array(Nullable(String))` per row. That gives three tables of different
//! shapes one cursor type, keeps `NULL` apart from an empty string (the JSON
//! files show the difference; CSV cannot), and leaves the spelling of every
//! cell to [`write_csv_row`] and [`write_json_row`], where it is tested without
//! a database.
//!
//! ## Bounded
//!
//! - **[`ROW_CAP`] rows per file**, earliest first. The query asks for one row
//!   more than the cap, so a file that stopped at the cap is told apart from a
//!   window that happened to hold exactly that many rows.
//! - **The ZIP32 limit.** The archive is ZIP32 (see `zip` for why), and a file
//!   whose next row would put a size or an offset past 4 GiB stops before it.
//!
//! Either way the file is ended cleanly, the archive stays valid, and the README
//! names the file, the reason, and the time of the last row written, which is
//! where a second export should start.
//!
//! ## When the store fails
//!
//! The handler reads the first table's first chunk before it answers, so a
//! ClickHouse that is down, or that refuses the query, is an ordinary error
//! status with the standard envelope. A failure after that, once the `200` and
//! part of the archive are on the wire, can no longer become a status. The body
//! then ends with an error: the server closes the connection without the final
//! chunk, so the client reports an incomplete transfer, and the archive has no
//! central directory, so no unzip tool opens it as if it were whole. A cut file
//! that looks complete is the one outcome this route must never produce.
//!
//! ## No time budget
//!
//! `middleware::budget_for` exempts this route from the 30-second API budget.
//! The first chunk of an ordered read over a long window can take longer than
//! that to arrive, and the download after it lasts as long as the client makes
//! it last.

mod zip;

use std::borrow::Cow;
use std::future::Future;
use std::io;

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, SecondsFormat, Utc};
use clickhouse::query::RowCursor;
use serde::Deserialize;
use tracing::Instrument;

use micaforge_core::{Error, EventKind, QueryEnvelope, Table, TimeRange};
use micaforge_query::bind_all;
use micaforge_query::sql::{placeholder_count, scope_predicate, Param};
use micaforge_store::Ch;

use self::zip::ZipWriter;
use super::agents::same_site;
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::routes::orgs::{require_at_least, VIEWER};
use crate::state::AppState;

/// The one route.
pub fn router() -> Router<AppState> {
    Router::new().route("/api/export/raw", get(export_raw))
}

/// Rows per file, at most.
///
/// A million rows of `events` is a few hundred megabytes of CSV before
/// compression: large enough that most sites export a whole month in one
/// archive, small enough that the archive stays far inside ZIP32 and opens in
/// a spreadsheet that has a row limit of its own (Excel stops at 1,048,576).
/// A site with more asks again from where the README says the file stopped.
pub const ROW_CAP: u64 = 1_000_000;

/// How much formatted text is gathered before it is compressed and sent.
///
/// Large enough that the sync flush at the end of each write costs nothing
/// measurable, small enough that the first bytes reach the client quickly and
/// the memory a download holds stays in the hundreds of kilobytes.
const CHUNK_BYTES: usize = 256 * 1024;

/// The name of the file that explains the others.
const README: &str = "README.txt";

/// The longest site name or domain copied into the README, in characters.
const README_TEXT_MAX: usize = 200;

/// The query parameters this route reads beyond the envelope.
#[derive(Debug, Default, Deserialize)]
struct RawParams {
    /// `csv` (the default) or `json`.
    format: Option<String>,
    /// A comma-separated subset of `events`, `errors`, `vitals`.
    tables: Option<String>,
}

// ---------------------------------------------------------------------------
// What is exported
// ---------------------------------------------------------------------------

/// The file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// Comma-separated, a header row, one row per line.
    Csv,
    /// One JSON object per line, the column names as keys.
    Json,
}

impl Format {
    /// Read `?format=`. Empty means the default.
    fn parse(raw: Option<&str>) -> Result<Format, Error> {
        match raw.unwrap_or_default().trim().to_ascii_lowercase().as_str() {
            "" | "csv" => Ok(Format::Csv),
            "json" | "ndjson" => Ok(Format::Json),
            _ => Err(Error::validation("format", "can be csv or json")),
        }
    }

    /// The word for this format in the archive's filename. Never caller text.
    fn slug(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
        }
    }

    /// The README's description of the files.
    fn describe(self) -> &'static str {
        match self {
            Format::Csv => {
                "CSV, UTF-8: a header row, then one row per line. A cell that holds a comma, \
                 a double quote or a line break is wrapped in double quotes."
            }
            Format::Json => {
                "NDJSON, UTF-8: one JSON object per line, with the column names as keys."
            }
        }
    }
}

/// One file of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawTable {
    /// Every row of `events`.
    Events,
    /// Every row of `errors`.
    Errors,
    /// The `web_vitals` rows of `events`, narrowed to the columns they use.
    WebVitals,
}

impl RawTable {
    /// Every file, in the order the archive holds them.
    const ALL: [RawTable; 3] = [RawTable::Events, RawTable::Errors, RawTable::WebVitals];

    /// The file's name in the archive. A constant, so nothing a caller sent
    /// reaches a path inside the ZIP.
    fn file_name(self, format: Format) -> &'static str {
        match (self, format) {
            (RawTable::Events, Format::Csv) => "events.csv",
            (RawTable::Events, Format::Json) => "events.ndjson",
            (RawTable::Errors, Format::Csv) => "errors.csv",
            (RawTable::Errors, Format::Json) => "errors.ndjson",
            (RawTable::WebVitals, Format::Csv) => "web_vitals.csv",
            (RawTable::WebVitals, Format::Json) => "web_vitals.ndjson",
        }
    }

    /// The ClickHouse table the rows are read from.
    fn source(self) -> Table {
        match self {
            RawTable::Events | RawTable::WebVitals => Table::Events,
            RawTable::Errors => Table::Errors,
        }
    }

    /// The columns of the file, in file order.
    fn columns(self) -> &'static [Column] {
        match self {
            RawTable::Events => EVENT_COLUMNS,
            RawTable::Errors => ERROR_COLUMNS,
            RawTable::WebVitals => VITAL_COLUMNS,
        }
    }
}

/// Read `?tables=`: a comma-separated subset, or every table when empty.
fn parse_tables(raw: Option<&str>) -> Result<Vec<RawTable>, Error> {
    let mut wanted = Vec::new();
    for name in raw.unwrap_or_default().split(',').map(str::trim) {
        let table = match name.to_ascii_lowercase().as_str() {
            "" => continue,
            "events" => RawTable::Events,
            "errors" => RawTable::Errors,
            "vitals" | "web_vitals" => RawTable::WebVitals,
            _ => {
                return Err(Error::validation(
                    "tables",
                    format!(
                        "there is no `{name}` table to export; it can be events, errors or vitals"
                    ),
                ))
            }
        };
        if !wanted.contains(&table) {
            wanted.push(table);
        }
    }
    if wanted.is_empty() {
        return Ok(RawTable::ALL.to_vec());
    }
    Ok(RawTable::ALL
        .into_iter()
        .filter(|table| wanted.contains(table))
        .collect())
}

/// How a column is read out of ClickHouse and spelled in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Text, exactly as stored. Invalid UTF-8 is replaced inside ClickHouse,
    /// so one stray byte in a path cannot fail a whole export.
    Text,
    /// An integer, a float or a decimal, in ClickHouse's own spelling, and a
    /// bare number in JSON.
    Number,
    /// A `DateTime64(3)`, read as epoch milliseconds and written as RFC 3339 in
    /// UTC, to the millisecond.
    Time,
    /// A `Map` or `JSON` column, read as JSON text. Embedded as JSON in NDJSON,
    /// written as text in CSV.
    Json,
    /// The `FixedString(2)` country code, with the padding that stands for
    /// "unknown" trimmed away, as the `country` dimension trims it.
    Country,
}

/// One column of an exported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Column {
    /// The column's name in ClickHouse, which is also its name in the file.
    name: &'static str,
    /// How it is read and written.
    kind: Kind,
}

impl Column {
    /// The `SELECT` expression that turns this column into text.
    ///
    /// Names are constants from the lists below, and quoted anyway, so a
    /// column called `timezone` or `hash` is never read as a function.
    fn select_expr(&self) -> String {
        let name = self.name;
        match self.kind {
            Kind::Text => format!("toValidUTF8(toString(`{name}`))"),
            Kind::Number => format!("toString(`{name}`)"),
            Kind::Time => format!("toString(toUnixTimestamp64Milli(`{name}`))"),
            Kind::Json => format!("toValidUTF8(toJSONString(`{name}`))"),
            Kind::Country => format!("trimBoth(toString(`{name}`))"),
        }
    }
}

const fn text(name: &'static str) -> Column {
    Column {
        name,
        kind: Kind::Text,
    }
}

const fn number(name: &'static str) -> Column {
    Column {
        name,
        kind: Kind::Number,
    }
}

const fn time(name: &'static str) -> Column {
    Column {
        name,
        kind: Kind::Time,
    }
}

const fn json(name: &'static str) -> Column {
    Column {
        name,
        kind: Kind::Json,
    }
}

const fn country(name: &'static str) -> Column {
    Column {
        name,
        kind: Kind::Country,
    }
}

/// `events.*`: every column of `events`, in schema order.
///
/// Nothing is left out. The row holds no address (`PRODUCT.md`: no raw IP at
/// rest), `visitor_id` is the daily-rotating hash, and `identified_id` is only
/// ever what the site itself sent through `identify()`, so the file carries
/// nothing the owner could not already read on the visitor and session
/// screens. `site_id` stays in, so exports of several sites can be loaded into
/// one table. `sample_rate` is the last column of the schema.
const EVENT_COLUMNS: &[Column] = &[
    number("site_id"),
    time("timestamp"),
    text("event_id"),
    text("session_id"),
    text("visitor_id"),
    text("identified_id"),
    text("kind"),
    text("event_name"),
    text("hostname"),
    text("pathname"),
    text("page_title"),
    text("querystring"),
    json("url_params"),
    text("hash"),
    text("referrer"),
    text("referrer_host"),
    text("channel"),
    text("source"),
    text("utm_source"),
    text("utm_medium"),
    text("utm_campaign"),
    text("utm_term"),
    text("utm_content"),
    text("click_id"),
    text("browser"),
    text("browser_version"),
    text("os"),
    text("os_version"),
    text("device_type"),
    number("screen_w"),
    number("screen_h"),
    number("viewport_w"),
    number("viewport_h"),
    text("language"),
    text("timezone"),
    country("country"),
    text("region"),
    text("city"),
    number("lat"),
    number("lon"),
    number("asn"),
    text("asn_org"),
    number("is_datacenter"),
    text("entry_path"),
    text("exit_path"),
    number("duration_ms"),
    number("scroll_pct"),
    json("props"),
    number("revenue"),
    text("currency"),
    json("flags"),
    text("experiment"),
    text("variant"),
    number("lcp"),
    number("cls"),
    number("inp"),
    number("fcp"),
    number("ttfb"),
    text("import_id"),
    text("sdk_version"),
    number("sample_rate"),
];

/// `errors.*`: every column of `errors`, in schema order.
const ERROR_COLUMNS: &[Column] = &[
    number("site_id"),
    time("timestamp"),
    text("session_id"),
    text("visitor_id"),
    text("name"),
    text("message"),
    text("stack"),
    text("filename"),
    number("lineno"),
    number("colno"),
    text("pathname"),
    text("browser"),
    text("os"),
    text("release"),
    number("handled"),
    text("fingerprint"),
];

/// `web_vitals.*`: what a performance reading needs and nothing else.
///
/// The names are the `events` names, so the file joins back to `events.*` on
/// `session_id` and `timestamp` without a mapping.
const VITAL_COLUMNS: &[Column] = &[
    time("timestamp"),
    text("session_id"),
    text("visitor_id"),
    text("pathname"),
    text("browser"),
    text("os"),
    text("device_type"),
    country("country"),
    number("lcp"),
    number("cls"),
    number("inp"),
    number("fcp"),
    number("ttfb"),
];

// ---------------------------------------------------------------------------
// The route
// ---------------------------------------------------------------------------

/// `GET /api/export/raw` — the site's raw rows for a window, as a ZIP.
///
/// `viewer` is enough: every value in the archive is one the dashboard
/// already shows a viewer, row by row, on the session and visitor screens. An
/// API key needs `export:read` (`extract::key_route_policy`), a scope of its
/// own because a key that can carry off every row is a different grant from
/// one that reads totals.
async fn export_raw(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<RawParams>,
) -> Result<Response, ApiError> {
    same_site(&access, &env)?;
    require_at_least(access.role.as_str(), VIEWER)?;
    let format = Format::parse(params.format.as_deref())?;
    let tables = parse_tables(params.tables.as_deref())?;

    let filename = archive_name(format, &env.range);
    let plan = Plan {
        // The site from the authorised access, the window from the envelope,
        // and no filters: see the module notes.
        scope: QueryEnvelope {
            site: access.site_id,
            filters: None,
            ..env
        },
        format,
        tables,
        cap: ROW_CAP,
        site_name: access.site.name.clone(),
        site_domain: access.site.domain.clone(),
        taken: Utc::now(),
    };

    let ch = st.ch.clone();
    let mut archive = Archive::new(plan, move |plan: &Plan, table| {
        open_cursor(&ch, plan, table)
    });

    // Read before answering, so a store failure is a status, not a cut file.
    let first = archive.next_chunk().await?;

    Ok(download(body(archive, first), &filename))
}

/// The archive's filename: a format slug and two dates, nothing else.
///
/// Built only from a constant and the resolved window, so nothing a caller
/// sent reaches `content-disposition`, where a quote or a newline would be a
/// header-splitting bug rather than an odd download name.
fn archive_name(format: Format, range: &TimeRange) -> String {
    format!(
        "micaforge-raw-{}-{}-{}.zip",
        format.slug(),
        range.start.format("%Y%m%d"),
        range.end.format("%Y%m%d"),
    )
}

/// Wrap the streamed archive as a download.
fn download(body: Body, filename: &str) -> Response {
    let mut response = Response::new(body);
    let headers = response.headers_mut();

    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    // A reading of one moment: a cache between here and the reader would hand
    // them yesterday's rows for today's window.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );

    response
}

/// The archive as a response body: the chunk already read, then the rest, one
/// chunk per poll.
///
/// The stream runs after the handler has returned, outside the request's span,
/// so the span is carried in by hand: a failure logged halfway through a
/// download still carries the request id the access log printed.
fn body<S, O>(archive: Archive<S, O>, first: Option<Bytes>) -> Body
where
    S: RowSource + 'static,
    O: FnMut(&Plan, RawTable) -> Result<S, Error> + Send + 'static,
{
    let span = tracing::Span::current();
    let stream = futures::stream::unfold(Some((archive, first)), move |state| {
        next_piece(state).instrument(span.clone())
    });
    Body::from_stream(stream)
}

/// What the body stream carries between polls: the archive, and a chunk read
/// before the stream started, if one is still waiting. `None` once the stream
/// has ended, by success or failure.
type StreamState<S, O> = Option<(Archive<S, O>, Option<Bytes>)>;

/// One poll of the body stream.
///
/// An error ends the stream after one `Err` item, which is what makes the
/// server abort the response instead of finishing it. See the module notes.
async fn next_piece<S, O>(
    state: StreamState<S, O>,
) -> Option<(io::Result<Bytes>, StreamState<S, O>)>
where
    S: RowSource,
    O: FnMut(&Plan, RawTable) -> Result<S, Error> + Send,
{
    let (mut archive, waiting) = state?;
    if let Some(bytes) = waiting {
        return Some((Ok(bytes), Some((archive, None))));
    }
    match archive.next_chunk().await {
        Ok(Some(bytes)) => Some((Ok(bytes), Some((archive, None)))),
        Ok(None) => None,
        Err(e) => {
            tracing::error!(
                site = archive.plan.scope.site,
                file = archive.current_file().unwrap_or(README),
                error = %e,
                "the raw export failed after it started; the download was cut off"
            );
            Some((Err(io::Error::other("the raw export stopped early")), None))
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// One row as ClickHouse sends it: every column already turned into text.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct Cells {
    cells: Vec<Option<String>>,
}

/// Where a file's rows come from. A ClickHouse cursor, outside the tests.
trait RowSource: Send {
    /// The next row, or `None` when the table is done.
    fn next_row(
        &mut self,
    ) -> impl Future<Output = Result<Option<Vec<Option<String>>>, Error>> + Send;
}

impl RowSource for RowCursor<Cells> {
    async fn next_row(&mut self) -> Result<Option<Vec<Option<String>>>, Error> {
        match self.next().await {
            Ok(row) => Ok(row.map(|row| row.cells)),
            Err(e) => Err(Error::Database(format!("raw export read: {e}"))),
        }
    }
}

/// Start the read of one file.
///
/// No network call happens here; the driver sends the query on the first
/// `next`, which is why the handler reads a first chunk before it answers.
fn open_cursor(ch: &Ch, plan: &Plan, table: RawTable) -> Result<RowCursor<Cells>, Error> {
    let (sql, params) = select_statement(table, &plan.scope, plan.cap)?;
    tracing::debug!(
        report = "raw_export",
        sql,
        params = params.len(),
        "clickhouse read"
    );
    bind_all(ch.client().query(&sql), &params)
        .fetch::<Cells>()
        .map_err(|e| Error::Database(format!("raw export query: {e}")))
}

/// The statement for one file: its columns as one text array per row, the
/// site and window from `scope`, earliest first, one row past the cap.
///
/// The predicate is [`scope_predicate`]'s, the one every hand-written report
/// uses, over an envelope whose filters have been cleared. Every value in it,
/// the site, the two bounds, the kind and the limit, is a bind.
fn select_statement(
    table: RawTable,
    scope: &QueryEnvelope,
    cap: u64,
) -> Result<(String, Vec<Param>), Error> {
    let (predicate, mut params) = scope_predicate(scope, table.source())?;
    let cells = table
        .columns()
        .iter()
        .map(Column::select_expr)
        .collect::<Vec<_>>()
        .join(", ");

    // The cast is not decoration. A table with no nullable column would
    // otherwise produce `Array(String)`, whose wire form has no null flags,
    // and every row would be misread.
    let mut sql = format!(
        "SELECT CAST([{cells}], 'Array(Nullable(String))') AS cells FROM {} WHERE {predicate}",
        table.source().as_str()
    );
    if table == RawTable::WebVitals {
        sql.push_str(" AND kind = ?");
        params.push(Param::Str(EventKind::WebVitals.as_str().to_string()));
    }
    sql.push_str(" ORDER BY timestamp LIMIT ?");
    params.push(Param::U64(cap.saturating_add(1)));

    // The same guard `micaforge_query::exec::fetch_all` runs: positional binds
    // that are one out put every later value in the wrong slot.
    let slots = placeholder_count(&sql);
    if slots != params.len() {
        return Err(Error::Internal(format!(
            "raw export: statement has {slots} bind slots but {} parameters",
            params.len()
        )));
    }
    Ok((sql, params))
}

// ---------------------------------------------------------------------------
// The archive
// ---------------------------------------------------------------------------

/// Everything the archive needs to know, settled before the first byte.
struct Plan {
    /// The site (from the authorised access), the window (from the envelope),
    /// and no filters.
    scope: QueryEnvelope,
    /// CSV or NDJSON.
    format: Format,
    /// The files to write, in archive order.
    tables: Vec<RawTable>,
    /// Rows per file, at most. [`ROW_CAP`] outside the tests.
    cap: u64,
    /// The site's name, for the README.
    site_name: String,
    /// The site's domain, for the README.
    site_domain: String,
    /// When the export was asked for: the README's date and every entry's
    /// modification time.
    taken: DateTime<Utc>,
}

/// Why a file stopped before the window ran out of rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cut {
    /// It reached [`Plan::cap`] and the window held more.
    RowCap,
    /// Its next row would have carried the archive past ZIP32.
    SizeLimit,
}

/// How one file ended, for the README.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReport {
    /// The file's name in the archive.
    file: &'static str,
    /// Rows written.
    rows: u64,
    /// Why it stopped early, if it did.
    cut: Option<Cut>,
    /// The time of the last row written, in epoch milliseconds.
    last_ms: Option<i64>,
}

/// What became of one row offered to a file.
enum Offer {
    /// It was written.
    Taken,
    /// It was not, and the file ends here.
    Cut(Cut),
}

/// Where a fill of the chunk buffer stopped.
enum Fill {
    /// The chunk is full; the file goes on.
    Full,
    /// The file is done, complete or cut.
    Ended(Option<Cut>),
}

/// One file being written.
struct FileWriter {
    /// Which file.
    table: RawTable,
    /// CSV or NDJSON.
    format: Format,
    /// Rows per file, at most.
    cap: u64,
    /// Rows written so far.
    rows: u64,
    /// Where the time column sits in a row.
    time_index: Option<usize>,
    /// The time of the last row written.
    last_ms: Option<i64>,
    /// Formatted rows waiting to be compressed.
    chunk: Vec<u8>,
    /// The row being formatted, before it is known to fit.
    line: Vec<u8>,
}

impl FileWriter {
    /// A file with nothing written, its CSV header already in the chunk.
    fn new(table: RawTable, plan: &Plan) -> FileWriter {
        let mut chunk = Vec::with_capacity(CHUNK_BYTES + 16 * 1024);
        if plan.format == Format::Csv {
            write_csv_header(table.columns(), &mut chunk);
        }
        FileWriter {
            table,
            format: plan.format,
            cap: plan.cap,
            rows: 0,
            time_index: table
                .columns()
                .iter()
                .position(|column| column.kind == Kind::Time),
            last_ms: None,
            chunk,
            line: Vec::new(),
        }
    }

    /// Read rows into the chunk until it is full or the file ends.
    async fn fill<S: RowSource>(&mut self, source: &mut S, zip: &ZipWriter) -> Result<Fill, Error> {
        while self.chunk.len() < CHUNK_BYTES {
            let Some(cells) = source.next_row().await? else {
                return Ok(Fill::Ended(None));
            };
            if let Offer::Cut(cut) = self.offer(zip, &cells)? {
                return Ok(Fill::Ended(Some(cut)));
            }
        }
        Ok(Fill::Full)
    }

    /// Format one row and keep it, or say why the file ends before it.
    ///
    /// The cap is checked when a row *beyond* it arrives, so a window with
    /// exactly `cap` rows is reported complete, which it is.
    fn offer(&mut self, zip: &ZipWriter, cells: &[Option<String>]) -> Result<Offer, Error> {
        if self.rows >= self.cap {
            return Ok(Offer::Cut(Cut::RowCap));
        }

        self.line.clear();
        let columns = self.table.columns();
        match self.format {
            Format::Csv => write_csv_row(columns, cells, &mut self.line)?,
            Format::Json => write_json_row(columns, cells, &mut self.line)?,
        }
        if !zip.room_for(self.chunk.len() + self.line.len()) {
            return Ok(Offer::Cut(Cut::SizeLimit));
        }

        self.chunk.extend_from_slice(&self.line);
        self.rows += 1;
        let time = self
            .time_index
            .and_then(|index| cells.get(index))
            .and_then(|cell| cell.as_deref())
            .and_then(|ms| ms.parse::<i64>().ok());
        if time.is_some() {
            self.last_ms = time;
        }
        Ok(Offer::Taken)
    }

    /// How this file ended.
    fn report(&self, cut: Option<Cut>) -> FileReport {
        FileReport {
            file: self.table.file_name(self.format),
            rows: self.rows,
            cut,
            last_ms: self.last_ms,
        }
    }
}

/// The archive being produced: a ZIP writer, the file being read into it,
/// and the reports of the files already finished.
///
/// Generic over where rows come from, so the whole streaming path, chunking,
/// caps, the README and a failure halfway, is tested without a database.
struct Archive<S, O> {
    /// Opens a file's row source.
    open: O,
    /// What to export.
    plan: Plan,
    /// The ZIP being produced.
    zip: ZipWriter,
    /// Index into [`Plan::tables`] of the next file to start.
    next_table: usize,
    /// The file being written, and its rows.
    current: Option<(FileWriter, S)>,
    /// Files already finished.
    reports: Vec<FileReport>,
    /// Set once the README and the central directory are written.
    finished: bool,
}

impl<S, O> Archive<S, O>
where
    S: RowSource,
    O: FnMut(&Plan, RawTable) -> Result<S, Error> + Send,
{
    fn new(plan: Plan, open: O) -> Archive<S, O> {
        Archive {
            open,
            zip: ZipWriter::new(plan.taken),
            plan,
            next_table: 0,
            current: None,
            reports: Vec::new(),
            finished: false,
        }
    }

    /// The name of the file being written, for a log line.
    fn current_file(&self) -> Option<&'static str> {
        self.current
            .as_ref()
            .map(|(file, _)| file.table.file_name(file.format))
    }

    /// The next piece of the archive, or `None` when it has all been sent.
    ///
    /// A piece is whatever one chunk of rows compressed to, plus any headers,
    /// descriptors or directory that came due with it. A file's local header
    /// is never sent on its own: it waits for the file's first chunk, so the
    /// handler's first call has always been through a real read of the store.
    async fn next_chunk(&mut self) -> Result<Option<Bytes>, Error> {
        loop {
            if self.finished {
                let rest = self.zip.take();
                return Ok((!rest.is_empty()).then(|| Bytes::from(rest)));
            }
            if self.current.is_none() {
                self.begin_next()?;
                continue;
            }

            if let Some((file, source)) = self.current.as_mut() {
                let fill = file.fill(source, &self.zip).await?;
                self.zip.write(&file.chunk).map_err(archive_error)?;
                file.chunk.clear();
                if let Fill::Ended(cut) = fill {
                    let report = file.report(cut);
                    self.zip.finish_entry().map_err(archive_error)?;
                    self.reports.push(report);
                    self.current = None;
                }
            }

            let bytes = self.zip.take();
            if !bytes.is_empty() {
                return Ok(Some(Bytes::from(bytes)));
            }
        }
    }

    /// Start the next file, or, when there is none, close the archive.
    fn begin_next(&mut self) -> Result<(), Error> {
        let Some(table) = self.plan.tables.get(self.next_table).copied() else {
            return self.close();
        };
        self.next_table += 1;
        self.zip
            .start(table.file_name(self.plan.format))
            .map_err(archive_error)?;
        let source = (self.open)(&self.plan, table)?;
        self.current = Some((FileWriter::new(table, &self.plan), source));
        Ok(())
    }

    /// Write the README, then the central directory.
    fn close(&mut self) -> Result<(), Error> {
        let text = readme(&self.plan, &self.reports);
        self.zip.start(README).map_err(archive_error)?;
        self.zip.write(text.as_bytes()).map_err(archive_error)?;
        self.zip.finish().map_err(archive_error)?;
        self.finished = true;
        Ok(())
    }
}

/// A failure of the ZIP writer. Its message names a limit or a misuse, never
/// data, so it is safe to log.
fn archive_error(e: io::Error) -> Error {
    Error::Internal(format!("raw export archive: {e}"))
}

// ---------------------------------------------------------------------------
// Spelling cells
// ---------------------------------------------------------------------------

/// The CSV header: the column names, which are constants that never need
/// quoting.
fn write_csv_header(columns: &[Column], out: &mut Vec<u8>) {
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.extend_from_slice(column.name.as_bytes());
    }
    out.push(b'\n');
}

/// One CSV row.
///
/// The rules are the ones `micaforge_query::to_csv` follows for the report
/// export, so the two CSV files this product writes read the same way: quotes
/// only where a cell holds a comma, a double quote or a line break, doubled
/// quotes inside, `\n` between rows, and an empty cell for a value that was
/// never recorded. Text cells are neutralised against formula injection (see
/// [`neutralise`]); numbers and times never are, because they cannot carry one.
fn write_csv_row(
    columns: &[Column],
    cells: &[Option<String>],
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    same_shape(columns, cells)?;
    for (index, (column, cell)) in columns.iter().zip(cells).enumerate() {
        if index > 0 {
            out.push(b',');
        }
        let Some(value) = cell.as_deref() else {
            continue;
        };
        let spelled: Cow<'_, str> = match column.kind {
            Kind::Time => Cow::Owned(time_cell(value)),
            Kind::Number => Cow::Borrowed(value),
            Kind::Text | Kind::Json | Kind::Country => neutralise(value),
        };
        csv_field(&spelled, out);
    }
    out.push(b'\n');
    Ok(())
}

/// One CSV field, quoted when it has to be.
fn csv_field(value: &str, out: &mut Vec<u8>) {
    if value.contains([',', '"', '\r', '\n']) {
        out.push(b'"');
        out.extend_from_slice(value.replace('"', "\"\"").as_bytes());
        out.push(b'"');
    } else {
        out.extend_from_slice(value.as_bytes());
    }
}

/// A text cell, made safe to open in a spreadsheet.
///
/// Ingest is public, so an event name, a query string or a referrer can be any
/// text a stranger chose, `=HYPERLINK(...)` included, and a spreadsheet opening
/// the file would run it. A cell that starts with `=`, `+`, `-`, `@`, a tab or a
/// carriage return gets a leading `'`, unless the whole cell is a number. This
/// is the rule `micaforge_query::to_csv` applies; the JSON files carry every
/// value unchanged, and the README says so.
fn neutralise(cell: &str) -> Cow<'_, str> {
    let risky = cell
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '=' | '+' | '-' | '@' | '\t' | '\r'));
    if risky && cell.parse::<f64>().is_err() {
        Cow::Owned(format!("'{cell}"))
    } else {
        Cow::Borrowed(cell)
    }
}

/// One NDJSON row: an object with the columns in file order, and a newline.
///
/// Text is a JSON string, escaped by `serde_json`, with every other character,
/// emoji included, written as UTF-8. A number is written bare when it is a
/// JSON number, and as `null` when it is not (ClickHouse spells a float that is
/// not finite `nan` or `inf`, which JSON has no way to say). A `Map` or `JSON`
/// column is embedded as the JSON ClickHouse produced, after checking that it
/// parses and fits on one line; anything else would break the one-object-per-
/// line framing, so it is written as a string instead.
fn write_json_row(
    columns: &[Column],
    cells: &[Option<String>],
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    same_shape(columns, cells)?;
    out.push(b'{');
    for (index, (column, cell)) in columns.iter().zip(cells).enumerate() {
        if index > 0 {
            out.push(b',');
        }
        json_string(column.name, out)?;
        out.push(b':');
        match (column.kind, cell.as_deref()) {
            (_, None) => out.extend_from_slice(b"null"),
            (Kind::Text | Kind::Country, Some(value)) => json_string(value, out)?,
            (Kind::Time, Some(value)) => json_string(&time_cell(value), out)?,
            (Kind::Number, Some(value)) if is_json_number(value) => {
                out.extend_from_slice(value.as_bytes())
            }
            (Kind::Number, Some(_)) => out.extend_from_slice(b"null"),
            (Kind::Json, Some(value)) if is_one_line_json(value) => {
                out.extend_from_slice(value.as_bytes())
            }
            (Kind::Json, Some(value)) => json_string(value, out)?,
        }
    }
    out.extend_from_slice(b"}\n");
    Ok(())
}

/// Write `text` as a JSON string.
fn json_string(text: &str, out: &mut Vec<u8>) -> Result<(), Error> {
    serde_json::to_writer(&mut *out, text)
        .map_err(|e| Error::Internal(format!("raw export json: {e}")))
}

/// Is `text` exactly one JSON number, with nothing around it?
fn is_json_number(text: &str) -> bool {
    !text.is_empty()
        && !text.bytes().any(|b| b.is_ascii_whitespace())
        && serde_json::from_str::<serde_json::Number>(text).is_ok()
}

/// Is `text` valid JSON that holds no line break?
fn is_one_line_json(text: &str) -> bool {
    !text.contains(['\n', '\r']) && serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
}

/// A time cell: epoch milliseconds from ClickHouse, as RFC 3339 in UTC.
///
/// A cell that is not a number cannot come from `toUnixTimestamp64Milli`; it
/// is written as it came rather than dropped, so a surprise stays visible.
fn time_cell(ms: &str) -> String {
    ms.parse::<i64>()
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .map_or_else(|| ms.to_string(), rfc3339)
}

/// An instant as RFC 3339 in UTC, to the millisecond, with a `Z`.
fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Refuse a row whose width does not match its file. Cannot happen with the
/// statement this module builds; checked so it fails loudly if it ever does.
fn same_shape(columns: &[Column], cells: &[Option<String>]) -> Result<(), Error> {
    if columns.len() == cells.len() {
        return Ok(());
    }
    Err(Error::Internal(format!(
        "raw export: a row of {} cells for {} columns",
        cells.len(),
        columns.len()
    )))
}

// ---------------------------------------------------------------------------
// The README
// ---------------------------------------------------------------------------

/// The README: what this archive is, and what to do about a file that stopped
/// short.
fn readme(plan: &Plan, files: &[FileReport]) -> String {
    let range = &plan.scope.range;
    let mut out = String::with_capacity(2048);

    out.push_str("Micaforge raw data export\n\n");
    let site = format!(
        "{} ({}), site id {}",
        plain(&plan.site_name),
        plain(&plan.site_domain),
        plan.scope.site
    );
    let window = format!(
        "{} to {}, UTC. The start is included, the end is not.",
        rfc3339(range.start),
        rfc3339(range.end)
    );
    let taken = rfc3339(plan.taken);
    let cap = format!("{} rows per file, earliest rows first", grouped(plan.cap));
    for (label, value) in [
        ("Site", site.as_str()),
        ("Window", window.as_str()),
        ("Format", plan.format.describe()),
        ("Taken", taken.as_str()),
        ("Row limit", cap.as_str()),
    ] {
        out.push_str(&format!("{:<11}{value}\n", format!("{label}:")));
    }

    out.push_str("\nFiles\n\n");
    for file in files {
        out.push_str(&file_line(file, range));
        out.push('\n');
    }

    out.push_str("\nAbout the data\n\n");
    for note in [
        "Filters, saved segments and the audience setting do not apply to this export. \
         Every row recorded for the site in the window is here, up to the row limit.",
        "events: one row per hit that a visitor's browser or your server sent: pageviews, \
         custom events and the rest (the kind column says which). AI crawlers and other \
         bots are recorded separately and are not in this export.",
        "errors: one row per JavaScript error, with its stack.",
        "web_vitals: the web_vitals rows of events, with only the columns a performance \
         reading needs. They join back to events on session_id and timestamp.",
        "Times are RFC 3339 in UTC, to the millisecond.",
        "visitor_id is a salted hash that changes every day. It counts visitors; it does \
         not identify a person.",
        "A value that was never recorded is an empty cell in CSV and null in JSON.",
        "CSV only: a text cell that starts with =, +, -, @, a tab or a carriage return has \
         a ' added in front, so that a spreadsheet shows it as text and does not run it as \
         a formula. The JSON export carries every value unchanged.",
    ] {
        out.push_str("- ");
        out.push_str(note);
        out.push('\n');
    }

    out
}

/// One line of the README's file list.
fn file_line(file: &FileReport, range: &TimeRange) -> String {
    let rows = match file.rows {
        1 => "1 row".to_string(),
        n => format!("{} rows", grouped(n)),
    };
    let Some(cut) = file.cut else {
        return format!("{}: {rows}. Complete.", file.file);
    };

    let why = match cut {
        Cut::RowCap => "Stopped at the row limit; the window holds more rows.",
        Cut::SizeLimit => {
            "Stopped at 4 GiB, the most a ZIP file without ZIP64 extensions can hold."
        }
    };
    let next = match file.last_ms.and_then(DateTime::from_timestamp_millis) {
        Some(last) => {
            let last = rfc3339(last);
            format!(
                "The last row written is from {last}. To get the rest, export again with \
                 start={last} and end={}. Rows from that exact millisecond are in both exports.",
                rfc3339(range.end)
            )
        }
        None => "No row of it fitted. Export it on its own with the tables parameter.".to_string(),
    };
    format!("{}: {rows}. {why} {next}", file.file)
}

/// Site text made safe for one README line: control characters, line breaks
/// included, become spaces, and the length is capped.
fn plain(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(README_TEXT_MAX)
        .collect();
    cleaned.trim().to_string()
}

/// A whole number with thousands separators.
fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::zip::tests::read_archive;
    use super::*;
    use chrono::TimeZone;
    use micaforge_core::{Audience, Compare, Granularity};
    use micaforge_store::ch::TABLES;

    type Row = Result<Vec<Option<String>>, Error>;

    impl RowSource for std::vec::IntoIter<Row> {
        async fn next_row(&mut self) -> Result<Option<Vec<Option<String>>>, Error> {
            self.next().transpose()
        }
    }

    fn window() -> TimeRange {
        TimeRange::new(
            Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            chrono_tz::UTC,
        )
    }

    fn scope() -> QueryEnvelope {
        QueryEnvelope {
            site: 7,
            range: window(),
            compare: Compare::None,
            granularity: Granularity::Day,
            filters: None,
            audience: Audience::Human,
            limit: 100,
            page: 1,
        }
    }

    fn plan(format: Format, tables: &[RawTable], cap: u64) -> Plan {
        Plan {
            scope: scope(),
            format,
            tables: tables.to_vec(),
            cap,
            site_name: "Docs".into(),
            site_domain: "docs.example.com".into(),
            taken: Utc.with_ymd_and_hms(2026, 10, 8, 9, 30, 0).unwrap(),
        }
    }

    /// A plausible row for `table`, at `ms` epoch milliseconds.
    fn row(table: RawTable, ms: i64) -> Vec<Option<String>> {
        table
            .columns()
            .iter()
            .map(|column| match column.kind {
                Kind::Time => Some(ms.to_string()),
                Kind::Number if column.name == "lcp" || column.name == "revenue" => None,
                Kind::Number => Some("1".to_string()),
                Kind::Json => Some("{}".to_string()),
                Kind::Country => Some("PT".to_string()),
                Kind::Text => Some(format!("{}-{ms}", column.name)),
            })
            .collect()
    }

    fn rows(table: RawTable, count: i64) -> Vec<Row> {
        let start = window().start.timestamp_millis();
        (0..count)
            .map(|i| Ok(row(table, start + i * 1000)))
            .collect()
    }

    /// Drive an archive to the end, as the body stream does.
    async fn produce(
        plan: Plan,
        source: impl Fn(RawTable) -> Vec<Row> + Send + 'static,
    ) -> Result<(Vec<u8>, usize), (Error, Vec<u8>)> {
        let mut archive = Archive::new(plan, move |_: &Plan, table| Ok(source(table).into_iter()));
        let mut wire = Vec::new();
        let mut pieces = 0;
        loop {
            match archive.next_chunk().await {
                Ok(Some(bytes)) => {
                    wire.extend_from_slice(&bytes);
                    pieces += 1;
                }
                Ok(None) => return Ok((wire, pieces)),
                Err(e) => return Err((e, wire)),
            }
        }
    }

    fn file<'a>(files: &'a [(String, Vec<u8>)], name: &str) -> &'a str {
        let (_, data) = files
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("{name} is missing"));
        std::str::from_utf8(data).unwrap()
    }

    fn schema_columns(table: &str) -> Vec<&'static str> {
        TABLES
            .iter()
            .find(|t| t.name == table)
            .unwrap()
            .columns
            .iter()
            .map(|(name, _)| *name)
            .collect()
    }

    fn names(columns: &[Column]) -> Vec<&'static str> {
        columns.iter().map(|c| c.name).collect()
    }

    // -- the column lists ----------------------------------------------------

    #[test]
    fn events_export_every_schema_column_in_schema_order() {
        let schema = schema_columns("events");
        let exported = names(EVENT_COLUMNS);
        // A column appended to the schema must be appended here too, or this
        // fails; `sample_rate` is the schema's newest column.
        assert!(
            exported.len() >= schema.len() && exported[..schema.len()] == schema[..],
            "events.* must list the schema's columns, in order, before anything else"
        );
        assert_eq!(exported.last(), Some(&"sample_rate"));
    }

    #[test]
    fn errors_export_every_schema_column_in_schema_order() {
        assert_eq!(names(ERROR_COLUMNS), schema_columns("errors"));
    }

    #[test]
    fn web_vitals_use_event_columns_and_nothing_else() {
        let events = schema_columns("events");
        for column in VITAL_COLUMNS {
            assert!(events.contains(&column.name), "{}", column.name);
        }
        let vitals: Vec<_> = names(VITAL_COLUMNS);
        for metric in ["lcp", "cls", "inp", "fcp", "ttfb"] {
            assert!(vitals.contains(&metric), "{metric}");
        }
        assert_eq!(vitals.first(), Some(&"timestamp"));
    }

    #[test]
    fn every_column_name_is_a_plain_identifier_with_one_time_column() {
        for table in RawTable::ALL {
            let columns = table.columns();
            for column in columns {
                assert!(
                    column
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                    "{}",
                    column.name
                );
            }
            let times = columns.iter().filter(|c| c.kind == Kind::Time).count();
            assert_eq!(times, 1, "{table:?}");
        }
    }

    // -- the statement ---------------------------------------------------------

    #[test]
    fn every_value_in_the_statement_is_a_bind() {
        for table in RawTable::ALL {
            let (sql, params) = select_statement(table, &scope(), ROW_CAP).unwrap();
            assert_eq!(placeholder_count(&sql), params.len(), "{table:?}");
            assert!(sql.contains("CAST(["));
            assert!(sql.contains("'Array(Nullable(String))') AS cells"));
            assert!(sql.ends_with(" ORDER BY timestamp LIMIT ?"), "{sql}");
            assert_eq!(params.first(), Some(&Param::U16(7)), "the site comes first");
            assert_eq!(
                params.last(),
                Some(&Param::U64(ROW_CAP + 1)),
                "one past the cap"
            );
            assert!(params.contains(&Param::Str("2026-09-01 00:00:00.000".into())));
            assert!(params.contains(&Param::Str("2026-10-01 00:00:00.000".into())));
        }

        let (vitals, params) = select_statement(RawTable::WebVitals, &scope(), 10).unwrap();
        assert!(vitals.contains("FROM events WHERE"));
        assert!(vitals.contains(" AND kind = ?"));
        assert!(params.contains(&Param::Str("web_vitals".into())));
        let (errors, _) = select_statement(RawTable::Errors, &scope(), 10).unwrap();
        assert!(errors.contains("FROM errors WHERE"));
        assert!(!errors.contains("kind = ?"));
    }

    #[test]
    fn filters_never_reach_the_statement() {
        // The handler clears them; this pins that a cleared envelope adds no
        // predicate beyond the site and the window.
        let (sql, params) = select_statement(RawTable::Events, &scope(), 10).unwrap();
        assert_eq!(params.len(), 4, "site, start, end, limit");
        assert!(!sql.contains(" OR "));
    }

    #[test]
    fn each_kind_reads_its_column_as_text() {
        assert_eq!(
            text("timezone").select_expr(),
            "toValidUTF8(toString(`timezone`))"
        );
        assert_eq!(number("lcp").select_expr(), "toString(`lcp`)");
        assert_eq!(
            time("timestamp").select_expr(),
            "toString(toUnixTimestamp64Milli(`timestamp`))"
        );
        assert_eq!(
            json("props").select_expr(),
            "toValidUTF8(toJSONString(`props`))"
        );
        assert_eq!(
            country("country").select_expr(),
            "trimBoth(toString(`country`))"
        );
    }

    // -- CSV -----------------------------------------------------------------

    fn csv(columns: &[Column], cells: &[Option<&str>]) -> String {
        let owned: Vec<Option<String>> = cells.iter().map(|c| c.map(str::to_string)).collect();
        let mut out = Vec::new();
        write_csv_row(columns, &owned, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn csv_quotes_only_what_needs_quoting() {
        let columns = [text("a"), text("b"), text("c"), text("d"), text("e")];
        assert_eq!(
            csv(
                &columns,
                &[
                    Some("plain"),
                    Some("a,b"),
                    Some("say \"hi\""),
                    Some("two\nlines"),
                    Some("cr\rhere"),
                ],
            ),
            "plain,\"a,b\",\"say \"\"hi\"\"\",\"two\nlines\",\"cr\rhere\"\n"
        );
    }

    #[test]
    fn csv_keeps_unicode_as_it_is() {
        let columns = [text("title"), country("country")];
        assert_eq!(
            csv(
                &columns,
                &[Some("Caf\u{e9} \u{1f600} \u{6587}\u{5b57}"), Some("PT")]
            ),
            "Caf\u{e9} \u{1f600} \u{6587}\u{5b57},PT\n"
        );
    }

    #[test]
    fn csv_writes_null_as_an_empty_cell_and_times_as_rfc3339() {
        let columns = [
            time("timestamp"),
            number("lcp"),
            text("variant"),
            number("asn"),
        ];
        assert_eq!(
            csv(
                &columns,
                &[Some("1788265531123"), None, Some(""), Some("13335")]
            ),
            "2026-09-01T12:25:31.123Z,,,13335\n"
        );
    }

    #[test]
    fn csv_neutralises_formulas_in_text_but_not_numbers() {
        let columns = [
            text("event_name"),
            text("referrer"),
            text("q"),
            number("lat"),
            text("delta"),
        ];
        assert_eq!(
            csv(
                &columns,
                &[
                    Some("=HYPERLINK(\"http://x\")"),
                    Some("@SUM(A1)"),
                    Some("+cmd"),
                    Some("-33.86"),
                    Some("-3.1"),
                ],
            ),
            "\"'=HYPERLINK(\"\"http://x\"\")\",'@SUM(A1),'+cmd,-33.86,-3.1\n"
        );
    }

    #[test]
    fn the_csv_header_is_the_column_names() {
        let mut out = Vec::new();
        write_csv_header(VITAL_COLUMNS, &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "timestamp,session_id,visitor_id,pathname,browser,os,device_type,country,\
             lcp,cls,inp,fcp,ttfb\n"
        );
    }

    #[test]
    fn a_row_of_the_wrong_width_is_refused() {
        let mut out = Vec::new();
        assert!(write_csv_row(&[text("a")], &[], &mut out).is_err());
        assert!(write_json_row(&[text("a")], &[None, None], &mut out).is_err());
    }

    // -- NDJSON --------------------------------------------------------------

    fn ndjson(columns: &[Column], cells: &[Option<&str>]) -> String {
        let owned: Vec<Option<String>> = cells.iter().map(|c| c.map(str::to_string)).collect();
        let mut out = Vec::new();
        write_json_row(columns, &owned, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn ndjson_escapes_text_and_keeps_one_object_per_line() {
        let columns = [text("title"), text("path"), text("note")];
        let line = ndjson(
            &columns,
            &[
                Some("say \"hi\", a\\b"),
                Some("two\nlines\ttab\u{1}"),
                Some("Caf\u{e9} \u{1f600}"),
            ],
        );
        assert!(line.ends_with("}\n"));
        assert_eq!(line.matches('\n').count(), 1, "one line per row: {line}");
        assert!(line.contains("\"say \\\"hi\\\", a\\\\b\""));
        assert!(line.contains("two\\nlines\\ttab\\u0001"));
        assert!(line.contains("Caf\u{e9} \u{1f600}"), "unicode stays UTF-8");

        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["path"], "two\nlines\ttab\u{1}");
    }

    #[test]
    fn ndjson_types_numbers_times_nulls_and_json() {
        let columns = [
            time("timestamp"),
            number("asn"),
            number("cls"),
            number("revenue"),
            number("lcp"),
            number("inp"),
            json("props"),
            json("flags"),
            json("url_params"),
            text("identified_id"),
        ];
        let line = ndjson(
            &columns,
            &[
                Some("1788265531123"),
                Some("13335"),
                Some("0.05"),
                Some("12.5"),
                Some("nan"),
                None,
                Some("{\"plan\":\"pro\",\"n\":3}"),
                Some("not json"),
                Some("{\"a\":\n1}"),
                Some(""),
            ],
        );
        assert_eq!(
            line,
            "{\"timestamp\":\"2026-09-01T12:25:31.123Z\",\"asn\":13335,\"cls\":0.05,\
             \"revenue\":12.5,\"lcp\":null,\"inp\":null,\"props\":{\"plan\":\"pro\",\"n\":3},\
             \"flags\":\"not json\",\"url_params\":\"{\\\"a\\\":\\n1}\",\"identified_id\":\"\"}\n"
        );
        // The exact string above also pins the key order, which is the file's
        // contract; this pins that the line is JSON at all.
        assert!(serde_json::from_str::<serde_json::Value>(&line).is_ok());
    }

    #[test]
    fn only_real_json_numbers_are_written_bare() {
        for good in ["0", "-1", "12.5", "1e-05", "-0.25", "65535"] {
            assert!(is_json_number(good), "{good}");
        }
        for bad in ["", "nan", "inf", "-inf", "01", "1.", " 1", "1\n", "0x10"] {
            assert!(!is_json_number(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_time_that_is_not_a_number_is_kept_visible() {
        assert_eq!(time_cell("0"), "1970-01-01T00:00:00.000Z");
        assert_eq!(time_cell("soon"), "soon");
    }

    // -- parameters and headers ----------------------------------------------

    #[test]
    fn format_and_tables_default_and_refuse_by_name() {
        assert_eq!(Format::parse(None).unwrap(), Format::Csv);
        assert_eq!(Format::parse(Some("")).unwrap(), Format::Csv);
        assert_eq!(Format::parse(Some(" JSON ")).unwrap(), Format::Json);
        assert_eq!(Format::parse(Some("ndjson")).unwrap(), Format::Json);
        assert_eq!(
            Format::parse(Some("xlsx")).unwrap_err().field(),
            Some("format")
        );

        assert_eq!(parse_tables(None).unwrap(), RawTable::ALL.to_vec());
        assert_eq!(parse_tables(Some(" , ")).unwrap(), RawTable::ALL.to_vec());
        assert_eq!(
            parse_tables(Some("vitals,events,vitals")).unwrap(),
            vec![RawTable::Events, RawTable::WebVitals],
            "archive order, duplicates collapsed"
        );
        assert_eq!(
            parse_tables(Some("web_vitals")).unwrap(),
            vec![RawTable::WebVitals]
        );
        assert_eq!(
            parse_tables(Some("events,sessions")).unwrap_err().field(),
            Some("tables")
        );
    }

    #[test]
    fn the_filename_is_built_from_constants_and_dates_only() {
        for format in [Format::Csv, Format::Json] {
            let name = archive_name(format, &window());
            assert!(
                name.bytes().all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'.'),
                "{name}"
            );
            assert!(name.ends_with(".zip"));
        }
        assert_eq!(
            archive_name(Format::Json, &window()),
            "micaforge-raw-json-20260901-20261001.zip"
        );
        for table in RawTable::ALL {
            for format in [Format::Csv, Format::Json] {
                let file = table.file_name(format);
                assert!(!file.contains('/') && !file.contains(".."), "{file}");
            }
        }
    }

    #[test]
    fn the_download_headers_say_zip_attachment_and_no_cache() {
        let response = download(Body::empty(), &archive_name(Format::Csv, &window()));
        let headers = response.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "application/zip");
        assert_eq!(
            headers[header::CONTENT_DISPOSITION],
            "attachment; filename=\"micaforge-raw-csv-20260901-20261001.zip\""
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store, private");
        assert!(headers.get(header::CONTENT_LENGTH).is_none(), "streamed");
    }

    // -- the README ----------------------------------------------------------

    #[test]
    fn the_readme_names_the_site_the_window_and_the_cap() {
        let mut p = plan(Format::Csv, &RawTable::ALL, ROW_CAP);
        p.site_name = "Docs\r\nInjected: line".into();
        let text = readme(
            &p,
            &[FileReport {
                file: "events.csv",
                rows: 1,
                cut: None,
                last_ms: None,
            }],
        );
        assert!(text.starts_with("Micaforge raw data export\n"));
        assert!(text.contains("Site:      Docs  Injected: line (docs.example.com), site id 7\n"));
        assert!(text.contains("2026-09-01T00:00:00.000Z to 2026-10-01T00:00:00.000Z"));
        assert!(text.contains("1,000,000 rows per file"));
        assert!(text.contains("events.csv: 1 row. Complete.\n"));
        assert!(text.contains("do not apply"));
        assert!(
            !text.contains("\r"),
            "no control character from the site name"
        );
    }

    #[test]
    fn a_cut_file_says_why_and_where_to_start_again() {
        let range = window();
        let at = Utc.with_ymd_and_hms(2026, 9, 20, 10, 0, 0).unwrap();
        let capped = file_line(
            &FileReport {
                file: "errors.ndjson",
                rows: 1_000_000,
                cut: Some(Cut::RowCap),
                last_ms: Some(at.timestamp_millis()),
            },
            &range,
        );
        assert_eq!(
            capped,
            "errors.ndjson: 1,000,000 rows. Stopped at the row limit; the window holds more rows. \
             The last row written is from 2026-09-20T10:00:00.000Z. To get the rest, export again \
             with start=2026-09-20T10:00:00.000Z and end=2026-10-01T00:00:00.000Z. Rows from that \
             exact millisecond are in both exports."
        );

        let full = file_line(
            &FileReport {
                file: "web_vitals.csv",
                rows: 0,
                cut: Some(Cut::SizeLimit),
                last_ms: None,
            },
            &range,
        );
        assert!(full.starts_with("web_vitals.csv: 0 rows. Stopped at 4 GiB"));
        assert!(full.contains("Export it on its own"));
    }

    #[test]
    fn the_readme_fits_well_inside_the_reserve_kept_for_it() {
        let mut p = plan(Format::Csv, &RawTable::ALL, ROW_CAP);
        p.site_name = "\u{1f600}".repeat(500);
        p.site_domain = "x".repeat(500);
        let reports: Vec<FileReport> = RawTable::ALL
            .iter()
            .map(|t| FileReport {
                file: t.file_name(Format::Json),
                rows: u64::MAX,
                cut: Some(Cut::RowCap),
                last_ms: Some(i64::from(i32::MAX) * 1000),
            })
            .collect();
        let text = readme(&p, &reports);
        assert!(text.len() < 8 * 1024, "{} bytes", text.len());
        assert!((text.len() as u64) * 2 < zip::TAIL_RESERVE);
    }

    // -- the archive, end to end ---------------------------------------------

    #[tokio::test]
    async fn a_csv_archive_holds_each_file_and_a_readme_that_reports_the_cap() {
        let p = plan(Format::Csv, &RawTable::ALL, 3);
        let (wire, _) = produce(p, |table| match table {
            RawTable::Events => rows(table, 5),
            RawTable::Errors => rows(table, 2),
            RawTable::WebVitals => Vec::new(),
        })
        .await
        .unwrap();

        let files = read_archive(&wire);
        let listed: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            listed,
            ["events.csv", "errors.csv", "web_vitals.csv", "README.txt"]
        );

        let events = file(&files, "events.csv");
        assert_eq!(events.lines().count(), 1 + 3, "header and the capped rows");
        assert!(events.starts_with("site_id,timestamp,event_id,"));
        assert!(events.contains("2026-09-01T00:00:02.000Z"));
        assert!(
            !events.contains("2026-09-01T00:00:03.000Z"),
            "the fourth row is cut"
        );

        assert_eq!(file(&files, "errors.csv").lines().count(), 1 + 2);
        assert_eq!(
            file(&files, "web_vitals.csv").lines().count(),
            1,
            "a header, no rows"
        );

        let readme = file(&files, "README.txt");
        assert!(readme.contains(
            "events.csv: 3 rows. Stopped at the row limit; the window holds more rows. \
             The last row written is from 2026-09-01T00:00:02.000Z."
        ));
        assert!(readme.contains("errors.csv: 2 rows. Complete.\n"));
        assert!(readme.contains("web_vitals.csv: 0 rows. Complete.\n"));
        assert!(readme.contains("3 rows per file"));
    }

    #[tokio::test]
    async fn a_window_with_exactly_the_cap_is_complete() {
        let p = plan(Format::Csv, &[RawTable::Errors], 3);
        let (wire, _) = produce(p, |table| rows(table, 3)).await.unwrap();
        let files = read_archive(&wire);
        assert!(file(&files, "README.txt").contains("errors.csv: 3 rows. Complete."));
    }

    #[tokio::test]
    async fn a_json_archive_of_one_table_is_one_object_per_line() {
        let p = plan(Format::Json, &[RawTable::WebVitals], ROW_CAP);
        let (wire, _) = produce(p, |table| rows(table, 4)).await.unwrap();
        let files = read_archive(&wire);
        let listed: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(listed, ["web_vitals.ndjson", "README.txt"]);

        let lines: Vec<&str> = file(&files, "web_vitals.ndjson").lines().collect();
        assert_eq!(lines.len(), 4);
        for line in lines {
            let object: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(object["country"], "PT");
            assert!(object["lcp"].is_null());
            assert_eq!(object["cls"], 1);
        }
    }

    #[tokio::test]
    async fn a_large_table_streams_as_many_pieces() {
        let p = plan(Format::Csv, &[RawTable::Events], ROW_CAP);
        let (wire, pieces) = produce(p, |table| rows(table, 6_000)).await.unwrap();
        assert!(
            pieces > 3,
            "a file larger than a chunk leaves in pieces: {pieces}"
        );
        let files = read_archive(&wire);
        assert_eq!(file(&files, "events.csv").lines().count(), 1 + 6_000);
    }

    #[tokio::test]
    async fn a_failure_on_the_first_read_is_an_error_before_any_byte() {
        let p = plan(Format::Csv, &RawTable::ALL, ROW_CAP);
        let mut archive = Archive::new(p, |_: &Plan, _| {
            Ok(vec![Err(Error::Database("connection refused".into()))].into_iter())
        });
        // This is the handler's priming read: an error here is a status.
        assert!(archive.next_chunk().await.is_err());
    }

    #[tokio::test]
    async fn a_failure_halfway_cuts_the_body_and_leaves_no_directory() {
        let p = plan(Format::Csv, &RawTable::ALL, ROW_CAP);
        let mut archive = Archive::new(p, |_: &Plan, table| {
            let mut out = rows(table, 3_000);
            if table == RawTable::Errors {
                out.push(Err(Error::Database("lost the connection".into())));
            }
            Ok(out.into_iter())
        });
        let first = archive.next_chunk().await.unwrap();
        assert!(first.is_some(), "the first file starts normally");

        let collected = axum::body::to_bytes(body(archive, first), usize::MAX).await;
        assert!(
            collected.is_err(),
            "the body ends in an error, not a short file"
        );

        // And what had been sent cannot pass for a whole archive.
        let (error, wire) = produce(plan(Format::Csv, &RawTable::ALL, ROW_CAP), |table| {
            let mut out = rows(table, 10);
            if table == RawTable::Errors {
                out.push(Err(Error::Database("lost the connection".into())));
            }
            out
        })
        .await
        .unwrap_err();
        assert!(matches!(error, Error::Database(_)));
        assert!(!wire.is_empty());
        let end_record = [0x50, 0x4b, 0x05, 0x06];
        assert!(
            !wire.windows(4).any(|w| w == end_record),
            "no end of central directory"
        );
    }

    #[tokio::test]
    async fn a_whole_archive_streams_through_the_body() {
        let p = plan(Format::Json, &RawTable::ALL, ROW_CAP);
        let mut archive = Archive::new(p, |_: &Plan, table| Ok(rows(table, 20).into_iter()));
        let first = archive.next_chunk().await.unwrap();
        let wire = axum::body::to_bytes(body(archive, first), usize::MAX)
            .await
            .unwrap();
        let files = read_archive(&wire);
        assert_eq!(files.len(), 4);
        assert_eq!(file(&files, "events.ndjson").lines().count(), 20);
    }
}

/// Every file's statement, against a live ClickHouse.
///
/// The unit tests above feed the archive writer rows by hand; this checks that
/// ClickHouse accepts each statement and sends one text array per row with
/// exactly the columns the file declares. Opt in with
/// `MICAFORGE_TEST_CLICKHOUSE_URL=http://user:password@127.0.0.1:8123` and
/// `cargo test -p micaforge-server raw_export -- --ignored`.
#[cfg(test)]
mod live_tests {
    use super::*;
    use chrono::Duration;
    use micaforge_core::{Audience, Compare, Granularity};
    use micaforge_store::ch::{ErrorRow, EventRow};

    #[tokio::test]
    #[ignore = "needs a ClickHouse server"]
    async fn every_file_reads_from_a_live_store() {
        let raw = std::env::var("MICAFORGE_TEST_CLICKHOUSE_URL")
            .expect("set MICAFORGE_TEST_CLICKHOUSE_URL to run this test");
        let url = url::Url::parse(&raw).expect("a URL");
        let base = format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().expect("a host"),
            url.port().unwrap_or(8123)
        );
        let db = format!("mf_live_raw_export_{}", std::process::id());
        let ch = Ch::connect(&base, &db, url.username(), url.password().unwrap_or("")).unwrap();
        ch.init_schema().await.expect("the schema is created");

        let now = Utc::now();
        let mut vitals = EventRow {
            site_id: 9,
            timestamp: now - Duration::minutes(5),
            kind: "web_vitals".into(),
            pathname: "/docs".into(),
            lcp: Some(1180.0),
            ..EventRow::default()
        };
        vitals.props = r#"{"plan":"pro, \"team\""}"#.into();
        let pageview = EventRow {
            site_id: 9,
            timestamp: now - Duration::minutes(6),
            pathname: "/=SUM(A1)".into(),
            sample_rate: 25,
            ..EventRow::default()
        };
        let mut insert = ch.client().insert::<EventRow>("events").unwrap();
        insert.write(&pageview).await.unwrap();
        insert.write(&vitals).await.unwrap();
        insert.end().await.unwrap();
        let mut insert = ch.client().insert::<ErrorRow>("errors").unwrap();
        insert
            .write(&ErrorRow {
                site_id: 9,
                timestamp: now - Duration::minutes(4),
                session_id: "s".into(),
                visitor_id: "v".into(),
                name: "TypeError".into(),
                message: "x".into(),
                stack: String::new(),
                filename: String::new(),
                lineno: 1,
                colno: 2,
                pathname: "/docs".into(),
                browser: "Chrome".into(),
                os: "macOS".into(),
                release: "r1".into(),
                handled: 0,
                fingerprint: "abcd".into(),
            })
            .await
            .unwrap();
        insert.end().await.unwrap();

        let plan = Plan {
            scope: QueryEnvelope {
                site: 9,
                range: TimeRange::new(
                    now - Duration::hours(1),
                    now + Duration::minutes(1),
                    chrono_tz::UTC,
                ),
                compare: Compare::None,
                granularity: Granularity::Hour,
                filters: None,
                audience: Audience::Human,
                limit: 100,
                page: 1,
            },
            format: Format::Csv,
            tables: RawTable::ALL.to_vec(),
            cap: ROW_CAP,
            site_name: "Docs".into(),
            site_domain: "docs.example.com".into(),
            taken: now,
        };

        for (table, expected) in [
            (RawTable::Events, 2),
            (RawTable::Errors, 1),
            (RawTable::WebVitals, 1),
        ] {
            let mut cursor = open_cursor(&ch, &plan, table).expect("the statement is built");
            let mut rows = Vec::new();
            while let Some(row) = cursor
                .next_row()
                .await
                .expect("ClickHouse accepts the statement")
            {
                assert_eq!(
                    row.len(),
                    table.columns().len(),
                    "{table:?}: one cell per column"
                );
                rows.push(row);
            }
            assert_eq!(rows.len(), expected, "{table:?}");
            let mut out = Vec::new();
            for row in &rows {
                write_csv_row(table.columns(), row, &mut out).expect("the row formats");
                write_json_row(table.columns(), row, &mut out).expect("the row formats");
            }
        }

        // The sampled pageview carries its rate, last in the events file.
        let mut cursor = open_cursor(&ch, &plan, RawTable::Events).unwrap();
        let first = cursor.next_row().await.unwrap().unwrap();
        assert_eq!(first.last().cloned().flatten().as_deref(), Some("25"));

        ch.client()
            .query(&format!("DROP DATABASE IF EXISTS {db}"))
            .execute()
            .await
            .expect("the throwaway database is dropped");
    }
}
