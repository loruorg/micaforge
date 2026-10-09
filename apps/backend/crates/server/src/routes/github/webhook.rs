//! `POST /api/github/webhook` — what GitHub sends the App.
//!
//! One address for every installation, signed with the App's one webhook
//! secret. A delivery is handled in four steps, and each refuses early:
//!
//! 1. **Verify.** `X-Hub-Signature-256` is `sha256=` and the hex HMAC-SHA256
//!    of the raw body under `MICAFORGE_GITHUB_APP_WEBHOOK_SECRET`, compared in
//!    constant time. A delivery that fails is answered `401` and recorded
//!    nowhere, so a stranger cannot fill the delivery table. The body is read
//!    under [`BODY_LIMIT`] first; anything larger is `413` before any work.
//! 2. **Claim.** `X-GitHub-Delivery` is GitHub's id for the delivery, and a
//!    redelivery carries the same one. It is claimed in Postgres
//!    (`github_deliveries`); a delivery already `recorded` or `ignored` is
//!    answered `200` with `duplicate: true` and nothing else happens. One that
//!    `failed` can be claimed again, which is what GitHub's "Redeliver" is for.
//! 3. **Read.** [`read`] turns the payload into one [`Reading`], a pure
//!    function over GitHub's documented shapes, tested against fixtures of
//!    them: a push to the default branch, a published release, a successful
//!    deployment status, an installation deleted, suspended or changed,
//!    repositories removed from an installation. Everything else is ignored
//!    with a sentence saying why.
//! 4. **Record.** An activity is recorded for every site that follows the
//!    repository through that installation, filtered by the site's choices
//!    (its deployment environments; whether each kind becomes an annotation).
//!    The event and its annotation are written in one transaction under the
//!    event's own unique key ([`micaforge_store::models::github::record_event`]),
//!    so even two copies of one deployment racing each other write one note.
//!
//! The outcome and one plain sentence are kept on the delivery row, which is
//! what the site's GitHub settings show under "Recent deliveries".

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, TimeZone, Utc};
use hmac::{Hmac, Mac};
use micaforge_core::{new_id, Error, Result};
use micaforge_store::models::github::{
    self as store, DeliveryHead, InstallationFields, LinkRepo, NewEvent, NoteDraft,
};
use micaforge_store::models::sites;
use serde_json::{json, Value};
use sha2::Sha256;

use super::client::forget_token;
use crate::audit::{self, Actor, Entry};
use crate::error::ApiError;
use crate::state::AppState;

/// The largest delivery read. GitHub caps a payload at 25 MB, but only a push
/// of thousands of commits comes near this, and a push is read for its head
/// commit and its last twenty.
pub const BODY_LIMIT: usize = 4 * 1024 * 1024;

/// How long a delivery is remembered, for the dedupe and the settings list.
/// GitHub lets a delivery be redelivered for a few days; a month is ample.
const DELIVERY_MEMORY_DAYS: i32 = 30;

/// The commits of a push that are kept, newest last.
const MAX_COMMITS: usize = 20;

/// Longest one-line title kept, in characters.
const MAX_TITLE: usize = 200;

/// Longest annotation text: the limit every annotation keeps.
const MAX_NOTE: usize = 280;

/// Who the audit log says acted, for what GitHub told us.
const ACTOR: &str = "GitHub webhook";

// ---------------------------------------------------------------------------
// Verifying
// ---------------------------------------------------------------------------

/// Does `header` (`X-Hub-Signature-256`) sign `body` under `secret`?
///
/// `Mac::verify_slice` compares in constant time, so how long a wrong
/// signature takes to refuse says nothing about how much of it was right.
pub fn verify_signature(secret: &str, header: &str, body: &[u8]) -> bool {
    let Some(hex_sig) = header.trim().strip_prefix("sha256=") else {
        return false;
    };
    let Ok(given) = hex::decode(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&given).is_ok()
}

/// A delivery id as GitHub writes one (a GUID), or nothing usable.
fn delivery_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|id| {
            (1..=100).contains(&id.len())
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// The event name, as GitHub writes one (`deployment_status`).
fn event_name(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|e| {
            (1..=64).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// The three kinds of activity a site records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A successful deployment.
    Deploy,
    /// A published release.
    Release,
    /// A push to the default branch.
    Push,
}

impl Kind {
    /// The word stored in `github_events.kind` and used as the category.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Deploy => "deploy",
            Kind::Release => "release",
            Kind::Push => "push",
        }
    }
}

/// The repository a delivery is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    /// GitHub's repository id.
    pub id: i64,
    /// `owner/name`.
    pub full_name: String,
    /// Its default branch at the time of the delivery.
    pub default_branch: String,
    /// Its page on GitHub.
    pub html_url: String,
}

/// One commit of a push, as kept.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CommitRef {
    /// The full commit id.
    pub sha: String,
    /// Its subject line.
    pub message: String,
    /// Its page on GitHub.
    pub url: String,
    /// The author's GitHub login, or their name when they have none.
    pub author: Option<String>,
}

/// A deploy, a release or a push, read from a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// Which of the three.
    pub kind: Kind,
    /// The installation it came through.
    pub installation_id: i64,
    /// The repository.
    pub repo: RepoRef,
    /// GitHub's id for the object: deployment id, release id, head commit.
    pub external_id: String,
    /// The branch or tag.
    pub git_ref: Option<String>,
    /// The commit.
    pub sha: Option<String>,
    /// One line.
    pub title: String,
    /// The deployment environment.
    pub environment: Option<String>,
    /// The GitHub login that caused it.
    pub actor: Option<String>,
    /// The object on GitHub.
    pub url: String,
    /// A push's last commits, newest last.
    pub commits: Vec<CommitRef>,
    /// How many commits the push carried in all.
    pub commit_count: usize,
    /// A pre-release, for a release.
    pub prerelease: bool,
    /// When it happened.
    pub occurred_at: DateTime<Utc>,
}

impl Activity {
    /// The annotation it writes: one line, within the annotation limit.
    pub fn note_text(&self) -> String {
        let short = self.sha.as_deref().map(short_sha).unwrap_or_default();
        let text = match self.kind {
            Kind::Deploy => format!(
                "Deployed {short} to {} ({})",
                self.environment.as_deref().unwrap_or("an environment"),
                self.repo.full_name
            ),
            Kind::Release => {
                let tag = self.git_ref.as_deref().unwrap_or_default();
                let mut text = format!("Released {tag}");
                if self.title != tag {
                    text.push_str(&format!(": {}", self.title));
                }
                if self.prerelease {
                    text.push_str(" (pre-release)");
                }
                text
            }
            Kind::Push => format!(
                "Pushed {} to {}: {}",
                plural(self.commit_count, "commit", "commits"),
                self.git_ref.as_deref().unwrap_or_default(),
                self.title
            ),
        };
        fit(&text, MAX_NOTE)
    }
}

/// What a delivery asks of us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// Record an activity for every site that follows its repository. Boxed:
    /// it is many times the size of every other reading.
    Record(Box<Activity>),
    /// The App was uninstalled from the account.
    InstallationDeleted {
        /// GitHub's installation id.
        installation_id: i64,
        /// The account it was on.
        account: String,
    },
    /// The account suspended the App (`Some`) or lifted the suspension (`None`).
    InstallationSuspended {
        /// GitHub's installation id.
        installation_id: i64,
        /// When, or `None` for lifted.
        at: Option<DateTime<Utc>>,
    },
    /// The installation's account details changed, or it was created.
    InstallationChanged {
        /// GitHub's installation id.
        installation_id: i64,
        /// The account's login.
        account_login: String,
        /// `User` or `Organization`.
        account_type: String,
        /// `all` or `selected`.
        repository_selection: Option<String>,
    },
    /// Repositories were taken out of the installation.
    ReposRemoved {
        /// GitHub's installation id.
        installation_id: i64,
        /// The repositories' ids.
        repo_ids: Vec<i64>,
    },
    /// Nothing to do, and why, in a sentence.
    Ignore(String),
}

/// A trimmed, non-empty string at `v`.
fn text(v: &Value) -> Option<&str> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// An RFC 3339 instant at `v`.
fn instant(v: &Value) -> Option<DateTime<Utc>> {
    text(v)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// The first line of a message, cut to [`MAX_TITLE`].
fn subject(message: &str) -> String {
    fit(message.lines().next().unwrap_or_default().trim(), MAX_TITLE)
}

/// At most `max` characters, with an ellipsis where it was cut.
fn fit(raw: &str, max: usize) -> String {
    if raw.chars().count() <= max {
        return raw.to_string();
    }
    let mut out: String = raw.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// The seven characters GitHub shows for a commit.
fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// "1 commit", "3 commits".
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// An `https://` address at `v`.
fn https(v: &Value) -> Option<&str> {
    text(v).filter(|u| u.starts_with("https://"))
}

/// The repository of a payload.
fn repo_of(payload: &Value) -> Option<RepoRef> {
    let repo = &payload["repository"];
    Some(RepoRef {
        id: repo["id"].as_i64()?,
        full_name: text(&repo["full_name"])?.to_string(),
        default_branch: text(&repo["default_branch"]).unwrap_or("main").to_string(),
        html_url: https(&repo["html_url"])?.to_string(),
    })
}

/// The installation id of a payload.
fn installation_of(payload: &Value) -> Option<i64> {
    payload["installation"]["id"].as_i64()
}

/// Turn a delivery into what it asks of us. `now` is when it arrived, for a
/// payload that carries no time of its own.
pub fn read(event: &str, payload: &Value, now: DateTime<Utc>) -> Reading {
    let ignore = |why: &str| Reading::Ignore(why.to_string());
    match event {
        "ping" => ignore("GitHub checked that the webhook answers. It does."),
        "push" => read_push(payload, now),
        "release" => read_release(payload, now),
        "deployment_status" => read_deployment(payload, now),
        "installation" => read_installation_event(payload, now),
        "installation_repositories" => read_repositories(payload),
        other => Reading::Ignore(format!(
            "Micaforge does not record `{other}` events; the app does not need to send them."
        )),
    }
}

fn read_push(payload: &Value, now: DateTime<Utc>) -> Reading {
    let (Some(repo), Some(installation_id)) = (repo_of(payload), installation_of(payload)) else {
        return Reading::Ignore("The push named no repository or installation.".into());
    };
    let branch = text(&payload["ref"])
        .and_then(|r| r.strip_prefix("refs/heads/"))
        .unwrap_or_default()
        .to_string();
    if branch != repo.default_branch {
        return Reading::Ignore(format!(
            "A push to {}, not to the default branch {}.",
            if branch.is_empty() { "a tag" } else { &branch },
            repo.default_branch
        ));
    }
    if payload["deleted"].as_bool() == Some(true) {
        return Reading::Ignore("The push deleted the branch.".into());
    }
    let head = &payload["head_commit"];
    let (Some(sha), Some(message)) = (text(&head["id"]), text(&head["message"])) else {
        return Reading::Ignore("The push carried no head commit.".into());
    };
    let all = payload["commits"].as_array().cloned().unwrap_or_default();
    let commits: Vec<CommitRef> = all
        .iter()
        .skip(all.len().saturating_sub(MAX_COMMITS))
        .filter_map(|c| {
            Some(CommitRef {
                sha: text(&c["id"])?.to_string(),
                message: subject(text(&c["message"]).unwrap_or_default()),
                url: https(&c["url"])?.to_string(),
                author: text(&c["author"]["username"])
                    .or_else(|| text(&c["author"]["name"]))
                    .map(str::to_string),
            })
        })
        .collect();
    let url = https(&payload["compare"])
        .or_else(|| https(&head["url"]))
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}/commit/{sha}", repo.html_url));
    let occurred_at = payload["repository"]["pushed_at"]
        .as_i64()
        .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
        .or_else(|| instant(&head["timestamp"]))
        .unwrap_or(now);
    Reading::Record(Box::new(Activity {
        kind: Kind::Push,
        installation_id,
        external_id: sha.to_string(),
        git_ref: Some(branch),
        sha: Some(sha.to_string()),
        title: subject(message),
        environment: None,
        actor: text(&payload["sender"]["login"]).map(str::to_string),
        url,
        commit_count: all.len().max(1),
        commits,
        prerelease: false,
        occurred_at,
        repo,
    }))
}

fn read_release(payload: &Value, now: DateTime<Utc>) -> Reading {
    let action = text(&payload["action"]).unwrap_or_default();
    if action != "published" {
        return Reading::Ignore(format!(
            "A release was {action}; only a published release is recorded."
        ));
    }
    let release = &payload["release"];
    if release["draft"].as_bool() == Some(true) {
        return Reading::Ignore("The release is a draft.".into());
    }
    let (Some(repo), Some(installation_id)) = (repo_of(payload), installation_of(payload)) else {
        return Reading::Ignore("The release named no repository or installation.".into());
    };
    let (Some(id), Some(tag), Some(url)) = (
        release["id"].as_i64(),
        text(&release["tag_name"]),
        https(&release["html_url"]),
    ) else {
        return Reading::Ignore("The release had no id, tag or address.".into());
    };
    Reading::Record(Box::new(Activity {
        kind: Kind::Release,
        installation_id,
        external_id: id.to_string(),
        git_ref: Some(tag.to_string()),
        sha: None,
        title: subject(text(&release["name"]).unwrap_or(tag)),
        environment: None,
        actor: text(&release["author"]["login"])
            .or_else(|| text(&payload["sender"]["login"]))
            .map(str::to_string),
        url: url.to_string(),
        commits: Vec::new(),
        commit_count: 0,
        prerelease: release["prerelease"].as_bool().unwrap_or(false),
        occurred_at: instant(&release["published_at"]).unwrap_or(now),
        repo,
    }))
}

fn read_deployment(payload: &Value, now: DateTime<Utc>) -> Reading {
    let status = &payload["deployment_status"];
    let deployment = &payload["deployment"];
    let state = text(&status["state"]).unwrap_or_default();
    if state != "success" {
        return Reading::Ignore(format!(
            "A deployment status of {state}; only a successful deployment is recorded."
        ));
    }
    let (Some(repo), Some(installation_id)) = (repo_of(payload), installation_of(payload)) else {
        return Reading::Ignore("The deployment named no repository or installation.".into());
    };
    let Some(environment) =
        text(&status["environment"]).or_else(|| text(&deployment["environment"]))
    else {
        return Reading::Ignore("The deployment named no environment.".into());
    };
    let (Some(id), Some(sha)) = (deployment["id"].as_i64(), text(&deployment["sha"])) else {
        return Reading::Ignore("The deployment had no id or commit.".into());
    };
    let title = text(&deployment["description"])
        .or_else(|| text(&status["description"]))
        .map(subject)
        .unwrap_or_else(|| format!("Deployed {}", short_sha(sha)));
    Reading::Record(Box::new(Activity {
        kind: Kind::Deploy,
        installation_id,
        external_id: id.to_string(),
        git_ref: text(&deployment["ref"]).map(str::to_string),
        sha: Some(sha.to_string()),
        title,
        environment: Some(environment.to_string()),
        actor: text(&deployment["creator"]["login"])
            .or_else(|| text(&payload["sender"]["login"]))
            .map(str::to_string),
        url: format!("{}/commit/{sha}", repo.html_url),
        commits: Vec::new(),
        commit_count: 0,
        prerelease: false,
        occurred_at: instant(&status["created_at"]).unwrap_or(now),
        repo,
    }))
}

fn read_installation_event(payload: &Value, now: DateTime<Utc>) -> Reading {
    let installation = &payload["installation"];
    let Some(installation_id) = installation["id"].as_i64() else {
        return Reading::Ignore("The installation event had no installation id.".into());
    };
    let account = text(&installation["account"]["login"])
        .unwrap_or_default()
        .to_string();
    match text(&payload["action"]).unwrap_or_default() {
        "deleted" => Reading::InstallationDeleted {
            installation_id,
            account,
        },
        "suspend" => Reading::InstallationSuspended {
            installation_id,
            at: Some(instant(&installation["suspended_at"]).unwrap_or(now)),
        },
        "unsuspend" => Reading::InstallationSuspended {
            installation_id,
            at: None,
        },
        "created" | "new_permissions_accepted" => Reading::InstallationChanged {
            installation_id,
            account_login: account,
            account_type: text(&installation["account"]["type"])
                .or_else(|| text(&installation["target_type"]))
                .unwrap_or("User")
                .to_string(),
            repository_selection: text(&installation["repository_selection"]).map(str::to_string),
        },
        other => Reading::Ignore(format!("An installation was {other}; nothing to change.")),
    }
}

fn read_repositories(payload: &Value) -> Reading {
    let Some(installation_id) = installation_of(payload) else {
        return Reading::Ignore("The event had no installation id.".into());
    };
    match text(&payload["action"]).unwrap_or_default() {
        "removed" => Reading::ReposRemoved {
            installation_id,
            repo_ids: payload["repositories_removed"]
                .as_array()
                .map(|repos| repos.iter().filter_map(|r| r["id"].as_i64()).collect())
                .unwrap_or_default(),
        },
        _ => Reading::Ignore(
            "Repositories were added to the installation; link one from a site's GitHub settings."
                .into(),
        ),
    }
}

// ---------------------------------------------------------------------------
// The route
// ---------------------------------------------------------------------------

/// A plain JSON answer for GitHub.
fn answer(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// `POST /api/github/webhook`
pub async fn receive(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(app) = st.cfg.github_app.as_ref() else {
        return ApiError(super::client::not_configured()).into_response();
    };
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !verify_signature(&app.webhook_secret, signature, &body) {
        tracing::warn!("a GitHub webhook delivery failed its signature check");
        return ApiError(Error::Unauthorized(
            "the X-Hub-Signature-256 header does not sign this body with the app's webhook secret"
                .into(),
        ))
        .into_response();
    }
    let (Some(delivery), Some(event)) = (delivery_id(&headers), event_name(&headers)) else {
        return ApiError(Error::BadRequest(
            "a delivery needs its X-GitHub-Delivery and X-GitHub-Event headers".into(),
        ))
        .into_response();
    };
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return ApiError(Error::BadRequest("the body is not JSON".into())).into_response()
        }
    };

    let head = DeliveryHead {
        delivery_id: delivery,
        event,
        action: text(&payload["action"]),
        installation_id: installation_of(&payload),
        repo_id: payload["repository"]["id"].as_i64(),
        repo_full_name: text(&payload["repository"]["full_name"]),
    };
    match store::claim_delivery(&st.pg, &head).await {
        Ok(true) => {}
        Ok(false) => {
            return answer(
                StatusCode::OK,
                json!({ "received": true, "duplicate": true }),
            )
        }
        Err(e) => return ApiError(e).into_response(),
    }

    let reading = read(event, &payload, Utc::now());
    let response = match handle(&st, delivery, reading).await {
        Ok((outcome, detail)) => {
            if let Err(e) = store::finish_delivery(&st.pg, delivery, outcome, &detail).await {
                tracing::warn!(error = %e, "a GitHub delivery's outcome could not be saved");
            }
            answer(
                StatusCode::OK,
                json!({ "received": true, "outcome": outcome, "detail": detail }),
            )
        }
        Err(e) => {
            tracing::error!(delivery, event, error = %e, "a GitHub delivery could not be recorded");
            let said = "Could not be recorded. Use Redeliver on GitHub to try it again.";
            if let Err(e) = store::finish_delivery(&st.pg, delivery, "failed", said).await {
                tracing::warn!(error = %e, "a GitHub delivery's failure could not be saved");
            }
            ApiError(e).into_response()
        }
    };

    if let Err(e) = store::prune_deliveries(&st.pg, DELIVERY_MEMORY_DAYS).await {
        tracing::warn!(error = %e, "old GitHub deliveries could not be pruned");
    }
    response
}

/// Act on a reading. Returns the outcome (`recorded` or `ignored`) and a
/// sentence for the settings screen.
async fn handle(st: &AppState, delivery: &str, reading: Reading) -> Result<(&'static str, String)> {
    match reading {
        Reading::Ignore(why) => Ok(("ignored", why)),
        Reading::Record(activity) => record(st, delivery, &activity).await,
        Reading::InstallationDeleted {
            installation_id,
            account,
        } => {
            let gone = store::delete_by_github_id(&st.pg, installation_id).await?;
            if let Err(e) = forget_token(&st.kv, installation_id).await {
                tracing::warn!(error = %e, "a removed installation's token could not be forgotten");
            }
            for row in &gone {
                audit::record_as(
                    st,
                    Actor::system(ACTOR),
                    Entry::new("github.app_uninstalled")
                        .org(row.org_id)
                        .target(
                            "github_installation",
                            row.installation_id,
                            &row.account_login,
                        )
                        .meta(json!({ "on": "github" })),
                )
                .await;
            }
            Ok((
                if gone.is_empty() {
                    "ignored"
                } else {
                    "recorded"
                },
                format!(
                    "The app was uninstalled from {account} on GitHub; {} removed.",
                    plural(gone.len(), "connection was", "connections were")
                ),
            ))
        }
        Reading::InstallationSuspended {
            installation_id,
            at,
        } => {
            let changed = store::set_suspended(&st.pg, installation_id, at).await?;
            let action = if at.is_some() {
                "github.app_suspended"
            } else {
                "github.app_unsuspended"
            };
            for row in &changed {
                audit::record_as(
                    st,
                    Actor::system(ACTOR),
                    Entry::new(action).org(row.org_id).target(
                        "github_installation",
                        row.installation_id,
                        &row.account_login,
                    ),
                )
                .await;
            }
            Ok((
                if changed.is_empty() {
                    "ignored"
                } else {
                    "recorded"
                },
                if at.is_some() {
                    "The app was suspended on GitHub; nothing is recorded until it is lifted."
                        .to_string()
                } else {
                    "The suspension was lifted on GitHub; recording again.".to_string()
                },
            ))
        }
        Reading::InstallationChanged {
            installation_id,
            account_login,
            account_type,
            repository_selection,
        } => {
            store::refresh_installation(
                &st.pg,
                &InstallationFields {
                    installation_id,
                    account_login: &account_login,
                    account_type: &account_type,
                    repository_selection: repository_selection.as_deref(),
                    suspended_at: None,
                },
            )
            .await?;
            Ok((
                "ignored",
                format!("The installation on {account_login} changed; its details are current."),
            ))
        }
        Reading::ReposRemoved {
            installation_id,
            repo_ids,
        } => {
            let gone = store::unlink_repos(&st.pg, installation_id, &repo_ids).await?;
            for link in &gone {
                if let Ok(site) = sites::get(&st.pg, link.site_id).await {
                    audit::record_as(
                        st,
                        Actor::system(ACTOR),
                        Entry::new("github.repo_unlinked")
                            .target("github_repository", link.repo_id, &link.repo_full_name)
                            .site(&site)
                            .meta(json!({ "reason": "removed from the installation on GitHub" })),
                    )
                    .await;
                }
            }
            Ok((
                if gone.is_empty() {
                    "ignored"
                } else {
                    "recorded"
                },
                format!(
                    "Repositories were removed from the installation; {} unlinked.",
                    plural(gone.len(), "site was", "sites were")
                ),
            ))
        }
    }
}

/// Record an activity for every site that follows its repository.
async fn record(
    st: &AppState,
    delivery: &str,
    activity: &Activity,
) -> Result<(&'static str, String)> {
    let links = store::links_for_repo(&st.pg, activity.installation_id, activity.repo.id).await?;
    if links.is_empty() {
        return Ok((
            "ignored",
            format!("No site follows {}.", activity.repo.full_name),
        ));
    }
    let named = LinkRepo {
        repo_id: activity.repo.id,
        full_name: &activity.repo.full_name,
        default_branch: &activity.repo.default_branch,
        html_url: &activity.repo.html_url,
    };
    if let Err(e) = store::refresh_repo(&st.pg, activity.installation_id, &named).await {
        tracing::warn!(error = %e, "a linked repository's name could not be refreshed");
    }

    let commits = serde_json::to_value(&activity.commits)?;
    let note_text = activity.note_text();
    let event = NewEvent {
        kind: activity.kind.as_str(),
        external_id: &activity.external_id,
        repo_full_name: &activity.repo.full_name,
        git_ref: activity.git_ref.as_deref(),
        sha: activity.sha.as_deref(),
        title: &activity.title,
        environment: activity.environment.as_deref(),
        actor: activity.actor.as_deref(),
        url: &activity.url,
        commits: &commits,
        occurred_at: activity.occurred_at,
        delivery_id: Some(delivery),
    };

    let mut tally = Tally::default();
    for link in &links {
        if link.suspended_at.is_some() {
            tally
                .skipped
                .push("the installation is suspended".to_string());
            continue;
        }
        if let Some(environment) = activity.environment.as_deref() {
            if !link.records_environment(environment) {
                tally.skipped.push(format!(
                    "{environment} is not an environment the site records"
                ));
                continue;
            }
        }
        let annotate = match activity.kind {
            Kind::Deploy => link.annotate_deploys,
            Kind::Release => link.annotate_releases,
            Kind::Push => link.annotate_pushes,
        };
        let note = annotate.then(|| NoteDraft {
            id: new_id(),
            text: &note_text,
            category: activity.kind.as_str(),
        });
        match store::record_event(&st.pg, new_id(), link.site_id, &event, note.as_ref()).await? {
            Some(stored) => {
                tally.recorded += 1;
                if stored.annotation_id.is_some() {
                    tally.noted += 1;
                }
            }
            None => tally.duplicate += 1,
        }
    }
    Ok(tally.outcome())
}

/// What happened to one activity across the sites that follow it.
#[derive(Debug, Default, PartialEq, Eq)]
struct Tally {
    recorded: usize,
    noted: usize,
    duplicate: usize,
    skipped: Vec<String>,
}

impl Tally {
    /// The outcome and its sentence.
    fn outcome(&self) -> (&'static str, String) {
        let mut said = Vec::new();
        if self.recorded > 0 {
            said.push(format!(
                "Recorded for {}, {} on the chart.",
                plural(self.recorded, "site", "sites"),
                plural(self.noted, "annotation", "annotations")
            ));
        }
        if self.duplicate > 0 {
            said.push(format!(
                "Already recorded for {}.",
                plural(self.duplicate, "site", "sites")
            ));
        }
        if !self.skipped.is_empty() {
            let mut reasons = self.skipped.clone();
            reasons.dedup();
            said.push(format!(
                "Skipped for {}: {}.",
                plural(self.skipped.len(), "site", "sites"),
                reasons.join("; ")
            ));
        }
        let outcome = if self.recorded > 0 {
            "recorded"
        } else {
            "ignored"
        };
        (outcome, said.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let raw = match name {
            "push" => include_str!("testdata/hooks/push.json"),
            "release" => include_str!("testdata/hooks/release.json"),
            "deployment_status" => include_str!("testdata/hooks/deployment_status.json"),
            "installation_deleted" => include_str!("testdata/hooks/installation_deleted.json"),
            "installation_repositories_removed" => {
                include_str!("testdata/hooks/installation_repositories_removed.json")
            }
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(raw).unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap()
    }

    #[test]
    fn githubs_documented_signature_example_verifies() {
        // From GitHub's "Validating webhook deliveries".
        let secret = "It's a Secret to Everybody";
        let body = b"Hello, World!";
        let header = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        assert!(verify_signature(secret, header, body));

        assert!(!verify_signature("another secret", header, body));
        assert!(!verify_signature(secret, header, b"Hello, World?"));
        assert!(!verify_signature(
            secret,
            "sha1=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17",
            body
        ));
        assert!(!verify_signature(secret, "sha256=not-hex", body));
        assert!(!verify_signature(secret, "sha256=757107", body));
        assert!(!verify_signature(secret, "", body));
    }

    #[test]
    fn delivery_and_event_headers_are_shape_checked() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-github-delivery",
            "72d3162e-cc78-11e3-81ab-4c9367dc0958".parse().unwrap(),
        );
        headers.insert("x-github-event", "deployment_status".parse().unwrap());
        assert_eq!(
            delivery_id(&headers),
            Some("72d3162e-cc78-11e3-81ab-4c9367dc0958")
        );
        assert_eq!(event_name(&headers), Some("deployment_status"));

        headers.insert("x-github-delivery", "../../etc".parse().unwrap());
        headers.insert("x-github-event", "Push Event".parse().unwrap());
        assert_eq!(delivery_id(&headers), None);
        assert_eq!(event_name(&headers), None);
    }

    #[test]
    fn a_push_to_the_default_branch_is_read_with_its_commits() {
        let Reading::Record(push) = read("push", &fixture("push"), now()) else {
            panic!("a push to main is recorded");
        };
        assert_eq!(push.kind, Kind::Push);
        assert_eq!(push.installation_id, 42);
        assert_eq!(push.repo.id, 700_123);
        assert_eq!(push.external_id, "0d1a26e67d8f5eaf1f6ba5c57fc3c7d91ac0fd1c");
        assert_eq!(push.git_ref.as_deref(), Some("main"));
        assert_eq!(push.title, "Ship the new pricing page");
        assert_eq!(push.actor.as_deref(), Some("ada-example"));
        assert_eq!(
            push.url,
            "https://github.com/acme/site/compare/9049f1265b7d...0d1a26e67d8f"
        );
        assert_eq!(push.occurred_at.to_rfc3339(), "2026-10-06T08:00:12+00:00");
        assert_eq!(push.commit_count, 2);
        assert_eq!(
            push.commits[0].message,
            "Move the pricing table above the fold"
        );
        assert_eq!(push.commits[0].author.as_deref(), Some("ada-example"));
        assert_eq!(
            push.note_text(),
            "Pushed 2 commits to main: Ship the new pricing page"
        );
        // No address is kept: commits carry a login, not an email.
        let kept = serde_json::to_string(&push.commits).unwrap();
        assert!(!kept.contains('@'));
    }

    #[test]
    fn a_push_elsewhere_or_a_deletion_is_ignored() {
        let mut other = fixture("push");
        other["ref"] = json!("refs/heads/feature/x");
        assert!(
            matches!(read("push", &other, now()), Reading::Ignore(why) if why.contains("feature/x"))
        );

        let mut tag = fixture("push");
        tag["ref"] = json!("refs/tags/v1");
        assert!(matches!(read("push", &tag, now()), Reading::Ignore(why) if why.contains("a tag")));

        let mut deleted = fixture("push");
        deleted["deleted"] = json!(true);
        deleted["head_commit"] = Value::Null;
        assert!(matches!(read("push", &deleted, now()), Reading::Ignore(_)));
    }

    #[test]
    fn a_published_release_is_read_and_others_are_not() {
        let Reading::Record(release) = read("release", &fixture("release"), now()) else {
            panic!("a published release is recorded");
        };
        assert_eq!(release.kind, Kind::Release);
        assert_eq!(release.external_id, "180000001");
        assert_eq!(release.git_ref.as_deref(), Some("v2.4.0"));
        assert_eq!(
            release.url,
            "https://github.com/acme/site/releases/tag/v2.4.0"
        );
        assert_eq!(
            release.occurred_at.to_rfc3339(),
            "2026-10-07T14:00:00+00:00"
        );
        assert_eq!(
            release.note_text(),
            "Released v2.4.0: Pricing and docs search"
        );

        let mut pre = fixture("release");
        pre["release"]["prerelease"] = json!(true);
        pre["release"]["name"] = json!("v2.4.0");
        let Reading::Record(pre) = read("release", &pre, now()) else {
            panic!("a pre-release is recorded too");
        };
        assert_eq!(pre.note_text(), "Released v2.4.0 (pre-release)");

        for action in ["created", "edited", "deleted", "prereleased"] {
            let mut other = fixture("release");
            other["action"] = json!(action);
            assert!(
                matches!(read("release", &other, now()), Reading::Ignore(_)),
                "{action}"
            );
        }
    }

    #[test]
    fn a_successful_deployment_is_read_by_its_deployment_id() {
        let Reading::Record(deploy) =
            read("deployment_status", &fixture("deployment_status"), now())
        else {
            panic!("a successful deployment is recorded");
        };
        assert_eq!(deploy.kind, Kind::Deploy);
        assert_eq!(
            deploy.external_id, "990001",
            "keyed on the deployment, so a second success status is the same deploy"
        );
        assert_eq!(deploy.environment.as_deref(), Some("Production"));
        assert_eq!(deploy.git_ref.as_deref(), Some("main"));
        assert_eq!(
            deploy.url,
            "https://github.com/acme/site/commit/0d1a26e67d8f5eaf1f6ba5c57fc3c7d91ac0fd1c"
        );
        assert_eq!(deploy.title, "Deployment has completed");
        assert_eq!(deploy.actor.as_deref(), Some("vercel[bot]"));
        assert_eq!(
            deploy.note_text(),
            "Deployed 0d1a26e to Production (acme/site)"
        );

        for state in ["pending", "in_progress", "failure", "error", "inactive"] {
            let mut other = fixture("deployment_status");
            other["deployment_status"]["state"] = json!(state);
            assert!(
                matches!(read("deployment_status", &other, now()), Reading::Ignore(_)),
                "{state}"
            );
        }
    }

    #[test]
    fn installation_events_are_read() {
        assert_eq!(
            read("installation", &fixture("installation_deleted"), now()),
            Reading::InstallationDeleted {
                installation_id: 42,
                account: "acme".into()
            }
        );
        let mut suspended = fixture("installation_deleted");
        suspended["action"] = json!("suspend");
        suspended["installation"]["suspended_at"] = json!("2026-10-09T08:00:00Z");
        assert_eq!(
            read("installation", &suspended, now()),
            Reading::InstallationSuspended {
                installation_id: 42,
                at: Some(Utc.with_ymd_and_hms(2026, 10, 9, 8, 0, 0).unwrap())
            }
        );
        let mut lifted = fixture("installation_deleted");
        lifted["action"] = json!("unsuspend");
        assert_eq!(
            read("installation", &lifted, now()),
            Reading::InstallationSuspended {
                installation_id: 42,
                at: None
            }
        );
        let mut created = fixture("installation_deleted");
        created["action"] = json!("created");
        assert!(matches!(
            read("installation", &created, now()),
            Reading::InstallationChanged { installation_id: 42, ref account_type, .. }
                if account_type == "Organization"
        ));

        assert_eq!(
            read(
                "installation_repositories",
                &fixture("installation_repositories_removed"),
                now()
            ),
            Reading::ReposRemoved {
                installation_id: 42,
                repo_ids: vec![700_123]
            }
        );
        let mut added = fixture("installation_repositories_removed");
        added["action"] = json!("added");
        assert!(matches!(
            read("installation_repositories", &added, now()),
            Reading::Ignore(_)
        ));
    }

    #[test]
    fn a_ping_and_unknown_events_are_ignored_with_a_reason() {
        assert!(matches!(
            read(
                "ping",
                &json!({ "zen": "Keep it logically awesome." }),
                now()
            ),
            Reading::Ignore(_)
        ));
        assert!(
            matches!(read("star", &json!({}), now()), Reading::Ignore(why) if why.contains("star"))
        );
    }

    #[test]
    fn notes_stay_inside_the_annotation_limit() {
        let Reading::Record(mut push) = read("push", &fixture("push"), now()) else {
            panic!("a push to main is recorded");
        };
        push.title = "x".repeat(400);
        let note = push.note_text();
        assert_eq!(note.chars().count(), MAX_NOTE);
        assert!(note.ends_with('\u{2026}'));
        assert_eq!(subject("one\ntwo"), "one");
    }

    #[test]
    fn the_tally_says_what_happened_in_one_sentence_per_kind() {
        let both = Tally {
            recorded: 2,
            noted: 1,
            duplicate: 1,
            skipped: vec!["staging is not an environment the site records".into()],
        };
        let (outcome, said) = both.outcome();
        assert_eq!(outcome, "recorded");
        assert_eq!(
            said,
            "Recorded for 2 sites, 1 annotation on the chart. Already recorded for 1 site. \
             Skipped for 1 site: staging is not an environment the site records."
        );
        let again = Tally {
            duplicate: 1,
            ..Tally::default()
        };
        assert_eq!(
            again.outcome(),
            ("ignored", "Already recorded for 1 site.".to_string())
        );
    }
}
