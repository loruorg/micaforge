//! `get_breakdown`: ranked rows for any filter dimension.

use async_trait::async_trait;
use micaforge_core::{Dimension, Error, Metric, Result};
use micaforge_query::stats;
use serde_json::{json, Value as Json};

use super::{
    refuse_personal, schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput,
};

struct Breakdown;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Breakdown)
}

#[async_trait]
impl Tool for Breakdown {
    fn name(&self) -> &'static str {
        "get_breakdown"
    }

    fn description(&self) -> &'static str {
        "Ranked rows for one dimension with metrics and each row's share: the general tool \
         for \"which X drove the most Y\". dimension is one of pathname, entry_path, \
         exit_path, hostname, page_title, referrer_host, channel, source, utm_source, \
         utm_medium, utm_campaign, utm_term, utm_content, browser, os, device_type, \
         screen_class, language, country, region, city, asn_org, event_name, goal, \
         experiment, variant, agent_id, operator, purpose, verified, status, content_type, \
         or props.<key>. metrics defaults to visitors and pageviews. To find what moved, call \
         it for this window and for the previous one and compare the rows."
    }

    fn parameters(&self) -> Json {
        schema(
            &[
                (
                    "dimension",
                    json!({ "type": "string", "description": "The column to group by." }),
                ),
                (
                    "metrics",
                    json!({ "type": "array", "items": { "type": "string" }, "description": "Metrics per row, e.g. [\"visitors\", \"bounce_rate\"]." }),
                ),
            ],
            &["dimension"],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let raw = string_arg(args, "dimension")
            .ok_or_else(|| Error::validation("dimension", "is required"))?;
        let dimension = Dimension::parse(&raw)?;
        refuse_personal(&dimension)?;
        let metrics = metrics_arg(args)?;

        let rows = stats::breakdown(&ctx.ch, &env, &dimension, &metrics).await?;
        let names: Vec<&str> = metrics.iter().map(Metric::as_str).collect();
        Ok(ToolOutput {
            rows: Some(rows.len()),
            data: to_data(&rows)?,
            request: stats_request(
                "/api/stats/breakdown",
                &env,
                &[
                    ("dimension", dimension.as_str()),
                    ("metrics", names.join(",")),
                ],
            ),
        })
    }
}

/// The `metrics` list, defaulting to the two every breakdown table shows.
fn metrics_arg(args: &Json) -> Result<Vec<Metric>> {
    let mut metrics = Vec::new();
    if let Some(items) = args.get("metrics").and_then(Json::as_array) {
        for item in items {
            let name = item
                .as_str()
                .ok_or_else(|| Error::validation("metrics", "each entry must be a metric name"))?;
            metrics.push(Metric::parse(name)?);
        }
    }
    if metrics.is_empty() {
        metrics = vec![Metric::Visitors, Metric::Pageviews];
    }
    Ok(metrics)
}
