//! `micaforge-ai`: the AI analyst.
//!
//! A person asks a question about one site in plain words ("which pages lost
//! readers this week?"). A language model answers it by calling tools, and
//! every tool is one of the reports the dashboard already draws, run through
//! the same query engine with the caller's own authority and filters. The model
//! never writes SQL and never sees a table: it chooses a report and its
//! arguments, reads the rows that come back, and writes the answer from them.
//!
//! That shape is what makes three promises possible:
//!
//! 1. **Every number has a source.** Each answer carries the tool calls that
//!    produced it, with the equivalent stats request and the exact SQL each
//!    read ran ([`ToolTrace`]). The dashboard shows them as "the queries behind
//!    this answer".
//! 2. **The model can read only what the caller can.** The site is fixed
//!    before the loop starts and authorised by the caller of this crate; the
//!    caller's filters are merged into every query, and a tool cannot widen
//!    them. See [`tools::ToolCtx`].
//! 3. **Nothing leaves the server unless an operator configured a model.**
//!    [`Llm::from_config`] returns `None` without `MICAFORGE_LLM_BASE_URL`, and
//!    every caller turns that into a plain "AI is not configured".
//!
//! Layout:
//!
//! - [`llm`]: the OpenAI-compatible `chat/completions` client, with tool calls.
//! - [`tools`]: the registry. One file per tool in `src/tools/`, found by the
//!   build script, so adding a tool is adding a file.
//! - [`analyst`]: the bounded tool-calling loop.
//! - [`limit`]: the per-organisation daily question limit.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod analyst;
pub mod limit;
pub mod llm;
pub mod tools;

pub use analyst::{ask, AnalystError, Answer, Progress, Turn};
pub use llm::Llm;
pub use tools::{BaseQuery, Registry, Tool, ToolCtx, ToolOutput, ToolTrace};
