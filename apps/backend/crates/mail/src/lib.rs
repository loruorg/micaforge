//! `micaforge-mail`: everything Micaforge sends by email.
//!
//! Four messages, two transports, no queue. The queue lives in `micaforge-jobs`;
//! this crate's whole job is to turn a small struct into a well-formed message
//! and hand it to whichever way out the operator configured.
//!
//! # Two transports, and a third that sends nothing
//!
//! [`micaforge_core::config::MailTransport`] decides, from the configuration:
//!
//! * **Cloudflare Email Sending**, when `MICAFORGE_CLOUDFLARE_EMAIL_TOKEN` and
//!   `MICAFORGE_CLOUDFLARE_ACCOUNT_ID` are both set. One HTTPS `POST` per
//!   message to Cloudflare's API; no relay to run.
//! * **SMTP**, through lettre, when `MICAFORGE_SMTP_URL` is set and Cloudflare
//!   is not.
//! * **Nothing**, otherwise.
//!
//! # Mail is never a startup dependency
//!
//! A [`Mailer`] with no transport is not a broken mailer: it renders the message
//! exactly as it would have sent it, logs one line naming the recipient and the
//! subject, and reports success. That is the correct behaviour for the
//! deployment `PRODUCT.md` treats as first class, a single `docker compose up`
//! on a VPS, where mail is the last thing an operator sets up. A sign-in code is
//! then printed on the host by `micaforge-server sign-in-code <email>`, and an
//! invitation link is returned to the admin who created it so they can hand it
//! over ([`Mailer::is_sending`] tells the dashboard to say so).
//!
//! # The body is never logged
//!
//! Earlier builds wrote the plain-text part to the log. Every message this crate
//! sends to a person who asked for it carries a live credential: a sign-in code,
//! an invitation token. Anyone who could read the container log could take over
//! the account the message was for. So the log line says that the message was
//! redacted and nothing else about it, and no subject this crate writes carries
//! a credential.
//!
//! ```no_run
//! # async fn demo() -> micaforge_core::Result<()> {
//! use micaforge_core::config::MailTransport;
//! use micaforge_mail::Mailer;
//!
//! // `Log` here is the default install, not an error path.
//! let mailer = Mailer::new(MailTransport::Log, "Micaforge <no-reply@example.com>", "https://app.example.com")?;
//! mailer.send_sign_in_code("owner@example.com", "123456", 10).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # What the messages look like
//!
//! [`templates`] holds the bodies. They are light with a dark enhancement,
//! table-based, 560px, inline styled, and they carry no images and no tracking
//! pixel, which is a product constraint rather than a stylistic one: a privacy
//! tool that pixel-tracks its own mail has argued against itself.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::time::Duration;

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use micaforge_core::config::MailTransport;
use micaforge_core::{Error, Result};
use serde::{Deserialize, Serialize};

pub mod templates;

pub use templates::Rendered;

/// Cloudflare's API root. The send endpoint is
/// `{CLOUDFLARE_API}/accounts/{account_id}/email/sending/send`.
const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";

/// How long one Cloudflare send may take, connection included.
///
/// A sign-in code is sent from a background task, so this bounds how long that
/// task lives, not how long a person waits for a response. Twenty seconds is
/// far past a healthy API call and short enough that a hung connection does not
/// hold a task for the life of the process.
const CLOUDFLARE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long establishing the connection to Cloudflare may take.
const CLOUDFLARE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The outgoing mail path.
///
/// Cheap to clone through an `Arc` in application state and safe to share across
/// tasks: the SMTP transport opens a connection per message and the HTTP client
/// is a handle over its own pool, so nothing here is a contended resource.
pub struct Mailer {
    /// How a message leaves the process. See the crate docs for why sending
    /// nothing is a supported configuration and not a degraded one.
    transport: Transport,
    /// The `From` mailbox, validated once at construction.
    from: String,
    /// The origin every link in every message is built from. This is the
    /// *dashboard* origin (`MICAFORGE_DASHBOARD_URL`), not the API origin: every
    /// link in an email lands a person in the app, never on an endpoint.
    base_url: String,
}

/// The built form of [`MailTransport`].
enum Transport {
    Smtp(AsyncSmtpTransport<Tokio1Executor>),
    Cloudflare(Cloudflare),
    Log,
}

/// Cloudflare Email Sending, over HTTPS.
struct Cloudflare {
    client: reqwest::Client,
    /// The send endpoint, with the account id already in it.
    endpoint: String,
    /// The API token. Sent as a bearer token and never printed: see the `Debug`
    /// impl on [`Mailer`].
    token: String,
}

/// The body Cloudflare's send endpoint takes.
#[derive(Serialize)]
struct CloudflareMessage<'a> {
    from: &'a str,
    to: &'a str,
    subject: &'a str,
    text: &'a str,
    html: &'a str,
}

/// The envelope every Cloudflare API answer is wrapped in. Only the two fields
/// the outcome depends on are read.
#[derive(Deserialize)]
struct CloudflareEnvelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    errors: Vec<CloudflareError>,
}

/// One entry of [`CloudflareEnvelope::errors`].
#[derive(Deserialize)]
struct CloudflareError {
    #[serde(default)]
    message: String,
}

impl Mailer {
    /// Build a mailer.
    ///
    /// `transport` comes from [`micaforge_core::Config::mail_transport`], which
    /// owns the precedence. An SMTP URL is a standard lettre connection URL
    /// (`smtps://user:pass@host:465`, `smtp://host:25`,
    /// `smtp://host:587?tls=required`).
    ///
    /// Everything is validated here, at boot, rather than at the first send. An
    /// operator reading a container that refuses to start can fix a typo; an
    /// operator reading a support ticket about a sign-in code that silently
    /// never arrived cannot.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`], naming the environment variable at fault, when the
    /// SMTP URL cannot be parsed, the Cloudflare account id or token cannot be
    /// used, or the `From` address is not a mailbox.
    pub fn new(transport: MailTransport<'_>, from: &str, base_url: &str) -> Result<Mailer> {
        let from = from.trim();
        from.parse::<Mailbox>().map_err(|e| {
            Error::validation(
                "MICAFORGE_MAIL_FROM",
                format!("not a valid mail address: {e}"),
            )
        })?;

        let transport = match transport {
            MailTransport::Cloudflare { account_id, token } => {
                tracing::info!(target: "micaforge::mail", "mail is sent through Cloudflare Email Sending");
                Transport::Cloudflare(Cloudflare::new(account_id, token)?)
            }
            MailTransport::Smtp(url) => Transport::Smtp(
                AsyncSmtpTransport::<Tokio1Executor>::from_url(url.trim())
                    .map_err(|e| {
                        Error::validation(
                            "MICAFORGE_SMTP_URL",
                            format!("not a usable SMTP url: {e}"),
                        )
                    })?
                    .build(),
            ),
            MailTransport::Log => {
                tracing::info!(
                    target: "micaforge::mail",
                    "no mail transport configured; mail will be rendered but not sent, and no \
                     code or link from it is ever logged"
                );
                Transport::Log
            }
        };

        Ok(Mailer {
            transport,
            from: from.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// True when messages actually leave the process.
    ///
    /// The sign-in screen uses this to decide whether to tell the reader to
    /// check their inbox or to ask the operator for a code, and the invitation
    /// flow to decide whether to show the link for the admin to copy.
    pub fn is_sending(&self) -> bool {
        !matches!(self.transport, Transport::Log)
    }

    /// The dashboard origin every link is built from.
    ///
    /// Exposed so a caller that needs to render the same link on screen uses this
    /// crate's route knowledge rather than its own copy of it. Pair it with
    /// [`templates::invitation_link`].
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Send one message with both alternatives.
    ///
    /// The plain-text part is not optional and is not generated from the HTML.
    /// It is the part a screen reader and a terminal client see. With no
    /// transport configured, neither part is logged: see the crate docs.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a recipient that is not a mailbox, and
    /// [`Error::Upstream`] when the relay or the API refuses or the connection
    /// fails. A caller running inside a job should treat the second as
    /// retryable and the first as permanent.
    pub async fn send(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<()> {
        let recipient: Mailbox = to
            .trim()
            .parse()
            .map_err(|e| Error::validation("to", format!("not a valid mail address: {e}")))?;

        match &self.transport {
            Transport::Log => {
                // Deliberately `warn`: an operator scanning for why a code or a
                // link never arrived should find this line. The body is not
                // written: it carries live credentials, and a log is not a place
                // a credential may sit.
                tracing::warn!(
                    target: "micaforge::mail",
                    recipient = %recipient,
                    subject = %subject,
                    "no mail transport is configured; the message was not delivered and its \
                     contents are redacted from the log. Set MICAFORGE_CLOUDFLARE_EMAIL_TOKEN and \
                     MICAFORGE_CLOUDFLARE_ACCOUNT_ID, or MICAFORGE_SMTP_URL, to send it"
                );
                Ok(())
            }
            Transport::Cloudflare(cloudflare) => {
                cloudflare
                    .send(&CloudflareMessage {
                        from: &self.from,
                        to: &recipient.to_string(),
                        subject: &ascii_subject(subject),
                        text,
                        html,
                    })
                    .await
            }
            Transport::Smtp(transport) => {
                // Re-parsed rather than stored: `new` already proved this
                // succeeds, so the failure arm is unreachable in practice and is
                // reported as a bug if it ever is not.
                let sender: Mailbox = self.from.parse().map_err(|e| {
                    Error::Internal(format!("mail from address became invalid: {e}"))
                })?;

                let message = Message::builder()
                    .from(sender)
                    .to(recipient)
                    .subject(subject)
                    .multipart(MultiPart::alternative_plain_html(
                        text.to_string(),
                        html.to_string(),
                    ))
                    .map_err(|e| Error::Internal(format!("could not build message: {e}")))?;

                transport
                    .send(message)
                    .await
                    .map_err(|e| Error::Upstream(format!("smtp: {e}")))?;
                Ok(())
            }
        }
    }

    /// Send a sign-in code that works for `ttl_minutes`.
    pub async fn send_sign_in_code(&self, to: &str, code: &str, ttl_minutes: u32) -> Result<()> {
        self.dispatch(to, templates::sign_in_code(code, ttl_minutes))
            .await
    }

    /// Invite an address into an organisation.
    pub async fn send_invitation(&self, to: &str, org: &str, token: &str) -> Result<()> {
        self.dispatch(to, templates::invitation(&self.base_url, org, token))
            .await
    }

    /// Send a scheduled report for one site over one period.
    ///
    /// `period` is the human phrase the report covers ("last 7 days",
    /// "12 to 18 August"), already resolved in the site's own timezone. This
    /// crate does no date arithmetic; the job that computed the numbers owns the
    /// window they describe.
    pub async fn send_report(
        &self,
        to: &str,
        site: &str,
        period: &str,
        report: &ReportPayload,
    ) -> Result<()> {
        self.dispatch(to, templates::report(&self.base_url, site, period, report))
            .await
    }

    /// Send one fired alert.
    pub async fn send_alert(&self, to: &str, site: &str, alert: &AlertPayload) -> Result<()> {
        self.dispatch(to, templates::alert(&self.base_url, site, alert))
            .await
    }

    /// Send an already-rendered message.
    async fn dispatch(&self, to: &str, r: Rendered) -> Result<()> {
        self.send(to, &r.subject, &r.html, &r.text).await
    }
}

impl Cloudflare {
    /// Build the client and the endpoint, validating both inputs.
    ///
    /// The account id becomes a path segment, so it is held to the letters and
    /// digits a Cloudflare id is made of: anything else is a paste error, and a
    /// `/` or a `?` in it would send the token to a different endpoint.
    fn new(account_id: &str, token: &str) -> Result<Cloudflare> {
        let account_id = account_id.trim();
        if account_id.is_empty() || !account_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Error::validation(
                "MICAFORGE_CLOUDFLARE_ACCOUNT_ID",
                "must be the account id from the Cloudflare dashboard: letters and digits only",
            ));
        }
        let token = token.trim();
        if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::validation(
                "MICAFORGE_CLOUDFLARE_EMAIL_TOKEN",
                "is not a usable API token: it must be printable characters with no spaces",
            ));
        }

        let client = reqwest::Client::builder()
            .timeout(CLOUDFLARE_TIMEOUT)
            .connect_timeout(CLOUDFLARE_CONNECT_TIMEOUT)
            .user_agent(concat!("micaforge/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| Error::Internal(format!("could not build the mail http client: {e}")))?;

        Ok(Cloudflare {
            client,
            endpoint: format!("{CLOUDFLARE_API}/accounts/{account_id}/email/sending/send"),
            token: token.to_string(),
        })
    }

    /// Post one message, and read the envelope for the verdict.
    async fn send(&self, message: &CloudflareMessage<'_>) -> Result<()> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.token)
            .json(message)
            .send()
            .await
            .map_err(|e| Error::Upstream(format!("cloudflare email: {e}")))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|e| Error::Upstream(format!("cloudflare email: {e}")))?;
        cloudflare_outcome(status, &body)
    }
}

/// Whether Cloudflare accepted a message, from the status and the body.
///
/// Both have to agree: a `2xx` whose envelope says `success: false`, or an
/// envelope that cannot be read at all, is a refusal. The reason is the first
/// message in `errors`, which is the sentence Cloudflare writes for a person
/// ("Authentication error", a sender domain that is not verified), and the
/// status when there is none.
fn cloudflare_outcome(status: u16, body: &[u8]) -> Result<()> {
    let envelope: Option<CloudflareEnvelope> = serde_json::from_slice(body).ok();
    let accepted = (200..300).contains(&status) && envelope.as_ref().is_some_and(|e| e.success);
    if accepted {
        return Ok(());
    }
    let reason = envelope
        .and_then(|e| e.errors.into_iter().next())
        .map(|e| e.message.trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("HTTP {status}"));
    Err(Error::Upstream(format!("cloudflare email: {reason}")))
}

/// Fold typographic punctuation in a subject to ASCII, for Cloudflare.
///
/// A subject with any byte above `0x7F` has to be sent as an RFC 2047
/// encoded-word. Cloudflare encodes only the offending run rather than the
/// whole header, and enough receiving clients fail to decode that fragment that
/// the raw token (`=?utf-8?b?wrc=?=`) lands in the inbox where the `·` of a
/// report subject should be. A subject that is already ASCII never needs
/// encoding, so the failure cannot happen.
///
/// Only punctuation is folded. Letters are left alone: a site or organisation
/// name in another script has no ASCII spelling, and mangling it would be
/// worse than letting it be encoded. SMTP is not affected; lettre encodes the
/// whole header.
fn ascii_subject(subject: &str) -> String {
    let mut out = String::with_capacity(subject.len());
    for c in subject.chars() {
        match c {
            '\u{2014}' | '\u{2013}' | '\u{2212}' | '\u{00B7}' | '\u{2022}' => out.push('-'),
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => out.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => out.push('"'),
            '\u{2026}' => out.push_str("..."),
            '\u{00A0}' | '\u{2007}' | '\u{2009}' | '\u{202F}' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

impl std::fmt::Debug for Mailer {
    /// Hand-written so an SMTP URL with a password in it, or a Cloudflare
    /// token, can never reach a log line through a derived `Debug` on
    /// application state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let transport = match self.transport {
            Transport::Smtp(_) => "smtp",
            Transport::Cloudflare(_) => "cloudflare",
            Transport::Log => "log",
        };
        f.debug_struct("Mailer")
            .field("sending", &self.is_sending())
            .field("transport", &transport)
            .field("from", &self.from)
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// The numbers a scheduled report renders.
///
/// **Units, because two crates have to agree on them.** Counts are whole events.
/// `bounce_rate` and `visitors_delta` are *fractions*, not percentages: `0.41`
/// renders as `41%` and `-0.031` renders as `-3.1%`. `gap_ratio` is agent fetches
/// per answer-engine arrival, the Citation Gap of `docs/CONTRACT.md` §0, so `284.0`
/// renders as `284 : 1`.
///
/// The three ranked lists are `(label, count)` in the order they should appear,
/// already truncated to the length the report wants to show. The count is
/// visitors for pages and sources, and fetches for agents. An empty list is a
/// legitimate value and renders as an empty state, never as a zero row.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportPayload {
    /// Unique visitors in the period.
    pub visitors: u64,
    /// Change in visitors against the previous period, as a signed fraction.
    pub visitors_delta: f64,
    /// Pageviews in the period.
    pub pageviews: u64,
    /// Sessions in the period.
    pub sessions: u64,
    /// Bounce rate as a fraction of sessions.
    pub bounce_rate: f64,
    /// Fetches attributed to named agents in the period.
    pub agent_fetches: u64,
    /// Arrivals from an answer engine in the period.
    pub answer_arrivals: u64,
    /// Agent fetches per answer-engine arrival.
    pub gap_ratio: f64,
    /// Top pages by visitors, ranked.
    pub top_pages: Vec<(String, u64)>,
    /// Top sources by visitors, ranked.
    pub top_sources: Vec<(String, u64)>,
    /// Top agents by fetches, ranked.
    pub top_agents: Vec<(String, u64)>,
}

/// One fired alert, as the alerting job computed it.
///
/// `observed` and `expected` are in the units of `metric` and are rendered as
/// given; this crate scales nothing. `link` must be an absolute `http`/`https`
/// address into the dashboard, and anything else is dropped in favour of the
/// dashboard root rather than rendered, because an email from an analytics tool
/// is exactly the envelope a hostile link would want.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlertPayload {
    /// The rule that fired: `spike`, `drop`, `goal`, `error`, `agent_surge` or
    /// `citation_gap`, matching `alerts.kind` in Postgres.
    pub kind: String,
    /// One sentence naming what happened. Becomes the subject and the heading.
    pub headline: String,
    /// A short paragraph with the specifics.
    pub detail: String,
    /// The metric that tripped the rule, in the words a person would use.
    pub metric: String,
    /// What was measured.
    pub observed: f64,
    /// What the rule expected instead.
    pub expected: f64,
    /// Absolute dashboard link to the view that shows this.
    pub link: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Held by every test that logs through the mail callsites.
    ///
    /// tracing keeps process-wide state (each callsite's cached interest, and
    /// the global maximum level) that a test logging with no subscriber can
    /// recompute while the capturing test below is running, which makes the
    /// capture see nothing. Serialising the two is what makes it deterministic.
    static LOGGING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const FROM: &str = "Micaforge <no-reply@example.com>";
    const BASE: &str = "https://app.example.com";

    #[test]
    fn no_transport_is_a_working_mailer() {
        let m = Mailer::new(MailTransport::Log, FROM, "https://app.example.com/")
            .expect("a mailer with no transport is the default install");
        assert!(!m.is_sending());
        assert_eq!(m.base_url(), "https://app.example.com");
    }

    #[test]
    fn a_bad_from_address_fails_at_boot() {
        let e = Mailer::new(MailTransport::Log, "not an address", BASE).unwrap_err();
        assert_eq!(e.status(), 422);
        assert_eq!(e.field(), Some("MICAFORGE_MAIL_FROM"));
    }

    #[test]
    fn a_bad_smtp_url_fails_at_boot() {
        let e = Mailer::new(MailTransport::Smtp("not-a-url"), FROM, BASE).unwrap_err();
        assert_eq!(e.field(), Some("MICAFORGE_SMTP_URL"));
    }

    #[test]
    fn a_bad_cloudflare_pair_fails_at_boot_and_names_the_half_at_fault() {
        let e = Mailer::new(
            MailTransport::Cloudflare {
                account_id: "abc/../../other",
                token: "cf-token",
            },
            FROM,
            BASE,
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("MICAFORGE_CLOUDFLARE_ACCOUNT_ID"));

        let e = Mailer::new(
            MailTransport::Cloudflare {
                account_id: "0123abcd",
                token: "two words",
            },
            FROM,
            BASE,
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("MICAFORGE_CLOUDFLARE_EMAIL_TOKEN"));
    }

    #[test]
    fn either_transport_counts_as_sending() {
        let cloudflare = Mailer::new(
            MailTransport::Cloudflare {
                account_id: "0123abcd",
                token: "cf-token",
            },
            FROM,
            BASE,
        )
        .unwrap();
        assert!(cloudflare.is_sending());
        let smtp = Mailer::new(MailTransport::Smtp("smtp://localhost:2525"), FROM, BASE).unwrap();
        assert!(smtp.is_sending());
    }

    #[test]
    fn debug_never_prints_a_credential() {
        let smtp = Mailer::new(
            MailTransport::Smtp("smtp://user:hunter2@localhost:2525"),
            FROM,
            BASE,
        )
        .expect("a plain smtp url parses");
        let printed = format!("{smtp:?}");
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("sending: true"));

        let cloudflare = Mailer::new(
            MailTransport::Cloudflare {
                account_id: "0123abcd",
                token: "cf-secret-token",
            },
            FROM,
            BASE,
        )
        .unwrap();
        let printed = format!("{cloudflare:?}");
        assert!(!printed.contains("cf-secret-token"));
        assert!(printed.contains("cloudflare"));
    }

    #[test]
    fn cloudflare_must_say_success_in_the_status_and_the_envelope() {
        assert!(cloudflare_outcome(200, br#"{"success":true,"errors":[],"result":{}}"#).is_ok());

        let refused = cloudflare_outcome(
            403,
            br#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}]}"#,
        )
        .unwrap_err();
        assert_eq!(refused.status(), 502);
        assert!(
            refused.to_string().contains("Authentication error"),
            "{refused}"
        );

        // A 200 that says it failed is a failure.
        let contradicted = cloudflare_outcome(
            200,
            br#"{"success":false,"errors":[{"message":"sender domain not verified"}]}"#,
        )
        .unwrap_err();
        assert!(contradicted
            .to_string()
            .contains("sender domain not verified"));

        // An answer that is not an envelope at all is reported by its status.
        let garbled = cloudflare_outcome(502, b"<html>bad gateway</html>").unwrap_err();
        assert!(garbled.to_string().contains("HTTP 502"), "{garbled}");
        assert!(cloudflare_outcome(200, b"").is_err());
    }

    #[test]
    fn only_punctuation_is_folded_in_a_cloudflare_subject() {
        assert_eq!(
            ascii_subject("reeds.dev · 12 to 18 August"),
            "reeds.dev - 12 to 18 August"
        );
        assert_eq!(
            ascii_subject("\u{201C}Docs\u{201D} \u{2014} it\u{2019}s\u{2026}"),
            "\"Docs\" - it's..."
        );
        // A name in another script is left for the encoder, not mangled.
        assert_eq!(ascii_subject("Привет · Mira"), "Привет - Mira");
        assert!(
            templates::report(BASE, "reeds.dev", "last 7 days", &ReportPayload::default())
                .subject
                .contains('·')
        );
        assert!(ascii_subject(
            &templates::report(BASE, "reeds.dev", "last 7 days", &ReportPayload::default()).subject
        )
        .is_ascii());
    }

    /// Accept one request on a local socket, answer it with `status` and
    /// `body`, and hand back what was sent as text.
    async fn one_request(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut received = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                received.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())?
                        })
                        .unwrap_or(0);
                    if received.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let answer = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (format!("http://{address}/send"), handle)
    }

    /// A mailer whose Cloudflare transport points at `endpoint`.
    fn cloudflare_at(endpoint: String) -> Mailer {
        let mut m = Mailer::new(
            MailTransport::Cloudflare {
                account_id: "0123abcd",
                token: "cf-secret-token",
            },
            FROM,
            BASE,
        )
        .unwrap();
        if let Transport::Cloudflare(cloudflare) = &mut m.transport {
            cloudflare.endpoint = endpoint;
        }
        m
    }

    #[test]
    fn the_cloudflare_endpoint_names_the_account() {
        let m = Mailer::new(
            MailTransport::Cloudflare {
                account_id: " 0123abcd ",
                token: "cf-token",
            },
            FROM,
            BASE,
        )
        .unwrap();
        let Transport::Cloudflare(cloudflare) = &m.transport else {
            panic!("a Cloudflare mailer");
        };
        assert_eq!(
            cloudflare.endpoint,
            "https://api.cloudflare.com/client/v4/accounts/0123abcd/email/sending/send"
        );
    }

    #[tokio::test]
    async fn cloudflare_receives_one_json_message_with_a_bearer_token() {
        let (endpoint, server) =
            one_request("200 OK", r#"{"success":true,"errors":[],"messages":[]}"#).await;
        let m = cloudflare_at(endpoint);
        m.send_sign_in_code("owner@example.com", "482913", 10)
            .await
            .expect("an accepted message is a success");

        let request = server.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /send "), "{head}");
        assert!(head
            .to_ascii_lowercase()
            .contains("authorization: bearer cf-secret-token"));
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["from"], FROM);
        assert_eq!(json["to"], "owner@example.com");
        assert_eq!(json["subject"], "Your Micaforge sign-in code");
        assert!(json["text"].as_str().unwrap().contains("482913"));
        assert!(json["html"].as_str().unwrap().contains("482913"));
    }

    #[tokio::test]
    async fn a_cloudflare_refusal_is_an_upstream_error_with_its_reason() {
        let (endpoint, server) = one_request(
            "403 Forbidden",
            r#"{"success":false,"errors":[{"code":10000,"message":"Authentication error"}]}"#,
        )
        .await;
        let m = cloudflare_at(endpoint);
        let e = m
            .send_sign_in_code("owner@example.com", "482913", 10)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(e.status(), 502);
        assert!(e.to_string().contains("Authentication error"), "{e}");
        assert!(!e.to_string().contains("cf-secret-token"));
    }

    /// Every field of every event, as text, for the one thread that runs it.
    ///
    /// A hand-written subscriber rather than `tracing-subscriber`, so the crate
    /// gains no dependency for one test.
    mod capture {
        use std::fmt::Write as _;
        use std::sync::{Arc, Mutex};

        use tracing::field::{Field, Visit};
        use tracing::span::{Attributes, Id, Record};
        use tracing::{Event, Metadata, Subscriber};

        #[derive(Clone, Default)]
        pub struct Capture(pub Arc<Mutex<Vec<String>>>);

        struct Fields<'a>(&'a mut String);

        impl Visit for Fields<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                let _ = write!(self.0, "{}={:?} ", field.name(), value);
            }
        }

        impl Subscriber for Capture {
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &Attributes<'_>) -> Id {
                Id::from_u64(1)
            }
            fn record(&self, _: &Id, _: &Record<'_>) {}
            fn record_follows_from(&self, _: &Id, _: &Id) {}
            fn event(&self, event: &Event<'_>) {
                let mut line = String::new();
                event.record(&mut Fields(&mut line));
                self.0.lock().unwrap().push(line);
            }
            fn enter(&self, _: &Id) {}
            fn exit(&self, _: &Id) {}
        }
    }

    #[test]
    fn logging_mode_never_writes_a_code_a_token_or_a_body() {
        let _serial = LOGGING.lock().unwrap_or_else(|e| e.into_inner());
        let capture = capture::Capture::default();
        let lines = capture.0.clone();
        tracing::subscriber::with_default(capture, || {
            // Another test in this binary logs through the same callsites with
            // no subscriber. If it got there first, tracing has cached them as
            // disabled, and this subscriber would see nothing. Recompute.
            tracing::callsite::rebuild_interest_cache();
            let m = Mailer::new(MailTransport::Log, FROM, BASE).unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                m.send_sign_in_code("owner@example.com", "739104", 10)
                    .await
                    .unwrap();
                m.send_invitation("new@example.com", "Acme", "invite-token-91c2")
                    .await
                    .unwrap();
            });
        });

        let lines = lines.lock().unwrap();
        assert!(
            !lines.is_empty(),
            "the undelivered message is still reported"
        );
        let all = lines.join("\n");
        for secret in ["739104", "invite-token-91c2", "https://app.example.com/"] {
            assert!(!all.contains(secret), "`{secret}` reached the log: {all}");
        }
        assert!(all.contains("owner@example.com"), "the recipient is named");
        assert!(
            all.contains("redacted"),
            "the line says why the contents are missing"
        );
    }

    #[test]
    fn logging_mode_reports_success_and_rejects_a_bad_recipient() {
        let _serial = LOGGING.lock().unwrap_or_else(|e| e.into_inner());
        let m = Mailer::new(MailTransport::Log, FROM, BASE).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            m.send_sign_in_code("owner@example.com", "123456", 10)
                .await
                .expect("logging-only mail is not an error");
            let e = m
                .send_sign_in_code("nonsense", "123456", 10)
                .await
                .unwrap_err();
            assert_eq!(e.status(), 422);
        });
    }
}
