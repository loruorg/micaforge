//! The model client: OpenAI-compatible `chat/completions` with tool calling.
//!
//! The dialect is the one nearly every model server speaks, hosted or local,
//! so this is one small client rather than a provider SDK per vendor. Only the
//! fields the analyst needs are written and read; everything else a provider
//! adds to its answer is ignored.
//!
//! ## What never happens here
//!
//! - **No call without configuration.** [`Llm::from_config`] is the only
//!   constructor reachable from the server, and it returns `None` when the
//!   operator set no model.
//! - **No key in a log line.** The key is held in [`Llm`] and sent as a bearer
//!   header; `Debug` does not print it, and a failed call is reported by its
//!   status and the provider's own error message, never by the request.
//! - **No unbounded wait.** Every call carries the configured timeout.

use std::sync::OnceLock;
use std::time::Duration;

use micaforge_core::config::LlmConfig;
use micaforge_core::{Config, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

/// Largest answer body read from the model, in bytes.
///
/// A completion is a few kilobytes. A body far larger than that is a server
/// misbehaving, and reading it whole would let it fill memory.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// The configured model.
#[derive(Clone)]
pub struct Llm {
    /// Requests go to `{base_url}/chat/completions`.
    base_url: String,
    /// The model name sent with every request.
    model: String,
    /// The bearer token, when the server takes one.
    api_key: Option<String>,
    /// The ceiling on one call.
    timeout: Duration,
}

impl std::fmt::Debug for Llm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Llm")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// One message in a conversation with the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `system` | `user` | `assistant` | `tool`.
    pub role: String,
    /// The text. Always written, as an empty string when there is none: some
    /// servers refuse a `null` here even on an assistant turn that only calls
    /// tools.
    #[serde(default, deserialize_with = "null_as_empty")]
    pub content: String,
    /// The tools an assistant turn asked for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// On a `tool` message, the call it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    /// A message with only text.
    pub fn text(role: &str, content: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// A tool's answer to one call.
    pub fn tool_result(call_id: &str, content: impl Into<String>) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.to_string()),
        }
    }
}

/// `null` and absent both read as an empty string.
fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// One tool call the model asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call, echoed on the result.
    #[serde(default)]
    pub id: String,
    /// Always `function` in this dialect.
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    /// What to call.
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".to_string()
}

/// The function half of a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    /// The tool's name.
    pub name: String,
    /// The arguments, as a JSON document in a string, as the dialect has it.
    #[serde(default)]
    pub arguments: String,
}

/// A tool as the model is shown it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    /// The name the model calls it by.
    pub name: String,
    /// When to use it.
    pub description: String,
    /// JSON Schema of its arguments.
    pub parameters: Json,
}

/// What the model said on one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The assistant message, text and tool calls both.
    pub message: ChatMessage,
}

#[derive(Deserialize)]
struct CompletionBody {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChatMessage,
}

/// The process's one HTTP client for the model.
///
/// One pool for every question, rather than a client per request: a question
/// is several calls in a row to the same host, and they should share the
/// connection. The timeout is set per request, from the configuration.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("Micaforge/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

impl Llm {
    /// The model the operator configured, or `None` when AI is off.
    pub fn from_config(cfg: &Config) -> Option<Llm> {
        cfg.llm.as_ref().map(Llm::new)
    }

    /// Build from an explicit configuration.
    pub fn new(cfg: &LlmConfig) -> Llm {
        Llm {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            model: cfg.model.clone(),
            api_key: cfg.api_key.clone(),
            timeout: Duration::from_secs(cfg.timeout_secs.max(1)),
        }
    }

    /// The model name, for the status endpoint and the log.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// One turn: send the conversation, get the model's next message.
    ///
    /// `tools` empty sends no tools at all, which is how the analyst forces a
    /// final answer on its last turn.
    pub async fn chat(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Result<Reply> {
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": 0.2,
        });
        if !tools.is_empty() {
            body["tools"] = Json::Array(
                tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
            body["tool_choice"] = json!("auto");
        }

        let mut request = client()
            .post(format!("{}/chat/completions", self.base_url))
            .timeout(self.timeout)
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }

        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                Error::Upstream(format!(
                    "the model did not answer within {} seconds",
                    self.timeout.as_secs()
                ))
            } else {
                Error::Upstream(format!(
                    "the model could not be reached: {}",
                    without_url(&e)
                ))
            }
        })?;

        let status = response.status();
        let bytes = response.bytes().await.map_err(|e| {
            Error::Upstream(format!(
                "the model's answer was cut off: {}",
                without_url(&e)
            ))
        })?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Upstream(
                "the model's answer was too large".to_string(),
            ));
        }

        if !status.is_success() {
            return Err(Error::Upstream(format!(
                "the model answered {}: {}",
                status.as_u16(),
                provider_message(&bytes)
            )));
        }

        let parsed: CompletionBody = serde_json::from_slice(&bytes).map_err(|e| {
            Error::Upstream(format!("the model's answer was not a chat completion: {e}"))
        })?;
        let choice = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Error::Upstream("the model's answer had no choices".to_string()))?;
        Ok(Reply {
            message: choice.message,
        })
    }
}

/// A reqwest error without the URL it carries.
///
/// The base URL is configuration rather than a secret, but a self-hosted
/// endpoint's address is still nobody's business in an error a user may read.
fn without_url(e: &reqwest::Error) -> String {
    let mut owned = e.to_string();
    if let Some(url) = e.url() {
        owned = owned.replace(url.as_str(), "the model endpoint");
    }
    owned
}

/// The provider's own error sentence, when its body has one.
///
/// Both `{ "error": { "message": … } }` (OpenAI and most servers) and
/// `{ "error": "…" }` (some local servers) are read; anything else is
/// summarised by its length rather than echoed, because an error page can be
/// anything.
fn provider_message(body: &[u8]) -> String {
    let parsed: Option<Json> = serde_json::from_slice(body).ok();
    let message = parsed.as_ref().and_then(|v| {
        v.get("error")
            .and_then(|e| {
                e.get("message")
                    .and_then(Json::as_str)
                    .or_else(|| e.as_str())
            })
            .map(str::to_string)
    });
    match message {
        Some(text) => text.chars().take(300).collect(),
        None => format!("a body of {} bytes with no error message", body.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_only_turn_deserialises_with_null_content() {
        let raw = json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "get_overview", "arguments": "{\"range\":\"7d\"}" }
            }]
        });
        let message: ChatMessage = serde_json::from_value(raw).unwrap();
        assert_eq!(message.content, "");
        assert_eq!(message.tool_calls[0].function.name, "get_overview");

        // And it is written back with a string, never a null.
        let written = serde_json::to_value(&message).unwrap();
        assert_eq!(written["content"], "");
        assert!(written.get("tool_call_id").is_none());
    }

    #[test]
    fn provider_errors_are_read_in_both_common_shapes() {
        assert_eq!(
            provider_message(br#"{"error":{"message":"model not found"}}"#),
            "model not found"
        );
        assert_eq!(provider_message(br#"{"error":"bad key"}"#), "bad key");
        assert!(provider_message(b"<html>502</html>").contains("16 bytes"));
    }

    #[test]
    fn the_key_never_appears_in_debug() {
        let llm = Llm::new(&LlmConfig {
            base_url: "https://llm.example.com/v1/".into(),
            model: "m".into(),
            api_key: Some("sk-very-secret".into()),
            timeout_secs: 10,
            daily_limit: 0,
        });
        assert_eq!(llm.base_url, "https://llm.example.com/v1");
        assert!(!format!("{llm:?}").contains("sk-very-secret"));
    }
}
