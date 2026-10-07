//! The handles every route shares.
//!
//! [`AppState`] is built once in `main` and cloned per request, so every field
//! is something that is cheap to clone: a pool, an `Arc`, or a handle over a
//! multiplexed connection. Nothing here is a lock the request path contends on,
//! with one deliberate exception ([`AppState::ranges`]) that is explained on the
//! field itself.
//!
//! Two of these are shared with the embedded job worker rather than owned by the
//! server: `pg` and `ch` are the same pools the jobs use, because a second pool
//! against the same Postgres would double the connection budget for no gain.

use std::sync::Arc;

use micaforge_agents::verify::RangeStore;
use micaforge_core::Config;
use micaforge_enrich::Geo;
use micaforge_mail::Mailer;
use micaforge_store::{Ch, EventWriter, Kv, PgPool};
use tokio::sync::RwLock;

/// Everything a handler can reach.
#[derive(Clone)]
pub struct AppState {
    /// Process configuration, read once at boot and immutable after.
    pub cfg: Arc<Config>,
    /// Identity and configuration. Never on the ingest path.
    pub pg: PgPool,
    /// The event store. Reads only; writes go through [`AppState::writer`].
    pub ch: Ch,
    /// Sessions, rate limits, the rotating visitor salt and the realtime window.
    pub kv: Kv,
    /// The batching ClickHouse writer. `push_*` returns immediately, which is
    /// what keeps a slow event store from becoming a slow `POST /api/track`.
    pub writer: EventWriter,
    /// The MaxMind readers. Read-only after construction, so no lock is needed
    /// even though every hit queries it.
    pub geo: Arc<Geo>,
    /// Operators' published IP ranges, for agent verification.
    ///
    /// The one lock in this struct. It is a `tokio::sync::RwLock` rather than a
    /// `std` one because the read guard is held across the `verify` call inside
    /// an `async fn` in the ingest path, and a `std` guard is not `Send`. It is
    /// written twice a day at most, by the reloader in `main`, and read once per
    /// named agent fetch, so the contention is nil.
    pub ranges: Arc<RwLock<RangeStore>>,
    /// Outgoing mail. `Arc` because [`Mailer`] holds an SMTP transport and is
    /// not itself `Clone`; the worker holds its own, built from the same
    /// configuration.
    pub mailer: Arc<Mailer>,
    /// The one outbound HTTP client. Shared with the worker: `reqwest::Client`
    /// is documented as something to build once and reuse, and a second one
    /// would mean a second connection pool to every host a policy crawl reads.
    pub http: reqwest::Client,
}

impl std::fmt::Debug for AppState {
    /// Hand-written. `Config` carries `secret_key_base`, and the Redis and SMTP
    /// URLs carry passwords; none of them belong in a span a handler might emit.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("clickhouse_db", &self.ch.db)
            .field("geo_loaded", &self.geo.is_loaded())
            .finish_non_exhaustive()
    }
}

/// The state extractor, spelled once.
///
/// Handlers write `State(st): Ctx` where the full form would be
/// `State(st): axum::extract::State<AppState>`.
pub type Ctx = axum::extract::State<AppState>;

// ---------------------------------------------------------------------------
// Sub-state extraction
// ---------------------------------------------------------------------------
//
// `FromRef` lets a handler ask for one handle instead of the whole struct, which
// is what makes `State(pg): State<PgPool>` compile. The extractors in
// `crate::extract` take the whole state, so these exist for handlers and tests
// that want to be honest about needing only one store.

impl axum::extract::FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> PgPool {
        state.pg.clone()
    }
}

impl axum::extract::FromRef<AppState> for Ch {
    fn from_ref(state: &AppState) -> Ch {
        state.ch.clone()
    }
}

impl axum::extract::FromRef<AppState> for Kv {
    fn from_ref(state: &AppState) -> Kv {
        state.kv.clone()
    }
}

impl axum::extract::FromRef<AppState> for Arc<Config> {
    fn from_ref(state: &AppState) -> Arc<Config> {
        Arc::clone(&state.cfg)
    }
}

impl axum::extract::FromRef<AppState> for EventWriter {
    fn from_ref(state: &AppState) -> EventWriter {
        state.writer.clone()
    }
}
