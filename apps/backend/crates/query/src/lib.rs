//! `micaforge-query`: the analytical engine.
//!
//! Everything a dashboard, a CSV export, a scheduled report or the MCP server
//! asks about an event stream is answered from here. The crate has one job that
//! is worth stating before any of the reports: **no caller-supplied text ever
//! reaches ClickHouse as SQL**. A path, a property key, a regular expression, a
//! filter value and a page number all arrive as bind parameters, and the only
//! strings this crate formats into a statement are its own column names, its own
//! aggregate expressions, and integers it has already clamped.
//!
//! Layout:
//!
//! - [`sql`]: the query builder, the filter compiler and the bind plumbing.
//!   Every other module builds on it and none of them writes a predicate by
//!   hand.
//! - [`stats`]: one function per "Analytics read" endpoint in
//!   `docs/CONTRACT.md` §3.3.
//! - [`agents_q`]: the agent-traffic reports.
//! - [`gap`]: the Citation Gap, the metric this product exists for.
//! - [`export`]: rows to CSV.
//! - [`mcp_q`]: MCP server analytics, over the calls an MCP server measured.
//!
//! ## Where the metric definitions live
//!
//! They do not live here. [`micaforge_core::Metric::select_expr`] holds the
//! aggregate expression for every metric, and this crate uses those strings
//! verbatim. The consequence is worth the small awkwardness it causes: the
//! overview card, the time series, the breakdown table, the emailed report and
//! the CSV cannot each grow a slightly different definition of "bounce rate".
//!
//! Session-scoped metrics (`bounce_rate`, `avg_duration`, `views_per_session`)
//! are written against a table where one row is one session. When a report is
//! filtered, `sessions_mv` cannot answer it, because the rollup does not carry
//! event-level columns like `event_name` or `props.*`. So this crate rebuilds
//! the rollup on the fly from the *filtered* event stream, using the same column
//! names the rollup uses ([`sql::sessions_subquery`]). Core's expressions then
//! apply unchanged to either shape.
//!
//! ## Two kinds of timestamp on the wire
//!
//! - A **chart axis** is epoch milliseconds as an `i64`: `Bucket::t`,
//!   `Cohort::start`, the tuples in a trend or a per-minute series. Charts do
//!   arithmetic on these, and a string would only be parsed back.
//! - A **record timestamp** is a `chrono::DateTime<Utc>`, serialised as
//!   RFC-3339: `first_seen`, `last_crawl`, `started`, `ended`. These are read by
//!   a person, in a table.
//!
//! ## Audience
//!
//! [`micaforge_core::Audience`] chooses the population, and it changes what a
//! metric *means*, not just which rows are counted. The mapping is documented on
//! [`sql::QueryBuilder`] and on each report that supports more than `human`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agents_q;
pub mod exec;
pub mod export;
pub mod gap;
pub mod mcp_q;
pub mod sql;
pub mod stats;
// Google Search Console, read from the table its sync fills.
pub mod search;

pub use export::to_csv;
pub use gap::{DecayRow, GapRow, GapSummary, Verdict, VerdictCounts};
pub use sql::{bind_all, Param, QueryBuilder};
pub use stats::{FunnelStepDef, GoalDef, GoalKind};
