//! Every HTTP route, assembled.
//!
//! Each module in this directory exposes one `router() -> Router<AppState>` and
//! declares **absolute** paths (`/api/stats/overview`, not `/overview`). That is
//! why they are merged here rather than nested: several of them straddle two
//! prefixes. `routes::gap` serves both `/api/citation-gap` and
//! `/api/content/decay`, and `routes::sites` serves `/api/sites` and
//! `/api/share`. Nesting would force those apart into modules that do not match
//! how the product thinks about them.
//!
//! ## Three groups, because they need different treatment
//!
//! The split below is not organisational. Each group gets layers the others must
//! not have, and putting a route in the wrong one is a real defect:
//!
//! - **Collection** (`ingest`, `script`). Open to every origin by contract, and
//!   the only surface a stranger's browser reaches. No CORS layer: `ingest`
//!   writes its own headers per response, and two `access-control-allow-origin`
//!   headers on one response is a CORS *failure*. No body limit either: both
//!   modules cap their own reads, and `ingest` needs two megabytes for a
//!   server-side batch where an ordinary JSON route needs one. The short-link
//!   redirect (`links::redirect_router`) and the checkout reference
//!   (`revenue::public_router`) are here too, for the same reasons: strangers
//!   reach them, and they write their own headers. They read no body.
//! - **Uploads** (`imports`). A GA4 export is measured in gigabytes and is
//!   streamed to a scratch file field by field, so the body limit is
//!   `routes::imports`' own [`imports::BODY_LIMIT`] rather than the JSON one.
//! - **The API** (everything else). A strict origin allow-list and a one-megabyte
//!   body.
//!
//! A fourth, small group carries the OAuth endpoints an MCP client calls with
//! no session (`oauth::public`): the `.well-known` metadata, registration, the
//! token and revocation endpoints. A browser-based MCP client must be able to
//! read them from any origin, so they answer every origin **without**
//! credentials, which is safe precisely because none of them reads a cookie.
//!
//! The layers that apply to all three (the panic catcher, the access log, the
//! rate limiter, the security headers) are applied once around the whole thing
//! in `main`, so that a route added to any group inherits them without anybody
//! having to remember to ask.
//!
//! The API and upload groups also carry [`crate::extract::key_scope`], which
//! holds an API key's grant for the life of the request so the role functions
//! can bound it. A key presented on a group without that layer is refused, so
//! forgetting it on a new group fails closed.

use axum::extract::DefaultBodyLimit;
use axum::http::{header, Method};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use micaforge_core::Config;

use crate::middleware;
use crate::state::AppState;

pub mod admin;
pub mod agents;
pub mod ai;
pub mod annotations;
pub mod audit_log;
pub mod auth;
pub mod behaviour;
pub mod billing;
pub mod chat;
pub mod config_objects;
pub mod experiments;
pub mod export;
pub mod feedback;
pub mod flag_delivery;
pub mod flags;
pub mod gap;
pub mod guards;
pub mod health;
pub mod imports;
pub mod ingest;
pub mod insights;
pub mod keys;
pub mod links;
pub mod log_drain;
pub mod log_drains;
pub mod mcp;
pub mod mcp_ai;
pub mod mcp_annotations;
pub mod mcp_feedback;
pub mod mcp_flags;
pub mod mcp_goals;
pub mod mcp_links;
pub mod model_context;
pub mod notify;
pub mod oauth;
pub mod oauth_sign_in;
pub mod orgs;
pub mod people;
pub mod policy;
pub mod profiles;
pub mod raw_export;
pub mod revenue;
pub mod script;
pub mod search_console;
pub mod signals;
pub mod site_ops;
pub mod sites;
pub mod stats;
pub mod status_pages;
pub mod uptime;
// Tool sets for `POST /api/mcp`, not routers: declared `pub(crate)` so the
// module count below, which pairs every `pub mod` with one `router()`, does
// not take them for routes.
pub(crate) mod mcp_monitoring;
pub(crate) mod mcp_search;
// The GitHub App: its routes, then its MCP tools.
pub mod github;
pub mod mcp_github;
pub(crate) mod mcp_servers;

/// The whole application, still waiting for its state.
///
/// `cfg` is taken by reference rather than read from the state because the CORS
/// allow-list has to be built while the router is being assembled, and the state
/// is not attached until `main` calls `with_state`.
pub fn router(cfg: &Config) -> Router<AppState> {
    let api = Router::new()
        // Reads: the analytics surface of `docs/CONTRACT.md` §3.3.
        .merge(stats::router())
        .merge(behaviour::router())
        .merge(people::router())
        .merge(export::router())
        // The wedge.
        .merge(agents::router())
        .merge(gap::router())
        .merge(policy::router())
        // Management.
        .merge(auth::router())
        .merge(orgs::router())
        .merge(sites::router())
        .merge(config_objects::router())
        .merge(experiments::router())
        .merge(notify::router())
        .merge(keys::router())
        .merge(admin::router())
        // Machine surfaces that are still ordinary JSON routes.
        .merge(mcp::router())
        .merge(health::router())
        .merge(ai::router())
        .merge(insights::router())
        .merge(chat::router())
        .merge(feedback::router())
        .merge(annotations::router())
        .merge(oauth::router())
        // Monitoring and growth: uptime, status pages, short links, revenue.
        .merge(uptime::router())
        .merge(status_pages::router())
        .merge(links::router())
        .merge(revenue::router())
        .merge(audit_log::router())
        .merge(billing::router())
        .merge(flags::router())
        .merge(guards::router())
        .merge(oauth_sign_in::router())
        .merge(site_ops::router())
        // The management half of log drains; the intake is in `collection`.
        .merge(log_drains::router())
        // The raw rows as a streamed ZIP. Here rather than beside `export`, to
        // keep this list append-only; `middleware::budget_for` exempts it.
        .merge(raw_export::router())
        // Frustration signals and the sampling reading, then identified profiles.
        .merge(signals::router())
        .merge(profiles::router())
        // Google Search Console: connecting a site, and the search reads.
        .merge(search_console::router())
        // The GitHub App: install, link, the webhook, and feedback issues.
        .merge(github::router())
        // MCP server analytics: the reads. The intake is in `collection`.
        .merge(model_context::router())
        .layer(axum::middleware::from_fn(crate::extract::key_scope))
        // The caller's address and user agent, for the audit log.
        .layer(axum::middleware::from_fn(crate::audit::capture))
        .layer(DefaultBodyLimit::max(middleware::JSON_BODY_LIMIT))
        // Inside CORS, so a `503 timeout` carries the CORS headers too: a
        // browser on another origin would otherwise hide it and report a
        // network failure instead.
        .layer(axum::middleware::from_fn(middleware::timeout))
        .layer(middleware::cors(cfg));

    let uploads = imports::router()
        // The upload is read chunk by chunk under `routes::imports`' own
        // ceiling, and this limit sits just above it, so the module's check is
        // the one a caller normally meets while nothing larger is ever read.
        .layer(axum::middleware::from_fn(crate::extract::key_scope))
        .layer(axum::middleware::from_fn(crate::audit::capture))
        .layer(DefaultBodyLimit::max(imports::BODY_LIMIT))
        .layer(axum::middleware::from_fn(middleware::timeout))
        .layer(middleware::cors(cfg));

    // The short-link redirect and the checkout reference are stranger-facing
    // like ingest: open to every origin, answering for themselves.
    let collection = ingest::router()
        .merge(script::router())
        .merge(links::redirect_router())
        .merge(revenue::public_router())
        .merge(flag_delivery::router())
        .merge(log_drain::router())
        .merge(model_context::collection_router())
        .layer(axum::middleware::from_fn(middleware::timeout));

    let oauth_public = oauth::public()
        .layer(DefaultBodyLimit::max(OAUTH_BODY_LIMIT))
        .layer(axum::middleware::from_fn(middleware::timeout))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::AUTHORIZATION,
                    header::HeaderName::from_static("mcp-protocol-version"),
                ]),
        );

    // The fallback serves short links at the root of MICAFORGE_LINKS_HOST and
    // answers every other unknown path with the API's ordinary `404`.
    Router::new()
        .merge(api)
        .merge(uploads)
        .merge(collection)
        .merge(oauth_public)
        .fallback(links::host_fallback)
}

/// The OAuth endpoints read a registration or a token form: kilobytes.
const OAUTH_BODY_LIMIT: usize = 64 * 1024;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A configuration with nothing in it that touches a network.
    ///
    /// Built by hand rather than through `Config::from_env`, because the
    /// environment is process-global and a test that set six variables would
    /// race every other test in this crate.
    pub(crate) fn cfg() -> Config {
        Config {
            bind_addr: "127.0.0.1:0".into(),
            database_url: "postgres://localhost/micaforge".into(),
            clickhouse_url: "http://localhost:8123".into(),
            clickhouse_db: "micaforge".into(),
            clickhouse_user: "default".into(),
            clickhouse_password: String::new(),
            redis_url: "redis://127.0.0.1:6379".into(),
            public_url: "https://mf.example.com".into(),
            dashboard_url: "https://app.example.com".into(),
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
            mail_from: "Micaforge <noreply@localhost>".into(),
            log_format: "text".into(),
            worker_concurrency: 2,
            llm: None,
            chat: Default::default(),
            feedback_channel: Default::default(),
            region: "default".into(),
            links_host: None,
            google_oauth: None,
            github_oauth: None,
            billing_enabled: false,
            billing_plans: Vec::new(),
            stripe: None,
            search_console: None,
            github_app: None,
        }
    }

    /// The whole point of this test.
    ///
    /// `Router::merge` panics when two routers claim the same path and method,
    /// or when two routes disagree about what to call a parameter in the same
    /// position. Twenty modules declare absolute paths independently, so that is
    /// a real collision risk and the only place it would otherwise surface is at
    /// boot, in production, as a process that will not start.
    #[test]
    fn every_route_module_merges_without_a_collision() {
        let _ = router(&cfg());
    }

    /// The groups must cover every module exactly once.
    ///
    /// Adding a module to `mod` and forgetting to merge it is a silent `404` on
    /// a whole area of the product; merging it into two groups is a panic. This
    /// counts the merges in the source of `router` itself, so it fails the
    /// moment the two lists disagree. `mcp_<area>` modules hold MCP tools, not
    /// routes, and are registered in `mcp.rs` instead.
    #[test]
    fn every_declared_module_is_merged_exactly_once() {
        let source = include_str!("mod.rs");
        let declared: Vec<&str> = source
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub mod "))
            .filter_map(|rest| rest.strip_suffix(';'))
            .filter(|module| !module.starts_with("mcp_"))
            .collect();
        assert_eq!(declared.len(), 45, "a module was added or removed");

        for module in declared {
            // Preceded by `(` or a space, so `auth` is not counted inside `oauth`.
            let merges = source.matches(&format!("({module}::router()")).count()
                + source.matches(&format!(" {module}::router()")).count();
            assert_eq!(merges, 1, "`{module}` must be routed exactly once");
        }
    }
}
