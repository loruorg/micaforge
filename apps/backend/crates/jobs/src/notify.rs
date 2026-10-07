//! Getting a fired alert or a finished report out of the process.
//!
//! Three destinations, all optional, all best-effort: email through
//! [`micaforge_mail::Mailer`], a JSON webhook, and a Slack incoming webhook.
//!
//! **Best-effort is a decision, not a shrug.** An alert that reached the
//! operator's inbox and failed to reach their Slack has done its job, and
//! failing the whole job over the Slack half would retry it and send the email
//! again. So each destination is attempted, each failure is logged with the
//! destination named, and the job succeeds if anything got through. The one case
//! that *is* a failure is nothing getting through at all, because then nobody
//! knows.
//!
//! **A configured URL is still an input.** `alerts.channels` is a jsonb document
//! an authenticated user wrote, and posting to whatever it says would make the
//! worker a request forwarder for anything reachable from inside the network.
//! Every post goes through [`crate::outbound::post_json`], which guards the URL,
//! every redirect hop and every resolved address, and never reads the reply.

use micaforge_core::Result;
use micaforge_mail::AlertPayload;
use serde_json::{json, Value as Json};

use crate::outbound;
use crate::JobCtx;

/// Longest a notification post may take.
///
/// Short: a webhook that is slow is a webhook that will be slow again in five
/// minutes, and an alert sweep must not be held up by one endpoint.
const POST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Where a notification should go.
///
/// Parsed leniently from the `channels` and `recipients` documents in Postgres,
/// because those are written by the API and by hand, and a report configured
/// with a bare list of addresses is the common case.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Channels {
    /// Addresses to mail.
    pub emails: Vec<String>,
    /// A URL to POST the alert to as JSON.
    pub webhook: Option<String>,
    /// A Slack incoming-webhook URL.
    pub slack: Option<String>,
}

impl Channels {
    /// Read a `channels` or `recipients` document.
    ///
    /// Every shape that turns up is accepted:
    ///
    /// ```jsonc
    /// ["ops@example.com"]                                  // a bare list
    /// { "email": "ops@example.com" }                       // one address
    /// { "email": ["ops@example.com"], "slack": "https://…" }
    /// { "emails": [...], "webhook": "https://…" }
    /// ```
    ///
    /// Anything unrecognised contributes nothing rather than failing. A typo in a
    /// channels document should cost that channel, not the alert.
    pub fn from_json(value: &Json) -> Channels {
        let mut channels = Channels::default();

        match value {
            Json::Array(items) => {
                channels.emails = items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect();
            }
            Json::String(one) => channels.emails.push(one.clone()),
            Json::Object(map) => {
                for key in ["email", "emails", "recipients", "to"] {
                    if let Some(value) = map.get(key) {
                        channels.emails.extend(strings(value));
                    }
                }
                channels.webhook = map.get("webhook").and_then(url_of);
                channels.slack = map.get("slack").and_then(url_of);
            }
            _ => {}
        }

        channels.emails.sort();
        channels.emails.dedup();
        channels
    }

    /// Merge a second document in, so a report's `recipients` and its `channels`
    /// can both be honoured.
    pub fn merge(mut self, other: Channels) -> Channels {
        self.emails.extend(other.emails);
        self.emails.sort();
        self.emails.dedup();
        self.webhook = self.webhook.or(other.webhook);
        self.slack = self.slack.or(other.slack);
        self
    }

    /// True when there is nowhere at all to send.
    pub fn is_empty(&self) -> bool {
        self.emails.is_empty() && self.webhook.is_none() && self.slack.is_none()
    }
}

/// Every string in a value, whether it is one string or a list of them.
fn strings(value: &Json) -> Vec<String> {
    match value {
        Json::String(one) => vec![one.clone()],
        Json::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// A non-empty string value, as a URL.
fn url_of(value: &Json) -> Option<String> {
    let raw = value.as_str()?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

/// Send one fired alert everywhere it is configured to go.
///
/// Returns how many destinations accepted it. Zero means the alert fired and
/// nobody heard, which is the caller's cue to report a failure.
pub async fn deliver_alert(
    ctx: &JobCtx,
    site: &str,
    channels: &Channels,
    payload: &AlertPayload,
) -> usize {
    let mut delivered = 0;

    for address in &channels.emails {
        match ctx.mailer.send_alert(address, site, payload).await {
            Ok(()) => delivered += 1,
            Err(e) => tracing::warn!(recipient = %address, error = %e, "alert email failed"),
        }
    }

    if let Some(url) = &channels.webhook {
        let body = json!({
            "site": site,
            "kind": payload.kind,
            "headline": payload.headline,
            "detail": payload.detail,
            "metric": payload.metric,
            "observed": payload.observed,
            "expected": payload.expected,
            "link": payload.link,
        });
        match post_json(ctx, url, &body).await {
            Ok(()) => delivered += 1,
            Err(e) => tracing::warn!(error = %e, "alert webhook failed"),
        }
    }

    if let Some(url) = &channels.slack {
        let text = format!(
            "*{}*\n{}\n{}",
            payload.headline, payload.detail, payload.link
        );
        match post_slack(ctx, url, &text).await {
            Ok(()) => delivered += 1,
            Err(e) => tracing::warn!(error = %e, "alert slack message failed"),
        }
    }

    delivered
}

/// POST a JSON document to an operator-configured URL.
///
/// The error names the host but not the path, because a webhook URL routinely
/// carries its secret there and this error ends up in a log line.
pub async fn post_json(ctx: &JobCtx, url: &str, body: &Json) -> Result<()> {
    outbound::post_json(&ctx.http, url, body, POST_TIMEOUT).await?;
    Ok(())
}

/// Post a message to a Slack incoming webhook.
///
/// The `text` form rather than Block Kit on purpose. Blocks would render more
/// prettily and would also mean this crate owned a piece of Slack's layout
/// language; a bold headline, a sentence and a link is what an alert is, and it
/// renders correctly in every Slack client and in every other service that
/// accepts a Slack-shaped webhook.
pub async fn post_slack(ctx: &JobCtx, url: &str, text: &str) -> Result<()> {
    post_json(ctx, url, &json!({ "text": text })).await
}

/// An absolute dashboard link, for the button in a notification.
///
/// Built from `MICAFORGE_DASHBOARD_URL` so that a link in an email lands a person
/// in the app rather than on an API endpoint, and so an install behind a
/// different hostname still sends links that work.
pub fn dashboard_link(ctx: &JobCtx, site: u16, path: &str) -> String {
    let base = ctx.cfg.dashboard_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        format!("{base}/app/{site}")
    } else {
        format!("{base}/app/{site}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_a_channels_document_arrives_in_is_read() {
        let bare = Channels::from_json(&json!(["ops@example.com", "cto@example.com"]));
        assert_eq!(bare.emails.len(), 2);

        let one = Channels::from_json(&json!({ "email": "ops@example.com" }));
        assert_eq!(one.emails, vec!["ops@example.com".to_string()]);

        let full = Channels::from_json(&json!({
            "emails": ["ops@example.com"],
            "webhook": "https://example.com/hook",
            "slack": "https://hooks.slack.com/services/x"
        }));
        assert_eq!(full.emails.len(), 1);
        assert!(full.webhook.is_some());
        assert!(full.slack.is_some());

        // A string, not a list, is a Slack URL too.
        assert_eq!(
            Channels::from_json(&json!("ops@example.com")).emails.len(),
            1
        );
    }

    #[test]
    fn a_broken_channels_document_costs_the_channel_and_not_the_alert() {
        let junk = Channels::from_json(&json!({ "email": 42, "webhook": "" }));
        assert!(junk.is_empty());
        assert!(Channels::from_json(&json!(null)).is_empty());
        assert!(Channels::from_json(&json!(7)).is_empty());
    }

    #[test]
    fn duplicate_recipients_are_sent_to_once() {
        let doubled = Channels::from_json(&json!({
            "email": ["ops@example.com"],
            "recipients": ["ops@example.com", "cto@example.com"]
        }));
        assert_eq!(
            doubled.emails,
            vec!["cto@example.com".to_string(), "ops@example.com".to_string()]
        );
    }

    #[test]
    fn merging_keeps_both_lists_and_the_first_webhook() {
        let a = Channels::from_json(&json!({ "email": "a@example.com", "slack": "https://s/1" }));
        let b = Channels::from_json(&json!({ "email": "b@example.com", "slack": "https://s/2" }));
        let merged = a.merge(b);
        assert_eq!(merged.emails.len(), 2);
        assert_eq!(merged.slack.as_deref(), Some("https://s/1"));
    }
}
