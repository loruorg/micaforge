//! `/api/auth/*` — registration, sign-in, recovery and two-factor.
//!
//! Three properties are load-bearing here, and each one exists because the
//! obvious implementation leaks something.
//!
//! **1. No endpoint reveals whether an address has an account.** `register` and
//! `forgot-password` answer identically for a known and an unknown address, and
//! both are padded to a fixed floor ([`AUTH_FLOOR_MS`]) so the *duration* does
//! not answer the question the body refuses to. Registration always runs an
//! Argon2 hash, even when the address is taken and the hash will be discarded,
//! because skipping it would make the taken case measurably faster.
//!
//! **2. Sign-in is limited twice, on two different keys.** A per-address
//! exponential backoff ([`backoff_delay`]) makes guessing one account's password
//! unworkable, and a per-address-hash key means an attacker rotating IPs gains
//! nothing. A separate per-IP window limits an attacker spraying one password
//! across many addresses. The backoff is applied *before* the account is looked
//! up, so a locked unknown address behaves exactly like a locked real one.
//! Every backoff state admits **one** guess, claimed with an atomic Redis
//! increment before Argon2 runs ([`gate_key`]), so a burst of parallel requests
//! gets no more guesses per lock window than one patient client. The per-IP key
//! is the address [`crate::client_ip`] attributes the request to, the same one
//! the global limiter uses, so a forged header buys nothing.
//!
//! **3. Nothing recoverable is stored.** Session cookies, verification links and
//! reset links are all random tokens whose SHA-256 digest is what is written
//! down. A dump of Postgres or Redis yields no usable credential.
//!
//! ## The session cookie
//!
//! `HttpOnly`, `SameSite=Lax`, `Path=/`, `Secure` whenever the public origin is
//! `https`, and a rolling thirty days: every authenticated read of `/api/auth/me`
//! pushes both the stored expiry and the browser's copy forward, so an active
//! operator is never signed out and an abandoned session still lapses.
//!
//! `SameSite=Lax` means the cookie is not sent on cross-site requests. When the
//! dashboard is served from a different origin than the API, the dashboard must
//! be proxied under the API's origin, or callers must use an API key instead.

use std::net::IpAddr;
use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::{Duration, Utc};
use micaforge_core::{crypto, new_id, Config, Error, Result};
use micaforge_store::models::{auth as sessions, orgs, sites, users};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr};
use crate::response::ok;
use crate::routes::orgs::{clean_email, clean_name, unique_slug, OWNER};
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The cookie the dashboard authenticates with.
pub const SESSION_COOKIE: &str = "mf_session";

/// How long a session lives, and how far each use pushes it forward.
const SESSION_DAYS: i64 = 30;

/// Entropy in a session token, a verification token and a reset token.
const TOKEN_BYTES: usize = 32;

/// How long a verification link works. A day is long enough to survive a night
/// and short enough that a link found in an old mailbox is dead.
const VERIFY_TTL_SECS: u32 = 24 * 60 * 60;

/// How long a password-reset link works.
///
/// One hour. A reset link is a live credential for the account, so it gets the
/// shortest window that still lets somebody read their mail and act on it.
const RESET_TTL_SECS: u32 = 60 * 60;

/// How long an unconfirmed TOTP enrolment stays claimable.
const TOTP_PENDING_TTL_SECS: u32 = 10 * 60;

/// The floor every account-existence-sensitive endpoint is padded to.
///
/// Chosen above a default Argon2id hash on modest hardware, so the padding
/// masks the difference rather than merely adding to it.
const AUTH_FLOOR_MS: u64 = 400;

/// Failed sign-ins that cost nothing, before the backoff starts.
///
/// Two, because a typo and a stale password manager entry are ordinary and
/// locking somebody out over them would make the product hostile.
const BACKOFF_FREE_ATTEMPTS: u32 = 2;

/// Longest an account is held off, in seconds.
///
/// Fifteen minutes. Long enough that online guessing is hopeless, short enough
/// that a real person who forgot their password is not locked out for the day.
const BACKOFF_MAX_SECS: i64 = 900;

/// How long a failure record survives with no further attempts.
const BACKOFF_TTL_SECS: u32 = 24 * 60 * 60;

/// How long a claimed guess slot stays claimed when the attempt that claimed it
/// never finished (a dropped connection, a store failure). The longest hold the
/// backoff itself can impose, so an abandoned attempt costs the account no more
/// than a failed one would.
const GATE_WINDOW_SECS: u32 = BACKOFF_MAX_SECS as u32;

/// The advisory lock that makes "is this the first account?" and creating it
/// one step. Any constant works; it only has to be the same in every process.
const FIRST_ACCOUNT_LOCK: i64 = 0x6d66_6669_7273_7401;

/// Sign-in attempts one address may make per window, whatever the outcome.
const LOGIN_IP_LIMIT: u32 = 40;
/// That window, in seconds.
const LOGIN_IP_WINDOW: u32 = 300;

/// Registrations one address may make per hour.
const REGISTER_IP_LIMIT: u32 = 5;
/// That window, in seconds.
const REGISTER_IP_WINDOW: u32 = 3600;

/// Shortest accepted password.
const MIN_PASSWORD: usize = 10;
/// Longest accepted password. A bound exists so a megabyte of text cannot be
/// used to make Argon2 the denial-of-service vector.
const MAX_PASSWORD: usize = 200;

/// Digits in a TOTP code. Six is what every authenticator app shows.
const TOTP_DIGITS: usize = 6;

/// Seconds one TOTP code is valid for.
const TOTP_STEP_SECONDS: u64 = 30;

/// Adjacent windows accepted either side of the current one.
const TOTP_SKEW: u8 = 1;

/// The issuer name an authenticator app shows beside the account.
const TOTP_ISSUER: &str = "Micaforge";

/// A real Argon2id digest, verified against when no account exists.
///
/// [`crypto::verify_password`] returns `false` immediately for a malformed
/// stored hash, which would make "no such account" the fast path and undo the
/// whole point. Verifying against a genuine digest costs exactly what a real
/// sign-in costs.
static ABSENT_ACCOUNT_HASH: Lazy<String> = Lazy::new(|| {
    crypto::hash_password("micaforge/no-such-account").unwrap_or_else(|_| String::new())
});

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every authentication route. Paths are absolute from the app root.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/verify-email", post(verify_email))
        .route("/api/auth/resend-verification", post(resend_verification))
        .route("/api/auth/forgot-password", post(forgot_password))
        .route("/api/auth/reset-password", post(reset_password))
        .route("/api/auth/password", post(change_password))
        .route("/api/auth/totp/enroll", post(totp_enroll))
        .route("/api/auth/totp/verify", post(totp_verify))
        .route("/api/auth/totp/disable", post(totp_disable))
        .route("/api/auth/me", get(me).patch(update_me))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// What a signup form sends.
#[derive(Debug, Deserialize)]
struct RegisterBody {
    email: String,
    password: String,
    #[serde(default)]
    name: Option<String>,
    /// What to call the organisation created alongside the account.
    #[serde(default)]
    org_name: Option<String>,
}

/// `POST /api/auth/register`.
///
/// Answers the same shape whether or not the address was already taken, and
/// takes the same time either way. The account holder learns which case it was
/// from their inbox, which is the only channel that proves they own the address.
async fn register(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    Json(body): Json<RegisterBody>,
) -> ApiResult<Json<Value>> {
    let started = Instant::now();

    // A closed install still admits exactly one account: the first. Otherwise
    // setting MICAFORGE_DISABLE_REGISTRATION before the owner signed up would
    // leave an install nobody can enter, while `/api/config` (which reports
    // `registration_open` by the same rule) offered a form.
    let maybe_first = users::count(&st.pg).await? == 0;
    if st.cfg.disable_registration && !maybe_first {
        return Err(registration_closed().into());
    }

    if !st
        .kv
        .rate_limit(
            &format!("auth-register:{}", ip_digest(&st.cfg, &ip)),
            REGISTER_IP_LIMIT,
            REGISTER_IP_WINDOW,
        )
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(REGISTER_IP_WINDOW),
        }
        .into());
    }

    let email = clean_email(&body.email)?;
    check_password(&body.password)?;

    // Always paid, even when the result is thrown away. See the module docs.
    let password_hash = hash_password_blocking(body.password.clone()).await?;

    // The first account owns the instance (`routes::admin`), so deciding
    // "first" and creating it are one step under an advisory lock: two people
    // racing to claim a fresh install cannot both see an empty table. Only the
    // fresh-install path pays for the lock.
    let first_account_lock = if maybe_first {
        Some(lock_first_account(&st.pg).await?)
    } else {
        None
    };
    let is_first_account = first_account_lock.is_some() && users::count(&st.pg).await? == 0;
    if st.cfg.disable_registration && !is_first_account {
        return Err(registration_closed().into());
    }

    let created = if users::find_by_email(&st.pg, &email).await?.is_some() {
        None
    } else {
        let name = match body.name.as_deref() {
            Some(raw) if !raw.trim().is_empty() => clean_name(raw, "name")?,
            _ => default_name(&email),
        };
        match users::create(&st.pg, new_id(), &email, &password_hash, &name).await {
            Ok(user) => {
                provision_first_organization(&st, &user, body.org_name.as_deref()).await?;
                Some(user)
            }
            // Two registrations for one address racing each other. The loser
            // behaves exactly like the "already taken" branch.
            Err(Error::Conflict(_)) => None,
            Err(e) => return Err(e.into()),
        }
    };

    if let Some(user) = created.as_ref() {
        if is_first_account {
            users::mark_email_verified(&st.pg, user.id).await?;
            tracing::info!(user = %user.id, "first account created; it owns this install");
        } else {
            send_verification(&st, user.id, &email).await?;
        }
    }

    // Released only once the first account exists, so whoever waited on the
    // lock counts it.
    if let Some(lock) = first_account_lock {
        lock.commit().await?;
    }

    settle(started).await;
    Ok(ok(json!({
        "status": "ok",
        "email": email,
        // True for every registration after the first, whether or not this call
        // created an account, so the value cannot be read as an answer about
        // the address.
        "verification_required": !is_first_account,
        "mail_delivered": st.mailer.is_sending(),
        "message": "if that address can hold an account, check it for the next step",
    })))
}

/// The refusal a closed install answers with.
fn registration_closed() -> Error {
    Error::Forbidden(
        "registration is closed on this install; unset MICAFORGE_DISABLE_REGISTRATION to open it"
            .into(),
    )
}

/// Take the first-account lock, held until the returned transaction ends.
///
/// A transaction-scoped advisory lock rather than a session one, so every way
/// out of `register` (an early `?`, a panic, a dropped connection) releases it
/// by rolling the transaction back. A session lock left on a pooled connection
/// would block every registration after it.
async fn lock_first_account(
    pg: &micaforge_store::pg::PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
    let mut tx = pg.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(FIRST_ACCOUNT_LOCK)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// Give a brand-new account an organisation to put sites in.
///
/// Every site belongs to an organisation, so an account with none could not do
/// anything at all. The name defaults to the person's own, which reads correctly
/// for the single-operator install this product is built around.
async fn provision_first_organization(
    st: &AppState,
    user: &users::User,
    requested: Option<&str>,
) -> Result<()> {
    let name = match requested {
        Some(raw) if !raw.trim().is_empty() => clean_name(raw, "org_name")?,
        _ => format!("{}'s workspace", user.name),
    };
    let slug = unique_slug(&st.pg, &name).await?;
    let org = orgs::create(&st.pg, new_id(), &name, &slug, user.id).await?;
    orgs::add_member(&st.pg, org.id, user.id, OWNER).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Sign in and out
// ---------------------------------------------------------------------------

/// What a sign-in form sends.
#[derive(Debug, Deserialize)]
struct LoginBody {
    email: String,
    password: String,
    /// Required when the account has two-factor enabled.
    #[serde(default)]
    totp: Option<String>,
}

/// `POST /api/auth/login`.
///
/// The ordering matters: rate limit, then backoff, then the guess slot, then
/// look the account up. Checking the backoff before the lookup is what stops the
/// lockout state itself from becoming an enumeration oracle, and claiming the
/// slot before Argon2 is what makes the backoff hold under a parallel burst.
async fn login(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    let started = Instant::now();
    let email = body.email.trim().to_lowercase();

    if !st
        .kv
        .rate_limit(
            &format!("auth-login:{}", ip_digest(&st.cfg, &ip)),
            LOGIN_IP_LIMIT,
            LOGIN_IP_WINDOW,
        )
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(LOGIN_IP_WINDOW),
        }
        .into());
    }

    let key = backoff_key(&email);
    let backoff: Backoff = st.kv.get_json(&key).await?.unwrap_or_default();
    let now = Utc::now().timestamp();
    if backoff.locked_until > now {
        return Err(Error::RateLimited {
            retry_after: (backoff.locked_until - now).max(1) as u64,
        }
        .into());
    }

    // One guess per backoff state. Every request that read this same state
    // races for one atomic increment; the losers are told to come back, and by
    // then the winner has moved the state on (and, on a failure, locked it).
    if !st
        .kv
        .rate_limit(&gate_key(&key, &backoff), 1, GATE_WINDOW_SECS)
        .await?
    {
        return Err(Error::RateLimited { retry_after: 2 }.into());
    }

    match login_attempt(&st, &email, &body).await {
        Ok(LoginOutcome::Refused(message)) => {
            record_failure(&st, &key, backoff).await?;
            settle(started).await;
            Err(Error::Unauthorized(message.into()).into())
        }
        Ok(LoginOutcome::TotpRequired) => {
            // The password was right, so this is not a failure; but the slot
            // is spent, and the follow-up carrying the code needs a fresh one.
            advance(&st, &key, &backoff).await?;
            settle(started).await;
            Ok((jar, ok(json!({ "status": "totp_required" }))))
        }
        Ok(LoginOutcome::Accepted(user)) => {
            reset_backoff(&st, &key).await?;
            let (jar, session) = open_session(&st, jar, &user, &headers, &ip).await?;

            tracing::info!(user = %user.id, "signed in");
            Ok((
                jar,
                ok(json!({
                    "status": "ok",
                    "user": user_json(&user, false),
                    "expires_at": session.expires_at,
                })),
            ))
        }
        Err(e) => {
            // A store failure mid-attempt must not leave the slot spent and
            // the account held for the gate window. Best effort: if Redis is
            // what failed, there is nothing to write to anyway.
            let _ = advance(&st, &key, &backoff).await;
            Err(e.into())
        }
    }
}

/// How one gated sign-in attempt ended.
enum LoginOutcome {
    /// The password (or the second factor) was wrong. Carries the sentence.
    Refused(&'static str),
    /// The password was right and the account needs a code it was not sent.
    TotpRequired,
    /// Signed in.
    Accepted(users::User),
}

/// The part of a sign-in that runs after its guess slot is claimed.
async fn login_attempt(st: &AppState, email: &str, body: &LoginBody) -> Result<LoginOutcome> {
    let account = users::find_by_email(&st.pg, email).await?;
    let stored_hash = account.as_ref().map(|u| u.password_hash.clone());
    let password_ok = verify_account_password(body.password.clone(), stored_hash).await?;

    let Some(user) = account.filter(|_| password_ok) else {
        return Ok(LoginOutcome::Refused(
            "that email and password do not match",
        ));
    };

    // Two-factor is checked after the password, and a wrong code counts as a
    // failure on the same curve: otherwise the second factor would be an
    // unlimited guessing surface once the first one was known.
    if let Some(sealed) = user.totp_secret_sealed.as_deref() {
        let supplied = body.totp.as_deref().unwrap_or("").trim().to_string();
        if supplied.is_empty() {
            return Ok(LoginOutcome::TotpRequired);
        }
        if !verify_totp_secret(&st.cfg, &user, sealed, &supplied)? {
            return Ok(LoginOutcome::Refused("that code is not right"));
        }
    }

    Ok(LoginOutcome::Accepted(user))
}

/// `POST /api/auth/logout`.
///
/// Deliberately does not require an authenticated caller. Somebody holding an
/// expired or unrecognised cookie is asking to be signed out, and answering that
/// with a `401` would leave the dead cookie in their browser.
async fn logout(State(st): State<AppState>, jar: CookieJar) -> ApiResult<(CookieJar, Json<Value>)> {
    if let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) {
        sessions::delete_by_token_hash(&st.pg, &crypto::token_hash(&token)).await?;
    }
    let cleared = jar.add(cleared_cookie(&st.cfg)?);
    Ok((cleared, ok(json!({ "status": "signed_out" }))))
}

// ---------------------------------------------------------------------------
// Address verification
// ---------------------------------------------------------------------------

/// A token from an emailed link.
#[derive(Debug, Deserialize)]
struct TokenBody {
    token: String,
}

/// `POST /api/auth/verify-email`.
async fn verify_email(
    State(st): State<AppState>,
    Json(body): Json<TokenBody>,
) -> ApiResult<Json<Value>> {
    let key = verify_key(&crypto::token_hash(body.token.trim()));
    let user_id: Option<Uuid> = st.kv.get_json(&key).await?;
    let Some(user_id) = user_id else {
        return Err(Error::not_found("verification link").into());
    };

    users::mark_email_verified(&st.pg, user_id).await?;
    st.kv.del(&key).await?;

    Ok(ok(json!({ "status": "verified", "user_id": user_id })))
}

/// `POST /api/auth/resend-verification` — send the link again.
async fn resend_verification(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    if auth.user.email_verified_at.is_some() {
        return Ok(ok(json!({ "status": "already_verified" })));
    }
    send_verification(&st, auth.user.id, &auth.user.email).await?;
    Ok(ok(
        json!({ "status": "sent", "mail_delivered": st.mailer.is_sending() }),
    ))
}

/// Mint a verification token, store its digest, and mail the link.
async fn send_verification(st: &AppState, user_id: Uuid, email: &str) -> Result<()> {
    let token = crypto::random_token(TOKEN_BYTES);
    st.kv
        .set_json(
            &verify_key(&crypto::token_hash(&token)),
            &user_id,
            Some(VERIFY_TTL_SECS),
        )
        .await?;

    // A relay failure must not fail the registration that triggered it: the
    // account exists either way, and the operator can resend.
    if let Err(e) = st.mailer.send_verification(email, &token).await {
        tracing::warn!(user = %user_id, error = %e, "verification email could not be sent");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Password recovery
// ---------------------------------------------------------------------------

/// The address a reset was asked for.
#[derive(Debug, Deserialize)]
struct ForgotBody {
    email: String,
}

/// Store a fresh password-reset token for `email` and return it, with the
/// account it opens. `None` when no account has that address.
///
/// The one place a reset token is minted: `forgot_password` mails it, and the
/// operator command `micaforge-server reset-link` prints it, for an install with
/// no SMTP relay, where mail is never sent and the log never carries a link.
pub async fn issue_reset_token(
    pg: &micaforge_store::pg::PgPool,
    kv: &micaforge_store::kv::Kv,
    email: &str,
) -> Result<Option<(Uuid, String)>> {
    let Some(user) = users::find_by_email(pg, email).await? else {
        return Ok(None);
    };
    let token = crypto::random_token(TOKEN_BYTES);
    kv.set_json(
        &reset_key(&crypto::token_hash(&token)),
        &user.id,
        Some(RESET_TTL_SECS),
    )
    .await?;
    Ok(Some((user.id, token)))
}

/// `POST /api/auth/forgot-password`.
///
/// Always answers `ok`. An endpoint that said "no such account" would be a
/// public address-checking service.
async fn forgot_password(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    Json(body): Json<ForgotBody>,
) -> ApiResult<Json<Value>> {
    let started = Instant::now();

    // Limited per address rather than per account, so the mail path cannot be
    // used to flood somebody's inbox.
    if !st
        .kv
        .rate_limit(
            &format!("auth-reset:{}", ip_digest(&st.cfg, &ip)),
            REGISTER_IP_LIMIT,
            REGISTER_IP_WINDOW,
        )
        .await?
    {
        return Err(Error::RateLimited {
            retry_after: u64::from(REGISTER_IP_WINDOW),
        }
        .into());
    }

    let email = body.email.trim().to_lowercase();
    if let Some((user_id, token)) = issue_reset_token(&st.pg, &st.kv, &email).await? {
        if let Err(e) = st.mailer.send_password_reset(&email, &token).await {
            tracing::warn!(user = %user_id, error = %e, "reset email could not be sent");
        }
    }

    settle(started).await;
    Ok(ok(json!({
        "status": "ok",
        "message": "if that address has an account, a reset link is on its way",
    })))
}

/// A reset token and the password to install.
#[derive(Debug, Deserialize)]
struct ResetBody {
    token: String,
    password: String,
}

/// `POST /api/auth/reset-password`.
///
/// Signs every existing session out. A recovery that left the attacker's
/// cookies working would not be a recovery.
async fn reset_password(
    State(st): State<AppState>,
    Json(body): Json<ResetBody>,
) -> ApiResult<Json<Value>> {
    check_password(&body.password)?;

    let key = reset_key(&crypto::token_hash(body.token.trim()));
    let user_id: Option<Uuid> = st.kv.get_json(&key).await?;
    let Some(user_id) = user_id else {
        return Err(Error::not_found("reset link").into());
    };

    let hash = hash_password_blocking(body.password).await?;
    users::set_password(&st.pg, user_id, &hash).await?;
    // Following a reset link proves control of the mailbox, which is the same
    // proof the verification link asks for.
    users::mark_email_verified(&st.pg, user_id).await?;

    let closed = sessions::delete_for_user(&st.pg, user_id).await?;
    st.kv.del(&key).await?;
    reset_backoff(&st, &backoff_key_for_user(&st, user_id).await?).await?;

    tracing::info!(user = %user_id, sessions_closed = closed, "password reset");
    Ok(ok(json!({ "status": "reset", "sessions_closed": closed })))
}

/// A signed-in password change.
#[derive(Debug, Deserialize)]
struct ChangePasswordBody {
    current_password: String,
    new_password: String,
}

/// `POST /api/auth/password` — change it while signed in.
async fn change_password(
    State(st): State<AppState>,
    auth: AuthUser,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<ChangePasswordBody>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    auth.require_session()?;
    if !verify_password_blocking(body.current_password, auth.user.password_hash.clone()).await? {
        return Err(Error::Unauthorized("your current password is not right".into()).into());
    }
    check_password(&body.new_password)?;

    let hash = hash_password_blocking(body.new_password).await?;
    users::set_password(&st.pg, auth.user.id, &hash).await?;

    // Every other device is signed out, then this one is signed back in, so the
    // person who made the change is not the one inconvenienced by it.
    let closed = sessions::delete_for_user(&st.pg, auth.user.id).await?;
    let (jar, session) = open_session(&st, jar, &auth.user, &headers, &ip).await?;

    Ok((
        jar,
        ok(json!({
            "status": "changed",
            "other_sessions_closed": closed.saturating_sub(1),
            "expires_at": session.expires_at,
        })),
    ))
}

// ---------------------------------------------------------------------------
// Two-factor
// ---------------------------------------------------------------------------

/// `POST /api/auth/totp/enroll` — start enrolment.
///
/// The seed is held in Redis and **not** written to the user's row until a code
/// proves the authenticator actually has it. Writing it immediately would lock
/// somebody out of their own account if they closed the tab before scanning.
async fn totp_enroll(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    if auth.user.has_totp() {
        return Err(Error::Conflict(
            "two-factor is already enabled; disable it before enrolling again".into(),
        )
        .into());
    }

    let secret = Secret::generate_secret();
    let encoded = secret.to_encoded().to_string();
    // Built and immediately dropped: constructing it is what proves the seed
    // this account is about to be shown is one the verifier will accept.
    let bytes = secret
        .to_bytes()
        .map_err(|e| Error::internal(format!("could not encode a TOTP seed: {e:?}")))?;
    build_totp(bytes)?;

    st.kv
        .set_json(
            &totp_pending_key(auth.user.id),
            &encoded,
            Some(TOTP_PENDING_TTL_SECS),
        )
        .await?;

    Ok(ok(json!({
        "secret": encoded,
        "otpauth_url": otpauth_url(&encoded, &auth.user.email),
        "digits": TOTP_DIGITS,
        "period_seconds": TOTP_STEP_SECONDS,
        "expires_in_seconds": TOTP_PENDING_TTL_SECS,
    })))
}

/// A code from an authenticator app.
#[derive(Debug, Deserialize)]
struct TotpCodeBody {
    code: String,
}

/// `POST /api/auth/totp/verify` — confirm enrolment and turn it on.
async fn totp_verify(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TotpCodeBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let key = totp_pending_key(auth.user.id);
    let pending: Option<String> = st.kv.get_json(&key).await?;
    let Some(encoded) = pending else {
        return Err(Error::not_found("two-factor enrolment").into());
    };

    let bytes = Secret::Encoded(encoded)
        .to_bytes()
        .map_err(|e| Error::internal(format!("could not decode the TOTP seed: {e:?}")))?;
    let totp = build_totp(bytes.clone())?;

    if !check_code(&totp, body.code.trim())? {
        return Err(Error::Unauthorized("that code is not right".into()).into());
    }

    let sealed = crypto::seal(&st.cfg.derive_key("totp"), auth.user.id.as_bytes(), &bytes)?;
    users::set_totp(&st.pg, auth.user.id, Some(&sealed)).await?;
    st.kv.del(&key).await?;

    tracing::info!(user = %auth.user.id, "two-factor enabled");
    Ok(ok(json!({ "status": "enabled" })))
}

/// Turning two-factor off asks for the password, not a code.
#[derive(Debug, Deserialize)]
struct TotpDisableBody {
    password: String,
}

/// `POST /api/auth/totp/disable`.
///
/// Requires the password rather than a code, because the case that matters is a
/// lost authenticator, and asking for a code from the device somebody no longer
/// has would make the account unrecoverable.
async fn totp_disable(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TotpDisableBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    if !verify_password_blocking(body.password, auth.user.password_hash.clone()).await? {
        return Err(Error::Unauthorized("that password is not right".into()).into());
    }
    users::set_totp(&st.pg, auth.user.id, None).await?;
    st.kv.del(&totp_pending_key(auth.user.id)).await?;

    tracing::info!(user = %auth.user.id, "two-factor disabled");
    Ok(ok(json!({ "status": "disabled" })))
}

// ---------------------------------------------------------------------------
// The current account
// ---------------------------------------------------------------------------

/// `GET /api/auth/me` — who is signed in, and what they can reach.
///
/// Also where the rolling session is rolled: the stored expiry and the browser's
/// copy both move forward, so the dashboard keeps somebody signed in simply by
/// being open.
async fn me(
    State(st): State<AppState>,
    auth: AuthUser,
    jar: CookieJar,
) -> ApiResult<(CookieJar, Json<Value>)> {
    auth.require_session()?;
    let jar = roll_session(&st, jar).await?;

    let mut organizations = Vec::new();
    for org in orgs::list_for_user(&st.pg, auth.user.id).await? {
        let role = orgs::get_member(&st.pg, org.id, auth.user.id)
            .await?
            .map(|m| m.role);
        organizations.push(json!({
            "id": org.id,
            "name": org.name,
            "slug": org.slug,
            "role": role,
            "is_owner": org.owner_id == auth.user.id,
        }));
    }

    let mut visible_sites = Vec::new();
    for site in sites::list_sites_for_user(&st.pg, auth.user.id).await? {
        let role = crate::routes::sites::effective_site_role(&st.pg, &site, auth.user.id).await?;
        visible_sites.push(json!({
            "id": site.id,
            "site_id": site.site_id_u16(),
            "org_id": site.org_id,
            "name": site.name,
            "domain": site.domain,
            "role": role,
        }));
    }

    let is_admin = crate::routes::admin::is_instance_admin(&st, &auth.user).await?;

    Ok((
        jar,
        ok(json!({
            "user": user_json(&auth.user, is_admin),
            "organizations": organizations,
            "sites": visible_sites,
            "instance": {
                "self_hosted": st.cfg.self_hosted,
                "registration_open": !st.cfg.disable_registration,
                "mail_enabled": st.cfg.mail_enabled(),
            },
        })),
    ))
}

/// The fields a person may change about themselves.
#[derive(Debug, Deserialize)]
struct ProfilePatch {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
}

/// `PATCH /api/auth/me` — edit the profile.
async fn update_me(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<ProfilePatch>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let name = match body.name.as_deref() {
        Some(raw) => clean_name(raw, "name")?,
        None => auth.user.name.clone(),
    };
    let avatar = match body.avatar_url.as_deref().map(str::trim) {
        Some("") => None,
        Some(raw) => Some(clean_avatar_url(raw)?),
        None => auth.user.avatar_url.clone(),
    };

    let updated = users::update_profile(&st.pg, auth.user.id, &name, avatar.as_deref()).await?;
    Ok(ok(user_json(&updated, false)))
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Open a session and hand the browser its cookie.
async fn open_session(
    st: &AppState,
    jar: CookieJar,
    user: &users::User,
    headers: &HeaderMap,
    ip: &IpAddr,
) -> Result<(CookieJar, sessions::AuthSession)> {
    let token = crypto::random_token(TOKEN_BYTES);
    let expires_at = Utc::now() + Duration::days(SESSION_DAYS);

    let session = sessions::create(
        &st.pg,
        new_id(),
        user.id,
        &crypto::token_hash(&token),
        user_agent(headers).as_deref(),
        Some(&ip_digest(&st.cfg, ip)),
        expires_at,
    )
    .await?;

    Ok((jar.add(session_cookie(&st.cfg, &token)?), session))
}

/// Push the presented session's expiry forward, in Postgres and in the browser.
///
/// Silently does nothing when there is no cookie or the session is already gone:
/// the caller was authenticated by something else (an API key), and there is no
/// session to roll.
async fn roll_session(st: &AppState, jar: CookieJar) -> Result<CookieJar> {
    let Some(token) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) else {
        return Ok(jar);
    };
    let Some(session) = sessions::find_by_token_hash(&st.pg, &crypto::token_hash(&token)).await?
    else {
        return Ok(jar);
    };

    let expires_at = Utc::now() + Duration::days(SESSION_DAYS);
    sessions::extend(&st.pg, session.id, expires_at).await?;
    Ok(jar.add(session_cookie(&st.cfg, &token)?))
}

/// The session cookie, with every attribute set explicitly.
///
/// Built by parsing a header value rather than by the builder, because the
/// builder's `max_age` needs a `time::Duration` this crate does not depend on,
/// and a cookie whose lifetime is left to the browser is a session cookie that
/// dies when the tab closes.
fn session_cookie(cfg: &Config, token: &str) -> Result<Cookie<'static>> {
    build_cookie(cfg, token, SESSION_DAYS * 24 * 60 * 60)
}

/// The same cookie, expired, which is how a browser is told to forget it.
fn cleared_cookie(cfg: &Config) -> Result<Cookie<'static>> {
    build_cookie(cfg, "", 0)
}

/// Assemble a `Set-Cookie` value.
fn build_cookie(cfg: &Config, value: &str, max_age_secs: i64) -> Result<Cookie<'static>> {
    // Secure is decided by the *public* origin, not by whether this particular
    // request arrived over TLS: a request that reached a reverse proxy on http
    // is still part of an https deployment.
    let secure =
        if cfg.public_url.starts_with("https://") || cfg.dashboard_url.starts_with("https://") {
            "; Secure"
        } else {
            ""
        };

    Cookie::parse(format!(
        "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
    ))
    .map_err(|e| Error::internal(format!("could not build the session cookie: {e}")))
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

/// One address's recent sign-in failures.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Backoff {
    /// Consecutive failures. Reset to zero by a successful sign-in.
    failures: u32,
    /// Epoch seconds before which no attempt is even considered.
    locked_until: i64,
    /// Random, renewed whenever the state moves without a failure (a success,
    /// a `totp_required`, a reset), so no two states ever share a guess slot.
    /// Empty only before the address has ever been tried.
    #[serde(default)]
    epoch: String,
}

/// The guess slot for one backoff state.
///
/// Claimed with `Kv::rate_limit(.., 1, ..)`, an atomic `INCR`, so exactly one
/// request per state reaches Argon2. Every outcome moves the state on (a failure
/// raises `failures` and `locked_until`; anything else renews `epoch`), which is
/// what frees the next slot for the next honest attempt.
fn gate_key(account_key: &str, state: &Backoff) -> String {
    format!(
        "auth-gate:{account_key}:{}:{}:{}",
        state.failures, state.locked_until, state.epoch
    )
}

/// A fresh epoch.
fn new_epoch() -> String {
    crypto::random_token(9)
}

/// Move the state on without counting a failure.
async fn advance(st: &AppState, key: &str, state: &Backoff) -> Result<()> {
    let next = Backoff {
        failures: state.failures,
        locked_until: state.locked_until,
        epoch: new_epoch(),
    };
    st.kv.set_json(key, &next, Some(BACKOFF_TTL_SECS)).await
}

/// Clear the failures, after a sign-in or a password reset.
///
/// Written rather than deleted: an absent record is the one state every
/// address shares before its first attempt, and returning to it would put the
/// next attempt on a slot that may already be spent.
async fn reset_backoff(st: &AppState, key: &str) -> Result<()> {
    advance(st, key, &Backoff::default()).await
}

/// `mf:auth:backoff:<digest of the address>`.
///
/// Keyed by digest rather than by address so that a Redis dump does not list
/// which addresses somebody has been trying, and so the key exists identically
/// for an address with no account.
fn backoff_key(email: &str) -> String {
    format!("mf:auth:backoff:{}", crypto::token_hash(email.trim()))
}

/// The same key, for a user whose address has to be looked up first.
async fn backoff_key_for_user(st: &AppState, user_id: Uuid) -> Result<String> {
    let user = users::get(&st.pg, user_id).await?;
    Ok(backoff_key(&user.email))
}

/// How long to hold an address off after `failures` consecutive failures.
///
/// Doubling from two seconds, capped. The first two failures cost nothing so
/// ordinary mistakes are not punished; by the tenth the attacker is waiting
/// minutes per guess, which ends online guessing without ending the account.
fn backoff_delay(failures: u32) -> i64 {
    if failures <= BACKOFF_FREE_ATTEMPTS {
        return 0;
    }
    let steps = (failures - BACKOFF_FREE_ATTEMPTS).min(20);
    // `1 << 20` is about twelve days, far past the cap, so the shift cannot
    // overflow into a nonsensical delay.
    (1i64 << steps).min(BACKOFF_MAX_SECS)
}

/// Record one failure and extend the hold.
async fn record_failure(st: &AppState, key: &str, previous: Backoff) -> Result<()> {
    let failures = previous.failures.saturating_add(1);
    let epoch = if previous.epoch.is_empty() {
        new_epoch()
    } else {
        previous.epoch
    };
    let next = Backoff {
        failures,
        locked_until: Utc::now().timestamp() + backoff_delay(failures),
        epoch,
    };
    st.kv.set_json(key, &next, Some(BACKOFF_TTL_SECS)).await
}

// ---------------------------------------------------------------------------
// Argon2, off the runtime
// ---------------------------------------------------------------------------

/// Hash a password on the blocking pool.
///
/// Argon2id is tens to hundreds of milliseconds of pure CPU by design. Run on an
/// async worker it stalls every other request scheduled on that thread for the
/// whole of it, which turns a burst of sign-ins into a stalled server.
pub(crate) async fn hash_password_blocking(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || crypto::hash_password(&password))
        .await
        .map_err(|e| Error::internal(format!("the password hasher stopped: {e}")))?
}

/// Verify a password on the blocking pool. See [`hash_password_blocking`].
///
/// The comparison inside is Argon2's own, which is constant-time.
pub(crate) async fn verify_password_blocking(password: String, stored: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || crypto::verify_password(&password, &stored))
        .await
        .map_err(|e| Error::internal(format!("the password verifier stopped: {e}")))
}

/// Verify a sign-in password against an account's digest, or against
/// [`ABSENT_ACCOUNT_HASH`] when there is no account, so both cost the same.
async fn verify_account_password(password: String, stored: Option<String>) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        let digest = stored.as_deref().unwrap_or(ABSENT_ACCOUNT_HASH.as_str());
        crypto::verify_password(&password, digest)
    })
    .await
    .map_err(|e| Error::internal(format!("the password verifier stopped: {e}")))
}

// ---------------------------------------------------------------------------
// TOTP plumbing
// ---------------------------------------------------------------------------

/// Build a TOTP from raw seed bytes.
///
/// SHA-1, six digits, thirty seconds: not a preference, but what every
/// authenticator app implements. A skew of one accepts the adjacent window,
/// which is what makes a phone whose clock is a few seconds out still work,
/// without widening the guessing surface any further than that.
///
/// The account name is not passed in. `totp-rs` only carries an issuer and an
/// account under its `otpauth` feature, which this workspace does not enable, so
/// [`otpauth_url`] builds that string from the same parameters instead.
fn build_totp(secret: Vec<u8>) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1,
        TOTP_DIGITS,
        TOTP_SKEW,
        TOTP_STEP_SECONDS,
        secret,
    )
    .map_err(|e| Error::internal(format!("could not build a TOTP: {e}")))
}

/// The `otpauth://` URI an authenticator app scans.
///
/// Written here rather than taken from the library: enabling the `otpauth`
/// feature would add a URL parser and a second encoder to the binary to produce
/// one string. Every parameter is stated explicitly rather than left to the
/// app's defaults, because several popular authenticators quietly assume SHA-1
/// and six digits and one that does not would generate codes that never verify.
fn otpauth_url(secret_base32: &str, account: &str) -> String {
    format!(
        "otpauth://totp/{label}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits={TOTP_DIGITS}&period={TOTP_STEP_SECONDS}",
        label = percent_encode(&format!("{TOTP_ISSUER}:{account}")),
        secret = percent_encode(secret_base32),
        issuer = percent_encode(TOTP_ISSUER),
    )
}

/// Percent-encode everything outside the RFC 3986 unreserved set.
///
/// Deliberately conservative: encoding a character that did not need it is
/// always safe, and this value goes into both a path segment and a query
/// parameter, which have different reserved sets. An address containing `+`,
/// `&` or `=` — all legal in a mailbox — would otherwise split the URI.
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Check a code against a TOTP.
fn check_code(totp: &TOTP, code: &str) -> Result<bool> {
    totp.check_current(code)
        .map_err(|e| Error::internal(format!("the system clock is unusable: {e}")))
}

/// Open a stored TOTP seed and check a code against it.
fn verify_totp_secret(cfg: &Config, user: &users::User, sealed: &[u8], code: &str) -> Result<bool> {
    let bytes = crypto::open(&cfg.derive_key("totp"), user.id.as_bytes(), sealed)?;
    let totp = build_totp(bytes)?;
    check_code(&totp, code)
}

// ---------------------------------------------------------------------------
// Request shaping
// ---------------------------------------------------------------------------

/// A salted digest of an address, never the address.
///
/// Rate-limit keys and the `sessions_auth.ip_hash` column both take this form,
/// for the same reason `visitor_id` does: distinguishing two devices needs a
/// hash, and nothing in the product needs the address itself. The address is
/// the one [`ClientAddr`] resolved by the trusted-proxy rule, never a header
/// read here.
fn ip_digest(cfg: &Config, ip: &IpAddr) -> String {
    crypto::visitor_hash(&cfg.derive_key("session-ip"), 0, &ip.to_string(), "")
}

/// The user agent at sign-in, for the device list.
fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.chars().take(400).collect())
}

/// Pad a response out to [`AUTH_FLOOR_MS`].
///
/// The whole point is that the *fast* path is slowed to match the slow one, so
/// an attacker timing the endpoint learns nothing about which branch ran.
async fn settle(started: Instant) {
    let floor = std::time::Duration::from_millis(AUTH_FLOOR_MS);
    let elapsed = started.elapsed();
    if elapsed < floor {
        tokio::time::sleep(floor - elapsed).await;
    }
}

// ---------------------------------------------------------------------------
// Validation and shaping
// ---------------------------------------------------------------------------

/// Bound a password.
///
/// Length only. Composition rules ("one digit, one symbol") measurably push
/// people towards weaker, more predictable passwords, and the account is already
/// protected by Argon2id and a lockout curve.
fn check_password(raw: &str) -> Result<()> {
    let length = raw.chars().count();
    if length < MIN_PASSWORD {
        return Err(Error::validation(
            "password",
            format!("must be at least {MIN_PASSWORD} characters"),
        ));
    }
    if length > MAX_PASSWORD {
        return Err(Error::validation(
            "password",
            format!("cannot be longer than {MAX_PASSWORD} characters"),
        ));
    }
    Ok(())
}

/// Accept only an `http`/`https` avatar URL.
///
/// A `javascript:` or `data:` value here would be rendered into an `img` tag on
/// every screen that shows a member list.
fn clean_avatar_url(raw: &str) -> Result<String> {
    let url = raw.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(Error::validation(
            "avatar_url",
            "must be an http or https URL",
        ));
    }
    if url.len() > 2048 {
        return Err(Error::validation("avatar_url", "is too long"));
    }
    Ok(url.to_string())
}

/// A readable default display name from an address.
fn default_name(email: &str) -> String {
    let local = email.split('@').next().unwrap_or(email);
    let cleaned: String = local
        .chars()
        .map(|c| {
            if c == '.' || c == '_' || c == '-' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "New user".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

/// One user on the wire. Never carries the password digest or the TOTP seed.
fn user_json(user: &users::User, is_instance_admin: bool) -> Value {
    json!({
        "id": user.id,
        "email": user.email,
        "name": user.name,
        "avatar_url": user.avatar_url,
        "email_verified": user.email_verified_at.is_some(),
        "email_verified_at": user.email_verified_at,
        "totp_enabled": user.has_totp(),
        "instance_admin": is_instance_admin,
        "created_at": user.created_at,
    })
}

/// `mf:auth:verify:<digest>`.
fn verify_key(digest: &str) -> String {
    format!("mf:auth:verify:{digest}")
}

/// `mf:auth:reset:<digest>`.
fn reset_key(digest: &str) -> String {
    format!("mf:auth:reset:{digest}")
}

/// `mf:auth:totp:<user>` — an enrolment that has not been confirmed yet.
fn totp_pending_key(user_id: Uuid) -> String {
    format!("mf:auth:totp:{user_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_two_mistakes_are_free_and_then_it_doubles() {
        assert_eq!(backoff_delay(0), 0);
        assert_eq!(backoff_delay(BACKOFF_FREE_ATTEMPTS), 0);
        assert_eq!(backoff_delay(3), 2);
        assert_eq!(backoff_delay(4), 4);
        assert_eq!(backoff_delay(5), 8);
        // The cap holds, and a corrupt counter cannot walk past it.
        assert_eq!(backoff_delay(30), BACKOFF_MAX_SECS);
        assert_eq!(backoff_delay(u32::MAX), BACKOFF_MAX_SECS);
    }

    #[test]
    fn the_absent_account_hash_is_a_real_argon2_digest() {
        // If this were malformed, verifying against it would return instantly
        // and "no such account" would become the measurably faster answer.
        assert!(ABSENT_ACCOUNT_HASH.starts_with("$argon2id$"));
        assert!(!crypto::verify_password("anything", &ABSENT_ACCOUNT_HASH));
    }

    #[test]
    fn backoff_keys_never_contain_the_address() {
        let key = backoff_key("Owner@Example.com");
        assert!(!key.contains('@'));
        assert!(!key.to_lowercase().contains("owner"));
        // Trimming and the digest are stable, so retries land on one key.
        assert_eq!(key, backoff_key("  Owner@Example.com  "));
    }

    #[test]
    fn passwords_are_bounded_at_both_ends() {
        assert!(check_password(&"x".repeat(MIN_PASSWORD)).is_ok());
        assert!(check_password(&"x".repeat(MIN_PASSWORD - 1)).is_err());
        assert!(check_password(&"x".repeat(MAX_PASSWORD + 1)).is_err());
    }

    #[test]
    fn the_cookie_carries_every_attribute_that_matters() {
        let mut cfg = test_config();
        cfg.public_url = "https://mf.example.com".into();
        let cookie = session_cookie(&cfg, "a-token").unwrap();

        assert_eq!(cookie.name(), SESSION_COOKIE);
        assert_eq!(cookie.value(), "a-token");
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(
            cookie.secure(),
            Some(true),
            "an https origin must set Secure"
        );
        assert_eq!(cookie.path(), Some("/"));
        assert!(
            cookie.max_age().is_some(),
            "a session cookie would die with the tab"
        );
    }

    #[test]
    fn a_plain_http_install_does_not_set_secure() {
        // Setting Secure on http would make the cookie undeliverable, and a
        // local `docker compose up` is a supported deployment.
        let cookie = session_cookie(&test_config(), "a-token").unwrap();
        assert_ne!(cookie.secure(), Some(true));
    }

    #[test]
    fn clearing_the_cookie_expires_it_immediately() {
        let cookie = cleared_cookie(&test_config()).unwrap();
        assert_eq!(cookie.value(), "");
        assert_eq!(cookie.max_age().map(|d| d.whole_seconds()), Some(0));
    }

    #[test]
    fn every_backoff_state_has_its_own_guess_slot() {
        let key = backoff_key("owner@example.com");
        let fresh = Backoff::default();
        let failed = Backoff {
            failures: 1,
            locked_until: 1_700_000_000,
            epoch: "e1".into(),
        };
        let failed_again = Backoff {
            failures: 2,
            ..failed.clone()
        };
        let renewed = Backoff {
            epoch: "e2".into(),
            ..failed.clone()
        };

        // Requests that read the same state race for the same slot.
        assert_eq!(gate_key(&key, &failed), gate_key(&key, &failed.clone()));
        // Every way the state moves on frees a different one.
        let slots = [
            gate_key(&key, &fresh),
            gate_key(&key, &failed),
            gate_key(&key, &failed_again),
            gate_key(&key, &renewed),
        ];
        for (i, a) in slots.iter().enumerate() {
            for b in &slots[i + 1..] {
                assert_ne!(a, b);
            }
        }
        // And another account never shares one.
        assert_ne!(
            gate_key(&backoff_key("other@example.com"), &failed),
            gate_key(&key, &failed)
        );
        // The slot name carries no address either.
        assert!(!gate_key(&key, &failed).contains('@'));
    }

    #[test]
    fn a_reset_never_returns_to_the_shared_fresh_state() {
        // A record from before the epoch existed still reads.
        let old: Backoff = serde_json::from_str(r#"{"failures":3,"locked_until":5}"#).unwrap();
        assert!(old.epoch.is_empty());
        // Two epochs drawn in a row differ, so two resets in a row do too.
        assert_ne!(new_epoch(), new_epoch());
        assert!(!new_epoch().is_empty());
    }

    #[tokio::test]
    async fn argon2_runs_off_the_runtime_and_still_agrees_with_itself() {
        let digest = hash_password_blocking("correct horse battery".into())
            .await
            .unwrap();
        assert!(
            verify_password_blocking("correct horse battery".into(), digest.clone())
                .await
                .unwrap()
        );
        assert!(!verify_password_blocking("wrong".into(), digest)
            .await
            .unwrap());
        // No account costs a real verification and never succeeds.
        assert!(!verify_account_password("anything".into(), None)
            .await
            .unwrap());
    }

    #[test]
    fn an_avatar_url_can_only_be_http() {
        assert!(clean_avatar_url("https://example.com/a.png").is_ok());
        assert!(clean_avatar_url("javascript:alert(1)").is_err());
        assert!(clean_avatar_url("data:image/png;base64,AAAA").is_err());
    }

    #[test]
    fn the_otpauth_uri_states_every_parameter_and_escapes_the_account() {
        let url = otpauth_url("JBSWY3DPEHPK3PXP", "ada+work@example.com");

        assert!(url.starts_with("otpauth://totp/Micaforge%3Aada%2Bwork%40example.com?"));
        assert!(url.contains("secret=JBSWY3DPEHPK3PXP"));
        assert!(url.contains("issuer=Micaforge"));
        // Left to the app's defaults, an authenticator that assumes anything
        // else would generate codes this server never accepts.
        assert!(url.contains("algorithm=SHA1"));
        assert!(url.contains("digits=6"));
        assert!(url.contains("period=30"));
        // The `+` and the `@` must not survive unescaped: either would split
        // the label from the query.
        assert!(!url.contains("ada+work"));
    }

    #[test]
    fn a_generated_seed_builds_a_verifier() {
        let secret = Secret::generate_secret();
        let encoded = secret.to_encoded().to_string();
        let bytes = secret.to_bytes().expect("a generated seed decodes");

        let totp = build_totp(bytes.clone()).expect("a generated seed is a usable TOTP");

        // The base32 the operator scans and the bytes that are sealed have to be
        // the same secret, or enrolment succeeds and sign-in never does.
        let round_tripped = Secret::Encoded(encoded).to_bytes().unwrap();
        assert_eq!(round_tripped, bytes);

        // A code this seed produces right now must verify against it.
        let current = totp.generate_current().expect("the clock is usable");
        assert!(check_code(&totp, &current).unwrap());
        assert_eq!(current.len(), TOTP_DIGITS);
    }

    #[test]
    fn a_default_name_is_readable() {
        assert_eq!(default_name("ada.lovelace@example.com"), "ada lovelace");
        assert_eq!(default_name("@example.com"), "New user");
    }

    /// A configuration with the three required values and nothing else.
    fn test_config() -> Config {
        Config {
            bind_addr: "0.0.0.0:8080".into(),
            database_url: "postgres://localhost/micaforge".into(),
            clickhouse_url: "http://localhost:8123".into(),
            clickhouse_db: "micaforge".into(),
            clickhouse_user: "default".into(),
            clickhouse_password: String::new(),
            redis_url: "redis://127.0.0.1:6379".into(),
            public_url: "http://localhost:8080".into(),
            dashboard_url: "http://localhost:4321".into(),
            secret_key_base: "x".repeat(48),
            geoip_city_path: None,
            geoip_asn_path: None,
            retention_days: 0,
            replay_retention_days: 30,
            disable_registration: false,
            self_hosted: true,
            smtp_url: None,
            mail_from: "Micaforge <no-reply@localhost>".into(),
            log_format: "text".into(),
            worker_concurrency: 2,
        }
    }
}
