//! `get_pages`: pages with entries, exits, scroll depth and time on page.

use async_trait::async_trait;
use micaforge_core::Result;
use micaforge_query::stats;
use serde_json::Value as Json;

use super::{schema, stats_request, to_data, Tool, ToolCtx, ToolOutput};

struct Pages;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Pages)
}

#[async_trait]
impl Tool for Pages {
    fn name(&self) -> &'static str {
        "get_pages"
    }

    fn description(&self) -> &'static str {
        "The site's pages ranked by visitors, each with pageviews, entries, exits, scroll \
         depth and time on page. Use it for questions about content: which pages are read, \
         which ones people leave from, which ones hold attention."
    }

    fn parameters(&self) -> Json {
        schema(&[], &[])
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let rows = stats::pages(&ctx.ch, &env).await?;
        Ok(ToolOutput {
            rows: Some(rows.len()),
            data: to_data(&rows)?,
            request: stats_request("/api/stats/pages", &env, &[]),
        })
    }
}
