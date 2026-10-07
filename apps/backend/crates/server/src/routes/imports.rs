//! `/api/imports/*` — migrating GA4, Plausible and Umami history in.
//!
//! One row per import, tracking status and progress so the settings screen can
//! show a bar and so a failed import says why. The events themselves land in
//! ClickHouse with `import_id` set to that row's id, which is what makes an
//! import reversible: undoing one is a delete on a single column.
//!
//! ## The upload is authorised first, then streamed, never buffered
//!
//! A decade of GA4 is gigabytes, and reading it into a `Vec<u8>` before deciding
//! whether the caller may even touch this site would let any authenticated
//! account exhaust the host's memory or its disk. So:
//!
//! - the route has an explicit body limit, [`BODY_LIMIT`]: the file ceiling
//!   plus room for the text fields and the multipart framing;
//! - every text field is read under [`MAX_FIELD_BYTES`];
//! - the `site` field must come before the `file` part, and the caller's role on
//!   that site is checked before a single byte of the file is written;
//! - the file is streamed to a scratch file chunk by chunk under a hard ceiling
//!   ([`MAX_UPLOAD_BYTES`]), and deleted again if anything after that fails.
//!
//! The scratch path is [`micaforge_jobs::run_import::scratch_path`], the one the
//! importer uses for a URL download, so both routes into the importer look
//! identical from inside it. The importer removes the file when the job is done
//! or parked for good, and the worker's janitor removes any that outlive that.
//!
//! ## The importer reads CSV
//!
//! All three sources export CSV, and the staging reader is a CSV reader. An
//! upload with any other extension is refused *here*, with a message naming what
//! to export, rather than accepted and failed by a worker twenty minutes later
//! with a parse error nobody can act on.

use axum::extract::multipart::Field;
use axum::extract::{Multipart, Path, State};
use axum::routing::get;
use axum::{Json, Router};
use micaforge_core::{new_id, Error, Result};
use micaforge_store::models::imports;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use micaforge_jobs::outbound::BodyCap;
use micaforge_store::models::sites::Site;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::{authorize_site, authorize_site_ref};
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The three tools Micaforge can read an export from.
const SOURCES: &[&str] = &["ga4", "plausible", "umami"];

/// Hard ceiling on an uploaded export.
///
/// Two gigabytes, the same constant the importer downloads a URL under. Past
/// this the export belongs on a URL the worker can stream at its own pace
/// rather than inside a request a browser is holding open.
const MAX_UPLOAD_BYTES: u64 = micaforge_jobs::run_import::MAX_DOWNLOAD_BYTES;

/// Most bytes one text field may carry.
///
/// `site`, `source` and `url` are all short, and a browser form's stray CSRF
/// token or button name is shorter still. Four kilobytes is generous for every
/// one of them and makes a field a poor place to hide a payload.
const MAX_FIELD_BYTES: usize = 4 * 1024;

/// Room in the body for everything that is not the file: the text fields and
/// the multipart boundaries and part headers around them.
const MULTIPART_OVERHEAD: usize = 64 * 1024;

/// The request-body limit on `POST /api/imports`.
///
/// The file ceiling plus [`MULTIPART_OVERHEAD`], so that a file just under the
/// ceiling is never refused by the transport, while a body that is larger than
/// anything this route will accept is cut off by the transport instead of being
/// read to the end. The file ceiling is still checked here, chunk by chunk,
/// because that check is the one that can explain itself.
pub const BODY_LIMIT: usize = MAX_UPLOAD_BYTES as usize + MULTIPART_OVERHEAD;

/// Imports one account may start per hour.
///
/// Each one writes a multi-gigabyte scratch file, so this is a disk limit rather
/// than an abuse limit: ten stalled uploads is already more than the host should
/// be holding.
const IMPORT_LIMIT: u32 = 10;
/// That window, in seconds.
const IMPORT_WINDOW: u32 = 3600;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every import route.
///
/// `POST /api/imports` reads a multipart body, so the application's JSON body
/// limit must not apply to this path; it gets [`BODY_LIMIT`] instead.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/imports", get(list_imports).post(create_import))
        .route("/api/imports/{id}", get(get_import).delete(delete_import))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/imports?site=…` — every import on a site, newest first.
async fn list_imports(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = imports::list_for_site(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(import_json).collect::<Vec<_>>()))
}

/// What the upload form sent, once every part has been read.
#[derive(Debug, Default)]
struct Upload {
    /// The site, as a UUID or as the numeric id from the snippet.
    site: Option<String>,
    /// The site, once the caller's role on it has been checked. Set before a
    /// file part is streamed; a `url`-only form is authorised afterwards.
    authorized: Option<Site>,
    /// `ga4` | `plausible` | `umami`.
    source: Option<String>,
    /// A URL to fetch instead of uploading a file.
    url: Option<String>,
    /// The uploaded name, kept only to check the extension.
    filename: Option<String>,
    /// How much was written to the scratch file.
    bytes: u64,
}

/// `POST /api/imports` — upload an export, or point at a URL.
///
/// Multipart fields: `site`, `source`, and then either a `file` part or a `url`
/// field, in that order. The caller's role on the site is checked when the
/// `file` part arrives and before any of it is written, so `site` has to come
/// first; a form that sends the file before the site is refused. The scratch
/// file is removed on every failure path, so a refused upload leaves nothing
/// behind.
async fn create_import(
    State(st): State<AppState>,
    auth: AuthUser,
    multipart: Multipart,
) -> ApiResult<Json<Value>> {
    // Counted per key for a key, because every key acts as its organisation's
    // owner, and one integration must not spend the owner's whole allowance.
    let caller = match &auth.key {
        Some(grant) => format!("key:{}", grant.key_id),
        None => format!("user:{}", auth.user.id),
    };
    if !st
        .kv
        .rate_limit(&format!("import:{caller}"), IMPORT_LIMIT, IMPORT_WINDOW)
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(IMPORT_WINDOW),
        }
        .into());
    }

    // Minted before the body is read so the scratch file can be named after the
    // row it will become, which is what lets a stray file be traced back.
    let import_id = new_id();
    let path = scratch_path(import_id);

    let upload = match read_upload(&st, &auth, multipart, &path).await {
        Ok(upload) => upload,
        Err(e) => {
            discard(&path).await;
            return Err(e.into());
        }
    };

    match start_import(&st, &auth, import_id, &path, &upload).await {
        Ok(payload) => Ok(ok(payload)),
        Err(e) => {
            discard(&path).await;
            Err(e.into())
        }
    }
}

/// Authorise the upload and queue the work.
///
/// Split from the handler so every failure inside it is caught by one cleanup
/// path rather than repeating the file removal at each `?`.
async fn start_import(
    st: &AppState,
    auth: &AuthUser,
    import_id: Uuid,
    path: &std::path::Path,
    upload: &Upload,
) -> Result<Value> {
    let site = match &upload.authorized {
        Some(site) => site.clone(),
        None => authorize_upload_site(st, auth, upload.site.as_deref()).await?,
    };

    let source = check_source(upload.source.as_deref())?;
    check_extension(upload.filename.as_deref())?;

    let has_file = upload.bytes > 0;
    let url = upload
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    if !has_file && url.is_none() {
        return Err(Error::validation(
            "file",
            "attach an export, or give a `url` to fetch it from",
        ));
    }
    if let Some(url) = url {
        // The same guard the crawler uses: an import URL is a server-side fetch
        // of whatever the caller names, so it must not reach the local network.
        micaforge_jobs::fetch::guard_url(url)
            .map_err(|e| Error::validation("url", e.to_string()))?;
    }

    let import = imports::create(&st.pg, import_id, site.id, source).await?;

    let queued = micaforge_jobs::enqueue_now(
        &st.pg,
        micaforge_jobs::Kind::RunImport,
        &micaforge_jobs::run_import::Payload {
            import: import.id,
            source: Some(source.to_string()),
            // A file wins over a URL: somebody who attached both meant the file.
            path: has_file.then(|| path.display().to_string()),
            url: if has_file {
                None
            } else {
                url.map(str::to_string)
            },
        },
    )
    .await?;

    tracing::info!(
        import = %import.id,
        site = %site.id,
        source,
        bytes = upload.bytes,
        "import queued"
    );

    let mut payload = import_json(&import);
    payload["job_id"] = json!(queued);
    payload["uploaded_bytes"] = json!(upload.bytes);
    Ok(payload)
}

/// `GET /api/imports/{id}` — one import's progress.
async fn get_import(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let import = imports::get(&st.pg, id).await?;
    reachable(&st, import.site_id, auth.user.id, VIEWER).await?;
    Ok(ok(import_json(&import)))
}

/// `DELETE /api/imports/{id}` — forget the import record.
///
/// Deliberately does **not** remove the events it wrote. Undoing an import is a
/// separate, explicit operation keyed on `events.import_id`, and a settings
/// screen that silently deleted a year of history because somebody tidied a list
/// would be a trap. The response says so rather than leaving it to be discovered.
async fn delete_import(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let import = imports::get(&st.pg, id).await?;
    reachable(&st, import.site_id, auth.user.id, MEMBER).await?;

    if import.status == "running" {
        return Err(Error::Conflict(
            "this import is still running; wait for it to finish or fail".into(),
        )
        .into());
    }

    imports::delete(&st.pg, import.id).await?;
    // Normally the importer has already removed the file; this covers a
    // record deleted before its job ever ran.
    discard(&scratch_path(import.id)).await;

    Ok(ok(json!({
        "deleted": import.id,
        "events_removed": false,
        "note": "the imported events remain; removing them is a separate operation on events.import_id",
    })))
}

// ---------------------------------------------------------------------------
// Multipart
// ---------------------------------------------------------------------------

/// Drain the multipart body, streaming any file part to `path`.
///
/// The site is authorised when the `file` part arrives, before the scratch file
/// is even created, so a caller with no role on the site never gets to write a
/// byte to this host's disk.
async fn read_upload(
    st: &AppState,
    auth: &AuthUser,
    mut multipart: Multipart,
    path: &std::path::Path,
) -> Result<Upload> {
    let mut upload = Upload::default();
    let mut file: Option<tokio::fs::File> = None;
    let mut cap = BodyCap::new(MAX_UPLOAD_BYTES);

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| Error::BadRequest(format!("malformed upload: {e}")))?
    {
        // Both borrow the field, and streaming it needs `&mut`, so they are
        // taken as owned values first.
        let name = field.name().unwrap_or_default().to_string();
        let filename = field.file_name().map(str::to_string);

        if name != "file" {
            let text = read_text_field(&mut field, &name).await?;
            match name.as_str() {
                "site" => upload.site = Some(text),
                "source" => upload.source = Some(text),
                "url" => upload.url = Some(text),
                // An unknown field is ignored rather than refused: a browser
                // form habitually carries a CSRF token or a submit button name.
                _ => {}
            }
            continue;
        }

        if file.is_some() {
            return Err(Error::validation("file", "send one file per import"));
        }

        // Everything that can be judged without the file is judged now, so a
        // refusal costs the caller nothing but this request.
        if upload.site.is_none() {
            return Err(Error::validation(
                "site",
                "must be sent before the `file` part, so the upload can be authorised first",
            ));
        }
        let site = authorize_upload_site(st, auth, upload.site.as_deref()).await?;
        if upload.source.is_some() {
            check_source(upload.source.as_deref())?;
        }
        check_extension(filename.as_deref())?;
        upload.authorized = Some(site);
        upload.filename = filename;

        let handle = file.insert(
            tokio::fs::File::create(path)
                .await
                .map_err(|e| Error::internal(format!("could not open {}: {e}", path.display())))?,
        );

        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|e| Error::BadRequest(format!("upload interrupted: {e}")))?
        {
            cap.admit(chunk.len());
            if cap.exceeded() {
                return Err(Error::validation(
                    "file",
                    format!(
                        "is larger than {} GB; put it somewhere the importer can fetch it and send a `url` instead",
                        MAX_UPLOAD_BYTES / (1024 * 1024 * 1024)
                    ),
                ));
            }
            handle
                .write_all(&chunk)
                .await
                .map_err(|e| Error::internal(format!("could not write {}: {e}", path.display())))?;
        }
        upload.bytes = cap.seen();
    }

    if let Some(mut handle) = file {
        handle
            .flush()
            .await
            .map_err(|e| Error::internal(format!("could not flush {}: {e}", path.display())))?;
    }

    Ok(upload)
}

/// Read one text field, refusing it past [`MAX_FIELD_BYTES`].
///
/// Read chunk by chunk rather than with `Field::text`, which would buffer
/// whatever the field holds before anything could measure it.
async fn read_text_field(field: &mut Field<'_>, name: &str) -> Result<String> {
    let mut cap = BodyCap::new(MAX_FIELD_BYTES as u64);
    let mut buf = Vec::new();

    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|e| Error::BadRequest(format!("unreadable `{name}` field: {e}")))?
    {
        cap.admit(chunk.len());
        if cap.exceeded() {
            return Err(Error::validation(
                name,
                format!("is longer than {MAX_FIELD_BYTES} bytes"),
            ));
        }
        buf.extend_from_slice(&chunk);
    }

    String::from_utf8(buf).map_err(|_| Error::validation(name, "is not valid UTF-8"))
}

/// Resolve the `site` field and check the caller may import into it.
async fn authorize_upload_site(
    st: &AppState,
    auth: &AuthUser,
    reference: Option<&str>,
) -> Result<Site> {
    let reference = reference
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::validation("site", "is required"))?;
    authorize_site_ref(st, reference, auth.user.id, MEMBER).await
}

/// Where an export is staged.
///
/// The importer's own scratch path, so an uploaded file and a fetched one are
/// the same thing to the worker.
fn scratch_path(import_id: Uuid) -> std::path::PathBuf {
    micaforge_jobs::run_import::scratch_path(import_id)
}

/// Remove a scratch file, best effort.
///
/// A file that outlives its import is untidy; failing a request over it would be
/// worse, and the operator has already been told what happened.
async fn discard(path: &std::path::Path) {
    if let Err(e) = tokio::fs::remove_file(path).await {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(file = %path.display(), error = %e, "could not remove the staged export");
        }
    }
}

// ---------------------------------------------------------------------------
// Validation and shaping
// ---------------------------------------------------------------------------

/// Validate the source tool.
fn check_source(raw: Option<&str>) -> Result<&'static str> {
    let source = raw.map(str::trim).unwrap_or_default().to_lowercase();
    SOURCES
        .iter()
        .copied()
        .find(|s| *s == source)
        .ok_or_else(|| {
            Error::validation("source", format!("must be one of {}", SOURCES.join(", ")))
        })
}

/// Refuse a file the staging reader could not read.
///
/// All three tools export CSV. A `.json` or a `.zip` here would be accepted,
/// queued, and fail inside a worker with a parse error, so it is named now.
fn check_extension(filename: Option<&str>) -> Result<()> {
    let Some(name) = filename.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(());
    };
    let lower = name.to_lowercase();
    if lower.ends_with(".csv") {
        return Ok(());
    }
    Err(Error::validation(
        "file",
        format!("`{name}` is not a CSV; export the report as CSV and upload that"),
    ))
}

/// One import on the wire.
fn import_json(import: &imports::Import) -> Value {
    json!({
        "id": import.id,
        "site_id": import.site_id,
        "source": import.source,
        "status": import.status,
        "progress": import.progress,
        "error": import.error,
        "created_at": import.created_at,
        "finished": import.status == "done" || import.status == "failed",
    })
}

/// Assert the caller may act on an import belonging to `site_id`.
async fn reachable(st: &AppState, site_id: Uuid, user_id: Uuid, needed: &str) -> Result<()> {
    match authorize_site(&st.pg, site_id, user_id, needed).await {
        Ok(_) => Ok(()),
        Err(Error::NotFound(_)) => Err(Error::not_found("import")),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_named_tools_are_accepted() {
        for source in SOURCES {
            assert_eq!(check_source(Some(source)).unwrap(), *source);
        }
        assert_eq!(check_source(Some("  GA4 ")).unwrap(), "ga4");
        assert_eq!(check_source(None).unwrap_err().field(), Some("source"));
        assert!(check_source(Some("matomo")).is_err());
    }

    #[test]
    fn a_non_csv_upload_is_refused_before_it_is_queued() {
        assert!(check_extension(Some("report.csv")).is_ok());
        assert!(check_extension(Some("REPORT.CSV")).is_ok());
        // Named now rather than twenty minutes later in a worker.
        assert!(check_extension(Some("export.json")).is_err());
        assert!(check_extension(Some("ga4.zip")).is_err());
        assert!(check_extension(Some("events.csv.gz")).is_err());
        // No filename at all is the `url` path, which has no file to check.
        assert!(check_extension(None).is_ok());
        assert!(check_extension(Some("  ")).is_ok());
    }

    #[test]
    fn the_scratch_path_is_the_one_the_worker_already_uses() {
        let id = new_id();
        let path = scratch_path(id);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();

        assert_eq!(name, format!("micaforge-import-{id}.csv"));
        assert!(path.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn the_upload_ceiling_matches_what_the_importer_will_download() {
        // Both are two gigabytes. A larger upload than the importer can fetch
        // would be an inconsistency an operator would trip over exactly once.
        assert_eq!(MAX_UPLOAD_BYTES, 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn the_route_limit_sits_just_above_the_file_ceiling() {
        // Above it, so a file at the ceiling is refused by this module with a
        // sentence rather than by the transport with a bare 413; and only just,
        // so a body nobody could accept is never read to the end.
        let limit = BODY_LIMIT as u64;
        assert!(limit > MAX_UPLOAD_BYTES);
        assert!(limit - MAX_UPLOAD_BYTES <= 1024 * 1024);
    }

    /// A multipart body with one text field called `name`.
    async fn one_field(name: &str, value: &str) -> Multipart {
        use axum::extract::FromRequest;

        let body = format!(
            "--XBOUNDARY\r\ncontent-disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n--XBOUNDARY--\r\n"
        );
        let request = axum::http::Request::builder()
            .method("POST")
            .header("content-type", "multipart/form-data; boundary=XBOUNDARY")
            .body(axum::body::Body::from(body))
            .unwrap();
        Multipart::from_request(request, &()).await.unwrap()
    }

    #[tokio::test]
    async fn a_text_field_is_read_whole_under_its_cap() {
        let mut multipart = one_field("source", "plausible").await;
        let mut field = multipart.next_field().await.unwrap().unwrap();
        assert_eq!(
            read_text_field(&mut field, "source").await.unwrap(),
            "plausible"
        );

        let exact = "s".repeat(MAX_FIELD_BYTES);
        let mut multipart = one_field("site", &exact).await;
        let mut field = multipart.next_field().await.unwrap().unwrap();
        assert_eq!(read_text_field(&mut field, "site").await.unwrap(), exact);
    }

    #[tokio::test]
    async fn a_text_field_past_its_cap_is_refused_not_buffered() {
        let long = "a".repeat(MAX_FIELD_BYTES + 1);
        let mut multipart = one_field("csrf", &long).await;
        let mut field = multipart.next_field().await.unwrap().unwrap();
        let e = read_text_field(&mut field, "csrf").await.unwrap_err();
        assert_eq!(e.field(), Some("csrf"));
        assert_eq!(e.status(), 422);
    }
}
