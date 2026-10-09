//! `/api/ai/*` — the AI analyst, for the dashboard.
//!
//! The analyst itself lives in `micaforge-ai`; this module is the front door
//! the dashboard uses, plus [`answer`], the one function the MCP `ask` tool and
//! the chat apps call too. Every surface therefore shares four rules:
//!
//! 1. **Off means off.** With no model configured nothing leaves the server,
//!    and the answer says so in words ([`not_configured`]) rather than failing
//!    in a way that looks like an outage.
//! 2. **One site, already authorised.** The caller authorises the site before
//!    [`answer`] is reached; the analyst's tools cannot leave it.
//! 3. **One allowance.** Every question takes a unit from its organisation's
//!    daily limit (`micaforge_ai::limit`).
//! 4. **One deadline.** [`ASK_DEADLINE`] bounds a question whatever asked it.
//!
//! ## Streaming
//!
//! `POST /api/ai/ask` with `Accept: text/event-stream` answers with
//! Server-Sent Events as the work happens: the conversation, each tool as it
//! starts and ends, then the answer. A question takes many seconds, and a
//! screen that shows "reading the sources table…" is a screen a person waits
//! on rather than abandons. The work runs on a task of its own, so a reader who
//! closes the tab still finds the answer stored when they come back.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use micaforge_ai::{AnalystError, Answer, BaseQuery, Llm, Progress, Registry, ToolCtx, Turn};
use micaforge_core::{filters, Audience, Error, FilterGroup, Result};
use micaforge_store::models::ai::{self, Message};
use micaforge_store::models::sites::Site;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::AuthUser;
use crate::response::ok;
use crate::routes::orgs::VIEWER;
use crate::routes::sites::authorize_site_ref;
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The ceiling on one question, model calls and reads together.
///
/// Under the request budget ([`crate::middleware::AI_BUDGET`]), so a slow
/// question ends with an answer that says it was stopped.
pub const ASK_DEADLINE: Duration = Duration::from_secs(100);

/// Longest question, in characters.
const MAX_QUESTION: usize = 2_000;

/// Conversations listed at once.
const MAX_CONVERSATIONS: i64 = 50;

/// Every AI analyst route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/ai/status", get(status))
        .route("/api/ai/ask", axum::routing::post(ask))
        .route("/api/ai/conversations", get(list_conversations))
        .route(
            "/api/ai/conversations/{id}",
            get(get_conversation).delete(delete_conversation),
        )
}

// ---------------------------------------------------------------------------
// The shared front door
// ---------------------------------------------------------------------------

/// The sentence every surface says when no model is configured.
pub fn not_configured() -> String {
    "AI is not configured on this server. The operator sets MICAFORGE_LLM_BASE_URL and \
     MICAFORGE_LLM_MODEL (any OpenAI-compatible endpoint) to switch it on."
        .to_string()
}

/// The `503` the dashboard gets when no model is configured.
fn not_configured_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": { "code": "ai_not_configured", "message": not_configured() } })),
    )
        .into_response()
}

/// The tools' view of one question.
pub fn tool_ctx(st: &AppState, site: &Site, base: BaseQuery) -> ToolCtx {
    ToolCtx {
        ch: st.ch.clone(),
        pg: st.pg.clone(),
        http: st.http.clone(),
        cfg: st.cfg.clone(),
        site: site.clone(),
        base,
    }
}

/// Take one question from the site's organisation's daily allowance.
///
/// `Conflict` when no model is configured, `RateLimited` over the limit.
pub async fn take_slot(st: &AppState, site: &Site) -> Result<()> {
    let Some(cfg) = st.cfg.llm.as_ref() else {
        return Err(Error::Conflict(not_configured()));
    };
    micaforge_ai::limit::take(&st.kv, cfg, site.org_id).await
}

/// Answer one question about an authorised site, its allowance already taken.
///
/// `Err` with an `AnalystError` carries what was gathered before a failure.
async fn run_question(
    st: &AppState,
    site: &Site,
    base: BaseQuery,
    history: &[Turn],
    question: &str,
    progress: Option<&mpsc::UnboundedSender<Progress>>,
) -> std::result::Result<Answer, AnalystError> {
    let Some(llm) = Llm::from_config(&st.cfg) else {
        return Err(AnalystError {
            error: Error::Conflict(not_configured()),
            traces: Vec::new(),
        });
    };
    let ctx = tool_ctx(st, site, base);
    let registry = Registry::builtin();
    micaforge_ai::ask(
        &llm,
        &registry,
        &ctx,
        history,
        question,
        progress,
        ASK_DEADLINE,
    )
    .await
}

/// Answer one question about an authorised site, taking its allowance.
///
/// For the MCP `ask` tool and the chat apps. "Not configured" and "over the
/// daily limit" are errors here too; each surface says them in its own way.
pub async fn answer(
    st: &AppState,
    site: &Site,
    base: BaseQuery,
    question: &str,
) -> std::result::Result<Answer, AnalystError> {
    take_slot(st, site).await.map_err(|error| AnalystError {
        error,
        traces: Vec::new(),
    })?;
    run_question(st, site, base, &[], question, None).await
}

/// A failure, in a sentence safe to show and store.
pub fn failure_text(error: &Error) -> String {
    match error {
        Error::Upstream(message) => message.clone(),
        e if e.is_public() => e.to_string(),
        e => {
            tracing::error!(error = %e, "an AI answer failed");
            "the answer failed on our side; the failure has been logged".to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/ai/status`.
async fn status(State(st): State<AppState>, _auth: AuthUser) -> Json<Value> {
    let llm = st.cfg.llm.as_ref();
    ok(json!({
        "configured": llm.is_some(),
        "model": llm.map(|c| c.model.clone()),
        "daily_limit": llm.map(|c| c.daily_limit).filter(|l| *l > 0),
    }))
}

/// A question from the dashboard.
#[derive(Debug, Deserialize)]
struct AskBody {
    site: Value,
    question: String,
    #[serde(default)]
    conversation_id: Option<Uuid>,
    #[serde(default)]
    range: Option<String>,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    #[serde(default)]
    tz: Option<String>,
    #[serde(default)]
    filters: Option<Value>,
    #[serde(default)]
    audience: Option<String>,
}

/// The caller's view, from the request.
fn base_of(body: &AskBody) -> Result<BaseQuery> {
    let filters: Option<FilterGroup> = match body.filters.as_ref().filter(|v| !v.is_null()) {
        Some(raw) => {
            let text = serde_json::to_string(raw)?;
            let group = filters::parse_filters(&text)?;
            group.validate()?;
            Some(group).filter(|g| !g.is_empty())
        }
        None => None,
    };
    let audience = match body.audience.as_deref().filter(|a| !a.is_empty()) {
        Some(raw) => Some(Audience::parse(raw)?),
        None => None,
    };
    Ok(BaseQuery {
        range: body.range.clone().filter(|r| !r.is_empty()),
        start: body.start.clone().filter(|s| !s.is_empty()),
        end: body.end.clone().filter(|s| !s.is_empty()),
        tz: body.tz.clone().filter(|s| !s.is_empty()),
        filters,
        audience,
    })
}

/// A title from the first question.
fn title_of(question: &str) -> String {
    let one_line = question.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 80 {
        one_line
    } else {
        let mut cut: String = one_line.chars().take(79).collect();
        cut.push('…');
        cut
    }
}

/// `POST /api/ai/ask`.
async fn ask(
    State(st): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(body): Json<AskBody>,
) -> ApiResult<Response> {
    auth.require_session()?;
    let reference = match &body.site {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return Err(Error::validation("site", "must be a site id").into()),
    };
    let site = authorize_site_ref(&st, &reference, auth.user.id, VIEWER).await?;
    let question = body.question.trim().to_string();
    let length = question.chars().count();
    if length == 0 || length > MAX_QUESTION {
        return Err(Error::validation(
            "question",
            format!("must be 1 to {MAX_QUESTION} characters"),
        )
        .into());
    }
    let base = base_of(&body)?;
    if st.cfg.llm.is_none() {
        return Ok(not_configured_response());
    }
    // Before anything is stored, so a question over the limit leaves no trace
    // and is answered `429` with the wait.
    take_slot(&st, &site).await?;

    // The conversation, opened or continued, and its history for the model.
    let (conversation, history) = match body.conversation_id {
        Some(id) => {
            let conversation = ai::get_conversation(&st.pg, id, auth.user.id).await?;
            if conversation.site_id != site.id {
                return Err(Error::not_found("conversation").into());
            }
            let turns = ai::list_messages(&st.pg, id)
                .await?
                .into_iter()
                .filter(|m| m.error.is_none())
                .map(|m| Turn {
                    role: m.role,
                    content: m.content,
                })
                .collect::<Vec<_>>();
            (conversation, turns)
        }
        None => (
            ai::create_conversation(
                &st.pg,
                micaforge_core::new_id(),
                site.id,
                auth.user.id,
                &title_of(&question),
            )
            .await?,
            Vec::new(),
        ),
    };
    let asked = ai::add_message(
        &st.pg,
        micaforge_core::new_id(),
        conversation.id,
        "user",
        &question,
        &json!([]),
        None,
    )
    .await?;

    let streaming = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));

    if !streaming {
        let outcome = run_question(&st, &site, base, &history, &question, None).await;
        let stored = store_outcome(&st, conversation.id, outcome).await?;
        return Ok(ok(json!({
            "conversation_id": conversation.id,
            "message": message_json(&stored),
        }))
        .into_response());
    }

    let (tx, rx) = mpsc::unbounded_channel::<Event>();
    let _ = tx.send(sse(
        "conversation",
        &json!({
            "conversation_id": conversation.id,
            "user_message": message_json(&asked),
        }),
    ));

    let conversation_id = conversation.id;
    tokio::spawn(async move {
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<Progress>();
        let relay_tx = tx.clone();
        let relay = tokio::spawn(async move {
            while let Some(event) = progress_rx.recv().await {
                let sent = match event {
                    Progress::ToolStart {
                        index,
                        tool,
                        arguments,
                    } => relay_tx.send(sse(
                        "tool_start",
                        &json!({ "index": index, "tool": tool, "arguments": arguments }),
                    )),
                    Progress::ToolEnd { index, trace } => {
                        relay_tx.send(sse("tool_end", &json!({ "index": index, "trace": trace })))
                    }
                };
                // A reader who left is not a reason to stop the answer.
                drop(sent);
            }
        });
        let outcome = run_question(&st, &site, base, &history, &question, Some(&progress_tx)).await;
        drop(progress_tx);
        let _ = relay.await;

        let failed = outcome
            .as_ref()
            .err()
            .map(|e| (e.error.code(), failure_text(&e.error)));
        match store_outcome(&st, conversation_id, outcome).await {
            Ok(stored) => {
                if let Some((code, message)) = failed {
                    let _ = tx.send(sse("error", &json!({ "code": code, "message": message })));
                } else {
                    let _ = tx.send(sse("answer", &json!({ "message": message_json(&stored) })));
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "an AI answer could not be stored");
                let _ = tx.send(sse(
                    "error",
                    &json!({ "code": e.code(), "message": "the answer could not be stored" }),
                ));
            }
        }
    });

    let stream = UnboundedReceiverStream::new(rx).map(Ok::<Event, Infallible>);
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// One SSE event with a JSON payload.
fn sse(name: &str, data: &Value) -> Event {
    Event::default().event(name).data(data.to_string())
}

/// Store the answer, or the failure, as the assistant's message.
async fn store_outcome(
    st: &AppState,
    conversation_id: Uuid,
    outcome: std::result::Result<Answer, AnalystError>,
) -> Result<Message> {
    let (content, traces, error) = match outcome {
        Ok(answer) => (answer.text, answer.traces, None),
        Err(failed) => (
            String::new(),
            failed.traces,
            Some(failure_text(&failed.error)),
        ),
    };
    ai::add_message(
        &st.pg,
        micaforge_core::new_id(),
        conversation_id,
        "assistant",
        &content,
        &serde_json::to_value(&traces)?,
        error.as_deref(),
    )
    .await
}

/// `?site=` for the conversation list.
#[derive(Debug, Deserialize)]
struct SiteQuery {
    site: String,
}

/// `GET /api/ai/conversations?site=`.
async fn list_conversations(
    State(st): State<AppState>,
    auth: AuthUser,
    Query(q): Query<SiteQuery>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let site = authorize_site_ref(&st, &q.site, auth.user.id, VIEWER).await?;
    let rows = ai::list_conversations(&st.pg, auth.user.id, site.id, MAX_CONVERSATIONS).await?;
    Ok(ok(rows
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "title": c.title,
                "created_at": c.created_at,
                "updated_at": c.updated_at,
                "messages": c.messages,
            })
        })
        .collect::<Vec<_>>()))
}

/// `GET /api/ai/conversations/{id}`.
///
/// The site is checked again: a person who lost access to a site does not
/// keep reading its numbers through an old conversation.
async fn get_conversation(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let conversation = ai::get_conversation(&st.pg, id, auth.user.id).await?;
    let site = match crate::routes::sites::authorize_site(
        &st.pg,
        conversation.site_id,
        auth.user.id,
        VIEWER,
    )
    .await
    {
        Ok(site) => site,
        Err(Error::NotFound(_)) => return Err(Error::not_found("conversation").into()),
        Err(e) => return Err(e.into()),
    };
    let messages = ai::list_messages(&st.pg, id).await?;
    Ok(ok(json!({
        "id": conversation.id,
        "site": site.site_id_u16(),
        "title": conversation.title,
        "created_at": conversation.created_at,
        "updated_at": conversation.updated_at,
        "messages": messages.iter().map(message_json).collect::<Vec<_>>(),
    })))
}

/// `DELETE /api/ai/conversations/{id}`.
async fn delete_conversation(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    if !ai::delete_conversation(&st.pg, id, auth.user.id).await? {
        return Err(Error::not_found("conversation").into());
    }
    Ok(ok(json!({ "deleted": id })))
}

/// One message on the wire.
fn message_json(m: &Message) -> Value {
    json!({
        "id": m.id,
        "role": m.role,
        "content": m.content,
        "tool_calls": m.tool_calls,
        "error": m.error,
        "created_at": m.created_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_one_short_line() {
        assert_eq!(
            title_of("  Which pages\n lost readers? "),
            "Which pages lost readers?"
        );
        let long = title_of(&"word ".repeat(40));
        assert_eq!(long.chars().count(), 80);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn the_callers_view_is_read_and_checked() {
        let body: AskBody = serde_json::from_value(json!({
            "site": 3,
            "question": "q",
            "range": "30d",
            "filters": [{ "dimension": "country", "op": "is", "value": "PT" }],
            "audience": "all"
        }))
        .unwrap();
        let base = base_of(&body).unwrap();
        assert_eq!(base.range.as_deref(), Some("30d"));
        assert_eq!(base.audience, Some(Audience::All));
        assert!(base.filters.is_some());

        let bad: AskBody = serde_json::from_value(json!({
            "site": 3, "question": "q", "audience": "robots"
        }))
        .unwrap();
        assert!(base_of(&bad).is_err());
    }

    #[test]
    fn upstream_failures_are_shown_and_internal_ones_are_not() {
        assert_eq!(
            failure_text(&Error::Upstream("the model answered 401: bad key".into())),
            "the model answered 401: bad key"
        );
        assert!(!failure_text(&Error::Database("SELECT secret".into())).contains("SELECT"));
        assert!(failure_text(&Error::RateLimited { retry_after: 60 }).contains("60"));
    }
}
