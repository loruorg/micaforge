//! `get_sources`: acquisition, with AI answers as a channel of its own.

use async_trait::async_trait;
use micaforge_core::Result;
use micaforge_query::stats;
use serde_json::Value as Json;

use super::{schema, stats_request, to_data, Tool, ToolCtx, ToolOutput};

struct Sources;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Sources)
}

#[async_trait]
impl Tool for Sources {
    fn name(&self) -> &'static str {
        "get_sources"
    }

    fn description(&self) -> &'static str {
        "Where visitors came from, as channels (direct, search, answer_engine, social, \
         referral, email, paid, internal), then sources inside each channel, then campaigns. \
         answer_engine is visits from AI answers: ChatGPT, Perplexity, Claude, Gemini, \
         Copilot and their peers. Use it for any question about traffic from AI tools, \
         search or campaigns."
    }

    fn parameters(&self) -> Json {
        schema(&[], &[])
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let tree = stats::sources(&ctx.ch, &env).await?;
        Ok(ToolOutput {
            rows: None,
            data: to_data(&tree)?,
            request: stats_request("/api/stats/sources", &env, &[]),
        })
    }
}
