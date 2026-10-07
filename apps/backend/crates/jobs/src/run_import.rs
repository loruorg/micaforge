//! `run_import` — bring a site's history over from GA4, Plausible or Umami.
//!
//! An analytics tool that starts at zero is a tool nobody switches to. This job
//! is the migration path, and it is built around two properties that matter more
//! than speed.
//!
//! # Idempotent, by replacement
//!
//! Every row it writes carries `events.import_id`. Re-running the same import
//! deletes that id's rows first and writes them again, so an import that is run
//! twice — by a retry, by an operator pressing the button again, by a worker that
//! died halfway — leaves exactly one copy. That single column is also what makes
//! an import reversible: undoing one is a delete on it.
//!
//! # Resumable, through the staging table
//!
//! The source file is streamed into `imports_raw` first, verbatim, one row per
//! source record with its position in the file as `seq`. Only then is it
//! normalised into `events`. Two things follow. A job that died at row 400 000
//! resumes from 400 001 instead of re-reading four hundred thousand rows of
//! somebody's CSV. And a mapping bug is re-runnable: the original records are
//! still there, in the shape the exporting tool wrote them, so fixing the mapping
//! is a re-normalise rather than asking the operator to export again.
//!
//! # Header-driven, not format-driven
//!
//! There is no GA4 parser, no Plausible parser and no Umami parser. There is one
//! reader that resolves *columns* through a table of aliases
//! ([`fields`](self)), because the six shapes these three tools export between
//! them — BigQuery event export, Data API aggregate export, Plausible's per-report
//! CSVs, Umami's event dump — differ only in what they call each column. A
//! per-format parser would be three brittle things instead of one tolerant one,
//! and it would break the first time Google renamed `screenPageViews`.
//!
//! # Aggregate exports become events, and the report says so
//!
//! Plausible and the GA4 Data API export *counts*, not events: one row saying
//! this page had 412 views that day. Those rows expand into that many events at
//! the day's start, because `events` is the only place this product stores a
//! pageview. The expansion is capped, logged, and marked with `sdk_version`, so a
//! reader can always tell an expanded row from a real one.

use std::io::BufReader;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use micaforge_core::{new_id, Error, Result};
use micaforge_store::ch::{country_code, EventRow, ImportRawRow};
use micaforge_store::models::{imports, sites};
use micaforge_store::pg::PgPool;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{outbound, JobCtx};

/// Rows per ClickHouse insert, in both directions.
const BATCH_ROWS: usize = 5_000;

/// Most synthetic events one aggregate row may expand into.
///
/// Ten thousand. A single day's row for a single page above this is either a very
/// large site or a misread column, and both are better served by a capped row and
/// a log line than by a hundred million inserts.
const MAX_EXPANSION: u64 = 10_000;

/// Most bytes downloaded when the payload names a URL rather than a file, and
/// most bytes the upload route accepts. One number, so the two ways in agree.
pub const MAX_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Longest a download of an export may take.
///
/// The shared client's twenty seconds is sized for a `robots.txt`; two
/// gigabytes needs longer. Half an hour still finishes well inside this kind's
/// two-hour job timeout, which stays the outer bound.
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How long a scratch file may sit unmodified before the janitor removes it.
///
/// Longer than any import can still need it: eight attempts on the retry
/// curve, each bounded by the two-hour job timeout, come to about eighteen
/// hours. A file older than this belongs to an import that finished, was
/// parked, or was never queued because the process died mid-request.
pub const ORPHAN_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// File-name prefix of every scratch export.
const SCRATCH_PREFIX: &str = "micaforge-import-";

/// What to import, and where from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// The `imports` row this job is executing. Required.
    pub import: Uuid,
    /// `ga4` | `plausible` | `umami`. Defaults to `imports.source`.
    #[serde(default)]
    pub source: Option<String>,
    /// A file the upload handler already wrote to disk.
    #[serde(default)]
    pub path: Option<String>,
    /// A URL to stream the export from, when it was not uploaded.
    #[serde(default)]
    pub url: Option<String>,
}

/// Run one import to completion.
///
/// The `imports` row is moved to `failed` with the reason before the error is
/// returned, so the settings screen shows what went wrong rather than a progress
/// bar that stopped. The error is still returned, so the queue retries it: most
/// import failures are a ClickHouse hiccup rather than a bad file.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let import = imports::get(&ctx.pg, payload.import).await?;

    match execute(ctx, &import, &payload).await {
        Ok(events) => {
            imports::update_progress(&ctx.pg, import.id, 100).await?;
            imports::complete(&ctx.pg, import.id).await?;
            // Only after the row says `done`: a retry of a job whose bookkeeping
            // failed still needs the file. The staged rows keep the originals,
            // so nothing reads the upload again.
            discard_upload(&payload).await;
            tracing::info!(import = %import.id, source = %import.source, events, "import finished");
            Ok(())
        }
        Err(e) => {
            if let Err(recorded) = imports::fail(&ctx.pg, import.id, &e.to_string()).await {
                tracing::error!(import = %import.id, error = %recorded, "could not record the import failure");
            }
            Err(e)
        }
    }
}

/// Stage, then normalise. Returns how many events were written.
async fn execute(ctx: &JobCtx, import: &imports::Import, payload: &Payload) -> Result<u64> {
    let site = sites::get(&ctx.pg, import.site_id).await?;
    let source = payload
        .source
        .clone()
        .unwrap_or_else(|| import.source.clone());

    let (file, temporary) = source_file(ctx, import.id, payload).await?;
    let staged = stage(ctx, import, site.site_id_u16(), &source, &file).await;

    if temporary {
        // The download is scratch. Removing it is best-effort: a temp file that
        // outlives the job is untidy, and failing the import over it would be
        // worse.
        discard(&file).await;
    }

    let staged = staged?;
    tracing::info!(import = %import.id, rows = staged, "source staged");
    imports::update_progress(&ctx.pg, import.id, 50).await?;

    normalise(ctx, import, &site, &source).await
}

/// Where the export actually is.
///
/// A path is used in place; a URL is downloaded to a scratch file first. It is
/// downloaded rather than streamed because the CSV reader is synchronous — it
/// runs on a blocking thread — and bridging an async byte stream into it would
/// buy nothing: an import is not latency-sensitive and the file has to be read
/// twice anyway, once to stage and once to resume.
async fn source_file(ctx: &JobCtx, import: Uuid, payload: &Payload) -> Result<(PathBuf, bool)> {
    if let Some(path) = payload
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let path = PathBuf::from(path);
        if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Err(Error::validation(
                "path",
                format!("{} is not a readable file", path.display()),
            ));
        }
        return Ok((path, false));
    }

    let Some(url) = payload
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    else {
        return Err(Error::validation(
            "path",
            "an import needs either a `path` or a `url` to read from",
        ));
    };

    Ok((download(ctx, import, url).await?, true))
}

/// Stream a remote export to a scratch file.
///
/// A partial file is removed on every failure, so a download that broke off at
/// 1.9 GB does not sit on the disk until the janitor finds it.
async fn download(ctx: &JobCtx, import: Uuid, url: &str) -> Result<PathBuf> {
    let target = crate::fetch::guard_url(url)?;
    let path = scratch_path(import);

    match download_to(ctx, &target, &path).await {
        Ok(bytes) => {
            tracing::info!(url = %outbound::redacted(&target), bytes, "export downloaded");
            Ok(path)
        }
        Err(e) => {
            discard(&path).await;
            Err(e)
        }
    }
}

/// The download itself. Returns how many bytes were written.
async fn download_to(ctx: &JobCtx, target: &url::Url, path: &Path) -> Result<u64> {
    let mut response = ctx
        .http
        .get(target.clone())
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| download_failed(target, &e))?;

    if !response.status().is_success() {
        return Err(Error::Upstream(format!(
            "the export URL answered HTTP {}",
            response.status().as_u16()
        )));
    }

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| Error::Internal(format!("could not open {}: {e}", path.display())))?;

    let mut cap = outbound::BodyCap::new(MAX_DOWNLOAD_BYTES);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| download_failed(target, &e))?
    {
        cap.admit(chunk.len());
        if cap.exceeded() {
            return Err(Error::validation(
                "url",
                format!("the export is larger than {MAX_DOWNLOAD_BYTES} bytes"),
            ));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| Error::Internal(format!("could not write {}: {e}", path.display())))?;
    }
    file.flush()
        .await
        .map_err(|e| Error::Internal(format!("could not flush {}: {e}", path.display())))?;

    Ok(cap.seen())
}

/// A failed download, as the import row will show it.
///
/// The message is stored on the `imports` row and returned to whoever started
/// the import, so it carries the coarse kind only. An address, a port or an
/// operating-system error there would let a user map the server's network one
/// import at a time. The detail goes to the log.
fn download_failed(target: &url::Url, e: &reqwest::Error) -> Error {
    let failure = outbound::Failure::from_reqwest(target, e);
    tracing::warn!(error = %failure, "export download failed");
    Error::Upstream(format!(
        "could not fetch the export: {}",
        failure.public_message()
    ))
}

/// Where an import's export is staged on disk.
///
/// One name per import, in the system temp directory. The upload route writes
/// an uploaded file here and a URL import is downloaded here, so the two ways
/// in look identical from inside the job, and a stray file can be traced back
/// to its `imports` row by name.
pub fn scratch_path(import: Uuid) -> PathBuf {
    std::env::temp_dir().join(format!("{SCRATCH_PREFIX}{import}.csv"))
}

/// True for a file name [`scratch_path`] produces.
fn is_scratch_name(name: &str) -> bool {
    name.strip_prefix(SCRATCH_PREFIX)
        .and_then(|rest| rest.strip_suffix(".csv"))
        .is_some_and(|id| Uuid::parse_str(id).is_ok())
}

/// Remove a file, best effort.
///
/// A file that outlives its import is untidy; failing the import over it would
/// be worse, and [`sweep_orphans`] will find it later.
async fn discard(path: &Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(file = %path.display(), error = %e, "could not remove a scratch export");
        }
    }
}

/// Remove the uploaded file a payload names, once nothing will read it again.
///
/// Only the import's own scratch path is ever removed. The path arrives in a
/// jsonb payload, and a job that deleted whatever file a payload named would be
/// one bad row away from deleting something that matters.
pub async fn discard_upload(payload: &Payload) {
    let Some(path) = payload
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    else {
        return;
    };
    if Path::new(path) != scratch_path(payload.import) {
        tracing::debug!(
            file = path,
            "not removing an export outside the scratch path"
        );
        return;
    }
    discard(Path::new(path)).await;
}

/// The hook for an import that has been parked for good, by the worker after
/// its final attempt or by the janitor after its process died too often.
///
/// No retry will read the upload again, so the file goes. And the `imports` row
/// is marked failed here too: [`run`] records a failure only when `execute`
/// returns one, so a job that panicked, timed out or took its process down
/// would otherwise leave the settings screen showing an import still running.
/// A payload that does not decode names no row and no file to touch.
pub async fn discard_after_failure(pg: &PgPool, payload: &Json, reason: &str) {
    if let Ok(payload) = serde_json::from_value::<Payload>(payload.clone()) {
        if let Err(e) = imports::fail(pg, payload.import, reason).await {
            tracing::error!(import = %payload.import, error = %e, "could not record the import failure");
        }
        discard_upload(&payload).await;
    }
}

/// Remove scratch exports nobody has touched for `max_age`. Returns how many.
///
/// The janitor runs this at boot and then hourly. It is the backstop for the
/// cases the job cannot clean up after: a process that died between writing an
/// upload and queueing its import, and an import whose row was deleted while
/// its file stayed.
pub async fn sweep_orphans(max_age: std::time::Duration) -> usize {
    sweep_dir(&std::env::temp_dir(), max_age).await
}

async fn sweep_dir(dir: &Path, max_age: std::time::Duration) -> usize {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) => {
            tracing::debug!(dir = %dir.display(), error = %e, "could not list the scratch directory");
            return 0;
        }
    };

    let mut removed = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        if !name.to_str().is_some_and(is_scratch_name) {
            continue;
        }
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        // The modification time, not the creation time: an upload still being
        // written is touched with every chunk and is never mistaken for one
        // that was abandoned.
        let idle = meta
            .modified()
            .ok()
            .and_then(|at| at.elapsed().ok())
            .unwrap_or_default();
        if !meta.is_file() || idle < max_age {
            continue;
        }
        match tokio::fs::remove_file(entry.path()).await {
            Ok(()) => removed += 1,
            Err(e) => {
                tracing::debug!(file = %entry.path().display(), error = %e, "could not remove an orphaned export")
            }
        }
    }
    removed
}

/// One record on its way into the staging table.
struct Staged {
    seq: u64,
    timestamp: DateTime<Utc>,
    payload: String,
}

/// A batch of records, and how far through the file they were.
struct Chunk {
    rows: Vec<Staged>,
    byte_pos: u64,
}

/// Copy the source into `imports_raw`, resuming from whatever is already there.
///
/// The CSV reader is synchronous and runs on a blocking thread; batches cross
/// back on a bounded channel, so a slow ClickHouse applies backpressure to the
/// reader instead of letting it buffer the whole file into memory.
async fn stage(
    ctx: &JobCtx,
    import: &imports::Import,
    site_id: u16,
    source: &str,
    file: &Path,
) -> Result<u64> {
    let resume = staged_upto(ctx, import.id, site_id).await?;
    if resume > 0 {
        tracing::info!(import = %import.id, resume, "resuming a partially staged import");
    }

    let total_bytes = tokio::fs::metadata(file)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
        .max(1);

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Chunk>(4);
    let path = file.to_path_buf();
    let reader = tokio::task::spawn_blocking(move || read_csv(&path, resume, tx));

    let mut staged = 0u64;
    let mut last_progress = 0i32;

    while let Some(chunk) = rx.recv().await {
        let rows: Vec<ImportRawRow> = chunk
            .rows
            .iter()
            .map(|row| ImportRawRow {
                import_id: import.id,
                site_id,
                timestamp: row.timestamp,
                source: source.to_string(),
                seq: row.seq,
                payload: row.payload.clone(),
            })
            .collect();

        insert_rows(ctx, "imports_raw", &rows).await?;
        staged += rows.len() as u64;

        // Staging is the first half of the bar.
        let progress = ((chunk.byte_pos * 50) / total_bytes).min(49) as i32;
        if progress > last_progress {
            last_progress = progress;
            imports::update_progress(&ctx.pg, import.id, progress).await?;
        }
    }

    reader
        .await
        .map_err(|e| Error::Internal(format!("the import reader stopped: {e}")))??;

    Ok(staged)
}

/// How far into the source file this import has already been staged.
///
/// `seq` is the record's position in the file, so the highest staged `seq` is the
/// resume point. Records are staged in order and in batches, so at worst the last
/// partial batch is re-read, and the normaliser de-duplicates on `seq` anyway.
async fn staged_upto(ctx: &JobCtx, import: Uuid, site_id: u16) -> Result<u64> {
    let sql = format!(
        "SELECT max(seq) FROM {}.imports_raw WHERE site_id = ? AND import_id = toUUID(?)",
        ctx.ch.db
    );

    ctx.ch
        .client()
        .query(&sql)
        .bind(site_id)
        .bind(import.to_string())
        .fetch_one()
        .await
        .map_err(|e| Error::Database(format!("clickhouse read imports_raw: {e}")))
}

/// Read the CSV on a blocking thread, sending batches back.
///
/// `comment(b'#')` is not decoration: a GA4 Data API export begins with several
/// `# ----` preamble lines before the header, and a reader that treats them as
/// data finds no columns at all.
fn read_csv(path: &Path, resume: u64, tx: tokio::sync::mpsc::Sender<Chunk>) -> Result<u64> {
    let file = std::fs::File::open(path).map_err(|e| {
        Error::validation("path", format!("could not open {}: {e}", path.display()))
    })?;

    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .comment(Some(b'#'))
        .from_reader(BufReader::new(file));

    let headers: Vec<String> = reader
        .headers()
        .map_err(|e| Error::BadRequest(format!("the export has no readable header row: {e}")))?
        .iter()
        .map(normalise_key)
        .collect();

    if headers.iter().all(|h| h.is_empty()) {
        return Err(Error::BadRequest(
            "the export's header row is empty".to_string(),
        ));
    }

    let mut seq = 0u64;
    let mut read = 0u64;
    let mut skipped = 0u64;
    let mut byte_pos = 0u64;
    let mut batch: Vec<Staged> = Vec::with_capacity(BATCH_ROWS);

    for record in reader.records() {
        let record = record
            .map_err(|e| Error::BadRequest(format!("the export stopped being readable: {e}")))?;
        // Taken from the record rather than from the reader: the reader is
        // mutably borrowed for the whole of this loop, and the record's own
        // position is the same number.
        byte_pos = record.position().map(|p| p.byte()).unwrap_or(byte_pos);
        seq += 1;
        if seq <= resume {
            continue;
        }

        let mut object = Map::new();
        for (index, value) in record.iter().enumerate() {
            let Some(key) = headers.get(index).filter(|k| !k.is_empty()) else {
                continue;
            };
            object.insert(key.clone(), Json::String(value.to_string()));
        }

        // A record with no readable time cannot become an event. It is counted
        // and reported rather than dropped in silence.
        let Some(timestamp) = row_timestamp(&object) else {
            skipped += 1;
            continue;
        };

        batch.push(Staged {
            seq,
            timestamp,
            payload: Json::Object(object).to_string(),
        });
        read += 1;

        if batch.len() >= BATCH_ROWS {
            send(&tx, &mut batch, byte_pos)?;
        }
    }

    if !batch.is_empty() {
        send(&tx, &mut batch, byte_pos)?;
    }

    if skipped > 0 {
        tracing::warn!(
            skipped,
            read,
            "records with no readable timestamp were not imported"
        );
    }
    Ok(read)
}

/// Hand a batch to the async side, blocking until it has room.
fn send(
    tx: &tokio::sync::mpsc::Sender<Chunk>,
    batch: &mut Vec<Staged>,
    byte_pos: u64,
) -> Result<()> {
    let rows = std::mem::take(batch);
    tx.blocking_send(Chunk { rows, byte_pos })
        .map_err(|_| Error::Internal("the import writer stopped".to_string()))
}

/// Turn the staged records into `events`.
///
/// The delete comes first and waits for the mutation to finish. Writing before
/// deleting, or deleting asynchronously, would let a re-run double every row for
/// as long as ClickHouse took to catch up.
async fn normalise(
    ctx: &JobCtx,
    import: &imports::Import,
    site: &sites::Site,
    source: &str,
) -> Result<u64> {
    let site_id = site.site_id_u16();
    clear_previous(ctx, import.id, site_id).await?;

    let sql = format!(
        "SELECT seq, toUnixTimestamp64Milli(timestamp), payload
         FROM {}.imports_raw
         WHERE site_id = ? AND import_id = toUUID(?)
         ORDER BY seq",
        ctx.ch.db
    );

    let mut cursor = ctx
        .ch
        .client()
        .query(&sql)
        .bind(site_id)
        .bind(import.id.to_string())
        .fetch::<(u64, i64, String)>()
        .map_err(|e| Error::Database(format!("clickhouse read imports_raw: {e}")))?;

    let host = site
        .domain
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = host.trim_end_matches('/').to_string();

    let mut batch: Vec<EventRow> = Vec::with_capacity(BATCH_ROWS);
    let mut written = 0u64;
    let mut last_seq = 0u64;
    let mut expanded = 0u64;

    while let Some((seq, millis, payload)) = cursor
        .next()
        .await
        .map_err(|e| Error::Database(format!("clickhouse read imports_raw: {e}")))?
    {
        // The staging table is a plain `MergeTree`, so a batch that was written
        // twice by an interrupted run appears twice. `seq` is monotonic, which
        // makes de-duplication one comparison.
        if seq <= last_seq {
            continue;
        }
        last_seq = seq;

        let Ok(Json::Object(object)) = serde_json::from_str::<Json>(&payload) else {
            continue;
        };
        let at = DateTime::from_timestamp_millis(millis).unwrap_or_else(Utc::now);

        let rows = to_events(import.id, site_id, &host, source, at, &object);
        if rows.len() > 1 {
            expanded += rows.len() as u64 - 1;
        }
        batch.extend(rows);

        if batch.len() >= BATCH_ROWS {
            written += flush(ctx, &mut batch).await?;
        }
    }

    written += flush(ctx, &mut batch).await?;

    if expanded > 0 {
        tracing::info!(
            import = %import.id,
            expanded,
            "aggregate rows were expanded into individual events"
        );
    }
    Ok(written)
}

/// Write and clear the batch.
async fn flush(ctx: &JobCtx, batch: &mut Vec<EventRow>) -> Result<u64> {
    if batch.is_empty() {
        return Ok(0);
    }
    let count = batch.len() as u64;
    insert_rows(ctx, "events", batch).await?;
    batch.clear();
    Ok(count)
}

/// Remove anything a previous run of this import wrote.
async fn clear_previous(ctx: &JobCtx, import: Uuid, site_id: u16) -> Result<()> {
    let sql = format!(
        "ALTER TABLE {}.events DELETE WHERE site_id = ? AND import_id = toUUID(?)",
        ctx.ch.db
    );

    ctx.ch
        .client()
        .query(&sql)
        .bind(site_id)
        // `2` waits for the mutation to finish everywhere, not just to be
        // accepted. Anything less and the re-insert races the delete.
        .with_option("mutations_sync", "2")
        .bind(import.to_string())
        .execute()
        .await
        .map_err(|e| Error::Database(format!("clickhouse clear import: {e}")))
}

/// Insert typed rows.
async fn insert_rows<T>(ctx: &JobCtx, table: &str, rows: &[T]) -> Result<()>
where
    T: clickhouse::Row + serde::Serialize + Send + Sync + 'static,
{
    let mut insert = ctx
        .ch
        .client()
        .insert::<T>(table)
        .map_err(|e| Error::Database(format!("clickhouse insert {table}: {e}")))?;

    for row in rows {
        insert
            .write(row)
            .await
            .map_err(|e| Error::Database(format!("clickhouse write {table}: {e}")))?;
    }

    insert
        .end()
        .await
        .map_err(|e| Error::Database(format!("clickhouse commit {table}: {e}")))
}

// ---------------------------------------------------------------------------
// Column mapping
// ---------------------------------------------------------------------------

/// Reduce a header cell to a comparable key.
///
/// Lower-cased with everything that is not a letter or a digit removed, so
/// `Page path`, `page_path`, `pagePath` and `"Page Path "` are all `pagepath`.
/// This is what lets one reader handle six export shapes instead of three
/// parsers handling two each.
fn normalise_key(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The aliases each field is known by across GA4, Plausible and Umami.
mod fields {
    /// A full or partial timestamp.
    pub const TIMESTAMP: &[&str] = &[
        "eventtimestamp",
        "timestamp",
        "createdat",
        "datetime",
        "occurredat",
        "time",
    ];
    /// A date with no time of day.
    pub const DATE: &[&str] = &["eventdate", "date", "day", "visitdate"];
    /// The page.
    pub const PATH: &[&str] = &[
        "pathname",
        "pagepath",
        "urlpath",
        "path",
        "page",
        "pagepathandscreenclass",
        "pagelocation",
        "url",
    ];
    /// The host it was served from.
    pub const HOST: &[&str] = &["hostname", "host", "urlhost", "domain"];
    /// Where the visit came from.
    pub const REFERRER: &[&str] = &["referrer", "pagereferrer", "referrerdomain", "referer"];
    /// A stable pseudonymous identifier for the person.
    pub const VISITOR: &[&str] = &[
        "userpseudoid",
        "visitorid",
        "distinctid",
        "visitor",
        "userid",
    ];
    /// A stable identifier for the visit.
    pub const SESSION: &[&str] = &["sessionid", "gasessionid", "visitid", "session"];
    /// The name of a custom event.
    pub const EVENT: &[&str] = &["eventname", "eventtype", "event", "name"];
    /// A count, for the aggregate exports.
    pub const COUNT: &[&str] = &["pageviews", "screenpageviews", "views", "hits", "count"];
    /// A visitor count, used only when there is no pageview count.
    pub const VISITORS: &[&str] = &["visitors", "totalusers", "activeusers", "users"];
    /// The page's title.
    pub const TITLE: &[&str] = &["pagetitle", "title", "unifiedscreenname"];
    /// Where the visitor was.
    pub const COUNTRY: &[&str] = &["country", "countryid", "countrycode", "geocountry"];
    /// What they used.
    pub const BROWSER: &[&str] = &["browser", "browsername"];
    /// What it ran on.
    pub const OS: &[&str] = &["os", "operatingsystem", "osname", "platform"];
    /// Desktop, mobile or tablet.
    pub const DEVICE: &[&str] = &["device", "devicetype", "devicecategory", "screensize"];
    /// The visitor's language.
    pub const LANGUAGE: &[&str] = &["language", "locale"];
    /// Campaign tags.
    pub const UTM_SOURCE: &[&str] = &["utmsource", "source", "sessionsource", "trafficsource"];
    /// The medium.
    pub const UTM_MEDIUM: &[&str] = &["utmmedium", "medium", "sessionmedium"];
    /// The campaign.
    pub const UTM_CAMPAIGN: &[&str] = &["utmcampaign", "campaign", "sessioncampaignname", "utm"];
}

/// The first alias present in the record, trimmed, if it holds anything.
fn pick<'a>(object: &'a Map<String, Json>, aliases: &[&str]) -> Option<&'a str> {
    for alias in aliases {
        if let Some(Json::String(value)) = object.get(*alias) {
            let value = value.trim();
            if !value.is_empty() && value != "(not set)" && value != "(none)" {
                return Some(value);
            }
        }
    }
    None
}

/// The same, as an owned string, or empty.
fn text(object: &Map<String, Json>, aliases: &[&str]) -> String {
    pick(object, aliases).unwrap_or_default().to_string()
}

/// When a staged record happened.
///
/// Tried in order: an explicit timestamp column, then a date column. A GA4
/// BigQuery export carries microseconds since the epoch as a sixteen-digit
/// integer; a Data API export carries `20260821`; Plausible and Umami carry ISO
/// text. All four turn up, so all four are read.
pub fn row_timestamp(object: &Map<String, Json>) -> Option<DateTime<Utc>> {
    if let Some(raw) = pick(object, fields::TIMESTAMP) {
        if let Some(at) = parse_instant(raw) {
            return Some(at);
        }
    }
    pick(object, fields::DATE).and_then(parse_instant)
}

/// Parse any of the time shapes these exports use.
pub fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    // A bare integer is an epoch, and the magnitude says in what unit. GA4's
    // `event_timestamp` is microseconds; Umami and most exports use seconds or
    // milliseconds. Reading a microsecond value as seconds would file the row in
    // the year 500 000.
    if raw.chars().all(|c| c.is_ascii_digit()) && raw.len() >= 8 {
        if let Ok(number) = raw.parse::<i64>() {
            if raw.len() == 8 {
                // `20260821`, GA4's `event_date`.
                return NaiveDate::parse_from_str(raw, "%Y%m%d")
                    .ok()
                    .and_then(|d| d.and_hms_opt(0, 0, 0))
                    .map(|d| d.and_utc());
            }
            return match raw.len() {
                0..=10 => DateTime::from_timestamp(number, 0),
                11..=13 => DateTime::from_timestamp_millis(number),
                _ => DateTime::from_timestamp_micros(number),
            };
        }
    }

    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Some(at.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            return Some(Utc.from_utc_datetime(&naive));
        }
    }
    for format in ["%Y-%m-%d", "%d/%m/%Y", "%Y%m%d"] {
        if let Ok(date) = NaiveDate::parse_from_str(raw, format) {
            return date.and_hms_opt(0, 0, 0).map(|d| d.and_utc());
        }
    }
    None
}

/// Turn one staged record into the events it represents.
///
/// Usually one. An aggregate row carrying a count becomes that many, each a
/// second apart so they land in the same hour without sharing a timestamp, which
/// keeps a minute-granularity chart from showing a single vertical spike where a
/// day of traffic should be.
fn to_events(
    import: Uuid,
    site_id: u16,
    site_host: &str,
    source: &str,
    at: DateTime<Utc>,
    object: &Map<String, Json>,
) -> Vec<EventRow> {
    let hostname = pick(object, fields::HOST)
        .map(str::to_string)
        .unwrap_or_else(|| site_host.to_string());

    let raw_path = text(object, fields::PATH);
    let (pathname, querystring) = split_path(&raw_path);
    let referrer = text(object, fields::REFERRER);

    let url = url::Url::parse(&format!(
        "https://{hostname}{pathname}{}{querystring}",
        if querystring.is_empty() { "" } else { "?" }
    ))
    .ok();

    let attribution = url.as_ref().map(|url| {
        micaforge_enrich::referrer::attribute(
            Some(referrer.as_str()).filter(|r| !r.is_empty()),
            url,
            site_host,
            &|host| {
                micaforge_agents::answer_engines::classify_referrer_host(host)
                    .map(|engine| engine.slug)
            },
        )
    });
    let utm = url
        .as_ref()
        .map(micaforge_enrich::referrer::parse_utm)
        .unwrap_or_default();

    let event_name = text(object, fields::EVENT);
    let is_pageview = event_name.is_empty()
        || matches!(
            event_name.as_str(),
            "page_view" | "pageview" | "pageviews" | "screen_view"
        );

    // The count column is what makes an aggregate export an aggregate export.
    // Visitors are the fallback so a Plausible `imported_sources.csv`, which has
    // no pageview column, still contributes something rather than nothing.
    let count = pick(object, fields::COUNT)
        .or_else(|| pick(object, fields::VISITORS))
        .and_then(|raw| raw.replace([',', ' '], "").parse::<u64>().ok())
        .unwrap_or(1)
        .clamp(1, MAX_EXPANSION);

    let identity = pick(object, fields::VISITOR)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}|{}", at.date_naive(), pathname));
    let session_key = pick(object, fields::SESSION)
        .map(str::to_string)
        .unwrap_or_else(|| identity.clone());

    (0..count)
        .map(|index| {
            // Each synthetic event gets its own identity, or a hundred imported
            // pageviews of one page would look like one very busy person.
            let suffix = if count > 1 {
                index.to_string()
            } else {
                String::new()
            };

            EventRow {
                site_id,
                timestamp: at + chrono::Duration::seconds(index as i64),
                event_id: new_id(),
                session_id: digest(import, &format!("s:{session_key}:{suffix}")),
                visitor_id: digest(import, &format!("v:{identity}:{suffix}")),
                kind: if is_pageview { "pageview" } else { "custom" }.to_string(),
                event_name: if is_pageview {
                    String::new()
                } else {
                    event_name.clone()
                },
                hostname: hostname.clone(),
                pathname: pathname.clone(),
                page_title: text(object, fields::TITLE),
                querystring: querystring.clone(),
                referrer: referrer.clone(),
                referrer_host: attribution
                    .as_ref()
                    .map(|a| a.referrer_host.clone())
                    .unwrap_or_default(),
                channel: attribution
                    .as_ref()
                    .map(|a| a.channel.as_str().to_string())
                    .unwrap_or_else(|| "direct".to_string()),
                source: attribution
                    .as_ref()
                    .map(|a| a.source.clone())
                    .unwrap_or_default(),
                utm_source: first(&utm.source, &text(object, fields::UTM_SOURCE)),
                utm_medium: first(&utm.medium, &text(object, fields::UTM_MEDIUM)),
                utm_campaign: first(&utm.campaign, &text(object, fields::UTM_CAMPAIGN)),
                browser: text(object, fields::BROWSER),
                os: text(object, fields::OS),
                device_type: device_class(&text(object, fields::DEVICE)),
                language: text(object, fields::LANGUAGE),
                country: country_code(&text(object, fields::COUNTRY)),
                entry_path: pathname.clone(),
                exit_path: pathname.clone(),
                import_id: Some(import),
                // Marked, always. A reader has to be able to tell an imported row
                // from a collected one without joining another table.
                sdk_version: format!("import/{source}"),
                ..EventRow::default()
            }
        })
        .collect()
}

/// Split a path column into a path and a query string.
///
/// The column may hold a bare path, a path with a query, or a whole URL: GA4's
/// `page_location` is a URL and its `pagePath` is not.
fn split_path(raw: &str) -> (String, String) {
    let raw = raw.trim();
    if raw.is_empty() {
        return ("/".to_string(), String::new());
    }

    let (path, query) = match url::Url::parse(raw) {
        Ok(url) if url.has_host() => (
            url.path().to_string(),
            url.query().unwrap_or_default().to_string(),
        ),
        _ => match raw.split_once('?') {
            Some((path, query)) => (path.to_string(), query.to_string()),
            None => (raw.to_string(), String::new()),
        },
    };

    let path = micaforge_enrich::url_norm::normalise_path(&path, false);
    (
        path,
        query.split('#').next().unwrap_or_default().to_string(),
    )
}

/// The first non-empty of two values.
fn first(preferred: &str, fallback: &str) -> String {
    if preferred.is_empty() {
        fallback.to_string()
    } else {
        preferred.to_string()
    }
}

/// Reduce whatever the export called a device to the contract's vocabulary.
fn device_class(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => String::new(),
        "desktop" | "laptop" | "computer" => "desktop".to_string(),
        "mobile" | "phone" | "smartphone" => "mobile".to_string(),
        "tablet" => "tablet".to_string(),
        "tv" | "smart tv" | "smarttv" | "connected tv" => "tv".to_string(),
        "wearable" | "watch" => "wearable".to_string(),
        "console" | "game console" => "console".to_string(),
        other => other.to_string(),
    }
}

/// A stable pseudonymous id for an imported identity.
///
/// The import id is mixed in so the same visitor id from two different exports
/// does not collapse into one person, and the result is a truncated SHA-256 hex
/// digest, matching the width of a live `visitor_id`. Nothing reversible and
/// nothing derived from an address: the source's own pseudonym is all that goes
/// in, and it is already a pseudonym.
fn digest(import: Uuid, key: &str) -> String {
    let full = micaforge_core::crypto::token_hash(&format!("{import}:{key}"));
    full.chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(pairs: &[(&str, &str)]) -> Map<String, Json> {
        pairs
            .iter()
            .map(|(k, v)| (normalise_key(k), Json::String((*v).to_string())))
            .collect()
    }

    #[test]
    fn headers_normalise_to_one_spelling() {
        assert_eq!(normalise_key("Page path"), "pagepath");
        assert_eq!(normalise_key("page_path"), "pagepath");
        assert_eq!(normalise_key("pagePath"), "pagepath");
        assert_eq!(normalise_key("  Page Path  "), "pagepath");
        assert_eq!(normalise_key("screenPageViews"), "screenpageviews");
        assert_eq!(normalise_key(""), "");
    }

    #[test]
    fn every_time_shape_these_exports_use_is_read() {
        // GA4 BigQuery: microseconds.
        let micros = parse_instant("1755777600000000").unwrap();
        assert_eq!(micros.timestamp(), 1_755_777_600);
        // GA4 Data API: a bare date.
        assert_eq!(
            parse_instant("20260821").unwrap().to_rfc3339(),
            "2026-08-21T00:00:00+00:00"
        );
        // Plausible and Umami: ISO text.
        assert!(parse_instant("2026-08-21T09:30:00Z").is_some());
        assert!(parse_instant("2026-08-21 09:30:00").is_some());
        assert!(parse_instant("2026-08-21").is_some());
        // Seconds and milliseconds.
        assert_eq!(
            parse_instant("1755777600").unwrap().timestamp(),
            1_755_777_600
        );
        assert_eq!(
            parse_instant("1755777600000").unwrap().timestamp(),
            1_755_777_600
        );

        assert!(parse_instant("").is_none());
        assert!(parse_instant("yesterday").is_none());
    }

    #[test]
    fn a_row_with_no_time_is_not_an_event() {
        assert!(row_timestamp(&object(&[("page", "/a")])).is_none());
        assert!(row_timestamp(&object(&[("date", "2026-08-21")])).is_some());
        // A timestamp column wins over a date column.
        let both = object(&[
            ("date", "2026-08-01"),
            ("timestamp", "2026-08-21T09:00:00Z"),
        ]);
        assert_eq!(
            row_timestamp(&both).unwrap().date_naive().to_string(),
            "2026-08-21"
        );
    }

    #[test]
    fn ga4_placeholder_values_are_read_as_absent() {
        let row = object(&[("country", "(not set)"), ("browser", "Chrome")]);
        assert_eq!(pick(&row, fields::COUNTRY), None);
        assert_eq!(pick(&row, fields::BROWSER), Some("Chrome"));
    }

    #[test]
    fn a_path_column_may_be_a_path_a_query_or_a_whole_url() {
        assert_eq!(
            split_path("/docs/guide"),
            ("/docs/guide".into(), String::new())
        );
        assert_eq!(
            split_path("/docs/guide?utm_source=x"),
            ("/docs/guide".into(), "utm_source=x".into())
        );
        assert_eq!(
            split_path("https://example.com/docs/guide/?a=1#top"),
            ("/docs/guide".into(), "a=1".into())
        );
        assert_eq!(split_path(""), ("/".into(), String::new()));
    }

    #[test]
    fn one_event_row_stays_one_row() {
        let row = object(&[
            ("date", "2026-08-21"),
            ("page path", "/docs/guide"),
            ("event name", "page_view"),
        ]);
        let at = row_timestamp(&row).unwrap();
        let events = to_events(Uuid::nil(), 1, "example.com", "ga4", at, &row);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pathname, "/docs/guide");
        assert_eq!(events[0].kind, "pageview");
        assert_eq!(events[0].event_name, "");
        assert_eq!(events[0].import_id, Some(Uuid::nil()));
        assert_eq!(events[0].sdk_version, "import/ga4");
        assert_eq!(events[0].visitor_id.len(), 16);
    }

    #[test]
    fn an_aggregate_row_expands_into_the_events_it_counts() {
        let row = object(&[
            ("date", "2026-08-21"),
            ("Page path", "/pricing"),
            ("screenPageViews", "3"),
        ]);
        let at = row_timestamp(&row).unwrap();
        let events = to_events(Uuid::nil(), 1, "example.com", "ga4", at, &row);

        assert_eq!(events.len(), 3);
        // Each expanded event is its own visitor, or one busy page would look
        // like one very busy person.
        assert_ne!(events[0].visitor_id, events[1].visitor_id);
        // And they are spread through the period rather than stacked on one
        // instant.
        assert!(events[1].timestamp > events[0].timestamp);
    }

    #[test]
    fn an_expansion_cannot_run_away() {
        let row = object(&[
            ("date", "2026-08-21"),
            ("page", "/"),
            ("pageviews", "99999999"),
        ]);
        let at = row_timestamp(&row).unwrap();
        let events = to_events(Uuid::nil(), 1, "example.com", "ga4", at, &row);
        assert_eq!(events.len() as u64, MAX_EXPANSION);
    }

    #[test]
    fn a_thousands_separator_in_a_count_is_read() {
        let row = object(&[("date", "2026-08-21"), ("page", "/"), ("views", "1,204")]);
        let at = row_timestamp(&row).unwrap();
        assert_eq!(
            to_events(Uuid::nil(), 1, "example.com", "ga4", at, &row).len(),
            1_204
        );
    }

    #[test]
    fn an_answer_engine_referrer_survives_the_import() {
        let row = object(&[
            ("timestamp", "2026-08-21T09:00:00Z"),
            ("page", "/docs/guide"),
            ("referrer", "https://chatgpt.com/"),
        ]);
        let at = row_timestamp(&row).unwrap();
        let events = to_events(Uuid::nil(), 1, "example.com", "plausible", at, &row);

        // The whole product depends on this classification surviving a migration:
        // an import that filed every ChatGPT arrival as a referral would erase the
        // history the Citation Gap is measured against.
        assert_eq!(events[0].channel, "answer_engine");
        assert_eq!(events[0].source, "chatgpt");
    }

    #[test]
    fn a_custom_event_keeps_its_name() {
        let row = object(&[
            ("created_at", "2026-08-21T09:00:00Z"),
            ("url_path", "/checkout"),
            ("event_name", "purchase"),
        ]);
        let at = row_timestamp(&row).unwrap();
        let events = to_events(Uuid::nil(), 1, "example.com", "umami", at, &row);
        assert_eq!(events[0].kind, "custom");
        assert_eq!(events[0].event_name, "purchase");
    }

    #[test]
    fn device_names_reduce_to_the_contracts_vocabulary() {
        assert_eq!(device_class("Desktop"), "desktop");
        assert_eq!(device_class("smartphone"), "mobile");
        assert_eq!(device_class("Smart TV"), "tv");
        assert_eq!(device_class(""), "");
        // An unrecognised value is kept rather than invented over.
        assert_eq!(device_class("fridge"), "fridge");
    }

    #[test]
    fn two_imports_of_the_same_visitor_stay_two_people() {
        let a = digest(Uuid::from_u128(1), "v:user-7:");
        let b = digest(Uuid::from_u128(2), "v:user-7:");
        assert_ne!(a, b);
        assert_eq!(a.len(), 16);
        // And the same import twice is the same person, which is what makes a
        // re-run produce identical rows.
        assert_eq!(a, digest(Uuid::from_u128(1), "v:user-7:"));
    }

    #[test]
    fn only_names_the_scratch_path_produces_are_scratch() {
        let id = Uuid::from_u128(7);
        let name = scratch_path(id)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .to_string();
        assert!(is_scratch_name(&name));
        assert!(!is_scratch_name("micaforge-import-notauuid.csv"));
        assert!(!is_scratch_name("micaforge-import-.csv"));
        assert!(!is_scratch_name(&format!("micaforge-import-{id}.csv.bak")));
        assert!(!is_scratch_name("passwd"));
    }

    #[tokio::test]
    async fn the_sweep_removes_idle_scratch_files_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("micaforge-sweep-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let scratch = dir.join(format!("{SCRATCH_PREFIX}{}.csv", Uuid::new_v4()));
        let other = dir.join("keep-me.csv");
        tokio::fs::write(&scratch, b"date,pageviews\n")
            .await
            .unwrap();
        tokio::fs::write(&other, b"not ours").await.unwrap();

        // Freshly written, so a day's grace keeps it.
        assert_eq!(sweep_dir(&dir, ORPHAN_AFTER).await, 0);
        assert!(scratch.exists());

        // With no grace at all it goes, and the stranger's file stays.
        assert_eq!(sweep_dir(&dir, std::time::Duration::ZERO).await, 1);
        assert!(!scratch.exists());
        assert!(other.exists());

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn a_payload_cannot_make_the_job_delete_an_arbitrary_file() {
        let dir = std::env::temp_dir().join(format!("micaforge-discard-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let precious = dir.join("precious.csv");
        tokio::fs::write(&precious, b"keep").await.unwrap();

        let import = Uuid::new_v4();
        discard_upload(&Payload {
            import,
            path: Some(precious.display().to_string()),
            ..Payload::default()
        })
        .await;
        assert!(precious.exists());

        let own = scratch_path(import);
        tokio::fs::write(&own, b"date\n").await.unwrap();
        discard_upload(&Payload {
            import,
            path: Some(own.display().to_string()),
            ..Payload::default()
        })
        .await;
        assert!(!own.exists());

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
