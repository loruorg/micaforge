//! The error, frustration, sampling and profile reports, against a live
//! ClickHouse.
//!
//! Their unit tests check the Rust around the SQL. This checks the SQL itself:
//! that ClickHouse accepts every statement (`topK`, `arrayJoin` pairs,
//! `LIMIT … BY`, `argMaxIf`, a `HAVING` over a JSON column's text) and that the
//! rows come back in the shape the structs read. It builds the real schema in a
//! throwaway database, writes a handful of rows, reads them back through each
//! report, and drops the database.
//!
//! Opt in with
//! `MICAFORGE_TEST_CLICKHOUSE_URL=http://user:password@127.0.0.1:8123`
//! and `cargo test -p micaforge-query --test live_reports -- --ignored`.

use chrono::{Duration, Utc};
use micaforge_core::{Audience, Compare, Granularity, QueryEnvelope, TimeRange};
use micaforge_query::stats;
use micaforge_store::ch::{ErrorRow, EventRow};
use micaforge_store::Ch;

const SITE: u16 = 7;

fn event(
    minutes_ago: i64,
    kind: &str,
    name: &str,
    path: &str,
    visitor: &str,
    session: &str,
) -> EventRow {
    EventRow {
        site_id: SITE,
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
        event_id: micaforge_core::new_id(),
        session_id: session.into(),
        visitor_id: visitor.into(),
        kind: kind.into(),
        event_name: name.into(),
        hostname: "example.com".into(),
        pathname: path.into(),
        ..EventRow::default()
    }
}

fn error(
    minutes_ago: i64,
    fingerprint: &str,
    browser: &str,
    release: &str,
    handled: u8,
) -> ErrorRow {
    ErrorRow {
        site_id: SITE,
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
        session_id: format!("s-{minutes_ago}"),
        visitor_id: "v1".into(),
        name: "TypeError".into(),
        message: format!("broken {fingerprint}"),
        stack: "TypeError: broken\n    at f (app.js:1:2)".into(),
        filename: "https://example.com/app.js".into(),
        lineno: 1,
        colno: 2,
        pathname: "/checkout".into(),
        browser: browser.into(),
        os: "macOS".into(),
        release: release.into(),
        handled,
        fingerprint: fingerprint.into(),
    }
}

async fn insert<T>(ch: &Ch, table: &str, rows: &[T])
where
    T: clickhouse::Row + serde::Serialize + Send + Sync + 'static,
{
    let mut insert = ch.client().insert::<T>(table).expect("an insert");
    for row in rows {
        insert.write(row).await.expect("a row is written");
    }
    insert.end().await.expect("the insert commits");
}

#[tokio::test]
#[ignore = "needs a ClickHouse server"]
async fn the_new_reports_read_a_live_store() {
    let raw = std::env::var("MICAFORGE_TEST_CLICKHOUSE_URL")
        .expect("set MICAFORGE_TEST_CLICKHOUSE_URL to run this test");
    let url = url::Url::parse(&raw).expect("a URL");
    let base = format!(
        "{}://{}:{}",
        url.scheme(),
        url.host_str().expect("a host"),
        url.port().unwrap_or(8123)
    );
    let db = format!("mf_live_reports_{}", std::process::id());
    let ch = Ch::connect(&base, &db, url.username(), url.password().unwrap_or("")).unwrap();
    ch.init_schema().await.expect("the schema is created");

    let mut events = vec![
        // An anonymous visit on the day hash the site later identifies.
        event(50, "pageview", "", "/docs", "v2", "s-anon"),
        event(40, "pageview", "", "/pricing", "v1", "s1"),
        event(39, "pageview", "", "/pricing", "v1", "s1"),
        event(38, "pageview", "", "/pricing", "v3", "s3"),
    ];
    let mut rage = event(37, "frustration", "rage_click", "/pricing", "v1", "s1");
    rage.props = r#"{"selector":"button.buy","clicks":4}"#.into();
    let mut dead = event(36, "frustration", "dead_click", "/pricing", "v3", "s3");
    dead.props = r#"{"selector":"a.nav"}"#.into();
    let mut abandon_a = event(35, "frustration", "form_abandon", "/signup", "v1", "s1");
    abandon_a.props = r#"{"form":"signup","field":"email"}"#.into();
    let mut abandon_b = event(34, "frustration", "form_abandon", "/signup", "v3", "s3");
    abandon_b.props = r#"{"form":"signup","field":"email"}"#.into();
    let mut first_call = event(33, "identify", "", "/account", "v1", "s1");
    first_call.identified_id = "u_1".into();
    first_call.props = r#"{"email":"Ana@Example.com","plan":"free"}"#.into();
    let mut second_call = event(20, "identify", "", "/account", "v2", "s2");
    second_call.identified_id = "u_1".into();
    second_call.props = r#"{"plan":"pro"}"#.into();
    let mut sampled = event(10, "pageview", "", "/blog", "v4", "s4");
    sampled.sample_rate = 25;
    events.extend([
        rage,
        dead,
        abandon_a,
        abandon_b,
        first_call,
        second_call,
        sampled,
    ]);
    insert(&ch, "events", &events).await;

    let a = "a1a1a1a1a1a1a1a1";
    let b = "b2b2b2b2b2b2b2b2";
    insert(
        &ch,
        "errors",
        &[
            error(30, a, "Chrome", "r1", 0),
            error(25, a, "Chrome", "r1", 1),
            error(20, b, "Firefox", "r2", 1),
        ],
    )
    .await;

    let end = Utc::now() + Duration::minutes(1);
    let env = QueryEnvelope {
        site: SITE,
        range: TimeRange::new(end - Duration::hours(2), end, chrono_tz::UTC),
        compare: Compare::None,
        granularity: Granularity::Hour,
        filters: None,
        audience: Audience::Human,
        limit: 100,
        page: 1,
    };

    // Frustration.
    let report = stats::frustration(&ch, &env)
        .await
        .expect("frustration reads");
    assert_eq!(
        (
            report.rage_clicks,
            report.dead_clicks,
            report.abandoned_forms
        ),
        (1, 1, 2)
    );
    let pricing = report
        .pages
        .iter()
        .find(|p| p.pathname == "/pricing")
        .expect("/pricing");
    assert_eq!(pricing.pageviews, 3);
    assert_eq!(pricing.rage_clicks, 1);
    let buy = report
        .elements
        .iter()
        .find(|e| e.selector == "button.buy")
        .expect("button.buy");
    assert_eq!((buy.signal.as_str(), buy.clicks), ("rage_click", 4));
    assert_eq!(report.forms[0].form, "signup");
    assert_eq!(report.forms[0].abandons, 2);
    assert_eq!(report.forms[0].last_fields[0].field, "email");

    // Sampling.
    let sampling = stats::sampling(&ch, &env).await.expect("sampling reads");
    assert!(sampling.sampled);
    assert_eq!((sampling.min_rate, sampling.max_rate), (25, 100));

    // Profiles.
    let profile = stats::profile(&ch, &env, "u_1")
        .await
        .expect("the profile reads")
        .expect("u_1 was identified");
    assert_eq!(profile.history.len(), 2);
    assert_eq!(profile.traits["plan"], "pro");
    assert_eq!(profile.linked.len(), 2);
    assert!(
        profile
            .anonymous_sessions
            .iter()
            .any(|s| s.session_id == "s-anon"),
        "the day hash's earlier visit is the person's"
    );
    assert!(stats::profile(&ch, &env, "nobody").await.unwrap().is_none());
    let hits = stats::search_profiles(&ch, &env, "ana@example")
        .await
        .expect("search reads");
    assert_eq!(hits.len(), 1, "matched without regard to case");
    assert_eq!(hits[0].identified_id, "u_1");
    assert!(stats::search_profiles(&ch, &env, "zzz")
        .await
        .unwrap()
        .is_empty());

    // Errors.
    let all = stats::ErrorNarrow::default();
    let groups = stats::error_groups(&ch, &env, &all)
        .await
        .expect("groups read");
    assert_eq!(groups.len(), 2);
    let group_a = groups.iter().find(|g| g.fingerprint == a).unwrap();
    assert_eq!(group_a.count, 2);
    assert!(
        !group_a.handled,
        "one uncaught copy makes the group a crash"
    );
    assert_eq!(group_a.release, "r1");
    assert_eq!(group_a.top_path, "/checkout");
    assert_eq!(group_a.browsers, vec!["Chrome".to_string()]);
    let totals = stats::error_totals(&ch, &env, &all)
        .await
        .expect("totals read");
    assert_eq!(
        (totals.groups, totals.occurrences, totals.unhandled),
        (2, 3, 1)
    );
    let facets = stats::error_facets(&ch, &env, &all)
        .await
        .expect("facets read");
    assert_eq!(facets.browser.len(), 2);
    assert_eq!(facets.release.len(), 2);
    let r1 = stats::ErrorNarrow {
        release: Some("r1".into()),
        fingerprint: None,
    };
    assert_eq!(stats::error_groups(&ch, &env, &r1).await.unwrap().len(), 1);
    let detail = stats::error_detail(&ch, &env, a, None)
        .await
        .expect("the detail reads")
        .expect("group a fired");
    assert!(detail.stack.contains("TypeError"));
    assert_eq!(detail.sessions.len(), 2);
    assert!(!detail.series.is_empty());
    assert_eq!(detail.by_browser[0].value, "Chrome");

    ch.client()
        .query(&format!("DROP DATABASE IF EXISTS {db}"))
        .execute()
        .await
        .expect("the throwaway database is dropped");
}
