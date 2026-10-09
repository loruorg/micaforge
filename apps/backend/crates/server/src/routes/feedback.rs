//! `/api/feedback` and `/api/admin/feedback` — reports from people and agents.
//!
//! Four ways in, one way to store: the dashboard's "Send feedback" dialog, the
//! API with a key, the MCP `submit_feedback` tool, and a chat app's
//! `feedback` command all end in [`submit`], so a report reads the same in the
//! triage screen whoever sent it.
//!
//! ## Who may read what
//!
//! The whole rule is [`may_read`], and it is deliberately narrow. An instance
//! admin reads every report: they run the install the reports are about. A
//! reporter reads their own, and "own" is precise: a person's reports are the
//! ones they sent signed in; an API key's are the ones *that key* sent. A key
//! resolves against its organisation's owner, and the owner's own reports are
//! not the key's to read, nor the key's reports the owner's by default. Nobody
//! else reads anything, organisation admins included: a report can quote a
//! page address or a configuration detail its author did not mean to share
//! with colleagues.
//!
//! Triage (status, severity) is for instance admins only, signed in.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use micaforge_core::{new_id, Error, Result};
use micaforge_mail::FeedbackPayload;
use micaforge_store::models::feedback::{self, Feedback, Filter, NewFeedback};
use micaforge_store::models::sites::Site;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthMethod, AuthUser};
use crate::response::{ok, ok_meta};
use crate::routes::admin::{instance_admin_addresses, is_instance_admin};
use crate::routes::orgs::VIEWER;
use crate::routes::sites::authorize_site_ref;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Report kinds.
pub const KINDS: &[&str] = &["bug", "problem", "feature", "other"];
/// Triage states.
pub const STATUSES: &[&str] = &["new", "triaged", "planned", "done", "wont_fix"];
/// Severities.
pub const SEVERITIES: &[&str] = &["low", "medium", "high", "critical"];
/// Where a report came from.
pub const SOURCES: &[&str] = &["dashboard", "mcp", "api", "chat"];

/// Reports one reporter may send in an hour.
const HOURLY_LIMIT: u32 = 20;
/// Longest title, in characters.
const MAX_TITLE: usize = 140;
/// Longest body, in characters.
const MAX_BODY: usize = 5_000;

/// Every feedback route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/feedback", post(create))
        .route("/api/feedback/mine", get(mine))
        .route("/api/admin/feedback", get(triage_list))
        .route("/api/admin/feedback/{id}", patch(triage_update))
}

// ---------------------------------------------------------------------------
// Who is reporting
// ---------------------------------------------------------------------------

/// The author of a report, as precisely as the credential allows.
#[derive(Debug, Clone, Default)]
pub struct Reporter {
    /// `dashboard` | `mcp` | `api` | `chat`.
    pub source: &'static str,
    /// The account, for a person or an OAuth client acting for one.
    pub user: Option<(Uuid, String, String)>,
    /// An MCP client's own name.
    pub client_name: Option<String>,
    /// The API key an agent used.
    pub api_key_id: Option<Uuid>,
    /// The OAuth grant an MCP client used.
    pub oauth_grant_id: Option<Uuid>,
    /// Platform and sender of a chat message.
    pub chat: Option<String>,
    /// The organisation the credential belongs to.
    pub org_id: Option<Uuid>,
}

impl Reporter {
    /// The reporter behind an authenticated request.
    ///
    /// `via_mcp` marks a report that came through the MCP endpoint, whatever
    /// the credential; `client_name` is the MCP client's `clientInfo.name`.
    pub fn from_auth(auth: &AuthUser, via_mcp: bool, client_name: Option<String>) -> Reporter {
        let person = || {
            Some((
                auth.user.id,
                auth.user.email.clone(),
                auth.user.name.clone(),
            ))
        };
        match auth.method {
            AuthMethod::Session(_) => Reporter {
                source: if via_mcp { "mcp" } else { "dashboard" },
                user: person(),
                client_name,
                ..Reporter::default()
            },
            // A key is not its owner, so the owner is not recorded as the
            // reporter; the key is.
            AuthMethod::ApiKey(key) => Reporter {
                source: if via_mcp { "mcp" } else { "api" },
                api_key_id: Some(key),
                org_id: auth.key.as_ref().map(|g| g.org_id),
                client_name,
                ..Reporter::default()
            },
            AuthMethod::OAuth(grant) => Reporter {
                source: "mcp",
                user: person(),
                oauth_grant_id: Some(grant),
                org_id: auth.key.as_ref().map(|g| g.org_id),
                client_name,
                ..Reporter::default()
            },
        }
    }

    /// The rate-limit bucket: the narrowest identity the reporter has.
    fn bucket(&self) -> String {
        if let Some(key) = self.api_key_id {
            format!("key:{key}")
        } else if let Some((user, ..)) = &self.user {
            format!("user:{user}")
        } else if let Some(chat) = &self.chat {
            format!("chat:{chat}")
        } else {
            "anonymous".to_string()
        }
    }

    /// The reporter in words, for the admins' notice.
    fn words(&self) -> String {
        let who = match (&self.user, &self.chat, self.api_key_id) {
            (Some((_, email, _)), _, _) => email.clone(),
            (None, Some(chat), _) => chat.clone(),
            (None, None, Some(key)) => format!("API key {key}"),
            _ => "an unknown reporter".to_string(),
        };
        match &self.client_name {
            Some(client) => format!("{client} ({who})"),
            None => who,
        }
    }
}

/// Who is reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    /// The account, for a session.
    pub user_id: Option<Uuid>,
    /// The key, for an API key.
    pub api_key_id: Option<Uuid>,
    /// An instance admin, signed in.
    pub admin: bool,
}

/// May `viewer` read `report`? See the module notes.
pub fn may_read(viewer: &Viewer, report: &Feedback) -> bool {
    if viewer.admin {
        return true;
    }
    match viewer.api_key_id {
        Some(key) => report.api_key_id == Some(key),
        None => {
            report.api_key_id.is_none()
                && report.reporter_user_id.is_some()
                && report.reporter_user_id == viewer.user_id
        }
    }
}

/// May this caller triage? An instance admin, signed in, and nobody else.
pub fn may_triage(is_admin: bool, signed_in: bool) -> bool {
    is_admin && signed_in
}

// ---------------------------------------------------------------------------
// Submitting
// ---------------------------------------------------------------------------

/// A report as any of the four front doors receives it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Input {
    /// `bug` | `problem` | `feature` | `other`.
    pub kind: String,
    /// One line.
    pub title: String,
    /// The details.
    #[serde(default)]
    pub body: String,
    /// Optional severity.
    #[serde(default)]
    pub severity: Option<String>,
    /// The site it is about, numeric or UUID.
    #[serde(default)]
    pub site: Option<Value>,
    /// The page it was sent from.
    #[serde(default)]
    pub page_url: Option<String>,
    /// The client's version.
    #[serde(default)]
    pub app_version: Option<String>,
}

/// One of a closed vocabulary, or a validation error naming the field.
fn one_of(field: &str, raw: &str, allowed: &'static [&'static str]) -> Result<&'static str> {
    let wanted = raw.trim().to_lowercase();
    allowed
        .iter()
        .copied()
        .find(|a| *a == wanted)
        .ok_or_else(|| Error::validation(field, format!("must be one of {}", allowed.join(", "))))
}

/// Trim and bound a free text field.
fn text(field: &str, raw: &str, min: usize, max: usize) -> Result<String> {
    let trimmed = raw.trim();
    let count = trimmed.chars().count();
    if count < min || count > max {
        return Err(Error::validation(
            field,
            format!("must be {min} to {max} characters"),
        ));
    }
    Ok(trimmed.to_string())
}

/// An optional short field: trimmed, empty is absent, longer than `max` cut.
fn short(raw: Option<&str>, max: usize) -> Option<String> {
    raw.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.chars().take(max).collect())
}

/// Store a report, then tell the people who triage it.
///
/// `site` must already be authorised for the reporter, or be `None`. The
/// notice goes out on a task of its own: a slow mail relay must not hold the
/// reporter's request open.
pub async fn submit(
    st: &AppState,
    reporter: &Reporter,
    input: &Input,
    site: Option<&Site>,
    user_agent: Option<&str>,
) -> Result<Feedback> {
    let kind = one_of("kind", &input.kind, KINDS)?;
    let title = text("title", &input.title, 3, MAX_TITLE)?;
    let body = text("body", &input.body, 1, MAX_BODY)?;
    let severity = match input.severity.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(raw) => Some(one_of("severity", raw, SEVERITIES)?),
        None => None,
    };
    let page_url = short(input.page_url.as_deref(), 2_000)
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"));

    if !st
        .kv
        .rate_limit(
            &format!("feedback:{}", reporter.bucket()),
            HOURLY_LIMIT,
            3600,
        )
        .await?
    {
        return Err(Error::RateLimited { retry_after: 3600 });
    }

    let (user_id, email, name) = match &reporter.user {
        Some((id, email, name)) => (Some(*id), Some(email.as_str()), Some(name.as_str())),
        None => (None, None, None),
    };
    let app_version = short(input.app_version.as_deref(), 40);
    let agent = short(user_agent, 400);
    let new = NewFeedback {
        kind,
        title: &title,
        body: &body,
        severity,
        source: reporter.source,
        reporter_user_id: user_id,
        reporter_email: email,
        reporter_name: name,
        client_name: reporter.client_name.as_deref(),
        api_key_id: reporter.api_key_id,
        oauth_grant_id: reporter.oauth_grant_id,
        chat_reporter: reporter.chat.as_deref(),
        org_id: site.map(|s| s.org_id).or(reporter.org_id),
        site_id: site.map(|s| s.id),
        page_url: page_url.as_deref(),
        app_version: app_version.as_deref(),
        user_agent: agent.as_deref(),
    };
    let stored = feedback::create(&st.pg, new_id(), &new).await?;

    let notice = FeedbackPayload {
        kind: stored.kind.clone(),
        title: stored.title.clone(),
        body: stored.body.clone(),
        source: stored.source.clone(),
        reporter: reporter.words(),
        site: stored.site_domain.clone(),
        page_url: stored.page_url.clone(),
        link: format!("{}/app/admin", st.cfg.dashboard_url.trim_end_matches('/')),
    };
    let st2 = st.clone();
    tokio::spawn(async move { announce(&st2, &notice).await });
    Ok(stored)
}

/// Mail the instance admins and post to the instance feedback channel.
async fn announce(st: &AppState, notice: &FeedbackPayload) {
    if st.mailer.is_sending() {
        match instance_admin_addresses(st).await {
            Ok(addresses) => {
                for address in addresses {
                    if let Err(e) = st.mailer.send_feedback_notice(&address, notice).await {
                        tracing::warn!(error = %e, "feedback notice could not be mailed");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not list the instance admins"),
        }
    }

    let text = format!(
        "New feedback ({}, via {}) from {}: {}\n{}\n{}",
        notice.kind,
        notice.source,
        notice.reporter,
        notice.title,
        micaforge_jobs::chat::fit(&notice.body, 600),
        notice.link
    );
    let channel = &st.cfg.feedback_channel;
    if let Some(url) = channel.webhook_url.as_deref() {
        let sent = if micaforge_jobs::chat::is_discord_webhook(url) {
            micaforge_jobs::chat::discord_webhook(&st.http, url, &text).await
        } else {
            micaforge_jobs::outbound::post_json(
                &st.http,
                url,
                &json!({ "text": micaforge_jobs::chat::slack_escape(&text) }),
                std::time::Duration::from_secs(10),
            )
            .await
            .map(|_| ())
            .map_err(Error::from)
        };
        if let Err(e) = sent {
            tracing::warn!(error = %e, "feedback channel post failed");
        }
    }
    if let (Some(chat_id), Some(app)) = (
        channel.telegram_chat_id.as_deref(),
        st.cfg.chat.telegram.as_ref(),
    ) {
        if let Err(e) =
            micaforge_jobs::chat::telegram_send(&st.http, &app.bot_token, chat_id, &text, None)
                .await
        {
            tracing::warn!(error = %e, "feedback telegram post failed");
        }
    }
}

/// The site named by an input, authorised for a viewer.
pub async fn site_of(st: &AppState, auth: &AuthUser, raw: Option<&Value>) -> Result<Option<Site>> {
    let reference = match raw {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
        Some(Value::String(_)) => return Ok(None),
        Some(_) => return Err(Error::validation("site", "must be a site id")),
    };
    Ok(Some(
        authorize_site_ref(st, &reference, auth.user.id, VIEWER).await?,
    ))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/feedback`.
async fn create(
    State(st): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(input): Json<Input>,
) -> ApiResult<Json<Value>> {
    let site = site_of(&st, &auth, input.site.as_ref()).await?;
    let reporter = Reporter::from_auth(&auth, false, None);
    let agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    let stored = submit(&st, &reporter, &input, site.as_ref(), agent).await?;
    Ok(ok(feedback_json(&stored)))
}

/// The caller as a [`Viewer`].
pub fn viewer_of(auth: &AuthUser, admin: bool) -> Viewer {
    match auth.method {
        AuthMethod::ApiKey(key) => Viewer {
            user_id: None,
            api_key_id: Some(key),
            admin: false,
        },
        AuthMethod::Session(_) | AuthMethod::OAuth(_) => Viewer {
            user_id: Some(auth.user.id),
            api_key_id: None,
            admin: admin && matches!(auth.method, AuthMethod::Session(_)),
        },
    }
}

/// The caller's own reports, newest first.
pub async fn list_mine(st: &AppState, auth: &AuthUser) -> Result<Vec<Feedback>> {
    let viewer = viewer_of(auth, false);
    let rows = match viewer.api_key_id {
        Some(key) => feedback::for_api_key(&st.pg, key, 100).await?,
        None => feedback::for_user(&st.pg, auth.user.id, 100).await?,
    };
    Ok(rows.into_iter().filter(|r| may_read(&viewer, r)).collect())
}

/// `GET /api/feedback/mine`.
async fn mine(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    let rows = list_mine(&st, &auth).await?;
    Ok(ok(rows.iter().map(feedback_json).collect::<Vec<_>>()))
}

/// The triage filters.
#[derive(Debug, Deserialize)]
struct TriageQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    page: Option<i64>,
}

/// Refuse anyone who may not triage.
async fn require_triage(st: &AppState, auth: &AuthUser) -> Result<()> {
    let signed_in = matches!(auth.method, AuthMethod::Session(_));
    let admin = signed_in && is_instance_admin(st, &auth.user).await?;
    if may_triage(admin, signed_in) {
        Ok(())
    } else {
        Err(Error::Forbidden(
            "triaging feedback needs an instance administrator".to_string(),
        ))
    }
}

/// An optional filter value from the vocabulary; empty and `all` are absent.
fn filter_value(
    field: &str,
    raw: Option<&str>,
    allowed: &'static [&'static str],
) -> Result<Option<&'static str>> {
    match raw.map(str::trim).filter(|v| !v.is_empty() && *v != "all") {
        Some(value) => Ok(Some(one_of(field, value, allowed)?)),
        None => Ok(None),
    }
}

/// `GET /api/admin/feedback`.
async fn triage_list(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<TriageQuery>,
) -> ApiResult<Json<Value>> {
    require_triage(&st, &auth).await?;
    let filter = Filter {
        status: filter_value("status", q.status.as_deref(), STATUSES)?,
        kind: filter_value("kind", q.kind.as_deref(), KINDS)?,
        source: filter_value("source", q.source.as_deref(), SOURCES)?,
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let page = q.page.unwrap_or(1).max(1);
    let (rows, total) = feedback::list(&st.pg, &filter, limit, (page - 1) * limit).await?;
    Ok(ok_meta(
        rows.iter().map(feedback_json).collect::<Vec<_>>(),
        json!({ "rows": rows.len(), "total": total }),
    ))
}

/// A triage change.
#[derive(Debug, Deserialize)]
struct TriageBody {
    #[serde(default)]
    status: Option<String>,
    /// `null` clears the severity; absent leaves it.
    #[serde(default, deserialize_with = "present")]
    severity: Option<Option<String>>,
}

/// Tell an absent field from an explicit `null`.
fn present<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

/// `PATCH /api/admin/feedback/{id}`.
async fn triage_update(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<TriageBody>,
) -> ApiResult<Json<Value>> {
    require_triage(&st, &auth).await?;
    let status = match body.status.as_deref() {
        Some(raw) => Some(one_of("status", raw, STATUSES)?),
        None => None,
    };
    let severity = match &body.severity {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => Some(Some(one_of("severity", raw, SEVERITIES)?)),
    };
    let updated = feedback::update(&st.pg, id, status, severity).await?;
    Ok(ok(feedback_json(&updated)))
}

/// One report on the wire.
pub fn feedback_json(f: &Feedback) -> Value {
    json!({
        "id": f.id,
        "kind": f.kind,
        "title": f.title,
        "body": f.body,
        "status": f.status,
        "severity": f.severity,
        "source": f.source,
        "reporter": {
            "user_id": f.reporter_user_id,
            "email": f.reporter_email,
            "name": f.reporter_name,
            "client_name": f.client_name,
            "api_key_id": f.api_key_id,
            "chat": f.chat_reporter,
        },
        "org": f.org_id.map(|id| json!({ "id": id, "name": f.org_name })),
        "site": f.site_number.map(|n| json!({ "id": n, "domain": f.site_domain })),
        "page_url": f.page_url,
        "app_version": f.app_version,
        "user_agent": f.user_agent,
        "created_at": f.created_at,
        "updated_at": f.updated_at,
        "github_issue": f.github_issue_url.as_ref().map(|url| json!({
            "url": url,
            "number": f.github_issue_number,
            "repo": f.github_issue_repo,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn report(user: Option<Uuid>, key: Option<Uuid>) -> Feedback {
        Feedback {
            id: Uuid::from_u128(1),
            kind: "bug".into(),
            title: "t".into(),
            body: "b".into(),
            status: "new".into(),
            severity: None,
            source: "api".into(),
            reporter_user_id: user,
            reporter_email: None,
            reporter_name: None,
            client_name: None,
            api_key_id: key,
            oauth_grant_id: None,
            chat_reporter: None,
            org_id: None,
            org_name: None,
            site_id: None,
            site_number: None,
            site_domain: None,
            page_url: None,
            app_version: None,
            user_agent: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            github_issue_url: None,
            github_issue_number: None,
            github_issue_repo: None,
        }
    }

    #[test]
    fn a_person_reads_their_own_reports_and_no_one_elses() {
        let ada = Uuid::from_u128(10);
        let bob = Uuid::from_u128(11);
        let viewer = Viewer {
            user_id: Some(ada),
            api_key_id: None,
            admin: false,
        };
        assert!(may_read(&viewer, &report(Some(ada), None)));
        assert!(!may_read(&viewer, &report(Some(bob), None)));
        // A chat report has no account, so no signed-in person owns it.
        assert!(!may_read(&viewer, &report(None, None)));
    }

    #[test]
    fn a_key_reads_what_it_sent_and_not_its_owners_reports() {
        let owner = Uuid::from_u128(10);
        let key = Uuid::from_u128(20);
        let other_key = Uuid::from_u128(21);
        let as_key = Viewer {
            user_id: None,
            api_key_id: Some(key),
            admin: false,
        };
        assert!(may_read(&as_key, &report(None, Some(key))));
        assert!(!may_read(&as_key, &report(None, Some(other_key))));
        assert!(!may_read(&as_key, &report(Some(owner), None)));

        // And the owner, signed in, does not read the key's reports either.
        let as_owner = Viewer {
            user_id: Some(owner),
            api_key_id: None,
            admin: false,
        };
        assert!(!may_read(&as_owner, &report(Some(owner), Some(key))));
    }

    #[test]
    fn an_instance_admin_reads_everything_and_only_they_triage() {
        let admin = Viewer {
            user_id: Some(Uuid::from_u128(1)),
            api_key_id: None,
            admin: true,
        };
        assert!(may_read(&admin, &report(Some(Uuid::from_u128(9)), None)));
        assert!(may_read(&admin, &report(None, Some(Uuid::from_u128(8)))));
        assert!(may_triage(true, true));
        assert!(!may_triage(true, false), "an admin's key never triages");
        assert!(!may_triage(false, true));
    }

    #[test]
    fn input_vocabularies_and_lengths_are_checked() {
        assert_eq!(one_of("kind", " Bug ", KINDS).unwrap(), "bug");
        assert_eq!(
            one_of("kind", "rant", KINDS).unwrap_err().field(),
            Some("kind")
        );
        assert!(text("title", "ok", 3, 140).is_err());
        assert_eq!(
            text("title", "  fine title ", 3, 140).unwrap(),
            "fine title"
        );
        assert!(text("body", &"x".repeat(5_001), 1, 5_000).is_err());
        assert_eq!(short(Some("  "), 10), None);
        assert_eq!(short(Some("abcdef"), 3).as_deref(), Some("abc"));
        assert_eq!(filter_value("status", Some("all"), STATUSES).unwrap(), None);
        assert!(filter_value("status", Some("closed"), STATUSES).is_err());
    }

    #[test]
    fn the_rate_limit_bucket_is_the_narrowest_identity() {
        let key = Uuid::from_u128(5);
        let r = Reporter {
            api_key_id: Some(key),
            user: Some((Uuid::from_u128(6), "a@b.c".into(), "A".into())),
            ..Reporter::default()
        };
        assert_eq!(r.bucket(), format!("key:{key}"));
        let chat = Reporter {
            chat: Some("slack:T/C U1".into()),
            ..Reporter::default()
        };
        assert_eq!(chat.bucket(), "chat:slack:T/C U1");
    }
}

/// The authorisation rules against real stores. See
/// [`crate::routes::oauth::harness`] for how to run these.
#[cfg(test)]
mod live {
    use axum::body::Body;
    use axum::http::Request;
    use micaforge_core::{crypto, new_id};
    use micaforge_store::models::keys;
    use serde_json::json;

    use crate::routes::oauth::harness::Harness;

    fn post(path: &str, auth: (&str, &str), body: serde_json::Value) -> Request<Body> {
        Request::post(path)
            .header(auth.0, auth.1)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(path: &str, auth: (&str, &str)) -> Request<Body> {
        Request::get(path)
            .header(auth.0, auth.1)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn reporters_read_their_own_and_only_instance_admins_triage() {
        let h = Harness::new().await;
        // The first account on an install is its instance admin.
        let ada = h.person("ada@example.com").await;
        let bob = h.person("bob@example.com").await;
        let as_ada = ("cookie", ada.cookie.as_str());
        let as_bob = ("cookie", bob.cookie.as_str());

        let (status, _, sent) = h
            .send(post(
                "/api/feedback",
                as_bob,
                json!({ "kind": "bug", "title": "The chart is empty", "body": "Since Monday.",
                        "page_url": "https://app.example.com/app/1" }),
            ))
            .await;
        assert_eq!(status, 200, "{sent}");
        assert_eq!(sent["data"]["source"], "dashboard");
        assert_eq!(sent["data"]["reporter"]["email"], "bob@example.com");
        let id = sent["data"]["id"].as_str().unwrap().to_string();

        // A key in Bob's organisation reports as itself, not as Bob.
        let token = format!("mfg_{}", crypto::random_token(32));
        keys::create(
            &h.st.pg,
            new_id(),
            bob.org,
            None,
            "agent",
            &token.chars().take(16).collect::<String>(),
            &crypto::token_hash(&token),
            &json!(["stats:read"]),
            None,
        )
        .await
        .unwrap();
        let bearer = format!("Bearer {token}");
        let as_key = ("authorization", bearer.as_str());
        let (status, _, by_key) = h
            .send(post(
                "/api/feedback",
                as_key,
                json!({ "kind": "feature", "title": "Export to Parquet", "body": "For a warehouse." }),
            ))
            .await;
        assert_eq!(status, 200, "{by_key}");
        assert_eq!(by_key["data"]["source"], "api");
        assert!(by_key["data"]["reporter"]["user_id"].is_null());

        let (_, _, bobs) = h.send(get("/api/feedback/mine", as_bob)).await;
        assert_eq!(
            bobs["data"].as_array().unwrap().len(),
            1,
            "the key's report is not Bob's"
        );
        let (_, _, keys_own) = h.send(get("/api/feedback/mine", as_key)).await;
        assert_eq!(keys_own["data"].as_array().unwrap().len(), 1);
        assert_eq!(keys_own["data"][0]["title"], "Export to Parquet");
        let (_, _, adas) = h.send(get("/api/feedback/mine", as_ada)).await;
        assert!(adas["data"].as_array().unwrap().is_empty());

        // Triage: the instance admin, signed in, and nobody else.
        let (status, _, _) = h.send(get("/api/admin/feedback", as_bob)).await;
        assert_eq!(status, 403);
        let (status, _, _) = h.send(get("/api/admin/feedback", as_key)).await;
        assert_eq!(status, 403);
        let (status, _, all) = h.send(get("/api/admin/feedback?kind=bug", as_ada)).await;
        assert_eq!(status, 200);
        assert_eq!(all["meta"]["total"], 1);

        let patch = |who: (&'static str, String)| {
            Request::patch(format!("/api/admin/feedback/{id}"))
                .header(who.0, who.1)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "status": "triaged", "severity": "high" }).to_string(),
                ))
                .unwrap()
        };
        let (status, _, _) = h.send(patch(("cookie", bob.cookie.clone()))).await;
        assert_eq!(status, 403);
        let (status, _, triaged) = h.send(patch(("cookie", ada.cookie.clone()))).await;
        assert_eq!(status, 200);
        assert_eq!(triaged["data"]["status"], "triaged");
        assert_eq!(triaged["data"]["severity"], "high");

        // A report about a site the reporter cannot read is refused as missing.
        let (status, _, _) = h
            .send(post(
                "/api/feedback",
                as_bob,
                json!({ "kind": "bug", "title": "Other site", "body": "x", "site": 4242 }),
            ))
            .await;
        assert_eq!(status, 404);

        h.finish().await;
    }
}
