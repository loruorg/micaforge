//! `micaforge-mail`: everything Micaforge sends by email.
//!
//! Five messages, one transport, no queue. The queue lives in `micaforge-jobs`;
//! this crate's whole job is to turn a small struct into a well-formed message
//! and hand it to an SMTP server.
//!
//! # Mail is never a startup dependency
//!
//! `MICAFORGE_SMTP_URL` is optional, and a [`Mailer`] built without one is not a
//! broken mailer: it renders the message exactly as it would have sent it, logs
//! one line naming the recipient and the subject, and reports success. That is
//! the correct behaviour for the deployment `PRODUCT.md` treats as first class, a
//! single `docker compose up` on a VPS, where mail is the last thing an operator
//! sets up. The first account on an install is verified without mail, and an
//! invitation link is returned to the admin who created it so they can hand it
//! over ([`Mailer::is_sending`] tells the dashboard to show it).
//!
//! # The body is never logged
//!
//! Earlier builds wrote the plain-text part to the log. Every message this crate
//! sends that carries a link carries a live credential in it: a password-reset
//! token, a verification token, an invitation token. Anyone who could read the
//! container log could take over the account the message was for. So the log
//! line says that the links were redacted and nothing else about the message.
//!
//! ```no_run
//! # async fn demo() -> micaforge_core::Result<()> {
//! use micaforge_mail::Mailer;
//!
//! // `None` here is the default install, not an error path.
//! let mailer = Mailer::new(None, "Micaforge <no-reply@example.com>", "https://app.example.com")?;
//! mailer.send_verification("owner@example.com", "a-token").await?;
//! # Ok(())
//! # }
//! ```
//!
//! # What the messages look like
//!
//! [`templates`] holds the bodies. They are dark, table-based, 600px, inline
//! styled, and they carry no images and no tracking pixel, which is a product
//! constraint rather than a stylistic one: a privacy tool that pixel-tracks its
//! own mail has argued against itself.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use micaforge_core::{Error, Result};
use serde::{Deserialize, Serialize};

pub mod templates;

pub use templates::Rendered;

/// The outgoing mail path.
///
/// Cheap to clone through an `Arc` in application state and safe to share across
/// tasks: the SMTP transport opens a connection per message, so nothing here is
/// a contended resource.
pub struct Mailer {
    /// `None` puts the mailer in logging-only mode. See the crate docs for why
    /// that is a supported configuration and not a degraded one.
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    /// The `From` mailbox, validated once at construction.
    from: String,
    /// The origin every link in every message is built from. This is the
    /// *dashboard* origin (`MICAFORGE_DASHBOARD_URL`), not the API origin: every
    /// link in an email lands a person in the app, never on an endpoint.
    base_url: String,
}

impl Mailer {
    /// Build a mailer.
    ///
    /// `smtp_url` is a standard lettre connection URL
    /// (`smtps://user:pass@host:465`, `smtp://host:25`, `smtp://host:587?tls=required`).
    /// Passing `None` yields the logging-only mailer described in the crate docs.
    ///
    /// Both the URL and the `From` address are validated here, at boot, rather
    /// than at the first send. An operator reading a container that refuses to
    /// start can fix a typo; an operator reading a support ticket about a
    /// password reset that silently never arrived cannot.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`], naming the environment variable at fault, when the
    /// SMTP URL cannot be parsed or the `From` address is not a mailbox.
    pub fn new(smtp_url: Option<&str>, from: &str, base_url: &str) -> Result<Mailer> {
        let from = from.trim();
        from.parse::<Mailbox>().map_err(|e| {
            Error::validation(
                "MICAFORGE_MAIL_FROM",
                format!("not a valid mail address: {e}"),
            )
        })?;

        let transport = match smtp_url.map(str::trim).filter(|s| !s.is_empty()) {
            Some(url) => Some(
                AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                    .map_err(|e| {
                        Error::validation(
                            "MICAFORGE_SMTP_URL",
                            format!("not a usable SMTP url: {e}"),
                        )
                    })?
                    .build(),
            ),
            None => {
                tracing::info!(
                    target: "micaforge::mail",
                    "no SMTP url configured; mail will be rendered but not sent, and its links are \
                     never logged"
                );
                None
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
    /// The invitation flow uses this to decide whether to show the invite link on
    /// screen for the admin to copy, which is the only way an invitation works on
    /// an install with no relay.
    pub fn is_sending(&self) -> bool {
        self.transport.is_some()
    }

    /// The dashboard origin every link is built from.
    ///
    /// Exposed so a caller that needs to render the same link on screen uses this
    /// crate's route knowledge rather than its own copy of it. Pair it with
    /// [`templates::invitation_link`] and friends.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Send one message with both alternatives.
    ///
    /// The plain-text part is not optional and is not generated from the HTML.
    /// It is the part a screen reader and a terminal client see. With no relay
    /// configured, neither part is logged: see the crate docs.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] for a recipient that is not a mailbox, and
    /// [`Error::Upstream`] when the relay refuses or the connection fails. A
    /// caller running inside a job should treat the second as retryable and the
    /// first as permanent.
    pub async fn send(&self, to: &str, subject: &str, html: &str, text: &str) -> Result<()> {
        let recipient: Mailbox = to
            .trim()
            .parse()
            .map_err(|e| Error::validation("to", format!("not a valid mail address: {e}")))?;

        let Some(transport) = self.transport.as_ref() else {
            // Deliberately `warn`: an operator scanning for why a link never
            // arrived should find this line. The body is not written: its links
            // carry live tokens, and a log is not a place a credential may sit.
            tracing::warn!(
                target: "micaforge::mail",
                recipient = %recipient,
                subject = %subject,
                "SMTP is not configured; the message was not delivered and its links are redacted \
                 from the log. Set MICAFORGE_SMTP_URL to send it"
            );
            return Ok(());
        };

        // Re-parsed rather than stored: `Mailer`'s shape is fixed by the crate
        // contract, and `new` already proved this succeeds, so the failure arm is
        // unreachable in practice and is reported as a bug if it ever is not.
        let sender: Mailbox = self
            .from
            .parse()
            .map_err(|e| Error::Internal(format!("mail from address became invalid: {e}")))?;

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

    /// Ask a new account to confirm its address.
    pub async fn send_verification(&self, to: &str, token: &str) -> Result<()> {
        self.dispatch(to, templates::verification(&self.base_url, token))
            .await
    }

    /// Send a password-reset link.
    pub async fn send_password_reset(&self, to: &str, token: &str) -> Result<()> {
        self.dispatch(to, templates::password_reset(&self.base_url, token))
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

impl std::fmt::Debug for Mailer {
    /// Hand-written so a connection URL with a password in it can never reach a
    /// log line through a derived `Debug` on application state.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mailer")
            .field("sending", &self.is_sending())
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

    /// Held by every test that logs through the mail callsites.
    ///
    /// tracing keeps process-wide state (each callsite's cached interest, and
    /// the global maximum level) that a test logging with no subscriber can
    /// recompute while the capturing test below is running, which makes the
    /// capture see nothing. Serialising the two is what makes it deterministic.
    static LOGGING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_missing_smtp_url_is_a_working_mailer() {
        let m = Mailer::new(
            None,
            "Micaforge <no-reply@example.com>",
            "https://app.example.com/",
        )
        .expect("a mailer with no relay is the default install");
        assert!(!m.is_sending());
        assert_eq!(m.base_url(), "https://app.example.com");
    }

    #[test]
    fn a_bad_from_address_fails_at_boot() {
        let e = Mailer::new(None, "not an address", "https://app.example.com").unwrap_err();
        assert_eq!(e.status(), 422);
        assert_eq!(e.field(), Some("MICAFORGE_MAIL_FROM"));
    }

    #[test]
    fn a_bad_smtp_url_fails_at_boot() {
        let e = Mailer::new(
            Some("not-a-url"),
            "Micaforge <no-reply@example.com>",
            "https://app.example.com",
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("MICAFORGE_SMTP_URL"));
    }

    #[test]
    fn debug_never_prints_the_relay_credentials() {
        let m = Mailer::new(
            Some("smtp://user:hunter2@localhost:2525"),
            "Micaforge <no-reply@example.com>",
            "https://app.example.com",
        )
        .expect("a plain smtp url parses");
        let printed = format!("{m:?}");
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("sending: true"));
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
    fn logging_mode_never_writes_a_token_or_a_body() {
        let _serial = LOGGING.lock().unwrap_or_else(|e| e.into_inner());
        let capture = capture::Capture::default();
        let lines = capture.0.clone();
        tracing::subscriber::with_default(capture, || {
            // Another test in this binary logs through the same callsites with
            // no subscriber. If it got there first, tracing has cached them as
            // disabled, and this subscriber would see nothing. Recompute.
            tracing::callsite::rebuild_interest_cache();
            let m = Mailer::new(
                None,
                "Micaforge <no-reply@example.com>",
                "https://app.example.com",
            )
            .unwrap();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                m.send_password_reset("owner@example.com", "reset-token-7f3a")
                    .await
                    .unwrap();
                m.send_invitation("new@example.com", "Acme", "invite-token-91c2")
                    .await
                    .unwrap();
                m.send_verification("owner@example.com", "verify-token-c0de")
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
        for secret in [
            "reset-token-7f3a",
            "invite-token-91c2",
            "verify-token-c0de",
            "https://app.example.com/",
        ] {
            assert!(!all.contains(secret), "`{secret}` reached the log: {all}");
        }
        assert!(all.contains("owner@example.com"), "the recipient is named");
        assert!(
            all.contains("redacted"),
            "the line says why the link is missing"
        );
    }

    #[test]
    fn logging_mode_reports_success_and_rejects_a_bad_recipient() {
        let _serial = LOGGING.lock().unwrap_or_else(|e| e.into_inner());
        let m = Mailer::new(
            None,
            "Micaforge <no-reply@example.com>",
            "https://app.example.com",
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            m.send_verification("owner@example.com", "tok")
                .await
                .expect("logging-only mail is not an error");
            let e = m.send_verification("nonsense", "tok").await.unwrap_err();
            assert_eq!(e.status(), 422);
        });
    }
}
