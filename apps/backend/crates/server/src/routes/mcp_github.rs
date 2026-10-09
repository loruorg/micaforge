//! MCP `list_deploys` and `create_github_issue_from_feedback`.
//!
//! Two tools, registered in the two ways [`super::mcp`] offers:
//!
//! - `list_deploys` is a site-scoped read: [`super::mcp`] authorises the site
//!   before [`call_site`] is asked, exactly as for the monitoring tools. It
//!   lists what the GitHub App recorded for the site in a window, so an agent
//!   asked "did traffic change after Tuesday's deploy?" can find the deploy
//!   before it reads the traffic.
//! - `create_github_issue_from_feedback` writes to GitHub, and carries its own
//!   rule ([`super::github::issues`]): the credential holds `manage:write`,
//!   it may read the report (it sent it), it is a member of the site, and the
//!   site's admins switched on "agents may open issues" for the linked
//!   repository. Dispatched from [`call`].

use micaforge_core::{Error, Result};
use micaforge_store::models::github as store;
use micaforge_store::models::sites::Site;
use serde_json::{json, Value};
use uuid::Uuid;

use super::github::{event_json, issues};
use super::mcp::{envelope_from_args, require_write, string_argument};
use crate::extract::AuthUser;
use crate::state::AppState;

/// The kinds of activity the App records.
const KINDS: &[&str] = &["deploy", "release", "push"];

/// Most events one call returns.
const MAX_EVENTS: i64 = 200;

/// This module's tools.
pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "list_deploys",
            "description": "The deploys, releases and pushes the GitHub App recorded for a site in a \
        window, newest first: what shipped, when, to which environment, from which commit, with its \
        GitHub link and whether it is on the chart as an annotation. Use it before explaining a change \
        in traffic: compare the window before a deploy with the window after it using the read tools. \
        Empty when the site has no linked repository.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "site": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`. Required." },
                    "range": { "type": "string", "description": "A named window: today, yesterday, 24h, 7d, 30d, 90d, 12mo, mtd, ytd or all." },
                    "start": { "type": "string", "description": "ISO-8601 start, instead of `range`." },
                    "end": { "type": "string", "description": "ISO-8601 end, with `start`." },
                    "kinds": {
                        "type": "array",
                        "items": { "type": "string", "enum": KINDS },
                        "description": "Which kinds to list. Defaults to all three."
                    },
                    "limit": { "type": "integer", "minimum": 1, "maximum": MAX_EVENTS, "description": "Most events to return. Defaults to 50." }
                },
                "required": ["site"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "create_github_issue_from_feedback",
            "description": "Open one of your own feedback reports (from `list_my_feedback`) as an issue \
        in the GitHub repository linked to the report's site, and return the issue's link. Calling it \
        again returns the same issue. Needs the manage:write scope, and the site's admins must have let \
        agents open issues in the site's GitHub settings; the refusal says which is missing. The issue \
        carries the report's text and plain facts (kind, severity, site, page path), never the reporter's \
        name or address. Ask the person before you open an issue for them: the repository may be public.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "feedback": { "type": "string", "description": "The report's id. Required." },
                    "site": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Only when the report names no site: the site whose repository should receive it." }
                },
                "required": ["feedback"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": true }
        }),
    ]
}

/// Run a tool that is not site-scoped, if it is this module's.
pub async fn call(
    st: &AppState,
    auth: &AuthUser,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    match name {
        "create_github_issue_from_feedback" => Some(create_issue(st, auth, args).await),
        _ => None,
    }
}

/// Run a site-scoped tool, if it is this module's. `site` is authorised.
pub async fn call_site(
    st: &AppState,
    site: &Site,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    match name {
        "list_deploys" => Some(list_deploys(st, site, args).await),
        _ => None,
    }
}

async fn create_issue(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let id = string_argument(args, "feedback")
        .and_then(|raw| raw.parse::<Uuid>().ok())
        .ok_or_else(|| {
            Error::validation(
                "feedback",
                "must be a report id; call `list_my_feedback` first",
            )
        })?;
    let site = match args.get("site") {
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::String(s)) => Some(s.trim().to_string()),
        _ => None,
    };
    let opened = issues::open_issue(st, auth, id, site.as_deref()).await?;
    Ok(opened.json())
}

/// The `kinds` argument: a list from [`KINDS`], all of them when absent.
fn kinds_argument(args: &Value) -> Result<Vec<String>> {
    let Some(raw) = args.get("kinds").filter(|v| !v.is_null()) else {
        return Ok(KINDS.iter().map(|k| (*k).to_string()).collect());
    };
    let items = raw
        .as_array()
        .ok_or_else(|| Error::validation("kinds", "must be a list of deploy, release, push"))?;
    let mut kinds = Vec::new();
    for item in items {
        let kind = item
            .as_str()
            .map(str::trim)
            .filter(|k| KINDS.contains(k))
            .ok_or_else(|| Error::validation("kinds", "each kind is deploy, release or push"))?;
        if !kinds.iter().any(|k: &String| k == kind) {
            kinds.push(kind.to_string());
        }
    }
    if kinds.is_empty() {
        return Ok(KINDS.iter().map(|k| (*k).to_string()).collect());
    }
    Ok(kinds)
}

async fn list_deploys(st: &AppState, site: &Site, args: &Value) -> Result<Value> {
    let env = envelope_from_args(site, args)?;
    let kinds = kinds_argument(args)?;
    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(50)
        .clamp(1, MAX_EVENTS);
    let link = store::link_for_site(&st.pg, site.id).await?;
    let rows = store::events_in(
        &st.pg,
        site.id,
        env.range.start,
        env.range.end,
        &kinds,
        limit,
    )
    .await?;
    Ok(json!({
        "site": site.site_id_u16(),
        "window": { "start": env.range.start, "end": env.range.end },
        "repository": link.as_ref().map(|l| l.repo_full_name.clone()),
        "events": rows.iter().map(event_json).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tools_describe_themselves_and_say_what_they_need() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["list_deploys", "create_github_issue_from_feedback"]);
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["site"]));
        assert_eq!(tools[1]["inputSchema"]["required"], json!(["feedback"]));
        assert!(tools[1]["description"]
            .as_str()
            .is_some_and(|d| d.contains("manage:write")));
    }

    #[test]
    fn kinds_default_to_all_and_refuse_strangers() {
        assert_eq!(kinds_argument(&json!({})).unwrap(), KINDS);
        assert_eq!(
            kinds_argument(&json!({ "kinds": ["release", "release"] })).unwrap(),
            ["release"]
        );
        assert_eq!(kinds_argument(&json!({ "kinds": [] })).unwrap(), KINDS);
        assert!(kinds_argument(&json!({ "kinds": ["merge"] })).is_err());
        assert!(kinds_argument(&json!({ "kinds": "deploy" })).is_err());
    }
}
