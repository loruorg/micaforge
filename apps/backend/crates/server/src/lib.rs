//! `micaforge-server`: the HTTP surface, and the process that runs it.
//!
//! One binary serves everything: collection (`/api/track`, `/api/log/edge`,
//! `/mf.js`), the analytics reads, the management API, the MCP endpoint, and the
//! embedded job worker. `PRODUCT.md` promises `docker compose up` on a single
//! VPS with Postgres, ClickHouse and Redis beside it, and that budget has no
//! room for a second service, so there is no separate worker image, no
//! migration container and no scheduler.
//!
//! # Layout
//!
//! - [`state`]: the handles every route shares, built once in `main`.
//! - [`extract`]: who is calling, which site, and over what window.
//! - [`error`]: [`micaforge_core::Error`] rendered as the contract's error
//!   envelope, with the private detail stripped and logged.
//! - [`response`]: the `{ data, meta }` success envelope.
//! - [`middleware`]: request ids, the access log, CORS, rate limiting, the panic
//!   catcher and the security headers.
//! - [`client_ip`]: which address a request is attributed to, and why.
//! - [`routes`]: one module per area of `docs/CONTRACT.md` §3.3.
//!
//! # Why this is a library as well as a binary
//!
//! The route modules compile into the library, and `main.rs` is a thin boot
//! sequence over it. That split is what lets an integration test name
//! `micaforge_server::routes::router` and drive the whole application through
//! `tower::ServiceExt::oneshot` without a process, a port or a signal handler.

#![forbid(unsafe_code)]

pub mod client_ip;
pub mod error;
pub mod extract;
pub mod middleware;
pub mod response;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use state::{AppState, Ctx};

/// The version this binary was built as, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
