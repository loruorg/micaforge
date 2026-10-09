//! MCP tools for feature flags: `list_flags`, `create_flag`, `update_flag`,
//! `set_flag_enabled` and `delete_flag`.
//!
//! An agent that ships a feature behind a flag can create the flag, roll it
//! out step by step, switch it off in an incident and remove it once the code
//! no longer reads it, without a person opening the flag screen for each step.
//! Every tool is a second front door onto the flag routes
//! ([`crate::routes::flags`]): the definition goes through the same checks
//! (variants, targeting conditions, environments, prerequisite flags and the
//! loop check, target groups), the same `flag_changes` history row and the
//! same audit entry, so a flag an agent wrote is indistinguishable from one a
//! person wrote except for who the history says wrote it.
//!
//! ## Who may write
//!
//! The same as the routes: `member` on the site for every write, `viewer` to
//! list. On top of that the credential must hold `manage:write`, whether it
//! is an API key or an OAuth token an MCP client obtained (the consent screen
//! offers that scope separately from reading). A key or token never acts
//! above `member`, so nothing here can exceed what the routes allow.
//!
//! ## Deleting needs `confirm: true`
//!
//! `delete_flag` is refused unless the call carries `confirm: true`. A flag
//! that is deleted takes its history with it and every SDK reading it falls
//! back to its default at once; an agent should have asked the person first,
//! and the argument is where it says it did.

use micaforge_core::{Error, Result};
use micaforge_store::models::flags::{self, FeatureFlag};
use micaforge_store::models::sites::Site;
use serde_json::{json, Map, Value};

use super::mcp::{acting_user, authorize_site, require_write, site_argument, string_argument};
use crate::extract::AuthUser;
use crate::routes::flags::{self as flag_routes, flag_json, FlagInput};
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::effective_site_role;
use crate::state::AppState;

/// The definition fields a create or an update may set, as the flag routes
/// name them.
const FIELDS: &[&str] = &[
    "name",
    "description",
    "enabled",
    "rollout_pct",
    "variants",
    "conditions",
    "target_groups",
    "dependencies",
    "environments",
];

/// The JSON Schema of the definition fields, shared by create and update.
fn field_schemas() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("name".into(), json!({ "type": "string", "maxLength": 120, "description": "What the flag screen calls it." }));
    m.insert("description".into(), json!({ "type": "string", "maxLength": 1000, "description": "What the flag controls, for the people who will find it later." }));
    m.insert("enabled".into(), json!({ "type": "boolean", "description": "Whether the flag is on at all. Defaults to true on create." }));
    m.insert("rollout_pct".into(), json!({ "type": "integer", "minimum": 0, "maximum": 100, "description": "The share of matching visitors who get it, 0 to 100. A visitor keeps the answer they first got as the share grows. Defaults to 100 on create." }));
    m.insert("variants".into(), json!({
        "type": "array",
        "description": "For a multivariate flag: [{\"key\":\"control\",\"weight\":50},{\"key\":\"treatment\",\"weight\":50}]. Weights are whole numbers. Leave empty for a plain on/off flag.",
        "items": { "type": "object", "properties": { "key": { "type": "string" }, "weight": { "type": "integer", "minimum": 0 } }, "required": ["key"] }
    }));
    m.insert("conditions".into(), json!({
        "type": "object",
        "description": "Who it applies to: {\"logic\":\"and\",\"filters\":[{\"dimension\":\"country\",\"op\":\"is\",\"value\":\"DE\"}]}. A dimension is any key the SDK sends in its context (country, plan, beta, ...). An empty filter list matches everyone."
    }));
    m.insert("target_groups".into(), json!({ "type": "array", "items": { "type": "string" }, "description": "Ids of this site's saved target groups the flag reaches, at most 16." }));
    m.insert("dependencies".into(), json!({
        "type": "array",
        "description": "Prerequisite flags on the same site: [{\"flag\":\"new-nav\",\"enabled\":true}] or with \"variant\":\"treatment\". The flag is off unless every prerequisite holds. Loops are refused.",
        "items": { "type": "object" }
    }));
    m.insert("environments".into(), json!({
        "type": "object",
        "description": "Per-environment overrides keyed by the environment the SDK names: {\"staging\":{\"enabled\":true,\"rollout_pct\":100}}."
    }));
    m
}

/// A tool's input schema: `site` and `key`, then `extra`.
fn schema(extra: Map<String, Value>, required: &[&str]) -> Value {
    let mut props = Map::new();
    props.insert(
        "site".into(),
        json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`." }),
    );
    props.insert(
        "key".into(),
        json!({ "type": "string", "maxLength": 64, "description": "The flag key the code reads: letters, digits, `-`, `_` or `.`, lower case." }),
    );
    props.extend(extra);
    json!({
        "type": "object",
        "properties": Value::Object(props),
        "required": required,
        "additionalProperties": false
    })
}

/// This module's tools.
pub fn tools() -> Vec<Value> {
    let mut create = field_schemas();
    create.insert(
        "template".into(),
        json!({ "type": "string", "enum": ["gradual-rollout", "beta-users", "kill-switch", "ab-test"], "description": "Start from a preset; any field given beside it wins." }),
    );
    let mut update = field_schemas();
    update.insert(
        "rename_to".into(),
        json!({ "type": "string", "maxLength": 64, "description": "A new key. Refused while another flag names this one as a prerequisite, and every SDK reading the old key falls back to its default." }),
    );
    let site_only = {
        let mut props = Map::new();
        props.insert(
            "site".into(),
            json!({ "type": "integer", "minimum": 1, "maximum": 65535, "description": "Numeric site id, from `list_sites`." }),
        );
        json!({ "type": "object", "properties": Value::Object(props), "required": ["site"], "additionalProperties": false })
    };
    let mut toggle = Map::new();
    toggle.insert(
        "enabled".into(),
        json!({ "type": "boolean", "description": "true to switch it on, false to switch it off." }),
    );
    let mut confirm = Map::new();
    confirm.insert(
        "confirm".into(),
        json!({ "type": "boolean", "const": true, "description": "Must be true. Set it only after the person agreed to delete this flag." }),
    );

    vec![
        json!({
            "name": "list_flags",
            "description": "The site's feature flags with their full definitions: key, on or off, rollout share, variants, \
        targeting conditions, target groups, prerequisites, per-environment overrides, and who changed each last. Read it before \
        changing a flag, to learn its exact key and current state.",
            "inputSchema": site_only,
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "create_flag",
            "description": "Create a feature flag on a site. Only `key` is needed: a flag with nothing else is on for everyone. \
        Use `rollout_pct` for a gradual rollout, `variants` for an A/B split, `conditions` to target by what the SDK sends \
        (country, plan, beta and so on), `dependencies` to require other flags, `environments` for per-environment overrides. \
        The key must not exist on the site yet. Create a flag only when the person asked for one or the code you are shipping \
        reads it. Needs the manage:write scope and a member's role.",
            "inputSchema": schema(create, &["site", "key"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "update_flag",
            "description": "Change an existing flag, found by `site` and `key`. Send only the fields to change; every other \
        field keeps its value. The whole definition is checked again (variants, conditions, prerequisites and loops) and the \
        change is kept in the flag's history, where a person can revert it. To switch a flag on or off use set_flag_enabled \
        instead. Needs the manage:write scope and a member's role.",
            "inputSchema": schema(update, &["site", "key"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "set_flag_enabled",
            "description": "Switch a flag on or off and change nothing else: the kill switch. Takes effect for the SDKs \
        within about thirty seconds. Use it to stop a misbehaving feature at once. Needs the manage:write scope and a member's role.",
            "inputSchema": schema(toggle, &["site", "key", "enabled"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "delete_flag",
            "description": "Delete a flag and its history for good. Every SDK reading it falls back to its default at once. \
        Refused while another flag names it as a prerequisite. Ask the person before deleting, then call with `confirm: true`; \
        without it the call is refused. To stop a feature without deleting anything, use set_flag_enabled. Needs the \
        manage:write scope and a member's role.",
            "inputSchema": schema(confirm, &["site", "key", "confirm"]),
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
    ]
}

/// Run `name` if it is this module's.
pub async fn call(
    st: &AppState,
    auth: &AuthUser,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    let done = match name {
        "list_flags" => list(st, auth, args).await,
        "create_flag" => create(st, auth, args).await,
        "update_flag" => update(st, auth, args).await,
        "set_flag_enabled" => set_enabled(st, auth, args).await,
        "delete_flag" => delete(st, auth, args).await,
        _ => return None,
    };
    Some(done)
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

/// Refuse an argument the tool does not take.
///
/// The schema says `additionalProperties: false`, and a model that misspells
/// `rollout_pct` as `rollout` would otherwise get a flag that silently kept
/// its old rollout.
pub(crate) fn only_known(args: &Value, allowed: &[&str]) -> Result<()> {
    let Some(object) = args.as_object() else {
        return Err(Error::validation("arguments", "must be an object"));
    };
    if let Some(unknown) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::validation(
            unknown.as_str(),
            format!(
                "`{unknown}` is not an argument of this tool; it takes {}",
                allowed.join(", ")
            ),
        ));
    }
    Ok(())
}

/// Refuse a destructive call that does not carry `confirm: true`.
pub(crate) fn require_confirm(args: &Value, what: &str) -> Result<()> {
    if args.get("confirm") == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(Error::validation(
            "confirm",
            format!(
                "must be true: deleting {what} cannot be undone. Ask the person first, then call again with `confirm: true`"
            ),
        ))
    }
}

/// The definition fields of a call, as the flag routes read them.
///
/// `rename_to` becomes the definition's key; on a create, `key` is the key.
/// Nothing is checked here beyond the JSON types: the route's own checks run
/// when the input is applied.
fn flag_input(args: &Value, for_update: bool) -> Result<FlagInput> {
    let mut allowed: Vec<&str> = vec!["site", "key"];
    allowed.extend_from_slice(FIELDS);
    allowed.push(if for_update { "rename_to" } else { "template" });
    only_known(args, &allowed)?;

    let mut body = Map::new();
    for field in FIELDS.iter().chain(["template"].iter()) {
        if let Some(v) = args.get(*field).filter(|v| !v.is_null()) {
            body.insert((*field).to_string(), v.clone());
        }
    }
    let key = if for_update {
        args.get("rename_to").filter(|v| !v.is_null()).cloned()
    } else {
        args.get("key").cloned()
    };
    if let Some(key) = key {
        body.insert("key".into(), key);
    }
    serde_json::from_value(Value::Object(body))
        .map_err(|e| Error::validation("arguments", format!("could not be read as a flag: {e}")))
}

/// The site, with `needed` asserted on it for the calling credential.
async fn site_with(st: &AppState, auth: &AuthUser, args: &Value, needed: &str) -> Result<Site> {
    let site = authorize_site(st, auth, site_argument(args)?).await?;
    let role = effective_site_role(&st.pg, &site, acting_user(auth)).await?;
    require_at_least(&role, needed)?;
    Ok(site)
}

/// The flag `key` names on `site`.
async fn flag_on(st: &AppState, site: &Site, args: &Value) -> Result<FeatureFlag> {
    let key = string_argument(args, "key")
        .ok_or_else(|| Error::validation("key", "is required; call `list_flags` for the keys"))?
        .to_lowercase();
    flags::get_flag_by_key(&st.pg, site.id, &key)
        .await?
        .ok_or_else(|| Error::not_found(format!("feature flag `{key}` on site {}", site.site_id)))
}

/// A flag answer, with the numeric site beside it.
fn answer(mut flag: Value, site: &Site) -> Value {
    flag["site"] = json!(site.site_id_u16());
    flag
}

// ---------------------------------------------------------------------------
// The tools
// ---------------------------------------------------------------------------

async fn list(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    only_known(args, &["site"])?;
    let site = site_with(st, auth, args, VIEWER).await?;
    let rows = flags::list_flags(&st.pg, site.id).await?;
    Ok(Value::Array(
        rows.iter()
            .map(|f| answer(flag_json(f, None), &site))
            .collect(),
    ))
}

async fn create(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let input = flag_input(args, false)?;
    let site = site_with(st, auth, args, MEMBER).await?;
    Ok(answer(
        flag_routes::create(st, auth, &site, input).await?,
        &site,
    ))
}

async fn update(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    let input = flag_input(args, true)?;
    let site = site_with(st, auth, args, MEMBER).await?;
    let flag = flag_on(st, &site, args).await?;
    Ok(answer(
        flag_routes::update(st, auth, &site, &flag, input).await?,
        &site,
    ))
}

async fn set_enabled(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    only_known(args, &["site", "key", "enabled"])?;
    let enabled = args
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| Error::validation("enabled", "must be true or false"))?;
    let site = site_with(st, auth, args, MEMBER).await?;
    let flag = flag_on(st, &site, args).await?;
    Ok(answer(
        flag_routes::set_enabled(st, auth, &site, &flag, enabled).await?,
        &site,
    ))
}

async fn delete(st: &AppState, auth: &AuthUser, args: &Value) -> Result<Value> {
    require_write(auth)?;
    only_known(args, &["site", "key", "confirm"])?;
    require_confirm(args, "a feature flag")?;
    let site = site_with(st, auth, args, MEMBER).await?;
    let flag = flag_on(st, &site, args).await?;
    let mut done = flag_routes::delete(st, auth, &site, &flag).await?;
    done["key"] = json!(flag.key);
    Ok(answer(done, &site))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_describes_itself_and_requires_what_it_needs() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "list_flags",
                "create_flag",
                "update_flag",
                "set_flag_enabled",
                "delete_flag"
            ]
        );
        for tool in &tools {
            assert!(tool["description"].as_str().is_some_and(|d| d.len() > 80));
            let required = tool["inputSchema"]["required"].as_array().unwrap();
            assert!(required.contains(&json!("site")));
            assert_eq!(tool["inputSchema"]["additionalProperties"], json!(false));
        }
        let delete = tools.iter().find(|t| t["name"] == "delete_flag").unwrap();
        assert!(delete["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("confirm")));
        assert_eq!(delete["annotations"]["destructiveHint"], json!(true));
    }

    #[test]
    fn a_delete_without_confirm_true_is_refused() {
        for args in [
            json!({ "site": 1, "key": "k" }),
            json!({ "site": 1, "key": "k", "confirm": false }),
            json!({ "site": 1, "key": "k", "confirm": "true" }),
            json!({ "site": 1, "key": "k", "confirm": 1 }),
        ] {
            let e = require_confirm(&args, "a feature flag").unwrap_err();
            assert_eq!(e.field(), Some("confirm"), "{args}");
        }
        assert!(require_confirm(&json!({ "confirm": true }), "a feature flag").is_ok());
    }

    #[test]
    fn a_misspelt_argument_is_named_rather_than_ignored() {
        let e = flag_input(&json!({ "site": 1, "key": "k", "rollout": 50 }), false).unwrap_err();
        assert_eq!(e.field(), Some("rollout"));
        // `rename_to` belongs to update, `template` to create.
        assert!(flag_input(&json!({ "site": 1, "key": "k", "rename_to": "x" }), false).is_err());
        assert!(flag_input(
            &json!({ "site": 1, "key": "k", "template": "ab-test" }),
            true
        )
        .is_err());
        assert!(only_known(&json!("nope"), &["site"]).is_err());
    }

    #[test]
    fn arguments_become_the_routes_own_input() {
        // A wrongly typed field is refused before anything is read.
        let e = flag_input(
            &json!({ "site": 1, "key": "k", "rollout_pct": "half" }),
            false,
        )
        .unwrap_err();
        assert_eq!(e.field(), Some("arguments"));
        assert!(flag_input(
            &json!({ "site": 1, "key": "new-nav", "rollout_pct": 25, "variants": [], "enabled": null }),
            false
        )
        .is_ok());
        assert!(flag_input(&json!({ "site": 1, "key": "k", "rename_to": "k2" }), true).is_ok());
    }

    #[test]
    fn a_write_needs_the_manage_write_scope() {
        use crate::extract::{AuthMethod, KeyGrant};
        let person = |scopes: &[&str]| AuthUser {
            user: micaforge_store::models::users::User {
                id: uuid::Uuid::from_u128(1),
                email: "owner@example.com".into(),
                name: "Owner".into(),
                avatar_url: None,
                email_verified_at: None,
                totp_secret_sealed: None,
                created_at: chrono::Utc::now(),
            },
            method: AuthMethod::ApiKey(uuid::Uuid::from_u128(9)),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            key: Some(KeyGrant {
                key_id: uuid::Uuid::from_u128(9),
                org_id: uuid::Uuid::from_u128(2),
                site_id: None,
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
            }),
        };
        let reader = person(&["stats:read"]);
        assert!(matches!(require_write(&reader), Err(Error::Forbidden(_))));
        assert!(require_write(&person(&["stats:read", "manage:write"])).is_ok());
    }
}

/// The flag and link write tools through the whole router, on real
/// Postgres and Valkey: scope refusal, the confirm rule, a site-bound key's
/// containment, and the audit trail with the MCP actor. Opt in with
/// `MICAFORGE_TEST_DATABASE_URL` and `MICAFORGE_TEST_REDIS_URL` and
/// `cargo test -p micaforge-server -- --ignored`.
#[cfg(test)]
mod live {
    use axum::body::Body;
    use axum::http::Request;
    use micaforge_core::{crypto, new_id};
    use micaforge_store::models::{keys, sites};
    use serde_json::{json, Value};
    use uuid::Uuid;

    use crate::routes::oauth::harness::Harness;

    /// An API key on `org` with `scopes`, optionally bound to one site.
    async fn key(h: &Harness, org: Uuid, site: Option<Uuid>, scopes: &[&str]) -> String {
        let token = format!("mfg_{}", crypto::random_token(32));
        let prefix: String = token.chars().take(16).collect();
        keys::create(
            &h.st.pg,
            new_id(),
            org,
            site,
            "agent",
            &prefix,
            &crypto::token_hash(&token),
            &json!(scopes),
            None,
        )
        .await
        .unwrap();
        token
    }

    /// One `tools/call`, answered as (result payload, error object).
    async fn call(
        h: &Harness,
        token: &str,
        tool: &str,
        args: Value,
    ) -> (Option<Value>, Option<Value>) {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": tool, "arguments": args } });
        let (status, _, reply) = h
            .send(
                Request::post("/api/mcp")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(status, 200, "{reply}");
        let result = reply["result"]["content"][0]["text"]
            .as_str()
            .map(|t| serde_json::from_str::<Value>(t).unwrap());
        (result, reply.get("error").cloned())
    }

    #[tokio::test]
    #[ignore = "needs Postgres and Valkey"]
    async fn flag_and_link_writes_hold_to_scope_confirm_and_audit() {
        let h = Harness::new().await;
        let owner = h.person("owner@example.com").await;
        let site = sites::create(&h.st.pg, new_id(), owner.org, "example.com", "Docs", "UTC")
            .await
            .unwrap();
        let other = sites::create(
            &h.st.pg,
            new_id(),
            owner.org,
            "other.example",
            "Other",
            "UTC",
        )
        .await
        .unwrap();
        let n = site.site_id_u16();
        let reader = key(&h, owner.org, None, &["stats:read"]).await;
        let writer = key(&h, owner.org, None, &["stats:read", "manage:write"]).await;
        let bound = key(
            &h,
            owner.org,
            Some(other.id),
            &["stats:read", "manage:write"],
        )
        .await;

        // A key without manage:write is refused, and nothing is written.
        let (_, err) = call(
            &h,
            &reader,
            "create_flag",
            json!({ "site": n, "key": "new-nav" }),
        )
        .await;
        assert_eq!(err.unwrap()["data"]["code"], "forbidden");

        // A key bound to another site cannot see this one.
        let (_, err) = call(
            &h,
            &bound,
            "create_flag",
            json!({ "site": n, "key": "new-nav" }),
        )
        .await;
        assert_eq!(err.unwrap()["data"]["code"], "not_found");

        let (made, err) = call(
            &h,
            &writer,
            "create_flag",
            json!({ "site": n, "key": "new-nav", "rollout_pct": 10 }),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(made.unwrap()["rollout_pct"], 10);

        let (_, err) = call(
            &h,
            &writer,
            "update_flag",
            json!({ "site": n, "key": "new-nav", "rollout_pct": 50 }),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        let (off, _) = call(
            &h,
            &writer,
            "set_flag_enabled",
            json!({ "site": n, "key": "new-nav", "enabled": false }),
        )
        .await;
        assert_eq!(off.unwrap()["enabled"], false);

        // Deleting needs confirm: true.
        let (_, err) = call(
            &h,
            &writer,
            "delete_flag",
            json!({ "site": n, "key": "new-nav" }),
        )
        .await;
        assert_eq!(err.unwrap()["data"]["field"], "confirm");
        let (gone, err) = call(
            &h,
            &writer,
            "delete_flag",
            json!({ "site": n, "key": "new-nav", "confirm": true }),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        assert_eq!(gone.unwrap()["key"], "new-nav");

        // Links: create, update, delete with confirm.
        let (link, err) = call(
            &h,
            &writer,
            "create_link",
            json!({ "site": n, "target_url": "https://example.com/launch", "slug": "launch-mcp",
                    "utm": { "source": "newsletter" } }),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        let id = link.unwrap()["id"].as_str().unwrap().to_string();
        let (changed, err) = call(
            &h,
            &writer,
            "update_link",
            json!({ "site": n, "link": id, "title": "Launch" }),
        )
        .await;
        assert!(err.is_none(), "{err:?}");
        let changed = changed.unwrap();
        assert_eq!(changed["title"], "Launch");
        assert_eq!(changed["slug"], "launch-mcp", "fields not sent are kept");
        let (_, err) = call(&h, &writer, "delete_link", json!({ "site": n, "link": id })).await;
        assert_eq!(err.unwrap()["data"]["field"], "confirm");
        let (_, err) = call(
            &h,
            &writer,
            "delete_link",
            json!({ "site": n, "link": id, "confirm": true }),
        )
        .await;
        assert!(err.is_none(), "{err:?}");

        // Every write is in the audit log, by the key, through MCP.
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT action, actor_kind, actor_label FROM audit_events WHERE site_id = $1 ORDER BY created_at",
        )
        .bind(site.id)
        .fetch_all(&h.st.pg)
        .await
        .unwrap();
        let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            actions,
            [
                "flag.created",
                "flag.updated",
                "flag.toggled",
                "flag.deleted",
                "link.created",
                "link.updated",
                "link.deleted"
            ]
        );
        assert!(
            rows.iter()
                .all(|r| r.1 == "api_key" && r.2.ends_with("via MCP")),
            "{rows:?}"
        );
        h.finish().await;
    }
}
