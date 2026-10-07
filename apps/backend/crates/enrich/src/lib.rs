//! `micaforge-enrich`: everything derived from one raw hit.
//!
//! Ingest receives very little: a short JSON payload from the tracker (or a
//! signed line from a log shipper), a `User-Agent` header, a set of Client Hint
//! headers, a referrer and a socket address. Every other column in the `events`
//! table is *derived*, and this crate is where that derivation happens.
//!
//! - [`ua`]: `User-Agent` + Client Hints → browser, OS, device type.
//! - [`geo`]: IP → country, region, city, ASN, and whether the ASN is a
//!   hosting provider.
//! - [`referrer`]: referrer + URL → channel, source, UTM campaign, click id.
//! - [`identity`]: the daily-rotating visitor hash and the session id.
//! - [`url_norm`]: one URL path spelled one way, plus a coarse page template.
//!
//! Two constraints shape the whole crate.
//!
//! **It never touches the network and never touches a database.** Enrichment
//! sits on the hot path of the ingest handler, between the request arriving and
//! the row being pushed onto the ClickHouse batch. A lookup that could block is
//! a lookup that cannot happen here; the GeoIP database is a local file read
//! once at startup, and every other table is compiled in.
//!
//! **It does not know what an AI agent is.** Answer-engine classification is
//! the job of `micaforge-agents`, which owns the agent catalogue and is free to
//! read it from Postgres. [`referrer::attribute`] takes the answer-engine lookup
//! as a callback so the two crates never depend on each other and the server
//! composes them. Without that split, the ingest path would drag the catalogue,
//! its refresh job and its store into every test in this crate.
//!
//! Nothing here is fallible in the `Result` sense. A hit with a broken referrer,
//! a nonsense user agent or an IP that is not in the database is still a hit,
//! and dropping it would silently under-report. Every function degrades to an
//! empty or default value instead, because an unknown browser has to be
//! reportable as unknown.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod geo;
pub mod identity;
pub mod referrer;
pub mod ua;
pub mod url_norm;

pub use geo::{Geo, Location};
pub use identity::{new_session_id, visitor_id, SESSION_WINDOW_SECS};
pub use referrer::{attribute, parse_utm, Attribution, Utm};
pub use ua::Client;
