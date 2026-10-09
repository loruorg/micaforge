//! `get_retention`: how many of each cohort came back.

use async_trait::async_trait;
use micaforge_core::{time, Granularity, Result};
use micaforge_query::stats;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput};

/// Cohorts when the model does not ask for a number.
const DEFAULT_COHORTS: u32 = 8;

struct Retention;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Retention)
}

#[async_trait]
impl Tool for Retention {
    fn name(&self) -> &'static str {
        "get_retention"
    }

    fn description(&self) -> &'static str {
        "The retention grid: visitors grouped by the period they were first seen in the \
         window (a cohort), and the share of each cohort seen again in each later period. \
         period is day, week (default) or month. Use it for questions about returning \
         readers and loyalty. Visitors are anonymous daily hashes, so long-period retention \
         is a lower bound."
    }

    fn parameters(&self) -> Json {
        schema(
            &[
                (
                    "period",
                    json!({ "type": "string", "enum": ["day", "week", "month"], "description": "Cohort width. Defaults to week." }),
                ),
                (
                    "cohorts",
                    json!({ "type": "integer", "minimum": 1, "maximum": 26, "description": "How many cohorts. Defaults to 8." }),
                ),
            ],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let mut env = ctx.envelope(args)?;
        let period = match string_arg(args, "period") {
            Some(raw) => Granularity::parse(&raw)?,
            None => Granularity::Week,
        };
        env.granularity = time::fit_granularity(&env.range, period);
        let cohorts = args
            .get("cohorts")
            .and_then(Json::as_u64)
            .map(|n| n.clamp(1, 26) as u32)
            .unwrap_or(DEFAULT_COHORTS);

        let grid = stats::retention(&ctx.ch, &env, env.granularity, cohorts).await?;
        Ok(ToolOutput {
            rows: Some(grid.cohorts.len()),
            data: to_data(&grid)?,
            request: stats_request(
                "/api/stats/retention",
                &env,
                &[
                    ("granularity", env.granularity.as_str().to_string()),
                    ("cohorts", cohorts.to_string()),
                ],
            ),
        })
    }
}
