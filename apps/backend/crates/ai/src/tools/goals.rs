//! `get_goals`: conversions per configured goal.
//!
//! Reads Postgres for the goal definitions and ClickHouse for the counts, the
//! same two reads the goals screen makes.

use async_trait::async_trait;
use micaforge_core::Result;
use micaforge_query::{stats, GoalDef};
use micaforge_store::models::goals;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, to_data, Tool, ToolCtx, ToolOutput};

struct Goals;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Goals)
}

#[async_trait]
impl Tool for Goals {
    fn name(&self) -> &'static str {
        "get_goals"
    }

    fn description(&self) -> &'static str {
        "Every goal configured on this site with its conversions, conversion rate and revenue \
         in the window. Use it for questions about signups, purchases or any outcome the site \
         owner defined, and to learn goal names for get_timeseries. An empty list means no \
         goal is configured, not that nothing converted."
    }

    fn parameters(&self) -> Json {
        schema(&[], &[])
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let configured = goals::list(&ctx.pg, ctx.site.id).await?;
        let defs = configured
            .iter()
            .map(|row| {
                GoalDef::parse(
                    row.id.to_string(),
                    row.name.clone(),
                    &row.kind,
                    &row.conditions.to_string(),
                    row.value_cents,
                )
            })
            .collect::<Result<Vec<_>>>()?;

        let request = stats_request("/api/stats/goals", &env, &[]);
        if defs.is_empty() {
            return Ok(ToolOutput {
                data: json!({ "goals": [], "note": "no goal is configured on this site" }),
                request,
                rows: Some(0),
            });
        }
        let rows = stats::goals(&ctx.ch, &env, &defs).await?;
        Ok(ToolOutput {
            rows: Some(rows.len()),
            data: json!({ "goals": to_data(&rows)? }),
            request,
        })
    }
}
