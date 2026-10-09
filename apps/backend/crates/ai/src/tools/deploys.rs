//! `list_deploys`: what shipped, from the site's linked GitHub repository.
//!
//! The question this answers is the one people ask after every ship: "did
//! traffic change after the deploy on Tuesday?". A model that knows when the
//! deploy happened can compare the window before it with the window after it
//! using the other tools, instead of guessing at the cause of a step in a
//! chart. The rows come from what the GitHub App recorded (successful
//! deployments, published releases, pushes to the default branch), whether
//! or not each one was drawn on the chart as an annotation.
//!
//! Commit authors are kept as GitHub logins, never addresses, so nothing here
//! identifies a visitor and nothing personal goes to the model.

use async_trait::async_trait;
use micaforge_core::{Error, Result};
use micaforge_store::models::github;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, Tool, ToolCtx, ToolOutput, MAX_ROWS};

/// The kinds of activity the App records.
const KINDS: &[&str] = &["deploy", "release", "push"];

struct Deploys;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(Deploys)
}

/// The `kinds` argument: a list from [`KINDS`], all of them when absent or empty.
fn kinds(args: &Json) -> Result<Vec<String>> {
    let all = || KINDS.iter().map(|k| (*k).to_string()).collect::<Vec<_>>();
    let Some(raw) = args.get("kinds").filter(|v| !v.is_null()) else {
        return Ok(all());
    };
    let items = raw
        .as_array()
        .ok_or_else(|| Error::validation("kinds", "must be a list of deploy, release, push"))?;
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let kind = item
            .as_str()
            .map(str::trim)
            .filter(|k| KINDS.contains(k))
            .ok_or_else(|| Error::validation("kinds", "each kind is deploy, release or push"))?;
        if !out.iter().any(|k| k == kind) {
            out.push(kind.to_string());
        }
    }
    Ok(if out.is_empty() { all() } else { out })
}

#[async_trait]
impl Tool for Deploys {
    fn name(&self) -> &'static str {
        "list_deploys"
    }

    fn description(&self) -> &'static str {
        "Deploys, releases and pushes to the default branch from the site's linked GitHub \
         repository in the window, newest first, each with its time, environment, commit, title \
         and GitHub link. Use it when a change in traffic could follow a release: find the deploy, \
         then compare the window before it with the window after it. Empty when the site has no \
         linked repository."
    }

    fn parameters(&self) -> Json {
        schema(
            &[(
                "kinds",
                json!({
                    "type": "array",
                    "items": { "type": "string", "enum": KINDS },
                    "description": "Which kinds to list: deploy, release, push. Defaults to all three."
                }),
            )],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let kinds = kinds(args)?;
        let link = github::link_for_site(&ctx.pg, ctx.site.id).await?;
        let rows = github::events_in(
            &ctx.pg,
            ctx.site.id,
            env.range.start,
            env.range.end,
            &kinds,
            i64::from(env.limit.min(MAX_ROWS)),
        )
        .await?;
        let events: Vec<Json> = rows
            .iter()
            .map(|e| {
                json!({
                    "kind": e.kind,
                    "at": e.occurred_at,
                    "title": e.title,
                    "environment": e.environment,
                    "ref": e.git_ref,
                    "sha": e.sha.as_deref().map(|s| s.chars().take(12).collect::<String>()),
                    "actor": e.actor,
                    "url": e.url,
                    "commits": e.commits.as_array().map_or(0, Vec::len),
                    "on_chart": e.annotation_id.is_some(),
                })
            })
            .collect();
        Ok(ToolOutput {
            rows: Some(events.len()),
            data: json!({
                "repository": link.as_ref().map(|l| l.repo_full_name.clone()),
                "events": events,
            }),
            request: stats_request("/api/github/events", &env, &[("kinds", kinds.join(","))]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_default_to_all_and_refuse_anything_else() {
        assert_eq!(kinds(&json!({})).unwrap(), KINDS);
        assert_eq!(kinds(&json!({ "kinds": [] })).unwrap(), KINDS);
        assert_eq!(
            kinds(&json!({ "kinds": ["deploy", "deploy", "push"] })).unwrap(),
            ["deploy", "push"]
        );
        assert!(kinds(&json!({ "kinds": ["merge"] })).is_err());
        assert!(kinds(&json!({ "kinds": "deploy" })).is_err());
    }

    #[test]
    fn the_tool_takes_the_window_and_the_kinds() {
        let t = tool();
        assert_eq!(t.name(), "list_deploys");
        let params = t.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["range"].is_object());
        assert!(params["properties"]["kinds"].is_object());
    }
}
