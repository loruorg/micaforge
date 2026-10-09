//! The monitoring and growth tables against a real Postgres.
//!
//! The properties that matter here live in SQL: a monitor cannot be claimed
//! twice while its lease runs, an incident opens once and closes once, a slug
//! is unique, an event id is processed once, and a refund total only moves
//! forward. A test without the database would be testing a model of them.
//!
//! Opt in with
//!
//! ```text
//! MICAFORGE_TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/postgres \
//! cargo test -p micaforge-store --test monitoring_growth_live -- --ignored
//! ```
//!
//! The test creates a database of its own (the role needs `CREATEDB`),
//! migrates it, and drops it at the end.

use chrono::Utc;
use micaforge_core::{new_id, Error};
use micaforge_store::models::links::{self, FolderFilter, LinkFields, Scope};
use micaforge_store::models::status_pages::{self, PageFields};
use micaforge_store::models::uptime::{self, CheckRecord, IncidentChange, MonitorFields};
use micaforge_store::models::{orgs, revenue, sites, users};
use serde_json::json;

#[tokio::test]
#[ignore = "needs a Postgres server"]
async fn monitoring_and_growth_hold_their_invariants_in_sql() {
    let admin_url = std::env::var("MICAFORGE_TEST_DATABASE_URL")
        .expect("set MICAFORGE_TEST_DATABASE_URL to run this test");
    let database = format!("mf_test_a2_{}", micaforge_core::crypto::numeric_code(9));
    let admin = micaforge_store::pg::connect(&admin_url).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let (server, _) = admin_url.rsplit_once('/').unwrap();
    let pg = micaforge_store::pg::connect(&format!("{server}/{database}"))
        .await
        .unwrap();
    micaforge_store::pg::migrate(&pg).await.unwrap();

    let user = users::create(&pg, new_id(), "owner@example.test", "Owner")
        .await
        .unwrap();
    let org = orgs::create(&pg, new_id(), "Org", "org", user.id)
        .await
        .unwrap();
    let site = sites::create(&pg, new_id(), org.id, "shop.example.test", "Shop", "UTC")
        .await
        .unwrap();

    // -- uptime: claim, lease, open, close --------------------------------
    let fields = MonitorFields {
        name: "Health".into(),
        url: "https://shop.example.test/health".into(),
        method: "GET".into(),
        interval_secs: 60,
        timeout_ms: 10_000,
        status_min: 200,
        status_max: 299,
        keyword: None,
        json_path: None,
        json_expected: None,
        ssl_check: true,
        ssl_expiry_days: 14,
        failure_threshold: 1,
        channels: json!([]),
        enabled: true,
    };
    let monitor = uptime::create(&pg, new_id(), site.id, &fields)
        .await
        .unwrap();
    assert!(uptime::any_enabled(&pg).await.unwrap());

    let claimed = uptime::claim_due(&pg, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(claimed[0].lease_until.is_some());
    assert!(
        uptime::claim_due(&pg, 10).await.unwrap().is_empty(),
        "a leased monitor is never claimed a second time"
    );

    let opened = uptime::record_check(
        &pg,
        &claimed[0],
        &CheckRecord {
            state: "down",
            consecutive_failures: 1,
            checked_at: Utc::now(),
            open: Some("could not connect"),
            close: false,
        },
    )
    .await
    .unwrap();
    let IncidentChange::Opened(incident) = opened else {
        panic!("the first failure at threshold 1 opens an incident");
    };
    // A second open while one is open opens nothing.
    let again = uptime::record_check(
        &pg,
        &claimed[0],
        &CheckRecord {
            state: "down",
            consecutive_failures: 2,
            checked_at: Utc::now(),
            open: Some("could not connect"),
            close: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(again, IncidentChange::None);
    assert_eq!(uptime::open_incident_count(&pg, site.id).await.unwrap(), 1);

    // -- status pages: an automatic incident, once per uptime incident -----
    let page = status_pages::create_page(
        &pg,
        new_id(),
        site.id,
        &PageFields {
            slug: "shop".into(),
            title: "Shop status".into(),
            description: String::new(),
            logo_url: None,
            accent_color: "#2f6bff".into(),
            footer_label: None,
            footer_url: None,
            published: true,
            monitors: vec![(monitor.id, Some("API".into()))],
        },
    )
    .await
    .unwrap();
    let words = |name: &str| {
        (
            format!("{name} is down"),
            format!("{name} is not answering."),
        )
    };
    assert_eq!(
        status_pages::auto_open(&pg, monitor.id, incident, words)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        status_pages::auto_open(&pg, monitor.id, incident, words)
            .await
            .unwrap(),
        0,
        "a retried sweep opens no second page incident"
    );
    let listed = status_pages::list_incidents(&pg, page.id, 10)
        .await
        .unwrap();
    assert_eq!(listed[0].title, "API is down");
    assert!(listed[0].automatic);

    let closed = uptime::record_check(
        &pg,
        &monitor,
        &CheckRecord {
            state: "up",
            consecutive_failures: 0,
            checked_at: Utc::now(),
            open: None,
            close: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(closed, IncidentChange::Closed(incident));
    assert_eq!(
        status_pages::auto_resolve(&pg, incident, "Back.")
            .await
            .unwrap(),
        1
    );
    let resolved = status_pages::get_incident(&pg, listed[0].id).await.unwrap();
    assert_eq!(resolved.status, "resolved");
    assert!(resolved.resolved_at.is_some());
    let updates = status_pages::updates_of(&pg, &[resolved.id]).await.unwrap();
    assert_eq!(updates.len(), 2);
    assert!(
        status_pages::get_published_by_slug(&pg, "shop")
            .await
            .is_ok(),
        "a published page resolves by slug"
    );

    // -- links: one namespace, a joined site number, search ----------------
    let link_fields = |slug: &str| LinkFields {
        slug: slug.into(),
        folder_id: None,
        target_url: "https://example.com/pricing".into(),
        utm: json!({ "source": "newsletter" }),
        title: Some("Pricing 50%".into()),
        description: None,
        image_url: None,
        ios_url: None,
        android_url: None,
        deep_link: None,
        expires_at: None,
    };
    let link = links::create(
        &pg,
        new_id(),
        org.id,
        Some(site.id),
        Some(user.id),
        &link_fields("launch"),
    )
    .await
    .unwrap();
    let resolved = links::get_by_slug(&pg, "launch").await.unwrap().unwrap();
    assert_eq!(resolved.link.id, link.id);
    assert_eq!(resolved.site_number, Some(site.site_id));
    let duplicate = links::create(&pg, new_id(), org.id, None, None, &link_fields("launch")).await;
    assert!(matches!(duplicate, Err(Error::Conflict(_))));
    let found = links::list(
        &pg,
        Scope::Site(site.id),
        FolderFilter::Any,
        Some("50%"),
        10,
    )
    .await
    .unwrap();
    assert_eq!(
        found.len(),
        1,
        "the search matches the title, wildcards literal"
    );
    assert!(
        links::list(&pg, Scope::Org(org.id), FolderFilter::None, None, 10)
            .await
            .unwrap()
            .is_empty()
    );

    // -- revenue: one event once, refund totals only forward ---------------
    let integration = revenue::create(&pg, new_id(), site.id, "stripe", None)
        .await
        .unwrap();
    assert!(integration.secret_sealed.is_none());
    assert!(revenue::claim_event(&pg, integration.id, "evt_1")
        .await
        .unwrap());
    assert!(!revenue::claim_event(&pg, integration.id, "evt_1")
        .await
        .unwrap());
    revenue::release_event(&pg, integration.id, "evt_1")
        .await
        .unwrap();
    assert!(revenue::claim_event(&pg, integration.id, "evt_1")
        .await
        .unwrap());

    assert_eq!(
        revenue::advance_refund_total(&pg, integration.id, "ch_1", 500)
            .await
            .unwrap(),
        (0, 500)
    );
    assert_eq!(
        revenue::advance_refund_total(&pg, integration.id, "ch_1", 700)
            .await
            .unwrap(),
        (500, 700)
    );
    assert_eq!(
        revenue::advance_refund_total(&pg, integration.id, "ch_1", 600)
            .await
            .unwrap(),
        (700, 700),
        "a stale, smaller total moves nothing"
    );
    revenue::rewind_refund_total(&pg, integration.id, "ch_1", 500, 700)
        .await
        .unwrap();
    assert_eq!(
        revenue::advance_refund_total(&pg, integration.id, "ch_1", 700)
            .await
            .unwrap(),
        (500, 700)
    );
    revenue::remember_customer_ref(&pg, integration.id, "cus_1", "mfr_a")
        .await
        .unwrap();
    revenue::remember_customer_ref(&pg, integration.id, "cus_1", "mfr_b")
        .await
        .unwrap();
    assert_eq!(
        revenue::customer_ref(&pg, integration.id, "cus_1")
            .await
            .unwrap()
            .as_deref(),
        Some("mfr_b")
    );

    pg.close().await;
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
        .execute(&admin)
        .await;
}
