//! `/api/keys/*` — bearer credentials for the API.
//!
//! A key is shown to its owner **once**, in the response that creates it. What
//! is stored is a SHA-256 digest and a short public prefix, so a Postgres dump
//! yields no working credential and a lost key is replaced rather than
//! recovered. `docs/CONTRACT.md` §3 fixes the token shape as `mfg_…`.
//!
//! ## The prefix is not a secret
//!
//! Authenticating a presented token by hashing it against every row would cost a
//! full table scan and an Argon2-shaped amount of work per request. Instead the
//! first [`PREFIX_LEN`] characters are stored in the clear and indexed: a
//! presented token is looked up by prefix, and the *digest* comparison against
//! the few candidates is what actually authenticates. The prefix is also what
//! the settings screen shows, so one key can be told from its siblings without
//! anybody having to keep the secret around to recognise it.
//!
//! ## Scopes
//!
//! [`SCOPES`] is the vocabulary. It is deliberately small and coarse — four
//! values, not one per endpoint — because a scope list nobody can hold in their
//! head is a scope list everybody sets to "all".
//!
//! ## What a key can reach
//!
//! `api_keys` has no `created_by` column in `docs/CONTRACT.md` §2, so a key
//! resolves against the organisation's owner rather than one person. It never
//! carries the owner's authority, though: `crate::extract::KeyGrant` holds it to
//! its organisation, its site when it has one, and its scopes, and caps it at
//! `member` (`manage:write`) or `viewer` (anything less). Creation and
//! revocation require `admin`, and every route in this module refuses an API
//! key outright: a key that could mint or widen keys could mint its successor.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::{crypto, new_id, Error, Result};
use micaforge_store::models::{keys, sites};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::orgs::{authorize_org, clean_name, ADMIN};
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Every token starts with this, so a leaked one is recognisable on sight.
const TOKEN_PREFIX: &str = "mfg_";

/// Entropy in the secret half of a token.
const TOKEN_BYTES: usize = 32;

/// How much of the token is stored in the clear and used as the lookup key.
///
/// Sixteen characters: enough that a prefix collision is rare, short enough that
/// the visible part is not itself most of the secret.
const PREFIX_LEN: usize = 16;

/// The scope vocabulary, in the order the settings screen lists them.
pub const SCOPES: &[&str] = &[
    // The analytics read endpoints of `docs/CONTRACT.md` §3.3.
    "stats:read",
    // Management `GET`s: sites, goals, funnels, alerts, flags.
    "manage:read",
    // Management writes. Everything a `member` could do in the dashboard.
    "manage:write",
    // `POST /api/log/edge`, for a log shipper or a reverse-proxy module.
    "ingest:write",
];

/// What a key gets when the caller does not say.
///
/// Read-only analytics: the overwhelmingly common reason to mint a key is to
/// pull numbers into somebody else's dashboard, and a default that could also
/// delete a site would be the wrong default.
const DEFAULT_SCOPES: &[&str] = &["stats:read"];

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every API-key route. Paths are absolute from the app root.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/keys", get(list_keys).post(create_key))
        .route(
            "/api/keys/{id}",
            get(get_key).patch(update_key).delete(revoke_key),
        )
        .route("/api/keys/{id}/usage", get(key_usage))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// Which organisation's keys to list.
#[derive(Debug, Deserialize)]
struct ListQuery {
    org: Uuid,
}

/// `GET /api/keys?org=…` — every key in an organisation.
///
/// Requires `admin`. The rows carry no secret, but the *shape* of an
/// organisation's credentials — how many there are, what each may reach, which
/// have gone unused — is operational detail a `viewer` has no claim to.
async fn list_keys(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, q.org, auth.user.id, ADMIN).await?;
    let rows = keys::list_for_org(&st.pg, org.id).await?;

    let mut out = Vec::with_capacity(rows.len());
    for key in rows {
        out.push(key_json(&st, &key).await?);
    }
    Ok(ok(out))
}

/// What a new key may specify.
#[derive(Debug, Deserialize)]
struct NewKey {
    org_id: Uuid,
    name: String,
    /// Narrow the key to one site. Absent means the whole organisation.
    #[serde(default)]
    site_id: Option<Uuid>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

/// `POST /api/keys` — mint one. **The only response that carries the token.**
async fn create_key(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<NewKey>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let (org, _) = authorize_org(&st.pg, body.org_id, auth.user.id, ADMIN).await?;
    let name = clean_name(&body.name, "name")?;
    let scopes = clean_scopes(body.scopes.as_deref())?;

    // A site-scoped key must name a site inside this organisation, and the
    // caller must be able to administer that site. Otherwise a key would be a
    // way to reach across a per-site restriction that `site_access` imposed.
    if let Some(site_id) = body.site_id {
        let site =
            crate::routes::sites::authorize_site(&st.pg, site_id, auth.user.id, ADMIN).await?;
        if site.org_id != org.id {
            return Err(Error::validation("site_id", "belongs to another organisation").into());
        }
    }

    if let Some(expiry) = body.expires_at {
        if expiry <= Utc::now() {
            return Err(Error::validation("expires_at", "is already in the past").into());
        }
    }

    let token = format!("{TOKEN_PREFIX}{}", crypto::random_token(TOKEN_BYTES));
    let prefix: String = token.chars().take(PREFIX_LEN).collect();

    let key = keys::create(
        &st.pg,
        new_id(),
        org.id,
        body.site_id,
        &name,
        &prefix,
        &crypto::token_hash(&token),
        &json!(scopes),
        body.expires_at,
    )
    .await?;

    tracing::info!(key = %key.id, org = %org.id, actor = %auth.user.id, "api key created");

    let mut payload = key_json(&st, &key).await?;
    payload["token"] = json!(token);
    payload["token_shown_once"] = json!(true);
    Ok(ok(payload))
}

/// Load a key and assert the caller may administer its organisation.
///
/// Refuses an API key before anything is read: no key manages keys.
async fn authorize_key(st: &AppState, id: Uuid, auth: &AuthUser) -> Result<keys::ApiKey> {
    auth.require_session()?;
    let key = keys::get(&st.pg, id).await?;
    // Reported as a missing key rather than a forbidden one: an id that
    // answered `403` would confirm the key exists.
    match authorize_org(&st.pg, key.org_id, auth.user.id, ADMIN).await {
        Ok(_) => Ok(key),
        Err(Error::NotFound(_)) => Err(Error::not_found("api key")),
        Err(e) => Err(e),
    }
}

/// `GET /api/keys/{id}` — one key, without its secret.
async fn get_key(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = authorize_key(&st, id, &auth).await?;
    Ok(ok(key_json(&st, &key).await?))
}

/// What a key update may change.
///
/// Neither the token nor the site binding is editable: a key that could be
/// re-pointed at another site would make its scope meaningless, and a token that
/// could be re-read would defeat storing only the digest.
#[derive(Debug, Deserialize)]
struct KeyPatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

/// `PATCH /api/keys/{id}` — rename it, or change what it may reach.
async fn update_key(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<KeyPatch>,
) -> ApiResult<Json<Value>> {
    let key = authorize_key(&st, id, &auth).await?;

    let name = match body.name.as_deref() {
        Some(raw) => clean_name(raw, "name")?,
        None => key.name.clone(),
    };
    let scopes = match body.scopes.as_deref() {
        Some(list) => json!(clean_scopes(Some(list))?),
        None => key.scopes.clone(),
    };

    let updated = keys::update(&st.pg, key.id, &name, &scopes).await?;
    Ok(ok(key_json(&st, &updated).await?))
}

/// `DELETE /api/keys/{id}` — revoke it. Immediate and total.
async fn revoke_key(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = authorize_key(&st, id, &auth).await?;
    keys::delete(&st.pg, key.id).await?;

    tracing::info!(key = %key.id, actor = %auth.user.id, "api key revoked");
    Ok(ok(json!({ "revoked": key.id, "prefix": key.prefix })))
}

/// `GET /api/keys/{id}/usage` — is this key still in use?
///
/// There is no per-key request log in `docs/CONTRACT.md` §2, so this reports
/// what the row itself knows: when it was minted, when it last authenticated a
/// request, and whether it is still live. That is enough to answer the question
/// the screen exists for — which of these can I safely delete?
async fn key_usage(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let key = authorize_key(&st, id, &auth).await?;
    let now = Utc::now();
    let created = created_at(key.id);

    let idle_days = key
        .last_used_at
        .map(|used| (now - used).num_days())
        .unwrap_or_else(|| created.map(|c| (now - c).num_days()).unwrap_or(0));

    Ok(ok(json!({
        "id": key.id,
        "prefix": key.prefix,
        "created_at": created,
        "last_used_at": key.last_used_at,
        "ever_used": key.last_used_at.is_some(),
        "idle_days": idle_days,
        "expires_at": key.expires_at,
        "live": key.is_live(now),
        "scopes": key.scopes,
    })))
}

// ---------------------------------------------------------------------------
// Shaping and validation
// ---------------------------------------------------------------------------

/// One key on the wire. Never carries the digest, and never the token.
///
/// The site binding is resolved to its numeric id, because that is what a caller
/// putting the key in a script will actually type into `?site=`.
async fn key_json(st: &AppState, key: &keys::ApiKey) -> Result<Value> {
    let numeric = match key.site_id {
        Some(site_id) => sites::get(&st.pg, site_id)
            .await
            .ok()
            .map(|s| s.site_id_u16()),
        None => None,
    };

    Ok(json!({
        "id": key.id,
        "org_id": key.org_id,
        "site_id": key.site_id,
        "site": numeric,
        "name": key.name,
        "prefix": key.prefix,
        "scopes": key.scopes,
        "created_at": created_at(key.id),
        "last_used_at": key.last_used_at,
        "expires_at": key.expires_at,
        "live": key.is_live(Utc::now()),
    }))
}

/// When a row was created, read out of its own UUIDv7.
///
/// `api_keys` has no `created_at` column, and it does not need one: a v7 key
/// carries a millisecond timestamp in its high bits, which is exactly the value
/// such a column would hold.
fn created_at(id: Uuid) -> Option<DateTime<Utc>> {
    let (secs, nanos) = id.get_timestamp()?.to_unix();
    DateTime::from_timestamp(secs as i64, nanos)
}

/// Validate a scope list against [`SCOPES`], or supply the default.
///
/// An unknown scope is refused rather than dropped. Silently discarding it would
/// mint a key the caller believes is more capable than it is, and they would
/// find out at the worst moment.
fn clean_scopes(requested: Option<&[String]>) -> Result<Vec<String>> {
    let Some(list) = requested else {
        return Ok(DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect());
    };
    if list.is_empty() {
        return Err(Error::validation(
            "scopes",
            "a key with no scopes could not do anything; omit the field for the default",
        ));
    }

    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for raw in list {
        let scope = raw.trim().to_lowercase();
        if !SCOPES.contains(&scope.as_str()) {
            return Err(Error::validation(
                "scopes",
                format!(
                    "`{scope}` is not a scope; expected one of {}",
                    SCOPES.join(", ")
                ),
            ));
        }
        if !out.contains(&scope) {
            out.push(scope);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_read_only() {
        let scopes = clean_scopes(None).unwrap();
        assert_eq!(scopes, vec!["stats:read".to_string()]);
        assert!(!scopes.iter().any(|s| s.ends_with(":write")));
    }

    #[test]
    fn an_unknown_scope_is_refused_rather_than_dropped() {
        let err = clean_scopes(Some(&["stats:read".into(), "root".into()])).unwrap_err();
        assert_eq!(err.field(), Some("scopes"));
    }

    #[test]
    fn scopes_are_normalised_and_deduplicated() {
        let scopes = clean_scopes(Some(&[
            " Stats:Read ".into(),
            "stats:read".into(),
            "manage:read".into(),
        ]))
        .unwrap();
        assert_eq!(
            scopes,
            vec!["stats:read".to_string(), "manage:read".to_string()]
        );
    }

    #[test]
    fn an_empty_list_is_a_mistake_not_a_default() {
        assert!(clean_scopes(Some(&[])).is_err());
    }

    #[test]
    fn a_v7_key_carries_its_own_creation_time() {
        let id = new_id();
        let created = created_at(id).expect("a v7 uuid has a timestamp");
        let drift = (Utc::now() - created).num_seconds().abs();
        assert!(
            drift < 5,
            "the embedded timestamp should be now, was {drift}s out"
        );
        // A v4 id has no timestamp, and the column is honest about that.
        assert!(created_at(Uuid::from_u128(0)).is_none());
    }

    #[test]
    fn the_prefix_leaves_the_secret_half_unguessable() {
        let token = format!("{TOKEN_PREFIX}{}", crypto::random_token(TOKEN_BYTES));
        let prefix: String = token.chars().take(PREFIX_LEN).collect();

        assert!(token.starts_with(TOKEN_PREFIX));
        assert_eq!(prefix.len(), PREFIX_LEN);

        // The prefix is stored in the clear and shown on screen, so what has to
        // hold is that the *rest* is still a secret. Each url-safe base64
        // character carries six bits.
        let hidden_bits = (token.len() - PREFIX_LEN) * 6;
        assert!(
            hidden_bits >= 128,
            "only {hidden_bits} bits stay hidden behind a {PREFIX_LEN}-character prefix"
        );
    }
}
