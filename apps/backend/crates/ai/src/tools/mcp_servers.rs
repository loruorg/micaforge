//! `get_mcp_servers`: how the MCP servers this team runs are doing.
//!
//! The readings of the "MCP servers" screen in one call (the totals, the
//! busiest tools with their error rates and latency, the clients, and the
//! calls that failed most recently), because the questions a person asks
//! about a server they run come together: "is it healthy, what is slow, who
//! uses it, what broke".
//!
//! The rows are calls an MCP server measured in its own process, not visits,
//! so the caller's page filters and audience do not apply to them; the tool
//! says so in its answer instead of quietly ignoring them. The arguments a
//! server may have opted in to storing are never read here: they would go to
//! a third-party model, and they are the one column that can hold what a
//! user typed.

use async_trait::async_trait;
use micaforge_core::{Audience, Result};
use micaforge_query::mcp_q::{self, CallsView, McpFilter, METHODS};
use serde_json::{json, Map, Value as Json};

use super::{stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput, MAX_ROWS};

/// Failed calls listed beside the report.
const FAILING_LISTED: u32 = 10;

struct McpServers;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(McpServers)
}

#[async_trait]
impl Tool for McpServers {
    fn name(&self) -> &'static str {
        "get_mcp_servers"
    }

    fn description(&self) -> &'static str {
        "Analytics for the MCP (Model Context Protocol) servers this team runs and measures \
         with the server SDK: calls, failed calls, tool errors, error rate, p50 and p95 handler \
         time in ms, sessions, the busiest tools, resources and prompts with their error rates \
         and latency, the clients by name (Claude, Cursor and others) with their share, and the \
         most recent failed calls with their error class and JSON-RPC code. Use it for any \
         question about an MCP server's usage, speed or errors. Not about website visitors."
    }

    fn parameters(&self) -> Json {
        let mut props = Map::new();
        props.insert(
            "range".into(),
            json!({ "type": "string", "description": "A named window in the site's timezone: today, yesterday, 24h, 7d, 30d, 90d, 12mo, mtd, ytd or all. Leave it out to use the window the person is looking at." }),
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
            json!({ "type": "string", "description": "Only this MCP server, by its name." }),
        );
        props.insert(
            "method".into(),
            json!({ "type": "string", "enum": METHODS, "description": "Only tool calls, resource reads or prompt gets." }),
        );
        props.insert(
            "client".into(),
            json!({ "type": "string", "description": "Only this MCP client, by its name." }),
        );
        props.insert(
            "limit".into(),
            json!({ "type": "integer", "minimum": 1, "maximum": MAX_ROWS, "description": "Most tools and clients to return." }),
        );
        json!({ "type": "object", "properties": Json::Object(props), "required": [] })
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let mut env = ctx.envelope(args)?;
        // Neither applies to a measured call; cleared so the request this tool
        // reports is the one that was really run.
        let page_filters = env.filters.take().is_some();
        env.audience = Audience::Human;

        let filter = McpFilter {
            server: string_arg(args, "server"),
            method: string_arg(args, "method"),
            client: string_arg(args, "client"),
        }
        .normalised()?;

        let overview = mcp_q::overview(&ctx.ch, &env, &filter).await?;
        let tools = mcp_q::tools(&ctx.ch, &env, &filter).await?;
        let clients = mcp_q::clients(&ctx.ch, &env, &filter).await?;
        let mut failing_env = env.clone();
        failing_env.limit = FAILING_LISTED;
        let failing = mcp_q::calls(&ctx.ch, &failing_env, &filter, CallsView::Failing).await?;

        let mut extra: Vec<(&str, String)> = Vec::new();
        for (key, value) in [
            ("server", &filter.server),
            ("method", &filter.method),
            ("client", &filter.client),
        ] {
            if let Some(value) = value {
                extra.push((key, value.clone()));
            }
        }
        let mut failing_extra = extra.clone();
        failing_extra.push(("view", "failing".to_string()));
        let requests = [
            stats_request("/api/stats/mcp/overview", &env, &extra),
            stats_request("/api/stats/mcp/tools", &env, &extra),
            stats_request("/api/stats/mcp/clients", &env, &extra),
            stats_request("/api/stats/mcp/calls", &failing_env, &failing_extra),
        ];

        let mut data = json!({
            "overview": to_data(&overview)?,
            "tools": to_data(&tools)?,
            "clients": to_data(&clients)?,
            "recent_failures": to_data(&failing)?,
        });
        if overview.calls == 0 {
            data["note"] = json!(
                "No MCP server calls were recorded in this window. Calls appear once an MCP \
                 server is wrapped with instrumentMcpServer from @micaforge/sdk-server."
            );
        } else if page_filters {
            data["note"] = json!(
                "The page filters the person applied describe visits, not MCP calls, so they \
                 were not applied here."
            );
        }
        Ok(ToolOutput {
            rows: Some(tools.len()),
            data,
            request: requests.join("\n"),
        })
    }
}
