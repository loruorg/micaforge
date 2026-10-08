//! `/api/auth/*` — sign-in by emailed code, two-factor, and the current account.
//!
//! There are no passwords. A person proves they control an address by reading a
//! six-digit code sent to it, and that is the whole sign-in: the same proof a
//! password-reset link used to ask for, without the password it would have
//! reset. Two calls make the flow:
//!
//! 1. `POST /api/auth/code` `{email, name?, org_name?}` sends a code to the
//!    address when it may sign in here, and answers the same either way.
//! 2. `POST /api/auth/code/verify` `{email, code, totp?, name?, org_name?}`
//!    trades the code for the session cookie, creating the account first when
//!    the address has none and is allowed one.
//!
//! Four properties are load-bearing here, and each one exists because the
//! obvious implementation leaks something.
//!
//! **1. No answer reveals whether an address has an account.** `code` answers
//! one body for every address, sent or not, and is padded to a fixed floor
//! ([`AUTH_FLOOR_MS`]) so the *duration* does not answer the question the body
//! refuses to. The email itself is sent from a background task, so a slow relay
//! cannot make "sent" measurably slower than "not sent". `code/verify` refuses
//! a wrong code, an expired code and an address that was never sent one with
//! the same sentence, after the same floor.
//!
//! **2. Guessing is bounded three ways.** A code is one of a million values and
//! lives ten minutes. Five wrong guesses against one code delete it
//! ([`CODE_MAX_MISSES`]). And every verification is counted per address with an
//! atomic Redis increment before anything is looked up
//! ([`VERIFY_ADDRESS_LIMIT`]), so a parallel burst gets no more guesses than one
//! patient client, and an attacker who keeps asking for fresh codes still gets
//! ten guesses per address per ten minutes: one chance in a hundred thousand.
//! Per-IP windows sit in front of both calls, keyed on the address the
//! trusted-proxy rule resolves ([`crate::client_ip`]), so a forged header buys
//! nothing.
//!
//! **3. A code works once, and only the latest one works.** It is stored as one
//! value per address, so a new request replaces the old code outright, and it
//! is consumed with an atomic compare-and-swap ([`Kv::swap_json`]): two requests
//! racing with the right code cannot both sign in, and a request that read an
//! old code cannot consume, or write back, a newer one.
//!
//! **4. Nothing recoverable is stored.** The code is kept as an HMAC keyed off
//! `MICAFORGE_SECRET_KEY_BASE` (a plain digest of six digits is reversed by a
//! million guesses), under a key built from a keyed digest of the address, so a
//! Redis dump lists neither codes nor addresses. Session cookies are random
//! tokens whose SHA-256 digest is what Postgres keeps. A code never reaches a
//! log line, here or in the mailer's logging-only mode.
//!
//! ## Who may be sent a code
//!
//! [`door`] is the rule. An address with an account; any address while the
//! install has no account at all, because the first account owns the install
//! (`routes::admin`); any address while registration is open; and an address an
//! unexpired invitation is waiting for (`routes::orgs`). Anybody else is sent
//! nothing, and told nothing.
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
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::{DateTime, Duration, Utc};
use micaforge_core::{crypto, new_id, Config, Error, Result};
use micaforge_store::kv::Snapshot;
use micaforge_store::models::{auth as sessions, orgs, sites, users};
use micaforge_store::pg::PgPool;
use micaforge_store::Kv;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, ClientAddr};
use crate::response::ok;
use crate::routes::orgs::{
    clean_email, clean_name, pending_invites_for, redeem_invite, unique_slug, OWNER,
};
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

/// Entropy in a session token.
const TOKEN_BYTES: usize = 32;

/// Digits in a sign-in code. Six is what a person can hold in their head
/// between two windows, and what a phone offers to fill in from a message.
const CODE_DIGITS: u32 = 6;

/// How long a sign-in code works.
///
/// Ten minutes: long enough for a slow relay and a person who had to find
/// their phone, short enough that a code found in an old mailbox is dead.
const CODE_TTL_SECS: u32 = 10 * 60;

/// Wrong guesses one code survives. The fifth deletes it, and the person asks
/// for a new one. A wrong authenticator code counts as a wrong guess too.
const CODE_MAX_MISSES: u32 = 5;

/// How often a lost compare-and-swap on a code is retried. See [`record_miss`].
const SWAP_RETRIES: usize = 4;

/// Codes one client address may ask for per window, for any addresses.
const REQUEST_IP_LIMIT: u32 = 10;
/// That window, in seconds.
const REQUEST_IP_WINDOW: u32 = 10 * 60;

/// Codes one email address may be sent per window, whoever asks.
///
/// Three, so the mail path cannot be used to flood somebody's inbox, and so a
/// person who asked twice and then found the first email is not refused. The
/// refusal is the same for every address, so it reveals nothing.
const REQUEST_ADDRESS_LIMIT: u32 = 3;
/// That window, in seconds.
const REQUEST_ADDRESS_WINDOW: u32 = 10 * 60;

/// Verifications one client address may make per window, for any addresses.
const VERIFY_IP_LIMIT: u32 = 30;
/// That window, in seconds.
const VERIFY_IP_WINDOW: u32 = 10 * 60;

/// Verifications one email address may receive per window, whoever sends them.
///
/// The bound on guessing that holds however many codes are asked for and
/// however many client addresses the guesses come from. Ten leaves room for
/// typos and a missing authenticator step.
const VERIFY_ADDRESS_LIMIT: u32 = 10;
/// That window, in seconds.
const VERIFY_ADDRESS_WINDOW: u32 = 10 * 60;

/// Attempts at the authenticator code that turns two-factor off, per account.
///
/// A session left open on a shared machine must not become a way to guess the
/// second factor away: three codes in a million are valid at any moment, and
/// this keeps the guesses to a handful.
const TOTP_DISABLE_LIMIT: u32 = 5;
/// That window, in seconds.
const TOTP_DISABLE_WINDOW: u32 = 15 * 60;

/// How long an unconfirmed TOTP enrolment stays claimable.
const TOTP_PENDING_TTL_SECS: u32 = 10 * 60;

/// The floor both sign-in calls are padded to on every answer that does not
/// sign anybody in.
///
/// Chosen well above the slowest path either call takes on its own (a handful
/// of Postgres and Valkey round trips), so the padding masks the difference
/// rather than merely adding to it.
const AUTH_FLOOR_MS: u64 = 400;

/// The advisory lock that makes "is this the first account?" and creating it
/// one step. Any constant works; it only has to be the same in every process.
const FIRST_ACCOUNT_LOCK: i64 = 0x6d66_6669_7273_7401;

/// The one sentence every refused code gets: wrong, expired, used, replaced, or
/// never sent. Telling those apart would tell an attacker which addresses were
/// sent one.
const CODE_REFUSED: &str = "that code is not right or has expired";

/// The refusal for a wrong authenticator code. Only reachable with the right
/// emailed code, so it says nothing the caller does not already know.
const TOTP_REFUSED: &str = "that authenticator code is not right";

/// Digits in a TOTP code. Six is what every authenticator app shows.
const TOTP_DIGITS: usize = 6;

/// Seconds one TOTP code is valid for.
const TOTP_STEP_SECONDS: u64 = 30;

/// Adjacent windows accepted either side of the current one.
const TOTP_SKEW: u8 = 1;

/// The issuer name an authenticator app shows beside the account.
const TOTP_ISSUER: &str = "Micaforge";

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every authentication route. Paths are absolute from the app root.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/code", post(request_code))
        .route("/api/auth/code/verify", post(verify_code))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/totp/enroll", post(totp_enroll))
        .route("/api/auth/totp/verify", post(totp_verify))
        .route("/api/auth/totp/disable", post(totp_disable))
        .route("/api/auth/me", get(me).patch(update_me))
}

// ---------------------------------------------------------------------------
// Who may be sent a code
// ---------------------------------------------------------------------------

/// Why an address may be sent a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Door {
    /// It has an account.
    Account,
    /// The install has no account yet, and the first one owns it.
    FirstAccount,
    /// Registration is open (`MICAFORGE_DISABLE_REGISTRATION` is false).
    OpenRegistration,
    /// An unexpired invitation is waiting for it.
    Invitation,
}

/// The rule, on its own so it can be read, and tested, without a store.
///
/// A closed install still admits exactly one account without an invitation:
/// the first. Otherwise setting `MICAFORGE_DISABLE_REGISTRATION` before the
/// owner signed up would leave an install nobody can enter, while
/// `/api/config` (which reports `registration_open` by the same rule) offered
/// the way in.
fn door(
    has_account: bool,
    no_accounts_yet: bool,
    registration_open: bool,
    invited: bool,
) -> Option<Door> {
    if has_account {
        Some(Door::Account)
    } else if no_accounts_yet {
        Some(Door::FirstAccount)
    } else if registration_open {
        Some(Door::OpenRegistration)
    } else if invited {
        Some(Door::Invitation)
    } else {
        None
    }
}

/// [`door`], with its three questions answered from the stores.
///
/// All three are always asked, whatever the first answer, so the work done
/// does not depend on which door is open.
async fn door_for(cfg: &Config, pg: &PgPool, kv: &Kv, email: &str) -> Result<Option<Door>> {
    let has_account = users::find_by_email(pg, email).await?.is_some();
    let no_accounts_yet = users::count(pg).await? == 0;
    let invited = !pending_invites_for(kv, cfg, email).await?.is_empty();
    Ok(door(
        has_account,
        no_accounts_yet,
        !cfg.disable_registration,
        invited,
    ))
}

// ---------------------------------------------------------------------------
// Asking for a code
// ---------------------------------------------------------------------------

/// What the first step of the sign-in form sends.
#[derive(Debug, Deserialize)]
struct CodeRequest {
    email: String,
    /// What to call the person, if this code ends up creating their account.
    #[serde(default)]
    name: Option<String>,
    /// What to call the organisation created alongside a new account.
    #[serde(default)]
    org_name: Option<String>,
}

/// The name and organisation name a brand-new account is created with.
///
/// Asked for when the code is requested, or when it is verified, or neither:
/// an address that already has an account ignores both, and a new one falls
/// back to a name made from the address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    org_name: Option<String>,
}

impl NewAccount {
    /// Validate what a request sent. A blank field is the same as no field.
    fn from_request(name: Option<&str>, org_name: Option<&str>) -> Result<NewAccount> {
        let clean = |raw: Option<&str>, field: &str| -> Result<Option<String>> {
            match raw {
                Some(raw) if !raw.trim().is_empty() => Ok(Some(clean_name(raw, field)?)),
                _ => Ok(None),
            }
        };
        Ok(NewAccount {
            name: clean(name, "name")?,
            org_name: clean(org_name, "org_name")?,
        })
    }

    /// Each field from `self` when it has one, else from `earlier`. What the
    /// verification sends wins over what the request stored.
    fn or(self, earlier: &NewAccount) -> NewAccount {
        NewAccount {
            name: self.name.or_else(|| earlier.name.clone()),
            org_name: self.org_name.or_else(|| earlier.org_name.clone()),
        }
    }
}

/// One address's live code, as it is held in Redis.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodeRecord {
    /// [`code_digest`] of the address and the code. The code is never stored.
    digest: String,
    /// Wrong guesses made against this code so far.
    #[serde(default)]
    misses: u32,
    /// The profile asked for with the code, for an account it may create.
    #[serde(default)]
    account: NewAccount,
}

impl CodeRecord {
    /// The record after one more wrong guess, or `None` when that guess was the
    /// last one the code survives and it is to be deleted.
    fn after_miss(&self) -> Option<CodeRecord> {
        let misses = self.misses.saturating_add(1);
        (misses < CODE_MAX_MISSES).then(|| CodeRecord {
            misses,
            ..self.clone()
        })
    }
}

/// A code that has just been stored, for the one caller allowed to see it.
///
/// Deliberately not `Debug`: nothing should be able to print it by accident.
pub struct IssuedCode {
    /// The six digits.
    pub code: String,
    /// When it stops working.
    pub expires_at: DateTime<Utc>,
}

/// Store a fresh code for `email` and return it, or `None` when the address may
/// not sign in here.
///
/// The one place a code is minted. [`request_code`] mails it, and the operator
/// command `micaforge-server sign-in-code` prints it, for an install with no
/// mail transport. Both replace whatever code the address had before, so only
/// the latest code ever works. `email` must already be through [`clean_email`].
///
/// The code is replaced and its wrong-guess count starts again; the profile
/// asked for with an earlier code is kept unless this call names a new one. On
/// an install with no mail transport the person asks on the sign-in screen
/// (naming themselves and their workspace) and the operator then prints the
/// code that works, and that code should not forget what they asked to be
/// called.
pub async fn issue_sign_in_code(
    cfg: &Config,
    pg: &PgPool,
    kv: &Kv,
    email: &str,
    account: NewAccount,
) -> Result<Option<IssuedCode>> {
    if door_for(cfg, pg, kv, email).await?.is_none() {
        return Ok(None);
    }

    let key = code_key(cfg, email);
    let earlier: Option<CodeRecord> = kv.get_json(&key).await?;
    let code = crypto::numeric_code(CODE_DIGITS);
    let record = CodeRecord {
        digest: code_digest(cfg, email, &code),
        misses: 0,
        account: match earlier {
            Some(earlier) => account.or(&earlier.account),
            None => account,
        },
    };
    kv.set_json(&key, &record, Some(CODE_TTL_SECS)).await?;

    Ok(Some(IssuedCode {
        code,
        expires_at: Utc::now() + Duration::seconds(i64::from(CODE_TTL_SECS)),
    }))
}

/// `POST /api/auth/code` — send a sign-in code.
///
/// Answers `{status: "ok", mail_delivered}` for every address, sent or not, and
/// takes the same time either way. `mail_delivered` says whether this install
/// sends mail at all, which is a fact about the install and not about the
/// address: when it is false the dashboard tells the reader to ask the operator
/// for a code instead of waiting for an email.
async fn request_code(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    Json(body): Json<CodeRequest>,
) -> ApiResult<Json<Value>> {
    let started = Instant::now();

    limit(
        &st.kv,
        &format!("auth-code:ip:{}", ip_digest(&st.cfg, &ip)),
        REQUEST_IP_LIMIT,
        REQUEST_IP_WINDOW,
    )
    .await?;

    let email = clean_email(&body.email)?;
    let account = NewAccount::from_request(body.name.as_deref(), body.org_name.as_deref())?;

    limit(
        &st.kv,
        &format!("auth-code:addr:{}", address_digest(&st.cfg, &email)),
        REQUEST_ADDRESS_LIMIT,
        REQUEST_ADDRESS_WINDOW,
    )
    .await?;

    if let Some(issued) = issue_sign_in_code(&st.cfg, &st.pg, &st.kv, &email, account).await? {
        // Sent from a task of its own, so the relay's speed, or its failure,
        // cannot show in this response. See the module docs.
        let mailer = Arc::clone(&st.mailer);
        tokio::spawn(async move {
            if let Err(e) = mailer
                .send_sign_in_code(&email, &issued.code, CODE_TTL_SECS / 60)
                .await
            {
                tracing::warn!(error = %e, "a sign-in code could not be sent");
            }
        });
    }

    settle(started).await;
    Ok(ok(json!({
        "status": "ok",
        "mail_delivered": st.mailer.is_sending(),
    })))
}

// ---------------------------------------------------------------------------
// Trading a code for a session
// ---------------------------------------------------------------------------

/// What the second step of the sign-in form sends.
///
/// Deliberately not `Debug`: it carries the code and the authenticator code.
#[derive(Deserialize)]
struct CodeVerify {
    email: String,
    code: String,
    /// Required when the account has two-factor enabled.
    #[serde(default)]
    totp: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    org_name: Option<String>,
}

/// How one verification ended.
enum Verdict {
    /// Refused, with the sentence to say.
    Refused(&'static str),
    /// The code is right, and the account needs an authenticator code it was
    /// not sent. The emailed code is left as it is for the follow-up.
    TotpRequired,
    /// Signed in.
    SignedIn {
        user: users::User,
        /// True when this verification created the account.
        created: bool,
        /// Organisations a new account joined through its invitations.
        joined: Vec<Uuid>,
    },
}

/// `POST /api/auth/code/verify` — trade a code for the session cookie.
///
/// The order is the point: the per-IP window, then the per-address count, then
/// the code. Counting the address before anything is read is what makes the
/// bound hold for an address that was never sent a code exactly as for one that
/// was, and the count is an atomic increment, so parallel requests cannot share
/// one allowance.
async fn verify_code(
    State(st): State<AppState>,
    ClientAddr(ip): ClientAddr,
    headers: HeaderMap,
    jar: CookieJar,
    Json(body): Json<CodeVerify>,
) -> ApiResult<(CookieJar, Json<Value>)> {
    let started = Instant::now();

    limit(
        &st.kv,
        &format!("auth-verify:ip:{}", ip_digest(&st.cfg, &ip)),
        VERIFY_IP_LIMIT,
        VERIFY_IP_WINDOW,
    )
    .await?;

    let email = clean_email(&body.email)?;

    limit(
        &st.kv,
        &format!("auth-verify:addr:{}", address_digest(&st.cfg, &email)),
        VERIFY_ADDRESS_LIMIT,
        VERIFY_ADDRESS_WINDOW,
    )
    .await?;

    match verify_attempt(&st, &email, &body).await? {
        Verdict::Refused(sentence) => {
            settle(started).await;
            Err(Error::Unauthorized(sentence.into()).into())
        }
        Verdict::TotpRequired => {
            settle(started).await;
            Ok((jar, ok(json!({ "status": "totp_required" }))))
        }
        Verdict::SignedIn {
            user,
            created,
            joined,
        } => {
            let (jar, session) = open_session(&st, jar, &user, &headers, &ip).await?;
            tracing::info!(user = %user.id, created, "signed in");
            Ok((
                jar,
                ok(json!({
                    "status": "ok",
                    "user": user_json(&user, false),
                    "expires_at": session.expires_at,
                    "created": created,
                    "joined": joined,
                })),
            ))
        }
    }
}

/// The part of a verification that runs once both windows have counted it.
async fn verify_attempt(st: &AppState, email: &str, body: &CodeVerify) -> Result<Verdict> {
    let Some(code) = normalise_code(&body.code) else {
        return Ok(Verdict::Refused(CODE_REFUSED));
    };

    let key = code_key(&st.cfg, email);
    let Some(seen) = st.kv.get_snapshot::<CodeRecord>(&key).await? else {
        return Ok(Verdict::Refused(CODE_REFUSED));
    };

    if !crypto::digests_match(&seen.value.digest, &code_digest(&st.cfg, email, &code)) {
        record_miss(&st.kv, &key, seen).await?;
        return Ok(Verdict::Refused(CODE_REFUSED));
    }

    match users::find_by_email(&st.pg, email).await? {
        Some(user) => sign_in(st, &key, seen, user, body.totp.as_deref()).await,
        None => {
            let asked = NewAccount::from_request(body.name.as_deref(), body.org_name.as_deref())?;
            create_account(st, &key, seen, email, asked).await
        }
    }
}

/// What a right code leads to for an account that exists, before anything is
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondFactor {
    /// No second factor, or the right one: consume the code and sign in.
    Passed,
    /// The account has two-factor and no authenticator code came with this
    /// request. Nothing is consumed and nothing is counted.
    Missing,
    /// The authenticator code is wrong. Counted against the emailed code, so
    /// the second factor is not an unlimited guessing surface once the first
    /// one is known.
    Wrong,
}

/// Decide [`SecondFactor`]. `check` is only called with a non-empty code.
fn second_factor(
    has_totp: bool,
    supplied: Option<&str>,
    check: impl FnOnce(&str) -> Result<bool>,
) -> Result<SecondFactor> {
    if !has_totp {
        return Ok(SecondFactor::Passed);
    }
    let supplied = supplied.unwrap_or("").trim();
    if supplied.is_empty() {
        return Ok(SecondFactor::Missing);
    }
    Ok(if check(supplied)? {
        SecondFactor::Passed
    } else {
        SecondFactor::Wrong
    })
}

/// Sign an existing account in with a code that has been checked.
async fn sign_in(
    st: &AppState,
    key: &str,
    seen: Snapshot<CodeRecord>,
    user: users::User,
    totp: Option<&str>,
) -> Result<Verdict> {
    let outcome = second_factor(user.has_totp(), totp, |supplied| {
        let sealed = user.totp_secret_sealed.as_deref().unwrap_or_default();
        verify_totp_secret(&st.cfg, &user, sealed, supplied)
    })?;
    match outcome {
        SecondFactor::Missing => return Ok(Verdict::TotpRequired),
        SecondFactor::Wrong => {
            record_miss(&st.kv, key, seen).await?;
            return Ok(Verdict::Refused(TOTP_REFUSED));
        }
        SecondFactor::Passed => {}
    }

    // Single use. Losing this race means another request consumed the code,
    // or replaced it, between the read and now.
    if !st.kv.swap_json(key, &seen, None).await? {
        return Ok(Verdict::Refused(CODE_REFUSED));
    }

    // The code arrived at this address, which is the proof a verification
    // link used to ask for.
    users::mark_email_verified(&st.pg, user.id).await?;
    let user = users::get(&st.pg, user.id).await?;
    Ok(Verdict::SignedIn {
        user,
        created: false,
        joined: Vec::new(),
    })
}

/// Create the account a checked code was sent to, and sign it in.
///
/// The door is checked again here, because it can have closed in the ten
/// minutes since the code was sent: registration closed, or the invitation it
/// relied on withdrawn. A new account that has invitations waiting joins those
/// organisations instead of getting a workspace of its own: it was invited
/// somewhere, and an empty workspace beside that is clutter.
async fn create_account(
    st: &AppState,
    key: &str,
    seen: Snapshot<CodeRecord>,
    email: &str,
    asked: NewAccount,
) -> Result<Verdict> {
    let account = asked.or(&seen.value.account);
    let invites = pending_invites_for(&st.kv, &st.cfg, email).await?;

    // The first account owns the instance (`routes::admin`), so deciding
    // "first" and creating it are one step under an advisory lock: two people
    // racing to claim a fresh install cannot both see an empty table. Only the
    // fresh-install path pays for the lock.
    let maybe_first = users::count(&st.pg).await? == 0;
    let first_account_lock = if maybe_first {
        Some(lock_first_account(&st.pg).await?)
    } else {
        None
    };
    let is_first_account = first_account_lock.is_some() && users::count(&st.pg).await? == 0;

    if door(
        false,
        is_first_account,
        !st.cfg.disable_registration,
        !invites.is_empty(),
    )
    .is_none()
    {
        return Ok(Verdict::Refused(CODE_REFUSED));
    }

    if !st.kv.swap_json(key, &seen, None).await? {
        return Ok(Verdict::Refused(CODE_REFUSED));
    }

    let name = account.name.unwrap_or_else(|| default_name(email));
    let user = match users::create(&st.pg, new_id(), email, &name).await {
        Ok(user) => user,
        // The address gained an account between the lookup and the insert. The
        // code that would have made it is the one just consumed, so this was
        // made some other way; refusing is better than guessing whose it is.
        Err(Error::Conflict(_)) => return Ok(Verdict::Refused(CODE_REFUSED)),
        Err(e) => return Err(e),
    };
    users::mark_email_verified(&st.pg, user.id).await?;

    let mut joined = Vec::new();
    if invites.is_empty() {
        provision_first_organization(&st.pg, &user, account.org_name.as_deref()).await?;
    } else {
        for invite in &invites {
            redeem_invite(&st.pg, &st.kv, invite, user.id).await?;
            joined.push(invite.org_id);
        }
    }

    if is_first_account {
        tracing::info!(user = %user.id, "first account created; it owns this install");
    }

    // Released only once the first account exists, so whoever waited on the
    // lock counts it.
    if let Some(lock) = first_account_lock {
        lock.commit().await?;
    }

    let user = users::get(&st.pg, user.id).await?;
    Ok(Verdict::SignedIn {
        user,
        created: true,
        joined,
    })
}

/// Count one wrong guess against the code `seen` read, deleting the code when
/// that was the last guess it survives.
///
/// A swap that loses a race is retried against what won it, as long as the key
/// still holds the same code: two wrong guesses in parallel are two wrong
/// guesses. When the key now holds a different code (a new request replaced
/// it) or none, the guess was made against a code that no longer exists and
/// there is nothing to count it on; the per-address window has counted it.
async fn record_miss(kv: &Kv, key: &str, mut seen: Snapshot<CodeRecord>) -> Result<()> {
    for _ in 0..SWAP_RETRIES {
        let next = seen.value.after_miss();
        if kv.swap_json(key, &seen, next.as_ref()).await? {
            return Ok(());
        }
        match kv.get_snapshot::<CodeRecord>(key).await? {
            // Two stored digests, not a guess against one, so `==` is fine.
            Some(current) if current.value.digest == seen.value.digest => seen = current,
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// The digits of a typed or pasted code, or `None` when it cannot be one.
///
/// Spaces and dashes are dropped, because a code copied out of an email or
/// read aloud often brings them along (`482 913`, `482-913`). Anything else
/// that is not an ASCII digit means it is not a code.
fn normalise_code(raw: &str) -> Option<String> {
    let digits: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (digits.len() == CODE_DIGITS as usize && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some(digits)
}

/// The refusal an exhausted window answers with.
async fn limit(kv: &Kv, key: &str, max: u32, window: u32) -> Result<()> {
    if kv.rate_limit(key, max, window).await? {
        Ok(())
    } else {
        Err(Error::RateLimited {
            retry_after: u64::from(window),
        })
    }
}

/// Take the first-account lock, held until the returned transaction ends.
///
/// A transaction-scoped advisory lock rather than a session one, so every way
/// out of the creation (an early `?`, a panic, a dropped connection) releases
/// it by rolling the transaction back. A session lock left on a pooled
/// connection would block every account creation after it.
async fn lock_first_account(pg: &PgPool) -> Result<sqlx::Transaction<'static, sqlx::Postgres>> {
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
    pg: &PgPool,
    user: &users::User,
    requested: Option<&str>,
) -> Result<()> {
    let name = match requested {
        Some(raw) if !raw.trim().is_empty() => clean_name(raw, "org_name")?,
        _ => format!("{}'s workspace", user.name),
    };
    let slug = unique_slug(pg, &name).await?;
    let org = orgs::create(pg, new_id(), &name, &slug, user.id).await?;
    orgs::add_member(pg, org.id, user.id, OWNER).await?;
    Ok(())
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
// Two-factor
// ---------------------------------------------------------------------------

/// `POST /api/auth/totp/enroll` — start enrolment.
///
/// Asks for the session and nothing else: adding a second factor makes the
/// account harder to take, never easier. The seed is held in Redis and **not**
/// written to the user's row until a code proves the authenticator actually has
/// it. Writing it immediately would lock somebody out of their own account if
/// they closed the tab before scanning.
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
///
/// Deliberately not `Debug`, for the same reason as [`CodeVerify`].
#[derive(Deserialize)]
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

/// `POST /api/auth/totp/disable` — turn two-factor off, with a current code
/// from the authenticator.
///
/// The session alone is not enough: a browser left signed in on a shared
/// machine must not be a way to remove the second factor, and there is no
/// password to ask for instead. Attempts are limited per account
/// ([`TOTP_DISABLE_LIMIT`]) for the same reason.
///
/// Somebody who has lost the authenticator cannot turn it off from here. On a
/// self-hosted install the operator clears it in Postgres
/// (`UPDATE users SET totp_secret_sealed = NULL WHERE email = '…'`), and the
/// next code sign-in needs no authenticator; `docs/self-hosting` says so.
async fn totp_disable(
    State(st): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TotpCodeBody>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let Some(sealed) = auth.user.totp_secret_sealed.as_deref() else {
        return Ok(ok(json!({ "status": "disabled" })));
    };

    limit(
        &st.kv,
        &format!("auth-totp-disable:{}", auth.user.id),
        TOTP_DISABLE_LIMIT,
        TOTP_DISABLE_WINDOW,
    )
    .await?;

    if !verify_totp_secret(&st.cfg, &auth.user, sealed, body.code.trim())? {
        return Err(Error::Unauthorized(TOTP_REFUSED.into()).into());
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

/// A salted digest of a client address, never the address.
///
/// Rate-limit keys and the `sessions_auth.ip_hash` column both take this form,
/// for the same reason `visitor_id` does: distinguishing two devices needs a
/// hash, and nothing in the product needs the address itself. The address is
/// the one [`ClientAddr`] resolved by the trusted-proxy rule, never a header
/// read here.
fn ip_digest(cfg: &Config, ip: &IpAddr) -> String {
    crypto::visitor_hash(&cfg.derive_key("session-ip"), 0, &ip.to_string(), "")
}

/// A keyed digest of an email address, never the address.
///
/// Every Redis key that is about one address (its sign-in code, its rate-limit
/// windows, the invitations waiting for it) is built from this, so a Redis
/// dump does not list which addresses have been signing in, and cannot be
/// checked against a list of known addresses without
/// `MICAFORGE_SECRET_KEY_BASE`. The key exists identically for an address with
/// no account.
pub(crate) fn address_digest(cfg: &Config, email: &str) -> String {
    crypto::keyed_digest(
        &cfg.derive_key("address"),
        &[email.trim().to_lowercase().as_str()],
    )
}

/// `mf:auth:code:<digest of the address>` — one address's live code.
///
/// The key depends on the address alone, never on the code, which is what
/// makes a new request replace the previous code rather than sit beside it.
fn code_key(cfg: &Config, email: &str) -> String {
    format!("mf:auth:code:{}", address_digest(cfg, email))
}

/// The stored form of a code: an HMAC over the address and the code.
///
/// The address is in it so a digest cannot be lifted from one address's record
/// and planted in another's.
fn code_digest(cfg: &Config, email: &str, code: &str) -> String {
    crypto::keyed_digest(&cfg.derive_key("sign-in-code"), &[email, code])
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

/// One user on the wire. Never carries the TOTP seed.
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

/// `mf:auth:totp:<user>` — an enrolment that has not been confirmed yet.
fn totp_pending_key(user_id: Uuid) -> String {
    format!("mf:auth:totp:{user_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_may_be_sent_a_code() {
        // (has_account, no_accounts_yet, registration_open, invited)
        assert_eq!(door(true, false, false, false), Some(Door::Account));
        assert_eq!(
            door(false, true, false, false),
            Some(Door::FirstAccount),
            "a closed install still admits its first account"
        );
        assert_eq!(
            door(false, false, true, false),
            Some(Door::OpenRegistration)
        );
        assert_eq!(
            door(false, false, false, true),
            Some(Door::Invitation),
            "an invitation opens a closed install to the address it was sent to"
        );
        // Closed, not the first, not invited: nothing is sent.
        assert_eq!(door(false, false, false, false), None);
        // An existing account signs in whatever registration says.
        assert_eq!(door(true, false, false, true), Some(Door::Account));
    }

    #[test]
    fn a_code_is_six_digits_and_pasted_spacing_is_forgiven() {
        assert_eq!(normalise_code("048213").as_deref(), Some("048213"));
        assert_eq!(normalise_code(" 048 213 ").as_deref(), Some("048213"));
        assert_eq!(normalise_code("048-213").as_deref(), Some("048213"));
        assert_eq!(normalise_code("04821"), None);
        assert_eq!(normalise_code("0482134"), None);
        assert_eq!(normalise_code("04821a"), None);
        assert_eq!(normalise_code("٠٤٨٢١٣"), None, "only ASCII digits");
        assert_eq!(normalise_code(""), None);
    }

    #[test]
    fn a_code_key_names_the_address_alone_and_never_in_the_clear() {
        let cfg = test_config();
        let key = code_key(&cfg, "Owner@Example.com");
        assert!(!key.contains('@'));
        assert!(!key.to_lowercase().contains("owner"));
        // Case and spacing land on one key, so retries replace rather than add.
        assert_eq!(key, code_key(&cfg, "  owner@example.com "));
        assert_ne!(key, code_key(&cfg, "other@example.com"));
        // Keyed: not the plain digest anyone could recompute from a list of
        // addresses, and different under another install's secret.
        assert!(!key.contains(&crypto::token_hash("owner@example.com")));
        let mut other = test_config();
        other.secret_key_base = "y".repeat(48);
        assert_ne!(key, code_key(&other, "owner@example.com"));
    }

    #[test]
    fn a_new_request_replaces_the_code_rather_than_adding_one() {
        // Two codes for one address are written to one key, so the second
        // overwrites the first and only the latest digest is ever compared.
        let cfg = test_config();
        let first = crypto::numeric_code(CODE_DIGITS);
        let second = crypto::numeric_code(CODE_DIGITS);
        assert_eq!(
            code_key(&cfg, "ada@example.com"),
            code_key(&cfg, "ada@example.com")
        );
        let stored_after_second = code_digest(&cfg, "ada@example.com", &second);
        assert_eq!(
            crypto::digests_match(
                &stored_after_second,
                &code_digest(&cfg, "ada@example.com", &first)
            ),
            first == second
        );
    }

    #[test]
    fn a_code_digest_is_bound_to_its_address_and_keyed() {
        let cfg = test_config();
        let digest = code_digest(&cfg, "ada@example.com", "048213");
        assert!(crypto::digests_match(
            &digest,
            &code_digest(&cfg, "ada@example.com", "048213")
        ));
        assert!(!crypto::digests_match(
            &digest,
            &code_digest(&cfg, "ada@example.com", "048214")
        ));
        // The same code for another address is another digest, so a record
        // cannot be lifted from one address and planted on another.
        assert!(!crypto::digests_match(
            &digest,
            &code_digest(&cfg, "bob@example.com", "048213")
        ));
        assert!(!digest.contains("048213"));
        assert_ne!(digest, crypto::token_hash("048213"));
    }

    #[test]
    fn the_fifth_wrong_guess_deletes_the_code() {
        let mut record = CodeRecord {
            digest: "d".into(),
            misses: 0,
            account: NewAccount::default(),
        };
        for expected in 1..CODE_MAX_MISSES {
            record = record.after_miss().expect("the code survives this guess");
            assert_eq!(record.misses, expected);
            assert_eq!(record.digest, "d", "a miss changes nothing else");
        }
        assert_eq!(record.misses, CODE_MAX_MISSES - 1);
        assert!(record.after_miss().is_none(), "the fifth miss deletes it");
        assert_eq!(CODE_MAX_MISSES, 5);

        // A corrupt counter cannot wrap round into a fresh allowance.
        let corrupt = CodeRecord {
            misses: u32::MAX,
            ..record
        };
        assert!(corrupt.after_miss().is_none());
    }

    #[test]
    fn an_old_record_without_a_counter_still_reads() {
        let old: CodeRecord = serde_json::from_str(r#"{"digest":"d"}"#).unwrap();
        assert_eq!(old.misses, 0);
        assert_eq!(old.account, NewAccount::default());
    }

    #[test]
    fn a_missing_authenticator_code_consumes_and_counts_nothing() {
        let never = |_: &str| -> Result<bool> { panic!("no code to check") };
        assert_eq!(
            second_factor(true, None, never).unwrap(),
            SecondFactor::Missing
        );
        assert_eq!(
            second_factor(true, Some("   "), never).unwrap(),
            SecondFactor::Missing
        );
        assert_eq!(
            second_factor(false, None, never).unwrap(),
            SecondFactor::Passed
        );
        assert_eq!(
            second_factor(true, Some(" 123456 "), |c| Ok(c == "123456")).unwrap(),
            SecondFactor::Passed
        );
        assert_eq!(
            second_factor(true, Some("000000"), |_| Ok(false)).unwrap(),
            SecondFactor::Wrong
        );
    }

    #[test]
    fn what_verification_sends_wins_over_what_the_request_stored() {
        let stored = NewAccount::from_request(Some("Ada"), Some("Engines")).unwrap();
        let sent = NewAccount::from_request(Some("  Ada Lovelace "), Some(" ")).unwrap();
        let merged = sent.or(&stored);
        assert_eq!(merged.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(merged.org_name.as_deref(), Some("Engines"));
        assert!(NewAccount::from_request(Some(&"x".repeat(500)), None).is_err());
        assert_eq!(
            NewAccount::from_request(None, None).unwrap(),
            NewAccount::default()
        );
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
            max_sites_per_org: 0,
            max_orgs_per_user: 0,
            self_hosted: true,
            smtp_url: None,
            cloudflare_email_token: None,
            cloudflare_account_id: None,
            mail_from: "Micaforge <no-reply@localhost>".into(),
            log_format: "text".into(),
            worker_concurrency: 2,
        }
    }

    /// The whole flow against a real Postgres and a real Valkey.
    ///
    /// The properties that matter most here (single use, replacement, the
    /// attempt cap, the first-account lock, the atomic swap) live in the
    /// stores, so a test without them would be testing a model of them. Opt in
    /// with
    ///
    /// ```text
    /// MICAFORGE_TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/postgres \
    /// MICAFORGE_TEST_REDIS_URL=redis://127.0.0.1:6379 \
    /// cargo test -p micaforge-server -- --ignored
    /// ```
    ///
    /// Each test creates a database of its own on that server (the role needs
    /// `CREATEDB`), migrates it, and drops it at the end, so nothing that is
    /// already there is touched. Redis keys are per address and every test
    /// uses fresh addresses and client addresses.
    mod live {
        use std::net::Ipv4Addr;

        use micaforge_agents::verify::RangeStore;
        use micaforge_core::config::MailTransport;
        use micaforge_enrich::Geo;
        use micaforge_mail::Mailer;
        use micaforge_store::{Ch, EventWriter, WriterCfg};
        use tokio::sync::RwLock;

        use super::*;
        use crate::extract::AuthMethod;

        /// One test's stores, and the database to drop afterwards.
        struct Harness {
            st: AppState,
            admin_url: String,
            database: String,
        }

        impl Harness {
            async fn new(disable_registration: bool) -> Harness {
                let admin_url = std::env::var("MICAFORGE_TEST_DATABASE_URL")
                    .expect("set MICAFORGE_TEST_DATABASE_URL to run this test");
                let redis_url = std::env::var("MICAFORGE_TEST_REDIS_URL")
                    .expect("set MICAFORGE_TEST_REDIS_URL to run this test");

                let database = format!("mf_test_{}", crypto::numeric_code(9));
                let admin = micaforge_store::pg::connect(&admin_url).await.unwrap();
                sqlx::query(&format!("CREATE DATABASE {database}"))
                    .execute(&admin)
                    .await
                    .unwrap();
                admin.close().await;

                let (server, _) = admin_url.rsplit_once('/').unwrap();
                let pg = micaforge_store::pg::connect(&format!("{server}/{database}"))
                    .await
                    .unwrap();
                micaforge_store::pg::migrate(&pg).await.unwrap();
                let kv = Kv::connect(&redis_url).await.unwrap();

                let mut cfg = test_config();
                cfg.disable_registration = disable_registration;
                cfg.secret_key_base = crypto::random_token(48);
                let ch = Ch::connect("http://127.0.0.1:9", "micaforge", "default", "").unwrap();
                let mailer =
                    Mailer::new(MailTransport::Log, &cfg.mail_from, &cfg.dashboard_url).unwrap();

                let st = AppState {
                    cfg: Arc::new(cfg),
                    pg,
                    writer: EventWriter::spawn(ch.clone(), WriterCfg::default()),
                    ch,
                    kv,
                    geo: Arc::new(Geo::open(None, None)),
                    ranges: Arc::new(RwLock::new(RangeStore::new())),
                    mailer: Arc::new(mailer),
                    http: reqwest::Client::new(),
                };
                Harness {
                    st,
                    admin_url,
                    database,
                }
            }

            async fn finish(self) {
                self.st.pg.close().await;
                let admin = micaforge_store::pg::connect(&self.admin_url).await.unwrap();
                let _ = sqlx::query(&format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    self.database
                ))
                .execute(&admin)
                .await;
            }

            /// Mint a code the way the operator command does.
            async fn issue(&self, email: &str) -> Option<String> {
                issue_sign_in_code(
                    &self.st.cfg,
                    &self.st.pg,
                    &self.st.kv,
                    email,
                    NewAccount::default(),
                )
                .await
                .unwrap()
                .map(|issued| issued.code)
            }

            /// `POST /api/auth/code`, from `ip`.
            async fn request(&self, email: &str, ip: IpAddr) -> ApiResult<Json<Value>> {
                request_code(
                    State(self.st.clone()),
                    ClientAddr(ip),
                    Json(CodeRequest {
                        email: email.into(),
                        name: None,
                        org_name: None,
                    }),
                )
                .await
            }

            /// `POST /api/auth/code/verify`, from `ip`.
            async fn verify(
                &self,
                email: &str,
                code: &str,
                totp: Option<&str>,
                ip: IpAddr,
            ) -> ApiResult<(CookieJar, Json<Value>)> {
                verify_code(
                    State(self.st.clone()),
                    ClientAddr(ip),
                    HeaderMap::new(),
                    CookieJar::new(),
                    Json(CodeVerify {
                        email: email.into(),
                        code: code.into(),
                        totp: totp.map(str::to_string),
                        name: Some("Test Person".into()),
                        org_name: None,
                    }),
                )
                .await
            }

            /// Create the account for `email` (or sign it in), the way a person
            /// would, and answer what verification answered.
            async fn sign_up(&self, email: &str) -> Value {
                let code = self.issue(email).await.expect("the address may sign in");
                let (_, body) = self.verify(email, &code, None, client()).await.unwrap();
                body.0["data"].clone()
            }

            /// The live code record for an address, if there is one.
            async fn record(&self, email: &str) -> Option<CodeRecord> {
                self.st
                    .kv
                    .get_json(&code_key(&self.st.cfg, email))
                    .await
                    .unwrap()
            }

            /// A session for an existing account, as the extractor would build it.
            async fn signed_in(&self, email: &str) -> AuthUser {
                let user = users::find_by_email(&self.st.pg, email)
                    .await
                    .unwrap()
                    .unwrap();
                AuthUser {
                    user,
                    method: AuthMethod::Session(Uuid::nil()),
                    scopes: Vec::new(),
                    key: None,
                }
            }
        }

        /// An address nobody else in this run uses.
        fn fresh(name: &str) -> String {
            format!("{name}.{}@example.com", crypto::numeric_code(9))
        }

        /// A client address nobody else in this run uses.
        fn client() -> IpAddr {
            let n: u32 = crypto::numeric_code(9).parse().unwrap();
            IpAddr::V4(Ipv4Addr::from(0x0a00_0000 | (n & 0x00ff_ffff)))
        }

        /// A code that is not `code`.
        fn not(code: &str) -> String {
            let flipped = (code.as_bytes()[5] - b'0' + 1) % 10;
            format!("{}{flipped}", &code[..5])
        }

        fn status(e: &ApiError) -> u16 {
            e.inner().status()
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn the_first_account_signs_up_by_code_and_owns_the_install() {
            let h = Harness::new(true).await;
            let owner = fresh("owner");
            let ip = client();

            // The answer is the same body for an address that gets a code.
            let answer = h.request(&owner, ip).await.unwrap();
            assert_eq!(answer.0["data"]["status"], "ok");
            assert_eq!(answer.0["data"]["mail_delivered"], false);
            assert!(h.record(&owner).await.is_some(), "a code was stored");

            let code = h
                .issue(&owner)
                .await
                .expect("the first account may sign up");
            let refused = h.verify(&owner, &not(&code), None, ip).await.unwrap_err();
            assert_eq!(status(&refused), 401);
            assert!(refused.inner().to_string().contains(CODE_REFUSED));

            let (jar, body) = h.verify(&owner, &code, None, ip).await.unwrap();
            let data = &body.0["data"];
            assert_eq!(data["status"], "ok");
            assert_eq!(data["created"], true);
            assert_eq!(data["user"]["email"], owner.as_str());
            assert_eq!(data["user"]["name"], "Test Person");
            assert_eq!(data["user"]["email_verified"], true);
            assert!(jar.get(SESSION_COOKIE).is_some(), "a session was opened");

            let user = users::find_by_email(&h.st.pg, &owner)
                .await
                .unwrap()
                .unwrap();
            assert!(crate::routes::admin::is_instance_admin(&h.st, &user)
                .await
                .unwrap());
            let orgs = orgs::list_for_user(&h.st.pg, user.id).await.unwrap();
            assert_eq!(orgs.len(), 1, "a workspace to put sites in");

            // Single use: the code that just worked does not work again.
            let again = h.verify(&owner, &code, None, ip).await.unwrap_err();
            assert_eq!(status(&again), 401);
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn closed_registration_sends_an_unknown_address_nothing() {
            let h = Harness::new(true).await;
            let owner = fresh("owner");
            assert_eq!(h.sign_up(&owner).await["created"], true);

            let stranger = fresh("stranger");
            let ip = client();
            let started = Instant::now();
            let answer = h.request(&stranger, ip).await.unwrap();
            // The same body, and no faster than the floor.
            assert_eq!(answer.0["data"]["status"], "ok");
            assert!(started.elapsed().as_millis() >= u128::from(AUTH_FLOOR_MS));
            assert!(h.record(&stranger).await.is_none(), "nothing was stored");
            assert!(h.issue(&stranger).await.is_none());

            let refused = h.verify(&stranger, "123456", None, ip).await.unwrap_err();
            assert_eq!(status(&refused), 401);
            assert!(refused.inner().to_string().contains(CODE_REFUSED));

            // The account that exists still signs in on a closed install.
            assert_eq!(h.sign_up(&owner).await["created"], false);
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn an_invitation_lets_its_address_sign_up_on_a_closed_install() {
            let h = Harness::new(true).await;
            let owner = fresh("owner");
            assert_eq!(h.sign_up(&owner).await["created"], true);
            let auth = h.signed_in(&owner).await;
            let org = orgs::list_for_user(&h.st.pg, auth.user.id).await.unwrap()[0].clone();

            let invitee = fresh("invitee");
            assert!(
                h.issue(&invitee).await.is_none(),
                "closed before the invite"
            );
            let invitation =
                crate::routes::orgs::tests_support::invite(&h.st, auth, org.id, &invitee).await;
            assert!(invitation["link"]
                .as_str()
                .unwrap()
                .contains("/app/invitation?token="));

            let code = h
                .issue(&invitee)
                .await
                .expect("the invitation opens the door");
            let (_, body) = h.verify(&invitee, &code, None, client()).await.unwrap();
            let data = &body.0["data"];
            assert_eq!(data["created"], true);
            assert_eq!(data["joined"], json!([org.id]));

            let user = users::find_by_email(&h.st.pg, &invitee)
                .await
                .unwrap()
                .unwrap();
            let member = orgs::get_member(&h.st.pg, org.id, user.id).await.unwrap();
            assert_eq!(member.map(|m| m.role).as_deref(), Some("member"));
            assert_eq!(
                orgs::list_for_user(&h.st.pg, user.id).await.unwrap().len(),
                1,
                "joined the inviting organisation, and no empty workspace beside it"
            );
            // The invitation is used up, so it no longer opens anything.
            assert!(pending_invites_for(&h.st.kv, &h.st.cfg, &invitee)
                .await
                .unwrap()
                .is_empty());
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn only_the_latest_code_works() {
            let h = Harness::new(false).await;
            let email = fresh("ada");
            let first = h.issue(&email).await.unwrap();
            let second = h.issue(&email).await.unwrap();
            let ip = client();
            if first != second {
                let refused = h.verify(&email, &first, None, ip).await.unwrap_err();
                assert_eq!(status(&refused), 401);
            }
            let (_, body) = h.verify(&email, &second, None, ip).await.unwrap();
            assert_eq!(body.0["data"]["status"], "ok");
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn a_printed_code_keeps_the_profile_the_request_named() {
            let h = Harness::new(true).await;
            let email = fresh("first");
            let ip = client();
            // The sign-in screen asks, naming the person and the workspace...
            let answer = request_code(
                State(h.st.clone()),
                ClientAddr(ip),
                Json(CodeRequest {
                    email: email.clone(),
                    name: Some("Ada Lovelace".into()),
                    org_name: Some("Analytical Engines".into()),
                }),
            )
            .await
            .unwrap();
            assert_eq!(answer.0["data"]["mail_delivered"], false);

            // ...the operator prints the code that works, which names nobody...
            let code = h.issue(&email).await.unwrap();

            // ...and the verification, which names nobody either, still uses them.
            let (_, body) = verify_code(
                State(h.st.clone()),
                ClientAddr(ip),
                HeaderMap::new(),
                CookieJar::new(),
                Json(CodeVerify {
                    email: email.clone(),
                    code,
                    totp: None,
                    name: None,
                    org_name: None,
                }),
            )
            .await
            .unwrap();
            assert_eq!(body.0["data"]["user"]["name"], "Ada Lovelace");
            let user = users::find_by_email(&h.st.pg, &email)
                .await
                .unwrap()
                .unwrap();
            let orgs = orgs::list_for_user(&h.st.pg, user.id).await.unwrap();
            assert_eq!(orgs[0].name, "Analytical Engines");
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn five_wrong_guesses_delete_the_code() {
            let h = Harness::new(false).await;
            let email = fresh("ada");
            let code = h.issue(&email).await.unwrap();
            let ip = client();
            for expected in 1..CODE_MAX_MISSES {
                h.verify(&email, &not(&code), None, ip).await.unwrap_err();
                assert_eq!(h.record(&email).await.unwrap().misses, expected);
            }
            h.verify(&email, &not(&code), None, ip).await.unwrap_err();
            assert!(
                h.record(&email).await.is_none(),
                "the fifth miss deleted it"
            );

            // The right code is now refused like any other.
            let refused = h.verify(&email, &code, None, ip).await.unwrap_err();
            assert_eq!(status(&refused), 401);
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn an_address_gets_ten_verifications_per_window_from_anywhere() {
            let h = Harness::new(false).await;
            let email = fresh("target");
            for _ in 0..VERIFY_ADDRESS_LIMIT {
                // A different client address every time buys nothing.
                let e = h
                    .verify(&email, "123456", None, client())
                    .await
                    .unwrap_err();
                assert_eq!(status(&e), 401);
            }
            let limited = h
                .verify(&email, "123456", None, client())
                .await
                .unwrap_err();
            assert_eq!(status(&limited), 429);

            // And three codes per window, whoever asks.
            let inbox = fresh("inbox");
            for _ in 0..REQUEST_ADDRESS_LIMIT {
                let answer = h.request(&inbox, client()).await.unwrap();
                assert_eq!(answer.0["data"]["status"], "ok");
            }
            let flooded = h.request(&inbox, client()).await.unwrap_err();
            assert_eq!(status(&flooded), 429);
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn two_factor_is_asked_for_without_spending_the_code() {
            let h = Harness::new(false).await;
            let email = fresh("guarded");
            assert_eq!(h.sign_up(&email).await["created"], true);

            // Turn two-factor on, as enrolment would.
            let user = users::find_by_email(&h.st.pg, &email)
                .await
                .unwrap()
                .unwrap();
            let secret = Secret::generate_secret().to_bytes().unwrap();
            let sealed =
                crypto::seal(&h.st.cfg.derive_key("totp"), user.id.as_bytes(), &secret).unwrap();
            users::set_totp(&h.st.pg, user.id, Some(&sealed))
                .await
                .unwrap();
            let totp = build_totp(secret).unwrap();

            let ip = client();
            let code = h.issue(&email).await.unwrap();
            let (jar, body) = h.verify(&email, &code, None, ip).await.unwrap();
            assert_eq!(body.0["data"]["status"], "totp_required");
            assert!(jar.get(SESSION_COOKIE).is_none(), "no session yet");
            let record = h.record(&email).await.expect("the code was not spent");
            assert_eq!(record.misses, 0, "and nothing was counted");

            // A wrong authenticator code counts as a wrong guess.
            let current = totp.generate_current().unwrap();
            let refused = h
                .verify(&email, &code, Some(&not(&current)), ip)
                .await
                .unwrap_err();
            assert!(refused.inner().to_string().contains(TOTP_REFUSED));
            assert_eq!(h.record(&email).await.unwrap().misses, 1);

            // The same emailed code, with the right authenticator code, signs in.
            let current = totp.generate_current().unwrap();
            let (jar, body) = h.verify(&email, &code, Some(&current), ip).await.unwrap();
            assert_eq!(body.0["data"]["status"], "ok");
            assert!(jar.get(SESSION_COOKIE).is_some());
            assert!(h.record(&email).await.is_none(), "and spent it");

            // Turning it off asks for an authenticator code, not the session alone.
            let auth = h.signed_in(&email).await;
            let wrong = totp_disable(
                State(h.st.clone()),
                auth,
                Json(TotpCodeBody {
                    code: not(&totp.generate_current().unwrap()),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(status(&wrong), 401);
            let auth = h.signed_in(&email).await;
            let disabled = totp_disable(
                State(h.st.clone()),
                auth,
                Json(TotpCodeBody {
                    code: totp.generate_current().unwrap(),
                }),
            )
            .await
            .unwrap();
            assert_eq!(disabled.0["data"]["status"], "disabled");
            assert!(!h.signed_in(&email).await.user.has_totp());
            h.finish().await;
        }

        #[tokio::test]
        #[ignore = "needs Postgres and Valkey"]
        async fn two_requests_racing_with_the_right_code_sign_in_once() {
            let h = Harness::new(false).await;
            let email = fresh("racer");
            let code = h.issue(&email).await.unwrap();
            let (a, b) = tokio::join!(
                h.verify(&email, &code, None, client()),
                h.verify(&email, &code, None, client())
            );
            assert_eq!(
                [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(),
                1,
                "exactly one of them signed in"
            );
            assert_eq!(users::count(&h.st.pg).await.unwrap(), 1);
            h.finish().await;
        }
    }
}
