//! `get_citation_gap`: what the crawlers took against what came back.

use async_trait::async_trait;
use micaforge_core::{Error, Result};
use micaforge_query::gap;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput};

struct CitationGap;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(CitationGap)
}

#[async_trait]
impl Tool for CitationGap {
    fn name(&self) -> &'static str {
        "get_citation_gap"
    }

    fn description(&self) -> &'static str {
        "The Citation Gap: for each page, how many times AI agents fetched it beside how many \
         people arrived at it from an AI answer, with the ratio and a verdict (extracted = \
         taken with nothing coming back, compounding = the exchange is working, invisible = \
         visits with almost no crawling, quiet = too little to judge). view=summary gives the \
         site totals and the trend instead. A high ratio is not success."
    }

    fn parameters(&self) -> Json {
        schema(
            &[(
                "view",
                json!({ "type": "string", "enum": ["pages", "summary"], "description": "pages (default) or summary." }),
            )],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        match string_arg(args, "view").as_deref().unwrap_or("pages") {
            "pages" => {
                let rows = gap::by_page(&ctx.ch, &env).await?;
                Ok(ToolOutput {
                    rows: Some(rows.len()),
                    data: to_data(&rows)?,
                    request: stats_request("/api/citation-gap", &env, &[]),
                })
            }
            "summary" => {
                let summary = gap::summary(&ctx.ch, &env).await?;
                Ok(ToolOutput {
                    rows: None,
                    data: to_data(&summary)?,
                    request: stats_request("/api/citation-gap/summary", &env, &[]),
                })
            }
            other => Err(Error::validation(
                "view",
                format!("`{other}` is not a view; expected pages or summary"),
            )),
        }
    }
}
