//! `/api/github/*` — the GitHub App: install it, link a repository, record
//! deploys, releases and pushes as annotations, and open issues from feedback.
//!
//! Off until `MICAFORGE_GITHUB_APP_*` is set. With it unset, every route here
//! answers that the App is not configured, the webhook refuses every delivery,
//! and nothing calls GitHub. It is a GitHub *App*, separate from the OAuth app
//! behind "Continue with GitHub" (`routes::oauth_sign_in`): the two have their
//! own variables and an install may run either without the other.
//!
//! ## Install: the person proves they can reach the installation
//!
//! An organisation admin presses Connect on a site's GitHub settings:
//!
//! 1. `POST /api/github/install?site=` answers with the App's install address
//!    and a **signed state**: the organisation, the person, the site to come
//!    back to, a random nonce and an expiry, HMAC-SHA256 under a key derived
//!    from `MICAFORGE_SECRET_KEY_BASE`.
//! 2. GitHub sends the browser back to `GET /api/github/callback` with the
//!    state, the installation id and an OAuth code (the App must have "Request
//!    user authorization during installation" on). The callback refuses unless
//!    the state verifies and has not expired, its nonce has not been used
//!    (consumed once, atomically, in Valkey), the browser is signed in as the
//!    person the state names, and that person is still an admin of the
//!    organisation.
//! 3. **The installation id in that address is a claim, not a fact**: anyone
//!    can type any number into it. So the code is exchanged for the person's
//!    own GitHub token, and the installation must be one GitHub lists for them
//!    (`GET /user/installations`). Without this, anybody could attach somebody
//!    else's installation to their organisation and read its repositories.
//!    Then the App reads the installation with its own JWT and mints an
//!    installation token, which proves it can act there; only then is the
//!    installation recorded. The person's token is dropped.
//!
//! ## Link: one repository per site
//!
//! A site admin picks a repository from the installation's own list
//! (`GET /api/github/repos`, read through GitHub, so the list is what the App
//! can actually reach), and chooses which events become annotations:
//! successful deployments (to the environments named, `production` by
//! default), published releases, and pushes to the default branch.
//!
//! ## Record
//!
//! `POST /api/github/webhook` ([`webhook`]) verifies, dedupes and records.
//! The annotations it writes are ordinary annotations with `source = github`
//! and a link to the object on GitHub, so every chart that draws annotations
//! shows them with no change.
//!
//! ## Who may do what
//!
//! | action | needs |
//! |---|---|
//! | read the status, the recent activity | `viewer` on the site |
//! | connect or disconnect an installation | `admin` of the organisation |
//! | list repositories, link, change, unlink, read deliveries | `admin` on the site |
//! | open an issue from feedback | see [`issues`] |
//!
//! API keys are refused on every route here (`extract::key_route_policy`
//! names no scope for them): an agent reads deploys and opens issues through
//! the MCP tools, which apply their own rules. Every change is written to the
//! audit log; changes GitHub makes (an uninstall, a suspension, a repository
//! taken out of the installation) are written under the system actor.

pub mod client;
pub mod issues;
pub mod webhook;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::response::Redirect;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use hmac::{Hmac, Mac};
use micaforge_core::{crypto, new_id, Config, Error, Result};
use micaforge_store::models::github::{
    self as store, Delivery, Event, Installation, InstallationFields, LinkOptions, LinkRepo,
    RepoLink,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use uuid::Uuid;

use self::client::{forget_token, GitHub};
use crate::audit::{self, Entry};
use crate::error::ApiError;
use crate::extract::{AuthMethod, AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{authorize_org, effective_org_role, require_at_least, ADMIN};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// What the install state is signed under.
const STATE_PURPOSE: &str = "github-install-state";

/// How long an install may take, from Connect to the callback. Choosing
/// repositories on GitHub's page takes a minute or two; a quarter of an hour
/// leaves room without leaving a state usable for long.
const STATE_TTL_SECS: i64 = 15 * 60;

/// Most environments one link names.
const MAX_ENVIRONMENTS: usize = 20;

/// Longest environment name, in characters.
const MAX_ENVIRONMENT: usize = 100;

/// Rows the activity and delivery lists return by default, and at most.
const DEFAULT_ROWS: i64 = 20;
const MAX_ROWS: i64 = 100;

/// Every GitHub route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/github/status", get(status))
        .route("/api/github/install", post(install))
        .route("/api/github/callback", get(callback))
        .route("/api/github/installations/{id}", delete(uninstall))
        .route("/api/github/repos", get(repos))
        .route(
            "/api/github/link",
            put(link_repo).patch(change_link).delete(unlink),
        )
        .route("/api/github/events", get(events))
        .route("/api/github/deliveries", get(deliveries))
        .route("/api/github/feedback/{id}/issue", post(feedback_issue))
        // A delivery may be larger than an ordinary JSON body; the route's
        // own limit is the one a delivery meets (the innermost limit wins).
        .route(
            "/api/github/webhook",
            post(webhook::receive).layer(DefaultBodyLimit::max(webhook::BODY_LIMIT)),
        )
}

/// Refuse, at boot, a configured App whose private key cannot sign.
pub fn check_key(cfg: &Config) -> Result<()> {
    match cfg.github_app.as_ref() {
        Some(app) => client::check_key(app),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// The install state
// ---------------------------------------------------------------------------

/// What an install carries to GitHub and back. Short field names: it rides
/// in a URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallState {
    /// The organisation the installation is for.
    pub o: Uuid,
    /// The person who started it.
    pub u: Uuid,
    /// The site whose settings to come back to.
    pub s: u16,
    /// Single-use nonce.
    pub n: String,
    /// Expiry, unix seconds.
    pub e: i64,
}

/// `base64url(json) . base64url(hmac-sha256(key, base64url(json)))`.
pub fn sign_state(key: &[u8; 32], state: &InstallState) -> Result<String> {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(state)?);
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|_| Error::internal("the state key was refused"))?;
    mac.update(payload.as_bytes());
    let tag = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{payload}.{tag}"))
}

/// The state a token carries, when its signature verifies (in constant time)
/// and it has not expired at `now`. One answer, `None`, for every failure.
pub fn read_state(key: &[u8; 32], token: &str, now: i64) -> Option<InstallState> {
    let (payload, tag) = token.split_once('.')?;
    let tag = URL_SAFE_NO_PAD.decode(tag).ok()?;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).ok()?;
    mac.update(payload.as_bytes());
    mac.verify_slice(&tag).ok()?;
    let state: InstallState =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    (state.e > now).then_some(state)
}

/// Use a state's nonce. True the first time only, across every process.
async fn consume_nonce(st: &AppState, nonce: &str) -> Result<bool> {
    let key = format!("mf:github:state:{nonce}");
    Ok(st.kv.count(&key, 1, STATE_TTL_SECS as u32 + 60).await? == 1)
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// `GET /api/github/status?site=`
async fn status(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    let can_link = require_at_least(&access.role, ADMIN).is_ok();
    let can_install = match effective_org_role(&st.pg, access.site.org_id, auth.user.id).await {
        Ok(role) => require_at_least(&role, ADMIN).is_ok(),
        Err(_) => false,
    };
    let link = store::link_for_site(&st.pg, access.site.id).await?;
    let installations = if can_link || can_install {
        store::installations_for_org(&st.pg, access.site.org_id).await?
    } else {
        Vec::new()
    };
    let public = st.cfg.public_url.trim_end_matches('/');
    Ok(ok(json!({
        "configured": st.cfg.github_app.is_some(),
        "app": st.cfg.github_app.as_ref().map(|app| json!({
            "slug": app.slug,
            "url": format!("{}/apps/{}", client::Endpoints::github_com().web, app.slug),
        })),
        "webhook_url": format!("{public}/api/github/webhook"),
        "callback_url": format!("{public}/api/github/callback"),
        "can_install": can_install,
        "can_link": can_link,
        "installations": installations.iter().map(installation_json).collect::<Vec<_>>(),
        "link": link.as_ref().map(link_json),
    })))
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// `POST /api/github/install?site=`: the address to send the browser to.
async fn install(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let github = GitHub::of(&st)?;
    authorize_org(&st.pg, access.site.org_id, auth.user.id, ADMIN).await?;
    let state = InstallState {
        o: access.site.org_id,
        u: auth.user.id,
        s: access.site_id,
        n: crypto::random_token(16),
        e: Utc::now().timestamp() + STATE_TTL_SECS,
    };
    let token = sign_state(&st.cfg.derive_key(STATE_PURPOSE), &state)?;
    Ok(ok(json!({
        "url": github.install_url(&token),
        "expires_in": STATE_TTL_SECS,
    })))
}

/// What GitHub sends the browser back with.
#[derive(Debug, Default, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    installation_id: Option<String>,
    #[serde(default)]
    setup_action: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Why an install came back without a connection. The code is all the
/// browser is told; the settings screen turns it into a sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// The state was missing, forged, expired or used.
    State,
    /// The browser is signed out, or signed in as somebody else.
    SignedOut,
    /// The person cancelled on GitHub.
    Denied,
    /// The person is no longer an admin of the organisation.
    Forbidden,
    /// GitHub does not list the installation for this person.
    NotYours,
    /// GitHub answered badly, or not at all.
    GitHub,
    /// The App is not configured here.
    Disabled,
}

impl Refusal {
    fn code(self) -> &'static str {
        match self {
            Refusal::State => "state",
            Refusal::SignedOut => "signed_out",
            Refusal::Denied => "denied",
            Refusal::Forbidden => "forbidden",
            Refusal::NotYours => "not_yours",
            Refusal::GitHub => "github",
            Refusal::Disabled => "disabled",
        }
    }
}

/// `GET /api/github/callback`
async fn callback(
    State(st): State<AppState>,
    auth: std::result::Result<AuthUser, ApiError>,
    Query(q): Query<CallbackQuery>,
) -> Redirect {
    let dashboard = st.cfg.dashboard_url.trim_end_matches('/').to_string();
    let key = st.cfg.derive_key(STATE_PURPOSE);
    let Some(state) = q
        .state
        .as_deref()
        .and_then(|token| read_state(&key, token, Utc::now().timestamp()))
    else {
        return Redirect::to(&format!("{dashboard}/app?github_error=state"));
    };
    let back = format!("{dashboard}/app/{}/settings/github", state.s);

    match connect(&st, auth.ok().as_ref(), &q, &state).await {
        Ok(Connected::Installed) => Redirect::to(&format!("{back}?github=installed")),
        Ok(Connected::Requested) => Redirect::to(&format!("{back}?github=requested")),
        Err(refusal) => {
            tracing::info!(
                reason = refusal.code(),
                "a GitHub App install came back unconnected"
            );
            Redirect::to(&format!("{back}?github_error={}", refusal.code()))
        }
    }
}

/// How a callback ended, short of a refusal.
enum Connected {
    /// The installation is recorded for the organisation.
    Installed,
    /// The person asked an owner on GitHub to approve the install.
    Requested,
}

/// Everything between GitHub's redirect and a recorded installation. See
/// "Install" in the module notes for why each check is here.
async fn connect(
    st: &AppState,
    auth: Option<&AuthUser>,
    q: &CallbackQuery,
    state: &InstallState,
) -> std::result::Result<Connected, Refusal> {
    let github = GitHub::of(st).map_err(|_| Refusal::Disabled)?;
    let auth = match auth {
        Some(auth) if matches!(auth.method, AuthMethod::Session(_)) => auth,
        _ => return Err(Refusal::SignedOut),
    };
    if auth.user.id != state.u {
        return Err(Refusal::SignedOut);
    }
    if q.error.is_some() {
        return Err(Refusal::Denied);
    }
    if !consume_nonce(st, &state.n)
        .await
        .map_err(|_| Refusal::State)?
    {
        return Err(Refusal::State);
    }
    if q.setup_action.as_deref() == Some("request") {
        return Ok(Connected::Requested);
    }
    authorize_org(&st.pg, state.o, auth.user.id, ADMIN)
        .await
        .map_err(|_| Refusal::Forbidden)?;

    let installation_id = q
        .installation_id
        .as_deref()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or(Refusal::GitHub)?;
    let code = q
        .code
        .as_deref()
        .filter(|c| !c.is_empty())
        .ok_or(Refusal::GitHub)?;

    let warn = |step: &'static str| {
        move |e: Error| {
            tracing::warn!(step, error = %e, "a GitHub App install step failed");
            Refusal::GitHub
        }
    };
    let person = github.user_token(code).await.map_err(warn("code"))?;
    let reachable = github
        .user_installation_ids(&person)
        .await
        .map_err(warn("user installations"))?;
    drop(person);
    if !reachable.contains(&installation_id) {
        return Err(Refusal::NotYours);
    }
    let info = github
        .installation(installation_id)
        .await
        .map_err(warn("installation"))?;
    github
        .installation_token(&st.kv, installation_id)
        .await
        .map_err(warn("installation token"))?;

    let fields = InstallationFields {
        installation_id,
        account_login: &info.account_login,
        account_type: &info.account_type,
        repository_selection: info.repository_selection.as_deref(),
        suspended_at: info.suspended_at,
    };
    let row = store::upsert_installation(&st.pg, new_id(), state.o, &fields, Some(auth.user.id))
        .await
        .map_err(warn("record"))?;
    audit::record(
        st,
        auth,
        Entry::new("github.app_installed")
            .org(row.org_id)
            .target(
                "github_installation",
                row.installation_id,
                &row.account_login,
            )
            .after(installation_json(&row)),
    )
    .await;
    Ok(Connected::Installed)
}

/// `DELETE /api/github/installations/{id}`: disconnect an installation from
/// the organisation, and uninstall the App on GitHub when no organisation on
/// this server uses it any more.
async fn uninstall(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let row = store::installation(&st.pg, id).await?;
    match authorize_org(&st.pg, row.org_id, auth.user.id, ADMIN).await {
        Ok(_) => {}
        Err(Error::NotFound(_)) => return Err(Error::not_found("GitHub installation").into()),
        Err(e) => return Err(e.into()),
    }
    store::delete_installation(&st.pg, row.id).await?;

    let mut uninstalled = false;
    if store::organisations_using(&st.pg, row.installation_id).await? == 0 {
        if let Ok(github) = GitHub::of(&st) {
            match github.delete_installation(row.installation_id).await {
                Ok(()) => uninstalled = true,
                Err(e) => tracing::warn!(
                    error = %e,
                    "the GitHub App could not be uninstalled on GitHub; it was disconnected here"
                ),
            }
        }
        if let Err(e) = forget_token(&st.kv, row.installation_id).await {
            tracing::warn!(error = %e, "a removed installation's token could not be forgotten");
        }
    }

    audit::record(
        &st,
        &auth,
        Entry::new("github.app_uninstalled")
            .org(row.org_id)
            .target(
                "github_installation",
                row.installation_id,
                &row.account_login,
            )
            .before(installation_json(&row))
            .meta(json!({ "uninstalled_on_github": uninstalled })),
    )
    .await;
    Ok(ok(json!({
        "removed": true,
        "uninstalled_on_github": uninstalled,
    })))
}

// ---------------------------------------------------------------------------
// Repositories and the link
// ---------------------------------------------------------------------------

/// The installation `id`, when it belongs to the site's organisation.
async fn installation_of_site(
    st: &AppState,
    access: &SiteAccess,
    id: Uuid,
) -> Result<Installation> {
    let row = store::installation(&st.pg, id).await?;
    if row.org_id != access.site.org_id {
        return Err(Error::not_found("GitHub installation"));
    }
    if row.suspended_at.is_some() {
        return Err(Error::Conflict(format!(
            "the GitHub App is suspended on {}; lift the suspension on GitHub first",
            row.account_login
        )));
    }
    Ok(row)
}

/// `?installation=` for the repository list.
#[derive(Debug, Deserialize)]
struct ReposQuery {
    installation: Uuid,
}

/// `GET /api/github/repos?site=&installation=`: what the App can reach there.
async fn repos(
    State(st): State<AppState>,
    access: SiteAccess,
    Query(q): Query<ReposQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let github = GitHub::of(&st)?;
    let row = installation_of_site(&st, &access, q.installation).await?;
    let token = github
        .installation_token(&st.kv, row.installation_id)
        .await?;
    let (mut repositories, truncated) = github.installation_repos(&token).await?;
    repositories.sort_by_key(|r| r.full_name.to_lowercase());
    Ok(ok(json!({
        "repositories": repositories,
        "truncated": truncated,
    })))
}

/// The options a link body may set. Absent means "as it was" (or the default
/// for a new link).
#[derive(Debug, Default, Deserialize)]
struct OptionsBody {
    #[serde(default)]
    deploys: Option<bool>,
    #[serde(default)]
    releases: Option<bool>,
    #[serde(default)]
    pushes: Option<bool>,
    #[serde(default)]
    environments: Option<Vec<String>>,
    #[serde(default)]
    agent_issues: Option<bool>,
}

/// `PUT /api/github/link` body.
#[derive(Debug, Deserialize)]
struct LinkBody {
    installation: Uuid,
    /// `owner/name`.
    repo: String,
    #[serde(flatten)]
    options: OptionsBody,
}

/// A link's options, over `base`, with the environment list cleaned.
fn settle_options(base: LinkOptions, body: &OptionsBody) -> Result<LinkOptions> {
    let environments = match &body.environments {
        None => base.environments,
        Some(raw) => {
            let mut clean: Vec<String> = Vec::new();
            for name in raw {
                let name = name.trim();
                if name.is_empty() {
                    continue;
                }
                if name.chars().count() > MAX_ENVIRONMENT || name.chars().any(char::is_control) {
                    return Err(Error::validation(
                        "environments",
                        format!(
                            "each environment is a name of at most {MAX_ENVIRONMENT} characters"
                        ),
                    ));
                }
                if !clean.iter().any(|c| c.eq_ignore_ascii_case(name)) {
                    clean.push(name.to_string());
                }
            }
            if clean.len() > MAX_ENVIRONMENTS {
                return Err(Error::validation(
                    "environments",
                    format!("name at most {MAX_ENVIRONMENTS} environments"),
                ));
            }
            clean
        }
    };
    Ok(LinkOptions {
        annotate_deploys: body.deploys.unwrap_or(base.annotate_deploys),
        annotate_releases: body.releases.unwrap_or(base.annotate_releases),
        annotate_pushes: body.pushes.unwrap_or(base.annotate_pushes),
        environments,
        agent_issues: body.agent_issues.unwrap_or(base.agent_issues),
    })
}

/// A stored link's options.
fn options_of(link: &RepoLink) -> LinkOptions {
    LinkOptions {
        annotate_deploys: link.annotate_deploys,
        annotate_releases: link.annotate_releases,
        annotate_pushes: link.annotate_pushes,
        environments: link.environment_list(),
        agent_issues: link.agent_issues,
    }
}

/// `PUT /api/github/link?site=`: follow a repository (or another one).
async fn link_repo(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(body): Json<LinkBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let github = GitHub::of(&st)?;
    let row = installation_of_site(&st, &access, body.installation).await?;
    let token = github
        .installation_token(&st.kv, row.installation_id)
        .await?;
    // Read through the installation: a repository it cannot reach is refused
    // by GitHub here, before anything is stored.
    let repo = github.repo(&token, &body.repo).await?;

    let before = store::link_for_site(&st.pg, access.site.id).await?;
    let base = before.as_ref().map(options_of).unwrap_or_default();
    let options = settle_options(base, &body.options)?;
    let link = store::put_link(
        &st.pg,
        new_id(),
        access.site.id,
        row.id,
        &LinkRepo {
            repo_id: repo.id,
            full_name: &repo.full_name,
            default_branch: &repo.default_branch,
            html_url: &repo.html_url,
        },
        &options,
        Some(auth.user.id),
    )
    .await?;

    let mut entry = Entry::new("github.repo_linked")
        .target("github_repository", repo.id, &repo.full_name)
        .site(&access.site)
        .after(link_json(&link));
    if let Some(old) = before.as_ref() {
        entry = entry.before(link_json(old));
    }
    audit::record(&st, &auth, entry).await;
    Ok(ok(link_json(&link)))
}

/// `PATCH /api/github/link?site=`: change what the link records.
async fn change_link(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(body): Json<OptionsBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let current = store::link_for_site(&st.pg, access.site.id)
        .await?
        .ok_or_else(|| Error::not_found("repository link"))?;
    let options = settle_options(options_of(&current), &body)?;
    let updated = store::set_link_options(&st.pg, access.site.id, &options).await?;
    audit::record(
        &st,
        &auth,
        Entry::new("github.repo_changed")
            .target(
                "github_repository",
                updated.repo_id,
                &updated.repo_full_name,
            )
            .site(&access.site)
            .before(link_json(&current))
            .after(link_json(&updated)),
    )
    .await;
    Ok(ok(link_json(&updated)))
}

/// `DELETE /api/github/link?site=`: stop following the repository. What was
/// recorded, and the annotations it wrote, stay.
async fn unlink(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let current = store::link_for_site(&st.pg, access.site.id)
        .await?
        .ok_or_else(|| Error::not_found("repository link"))?;
    store::delete_link(&st.pg, access.site.id).await?;
    audit::record(
        &st,
        &auth,
        Entry::new("github.repo_unlinked")
            .target(
                "github_repository",
                current.repo_id,
                &current.repo_full_name,
            )
            .site(&access.site)
            .before(link_json(&current)),
    )
    .await;
    Ok(ok(json!({ "unlinked": true })))
}

// ---------------------------------------------------------------------------
// What arrived
// ---------------------------------------------------------------------------

/// `?limit=` for the two lists.
#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    limit: Option<i64>,
}

/// `GET /api/github/events?site=`: the site's latest deploys, releases and
/// pushes.
async fn events(
    State(st): State<AppState>,
    access: SiteAccess,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let limit = q.limit.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS);
    let rows = store::recent_events(&st.pg, access.site.id, limit).await?;
    Ok(ok(rows.iter().map(event_json).collect::<Vec<_>>()))
}

/// `GET /api/github/deliveries?site=`: the latest deliveries about the linked
/// repository, with what became of each.
async fn deliveries(
    State(st): State<AppState>,
    access: SiteAccess,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    require_at_least(&access.role, ADMIN)?;
    let limit = q.limit.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS);
    let rows = match store::link_for_site(&st.pg, access.site.id).await? {
        Some(link) => {
            store::deliveries_for_repo(&st.pg, link.installation_id, link.repo_id, limit).await?
        }
        None => Vec::new(),
    };
    Ok(ok(rows.iter().map(delivery_json).collect::<Vec<_>>()))
}

// ---------------------------------------------------------------------------
// Feedback to an issue
// ---------------------------------------------------------------------------

/// The body of an issue request: the site, only when the report names none.
#[derive(Debug, Default, Deserialize)]
struct IssueBody {
    #[serde(default)]
    site: Option<Value>,
}

/// `POST /api/github/feedback/{id}/issue`
async fn feedback_issue(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<IssueBody>,
) -> ApiResult<Json<Value>> {
    let site = match body.site {
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(s)) => Some(s),
        _ => None,
    };
    let opened = issues::open_issue(&st, &auth, id, site.as_deref()).await?;
    Ok(ok(opened.json()))
}

// ---------------------------------------------------------------------------
// On the wire
// ---------------------------------------------------------------------------

/// One installation.
fn installation_json(i: &Installation) -> Value {
    json!({
        "id": i.id,
        "installation_id": i.installation_id,
        "account": i.account_login,
        "account_type": i.account_type,
        "repository_selection": i.repository_selection,
        "suspended": i.suspended_at.is_some(),
        "suspended_at": i.suspended_at,
        "created_at": i.created_at,
    })
}

/// One repository link.
pub fn link_json(l: &RepoLink) -> Value {
    json!({
        "id": l.id,
        "installation": l.installation_ref,
        "account": l.account_login,
        "repo": {
            "id": l.repo_id,
            "full_name": l.repo_full_name,
            "default_branch": l.default_branch,
            "html_url": l.html_url,
        },
        "deploys": l.annotate_deploys,
        "releases": l.annotate_releases,
        "pushes": l.annotate_pushes,
        "environments": l.environment_list(),
        "agent_issues": l.agent_issues,
        "suspended": l.suspended_at.is_some(),
        "created_at": l.created_at,
        "updated_at": l.updated_at,
    })
}

/// One recorded deploy, release or push.
pub fn event_json(e: &Event) -> Value {
    json!({
        "id": e.id,
        "kind": e.kind,
        "title": e.title,
        "repo": e.repo_full_name,
        "ref": e.git_ref,
        "sha": e.sha,
        "environment": e.environment,
        "actor": e.actor,
        "url": e.url,
        "commits": e.commits,
        "occurred_at": e.occurred_at,
        "annotated": e.annotation_id.is_some(),
    })
}

/// One delivery.
fn delivery_json(d: &Delivery) -> Value {
    json!({
        "id": d.delivery_id,
        "event": d.event,
        "action": d.action,
        "repo": d.repo_full_name,
        "outcome": d.outcome,
        "detail": d.detail,
        "received_at": d.received_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(expires: i64) -> InstallState {
        InstallState {
            o: Uuid::from_u128(1),
            u: Uuid::from_u128(2),
            s: 3,
            n: "nonce-123".into(),
            e: expires,
        }
    }

    #[test]
    fn a_signed_state_round_trips_until_it_expires() {
        let key = [9u8; 32];
        let token = sign_state(&key, &state(2_000)).unwrap();
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
            "the state rides in a URL as it is"
        );
        assert_eq!(read_state(&key, &token, 1_999), Some(state(2_000)));
        assert_eq!(read_state(&key, &token, 2_000), None, "expired");
    }

    #[test]
    fn a_forged_or_altered_state_is_refused() {
        let key = [9u8; 32];
        let token = sign_state(&key, &state(2_000)).unwrap();
        assert_eq!(read_state(&[8u8; 32], &token, 0), None, "another key");

        // Change the organisation and keep the old signature.
        let (_, tag) = token.split_once('.').unwrap();
        let mut forged = state(2_000);
        forged.o = Uuid::from_u128(666);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        assert_eq!(read_state(&key, &format!("{payload}.{tag}"), 0), None);

        for junk in ["", ".", "abc", "abc.def", "a.b.c"] {
            assert_eq!(read_state(&key, junk, 0), None, "{junk}");
        }
    }

    #[test]
    fn refusal_codes_are_the_ones_the_settings_screen_knows() {
        let codes: Vec<&str> = [
            Refusal::State,
            Refusal::SignedOut,
            Refusal::Denied,
            Refusal::Forbidden,
            Refusal::NotYours,
            Refusal::GitHub,
            Refusal::Disabled,
        ]
        .iter()
        .map(|r| r.code())
        .collect();
        assert_eq!(
            codes,
            [
                "state",
                "signed_out",
                "denied",
                "forbidden",
                "not_yours",
                "github",
                "disabled"
            ]
        );
    }

    #[test]
    fn link_options_keep_what_is_not_sent_and_clean_environments() {
        let base = LinkOptions::default();
        let same = settle_options(base.clone(), &OptionsBody::default()).unwrap();
        assert_eq!(same, base);

        let changed = settle_options(
            base.clone(),
            &OptionsBody {
                pushes: Some(true),
                environments: Some(vec![
                    " Production ".into(),
                    "production".into(),
                    "".into(),
                    "staging".into(),
                ]),
                ..OptionsBody::default()
            },
        )
        .unwrap();
        assert!(changed.annotate_pushes);
        assert!(changed.annotate_deploys, "not sent, so kept");
        assert_eq!(changed.environments, ["Production", "staging"]);

        let every = settle_options(
            base.clone(),
            &OptionsBody {
                environments: Some(vec![]),
                ..OptionsBody::default()
            },
        )
        .unwrap();
        assert!(
            every.environments.is_empty(),
            "empty records every environment"
        );

        let too_many = OptionsBody {
            environments: Some((0..=MAX_ENVIRONMENTS).map(|i| format!("env-{i}")).collect()),
            ..OptionsBody::default()
        };
        assert_eq!(
            settle_options(base.clone(), &too_many).unwrap_err().field(),
            Some("environments")
        );
        let control = OptionsBody {
            environments: Some(vec!["prod\nuction".into()]),
            ..OptionsBody::default()
        };
        assert!(settle_options(base, &control).is_err());
    }

    #[test]
    fn a_link_body_reads_its_options_beside_the_repository() {
        let body: LinkBody = serde_json::from_value(json!({
            "installation": Uuid::from_u128(4),
            "repo": "acme/site",
            "pushes": true,
            "environments": ["production", "staging"]
        }))
        .unwrap();
        assert_eq!(body.repo, "acme/site");
        assert_eq!(body.options.pushes, Some(true));
        assert_eq!(body.options.deploys, None);
    }
}

/// The webhook against real stores: dedupe, annotation idempotency, the
/// filters, and what GitHub's own changes do. See
/// [`crate::routes::oauth::harness`] for how to run these. The installation
/// and the link are written straight to Postgres, because creating them
/// through the routes needs GitHub itself.
#[cfg(test)]
mod live {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::Request;
    use hmac::{Hmac, Mac};
    use micaforge_core::{new_id, Config};
    use micaforge_store::models::github::{
        self as store, InstallationFields, LinkOptions, LinkRepo,
    };
    use micaforge_store::models::sites;
    use serde_json::{json, Value};
    use sha2::Sha256;

    use crate::routes::oauth::harness::Harness;

    const SECRET: &str = "It's a Secret to Everybody";

    fn signed(event: &str, delivery: &str, body: &[u8], secret: &str) -> Request<Body> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        Request::post("/api/github/webhook")
            .header("content-type", "application/json")
            .header("x-github-event", event)
            .header("x-github-delivery", delivery)
            .header("x-hub-signature-256", signature)
            .body(Body::from(body.to_vec()))
            .unwrap()
    }

    fn fixture(name: &str) -> Value {
        let raw = match name {
            "push" => include_str!("github/testdata/hooks/push.json"),
            "release" => include_str!("github/testdata/hooks/release.json"),
            "deployment_status" => include_str!("github/testdata/hooks/deployment_status.json"),
            "removed" => {
                include_str!("github/testdata/hooks/installation_repositories_removed.json")
            }
            "deleted" => include_str!("github/testdata/hooks/installation_deleted.json"),
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(raw).unwrap()
    }

    async fn count(h: &Harness, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(&h.st.pg).await.unwrap()
    }

    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn deliveries_are_recorded_once_and_annotate_once() {
        let mut h = Harness::new().await;
        let cfg = Config {
            github_app: Some(super::client::tests::app()),
            ..(*h.st.cfg).clone()
        };
        h.st.cfg = Arc::new(cfg);

        let ada = h.person("ada@example.com").await;
        let site = sites::create(&h.st.pg, new_id(), ada.org, "acme.example", "Acme", "UTC")
            .await
            .unwrap();
        let installation = store::upsert_installation(
            &h.st.pg,
            new_id(),
            ada.org,
            &InstallationFields {
                installation_id: 42,
                account_login: "acme",
                account_type: "Organization",
                repository_selection: Some("selected"),
                suspended_at: None,
            },
            None,
        )
        .await
        .unwrap();
        store::put_link(
            &h.st.pg,
            new_id(),
            site.id,
            installation.id,
            &LinkRepo {
                repo_id: 700_123,
                full_name: "acme/site",
                default_branch: "main",
                html_url: "https://github.com/acme/site",
            },
            &LinkOptions::default(),
            None,
        )
        .await
        .unwrap();

        // A forged delivery is refused and leaves no trace.
        let deploy = serde_json::to_vec(&fixture("deployment_status")).unwrap();
        let (status, _, _) = h
            .send(signed(
                "deployment_status",
                "d-forged",
                &deploy,
                "not the secret",
            ))
            .await;
        assert_eq!(status, 401);
        assert_eq!(count(&h, "SELECT count(*) FROM github_deliveries").await, 0);

        // The deployment is recorded with one annotation linked to GitHub.
        let (status, _, body) = h
            .send(signed("deployment_status", "d-1", &deploy, SECRET))
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["outcome"], "recorded");
        let note: (String, String, Option<String>, String) = sqlx::query_as(
            "SELECT source, text, url, category FROM annotations WHERE site_id = $1",
        )
        .bind(site.id)
        .fetch_one(&h.st.pg)
        .await
        .unwrap();
        assert_eq!(note.0, "github");
        assert_eq!(note.1, "Deployed 0d1a26e to Production (acme/site)");
        assert_eq!(
            note.2.as_deref(),
            Some("https://github.com/acme/site/commit/0d1a26e67d8f5eaf1f6ba5c57fc3c7d91ac0fd1c")
        );
        assert_eq!(note.3, "deploy");

        // A redelivery is a duplicate; a second copy of the same deployment
        // under a new delivery id is already recorded. One note either way.
        let (_, _, again) = h
            .send(signed("deployment_status", "d-1", &deploy, SECRET))
            .await;
        assert_eq!(again["duplicate"], true);
        let (_, _, copy) = h
            .send(signed("deployment_status", "d-2", &deploy, SECRET))
            .await;
        assert_eq!(copy["outcome"], "ignored");
        assert_eq!(count(&h, "SELECT count(*) FROM annotations").await, 1);

        // Two copies at once still write one note.
        let mut racing = fixture("deployment_status");
        racing["deployment"]["id"] = json!(990_002);
        let racing = serde_json::to_vec(&racing).unwrap();
        let (a, b) = tokio::join!(
            h.send(signed("deployment_status", "d-3", &racing, SECRET)),
            h.send(signed("deployment_status", "d-4", &racing, SECRET)),
        );
        assert_eq!((a.0, b.0), (200, 200));
        assert_eq!(count(&h, "SELECT count(*) FROM annotations").await, 2);

        // A staging deployment is not an environment this site records.
        let mut staging = fixture("deployment_status");
        staging["deployment"]["id"] = json!(990_003);
        staging["deployment_status"]["environment"] = json!("staging");
        let (_, _, skipped) = h
            .send(signed(
                "deployment_status",
                "d-5",
                &serde_json::to_vec(&staging).unwrap(),
                SECRET,
            ))
            .await;
        assert_eq!(skipped["outcome"], "ignored");
        assert!(skipped["detail"].as_str().unwrap().contains("staging"));

        // A release annotates; a push is recorded but, with pushes off, not
        // drawn on the chart.
        let release = serde_json::to_vec(&fixture("release")).unwrap();
        let (_, _, released) = h.send(signed("release", "r-1", &release, SECRET)).await;
        assert_eq!(released["outcome"], "recorded");
        let push = serde_json::to_vec(&fixture("push")).unwrap();
        let (_, _, pushed) = h.send(signed("push", "p-1", &push, SECRET)).await;
        assert_eq!(pushed["outcome"], "recorded");
        assert_eq!(count(&h, "SELECT count(*) FROM annotations").await, 3);
        assert_eq!(count(&h, "SELECT count(*) FROM github_events").await, 4);

        // A delivery larger than an ordinary JSON body is still read.
        let mut big = fixture("push");
        big["padding"] = json!("x".repeat(1_500_000));
        let (status, _, _) = h
            .send(signed(
                "push",
                "p-2",
                &serde_json::to_vec(&big).unwrap(),
                SECRET,
            ))
            .await;
        assert_eq!(status, 200, "the webhook's own body limit applies");

        // The chart's annotations carry the GitHub ones, with their link.
        let read = |path: String| {
            Request::get(path)
                .header("cookie", ada.cookie.as_str())
                .body(Body::empty())
                .unwrap()
        };
        let (status, _, notes) = h
            .send(read(format!(
                "/api/annotations?site={}&range=all",
                site.site_id
            )))
            .await;
        assert_eq!(status, 200, "{notes}");
        let notes = notes["data"].as_array().unwrap();
        assert_eq!(notes.len(), 3);
        assert!(notes.iter().all(|n| n["source"] == "github"
            && n["url"]
                .as_str()
                .is_some_and(|u| u.starts_with("https://github.com/"))));

        let (_, _, recent) = h
            .send(read(format!("/api/github/events?site={}", site.site_id)))
            .await;
        assert_eq!(recent["data"].as_array().unwrap().len(), 4);
        let (_, _, delivered) = h
            .send(read(format!(
                "/api/github/deliveries?site={}",
                site.site_id
            )))
            .await;
        assert!(delivered["data"].as_array().unwrap().len() >= 8);

        // A report about a site with a linked repository, but no admin role on
        // it for Bob: refused as missing, because Bob cannot read the report.
        let bob = h.person("bob@example.com").await;
        let (_, _, sent) = h
            .send(
                Request::post("/api/feedback")
                    .header("cookie", ada.cookie.as_str())
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "kind": "bug", "title": "The chart is empty", "body": "Since Monday.",
                                "site": site.site_id })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await;
        let report = sent["data"]["id"].as_str().unwrap().to_string();
        let (status, _, _) = h
            .send(
                Request::post(format!("/api/github/feedback/{report}/issue"))
                    .header("cookie", bob.cookie.as_str())
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 404);

        // GitHub takes the repository out of the installation: the link goes,
        // and the audit log says GitHub did it.
        let removed = serde_json::to_vec(&fixture("removed")).unwrap();
        let (_, _, unlinked) = h
            .send(signed("installation_repositories", "i-1", &removed, SECRET))
            .await;
        assert_eq!(unlinked["outcome"], "recorded");
        assert!(store::link_for_site(&h.st.pg, site.id)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            count(
                &h,
                "SELECT count(*) FROM audit_events WHERE action = 'github.repo_unlinked' \
                 AND actor_kind = 'system'"
            )
            .await,
            1
        );

        // With no repository linked, Ada (the instance admin and the site's
        // owner) is told so rather than sent to GitHub.
        let (status, _, refused) = h
            .send(
                Request::post(format!("/api/github/feedback/{report}/issue"))
                    .header("cookie", ada.cookie.as_str())
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 409, "{refused}");

        // The App is uninstalled on GitHub: the installation goes too.
        let deleted = serde_json::to_vec(&fixture("deleted")).unwrap();
        let (_, _, gone) = h
            .send(signed("installation", "i-2", &deleted, SECRET))
            .await;
        assert_eq!(gone["outcome"], "recorded");
        assert_eq!(
            count(&h, "SELECT count(*) FROM github_installations").await,
            0
        );
        // What was recorded stays: the annotations are the site's history.
        assert_eq!(count(&h, "SELECT count(*) FROM annotations").await, 3);

        h.finish().await;
    }
}
