//! `micaforge-store`: everything that talks to a database.
//!
//! Micaforge keeps three stores, and the split is deliberate rather than
//! fashionable:
//!
//! - **Postgres** ([`pg`], [`models`]) holds identity and configuration. It is
//!   small, relational, and the only store whose loss is unrecoverable, so it is
//!   the one that gets transactions and foreign keys.
//! - **ClickHouse** ([`ch`], [`writer`]) holds the event stream. Append-only,
//!   columnar, partitioned by month. Nothing here is ever updated in place.
//! - **Redis** ([`kv`]) holds what is allowed to disappear: the rotating visitor
//!   salt, the five-minute realtime window, rate-limit counters and the session
//!   window. A cold Redis costs a gap in the live view and nothing else.
//!
//! Two rules hold across all three:
//!
//! 1. **No caller text is ever interpolated into SQL.** Postgres reads use
//!    `sqlx` bind parameters; ClickHouse reads use the driver's positional `?`.
//!    The only strings this crate formats into SQL are its own schema DDL and
//!    identifiers it owns.
//! 2. **The ingest path never waits on a store.** [`writer::EventWriter`] takes
//!    a row on an unbounded channel and returns; batching, retrying and flushing
//!    happen on background tasks. A slow ClickHouse must never become a slow
//!    `POST /api/track`.
//!
//! Fallible functions return [`micaforge_core::Result`], so a handler can `?`
//! straight from a repository call to an HTTP response.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ch;
pub mod kv;
pub mod models;
pub mod pg;
pub mod writer;

pub use ch::{
    AgentEventRow, BotEventRow, Ch, ErrorRow, EventRow, ImportRawRow, PageIndexRow, ReplayRow,
};
pub use kv::Kv;
pub use pg::PgPool;
pub use writer::{EventWriter, WriterCfg};

pub use ch::{LinkClickRow, RevenueRow, UptimeCheckRow};
pub use writer::TableWriter;

pub use ch::McpCallRow;
