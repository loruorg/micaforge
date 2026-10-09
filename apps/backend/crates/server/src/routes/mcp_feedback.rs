//! MCP `submit_feedback` and `list_my_feedback`: agents report problems too.
//!
//! An agent working through this server is often the first to find that a
//! tool refuses a reasonable argument, an answer looks wrong, or a report the
//! person needs does not exist. These two tools let it say so in a place the
//! operator reads, with its own name attached (from the MCP session) and the
//! key or OAuth grant it used, and read back what happened to its reports.
//!
//! The descriptions carry the instruction an agent needs most: feedback is
//! about Micaforge itself, never about the person's website.

use micaforge_core::{Error, Result};
use serde_json::{json, Value};

use super::mcp::string_argument;
use crate::extract::AuthUser;
use crate::routes::feedback::{self, Input, Reporter};
use crate::state::AppState;

/// This module's tools.
pub fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "submit_feedback",
            "description": "Report a problem with Micaforge itself, or ask for something it lacks, \
        to the people who run this Micaforge server. Use it when a Micaforge tool fails or refuses an \
        argument you believe is valid, when a number looks wrong or inconsistent between tools, when an \
        answer is confusing, or when the person needs a report or capability that does not exist. Write \
        what you did, what you expected and what happened, with the tool names and arguments. Do not use \
        it for questions or findings about the person's own website; answer those yourself. Ask the \
        person before you send feedback on their behalf.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["bug", "problem", "feature", "other"], "description": "bug = something is broken; problem = it works but is confusing or wrong for the task; feature = something is missing; other." },
                    "title": { "type": "string", "minLength": 3, "maxLength": 140, "description": "One line." },
                    "body": { "type": "string", "minLength": 1, "maxLength": 5000, "description": "What you did, what you expected, what happened. Include tool names and arguments." },
                    "severity": { "type": "string", "enum": ["low", "medium", "high", "critical"], "description": "How much it gets in the way. Leave out when unsure." },
                    "site": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "The site it concerns, when it concerns one." }
                },
                "required": ["kind", "title", "body"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "list_my_feedback",
            "description": "List the feedback this credential has sent with `submit_feedback` (or, \
        for an OAuth connection, that its person has sent), newest first, with each report's triage \
        status: new, triaged, planned, done or wont_fix. Use it before reporting something again, or to \
        tell the person what became of an earlier report.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
    ]
}

/// Run `name` if it is this module's.
pub async fn call(
    st: &AppState,
    auth: &AuthUser,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    match name {
        "submit_feedback" => Some(submit(st, auth, args).await),
        "list_my_feedback" => Some(mine(st, auth).await),
        _ => None,
    }
}

async fn submit(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    let required = |key: &str| {
        string_argument(args, key)
            .map(str::to_string)
            .ok_or_else(|| Error::validation(key, "is required"))
    };
    let input = Input {
        kind: required("kind")?,
        title: required("title")?,
        body: required("body")?,
        severity: string_argument(args, "severity").map(str::to_string),
        site: args.get("site").cloned(),
        page_url: None,
        app_version: None,
    };
    let site = feedback::site_of(st, auth, input.site.as_ref()).await?;
    let reporter = Reporter::from_auth(auth, true, super::mcp::client_name());
    let stored = feedback::submit(st, &reporter, &input, site.as_ref(), None).await?;
    Ok(json!({
        "id": stored.id,
        "status": stored.status,
        "created_at": stored.created_at,
        "message": "Sent. The people who run this Micaforge server will triage it; check its status with list_my_feedback.",
    }))
}

async fn mine(st: &AppState, auth: &AuthUser) -> Result<Value> {
    let rows = feedback::list_mine(st, auth).await?;
    Ok(Value::Array(
        rows.iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "kind": f.kind,
                    "title": f.title,
                    "status": f.status,
                    "severity": f.severity,
                    "created_at": f.created_at,
                    "updated_at": f.updated_at,
                })
            })
            .collect(),
    ))
}
