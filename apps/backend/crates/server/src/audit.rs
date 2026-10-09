//! Recording who changed what: the one way into the audit log.
//!
//! Every mutating handler that matters ends with one line:
//!
//! ```text
//! audit::record(&st, &auth, Entry::new("site.created").site(&site).after(site_json(&site, None))).await;
//! ```
//!
//! and the rest is done here: the actor is read off the [`AuthUser`] (a
//! person, or an API key by its name), the caller's address and user agent
//! are read off the request by the [`capture`] layer, the address is replaced
//! by a keyed digest before it is written, and the row goes to Postgres
//! (`micaforge_store::models::audit`). Handlers with no [`AuthUser`] (a
//! sign-in, the OAuth callback, a Stripe webhook) call [`record_as`] with an
//! [`Actor`] they build themselves.
//!
//! ## Recording never fails the request
//!
//! By the time a handler records, its change has happened. Answering `500`
//! because the audit insert failed would tell the caller their change did not
//! happen when it did, and invite them to make it twice. So a failed insert is
//! logged at `error`, with the action and target but without the before/after
//! values, and the request answers as it would have. An operator alerting on
//! that log line is how a gap in the record gets noticed.
//!
//! ## What goes into `before` and `after`
//!
//! The relevant fields of the target, as the API would show them, never a
//! secret: no token, no digest, no sealed key, no password hash. The values
//! are shown to every admin of the organisation and exported in the CSV, so
//! anything a handler puts there must already be something an admin may read.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::middleware::Next;
use axum::response::Response;
use chrono::Utc;
use micaforge_core::{crypto, new_id};
use micaforge_store::models::audit::{self as audit_model, AuditEvent};
use micaforge_store::models::{keys, orgs, sites, users};
use serde_json::Value;
use uuid::Uuid;

use crate::extract::{AuthMethod, AuthUser};
use crate::state::AppState;

/// Longest user agent kept, in characters.
const MAX_USER_AGENT: usize = 400;

/// Longest target or actor label kept, in characters.
const MAX_LABEL: usize = 200;

/// Every action the server records, with the label the audit screen shows.
///
/// The filter on the screen is fed from this list (`GET /api/audit/actions`),
/// and [`Entry::new`] checks in debug builds that an action is on it, so a
/// new action cannot be recorded without also being named for the reader.
pub const ACTIONS: &[(&str, &str)] = &[
    ("auth.signed_in", "Signed in with an emailed code"),
    ("auth.signed_in_oauth", "Signed in with Google or GitHub"),
    ("auth.session_revoked", "Signed a device out"),
    ("auth.sessions_revoked", "Signed every other device out"),
    ("auth.totp_enabled", "Turned two-factor on"),
    ("auth.totp_disabled", "Turned two-factor off"),
    ("auth.oauth_linked", "Linked a Google or GitHub account"),
    ("auth.oauth_unlinked", "Unlinked a Google or GitHub account"),
    ("site.created", "Added a site"),
    ("site.updated", "Changed a site's settings"),
    ("site.deleted", "Deleted a site"),
    ("site.transferred", "Moved a site to another organisation"),
    ("site.reset", "Deleted a site's data"),
    ("site.salt_rotated", "Rotated a site's visitor salt"),
    ("site.ingest_key_rotated", "Rotated a site's ingest key"),
    ("site.guards_updated", "Changed a site's ingest guards"),
    ("site.access_granted", "Gave someone a role on one site"),
    ("site.access_changed", "Changed someone's role on one site"),
    ("site.access_revoked", "Removed someone's role on one site"),
    ("org.created", "Created an organisation"),
    ("org.updated", "Renamed an organisation"),
    ("org.deleted", "Deleted an organisation"),
    ("org.owner_transferred", "Transferred ownership"),
    ("member.role_changed", "Changed someone's role"),
    ("member.removed", "Removed someone from the organisation"),
    ("invite.created", "Invited someone"),
    ("invite.resent", "Sent an invitation again"),
    ("invite.revoked", "Cancelled an invitation"),
    ("invite.accepted", "Accepted an invitation"),
    ("team.created", "Created a team"),
    ("team.updated", "Renamed a team"),
    ("team.deleted", "Deleted a team"),
    ("team.member_added", "Added someone to a team"),
    ("team.member_removed", "Removed someone from a team"),
    ("api_key.created", "Created an API key"),
    ("api_key.updated", "Changed an API key"),
    ("api_key.revoked", "Revoked an API key"),
    ("share.created", "Created a share link"),
    ("share.updated", "Changed a share link"),
    ("share.deleted", "Deleted a share link"),
    ("alert.created", "Created an alert"),
    ("alert.updated", "Changed an alert"),
    ("alert.deleted", "Deleted an alert"),
    ("report.created", "Scheduled a report"),
    ("report.updated", "Changed a report"),
    ("report.deleted", "Deleted a report"),
    ("flag.created", "Created a feature flag"),
    ("flag.updated", "Changed a feature flag"),
    ("flag.toggled", "Switched a feature flag on or off"),
    ("flag.deleted", "Deleted a feature flag"),
    ("flag.reverted", "Reverted a feature flag"),
    ("target_group.created", "Created a target group"),
    ("target_group.updated", "Changed a target group"),
    ("target_group.deleted", "Deleted a target group"),
    ("import.created", "Started an import"),
    ("import.deleted", "Deleted an import"),
    (
        "error.status_changed",
        "Resolved, ignored or reopened an error",
    ),
    ("billing.checkout_started", "Started buying a plan"),
    ("billing.subscription_changed", "The subscription changed"),
    ("billing.payment_failed", "A payment failed"),
    (
        "search_console.connected",
        "Connected Google Search Console",
    ),
    (
        "search_console.property_chosen",
        "Chose the Search Console property a site reads",
    ),
    (
        "search_console.disconnected",
        "Disconnected Google Search Console",
    ),
    ("link.created", "Created a short link"),
    ("link.updated", "Changed a short link"),
    ("link.deleted", "Deleted a short link"),
    ("github.app_installed", "Connected the GitHub App"),
    ("github.app_uninstalled", "Disconnected the GitHub App"),
    (
        "github.app_suspended",
        "The GitHub App was suspended on GitHub",
    ),
    (
        "github.app_unsuspended",
        "The GitHub App was resumed on GitHub",
    ),
    ("github.repo_linked", "Linked a GitHub repository"),
    (
        "github.repo_changed",
        "Changed what a GitHub repository records",
    ),
    ("github.repo_unlinked", "Unlinked a GitHub repository"),
    (
        "github.issue_created",
        "Opened a GitHub issue from feedback",
    ),
];

/// True when `action` is one [`ACTIONS`] names.
pub fn is_known(action: &str) -> bool {
    ACTIONS.iter().any(|(name, _)| *name == action)
}

// ---------------------------------------------------------------------------
// The request facts
// ---------------------------------------------------------------------------

/// What [`capture`] keeps about the request being served.
#[derive(Debug, Clone, Default)]
struct RequestFacts {
    ip: Option<IpAddr>,
    user_agent: Option<String>,
}

tokio::task_local! {
    /// The facts of the request being served, set once by [`capture`].
    static REQUEST: RequestFacts;
}

/// Hold the caller's address and user agent for the life of the request, so
/// [`record`] can stamp them on an audit row without every handler taking two
/// more extractors.
///
/// Applied with `axum::middleware::from_fn` to the API route group in
/// [`crate::routes::router`]. The address is resolved by the trusted-proxy
/// rule of [`crate::client_ip`], the same one the rate limiter uses.
pub async fn capture(req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());
    let facts = RequestFacts {
        ip: Some(crate::client_ip::client_ip(req.headers(), peer)),
        user_agent: req
            .headers()
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.chars().take(MAX_USER_AGENT).collect()),
    };
    REQUEST.scope(facts, next.run(req)).await
}

/// The current request's facts, or none outside a request.
fn current_facts() -> RequestFacts {
    REQUEST.try_with(Clone::clone).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Who, and what
// ---------------------------------------------------------------------------

/// Who did it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A person, by account.
    User {
        /// The account.
        id: Uuid,
        /// Their address, as it was.
        label: String,
    },
    /// An API key, by id.
    ApiKey {
        /// The key.
        id: Uuid,
        /// Its name, as it was.
        label: String,
    },
    /// The server itself: a webhook from Stripe, a job.
    System {
        /// What part of the server.
        label: String,
    },
}

impl Actor {
    /// A person.
    pub fn user(user: &users::User) -> Actor {
        Actor::User {
            id: user.id,
            label: user.email.clone(),
        }
    }

    /// The server, acting on its own.
    pub fn system(label: &str) -> Actor {
        Actor::System {
            label: label.to_string(),
        }
    }

    /// The caller behind an authenticated request: the person for a session,
    /// the key (by name) for an API key. A key whose row has gone since the
    /// request began is labelled by its id.
    pub async fn of(st: &AppState, auth: &AuthUser) -> Actor {
        match auth.method {
            AuthMethod::Session(_) => Actor::user(&auth.user),
            // An MCP client acting on the person's consent: the person is the
            // author, and the label says which client made the change.
            AuthMethod::OAuth(_) => Actor::User {
                id: auth.user.id,
                label: match crate::routes::mcp::client_name() {
                    Some(client) => format!("{} via {client}", auth.user.email),
                    None => format!("{} via an MCP client", auth.user.email),
                },
            },
            AuthMethod::ApiKey(key_id) => {
                let name = keys::get(&st.pg, key_id).await.map(|k| k.name).ok();
                let key = match name {
                    Some(name) => format!("API key \u{201c}{name}\u{201d}"),
                    None => format!("API key {key_id}"),
                };
                // A key acting through the MCP endpoint says so, and names
                // the client when it gave a name, so a change an agent made
                // reads as one in the log.
                Actor::ApiKey {
                    id: key_id,
                    label: match crate::routes::mcp::via() {
                        Some(via) => format!("{key} via {via}"),
                        None => key,
                    },
                }
            }
        }
    }

    /// `(kind, id, label)`: `user`, `api_key` or `system`, the account or key
    /// id when there is one, and the readable label.
    pub fn parts(&self) -> (&'static str, Option<Uuid>, &str) {
        match self {
            Actor::User { id, label } => ("user", Some(*id), label),
            Actor::ApiKey { id, label } => ("api_key", Some(*id), label),
            Actor::System { label } => ("system", None, label),
        }
    }
}

/// What happened, to what. Built with the methods below and handed to
/// [`record`].
#[derive(Debug, Clone)]
pub struct Entry {
    action: &'static str,
    target_type: &'static str,
    target_id: Option<String>,
    target_label: String,
    org_id: Option<Uuid>,
    site_id: Option<Uuid>,
    before: Option<Value>,
    after: Option<Value>,
    metadata: Option<Value>,
}

impl Entry {
    /// Start an entry for one of the [`ACTIONS`].
    pub fn new(action: &'static str) -> Entry {
        debug_assert!(
            is_known(action),
            "`{action}` is not in audit::ACTIONS; add it with a label"
        );
        Entry {
            action,
            target_type: "",
            target_id: None,
            target_label: String::new(),
            org_id: None,
            site_id: None,
            before: None,
            after: None,
            metadata: None,
        }
    }

    /// Name the target: its kind (`site`, `flag`, `member`...), id and label.
    pub fn target(mut self, kind: &'static str, id: impl ToString, label: &str) -> Entry {
        self.target_type = kind;
        self.target_id = Some(id.to_string());
        self.target_label = label.chars().take(MAX_LABEL).collect();
        self
    }

    /// The organisation the change belongs to.
    pub fn org(mut self, org_id: Uuid) -> Entry {
        self.org_id = Some(org_id);
        self
    }

    /// The site the change was made on. Sets the organisation too, and makes
    /// the site the target unless one is named.
    pub fn site(mut self, site: &sites::Site) -> Entry {
        self.org_id = Some(site.org_id);
        self.site_id = Some(site.id);
        if self.target_type.is_empty() {
            let label = if site.name.is_empty() {
                site.domain.clone()
            } else {
                format!("{} ({})", site.name, site.domain)
            };
            self = self.target("site", site.id, &label);
        }
        self
    }

    /// An organisation as the target, and the organisation the change belongs to.
    pub fn organisation(self, org: &orgs::Organization) -> Entry {
        self.org(org.id).target("organisation", org.id, &org.name)
    }

    /// The target's state before the change.
    pub fn before(mut self, value: Value) -> Entry {
        self.before = Some(value);
        self
    }

    /// The target's state after the change.
    pub fn after(mut self, value: Value) -> Entry {
        self.after = Some(value);
        self
    }

    /// Anything else worth keeping with the row.
    pub fn meta(mut self, value: Value) -> Entry {
        self.metadata = Some(value);
        self
    }

    /// The action this entry records.
    pub fn action(&self) -> &'static str {
        self.action
    }
}

/// Record what an authenticated caller did. Never fails: see the module docs.
pub async fn record(st: &AppState, auth: &AuthUser, entry: Entry) {
    let actor = Actor::of(st, auth).await;
    record_as(st, actor, entry).await;
}

/// Record what somebody did, when the handler has no [`AuthUser`].
pub async fn record_as(st: &AppState, actor: Actor, entry: Entry) {
    let event = build(st, &actor, entry, current_facts());
    if let Err(e) = audit_model::insert(&st.pg, &event).await {
        tracing::error!(
            action = event.action,
            target_type = event.target_type,
            target_id = event.target_id.as_deref().unwrap_or(""),
            error = %e,
            "an audit event could not be written; the change itself was made"
        );
    }
}

/// Assemble the row. Split from [`record_as`] so it can be tested without a
/// database.
fn build(st: &AppState, actor: &Actor, entry: Entry, facts: RequestFacts) -> AuditEvent {
    let (kind, actor_id, label) = actor.parts();
    AuditEvent {
        id: new_id(),
        org_id: entry.org_id,
        actor_kind: kind.to_string(),
        actor_id,
        actor_label: label.chars().take(MAX_LABEL).collect(),
        action: entry.action.to_string(),
        target_type: if entry.target_type.is_empty() {
            "account".to_string()
        } else {
            entry.target_type.to_string()
        },
        target_id: entry.target_id,
        target_label: entry.target_label,
        site_id: entry.site_id,
        before: entry.before,
        after: entry.after,
        metadata: entry.metadata,
        ip_hash: facts.ip.map(|ip| ip_digest(st, ip)),
        user_agent: facts.user_agent,
        created_at: Utc::now(),
    }
}

/// The salted digest of an address that an audit row keeps instead of it.
///
/// The same construction as `sessions_auth.ip_hash`, under a key of its own
/// purpose: two rows from one address share a digest, so "these three changes
/// came from the same place" is answerable, and the address itself is not.
fn ip_digest(st: &AppState, ip: IpAddr) -> String {
    crypto::visitor_hash(&st.cfg.derive_key("audit-ip"), 0, &ip.to_string(), "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_is_dotted_lower_case_and_listed_once() {
        let mut seen = std::collections::BTreeSet::new();
        for (action, label) in ACTIONS {
            assert!(seen.insert(*action), "{action} is listed twice");
            assert!(
                action
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_'),
                "{action}"
            );
            assert_eq!(action.matches('.').count(), 1, "{action}");
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn an_entry_carries_its_site_and_organisation() {
        let site = sites::Site {
            id: Uuid::from_u128(10),
            site_id: 3,
            org_id: Uuid::from_u128(1),
            domain: "example.com".into(),
            name: "Docs".into(),
            timezone: "UTC".into(),
            public: false,
            salt_rotated_at: None,
            created_at: Utc::now(),
            settings: serde_json::json!({}),
        };
        let entry = Entry::new("site.reset").site(&site);
        assert_eq!(entry.org_id, Some(site.org_id));
        assert_eq!(entry.site_id, Some(site.id));
        assert_eq!(entry.target_type, "site");
        assert_eq!(entry.target_label, "Docs (example.com)");

        // A named target is kept; the site only adds where it happened.
        let flagged = Entry::new("flag.created")
            .target("flag", "new-nav", "new-nav")
            .site(&site);
        assert_eq!(flagged.target_type, "flag");
        assert_eq!(flagged.site_id, Some(site.id));
    }

    #[test]
    fn labels_are_bounded() {
        let long = "x".repeat(MAX_LABEL * 2);
        let entry = Entry::new("team.created").target("team", "t", &long);
        assert_eq!(entry.target_label.chars().count(), MAX_LABEL);
    }

    #[test]
    fn the_actor_is_read_off_the_credential() {
        let user = users::User {
            id: Uuid::from_u128(7),
            email: "ada@example.com".into(),
            name: "Ada".into(),
            avatar_url: None,
            email_verified_at: None,
            totp_secret_sealed: None,
            created_at: Utc::now(),
        };
        let actor = Actor::user(&user);
        assert_eq!(actor.parts(), ("user", Some(user.id), "ada@example.com"));
        assert_eq!(
            Actor::system("stripe webhook").parts(),
            ("system", None, "stripe webhook")
        );
    }

    #[tokio::test]
    async fn the_request_facts_are_visible_for_the_request_only() {
        assert!(current_facts().ip.is_none());
        let facts = RequestFacts {
            ip: Some("203.0.113.7".parse().unwrap()),
            user_agent: Some("curl/8".into()),
        };
        REQUEST
            .scope(facts, async {
                assert_eq!(
                    current_facts().ip,
                    Some("203.0.113.7".parse::<IpAddr>().unwrap())
                );
                assert_eq!(current_facts().user_agent.as_deref(), Some("curl/8"));
            })
            .await;
        assert!(current_facts().user_agent.is_none());
    }
}
