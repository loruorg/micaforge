//! `micaforge-core`: the vocabulary every other Micaforge crate speaks.
//!
//! This crate holds no I/O and no framework. It exists so that the ingest path,
//! the query engine, the job runner and the HTTP layer all agree on what a
//! dimension is, what a time range means, how an error is reported, and how a
//! visitor hash is computed. When two crates disagree about one of those, the
//! product reports two different numbers for the same question, which is the one
//! failure an analytics tool cannot recover from.
//!
//! Layout:
//!
//! - [`config`]: process configuration, read once from `MICAFORGE_*` env vars.
//! - [`error`]: the single error enum, plus its wire code and HTTP status.
//! - [`ids`]: UUIDv7 primary keys, generated application-side.
//! - [`crypto`]: password hashing, sealed secrets, token and visitor digests.
//! - [`types`]: the shared domain vocabulary (dimensions, metrics, filters).
//! - [`time`]: range resolution, bucket granularity, comparison periods.
//! - [`filters`]: parsing the wire filter shape from `docs/CONTRACT.md` §3.2.
//!
//! Deliberately **not** here: any `axum` types. The server crate owns the
//! mapping from [`Error`] to a response, so this crate stays usable from the
//! worker binary, the importer and the tests without pulling in HTTP.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod config;
pub mod crypto;
pub mod error;
pub mod filters;
pub mod ids;
pub mod time;
pub mod types;

pub use config::Config;
pub use error::{Error, Result};
pub use ids::new_id;
pub use types::*;
