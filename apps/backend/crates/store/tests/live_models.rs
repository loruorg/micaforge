//! The error-state, import-credential and log-drain models, against a live
//! Postgres.
//!
//! Their SQL is checked at run time, not at compile time, so this is where a
//! misspelt column or a constraint that refuses a legal row would surface. It
//! creates a database of its own on the server (the role needs `CREATEDB`),
//! applies every migration, exercises the three models, and drops the database.
//!
//! Opt in with
//! `MICAFORGE_TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/postgres`
//! and `cargo test -p micaforge-store --test live_models -- --ignored`.

use chrono::{Duration, Utc};
use micaforge_core::new_id;
use micaforge_store::models::{error_states, imports, log_drains};
use micaforge_store::pg;

#[tokio::test]
#[ignore = "needs a Postgres server"]
async fn the_new_models_round_trip_on_a_live_database() {
    let admin_url = std::env::var("MICAFORGE_TEST_DATABASE_URL")
        .expect("set MICAFORGE_TEST_DATABASE_URL to run this test");
    let database = format!("mf_live_models_{}", std::process::id());
    let admin = pg::connect(&admin_url).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {database}"))
        .execute(&admin)
        .await
        .unwrap();
    let (server, _) = admin_url.rsplit_once('/').unwrap();
    let pool = pg::connect(&format!("{server}/{database}")).await.unwrap();
    pg::migrate(&pool).await.expect("every migration applies");

    // One account, one organisation, one site.
    let (user, org, site) = (new_id(), new_id(), new_id());
    sqlx::query("INSERT INTO users (id, email, name) VALUES ($1, 'a@example.com', 'A')")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO organizations (id, name, slug, owner_id) VALUES ($1, 'Org', 'org', $2)",
    )
    .bind(org)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO sites (id, org_id, domain, name) VALUES ($1, $2, 'example.com', 'Ex')",
    )
    .bind(site)
    .bind(org)
    .execute(&pool)
    .await
    .unwrap();

    // Error states: marked, read back, re-marked, cleared.
    let fp = "a1a1a1a1a1a1a1a1";
    error_states::set(&pool, site, fp, "resolved", user)
        .await
        .unwrap();
    let marks = error_states::for_site(&pool, site).await.unwrap();
    assert_eq!(marks[fp].status, "resolved");
    assert_eq!(
        error_states::effective(marks.get(fp), Utc::now() - Duration::hours(1)),
        "resolved"
    );
    error_states::set(&pool, site, fp, "ignored", user)
        .await
        .unwrap();
    assert_eq!(
        error_states::for_site(&pool, site).await.unwrap()[fp].status,
        "ignored"
    );
    assert!(
        error_states::set(&pool, site, fp, "fixed", user)
            .await
            .is_err(),
        "the CHECK refuses a state that is not one"
    );
    error_states::clear(&pool, site, fp).await.unwrap();
    assert!(error_states::for_site(&pool, site)
        .await
        .unwrap()
        .is_empty());

    // Import credentials: stored sealed with the row, gone once it is done.
    let import = new_id();
    let created = imports::create(&pool, import, site, "posthog", Some(b"sealed-bytes"))
        .await
        .expect("the new sources pass the source CHECK");
    assert_eq!(created.source, "posthog");
    assert_eq!(
        imports::credentials(&pool, import)
            .await
            .unwrap()
            .as_deref(),
        Some(&b"sealed-bytes"[..])
    );
    imports::complete(&pool, import).await.unwrap();
    assert_eq!(imports::credentials(&pool, import).await.unwrap(), None);
    let other = new_id();
    imports::create(&pool, other, site, "simple_analytics", Some(b"x"))
        .await
        .unwrap();
    imports::forget_credentials(&pool, other).await.unwrap();
    assert_eq!(imports::credentials(&pool, other).await.unwrap(), None);
    assert!(imports::create(&pool, new_id(), site, "matomo", None)
        .await
        .is_err());

    // Log drains.
    let drain = new_id();
    log_drains::create(&pool, drain, site, "vercel", "c2VhbGVk", None, Some(user))
        .await
        .unwrap();
    assert_eq!(
        log_drains::find(&pool, drain)
            .await
            .unwrap()
            .unwrap()
            .site_id,
        site
    );
    let updated = log_drains::set_verify_token(&pool, site, drain, Some("tok"))
        .await
        .unwrap();
    assert_eq!(updated.verify_token.as_deref(), Some("tok"));
    log_drains::mark_received(&pool, drain).await.unwrap();
    let listed = log_drains::list_for_site(&pool, site).await.unwrap();
    assert!(listed[0].last_received_at.is_some());
    log_drains::delete(&pool, site, drain).await.unwrap();
    assert!(log_drains::find(&pool, drain).await.unwrap().is_none());

    pool.close().await;
    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)"))
        .execute(&admin)
        .await;
}
