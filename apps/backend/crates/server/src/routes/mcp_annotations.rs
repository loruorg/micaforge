//! MCP `create_annotation`, `list_annotations` and `delete_annotation`.
//!
//! An agent that ships a change (a deploy, a migration, a new page) can write
//! it onto the site's chart, so the person reading next week's dip sees the
//! cause beside it. Writing needs the `manage:write` scope and the role of a
//! member, exactly as in the dashboard; the checks are the HTTP route's own
//! ([`crate::routes::annotations`]).

use micaforge_core::{Error, Result};
use micaforge_store::models::annotations as notes;
use serde_json::{json, Value};
use uuid::Uuid;

use super::mcp::{
    acting_user, authorize_site, envelope_from_args, require_write, site_argument, string_argument,
};
use crate::extract::AuthUser;
use crate::routes::annotations::{self, annotation_json, Input};
use crate::routes::orgs::{require_at_least, MEMBER};
use crate::routes::sites::effective_site_role;
use crate::state::AppState;

/// Most annotations one list returns.
const MAX_LISTED: i64 = 200;

/// This module's tools.
pub fn tools() -> Vec<Value> {
    let site = json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`." });
    vec![
        json!({
            "name": "create_annotation",
            "description": "Write a note on a site's traffic chart at a moment or over a range: a \
        release, a migration, a campaign, an outage. People see it beside the chart, and the AI analyst \
        reads it when it explains a change. Use it after you ship something that could move traffic. \
        Needs the manage:write scope.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "site": site,
                    "starts_at": { "type": "string", "description": "ISO-8601 timestamp, or a date (midnight in the site's timezone)." },
                    "ends_at": { "type": "string", "description": "The end of a range. Leave out for a moment." },
                    "text": { "type": "string", "minLength": 1, "maxLength": 280, "description": "The note." },
                    "color": { "type": "string", "enum": ["neutral", "green", "red"], "description": "neutral (default), green for something good, red for an incident." },
                    "category": { "type": "string", "maxLength": 40, "description": "A short label such as release, campaign or incident." }
                },
                "required": ["site", "starts_at", "text"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "list_annotations",
            "description": "The notes written on a site's chart that overlap a window, oldest first, \
        with who or what wrote them. Check them before explaining a change in traffic.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "site": site,
                    "range": { "type": "string", "description": "A named window, as the read tools take it. Defaults to 24h." },
                    "start": { "type": "string", "description": "ISO-8601 start, instead of `range`." },
                    "end": { "type": "string", "description": "ISO-8601 end, with `start`." }
                },
                "required": ["site"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "delete_annotation",
            "description": "Delete one note from a site's chart, by the id `create_annotation` or \
        `list_annotations` returned. Needs the manage:write scope. Delete only notes you wrote or the \
        person asked you to remove.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "The annotation's id." } },
                "required": ["id"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
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
        "create_annotation" => Some(create(st, auth, args).await),
        "list_annotations" => Some(list(st, auth, args).await),
        "delete_annotation" => Some(delete(st, auth, args).await),
        _ => None,
    }
}

async fn create(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let site = authorize_site(st, auth, site_argument(args)?).await?;
    let role = effective_site_role(&st.pg, &site, acting_user(auth)).await?;
    require_at_least(&role, MEMBER)?;
    let text = |key: &str| string_argument(args, key).map(str::to_string);
    let input = Input {
        starts_at: text("starts_at"),
        ends_at: text("ends_at").map(Some),
        text: text("text"),
        color: text("color"),
        category: text("category").map(Some),
    };
    let note = annotations::create(st, &site, auth, &input, "mcp").await?;
    Ok(annotation_json(&note, site.site_id_u16()))
}

async fn list(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    let site = authorize_site(st, auth, site_argument(args)?).await?;
    let env = envelope_from_args(&site, args)?;
    let rows = notes::list_in(&st.pg, site.id, env.range.start, env.range.end, MAX_LISTED).await?;
    Ok(Value::Array(
        rows.iter()
            .map(|a| annotation_json(a, site.site_id_u16()))
            .collect(),
    ))
}

async fn delete(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let id = string_argument(args, "id")
        .and_then(|raw| raw.parse::<Uuid>().ok())
        .ok_or_else(|| Error::validation("id", "must be an annotation id"))?;
    annotations::delete(st, auth, id).await?;
    Ok(json!({ "deleted": id }))
}
