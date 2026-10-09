//! `/api/chat/*` — the Slack, Discord and Telegram apps.
//!
//! Three webhook endpoints, one per platform, and the dashboard's endpoints
//! for linking channels to sites. Each webhook does the same three things in
//! the same order, and the order matters:
//!
//! 1. **Verify** the request with that platform's scheme ([`verify`]), on the
//!    raw body, before reading anything in it. An unverified request is a
//!    `401` and nothing else happens.
//! 2. **Acknowledge** within the platform's limit: Slack and Discord give three
//!    seconds, and a question to the AI analyst takes longer than that. Slack
//!    gets an immediate acknowledgement, Discord a deferred response (its
//!    "thinking…" state), Telegram an empty `200`.
//! 3. **Answer** on a task of its own, through the shared conversation in
//!    [`adapter`], and deliver the reply the platform's way: Slack's
//!    `response_url` or a thread reply, Discord's interaction follow-up,
//!    Telegram's `sendMessage` as a reply to the question.
//!
//! An app that is not configured has no webhook: its endpoint answers `404`,
//! exactly as if it did not exist.
//!
//! Answers run under a small concurrency limit ([`ANSWERING`]), so a burst of
//! questions in one busy channel waits its turn instead of opening a hundred
//! model calls at once.

pub mod adapter;
pub mod verify;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use micaforge_core::{Config, Error, Result};
use micaforge_store::models::chat as links;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use uuid::Uuid;

use adapter::{Command, Incoming, LinkCode, Platform};

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::admin::is_instance_admin;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every dashboard handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Answers being written at once, across every platform.
static ANSWERING: Semaphore = Semaphore::const_new(8);

/// How long a link code works.
const CODE_TTL_MINUTES: i64 = 15;

/// The code alphabet: capitals and digits without the ones people misread
/// (I, O, 0, 1). Thirty-two symbols, so a byte maps onto one without bias.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Every chat route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/chat/status", get(status))
        .route("/api/chat/links", get(list_links))
        .route("/api/chat/links/{id}", delete(unlink))
        .route("/api/chat/link-codes", post(new_code))
        .route("/api/chat/slack/commands", post(slack_command))
        .route("/api/chat/slack/events", post(slack_event))
        .route("/api/chat/discord/interactions", post(discord_interaction))
        .route("/api/chat/telegram/webhook", post(telegram_update))
        .route("/api/admin/chat/discord/commands", post(admin_discord))
        .route("/api/admin/chat/telegram/webhook", post(admin_telegram))
}

/// Run the conversation on a task of its own and deliver the reply.
fn answer_later<F, Fut>(st: AppState, incoming: Incoming, deliver: F)
where
    F: FnOnce(AppState, String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(async move {
        let Ok(_permit) = ANSWERING.acquire().await else {
            return;
        };
        let platform = incoming.platform.as_str();
        let reply = adapter::handle(&st, &incoming).await;
        if let Err(e) = deliver(st, reply).await {
            tracing::warn!(platform, error = %e, "a chat reply could not be delivered");
        }
    });
}

/// A header as text.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The `401` for an unverified request.
fn unverified() -> Response {
    (StatusCode::UNAUTHORIZED, "invalid request signature").into_response()
}

// ---------------------------------------------------------------------------
// Slack
// ---------------------------------------------------------------------------

/// The fields of a slash command this app reads.
#[derive(Debug, Default, Deserialize)]
struct SlackCommand {
    #[serde(default)]
    team_id: String,
    #[serde(default)]
    channel_id: String,
    #[serde(default)]
    channel_name: String,
    #[serde(default)]
    user_name: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    response_url: String,
}

/// Verify a Slack request, or say why not.
fn slack_verified(cfg: &Config, headers: &HeaderMap, body: &[u8]) -> Option<bool> {
    let app = cfg.chat.slack.as_ref()?;
    Some(verify::slack(
        &app.signing_secret,
        header(headers, "x-slack-request-timestamp").unwrap_or(""),
        header(headers, "x-slack-signature").unwrap_or(""),
        body,
        Utc::now().timestamp(),
    ))
}

/// `POST /api/chat/slack/commands` — `/micaforge …`.
async fn slack_command(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    match slack_verified(&st.cfg, &headers, &body) {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(false) => return unverified(),
        Some(true) => {}
    }
    let Ok(cmd) = serde_urlencoded::from_bytes::<SlackCommand>(&body) else {
        return (StatusCode::BAD_REQUEST, "not a slash command").into_response();
    };
    let command = adapter::parse(&cmd.text);
    if command == Command::Help {
        return Json(json!({
            "response_type": "ephemeral",
            "text": adapter::help(Platform::Slack),
        }))
        .into_response();
    }
    let ack = match &command {
        Command::Ask(question) => format!("Looking into: {question}"),
        _ => "Working on it.".to_string(),
    };
    let incoming = Incoming {
        platform: Platform::Slack,
        workspace_id: cmd.team_id,
        channel_id: cmd.channel_id,
        channel_label: cmd.channel_name,
        sender: cmd.user_name,
        command,
    };
    let response_url = cmd.response_url;
    answer_later(st, incoming, move |st, reply| async move {
        micaforge_jobs::chat::slack_respond(&st.http, &response_url, &reply).await
    });
    Json(json!({ "response_type": "ephemeral", "text": micaforge_jobs::chat::slack_escape(&ack) }))
        .into_response()
}

/// `POST /api/chat/slack/events` — mentions and direct messages.
async fn slack_event(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    match slack_verified(&st.cfg, &headers, &body) {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(false) => return unverified(),
        Some(true) => {}
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not an event").into_response();
    };
    match payload.get("type").and_then(Value::as_str) {
        // Slack proves the endpoint once, signed like any event.
        Some("url_verification") => {
            let challenge = payload.get("challenge").cloned().unwrap_or(Value::Null);
            return Json(json!({ "challenge": challenge })).into_response();
        }
        Some("event_callback") => {}
        _ => return StatusCode::OK.into_response(),
    }

    // Slack retries an event it thinks was missed; each is answered once.
    let event_id = payload
        .get("event_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !event_id.is_empty() {
        match st
            .kv
            .rate_limit(&format!("slack:event:{event_id}"), 1, 3600)
            .await
        {
            Ok(true) => {}
            Ok(false) => return StatusCode::OK.into_response(),
            Err(e) => tracing::warn!(error = %e, "could not deduplicate a slack event"),
        }
    }

    let event = payload.get("event").cloned().unwrap_or(Value::Null);
    let Some(incoming) = slack_incoming(&payload, &event) else {
        return StatusCode::OK.into_response();
    };
    let channel = incoming.channel_id.clone();
    let thread = event
        .get("thread_ts")
        .or_else(|| event.get("ts"))
        .and_then(Value::as_str)
        .map(str::to_string);
    answer_later(st, incoming, move |st, reply| async move {
        let app = st
            .cfg
            .chat
            .slack
            .as_ref()
            .ok_or_else(|| Error::Conflict("the Slack app is not configured".to_string()))?;
        micaforge_jobs::chat::slack_post(&st.http, app, &channel, &reply, thread.as_deref())
            .await
            .map(|_| ())
    });
    StatusCode::OK.into_response()
}

/// The message an event carries, when it is one this app answers.
///
/// An `app_mention` anywhere, or a direct message. Never a message from a bot,
/// this one included, and never an edit or a deletion: answering those is how
/// two bots talk to each other forever.
fn slack_incoming(payload: &Value, event: &Value) -> Option<Incoming> {
    let kind = event.get("type").and_then(Value::as_str)?;
    let direct =
        kind == "message" && event.get("channel_type").and_then(Value::as_str) == Some("im");
    if kind != "app_mention" && !direct {
        return None;
    }
    if event.get("bot_id").is_some() || event.get("subtype").is_some() {
        return None;
    }
    let text = strip_mentions(event.get("text").and_then(Value::as_str).unwrap_or(""));
    Some(Incoming {
        platform: Platform::Slack,
        workspace_id: payload
            .get("team_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        channel_id: event.get("channel").and_then(Value::as_str)?.to_string(),
        channel_label: String::new(),
        sender: event
            .get("user")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        command: adapter::parse(&text),
    })
}

/// Remove `<@U123>` mentions from a Slack message.
fn strip_mentions(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<@") {
        out.push_str(&rest[..start]);
        match rest[start..].find('>') {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = &rest[start..];
                break;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Discord
// ---------------------------------------------------------------------------

/// `POST /api/chat/discord/interactions`.
async fn discord_interaction(
    State(st): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(app) = st.cfg.chat.discord.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let verified = verify::discord(
        &app.public_key,
        header(&headers, "x-signature-ed25519").unwrap_or(""),
        header(&headers, "x-signature-timestamp").unwrap_or(""),
        &body,
    );
    if !verified {
        return unverified();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not an interaction").into_response();
    };
    match payload.get("type").and_then(Value::as_u64) {
        // PING, sent when the endpoint is saved and from time to time after.
        Some(1) => return Json(json!({ "type": 1 })).into_response(),
        Some(2) => {}
        _ => return (StatusCode::BAD_REQUEST, "unsupported interaction").into_response(),
    }

    let Some(command) = discord_command(&payload) else {
        return (StatusCode::BAD_REQUEST, "unknown command").into_response();
    };
    if command == Command::Help {
        // Answered at once, and only to the person who asked (flag 64).
        return Json(json!({
            "type": 4,
            "data": { "content": adapter::help(Platform::Discord), "flags": 64 }
        }))
        .into_response();
    }

    let sender = payload
        .pointer("/member/user/username")
        .or_else(|| payload.pointer("/user/username"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let incoming = Incoming {
        platform: Platform::Discord,
        workspace_id: payload
            .get("guild_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        channel_id: payload
            .get("channel_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        channel_label: payload
            .pointer("/channel/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        sender,
        command,
    };
    let token = payload
        .get("token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    answer_later(st, incoming, move |st, reply| async move {
        let app = st
            .cfg
            .chat
            .discord
            .as_ref()
            .ok_or_else(|| Error::Conflict("the Discord app is not configured".to_string()))?;
        micaforge_jobs::chat::discord_edit_original(&st.http, app, &token, &reply).await
    });
    // Type 5: "thinking…", replaced by the follow-up within fifteen minutes.
    Json(json!({ "type": 5 })).into_response()
}

/// The command a `/micaforge <sub>` interaction names.
fn discord_command(payload: &Value) -> Option<Command> {
    if payload.pointer("/data/name").and_then(Value::as_str) != Some("micaforge") {
        return None;
    }
    let sub = payload.pointer("/data/options/0")?;
    let option = |name: &str| {
        sub.get("options")
            .and_then(Value::as_array)
            .and_then(|opts| {
                opts.iter()
                    .find(|o| o.get("name").and_then(Value::as_str) == Some(name))
            })
            .and_then(|o| o.get("value"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Some(match sub.get("name").and_then(Value::as_str)? {
        "ask" => {
            let question = option("question");
            if question.is_empty() {
                Command::Help
            } else {
                Command::Ask(question)
            }
        }
        "stats" => Command::Stats,
        "link" => Command::Link(option("code")),
        "unlink" => Command::Unlink,
        "feedback" => Command::Feedback(option("text")),
        _ => Command::Help,
    })
}

// ---------------------------------------------------------------------------
// Telegram
// ---------------------------------------------------------------------------

/// `POST /api/chat/telegram/webhook`.
async fn telegram_update(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(app) = st.cfg.chat.telegram.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !verify::telegram(
        &app.webhook_secret,
        header(&headers, "x-telegram-bot-api-secret-token"),
    ) {
        return unverified();
    }
    let Ok(update) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::OK.into_response();
    };
    let Some(message) = update.get("message") else {
        return StatusCode::OK.into_response();
    };
    let text = message
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() || message.pointer("/from/is_bot").and_then(Value::as_bool) == Some(true) {
        return StatusCode::OK.into_response();
    }
    let Some(chat_id) = message.pointer("/chat/id").map(|v| v.to_string()) else {
        return StatusCode::OK.into_response();
    };
    let private = message.pointer("/chat/type").and_then(Value::as_str) == Some("private");
    let command = adapter::parse(text);

    // In a group, plain chatter is a question only where the chat is linked:
    // a bot that answered "this chat is not linked" to every message in an
    // unlinked group would be removed from it within the hour.
    if !private && !text.starts_with('/') {
        match links::find(&st.pg, "telegram", "", &chat_id).await {
            Ok(Some(_)) => {}
            Ok(None) => return StatusCode::OK.into_response(),
            Err(e) => {
                tracing::warn!(error = %e, "could not read a telegram link");
                return StatusCode::OK.into_response();
            }
        }
    }

    let incoming = Incoming {
        platform: Platform::Telegram,
        workspace_id: String::new(),
        channel_id: chat_id.clone(),
        channel_label: message
            .pointer("/chat/title")
            .or_else(|| message.pointer("/chat/username"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        sender: message
            .pointer("/from/username")
            .or_else(|| message.pointer("/from/first_name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        command,
    };
    let reply_to = message.get("message_id").map(|v| v.to_string());
    answer_later(st, incoming, move |st, reply| async move {
        let app = st
            .cfg
            .chat
            .telegram
            .as_ref()
            .ok_or_else(|| Error::Conflict("the Telegram bot is not configured".to_string()))?;
        micaforge_jobs::chat::telegram_send(
            &st.http,
            &app.bot_token,
            &chat_id,
            &reply,
            reply_to.as_deref(),
        )
        .await
        .map(|_| ())
    });
    StatusCode::OK.into_response()
}

// ---------------------------------------------------------------------------
// The dashboard
// ---------------------------------------------------------------------------

/// `GET /api/chat/status` — which apps this install has, and where they point.
async fn status(State(st): State<AppState>, _auth: AuthUser) -> Json<Value> {
    let base = st.cfg.public_url.trim_end_matches('/');
    let chat = &st.cfg.chat;
    ok(json!({
        "slack": {
            "configured": chat.slack.is_some(),
            "commands_url": format!("{base}/api/chat/slack/commands"),
            "events_url": format!("{base}/api/chat/slack/events"),
        },
        "discord": {
            "configured": chat.discord.is_some(),
            "interactions_url": format!("{base}/api/chat/discord/interactions"),
            "application_id": chat.discord.as_ref().map(|d| d.application_id.clone()),
        },
        "telegram": {
            "configured": chat.telegram.is_some(),
            "webhook_url": format!("{base}/api/chat/telegram/webhook"),
        },
        "ai_configured": st.cfg.llm.is_some(),
    }))
}

/// `GET /api/chat/links?site=`.
async fn list_links(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = links::list_for_site(&st.pg, access.site.id).await?;
    Ok(ok(rows
        .iter()
        .map(|l| {
            json!({
                "id": l.id,
                "platform": l.platform,
                "workspace_id": l.workspace_id,
                "channel_id": l.channel_id,
                "label": l.label,
                "created_at": l.created_at,
                "created_by": l.created_by_name,
            })
        })
        .collect::<Vec<_>>()))
}

/// A fresh link code: eight symbols from [`CODE_ALPHABET`].
fn mint_code() -> String {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&Uuid::new_v4().as_bytes()[..8]);
    bytes
        .iter()
        .map(|b| CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()] as char)
        .collect()
}

/// `POST /api/chat/link-codes?site=` — a one-time code to link a channel.
async fn new_code(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    require_at_least(access.role.as_str(), MEMBER)?;
    let code = mint_code();
    let grant = LinkCode {
        site: access.site.id,
        org: access.site.org_id,
        user: auth.user.id,
    };
    st.kv
        .set_json(
            &adapter::code_key(&code),
            &grant,
            Some((CODE_TTL_MINUTES * 60) as u32),
        )
        .await?;
    Ok(ok(json!({
        "code": code,
        "expires_at": Utc::now() + Duration::minutes(CODE_TTL_MINUTES),
        "commands": {
            "slack": format!("/micaforge link {code}"),
            "discord": format!("/micaforge link code:{code}"),
            "telegram": format!("/link {code}"),
        },
    })))
}

/// `DELETE /api/chat/links/{id}`.
async fn unlink(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let link = links::get(&st.pg, id).await?;
    match authorize_site(&st.pg, link.site_id, auth.user.id, MEMBER).await {
        Ok(_) => {}
        Err(Error::NotFound(_)) => return Err(Error::not_found("chat link").into()),
        Err(e) => return Err(e.into()),
    }
    links::delete(&st.pg, link.id).await?;
    Ok(ok(json!({ "deleted": link.id })))
}

// ---------------------------------------------------------------------------
// Setting the apps up
// ---------------------------------------------------------------------------

/// Register the Discord `/micaforge` command. Also `chat-setup discord`.
pub async fn register_discord(cfg: &Config, http: &reqwest::Client) -> Result<usize> {
    let app = cfg.chat.discord.as_ref().ok_or_else(|| {
        Error::Conflict("the Discord app is not configured; set MICAFORGE_DISCORD_*".to_string())
    })?;
    micaforge_jobs::chat::discord_register_commands(http, app).await
}

/// Point the Telegram bot at this server. Also `chat-setup telegram`.
pub async fn set_telegram_webhook(cfg: &Config, http: &reqwest::Client) -> Result<String> {
    let app = cfg.chat.telegram.as_ref().ok_or_else(|| {
        Error::Conflict("the Telegram bot is not configured; set MICAFORGE_TELEGRAM_*".to_string())
    })?;
    let url = format!(
        "{}/api/chat/telegram/webhook",
        cfg.public_url.trim_end_matches('/')
    );
    micaforge_jobs::chat::telegram_set_webhook(http, &app.bot_token, &url, &app.webhook_secret)
        .await?;
    Ok(url)
}

/// Refuse anyone but an instance admin, signed in.
async fn require_admin(st: &AppState, auth: &AuthUser) -> Result<()> {
    auth.require_session()?;
    if is_instance_admin(st, &auth.user).await? {
        Ok(())
    } else {
        Err(Error::Forbidden(
            "setting up the chat apps needs an instance administrator".to_string(),
        ))
    }
}

/// `POST /api/admin/chat/discord/commands`.
async fn admin_discord(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;
    let registered = register_discord(&st.cfg, &st.http).await?;
    Ok(ok(json!({ "registered": registered })))
}

/// `POST /api/admin/chat/telegram/webhook`.
async fn admin_telegram(State(st): State<AppState>, auth: AuthUser) -> ApiResult<Json<Value>> {
    require_admin(&st, &auth).await?;
    let url = set_telegram_webhook(&st.cfg, &st.http).await?;
    Ok(ok(json!({ "ok": true, "url": url })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_codes_use_the_unambiguous_alphabet() {
        for _ in 0..50 {
            let code = mint_code();
            assert_eq!(code.len(), 8);
            assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
            assert!(!code.contains('O') && !code.contains('0') && !code.contains('I'));
        }
    }

    #[test]
    fn slack_mentions_are_removed_before_the_command_is_read() {
        assert_eq!(strip_mentions("<@U123ABC> stats"), "stats");
        assert_eq!(
            strip_mentions("hey <@U1> how was <@U2> last week?"),
            "hey how was last week?"
        );
        assert_eq!(strip_mentions("no mention"), "no mention");
        assert_eq!(strip_mentions("broken <@U1"), "broken <@U1");
    }

    #[test]
    fn only_mentions_and_direct_messages_from_people_are_answered() {
        let payload = json!({ "team_id": "T1" });
        let mention = json!({ "type": "app_mention", "channel": "C1", "user": "U1", "text": "<@B1> stats", "ts": "1.1" });
        let incoming = slack_incoming(&payload, &mention).unwrap();
        assert_eq!(incoming.workspace_id, "T1");
        assert_eq!(incoming.command, Command::Stats);

        let dm = json!({ "type": "message", "channel_type": "im", "channel": "D1", "user": "U1", "text": "how many visitors?" });
        assert!(matches!(
            slack_incoming(&payload, &dm).unwrap().command,
            Command::Ask(_)
        ));

        let channel_chatter =
            json!({ "type": "message", "channel_type": "channel", "channel": "C1", "text": "hi" });
        assert!(slack_incoming(&payload, &channel_chatter).is_none());
        let from_a_bot = json!({ "type": "app_mention", "channel": "C1", "bot_id": "B2", "text": "<@B1> stats" });
        assert!(slack_incoming(&payload, &from_a_bot).is_none());
        let an_edit = json!({ "type": "message", "channel_type": "im", "subtype": "message_changed", "channel": "D1" });
        assert!(slack_incoming(&payload, &an_edit).is_none());
    }

    #[test]
    fn discord_subcommands_map_onto_the_shared_commands() {
        let ask = json!({ "data": { "name": "micaforge", "options": [
            { "name": "ask", "type": 1, "options": [{ "name": "question", "type": 3, "value": " which pages grew? " }] }
        ]}});
        assert_eq!(
            discord_command(&ask),
            Some(Command::Ask("which pages grew?".into()))
        );
        let link = json!({ "data": { "name": "micaforge", "options": [
            { "name": "link", "type": 1, "options": [{ "name": "code", "type": 3, "value": "K7Q2M9XP" }] }
        ]}});
        assert_eq!(
            discord_command(&link),
            Some(Command::Link("K7Q2M9XP".into()))
        );
        let stats =
            json!({ "data": { "name": "micaforge", "options": [{ "name": "stats", "type": 1 }] }});
        assert_eq!(discord_command(&stats), Some(Command::Stats));
        let other = json!({ "data": { "name": "something-else" } });
        assert_eq!(discord_command(&other), None);
    }
}
