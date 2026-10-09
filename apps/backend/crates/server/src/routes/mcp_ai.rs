//! MCP `ask`: the AI analyst, for an agent.
//!
//! An agent can already call every report itself, so why ask? Because a
//! question in words is answered with a few well-chosen reads and an answer
//! that states its numbers, which costs an agent's context far less than the
//! tables would. The answer carries the same tool calls and SQL the dashboard
//! shows, so an agent can check any number it is about to repeat.
//!
//! Bounded exactly like the dashboard: the site is authorised against the
//! credential's grant first, the organisation's daily allowance is taken, and
//! the analyst cannot read outside the site.

use micaforge_ai::BaseQuery;
use micaforge_core::{filters, Audience, Error, Result};
use serde_json::{json, Value};

use super::mcp::{authorize_site, site_argument, string_argument};
use crate::extract::AuthUser;
use crate::state::AppState;

/// This module's tools.
pub fn tools() -> Vec<Value> {
    vec![json!({
        "name": "ask",
        "description": "Ask Micaforge's AI analyst a question about one site in plain words, \
    for example \"which sources sent more visitors this week than last, and why?\". It chooses the \
    reports to read, reads them, and answers with the numbers it found; `tool_calls` lists every \
    read with its equivalent stats request and SQL. Use it for open questions that would otherwise \
    take several report calls. It counts against the organisation's daily AI allowance, and it \
    answers only when the server has a model configured.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "site": { "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`." },
                "question": { "type": "string", "maxLength": 2000, "description": "The question, in plain words." },
                "range": { "type": "string", "description": "The window to start from: today, yesterday, 24h, 7d, 30d, 90d, 12mo, mtd, ytd or all. Defaults to 7d." },
                "start": { "type": "string", "description": "ISO-8601 start, instead of `range`." },
                "end": { "type": "string", "description": "ISO-8601 end, with `start`." },
                "filters": { "type": "array", "description": "Conditions every read must keep, in the same shape the read tools take." },
                "audience": { "type": "string", "enum": ["human", "agent", "all"], "description": "The population to start from. Defaults to human." }
            },
            "required": ["site", "question"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false }
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
        "ask" => Some(ask(st, auth, args).await),
        _ => None,
    }
}

async fn ask(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    let site = authorize_site(st, auth, site_argument(args)?).await?;
    let question = string_argument(args, "question")
        .ok_or_else(|| Error::validation("question", "is required"))?;
    if question.chars().count() > 2_000 {
        return Err(Error::validation(
            "question",
            "must be at most 2000 characters",
        ));
    }
    let filters = match args.get("filters").filter(|v| !v.is_null()) {
        Some(raw) => {
            let group = filters::parse_filters(&raw.to_string())?;
            group.validate()?;
            Some(group).filter(|g| !g.is_empty())
        }
        None => None,
    };
    let audience = match string_argument(args, "audience") {
        Some(raw) => Some(Audience::parse(raw)?),
        None => None,
    };
    let start = string_argument(args, "start").map(str::to_string);
    let base = BaseQuery {
        range: string_argument(args, "range")
            .map(str::to_string)
            .or_else(|| start.is_none().then(|| "7d".to_string())),
        start,
        end: string_argument(args, "end").map(str::to_string),
        tz: None,
        filters,
        audience,
    };

    match crate::routes::ai::answer(st, &site, base, question).await {
        Ok(answer) => Ok(json!({
            "site": site.site_id_u16(),
            "answer": answer.text,
            "tool_calls": answer.traces,
        })),
        Err(failed) => Err(failed.error),
    }
}
