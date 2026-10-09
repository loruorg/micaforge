//! `get_agent_traffic`: what the AI crawlers fetched.
//!
//! The overview and one breakdown in a single call, because the question a
//! person asks about crawlers is almost always both: "how much, and who".

use async_trait::async_trait;
use micaforge_core::{Audience, Dimension, Error, Result};
use micaforge_query::agents_q;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput};

/// The columns the agent trace records, and so the only ones it can be ranked by.
const AGENT_DIMENSIONS: &[&str] = &[
    "agent_id",
    "operator",
    "purpose",
    "pathname",
    "status",
    "content_type",
    "country",
    "asn_org",
    "verified",
    "robots_allowed",
];

struct AgentTraffic;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(AgentTraffic)
}

#[async_trait]
impl Tool for AgentTraffic {
    fn name(&self) -> &'static str {
        "get_agent_traffic"
    }

    fn description(&self) -> &'static str {
        "AI crawler and bot traffic (agents, not people): total fetches, distinct agents, the \
         share verified against the operator's published addresses, bytes served, the busiest \
         operators and purposes, plus a ranked breakdown by `dimension` (agent_id by default; \
         also operator, purpose, pathname, status, content_type, country, asn_org, verified, \
         robots_allowed). Use it for questions about GPTBot, ClaudeBot, PerplexityBot and \
         other crawlers."
    }

    fn parameters(&self) -> Json {
        schema(
            &[(
                "dimension",
                json!({ "type": "string", "enum": AGENT_DIMENSIONS, "description": "What to rank the fetches by. Defaults to agent_id." }),
            )],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let mut env = ctx.envelope(args)?;
        // Every read here is the agent trace by name; the audience is pinned so
        // the request it reports says so.
        env.audience = Audience::Agent;

        let raw = string_arg(args, "dimension").unwrap_or_else(|| "agent_id".to_string());
        if !AGENT_DIMENSIONS.contains(&raw.as_str()) {
            return Err(Error::validation(
                "dimension",
                format!("crawler traffic cannot be ranked by `{raw}`"),
            ));
        }
        let dimension = Dimension::parse(&raw)?;

        let overview = agents_q::overview(&ctx.ch, &env).await?;
        let rows = agents_q::breakdown(&ctx.ch, &env, &dimension).await?;
        let requests = [
            stats_request("/api/agents/overview", &env, &[]),
            stats_request(
                "/api/agents/breakdown",
                &env,
                &[("dimension", dimension.as_str())],
            ),
        ];
        Ok(ToolOutput {
            rows: Some(rows.len()),
            data: json!({
                "overview": to_data(&overview)?,
                "breakdown": to_data(&rows)?,
            }),
            request: requests.join("\n"),
        })
    }
}
