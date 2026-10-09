//! Sending messages to Slack, Discord and Telegram.
//!
//! One module for the three platforms' outbound calls, used by the worker
//! (investigations posted to linked channels, alerts, the feedback channel)
//! and by the server (answers to questions asked in a channel). Each platform
//! gets the same three things: a text-length ceiling it enforces, an escape
//! rule for the text, and one call that returns the id a later reply can
//! thread under.
//!
//! ## Credentials stay out of every log line
//!
//! A Telegram bot token and a Discord interaction token travel *in the URL
//! path*, and Slack and Discord bot tokens in a header. Every failure here is
//! described with [`crate::outbound::redacted`], which keeps the scheme and the
//! host and drops the path, and a platform's error is reported by its own
//! error code, never by echoing the request.
//!
//! ## The hosts are fixed, and still guarded
//!
//! The API hosts are the platforms' own, not something a user typed, but the
//! calls still go through the guarded client the worker uses, so a resolver
//! that returned a private address for `slack.com` would be refused like any
//! other. A webhook URL an operator typed (a Discord alert webhook) goes
//! through [`crate::outbound::post_json`], which guards every hop.

use std::time::Duration;

use micaforge_core::config::{ChatApps, DiscordApp, SlackApp};
use micaforge_core::{Error, Result};
use micaforge_store::models::chat::ChatLink;
use serde_json::{json, Value as Json};
use url::Url;

use crate::outbound::{self, redacted};

/// Longest a platform call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest platform response read, in bytes. Their answers are a few hundred.
const MAX_REPLY_BYTES: usize = 256 * 1024;

/// Slack's practical ceiling for one readable message (the API allows more).
pub const SLACK_MAX: usize = 3_900;
/// Discord's hard ceiling on `content`.
pub const DISCORD_MAX: usize = 2_000;
/// Telegram's hard ceiling on a message's text.
pub const TELEGRAM_MAX: usize = 4_096;

/// Discord's API root.
const DISCORD_API: &str = "https://discord.com/api/v10";
/// Slack's API root.
const SLACK_API: &str = "https://slack.com/api";
/// Telegram's Bot API root.
const TELEGRAM_API: &str = "https://api.telegram.org";

/// Cut `text` to `max` characters, marking the cut.
///
/// Characters, not bytes: both Discord and Telegram count characters, and a
/// cut through a multi-byte character would be refused as invalid text.
pub fn fit(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Escape text for Slack's `text` field.
///
/// Slack reads `<…>` as a link or a mention (`<!channel>`), so the three
/// characters it documents are escaped. A question asked in a channel and
/// echoed back can then never ping everybody in it.
pub fn slack_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// POST JSON to a platform API and read its JSON answer.
async fn call(
    http: &reqwest::Client,
    url: &str,
    auth: Option<String>,
    body: &Json,
) -> Result<Json> {
    let target =
        Url::parse(url).map_err(|_| Error::internal("a chat platform URL did not parse"))?;
    let mut request = http.post(target.clone()).timeout(CALL_TIMEOUT).json(body);
    if let Some(value) = auth {
        request = request.header("authorization", value);
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Upstream(outbound::Failure::from_reqwest(&target, &e).to_string()))?;
    let status = response.status();
    let (bytes, _) = outbound::read_capped(response, MAX_REPLY_BYTES).await?;
    let parsed: Json = if bytes.is_empty() {
        Json::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Json::Null)
    };
    if !status.is_success() {
        let reason = parsed
            .get("description")
            .or_else(|| parsed.get("message"))
            .or_else(|| parsed.get("error"))
            .and_then(Json::as_str)
            .unwrap_or("no reason given");
        return Err(Error::Upstream(format!(
            "{} answered HTTP {}: {reason}",
            redacted(&target),
            status.as_u16()
        )));
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// Slack
// ---------------------------------------------------------------------------

/// Post to a Slack channel with the bot token; returns the message `ts`.
///
/// `thread_ts` replies in that thread. Slack answers `200` with `ok: false`
/// for its own refusals (`not_in_channel`, `channel_not_found`), so `ok` is
/// read rather than the status alone.
pub async fn slack_post(
    http: &reqwest::Client,
    app: &SlackApp,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<String> {
    let mut body = json!({
        "channel": channel,
        "text": fit(&slack_escape(text), SLACK_MAX),
        "unfurl_links": false,
        "unfurl_media": false,
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = json!(ts);
    }
    let reply = call(
        http,
        &format!("{SLACK_API}/chat.postMessage"),
        Some(format!("Bearer {}", app.bot_token)),
        &body,
    )
    .await?;
    if reply.get("ok").and_then(Json::as_bool) != Some(true) {
        let code = reply
            .get("error")
            .and_then(Json::as_str)
            .unwrap_or("unknown_error");
        return Err(Error::Upstream(format!(
            "slack refused the message: {code}"
        )));
    }
    Ok(reply
        .get("ts")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string())
}

/// Answer a slash command through its `response_url`.
///
/// The URL is Slack's own and arrives in the signed request, so it is checked
/// to be a Slack host before anything is posted to it.
pub async fn slack_respond(http: &reqwest::Client, response_url: &str, text: &str) -> Result<()> {
    let target =
        Url::parse(response_url).map_err(|_| Error::validation("response_url", "is not a URL"))?;
    let slack_host = target.scheme() == "https"
        && target
            .host_str()
            .is_some_and(|h| h == "hooks.slack.com" || h.ends_with(".slack.com"));
    if !slack_host {
        return Err(Error::validation("response_url", "is not a Slack address"));
    }
    call(
        http,
        response_url,
        None,
        &json!({ "response_type": "in_channel", "text": fit(&slack_escape(text), SLACK_MAX) }),
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Discord
// ---------------------------------------------------------------------------

/// Post to a Discord channel with the bot token; returns the message id.
///
/// `reply_to` makes it a reply to that message, which is how an
/// investigation's updates stay together. Mentions are never parsed, so text
/// that contains `@everyone` stays text.
pub async fn discord_post(
    http: &reqwest::Client,
    app: &DiscordApp,
    channel: &str,
    text: &str,
    reply_to: Option<&str>,
) -> Result<String> {
    let mut body = json!({
        "content": fit(text, DISCORD_MAX),
        "allowed_mentions": { "parse": [] },
    });
    if let Some(id) = reply_to {
        body["message_reference"] = json!({ "message_id": id, "fail_if_not_exists": false });
    }
    let reply = call(
        http,
        &format!("{DISCORD_API}/channels/{channel}/messages"),
        Some(format!("Bot {}", app.bot_token)),
        &body,
    )
    .await?;
    Ok(reply
        .get("id")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string())
}

/// Replace the "thinking…" placeholder of a deferred interaction with the answer.
///
/// The interaction token in the path is the credential; it lives fifteen
/// minutes, which bounds how long an answer may take.
pub async fn discord_edit_original(
    http: &reqwest::Client,
    app: &DiscordApp,
    interaction_token: &str,
    text: &str,
) -> Result<()> {
    let url = format!(
        "{DISCORD_API}/webhooks/{}/{interaction_token}/messages/@original",
        app.application_id
    );
    let target = Url::parse(&url).map_err(|_| Error::internal("discord URL did not parse"))?;
    let response = http
        .patch(target.clone())
        .timeout(CALL_TIMEOUT)
        .json(&json!({ "content": fit(text, DISCORD_MAX), "allowed_mentions": { "parse": [] } }))
        .send()
        .await
        .map_err(|e| Error::Upstream(outbound::Failure::from_reqwest(&target, &e).to_string()))?;
    if !response.status().is_success() {
        return Err(Error::Upstream(format!(
            "{} answered HTTP {}",
            redacted(&target),
            response.status().as_u16()
        )));
    }
    Ok(())
}

/// Register the `/micaforge` application command, globally.
///
/// `PUT` replaces the whole command list, so running it twice leaves one
/// command, not two.
pub async fn discord_register_commands(http: &reqwest::Client, app: &DiscordApp) -> Result<usize> {
    let commands = json!([discord_command()]);
    let url = format!("{DISCORD_API}/applications/{}/commands", app.application_id);
    let target = Url::parse(&url).map_err(|_| Error::internal("discord URL did not parse"))?;
    let response = http
        .put(target.clone())
        .timeout(CALL_TIMEOUT)
        .header("authorization", format!("Bot {}", app.bot_token))
        .json(&commands)
        .send()
        .await
        .map_err(|e| Error::Upstream(outbound::Failure::from_reqwest(&target, &e).to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Upstream(format!(
            "{} answered HTTP {} to the command registration",
            redacted(&target),
            status.as_u16()
        )));
    }
    Ok(1)
}

/// The `/micaforge` command with its six subcommands.
///
/// Option type 1 is a subcommand and type 3 a string, in Discord's numbering.
pub fn discord_command() -> Json {
    let string = |name: &str, description: &str| json!({ "type": 3, "name": name, "description": description, "required": true });
    json!({
        "name": "micaforge",
        "type": 1,
        "description": "Ask Micaforge about the site linked to this channel",
        "options": [
            { "type": 1, "name": "ask", "description": "Ask a question about this channel's site",
              "options": [string("question", "What you want to know")] },
            { "type": 1, "name": "stats", "description": "Today's headline numbers" },
            { "type": 1, "name": "link", "description": "Link this channel to a site with a code from the dashboard",
              "options": [string("code", "The code from Site setup, Chat apps")] },
            { "type": 1, "name": "unlink", "description": "Stop this channel answering for its site" },
            { "type": 1, "name": "feedback", "description": "Send feedback to the people who run this Micaforge",
              "options": [string("text", "Your feedback")] },
            { "type": 1, "name": "help", "description": "What this app can do" },
        ],
    })
}

/// Post to a Discord webhook URL an operator configured.
pub async fn discord_webhook(http: &reqwest::Client, url: &str, text: &str) -> Result<()> {
    outbound::post_json(
        http,
        url,
        &json!({ "content": fit(text, DISCORD_MAX), "allowed_mentions": { "parse": [] } }),
        CALL_TIMEOUT,
    )
    .await?;
    Ok(())
}

/// True for a Discord webhook URL, which takes `content` rather than `text`.
pub fn is_discord_webhook(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| {
            host == "discord.com" || host == "discordapp.com" || host.ends_with(".discord.com")
        })
}

// ---------------------------------------------------------------------------
// Telegram
// ---------------------------------------------------------------------------

/// Send a message with a bot; returns the message id.
///
/// Plain text, no `parse_mode`: Telegram's Markdown and HTML modes refuse a
/// whole message over one unbalanced character, and an answer full of paths
/// and percentages is exactly the text that has them.
pub async fn telegram_send(
    http: &reqwest::Client,
    bot_token: &str,
    chat_id: &str,
    text: &str,
    reply_to: Option<&str>,
) -> Result<String> {
    let mut body = json!({
        "chat_id": chat_id,
        "text": fit(text, TELEGRAM_MAX),
        "link_preview_options": { "is_disabled": true },
    });
    if let Some(id) = reply_to.and_then(|id| id.parse::<i64>().ok()) {
        body["reply_parameters"] = json!({ "message_id": id, "allow_sending_without_reply": true });
    }
    let reply = call(
        http,
        &format!("{TELEGRAM_API}/bot{bot_token}/sendMessage"),
        None,
        &body,
    )
    .await?;
    if reply.get("ok").and_then(Json::as_bool) != Some(true) {
        return Err(Error::Upstream("telegram refused the message".to_string()));
    }
    Ok(reply
        .pointer("/result/message_id")
        .map(|v| v.to_string())
        .unwrap_or_default())
}

/// Point the bot's webhook at this server, with the secret Telegram will echo.
pub async fn telegram_set_webhook(
    http: &reqwest::Client,
    bot_token: &str,
    url: &str,
    secret: &str,
) -> Result<()> {
    call(
        http,
        &format!("{TELEGRAM_API}/bot{bot_token}/setWebhook"),
        None,
        &json!({
            "url": url,
            "secret_token": secret,
            "allowed_updates": ["message"],
            "drop_pending_updates": true,
        }),
    )
    .await?;
    call(
        http,
        &format!("{TELEGRAM_API}/bot{bot_token}/setMyCommands"),
        None,
        &json!({ "commands": [
            { "command": "ask", "description": "Ask a question about the linked site" },
            { "command": "stats", "description": "Today's headline numbers" },
            { "command": "link", "description": "Link this chat to a site with a dashboard code" },
            { "command": "unlink", "description": "Stop this chat answering for its site" },
            { "command": "feedback", "description": "Send feedback to the people who run this Micaforge" },
            { "command": "help", "description": "What this bot can do" },
        ]}),
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// One linked channel, whatever its platform
// ---------------------------------------------------------------------------

/// Post to a linked channel; returns the id a later update can reply under.
///
/// `thread` is that id, from the first post. A platform that is not
/// configured on this install is an error rather than a silent skip, so the
/// caller can say why a linked channel heard nothing.
pub async fn post_to_link(
    http: &reqwest::Client,
    apps: &ChatApps,
    link: &ChatLink,
    text: &str,
    thread: Option<&str>,
) -> Result<String> {
    let not_configured = || {
        Error::Conflict(format!(
            "the {} app is not configured on this server",
            link.platform
        ))
    };
    match link.platform.as_str() {
        "slack" => {
            let app = apps.slack.as_ref().ok_or_else(not_configured)?;
            slack_post(http, app, &link.channel_id, text, thread).await
        }
        "discord" => {
            let app = apps.discord.as_ref().ok_or_else(not_configured)?;
            discord_post(http, app, &link.channel_id, text, thread).await
        }
        "telegram" => {
            let app = apps.telegram.as_ref().ok_or_else(not_configured)?;
            telegram_send(http, &app.bot_token, &link.channel_id, text, thread).await
        }
        other => Err(Error::internal(format!("unknown chat platform `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_by_characters_with_a_mark() {
        assert_eq!(fit("short", 10), "short");
        let cut = fit(&"é".repeat(20), 5);
        assert_eq!(cut.chars().count(), 5);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn slack_text_cannot_ping_a_channel() {
        assert_eq!(
            slack_escape("<!channel> & <@U1>"),
            "&lt;!channel&gt; &amp; &lt;@U1&gt;"
        );
    }

    #[test]
    fn discord_webhooks_are_told_apart_by_host() {
        assert!(is_discord_webhook("https://discord.com/api/webhooks/1/abc"));
        assert!(is_discord_webhook(
            "https://canary.discord.com/api/webhooks/1/abc"
        ));
        assert!(is_discord_webhook(
            "https://discordapp.com/api/webhooks/1/abc"
        ));
        assert!(!is_discord_webhook(
            "https://hooks.slack.com/services/T/B/x"
        ));
        assert!(!is_discord_webhook(
            "https://discord.com.evil.example/api/webhooks/1/abc"
        ));
    }

    #[test]
    fn the_command_has_every_subcommand_the_other_platforms_have() {
        let command = discord_command();
        let names: Vec<&str> = command["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["ask", "stats", "link", "unlink", "feedback", "help"]
        );
    }
}
