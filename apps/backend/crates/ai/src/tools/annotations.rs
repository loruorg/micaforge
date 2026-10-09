//! `list_annotations`: what the team wrote on the chart.
//!
//! The context a metric cannot carry: "launched the pricing page", "outage".
//! A model explaining a drop that coincides with a note saying "moved the docs
//! to a new domain" should say so before it guesses anything.

use async_trait::async_trait;
use micaforge_core::Result;
use micaforge_store::models::annotations;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, Tool, ToolCtx, ToolOutput};

/// Most annotations returned in one call.
const MAX_NOTES: i64 = 50;

struct Annotations;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Annotations)
}

#[async_trait]
impl Tool for Annotations {
    fn name(&self) -> &'static str {
        "list_annotations"
    }

    fn description(&self) -> &'static str {
        "Notes the site's team wrote on the traffic chart in the window: releases, campaigns, \
         outages, migrations, each with its date or date range and category. Check them \
         before explaining a change: a note on the same day is the most likely cause."
    }

    fn parameters(&self) -> Json {
        schema(&[], &[])
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let rows = annotations::list_in(
            &ctx.pg,
            ctx.site.id,
            env.range.start,
            env.range.end,
            MAX_NOTES,
        )
        .await?;
        let notes: Vec<Json> = rows
            .iter()
            .map(|a| {
                json!({
                    "starts_at": a.starts_at,
                    "ends_at": a.ends_at,
                    "text": a.text,
                    "category": a.category,
                })
            })
            .collect();
        Ok(ToolOutput {
            rows: Some(notes.len()),
            data: json!({ "annotations": notes }),
            request: stats_request("/api/annotations", &env, &[]),
        })
    }
}
