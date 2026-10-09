//! MCP `create_goal`: an agent can define what counts as a conversion.
//!
//! The goal is validated by the same function the settings screen's goal form
//! goes through ([`crate::routes::config_objects::check_goal`]), which compiles
//! it exactly as a report will, so a goal an agent creates is one the goals
//! report can always answer for. Writing needs the `manage:write` scope and a
//! member's role on the site.

use micaforge_core::{new_id, Error, Result};
use micaforge_store::models::goals;
use serde_json::{json, Value};

use super::mcp::{acting_user, authorize_site, require_write, site_argument, string_argument};
use crate::extract::AuthUser;
use crate::routes::config_objects::{check_goal, goal_json, GoalBody};
use crate::routes::orgs::{require_at_least, MEMBER};
use crate::routes::sites::effective_site_role;
use crate::state::AppState;

/// This module's tools.
pub fn tools() -> Vec<Value> {
    vec![json!({
        "name": "create_goal",
        "description": "Create a goal on a site: the event that counts as a conversion. kind is \
    path (a pageview whose path matches), event (a custom event), outbound (a click leaving the \
    site), file (a download) or revenue (any event carrying revenue). conditions narrows it with \
    the same filter array the read tools take, for example [{\"dimension\":\"pathname\",\"op\":\"is\",\
    \"value\":\"/thanks\"}] or [{\"dimension\":\"event_name\",\"op\":\"is\",\"value\":\"signup\"}]. \
    Create a goal only when the person asked for one. Needs the manage:write scope.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "site": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`." },
                "name": { "type": "string", "maxLength": 120, "description": "What the goal is called on the goals screen." },
                "kind": { "type": "string", "enum": ["path", "event", "outbound", "file", "revenue"] },
                "conditions": { "description": "A filter array, or a {logic, filters} group. Leave out only for a bare revenue goal." },
                "value_cents": { "type": "integer", "minimum": 0, "description": "What one conversion is worth, in minor units." }
            },
            "required": ["site", "name", "kind"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    })]
}

/// Run `name` if it is this module's.
pub async fn call(
    st: &AppState,
    auth: &AuthUser,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    match name {
        "create_goal" => Some(create(st, auth, args).await),
        _ => None,
    }
}

async fn create(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let site = authorize_site(st, auth, site_argument(args)?).await?;
    let role = effective_site_role(&st.pg, &site, acting_user(auth)).await?;
    require_at_least(&role, MEMBER)?;

    let body = GoalBody {
        name: string_argument(args, "name")
            .ok_or_else(|| Error::validation("name", "is required"))?
            .to_string(),
        kind: string_argument(args, "kind")
            .ok_or_else(|| Error::validation("kind", "is required"))?
            .to_string(),
        conditions: args.get("conditions").filter(|v| !v.is_null()).cloned(),
        value_cents: args.get("value_cents").and_then(Value::as_i64),
    };
    let (name, kind, conditions, value) = check_goal(&body)?;
    let goal = goals::create(&st.pg, new_id(), site.id, &name, kind, &conditions, value).await?;
    let mut answer = goal_json(&goal);
    answer["site"] = json!(site.site_id_u16());
    Ok(answer)
}
