//! `get_timeseries`: one metric over time, for "when did it change?".

use async_trait::async_trait;
use micaforge_core::{Error, Metric, Result};
use micaforge_query::{stats, GoalDef};
use micaforge_store::models::goals;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput};

struct Timeseries;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Timeseries)
}

#[async_trait]
impl Tool for Timeseries {
    fn name(&self) -> &'static str {
        "get_timeseries"
    }

    fn description(&self) -> &'static str {
        "One metric bucketed over time (by hour, day, week or month), with the previous \
         period beside it when compare=previous. Use it to find when a change started or \
         ended. metric is one of visitors, sessions, pageviews, bounce_rate, avg_duration, \
         views_per_session, events, conversions, conversion_rate, revenue, agent_fetches, \
         answer_arrivals, gap_ratio; conversions and conversion_rate need `goal`."
    }

    fn parameters(&self) -> Json {
        schema(
            &[
                (
                    "metric",
                    json!({ "type": "string", "description": "The metric to plot. Defaults to visitors." }),
                ),
                (
                    "granularity",
                    json!({ "type": "string", "enum": ["hour", "day", "week", "month"], "description": "Bucket width. Chosen from the window when left out." }),
                ),
                (
                    "goal",
                    json!({ "type": "string", "description": "A goal's name or id, for conversions and conversion_rate." }),
                ),
            ],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let metric = match string_arg(args, "metric") {
            Some(raw) => Metric::parse(&raw)?,
            None => Metric::Visitors,
        };

        let goal = match string_arg(args, "goal") {
            Some(reference) => Some(find_goal(ctx, &reference).await?),
            None => None,
        };
        if matches!(metric, Metric::Conversions | Metric::ConversionRate) && goal.is_none() {
            return Err(Error::validation(
                "goal",
                "conversions need a goal; call get_goals for their names",
            ));
        }

        let series = stats::timeseries(&ctx.ch, &env, metric, goal.as_ref()).await?;
        let mut extra = vec![
            ("metric", metric.as_str().to_string()),
            ("granularity", env.granularity.as_str().to_string()),
        ];
        if let Some(def) = goal.as_ref() {
            extra.push(("goal", def.id.clone()));
        }
        Ok(ToolOutput {
            rows: Some(series.buckets.len()),
            data: to_data(&series)?,
            request: stats_request("/api/stats/timeseries", &env, &extra),
        })
    }
}

/// A goal on this site, by id or by name.
pub(crate) async fn find_goal(ctx: &ToolCtx, reference: &str) -> Result<GoalDef> {
    let rows = goals::list(&ctx.pg, ctx.site.id).await?;
    let wanted = reference.trim().to_lowercase();
    let row = rows
        .iter()
        .find(|g| g.id.to_string() == wanted || g.name.trim().to_lowercase() == wanted)
        .ok_or_else(|| {
            Error::validation("goal", format!("no goal named `{reference}` on this site"))
        })?;
    GoalDef::parse(
        row.id.to_string(),
        row.name.clone(),
        &row.kind,
        &row.conditions.to_string(),
        row.value_cents,
    )
}
