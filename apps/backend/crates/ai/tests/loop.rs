//! The tool-calling loop against a real HTTP server speaking the
//! chat-completions dialect.
//!
//! The server is an axum router on a loopback port, started inside each test.
//! It records every request body it receives and answers from a script, so a
//! test can assert both what the loop sent (the tools, the tool results, the
//! forced last turn) and what it did with the answers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use chrono::Utc;
use micaforge_ai::{ask, BaseQuery, Llm, Progress, Registry, Tool, ToolCtx, ToolOutput, Turn};
use micaforge_core::config::{ChatApps, FeedbackChannel, LlmConfig};
use micaforge_core::{Config, Error, Result};
use micaforge_query::exec;
use micaforge_store::models::sites::Site;
use micaforge_store::{Ch, PgPool};
use serde_json::{json, Value};
use uuid::Uuid;

/// What the fake model answers, call by call.
type Script = Arc<dyn Fn(usize, &Value) -> (StatusCode, Value) + Send + Sync>;

#[derive(Clone)]
struct Fake {
    seen: Arc<Mutex<Vec<Value>>>,
    script: Script,
}

async fn completions(
    State(fake): State<Fake>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let index = {
        let mut seen = fake.seen.lock().unwrap();
        seen.push(body.clone());
        seen.len() - 1
    };
    let (status, reply) = (fake.script)(index, &body);
    (status, Json(reply))
}

/// Start the fake model; returns its base URL and the request log.
async fn serve(script: Script) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/chat/completions", post(completions))
        .with_state(Fake {
            seen: Arc::clone(&seen),
            script,
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}/v1"), seen)
}

fn llm(base_url: &str) -> Llm {
    Llm::new(&LlmConfig {
        base_url: base_url.to_string(),
        model: "test-model".to_string(),
        api_key: Some("sk-test".to_string()),
        timeout_secs: 5,
        daily_limit: 0,
    })
}

fn assistant_text(text: &str) -> Value {
    json!({ "choices": [ { "message": { "role": "assistant", "content": text } } ] })
}

fn assistant_calls(calls: &[(&str, &str, Value)]) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .map(|(id, name, args)| {
            json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } })
        })
        .collect();
    json!({ "choices": [ { "message": { "role": "assistant", "content": null, "tool_calls": tool_calls } } ] })
}

/// A tool that answers from memory and runs no query.
struct Lookup;

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &'static str {
        "lookup"
    }
    fn description(&self) -> &'static str {
        "Returns a fixed table of page counts for the test, standing in for a report."
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": { "q": { "type": "string" } } })
    }
    async fn run(&self, ctx: &ToolCtx, args: &Value) -> Result<ToolOutput> {
        Ok(ToolOutput {
            data: json!({ "q": args["q"], "site": ctx.site_id(), "rows": [{ "page": "/docs", "visitors": 42 }] }),
            request: format!("GET /api/stats/pages?site={}", ctx.site_id()),
            rows: Some(1),
        })
    }
}

/// A tool that sends a real statement through the query engine to a store
/// that is not there, so the statement is recorded and the read fails.
struct DeadStore;

#[async_trait]
impl Tool for DeadStore {
    fn name(&self) -> &'static str {
        "dead_store"
    }
    fn description(&self) -> &'static str {
        "Reads ClickHouse at an address nothing listens on, to test recording and failure."
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn run(&self, ctx: &ToolCtx, _args: &Value) -> Result<ToolOutput> {
        #[derive(clickhouse::Row, serde::Deserialize)]
        struct One {
            #[allow(dead_code)]
            n: u64,
        }
        let rows: Vec<One> = exec::fetch_all(
            &ctx.ch,
            "test_report",
            "SELECT count() AS n FROM events WHERE site_id = ?",
            &[micaforge_query::Param::U16(ctx.site_id())],
        )
        .await?;
        Ok(ToolOutput {
            data: json!({ "n": rows.len() }),
            request: String::new(),
            rows: None,
        })
    }
}

fn config() -> Config {
    Config {
        bind_addr: "127.0.0.1:0".into(),
        database_url: "postgres://localhost/micaforge".into(),
        clickhouse_url: "http://127.0.0.1:9".into(),
        clickhouse_db: "micaforge".into(),
        clickhouse_user: "default".into(),
        clickhouse_password: String::new(),
        redis_url: "redis://127.0.0.1:6379".into(),
        public_url: "http://localhost:8080".into(),
        dashboard_url: "http://localhost:4321".into(),
        secret_key_base: "x".repeat(48),
        geoip_city_path: None,
        geoip_asn_path: None,
        retention_days: 0,
        replay_retention_days: 30,
        disable_registration: false,
        max_sites_per_org: 0,
        max_orgs_per_user: 0,
        self_hosted: true,
        smtp_url: None,
        cloudflare_email_token: None,
        cloudflare_account_id: None,
        mail_from: "Micaforge <noreply@localhost>".into(),
        log_format: "text".into(),
        worker_concurrency: 2,
        llm: None,
        chat: ChatApps::default(),
        feedback_channel: FeedbackChannel::default(),
        region: "default".into(),
        links_host: None,
        google_oauth: None,
        github_oauth: None,
        billing_enabled: false,
        billing_plans: Vec::new(),
        stripe: None,
        search_console: None,
        github_app: None,
    }
}

fn ctx() -> ToolCtx {
    // Neither store is reached by `Lookup`; the pool is lazy and connects to
    // nothing until a query runs, and `Ch::connect` only parses its URL.
    let pg = PgPool::connect_lazy("postgres://nobody@127.0.0.1:9/none").unwrap();
    let ch = Ch::connect("http://127.0.0.1:9", "micaforge", "default", "").unwrap();
    ToolCtx {
        ch,
        pg,
        http: reqwest::Client::new(),
        cfg: Arc::new(config()),
        site: Site {
            id: Uuid::from_u128(1),
            site_id: 7,
            org_id: Uuid::from_u128(2),
            domain: "docs.example.com".into(),
            name: "Example docs".into(),
            timezone: "Europe/Lisbon".into(),
            public: false,
            salt_rotated_at: None,
            created_at: Utc::now(),
            settings: json!({}),
        },
        base: BaseQuery {
            range: Some("7d".into()),
            ..BaseQuery::default()
        },
    }
}

#[tokio::test]
async fn a_tool_call_runs_and_its_result_reaches_the_model() {
    let script: Script = Arc::new(|index, _body| match index {
        0 => (
            StatusCode::OK,
            assistant_calls(&[
                ("call_a", "lookup", json!({ "q": "pages" })),
                ("call_b", "nope", json!({})),
            ]),
        ),
        _ => (StatusCode::OK, assistant_text("/docs had 42 visitors.")),
    });
    let (base, seen) = serve(script).await;
    let registry = Registry::of(vec![Box::new(Lookup)]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let history = vec![
        Turn {
            role: "user".into(),
            content: "How was last week?".into(),
        },
        Turn {
            role: "assistant".into(),
            content: "Steady.".into(),
        },
    ];
    let answer = ask(
        &llm(&base),
        &registry,
        &ctx(),
        &history,
        "Which page did best?",
        Some(&tx),
        Duration::from_secs(20),
    )
    .await
    .expect("the question is answered");

    assert_eq!(answer.text, "/docs had 42 visitors.");
    assert_eq!(answer.model_turns, 2);
    assert_eq!(answer.traces.len(), 2);
    assert!(answer.traces[0].ok);
    assert_eq!(answer.traces[0].tool, "lookup");
    assert_eq!(answer.traces[0].arguments["q"], "pages");
    assert_eq!(answer.traces[0].request, "GET /api/stats/pages?site=7");
    assert!(
        !answer.traces[1].ok,
        "an unknown tool is a failed call, not a failed answer"
    );
    assert!(answer.traces[1]
        .error
        .as_deref()
        .unwrap()
        .contains("lookup"));

    // Progress arrived in order: start, end, start, end.
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert_eq!(events.len(), 4);
    assert!(matches!(&events[0], Progress::ToolStart { index: 0, tool, .. } if tool == "lookup"));
    assert!(matches!(&events[3], Progress::ToolEnd { index: 1, .. }));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let first = &seen[0];
    assert_eq!(first["model"], "test-model");
    assert_eq!(first["tools"][0]["function"]["name"], "lookup");
    let roles: Vec<&str> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    assert!(first["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("docs.example.com"));

    // The second request carries the assistant's calls and both results.
    let second = seen[1]["messages"].as_array().unwrap();
    let tool_messages: Vec<&Value> = second.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(tool_messages.len(), 2);
    assert_eq!(tool_messages[0]["tool_call_id"], "call_a");
    assert!(tool_messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("\"visitors\":42"));
    assert_eq!(tool_messages[1]["tool_call_id"], "call_b");
    assert!(tool_messages[1]["content"]
        .as_str()
        .unwrap()
        .contains("no tool named"));
}

#[tokio::test]
async fn the_loop_is_bounded_and_the_last_turn_has_no_tools() {
    // A model that asks for a tool on every turn it is allowed to.
    let script: Script = Arc::new(|_index, body| {
        if body.get("tools").is_some() {
            (
                StatusCode::OK,
                assistant_calls(&[("c", "lookup", json!({}))]),
            )
        } else {
            (StatusCode::OK, assistant_text("Here is what I found."))
        }
    });
    let (base, seen) = serve(script).await;
    let registry = Registry::of(vec![Box::new(Lookup)]);

    let answer = ask(
        &llm(&base),
        &registry,
        &ctx(),
        &[],
        "Keep looking",
        None,
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    assert_eq!(answer.model_turns, micaforge_ai::analyst::MAX_TURNS);
    assert_eq!(answer.traces.len(), micaforge_ai::analyst::MAX_TURNS - 1);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), micaforge_ai::analyst::MAX_TURNS);
    let last = seen.last().unwrap();
    assert!(
        last.get("tools").is_none(),
        "the last turn forces an answer"
    );
    let closing = last["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(closing["role"], "user");
    assert!(closing["content"]
        .as_str()
        .unwrap()
        .contains("without calling more tools"));
}

#[tokio::test]
async fn a_recorded_query_survives_a_failed_read() {
    let script: Script = Arc::new(|index, _body| match index {
        0 => (
            StatusCode::OK,
            assistant_calls(&[("c", "dead_store", json!({}))]),
        ),
        _ => (StatusCode::OK, assistant_text("The store did not answer.")),
    });
    let (base, _) = serve(script).await;
    let registry = Registry::of(vec![Box::new(DeadStore)]);

    let answer = ask(
        &llm(&base),
        &registry,
        &ctx(),
        &[],
        "Count",
        None,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let trace = &answer.traces[0];
    assert!(!trace.ok);
    assert_eq!(
        trace.queries.len(),
        1,
        "the statement is recorded before it runs"
    );
    assert_eq!(trace.queries[0].report, "test_report");
    assert!(trace.queries[0].sql.contains("site_id = ?"));
    assert_eq!(trace.queries[0].params, vec![json!(7)]);
    // The store's own error text never reaches the trace or the model.
    assert_eq!(
        trace.error.as_deref(),
        Some("the analytics store could not answer this read")
    );
}

#[tokio::test]
async fn a_model_failure_is_an_upstream_error_with_its_message() {
    let script: Script = Arc::new(|_, _| {
        (
            StatusCode::UNAUTHORIZED,
            json!({ "error": { "message": "invalid api key" } }),
        )
    });
    let (base, _) = serve(script).await;
    let registry = Registry::of(vec![Box::new(Lookup)]);

    let failed = ask(
        &llm(&base),
        &registry,
        &ctx(),
        &[],
        "Hello",
        None,
        Duration::from_secs(20),
    )
    .await
    .unwrap_err();
    match failed.error {
        Error::Upstream(message) => {
            assert!(message.contains("401"));
            assert!(message.contains("invalid api key"));
            assert!(!message.contains("sk-test"), "the key is never echoed");
        }
        other => panic!("expected an upstream error, got {other:?}"),
    }
}

#[tokio::test]
async fn the_caller_filters_are_kept_and_personal_ones_refused() {
    let mut ctx = ctx();
    ctx.base.filters = Some(
        micaforge_core::filters::parse_filters(
            r#"[{"dimension":"country","op":"is","value":"PT"}]"#,
        )
        .unwrap(),
    );

    // The model adds a filter; the caller's survives beside it.
    let env = ctx
        .envelope(&json!({ "filters": [{ "dimension": "pathname", "op": "starts_with", "value": "/docs" }] }))
        .unwrap();
    let dims: Vec<String> = env
        .filters
        .unwrap()
        .dimensions()
        .iter()
        .map(|d| d.as_str())
        .collect();
    assert!(dims.contains(&"country".to_string()));
    assert!(dims.contains(&"pathname".to_string()));

    // Without arguments the caller's window and filters stand.
    let env = ctx.envelope(&json!({})).unwrap();
    assert_eq!(env.site, 7);
    assert!(env.filters.is_some());
    assert!(env.limit <= micaforge_ai::tools::MAX_ROWS);

    // A filter on a visitor id is refused before any query is built.
    let refused = ctx.envelope(
        &json!({ "filters": [{ "dimension": "visitor_id", "op": "is", "value": "abc" }] }),
    );
    assert!(refused.is_err());
}
