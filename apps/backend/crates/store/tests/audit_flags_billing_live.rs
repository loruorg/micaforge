//! The audit, flag, billing, sign-in identity, error-state and transfer queries,
//! against a real Postgres.
//!
//! Every query in these modules is runtime-checked SQL, so the build cannot
//! tell a misspelt column or a wrong jsonb operator from a correct one; only
//! running them can. Opt in with
//!
//! ```text
//! MICAFORGE_TEST_DATABASE_URL=postgres://user:pass@127.0.0.1:5432/postgres \
//! cargo test -p micaforge-store --test audit_flags_billing_live -- --ignored
//! ```
//!
//! Each test creates a database of its own on that server (the role needs
//! `CREATEDB`), migrates it, and drops it at the end.

use chrono::{Duration, NaiveDate, Utc};
use micaforge_core::{new_id, Error};
use micaforge_store::models::audit::{self, AuditEvent, AuditQuery};
use micaforge_store::models::billing::{self, SubscriptionState, Threshold};
use micaforge_store::models::flag_rules::{self, ChangeActor, NewChange};
use micaforge_store::models::flags::{self, FlagFields};
use micaforge_store::models::{error_states, keys, oauth_identities, orgs, site_ops, sites, users};
use micaforge_store::pg::{self, PgPool};
use serde_json::json;
use uuid::Uuid;

/// A migrated database of its own, and how to drop it.
struct Db {
    pool: PgPool,
    admin_url: String,
    name: String,
}

impl Db {
    async fn new() -> Db {
        let admin_url = std::env::var("MICAFORGE_TEST_DATABASE_URL")
            .expect("set MICAFORGE_TEST_DATABASE_URL to run this test");
        let name = format!("mf_test_{}", micaforge_core::crypto::numeric_code(9));
        let admin = pg::connect(&admin_url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        let (server, _) = admin_url.rsplit_once('/').unwrap();
        let pool = pg::connect(&format!("{server}/{name}")).await.unwrap();
        pg::migrate(&pool).await.unwrap();
        Db {
            pool,
            admin_url,
            name,
        }
    }

    async fn finish(self) {
        self.pool.close().await;
        let admin = pg::connect(&self.admin_url).await.unwrap();
        let _ = sqlx::query(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            self.name
        ))
        .execute(&admin)
        .await;
    }
}

/// An account, an organisation it owns, and a site in it.
async fn seed(pool: &PgPool, tag: &str) -> (users::User, orgs::Organization, sites::Site) {
    let user = users::create(pool, new_id(), &format!("{tag}@example.com"), tag)
        .await
        .unwrap();
    let org = orgs::create(pool, new_id(), tag, &format!("{tag}-org"), user.id)
        .await
        .unwrap();
    orgs::add_member(pool, org.id, user.id, "owner")
        .await
        .unwrap();
    let site = sites::create(
        pool,
        new_id(),
        org.id,
        &format!("{tag}.example.com"),
        tag,
        "UTC",
    )
    .await
    .unwrap();
    (user, org, site)
}

fn event(org: Option<Uuid>, actor: Uuid, action: &str, minutes_ago: i64) -> AuditEvent {
    AuditEvent {
        id: new_id(),
        org_id: org,
        actor_kind: "user".into(),
        actor_id: Some(actor),
        actor_label: "someone".into(),
        action: action.into(),
        target_type: "site".into(),
        target_id: Some("t1".into()),
        target_label: "label".into(),
        site_id: None,
        before: None,
        after: Some(json!({ "name": "x" })),
        metadata: None,
        ip_hash: Some("abc".into()),
        user_agent: None,
        created_at: Utc::now() - Duration::minutes(minutes_ago),
    }
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn an_organisations_log_holds_its_events_and_its_members_account_events() {
    let db = Db::new().await;
    let (owner, org, _) = seed(&db.pool, "ada").await;
    let (stranger, other_org, _) = seed(&db.pool, "bob").await;

    for e in [
        event(Some(org.id), owner.id, "site.created", 30),
        event(Some(org.id), owner.id, "site.updated", 20),
        event(Some(org.id), owner.id, "flag.created", 10),
        event(None, owner.id, "auth.signed_in", 5),
        event(None, stranger.id, "auth.signed_in", 4),
        event(Some(other_org.id), stranger.id, "site.created", 3),
    ] {
        audit::insert(&db.pool, &e).await.unwrap();
    }

    let query = |action: Option<&str>| AuditQuery {
        org_id: org.id,
        action: action.map(str::to_string),
        limit: 50,
        ..AuditQuery::default()
    };
    let all = audit::list(&db.pool, &query(None)).await.unwrap();
    let actions: Vec<&str> = all.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        vec![
            "auth.signed_in",
            "flag.created",
            "site.updated",
            "site.created"
        ],
        "newest first; the stranger's events are not in this log"
    );

    let sites_only = audit::list(&db.pool, &query(Some("site."))).await.unwrap();
    assert_eq!(sites_only.len(), 2);
    let exact = audit::list(&db.pool, &query(Some("site.created")))
        .await
        .unwrap();
    assert_eq!(exact.len(), 1);
    // A `%` in a filter is a character, not a wildcard.
    let wild = audit::list(&db.pool, &query(Some("site%"))).await.unwrap();
    assert!(wild.is_empty());

    // Paging down the cursor.
    let first = audit::list(
        &db.pool,
        &AuditQuery {
            limit: 2,
            ..query(None)
        },
    )
    .await
    .unwrap();
    let second = audit::list(
        &db.pool,
        &AuditQuery {
            limit: 2,
            before: first.last().map(|e| e.id),
            ..query(None)
        },
    )
    .await
    .unwrap();
    assert_eq!(first.len() + second.len(), 4);
    assert!(second.iter().all(|e| first.iter().all(|f| f.id != e.id)));

    // A time window.
    let recent = audit::list(
        &db.pool,
        &AuditQuery {
            from: Some(Utc::now() - Duration::minutes(15)),
            ..query(None)
        },
    )
    .await
    .unwrap();
    assert_eq!(recent.len(), 2);
    db.finish().await;
}

fn fields(key: &str) -> FlagFields {
    FlagFields {
        key: key.into(),
        name: String::new(),
        description: String::new(),
        enabled: true,
        rollout_pct: 100,
        variants: json!([]),
        conditions: json!({ "logic": "and", "filters": [] }),
        target_groups: json!([]),
        dependencies: json!([]),
        environments: json!({}),
    }
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn flags_their_groups_dependencies_and_history_round_trip() {
    let db = Db::new().await;
    let (owner, _, site) = seed(&db.pool, "cy").await;

    let group = flag_rules::create_group(
        &db.pool,
        new_id(),
        site.id,
        "Beta",
        "",
        &json!({ "logic": "and", "filters": [{ "dimension": "beta", "op": "is", "value": "true" }] }),
    )
    .await
    .unwrap();
    let duplicate =
        flag_rules::create_group(&db.pool, new_id(), site.id, "Beta", "", &json!({})).await;
    assert!(matches!(duplicate, Err(Error::Conflict(_))));

    let base = flags::create_flag(&db.pool, new_id(), site.id, &fields("base"))
        .await
        .unwrap();
    let mut child_fields = fields("child");
    child_fields.dependencies = json!([{ "flag": "base", "enabled": true, "variant": null }]);
    child_fields.target_groups = json!([group.id.to_string()]);
    child_fields.environments = json!({ "staging": { "enabled": false } });
    let child = flags::create_flag(&db.pool, new_id(), site.id, &child_fields)
        .await
        .unwrap();
    assert_eq!(child.environments["staging"]["enabled"], json!(false));

    let dependents = flags::list_dependents(&db.pool, site.id, "base")
        .await
        .unwrap();
    assert_eq!(
        dependents
            .iter()
            .map(|f| f.key.as_str())
            .collect::<Vec<_>>(),
        vec!["child"]
    );
    assert!(flags::list_dependents(&db.pool, site.id, "child")
        .await
        .unwrap()
        .is_empty());
    let reaching = flags::list_with_group(&db.pool, site.id, group.id)
        .await
        .unwrap();
    assert_eq!(reaching.len(), 1);

    // An update and a toggle, each recorded in one transaction with its write.
    let actor = ChangeActor {
        kind: "user".into(),
        id: Some(owner.id),
        label: owner.email.clone(),
    };
    let mut tx = db.pool.begin().await.unwrap();
    let mut next = FlagFields::of(&base);
    next.rollout_pct = 25;
    let updated = flags::update_flag(&mut *tx, base.id, &next).await.unwrap();
    flag_rules::record_change(
        &mut *tx,
        new_id(),
        &NewChange {
            flag_id: base.id,
            site_id: site.id,
            action: "updated",
            actor: &actor,
            before: Some(&json!({ "rollout_pct": 100 })),
            after: Some(&json!({ "rollout_pct": 25 })),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(updated.rollout_pct, 25);
    assert!(updated.updated_at >= base.updated_at);

    let toggled = flags::set_flag_enabled(&db.pool, base.id, false)
        .await
        .unwrap();
    assert!(!toggled.enabled);

    let history = flag_rules::list_changes(&db.pool, base.id, None)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(
        flag_rules::get_change(&db.pool, history[0].id)
            .await
            .unwrap()
            .action,
        "updated"
    );
    let latest = flag_rules::latest_changes(&db.pool, site.id).await.unwrap();
    assert_eq!(latest.len(), 1);

    // Deleting a flag takes its history; deleting a group leaves the flags.
    flags::delete_flag(&db.pool, base.id).await.unwrap();
    assert!(flag_rules::get_change(&db.pool, history[0].id)
        .await
        .is_err());
    flag_rules::delete_group(&db.pool, group.id).await.unwrap();
    assert!(flags::get_flag(&db.pool, child.id).await.is_ok());
    db.finish().await;
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn billing_state_usage_claims_and_stripe_events() {
    let db = Db::new().await;
    let (_, org, _) = seed(&db.pool, "di").await;

    assert!(billing::get(&db.pool, org.id).await.unwrap().is_none());
    billing::set_customer(&db.pool, org.id, "cus_1")
        .await
        .unwrap();
    assert_eq!(
        billing::find_by_customer(&db.pool, "cus_1")
            .await
            .unwrap()
            .unwrap()
            .org_id,
        org.id
    );

    billing::mark_payment_failed(&db.pool, org.id)
        .await
        .unwrap();
    assert!(billing::get(&db.pool, org.id)
        .await
        .unwrap()
        .unwrap()
        .payment_failed_at
        .is_some());
    let state = SubscriptionState {
        plan_id: Some("team".into()),
        customer_id: None,
        subscription_id: Some("sub_1".into()),
        status: "active".into(),
        current_period_end: Some(Utc::now() + Duration::days(30)),
        cancel_at_period_end: false,
    };
    let row = billing::apply_subscription(&db.pool, org.id, &state)
        .await
        .unwrap();
    assert_eq!(row.plan_id.as_deref(), Some("team"));
    assert_eq!(
        row.stripe_customer_id.as_deref(),
        Some("cus_1"),
        "a missing customer keeps the old one"
    );
    assert!(
        row.payment_failed_at.is_none(),
        "a good status clears a failed payment"
    );

    let month = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
    billing::upsert_usage(&db.pool, org.id, month, 900, 40, Some(1_000), 0)
        .await
        .unwrap();
    assert!(
        billing::claim_warning(&db.pool, org.id, month, Threshold::Warn)
            .await
            .unwrap()
    );
    assert!(
        !billing::claim_warning(&db.pool, org.id, month, Threshold::Warn)
            .await
            .unwrap()
    );
    let again = billing::upsert_usage(&db.pool, org.id, month, 1_200, 50, Some(1_000), 200)
        .await
        .unwrap();
    assert!(
        again.warned_80_at.is_some(),
        "a new measurement keeps the warning stamp"
    );
    assert_eq!(again.overage_events, 200);
    assert!(
        billing::claim_warning(&db.pool, org.id, month, Threshold::Reached)
            .await
            .unwrap()
    );
    billing::release_warning(&db.pool, org.id, month, Threshold::Reached)
        .await
        .unwrap();
    assert!(
        billing::claim_warning(&db.pool, org.id, month, Threshold::Reached)
            .await
            .unwrap()
    );
    assert_eq!(
        billing::get_usage(&db.pool, org.id, month)
            .await
            .unwrap()
            .unwrap()
            .events,
        1_200
    );

    assert!(
        billing::record_stripe_event(&db.pool, "evt_1", "invoice.payment_failed")
            .await
            .unwrap()
    );
    assert!(
        !billing::record_stripe_event(&db.pool, "evt_1", "invoice.payment_failed")
            .await
            .unwrap()
    );
    billing::forget_stripe_event(&db.pool, "evt_1")
        .await
        .unwrap();
    assert!(
        billing::record_stripe_event(&db.pool, "evt_1", "invoice.payment_failed")
            .await
            .unwrap()
    );
    db.finish().await;
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn identities_link_once_per_provider_and_unlink() {
    let db = Db::new().await;
    let (user, _, _) = seed(&db.pool, "eve").await;

    oauth_identities::link(&db.pool, "github", "42", user.id, "Eve@Example.com")
        .await
        .unwrap();
    let found = oauth_identities::find(&db.pool, "github", "42")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.user_id, user.id);
    assert_eq!(found.email, "eve@example.com");

    // A second GitHub account for the same Micaforge account is refused.
    let second = oauth_identities::link(&db.pool, "github", "43", user.id, "eve@example.com").await;
    assert!(matches!(second, Err(Error::Conflict(_))));
    oauth_identities::link(&db.pool, "google", "g-1", user.id, "eve@example.com")
        .await
        .unwrap();

    oauth_identities::touch(&db.pool, "github", "42", "new@example.com")
        .await
        .unwrap();
    assert_eq!(
        oauth_identities::find(&db.pool, "github", "42")
            .await
            .unwrap()
            .unwrap()
            .email,
        "new@example.com"
    );
    assert_eq!(
        oauth_identities::list_for_user(&db.pool, user.id)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(oauth_identities::unlink(&db.pool, user.id, "github")
        .await
        .unwrap());
    assert!(!oauth_identities::unlink(&db.pool, user.id, "github")
        .await
        .unwrap());
    db.finish().await;
}

#[tokio::test]
#[ignore = "needs Postgres"]
async fn error_states_and_a_site_transfer() {
    let db = Db::new().await;
    let (owner, from, site) = seed(&db.pool, "fay").await;
    let (_, to, _) = seed(&db.pool, "gus").await;

    error_states::set(&db.pool, site.id, "fp1", "resolved", owner.id)
        .await
        .unwrap();
    let flipped = error_states::set(&db.pool, site.id, "fp1", "ignored", owner.id)
        .await
        .unwrap();
    assert_eq!(flipped.status, "ignored");
    let states = error_states::for_site(&db.pool, site.id).await.unwrap();
    assert_eq!(states.len(), 1);
    assert!(states.contains_key("fp1"));

    // A member with an override, and a key bound to the site.
    let member = users::create(&db.pool, new_id(), "hal@example.com", "Hal")
        .await
        .unwrap();
    orgs::add_member(&db.pool, from.id, member.id, "viewer")
        .await
        .unwrap();
    sites::grant_access(&db.pool, site.id, member.id, "member")
        .await
        .unwrap();
    let access = site_ops::access_of_member(&db.pool, from.id, member.id)
        .await
        .unwrap();
    assert_eq!(access.len(), 1);
    assert_eq!(access[0].role, "member");
    keys::create(
        &db.pool,
        new_id(),
        from.id,
        Some(site.id),
        "bound",
        "mfg_prefix00000",
        "digest",
        &json!(["stats:read"]),
        None,
    )
    .await
    .unwrap();

    let done = site_ops::transfer_site(&db.pool, site.id, from.id, to.id)
        .await
        .unwrap();
    assert_eq!(done.access_dropped, 1);
    assert_eq!(done.keys_revoked, 1);
    assert_eq!(sites::get(&db.pool, site.id).await.unwrap().org_id, to.id);
    // Moving it again from the wrong organisation finds nothing to move.
    let stale = site_ops::transfer_site(&db.pool, site.id, from.id, to.id).await;
    assert!(matches!(stale, Err(Error::NotFound(_))));

    error_states::clear_site(&db.pool, site.id).await.unwrap();
    assert!(error_states::for_site(&db.pool, site.id)
        .await
        .unwrap()
        .is_empty());
    db.finish().await;
}
