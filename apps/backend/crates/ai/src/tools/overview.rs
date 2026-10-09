//! `get_overview`: the headline totals the dashboard opens on.
//!
//! The smallest complete tool, and the one to copy when adding another: a
//! name, a description written for the model, a schema built from the shared
//! envelope properties, and a `run` that builds the envelope through
//! [`ToolCtx::envelope`] (which keeps the caller's filters) and calls the same
//! report the HTTP endpoint calls.

use async_trait::async_trait;
use micaforge_core::Result;
use micaforge_query::stats;
use serde_json::Value as Json;

use super::{schema, stats_request, to_data, Tool, ToolCtx, ToolOutput};

struct Overview;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Overview)
}

#[async_trait]
impl Tool for Overview {
    fn name(&self) -> &'static str {
        "get_overview"
    }

    fn description(&self) -> &'static str {
        "Headline totals for a window: visitors, sessions, pageviews, bounce rate, average \
         session duration and views per session. With compare=previous it also returns the \
         previous period, so you can state the change. Start here for any question about \
         overall traffic."
    }

    fn parameters(&self) -> Json {
        schema(&[], &[])
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let report = stats::overview(&ctx.ch, &env).await?;
        Ok(ToolOutput {
            data: to_data(&report)?,
            request: stats_request("/api/stats/overview", &env, &[]),
            rows: None,
        })
    }
}
