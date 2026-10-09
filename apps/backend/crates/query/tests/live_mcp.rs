//! The MCP server reports, against a live ClickHouse.
//!
//! The unit tests in `mcp_q` check the Rust around the SQL. This checks the
//! SQL itself: that ClickHouse accepts every statement (`quantileTiming`,
//! `topKIf(…)[1]`, `arrayFilter` over `topK`) and that the rows decode into
//! the structs. It builds the real schema in a throwaway database, writes a
//! handful of calls for two sites, reads them back through each report, and
//! drops the database.
//!
//! Opt in with
//! `MICAFORGE_TEST_CLICKHOUSE_URL=http://user:password@127.0.0.1:8123`
//! and `cargo test -p micaforge-query --test live_mcp -- --ignored`.

use chrono::{Duration, Utc};
use micaforge_core::{Granularity, QueryEnvelope, TimeRange};
use micaforge_query::mcp_q::{self, CallsView, McpFilter};
use micaforge_store::{Ch, McpCallRow};

const SITE: u16 = 9;

/// A successful tool call on `SITE`; the rows below change what differs.
fn call(minutes_ago: i64, name: &str, client: &str, session: &str, duration_ms: u32) -> McpCallRow {
    McpCallRow {
        site_id: SITE,
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
        server_name: "docs".into(),
        server_version: "1.2.0".into(),
        method: "tools/call".into(),
        name: name.into(),
        client_name: client.into(),
        client_version: "0.9.1".into(),
        protocol_version: "2025-06-18".into(),
        transport: "streamable-http".into(),
        session_hash: session.into(),
        duration_ms,
        status: "ok".into(),
        arg_bytes: 40,
        result_bytes: 900,
        sdk_version: "sdk-server/0.1.0".into(),
        ..McpCallRow::default()
    }
}

/// The same call, failed with `error_type`.
fn failed(row: McpCallRow, error_type: &str) -> McpCallRow {
    McpCallRow {
        status: "error".into(),
        error_type: error_type.into(),
        error_code: -32603,
        ..row
    }
}

#[tokio::test]
#[ignore = "needs a ClickHouse server"]
async fn the_mcp_reports_read_a_live_store() {
    let raw = std::env::var("MICAFORGE_TEST_CLICKHOUSE_URL")
        .expect("set MICAFORGE_TEST_CLICKHOUSE_URL to run this test");
    let url = url::Url::parse(&raw).expect("a URL");
    let base = format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().expect("a host"),
        url.port().unwrap_or(8123)
    );
    let db = format!("mf_live_mcp_{}", std::process::id());
    let ch = Ch::connect(&base, &db, url.username(), url.password().unwrap_or("")).unwrap();
    ch.init_schema().await.expect("the schema is created");

    let rows = vec![
        call(50, "search", "claude-ai", "aa", 20),
        call(40, "search", "claude-ai", "aa", 40),
        failed(call(30, "search", "cursor-vscode", "bb", 900), "McpError"),
        McpCallRow {
            status: "tool_error".into(),
            ..call(20, "fetch_page", "claude-ai", "cc", 60)
        },
        McpCallRow {
            method: "resources/read".into(),
            ..call(10, "file:///docs/a.md", "claude-ai", "cc", 5)
        },
        // Another site's call, which no report for SITE may count.
        McpCallRow {
            site_id: SITE + 1,
            ..failed(call(10, "search", "claude-ai", "dd", 5000), "TypeError")
        },
    ];
    let mut insert = ch.client().insert::<McpCallRow>("mcp_calls").unwrap();
    for row in &rows {
        insert.write(row).await.unwrap();
    }
    insert.end().await.unwrap();

    let now = Utc::now();
    let mut env = QueryEnvelope::new(
        SITE,
        TimeRange::new(
            now - Duration::hours(2),
            now + Duration::minutes(1),
            chrono_tz::UTC,
        ),
    );
    env.granularity = Granularity::Hour;
    let all = McpFilter::default();

    let overview = mcp_q::overview(&ch, &env, &all).await.unwrap();
    assert_eq!(overview.calls, 5, "the other site leaked in: {overview:?}");
    assert_eq!(overview.errors, 1);
    assert_eq!(overview.tool_errors, 1);
    assert_eq!(overview.error_rate, 0.4);
    assert_eq!(overview.sessions, 3);
    assert_eq!(overview.clients, 2);
    assert_eq!(overview.tools, 2);
    assert!(overview.p95_ms >= overview.p50_ms && overview.p50_ms > 0.0);
    assert_eq!(overview.servers.len(), 1);
    assert_eq!(overview.servers[0].version, "1.2.0");

    let series = mcp_q::timeseries(&ch, &env, &all).await.unwrap();
    assert_eq!(series.iter().map(|b| b.calls).sum::<u64>(), 5);
    assert_eq!(series.iter().map(|b| b.errors).sum::<u64>(), 2);
    assert!(series.iter().all(|b| b.t % 3_600_000 == 0), "{series:?}");

    let tools = mcp_q::tools(&ch, &env, &all).await.unwrap();
    assert_eq!(tools[0].name, "search");
    assert_eq!(tools[0].calls, 3);
    assert_eq!(tools[0].errors, 1);
    assert_eq!(tools[0].top_error, "McpError");
    assert_eq!(tools[0].clients, 2);
    let page = tools.iter().find(|t| t.name == "fetch_page").unwrap();
    assert_eq!((page.tool_errors, page.top_error.as_str()), (1, ""));

    let clients = mcp_q::clients(&ch, &env, &all).await.unwrap();
    assert_eq!(clients[0].client, "claude-ai");
    assert_eq!(clients[0].calls, 4);
    assert_eq!(clients[0].share, 0.8);
    assert_eq!(clients[0].versions, ["0.9.1"]);

    let slowest = mcp_q::calls(&ch, &env, &all, CallsView::Slowest)
        .await
        .unwrap();
    assert_eq!(slowest[0].duration_ms, 900);
    let failing = mcp_q::calls(&ch, &env, &all, CallsView::Failing)
        .await
        .unwrap();
    assert_eq!(failing.len(), 2);
    assert_eq!(failing[0].status, "tool_error", "newest first");

    let only_resources = McpFilter {
        method: Some("resources/read".into()),
        ..McpFilter::default()
    };
    let read = mcp_q::overview(&ch, &env, &only_resources).await.unwrap();
    assert_eq!((read.calls, read.tools), (1, 0));

    let nobody = McpFilter {
        server: Some("nobody".into()),
        ..McpFilter::default()
    };
    let empty = mcp_q::overview(&ch, &env, &nobody).await.unwrap();
    assert_eq!(empty.calls, 0);
    assert_eq!((empty.p50_ms, empty.p95_ms), (0.0, 0.0), "never NaN");
    assert!(mcp_q::tools(&ch, &env, &nobody).await.unwrap().is_empty());

    let _ = ch.exec(&format!("DROP DATABASE IF EXISTS {db}")).await;
}
