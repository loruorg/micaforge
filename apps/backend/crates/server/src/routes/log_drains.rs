//! `/api/sites/{site}/log-drains/*`: creating and managing a site's log drains.
//!
//! A log drain is the address a hosting platform posts its request log to, so
//! that AI crawler fetches are recorded with no change to the site's code. The
//! intake itself, `POST /api/log/vercel/{drain}`, is a collection route and
//! lives in [`crate::routes::log_drain`]; this module is the management half,
//! in the API group, behind the dashboard's CORS allow-list and the API-key
//! scope layer.
//!
//! ## The secret
//!
//! Each drain has its own signing secret, minted here from the OS CSPRNG,
//! never chosen by the operator. Vercel lets the operator paste their own
//! secret over the one it generates, which is what makes a server-minted one
//! possible, and a minted one is always long enough and never reused from
//! somewhere else. It is a credential, so `docs/CONTRACT.md` §2 requires it
//! sealed at rest: XChaCha20-Poly1305 keyed off `MICAFORGE_SECRET_KEY_BASE` for
//! the `log-drain` purpose, with the drain id as additional authenticated data,
//! the scheme `routes::sites` uses for the ingest key. The plaintext is in
//! exactly two answers, the one that creates the drain and the one that
//! rotates its secret, and in no list, no read and no log line.
//!
//! ## Who may do this
//!
//! `admin` on the site, the role that may read the ingest key, because a drain
//! secret is the same kind of credential: whoever holds it can write crawler
//! rows into the site. An API key never reaches `admin` (`docs/CONTRACT.md`
//! §3.5), so no key can mint or rotate one.

use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use micaforge_core::{crypto, new_id, Config, Error, Result};
use micaforge_store::models::log_drains::{self as drains, LogDrain, PROVIDERS};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::orgs::ADMIN;
use crate::routes::sites::authorize_site_ref;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Key derivation purpose for drain secrets. Changing it invalidates every
/// drain's secret at once.
const SECRET_PURPOSE: &str = "log-drain";

/// Drain secrets are prefixed so a leaked one is recognisable in a log or a
/// paste, like the `mfi_` ingest key.
const SECRET_PREFIX: &str = "mfd_";

/// Entropy in a drain secret.
const SECRET_BYTES: usize = 32;

/// Most drains one site may have. One per Vercel team or project is the
/// normal shape; the ceiling keeps a script stuck in a loop from minting
/// thousands of credentials.
const MAX_DRAINS_PER_SITE: i64 = 10;

/// Longest accepted ownership token. Vercel's are far shorter.
const MAX_VERIFY_TOKEN: usize = 256;

/// The header a drain's deliveries are signed with, named in the create
/// answer so the screen can say where the secret goes.
const SIGNATURE_HEADER: &str = "x-vercel-signature";

/// Every log drain management route. Paths are absolute from the app root, and
/// `{site}` is spelled as `routes::sites` spells it, because two names for one
/// segment position is a router panic.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/sites/{site}/log-drains",
            get(list_drains).post(create_drain),
        )
        .route(
            "/api/sites/{site}/log-drains/{drain}",
            patch(update_drain).delete(delete_drain),
        )
        .route(
            "/api/sites/{site}/log-drains/{drain}/rotate",
            post(rotate_drain),
        )
}

// ---------------------------------------------------------------------------
// The secret
// ---------------------------------------------------------------------------

/// The sealing key for drain secrets on this install.
fn sealing_key(cfg: &Config) -> [u8; 32] {
    cfg.derive_key(SECRET_PURPOSE)
}

/// A fresh secret for drain `id`: the plaintext, and the sealed form to store.
fn mint_secret(key: &[u8; 32], id: Uuid) -> Result<(String, String)> {
    let secret = format!("{SECRET_PREFIX}{}", crypto::random_token(SECRET_BYTES));
    let sealed = crypto::seal(key, id.as_bytes(), secret.as_bytes())?;
    Ok((secret, URL_SAFE_NO_PAD.encode(sealed)))
}

/// Open a stored secret with the install's sealing key.
fn open_secret(key: &[u8; 32], drain: &LogDrain) -> Result<String> {
    let sealed = URL_SAFE_NO_PAD
        .decode(&drain.secret_sealed)
        .map_err(|_| Error::internal("the stored drain secret is not valid base64"))?;
    let plaintext = crypto::open(key, drain.id.as_bytes(), &sealed)?;
    String::from_utf8(plaintext)
        .map_err(|_| Error::internal("the stored drain secret is not valid utf-8"))
}

/// The drain's plaintext secret.
///
/// Public to the crate because the collection route verifies against the same
/// value this module showed the operator, and one place should know the stored
/// form. Only ever called there, behind nothing but the signature it checks.
pub(crate) fn drain_secret(cfg: &Config, drain: &LogDrain) -> Result<String> {
    open_secret(&sealing_key(cfg), drain)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/sites/{site}/log-drains` body.
#[derive(Debug, Deserialize)]
struct NewDrain {
    /// One of [`PROVIDERS`]; `vercel` when absent.
    #[serde(default)]
    provider: Option<String>,
    /// The URL ownership token, when the platform asked for one.
    #[serde(default)]
    verify_token: Option<String>,
}

/// `PATCH /api/sites/{site}/log-drains/{drain}` body. The token is the only
/// thing a drain has to change; `null`, empty or absent clears it.
#[derive(Debug, Deserialize)]
struct DrainPatch {
    #[serde(default)]
    verify_token: Option<String>,
}

/// `GET /api/sites/{site}/log-drains` — every drain of the site, oldest
/// first, without their secrets.
async fn list_drains(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let rows = drains::list_for_site(&st.pg, site.id).await?;
    let base = &st.cfg.public_url;
    Ok(ok(rows
        .iter()
        .map(|d| drain_json(d, base))
        .collect::<Vec<_>>()))
}

/// `POST /api/sites/{site}/log-drains` — create a drain, and show its secret
/// for the only time it is ever shown.
async fn create_drain(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(site_ref): Path<String>,
    Json(body): Json<NewDrain>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let provider = clean_provider(body.provider.as_deref())?;
    let verify_token = clean_verify_token(body.verify_token.as_deref())?;

    if drains::count_for_site(&st.pg, site.id).await? >= MAX_DRAINS_PER_SITE {
        return Err(Error::Conflict(format!(
            "this site already has {MAX_DRAINS_PER_SITE} log drains; delete one before adding another"
        ))
        .into());
    }

    let id = new_id();
    let (secret, sealed) = mint_secret(&sealing_key(&st.cfg), id)?;
    let drain = drains::create(
        &st.pg,
        id,
        site.id,
        provider,
        &sealed,
        verify_token.as_deref(),
        Some(auth.user.id),
    )
    .await?;

    tracing::info!(site = %site.id, drain = %drain.id, actor = %auth.user.id, "log drain created");
    Ok(ok(with_secret(&drain, &st.cfg.public_url, &secret)))
}

/// `POST /api/sites/{site}/log-drains/{drain}/rotate` — replace the secret now.
///
/// Immediate and total, like the ingest key: the previous secret stops
/// verifying on the next delivery, because the intake reads the row every
/// time. A grace period would keep a leaked secret working after the operator
/// revoked it.
async fn rotate_drain(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((site_ref, drain_id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let current = drains::get_for_site(&st.pg, site.id, drain_id).await?;
    let (secret, sealed) = mint_secret(&sealing_key(&st.cfg), current.id)?;
    let drain = drains::set_secret(&st.pg, site.id, current.id, &sealed).await?;

    tracing::info!(site = %site.id, drain = %drain.id, actor = %auth.user.id, "log drain secret rotated");
    let mut answer = with_secret(&drain, &st.cfg.public_url, &secret);
    answer["rotated_at"] = json!(Utc::now());
    Ok(ok(answer))
}

/// `PATCH /api/sites/{site}/log-drains/{drain}` — set or clear the ownership
/// token.
async fn update_drain(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((site_ref, drain_id)): Path<(String, Uuid)>,
    Json(body): Json<DrainPatch>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    let verify_token = clean_verify_token(body.verify_token.as_deref())?;
    let drain =
        drains::set_verify_token(&st.pg, site.id, drain_id, verify_token.as_deref()).await?;
    Ok(ok(drain_json(&drain, &st.cfg.public_url)))
}

/// `DELETE /api/sites/{site}/log-drains/{drain}` — stop accepting deliveries.
///
/// Nothing already recorded is touched: the rows a drain wrote are the site's
/// crawler history, not the drain's.
async fn delete_drain(
    State(st): State<AppState>,
    auth: AuthUser,
    Path((site_ref, drain_id)): Path<(String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let site = authorize_site_ref(&st, &site_ref, auth.user.id, ADMIN).await?;
    drains::delete(&st.pg, site.id, drain_id).await?;
    tracing::info!(site = %site.id, drain = %drain_id, actor = %auth.user.id, "log drain deleted");
    Ok(ok(json!({ "deleted": drain_id })))
}

// ---------------------------------------------------------------------------
// Shapes and checks
// ---------------------------------------------------------------------------

/// The address a drain's deliveries go to.
fn drain_url(public_url: &str, drain: &LogDrain) -> String {
    format!(
        "{}/api/log/{}/{}",
        public_url.trim_end_matches('/'),
        drain.provider,
        drain.id
    )
}

/// One drain on the wire. Never carries the secret, sealed or not.
fn drain_json(drain: &LogDrain, public_url: &str) -> Value {
    json!({
        "id": drain.id,
        "provider": drain.provider,
        "url": drain_url(public_url, drain),
        "verify_token": drain.verify_token,
        "created_by": drain.created_by,
        "created_at": drain.created_at,
        "last_received_at": drain.last_received_at,
    })
}

/// A drain with its plaintext secret: the create and rotate answers only.
fn with_secret(drain: &LogDrain, public_url: &str, secret: &str) -> Value {
    let mut out = drain_json(drain, public_url);
    out["secret"] = json!(secret);
    out["signature_header"] = json!(SIGNATURE_HEADER);
    out["note"] = json!("this is the only time the secret is shown; rotating issues a new one");
    out
}

/// The provider, defaulting to Vercel, from the supported list only.
fn clean_provider(raw: Option<&str>) -> Result<&'static str> {
    let wanted = raw
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("vercel");
    PROVIDERS
        .iter()
        .copied()
        .find(|p| p.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| {
            Error::validation(
                "provider",
                format!("must be one of: {}", PROVIDERS.join(", ")),
            )
        })
}

/// The ownership token, trimmed, or `None` for an empty one.
///
/// It is written into a response header, so only visible ASCII is accepted: a
/// space or a control character there would be a malformed header at best and
/// a header injection at worst.
fn clean_verify_token(raw: Option<&str>) -> Result<Option<String>> {
    let Some(token) = raw.map(str::trim).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if token.len() > MAX_VERIFY_TOKEN {
        return Err(Error::validation(
            "verify_token",
            format!("must be at most {MAX_VERIFY_TOKEN} characters"),
        ));
    }
    if !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::validation(
            "verify_token",
            "must be the token exactly as Vercel shows it, with no spaces",
        ));
    }
    Ok(Some(token.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public URL with the trailing slash an operator might leave on it.
    const PUBLIC_URL: &str = "https://mf.example.com/";

    /// The sealing key of an install with this `MICAFORGE_SECRET_KEY_BASE`.
    /// Derived directly, so the tests need no whole `Config`.
    fn install_key(base: &str) -> [u8; 32] {
        crypto::derive_key(base, SECRET_PURPOSE)
    }

    fn drain(id: Uuid, sealed: String) -> LogDrain {
        LogDrain {
            id,
            site_id: Uuid::now_v7(),
            provider: "vercel".into(),
            secret_sealed: sealed,
            verify_token: None,
            created_by: None,
            created_at: Utc::now(),
            last_received_at: None,
        }
    }

    #[test]
    fn a_minted_secret_opens_only_in_its_own_row() {
        let key = install_key(&"x".repeat(48));
        let id = Uuid::now_v7();
        let (secret, sealed) = mint_secret(&key, id).unwrap();
        assert!(secret.starts_with(SECRET_PREFIX));
        assert!(secret.len() >= 40, "32 bytes of entropy");
        assert!(!sealed.contains(&secret), "stored sealed, not in the clear");
        assert_eq!(
            open_secret(&key, &drain(id, sealed.clone())).unwrap(),
            secret
        );

        // The same blob in another drain's row does not open.
        assert!(open_secret(&key, &drain(Uuid::now_v7(), sealed.clone())).is_err());
        // Nor under another install's key.
        let other = install_key(&"y".repeat(48));
        assert!(open_secret(&other, &drain(id, sealed)).is_err());

        // Two mints never agree.
        let (again, _) = mint_secret(&key, id).unwrap();
        assert_ne!(again, secret);
    }

    #[test]
    fn a_drain_never_serialises_its_secret() {
        let id = Uuid::now_v7();
        let (secret, sealed) = mint_secret(&install_key(&"x".repeat(48)), id).unwrap();
        let row = drain(id, sealed.clone());

        let listed = drain_json(&row, PUBLIC_URL).to_string();
        assert!(!listed.contains(&secret));
        assert!(!listed.contains(&sealed));
        assert!(listed.contains(&format!("https://mf.example.com/api/log/vercel/{id}")));

        let shown = with_secret(&row, PUBLIC_URL, &secret);
        assert_eq!(shown["secret"], json!(secret));
        assert!(!shown.to_string().contains(&sealed));
    }

    #[test]
    fn the_provider_comes_from_the_list() {
        assert_eq!(clean_provider(None).unwrap(), "vercel");
        assert_eq!(clean_provider(Some("  ")).unwrap(), "vercel");
        assert_eq!(clean_provider(Some("Vercel")).unwrap(), "vercel");
        assert!(clean_provider(Some("netlify")).is_err());
    }

    #[test]
    fn an_ownership_token_must_be_a_safe_header_value() {
        assert_eq!(clean_verify_token(None).unwrap(), None);
        assert_eq!(clean_verify_token(Some("  ")).unwrap(), None);
        assert_eq!(
            clean_verify_token(Some(" 3f9a1c0d ")).unwrap().as_deref(),
            Some("3f9a1c0d")
        );
        for bad in ["two words", "line\r\nx-evil: 1", "tab\there", "é"] {
            assert!(clean_verify_token(Some(bad)).is_err(), "{bad:?}");
        }
        assert!(clean_verify_token(Some(&"a".repeat(MAX_VERIFY_TOKEN + 1))).is_err());
    }
}
