//! `get_funnel`: the site's funnels, and one funnel's steps and drop-off.
//!
//! The step translation follows the server's funnel endpoint
//! (`routes::behaviour::step_def`) rule for rule: a `page` step is a path
//! condition, an `event` step an event-name condition, a `goal` step that
//! goal's own conditions, and a `{label, conditions}` step is read as written.
//! The counts come from the same `stats::funnel` report.

use async_trait::async_trait;
use micaforge_core::{Error, Filter, FilterGroup, FilterNode, Result};
use micaforge_query::{stats, FunnelStepDef};
use micaforge_store::models::{funnels, goals};
use serde::Deserialize;
use serde_json::{json, Value as Json};
use uuid::Uuid;

use super::{schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput};

struct FunnelTool;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(FunnelTool)
}

/// One stored step, in either shape `funnels.steps` carries.
#[derive(Debug, Deserialize)]
struct StepSpec {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    conditions: Option<Json>,
}

#[async_trait]
impl Tool for FunnelTool {
    fn name(&self) -> &'static str {
        "get_funnel"
    }

    fn description(&self) -> &'static str {
        "Funnels configured on this site. Without `funnel` it lists them (name, id, steps). \
         With `funnel` (a name or id) it returns each step's visitors, the conversion from \
         the previous step and the drop-off, in the window. Use it for questions about where \
         people stop on the way to signing up or buying."
    }

    fn parameters(&self) -> Json {
        schema(
            &[(
                "funnel",
                json!({ "type": "string", "description": "A funnel's name or id. Leave out to list the funnels." }),
            )],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let configured = funnels::list(&ctx.pg, ctx.site.id).await?;

        let Some(reference) = string_arg(args, "funnel") else {
            let list: Vec<Json> = configured
                .iter()
                .map(|f| json!({ "id": f.id, "name": f.name, "steps": f.steps, "window_hours": f.window_hours }))
                .collect();
            return Ok(ToolOutput {
                rows: Some(list.len()),
                data: json!({ "funnels": list }),
                request: format!("GET /api/funnels?site={}", env.site),
            });
        };

        let wanted = reference.trim().to_lowercase();
        let row = configured
            .iter()
            .find(|f| f.id.to_string() == wanted || f.name.trim().to_lowercase() == wanted)
            .ok_or_else(|| {
                Error::validation("funnel", format!("no funnel named `{reference}` on this site"))
            })?;

        let specs: Vec<StepSpec> = serde_json::from_value(row.steps.clone()).map_err(|e| {
            Error::validation("funnel", format!("this funnel's steps are unreadable: {e}"))
        })?;
        let mut defs = Vec::with_capacity(specs.len());
        for (index, spec) in specs.iter().enumerate() {
            defs.push(step_def(ctx, index, spec).await?);
        }
        let window = u32::try_from(row.window_hours.max(1)).unwrap_or(24);

        let report = stats::funnel(&ctx.ch, &env, &defs, window).await?;
        Ok(ToolOutput {
            rows: Some(defs.len()),
            data: json!({ "funnel": row.name, "window_hours": window, "report": to_data(&report)? }),
            request: stats_request(&format!("/api/stats/funnel/{}", row.id), &env, &[]),
        })
    }
}

/// Translate one stored step, as the funnel endpoint does.
async fn step_def(ctx: &ToolCtx, index: usize, spec: &StepSpec) -> Result<FunnelStepDef> {
    let label = spec
        .label
        .clone()
        .filter(|l| !l.trim().is_empty())
        .unwrap_or_else(|| format!("Step {}", index + 1));

    if let Some(conditions) = &spec.conditions {
        return FunnelStepDef::parse(label, &conditions.to_string());
    }

    let value = spec.value.as_deref().unwrap_or("").trim();
    if value.is_empty() {
        return Err(Error::validation("funnel", format!("step {} matches nothing", index + 1)));
    }
    let dimension = match spec.kind.as_deref().unwrap_or("page") {
        "page" => "pathname",
        "event" => "event_name",
        "goal" => {
            let id = Uuid::parse_str(value)
                .map_err(|_| Error::validation("funnel", "a goal step names a goal by its id"))?;
            let goal = goals::get(&ctx.pg, id).await?;
            if goal.site_id != ctx.site.id {
                return Err(Error::not_found("goal"));
            }
            return FunnelStepDef::parse(label, &goal.conditions.to_string());
        }
        other => {
            return Err(Error::validation(
                "funnel",
                format!("unknown step kind `{other}`"),
            ))
        }
    };

    let filter: Filter = serde_json::from_value(json!({
        "dimension": dimension,
        "op": spec.op.as_deref().unwrap_or("is"),
        "value": value,
    }))
    .map_err(|e| Error::validation("funnel", e.to_string()))?;
    filter.validate()?;
    Ok(FunnelStepDef {
        label,
        conditions: FilterGroup::and(vec![FilterNode::Leaf(filter)]),
    })
}
