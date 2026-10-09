//! MCP tools for MCP server analytics: an agent asking about the MCP servers
//! its own team runs.
//!
//! The same rule as every other tool in [`super::mcp`]: a second front door
//! onto the reports the "MCP servers" screen reads, answered by the same
//! functions in [`micaforge_query::mcp_q`], never a second implementation.
//! Both tools are reads scoped to one site, and [`super::mcp`] authorises the
//! site before [`call`] is asked (the site-scoped pattern, dispatched from
//! `run_tool`).
//!
//! The schema is this module's own rather than the shared envelope's, because
//! the shared one offers `filters` and `audience`, which mean nothing for a
//! call an MCP server measured. A tool should not accept an argument it would
//! quietly ignore.

use micaforge_core::{Error, Result};
use micaforge_query::mcp_q::{self, CallsView, McpFilter, METHODS};
use micaforge_store::models::sites::Site;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::mcp::envelope_from_args;
use crate::state::AppState;

/// Most tools and clients the stats tool lists. A context window, not an export.
const MAX_LISTED: u32 = 25;

/// The tool descriptors, appended to the list `tools/list` answers.
pub fn tools() -> Vec<Value> {
    let tool = |name: &str, description: &str, extra: &[(&str, Value)]| {
        let mut props = Map::new();
        props.insert(
            "site".into(),
            json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`. Required." }),
        );
        props.insert(
            "range".into(),
            json!({ "type": "string", "enum": ["today", "yesterday", "24h", "7d", "30d", "90d", "12mo", "mtd", "ytd", "all"], "description": "The window, in the site's timezone. Defaults to 7d." }),
        );
        props.insert(
            "start".into(),
            json!({ "type": "string", "description": "ISO-8601 start, instead of `range`." }),
        );
        props.insert(
            "end".into(),
            json!({ "type": "string", "description": "ISO-8601 end, with `start`." }),
        );
        props.insert(
            "server".into(),
            json!({ "type": "string", "description": "Only this MCP server, by its serverInfo name." }),
        );
        props.insert(
            "method".into(),
            json!({ "type": "string", "enum": METHODS, "description": "Only this JSON-RPC method." }),
        );
        props.insert(
            "client".into(),
            json!({ "type": "string", "description": "Only this MCP client, by its clientInfo name." }),
        );
        for (key, schema) in extra {
            props.insert((*key).to_string(), schema.clone());
        }
        json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": Value::Object(props),
                "required": ["site"],
                "additionalProperties": false
            }
        })
    };

    vec![
        tool(
            "get_mcp_server_stats",
            "Analytics for the MCP servers this site's team runs (instrumented with \
@micaforge/sdk-server): calls, failed calls, tool errors (isError results), error rate, p50 and \
p95 handler time in ms, sessions and the servers seen, plus the busiest tools, resources and \
prompts with their error rates and latency, and the clients (clientInfo names such as Claude or \
Cursor) with their share of calls. Empty when no server is instrumented.",
            &[],
        ),
        tool(
            "list_mcp_calls",
            "Single MCP server calls: the slowest (`view: \"slowest\"`, the default) or the \
failed ones and tool errors, newest first (`view: \"failing\"`). Each has the time, server, \
method, tool or resource, client and version, transport, duration in ms, status, error class \
and JSON-RPC code, and the argument and result sizes. Never argument values or messages.",
            &[
                (
                    "view",
                    json!({ "type": "string", "enum": ["slowest", "failing"], "description": "Defaults to slowest." }),
                ),
                (
                    "limit",
                    json!({ "type": "integer", "minimum": 1, "maximum": 100, "description": "Rows to return. Defaults to 20." }),
                ),
            ],
        ),
    ]
}

/// Run one of this module's tools, or `None` for a name it does not own.
///
/// `site` has already been authorised for the calling key's user by
/// [`super::mcp`]; both tools read that site and nothing else.
pub async fn call(
    st: &AppState,
    _user: Uuid,
    site: &Site,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    let outcome = match name {
        "get_mcp_server_stats" => stats(st, site, args).await,
        "list_mcp_calls" => calls(st, site, args).await,
        _ => return None,
    };
    Some(outcome)
}

/// The filter from the tool's arguments.
fn filter_of(args: &Value) -> Result<McpFilter> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::to_string);
    McpFilter {
        server: text("server"),
        method: text("method"),
        client: text("client"),
    }
    .normalised()
}

/// The envelope, defaulting the window to the last seven days when the
/// caller named none: "how is my server doing" means recently.
fn envelope_of(site: &Site, args: &Value) -> Result<micaforge_core::QueryEnvelope> {
    for refused in ["filters", "audience"] {
        if args.get(refused).is_some_and(|v| !v.is_null()) {
            return Err(Error::validation(
                refused,
                "does not apply to MCP server calls; use `server`, `method` or `client`",
            ));
        }
    }
    let mut args = args.clone();
    if let Some(map) = args.as_object_mut() {
        if !map.contains_key("range") && !map.contains_key("start") {
            map.insert("range".into(), json!("7d"));
        }
    }
    envelope_from_args(site, &args)
}

/// `get_mcp_server_stats`.
async fn stats(st: &AppState, site: &Site, args: &Value) -> Result<Value> {
    let mut env = envelope_of(site, args)?;
    let filter = filter_of(args)?;
    env.limit = MAX_LISTED;
    let overview = mcp_q::overview(&st.ch, &env, &filter).await?;
    let tools = mcp_q::tools(&st.ch, &env, &filter).await?;
    let clients = mcp_q::clients(&st.ch, &env, &filter).await?;
    Ok(json!({
        "window": { "start": env.range.start, "end": env.range.end },
        "overview": overview,
        "tools": tools,
        "clients": clients,
    }))
}

/// `list_mcp_calls`.
async fn calls(st: &AppState, site: &Site, args: &Value) -> Result<Value> {
    let mut env = envelope_of(site, args)?;
    let filter = filter_of(args)?;
    env.limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as u32;
    let view = CallsView::parse(args.get("view").and_then(Value::as_str))?;
    let rows = mcp_q::calls(&st.ch, &env, &filter, view).await?;
    Ok(json!({ "view": view.as_str(), "calls": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tools_require_a_site_and_take_only_what_applies() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["get_mcp_server_stats", "list_mcp_calls"]);
        for tool in &tools {
            assert!(tool["description"].as_str().is_some_and(|d| d.len() > 40));
            assert_eq!(tool["inputSchema"]["required"], json!(["site"]));
            let props = tool["inputSchema"]["properties"].as_object().unwrap();
            assert!(!props.contains_key("filters") && !props.contains_key("audience"));
        }
    }

    #[test]
    fn the_filter_refuses_a_method_it_never_measures() {
        assert!(filter_of(&json!({ "method": "tools/list" })).is_err());
        let ok = filter_of(&json!({ "server": " docs ", "method": "tools/call" })).unwrap();
        assert_eq!(ok.server.as_deref(), Some("docs"));
    }
}
