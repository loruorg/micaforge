//! `/api/flags/*`: feature flags and everything around them.
//!
//! A **flag** is a runtime switch: is this on, for whom, at what percentage,
//! in which environment, and only when which other flags are on. This module
//! is the management side: the definitions, the target groups they reach, the
//! templates a new one starts from, every change ever made to one (with
//! revert), the evaluation counts the SDKs report, and the ruleset a server SDK
//! evaluates locally. The public side, the browser's evaluation call and the
//! SDKs' stats batches, is [`super::flag_delivery`].
//!
//! The rule a flag is decided by lives in one place,
//! [`micaforge_core::flags`], so the server, the browser SDK and the Node SDK
//! cannot disagree about a visitor. Everything written here is checked by that
//! module's strict functions before it reaches Postgres.
//!
//! ## Every write is recorded twice
//!
//! In `flag_changes`, in the same transaction as the write, with the whole
//! definition before and after: that is the history the flag screen draws a
//! diff from and reverts to. And in the audit log, through
//! [`crate::audit::record`], which keeps the record after the flag (and its
//! history) has been deleted.
//!
//! ## Dependencies are a graph, and it stays acyclic
//!
//! A flag may name prerequisite flags on the same site. A save that would let
//! a flag depend on itself through any chain is refused with the chain
//! spelled out, a flag other flags depend on cannot be deleted or have its key
//! changed while they do, and the evaluator still refuses a cycle at run time
//! for a row somebody wrote by hand.

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::flags::{check_conditions, check_dependencies, check_environments, find_cycle};
use micaforge_core::{new_id, Error, Result};
use micaforge_store::models::flag_rules::{self, ChangeActor, FlagChange, NewChange, TargetGroup};
use micaforge_store::models::flags::{self, FeatureFlag, FlagFields};
use micaforge_store::models::sites::Site;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::audit::{self, Actor, Entry};
use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::experiments::{check_variants, clean_key, reachable};
use crate::routes::orgs::{clean_name, require_at_least, MEMBER, VIEWER};
use crate::state::AppState;

/// What every handler in this module returns.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// How long the ruleset is cached in Valkey, and how long the SDKs are told
/// they may hold an answer.
///
/// Thirty seconds. Long enough that a busy site does not read Postgres on
/// every page load, short enough that turning a flag off feels immediate.
/// Every write drops the cache anyway, so the TTL only bounds the staleness of
/// a change made by another replica.
pub(crate) const RULESET_TTL_SECS: u32 = 30;

/// Longest description, in characters.
const MAX_DESCRIPTION: usize = 1_000;

/// Most target groups one flag may reach.
const MAX_TARGET_GROUPS: usize = 16;

/// Most changes the history screen reads.
const HISTORY_LIMIT: i64 = 200;

/// The fields a change snapshot and a diff cover, in the order a diff lists
/// them.
const SNAPSHOT_FIELDS: &[&str] = &[
    "key",
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

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every flag management route. Paths are absolute.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/flags", get(list_flags).post(create_flag))
        .route("/api/flags/templates", get(templates))
        .route("/api/flags/ruleset", get(ruleset))
        .route("/api/flags/groups", get(list_groups).post(create_group))
        .route(
            "/api/flags/groups/{group}",
            patch(update_group).delete(delete_group),
        )
        .route(
            "/api/flags/{id}",
            get(get_flag).patch(update_flag).delete(delete_flag),
        )
        .route("/api/flags/{id}/toggle", post(toggle_flag))
        .route("/api/flags/{id}/history", get(history))
        .route("/api/flags/{id}/revert", post(revert))
        .route("/api/flags/{id}/stats", get(stats))
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

/// The rollout patterns a new flag can start from.
///
/// Each one is only a starting point: the editor shows the filled-in fields
/// and every one of them can be changed before the flag is saved.
fn template_list() -> Vec<Value> {
    vec![
        json!({
            "id": "gradual-rollout",
            "name": "Gradual rollout",
            "description": "Start with a small share of visitors and raise it as you gain confidence. Every visitor keeps the answer they first got as the share grows.",
            "flag": {
                "enabled": true,
                "rollout_pct": 5,
                "description": "Rolling out step by step: 5%, then 25%, 50% and 100%.",
            },
        }),
        json!({
            "id": "beta-users",
            "name": "Beta users",
            "description": "On only for visitors your code marks as beta testers. Send `beta: true` in the SDK context, or attach a target group to reuse one audience across flags.",
            "flag": {
                "enabled": true,
                "rollout_pct": 100,
                "description": "On for beta users only.",
                "conditions": {
                    "logic": "and",
                    "filters": [{ "dimension": "beta", "op": "is", "value": "true" }],
                },
            },
        }),
        json!({
            "id": "kill-switch",
            "name": "Kill switch",
            "description": "On for everyone. Switch it off to disable the feature at once, without a deploy.",
            "flag": {
                "enabled": true,
                "rollout_pct": 100,
                "description": "Turn this off to disable the feature immediately.",
            },
        }),
        json!({
            "id": "ab-test",
            "name": "A/B test",
            "description": "Splits visitors evenly between two variants. Pair it with an experiment so a goal decides which one wins.",
            "flag": {
                "enabled": true,
                "rollout_pct": 100,
                "description": "Even split between control and treatment.",
                "variants": [
                    { "key": "control", "weight": 50 },
                    { "key": "treatment", "weight": 50 },
                ],
            },
        }),
    ]
}

/// `GET /api/flags/templates`
async fn templates(_auth: AuthUser) -> ApiResult<Json<Value>> {
    Ok(ok(template_list()))
}

/// A template's starting fields, by id.
fn template_fields(id: &str) -> Result<Map<String, Value>> {
    template_list()
        .into_iter()
        .find(|t| t["id"] == json!(id))
        .and_then(|t| t["flag"].as_object().cloned())
        .ok_or_else(|| {
            Error::validation(
                "template",
                format!("`{id}` is not a template; ask GET /api/flags/templates for the list"),
            )
        })
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// `GET /api/flags?site=…`
async fn list_flags(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = flags::list_flags(&st.pg, access.site.id).await?;
    let latest: BTreeMap<Uuid, FlagChange> = flag_rules::latest_changes(&st.pg, access.site.id)
        .await?
        .into_iter()
        .map(|c| (c.flag_id, c))
        .collect();
    Ok(ok(rows
        .iter()
        .map(|f| flag_json(f, latest.get(&f.id)))
        .collect::<Vec<_>>()))
}

/// A flag as the editor or an API client sends it. Every field is optional on
/// `PATCH`; on `POST` only `key` is required, and `template` fills the rest.
/// The MCP flag tools build one from their arguments.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct FlagInput {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    rollout_pct: Option<i64>,
    #[serde(default)]
    variants: Option<Value>,
    #[serde(default)]
    conditions: Option<Value>,
    #[serde(default)]
    target_groups: Option<Value>,
    #[serde(default)]
    dependencies: Option<Value>,
    #[serde(default)]
    environments: Option<Value>,
    #[serde(default)]
    template: Option<String>,
}

impl FlagInput {
    /// The fields this input sets, laid over `base`. Nothing is validated yet.
    fn over(self, base: FlagFields) -> Result<FlagFields> {
        let rollout = match self.rollout_pct {
            Some(pct) if (0..=100).contains(&pct) => pct as i32,
            Some(_) => {
                return Err(Error::validation(
                    "rollout_pct",
                    "must be a whole number from 0 to 100",
                ))
            }
            None => base.rollout_pct,
        };
        Ok(FlagFields {
            key: self.key.unwrap_or(base.key),
            name: self.name.unwrap_or(base.name),
            description: self.description.unwrap_or(base.description),
            enabled: self.enabled.unwrap_or(base.enabled),
            rollout_pct: rollout,
            variants: self.variants.unwrap_or(base.variants),
            conditions: self.conditions.unwrap_or(base.conditions),
            target_groups: self.target_groups.unwrap_or(base.target_groups),
            dependencies: self.dependencies.unwrap_or(base.dependencies),
            environments: self.environments.unwrap_or(base.environments),
        })
    }
}

/// What a flag is before anything is said about it: on, for everyone, with no
/// variants or targeting. An API client that sends only a key gets a working
/// switch.
fn blank_fields() -> FlagFields {
    FlagFields {
        key: String::new(),
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

/// `POST /api/flags?site=…`
async fn create_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(body): Json<FlagInput>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    Ok(ok(create(&st, &auth, &access.site, body).await?))
}

/// Create a flag on `site`. The caller has already asserted `member` there.
///
/// The one write path for the route and the MCP `create_flag` tool, so both
/// check the definition, record the change and audit it the same way.
pub(crate) async fn create(
    st: &AppState,
    auth: &AuthUser,
    site: &Site,
    body: FlagInput,
) -> Result<Value> {
    if body.key.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err(Error::validation("key", "is required"));
    }

    let mut base = blank_fields();
    if let Some(template) = body.template.as_deref() {
        let preset = template_fields(template.trim())?;
        base = FlagInput {
            description: preset
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            enabled: preset.get("enabled").and_then(Value::as_bool),
            rollout_pct: preset.get("rollout_pct").and_then(Value::as_i64),
            variants: preset.get("variants").cloned(),
            conditions: preset.get("conditions").cloned(),
            ..FlagInput::default()
        }
        .over(base)?;
    }
    let fields = check_fields(st, site, body.over(base)?, None).await?;

    if flags::get_flag_by_key(&st.pg, site.id, &fields.key)
        .await?
        .is_some()
    {
        return Err(Error::Conflict(format!(
            "a flag named `{}` already exists",
            fields.key
        )));
    }

    let actor = Actor::of(st, auth).await;
    let snapshot = snapshot(&fields);
    let mut tx = st.pg.begin().await?;
    let flag = flags::create_flag(&mut *tx, new_id(), site.id, &fields).await?;
    flag_rules::record_change(
        &mut *tx,
        new_id(),
        &NewChange {
            flag_id: flag.id,
            site_id: flag.site_id,
            action: "created",
            actor: &change_actor(&actor),
            before: None,
            after: Some(&snapshot),
        },
    )
    .await?;
    tx.commit().await?;

    invalidate(st, site.site_id_u16()).await;
    audit::record_as(
        st,
        actor,
        Entry::new("flag.created")
            .target("flag", flag.id, &flag.key)
            .site(site)
            .after(snapshot),
    )
    .await;
    Ok(flag_json(&flag, None))
}

/// `GET /api/flags/{id}`
async fn get_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    reachable(&st, flag.site_id, auth.user.id, VIEWER, "feature flag").await?;
    let latest = flag_rules::list_changes(&st.pg, flag.id, Some(1)).await?;
    Ok(ok(flag_json(&flag, latest.first())))
}

/// `PATCH /api/flags/{id}`
async fn update_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<FlagInput>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;
    Ok(ok(update(&st, &auth, &site, &flag, body).await?))
}

/// Change the fields `body` sets on `flag`. The caller has already asserted
/// `member` on its site. The one write path for the route and the MCP
/// `update_flag` tool.
pub(crate) async fn update(
    st: &AppState,
    auth: &AuthUser,
    site: &Site,
    flag: &FeatureFlag,
    body: FlagInput,
) -> Result<Value> {
    if body.template.is_some() {
        return Err(Error::validation(
            "template",
            "a template only starts a new flag; send the fields to change instead",
        ));
    }
    let fields = check_fields(st, site, body.over(FlagFields::of(flag))?, Some(flag)).await?;
    write_flag(st, auth, site, flag, fields, "updated", "flag.updated").await
}

/// The one field a kill switch changes.
#[derive(Debug, Deserialize)]
struct ToggleBody {
    enabled: bool,
}

/// `POST /api/flags/{id}/toggle` — flip it without touching anything else.
///
/// A separate endpoint from `PATCH` on purpose. Turning a flag off in an
/// incident must not require sending back the rollout and conditions, because
/// that is how somebody at three in the morning accidentally re-enables the
/// thing they were trying to switch off.
async fn toggle_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ToggleBody>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;
    Ok(ok(
        set_enabled(&st, &auth, &site, &flag, body.enabled).await?
    ))
}

/// Switch `flag` on or off and touch nothing else. The caller has already
/// asserted `member` on its site. The one write path for the route and the
/// MCP `set_flag_enabled` tool.
pub(crate) async fn set_enabled(
    st: &AppState,
    auth: &AuthUser,
    site: &Site,
    flag: &FeatureFlag,
    enabled: bool,
) -> Result<Value> {
    let before = snapshot(&FlagFields::of(flag));
    let actor = Actor::of(st, auth).await;
    let mut tx = st.pg.begin().await?;
    let updated = flags::set_flag_enabled(&mut *tx, flag.id, enabled).await?;
    let after = snapshot(&FlagFields::of(&updated));
    flag_rules::record_change(
        &mut *tx,
        new_id(),
        &NewChange {
            flag_id: flag.id,
            site_id: flag.site_id,
            action: "toggled",
            actor: &change_actor(&actor),
            before: Some(&before),
            after: Some(&after),
        },
    )
    .await?;
    tx.commit().await?;

    invalidate(st, site.site_id_u16()).await;
    audit::record_as(
        st,
        actor,
        Entry::new("flag.toggled")
            .target("flag", flag.id, &flag.key)
            .site(site)
            .before(json!({ "enabled": flag.enabled }))
            .after(json!({ "enabled": updated.enabled })),
    )
    .await;
    tracing::info!(flag = %flag.id, key = %flag.key, enabled, "flag toggled");
    Ok(flag_json(&updated, None))
}

/// `DELETE /api/flags/{id}`
///
/// Refused while another flag names this one as a prerequisite: deleting it
/// would silently switch every dependent off, which is a change to flags the
/// caller did not ask to change.
async fn delete_flag(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;
    Ok(ok(delete(&st, &auth, &site, &flag).await?))
}

/// Delete `flag`, unless another flag depends on it. The caller has already
/// asserted `member` on its site. The one write path for the route and the
/// MCP `delete_flag` tool.
pub(crate) async fn delete(
    st: &AppState,
    auth: &AuthUser,
    site: &Site,
    flag: &FeatureFlag,
) -> Result<Value> {
    let dependents = flags::list_dependents(&st.pg, site.id, &flag.key).await?;
    if !dependents.is_empty() {
        let names: Vec<&str> = dependents.iter().map(|f| f.key.as_str()).collect();
        return Err(Error::Conflict(format!(
            "`{}` is a prerequisite of {}; remove it from them first",
            flag.key,
            names.join(", ")
        )));
    }

    flags::delete_flag(&st.pg, flag.id).await?;
    invalidate(st, site.site_id_u16()).await;
    audit::record(
        st,
        auth,
        Entry::new("flag.deleted")
            .target("flag", flag.id, &flag.key)
            .site(site)
            .before(snapshot(&FlagFields::of(flag))),
    )
    .await;
    Ok(json!({ "deleted": flag.id }))
}

// ---------------------------------------------------------------------------
// History and revert
// ---------------------------------------------------------------------------

/// `GET /api/flags/{id}/history` — every change, newest first.
async fn history(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    reachable(&st, flag.site_id, auth.user.id, VIEWER, "feature flag").await?;
    let changes = flag_rules::list_changes(&st.pg, flag.id, Some(HISTORY_LIMIT)).await?;
    Ok(ok(changes.iter().map(change_json).collect::<Vec<_>>()))
}

/// Which change to undo.
#[derive(Debug, Deserialize)]
struct RevertBody {
    change_id: Uuid,
}

/// `POST /api/flags/{id}/revert` — put back the state a change replaced.
///
/// The restored definition goes through the same checks as an edit: a target
/// group deleted since is dropped from it (it would match nobody), and a
/// prerequisite that no longer exists, or a key another flag has taken since,
/// refuses the revert with the reason rather than restoring something that
/// would not evaluate the way it did then.
async fn revert(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RevertBody>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, MEMBER, "feature flag").await?;

    let change = flag_rules::get_change(&st.pg, body.change_id).await?;
    if change.flag_id != flag.id {
        return Err(Error::not_found("flag change").into());
    }
    let Some(before) = change.before.as_ref() else {
        return Err(Error::Conflict(
            "that change created the flag; there is no earlier state to put back".into(),
        )
        .into());
    };

    let mut restored = fields_from_snapshot(before, &flag);
    let live_groups: BTreeSet<String> = flag_rules::list_groups(&st.pg, site.id)
        .await?
        .into_iter()
        .map(|g| g.id.to_string())
        .collect();
    restored.target_groups = Value::Array(
        restored
            .target_groups
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter(|id| id.as_str().is_some_and(|s| live_groups.contains(s)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
    );

    let fields = check_fields(&st, &site, restored, Some(&flag)).await?;
    let updated = write_flag(
        &st,
        &auth,
        &site,
        &flag,
        fields,
        "reverted",
        "flag.reverted",
    )
    .await?;
    Ok(ok(updated))
}

/// Write an edited definition, record the change, drop the cache and audit.
async fn write_flag(
    st: &AppState,
    auth: &AuthUser,
    site: &Site,
    flag: &FeatureFlag,
    fields: FlagFields,
    change: &str,
    action: &'static str,
) -> Result<Value> {
    if fields.key != flag.key {
        if let Some(other) = flags::get_flag_by_key(&st.pg, site.id, &fields.key).await? {
            if other.id != flag.id {
                return Err(Error::Conflict(format!(
                    "a flag named `{}` already exists",
                    fields.key
                )));
            }
        }
        let dependents = flags::list_dependents(&st.pg, site.id, &flag.key).await?;
        if !dependents.is_empty() {
            let names: Vec<&str> = dependents.iter().map(|f| f.key.as_str()).collect();
            return Err(Error::Conflict(format!(
                "`{}` is a prerequisite of {}, which name it by key; remove it from them before renaming it",
                flag.key,
                names.join(", ")
            )));
        }
    }

    let before = snapshot(&FlagFields::of(flag));
    let after = snapshot(&fields);
    let actor = Actor::of(st, auth).await;
    let mut tx = st.pg.begin().await?;
    let updated = flags::update_flag(&mut *tx, flag.id, &fields).await?;
    flag_rules::record_change(
        &mut *tx,
        new_id(),
        &NewChange {
            flag_id: flag.id,
            site_id: flag.site_id,
            action: change,
            actor: &change_actor(&actor),
            before: Some(&before),
            after: Some(&after),
        },
    )
    .await?;
    tx.commit().await?;

    invalidate(st, site.site_id_u16()).await;
    audit::record_as(
        st,
        actor,
        Entry::new(action)
            .target("flag", flag.id, &updated.key)
            .site(site)
            .before(before)
            .after(after),
    )
    .await;
    Ok(flag_json(&updated, None))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Check a whole definition against the rule, the site, and the other flags.
///
/// `existing` is the flag being edited, `None` for a new one.
async fn check_fields(
    st: &AppState,
    site: &Site,
    fields: FlagFields,
    existing: Option<&FeatureFlag>,
) -> Result<FlagFields> {
    let key = clean_key(&fields.key, "key")?;
    let name = fields.name.trim().to_string();
    if name.chars().count() > 120 {
        return Err(Error::validation(
            "name",
            "cannot be longer than 120 characters",
        ));
    }
    let description = fields.description.trim().to_string();
    if description.chars().count() > MAX_DESCRIPTION {
        return Err(Error::validation(
            "description",
            format!("cannot be longer than {MAX_DESCRIPTION} characters"),
        ));
    }
    if !(0..=100).contains(&fields.rollout_pct) {
        return Err(Error::validation(
            "rollout_pct",
            "must be a whole number from 0 to 100",
        ));
    }
    check_variants(&fields.variants, false)?;
    let conditions = check_conditions(&fields.conditions, "conditions")?;
    let environments = check_environments(&fields.environments)?;
    let dependencies = check_dependencies(&fields.dependencies, &key)?;
    let target_groups = check_target_groups(st, site, &fields.target_groups).await?;

    // Every prerequisite must exist on this site, and the graph must stay
    // acyclic with this flag's new edges in it.
    let others = flags::list_flags(&st.pg, site.id).await?;
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for other in &others {
        if existing.is_some_and(|e| e.id == other.id) {
            continue;
        }
        edges.insert(other.key.clone(), dependency_keys(&other.dependencies));
    }
    for wanted in dependency_keys(&dependencies) {
        if !edges.contains_key(&wanted) {
            return Err(Error::validation(
                "dependencies",
                format!("there is no flag named `{wanted}` on this site"),
            ));
        }
    }
    edges.insert(key.clone(), dependency_keys(&dependencies));
    if let Some(cycle) = find_cycle(&edges, &key) {
        return Err(Error::validation(
            "dependencies",
            format!(
                "these flags would depend on each other in a loop: {}",
                cycle.join(" -> ")
            ),
        ));
    }

    Ok(FlagFields {
        key,
        name,
        description,
        enabled: fields.enabled,
        rollout_pct: fields.rollout_pct,
        variants: fields.variants,
        conditions,
        target_groups,
        dependencies,
        environments,
    })
}

/// The keys a stored or checked dependency list names.
fn dependency_keys(dependencies: &Value) -> Vec<String> {
    dependencies
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|d| d.get("flag").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Validate target group ids: a list of ids of this site's groups, no repeats.
async fn check_target_groups(st: &AppState, site: &Site, raw: &Value) -> Result<Value> {
    let list = match raw {
        Value::Null => return Ok(json!([])),
        Value::Array(list) => list,
        _ => {
            return Err(Error::validation(
                "target_groups",
                "must be a list of group ids",
            ))
        }
    };
    if list.len() > MAX_TARGET_GROUPS {
        return Err(Error::validation(
            "target_groups",
            format!("a flag can reach at most {MAX_TARGET_GROUPS} target groups"),
        ));
    }
    if list.is_empty() {
        return Ok(json!([]));
    }
    let known: BTreeSet<Uuid> = flag_rules::list_groups(&st.pg, site.id)
        .await?
        .into_iter()
        .map(|g| g.id)
        .collect();
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    for entry in list {
        let id = entry
            .as_str()
            .and_then(|s| s.trim().parse::<Uuid>().ok())
            .ok_or_else(|| Error::validation("target_groups", "every entry must be a group id"))?;
        if !known.contains(&id) {
            return Err(Error::validation(
                "target_groups",
                "names a target group that is not on this site",
            ));
        }
        let id = id.to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(json!(out))
}

// ---------------------------------------------------------------------------
// Target groups
// ---------------------------------------------------------------------------

/// `GET /api/flags/groups?site=…`
async fn list_groups(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let groups = flag_rules::list_groups(&st.pg, access.site.id).await?;
    let site_flags = flags::list_flags(&st.pg, access.site.id).await?;
    Ok(ok(groups
        .iter()
        .map(|g| group_json(g, &site_flags))
        .collect::<Vec<_>>()))
}

/// A target group, as the editor sends it.
#[derive(Debug, Deserialize)]
struct GroupBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    conditions: Option<Value>,
}

/// `POST /api/flags/groups?site=…`
async fn create_group(
    State(st): State<AppState>,
    auth: AuthUser,
    access: SiteAccess,
    Json(body): Json<GroupBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let name = clean_name(body.name.as_deref().unwrap_or(""), "name")?;
    let description = clean_description(body.description.as_deref().unwrap_or(""))?;
    let conditions = check_conditions(
        body.conditions.as_ref().unwrap_or(&Value::Null),
        "conditions",
    )?;

    let group = match flag_rules::create_group(
        &st.pg,
        new_id(),
        access.site.id,
        &name,
        &description,
        &conditions,
    )
    .await
    {
        Ok(group) => group,
        Err(Error::Conflict(_)) => {
            return Err(Error::Conflict(format!(
                "a target group called `{name}` already exists on this site"
            ))
            .into())
        }
        Err(e) => return Err(e.into()),
    };

    invalidate(&st, access.site_id).await;
    audit::record(
        &st,
        &auth,
        Entry::new("target_group.created")
            .target("target_group", group.id, &group.name)
            .site(&access.site)
            .after(json!({ "name": group.name, "description": group.description, "conditions": group.conditions })),
    )
    .await;
    Ok(ok(group_json(&group, &[])))
}

/// Load a group and assert the caller's role on its site.
async fn reachable_group(
    st: &AppState,
    auth: &AuthUser,
    id: Uuid,
    needed: &str,
) -> Result<(TargetGroup, Site)> {
    let group = flag_rules::get_group(&st.pg, id).await?;
    let site = reachable(st, group.site_id, auth.user.id, needed, "target group").await?;
    Ok((group, site))
}

/// `PATCH /api/flags/groups/{group}`
async fn update_group(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<GroupBody>,
) -> ApiResult<Json<Value>> {
    let (group, site) = reachable_group(&st, &auth, id, MEMBER).await?;
    let name = match body.name.as_deref() {
        Some(raw) => clean_name(raw, "name")?,
        None => group.name.clone(),
    };
    let description = match body.description.as_deref() {
        Some(raw) => clean_description(raw)?,
        None => group.description.clone(),
    };
    let conditions = match body.conditions.as_ref() {
        Some(raw) => check_conditions(raw, "conditions")?,
        None => group.conditions.clone(),
    };

    let updated =
        match flag_rules::update_group(&st.pg, group.id, &name, &description, &conditions).await {
            Ok(updated) => updated,
            Err(Error::Conflict(_)) => {
                return Err(Error::Conflict(format!(
                    "a target group called `{name}` already exists on this site"
                ))
                .into())
            }
            Err(e) => return Err(e.into()),
        };

    invalidate(&st, site.site_id_u16()).await;
    audit::record(
        &st,
        &auth,
        Entry::new("target_group.updated")
            .target("target_group", group.id, &updated.name)
            .site(&site)
            .before(json!({ "name": group.name, "description": group.description, "conditions": group.conditions }))
            .after(json!({ "name": updated.name, "description": updated.description, "conditions": updated.conditions })),
    )
    .await;
    let site_flags = flags::list_flags(&st.pg, site.id).await?;
    Ok(ok(group_json(&updated, &site_flags)))
}

/// `DELETE /api/flags/groups/{group}`
///
/// Detaches the group from every flag that reaches it first, each through the
/// flag write path, so every flag's history shows the detachment. A flag left
/// with no target group reaches everyone its own conditions allow, which is
/// what its history and the response say.
async fn delete_group(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (group, site) = reachable_group(&st, &auth, id, MEMBER).await?;
    let attached = flags::list_with_group(&st.pg, site.id, group.id).await?;
    let gid = group.id.to_string();

    let mut detached = Vec::with_capacity(attached.len());
    for flag in &attached {
        let mut fields = FlagFields::of(flag);
        fields.target_groups = Value::Array(
            fields
                .target_groups
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter(|v| v.as_str() != Some(gid.as_str()))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        );
        write_flag(&st, &auth, &site, flag, fields, "updated", "flag.updated").await?;
        detached.push(flag.key.clone());
    }

    flag_rules::delete_group(&st.pg, group.id).await?;
    invalidate(&st, site.site_id_u16()).await;
    audit::record(
        &st,
        &auth,
        Entry::new("target_group.deleted")
            .target("target_group", group.id, &group.name)
            .site(&site)
            .before(json!({ "name": group.name, "description": group.description, "conditions": group.conditions }))
            .meta(json!({ "detached_from": detached })),
    )
    .await;
    Ok(ok(
        json!({ "deleted": group.id, "detached_from": detached }),
    ))
}

/// Bound a description.
fn clean_description(raw: &str) -> Result<String> {
    let text = raw.trim();
    if text.chars().count() > MAX_DESCRIPTION {
        return Err(Error::validation(
            "description",
            format!("cannot be longer than {MAX_DESCRIPTION} characters"),
        ));
    }
    Ok(text.to_string())
}

// ---------------------------------------------------------------------------
// The ruleset
// ---------------------------------------------------------------------------

/// `GET /api/flags/ruleset?site=…` — everything a server SDK needs to decide
/// every flag on the site itself.
///
/// For a server holding an API key with `manage:read`. The whole definition
/// is in it, conditions included, which is why the browser never gets this
/// and asks [`super::flag_delivery`] for decisions instead. `version` changes
/// whenever anything in the ruleset does, so an SDK can skip a re-parse.
async fn ruleset(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let doc = build_ruleset(&st, access.site.id).await?;
    Ok(ok(json!({
        "site": access.site_id,
        "version": ruleset_version(&doc),
        "ttl_seconds": RULESET_TTL_SECS,
        "ruleset": doc,
    })))
}

/// The ruleset document of one site, from Postgres.
pub(crate) async fn build_ruleset(st: &AppState, site_id: Uuid) -> Result<Value> {
    let rows = flags::list_flags(&st.pg, site_id).await?;
    let groups = flag_rules::list_groups(&st.pg, site_id).await?;
    let flags_doc: Vec<Value> = rows
        .iter()
        .map(|f| {
            json!({
                "key": f.key,
                "enabled": f.enabled,
                "rollout_pct": f.rollout_pct,
                "variants": f.variants,
                "conditions": f.conditions,
                "target_groups": f.target_groups,
                "dependencies": f.dependencies,
                "environments": f.environments,
            })
        })
        .collect();
    let mut groups_doc = Map::new();
    for group in groups {
        groups_doc.insert(
            group.id.to_string(),
            json!({ "conditions": group.conditions }),
        );
    }
    Ok(json!({ "flags": flags_doc, "target_groups": groups_doc }))
}

/// A short digest of a ruleset document, stable for identical content.
fn ruleset_version(doc: &Value) -> String {
    let digest = blake3::hash(doc.to_string().as_bytes());
    digest.to_hex()[..16].to_string()
}

/// `mf:flags:rules:<site>` — the cached ruleset document.
pub(crate) fn ruleset_cache_key(site: u16) -> String {
    format!("mf:flags:rules:{site}")
}

/// Drop a site's cached ruleset after a write.
///
/// Best-effort: the TTL bounds the damage of a failure to
/// [`RULESET_TTL_SECS`], and failing an otherwise successful write because
/// Valkey was unreachable would be the wrong trade.
async fn invalidate(st: &AppState, site: u16) {
    if let Err(e) = st.kv.del(&ruleset_cache_key(site)).await {
        tracing::debug!(site, error = %e, "could not clear the flag ruleset cache");
    }
}

// ---------------------------------------------------------------------------
// Evaluation stats
// ---------------------------------------------------------------------------

/// The window the stats screen asks for.
#[derive(Debug, Deserialize)]
struct StatsQuery {
    #[serde(default)]
    range: Option<String>,
}

/// One bucket of one variant, as ClickHouse reports it.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct StatRow {
    t: u64,
    variant: String,
    evaluations: u64,
    exposures: u64,
}

/// A stats window: its length and the width of one bucket, in seconds.
fn stats_window(range: Option<&str>) -> Result<(i64, i64, &'static str)> {
    const HOUR: i64 = 3_600;
    const DAY: i64 = 86_400;
    Ok(match range.unwrap_or("7d") {
        "24h" => (DAY, HOUR, "hour"),
        "7d" => (7 * DAY, DAY, "day"),
        "30d" => (30 * DAY, DAY, "day"),
        "90d" => (90 * DAY, DAY, "day"),
        other => {
            return Err(Error::validation(
                "range",
                format!("`{other}` is not one of 24h, 7d, 30d, 90d"),
            ))
        }
    })
}

/// `GET /api/flags/{id}/stats?range=…` — evaluations and exposures per
/// variant over time, from what the SDKs reported.
///
/// Buckets are aligned to UTC hours or days and zero-filled, so every series
/// has one value per bucket and a quiet hour reads as zero rather than as a
/// gap the chart would draw a line across.
async fn stats(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(q): Query<StatsQuery>,
) -> ApiResult<Json<Value>> {
    let flag = flags::get_flag(&st.pg, id).await?;
    let site = reachable(&st, flag.site_id, auth.user.id, VIEWER, "feature flag").await?;
    let (span, width, granularity) = stats_window(q.range.as_deref().map(str::trim))?;

    let end = Utc::now();
    let first = (end.timestamp() - span) / width * width + width;
    let start = DateTime::from_timestamp(first, 0).unwrap_or(end);
    let rows = read_stats(&st.ch, site.site_id_u16(), &flag.key, start, width).await?;

    let buckets: Vec<i64> = (0..)
        .map(|i| (first + i * width) * 1000)
        .take_while(|t| *t <= end.timestamp_millis())
        .collect();
    Ok(ok(shape_stats(
        &flag.key,
        start,
        end,
        granularity,
        &buckets,
        &rows,
    )))
}

/// The counts of one flag from `start` on, summed per bucket of `width`
/// seconds (aligned to the Unix epoch, so to UTC) and per variant.
///
/// `width` is formatted into the statement: it is one of this module's own
/// constants from [`stats_window`], never caller text.
async fn read_stats(
    ch: &micaforge_store::Ch,
    site: u16,
    key: &str,
    start: DateTime<Utc>,
    width: i64,
) -> Result<Vec<StatRow>> {
    let sql = format!(
        "SELECT
             toUInt64(intDiv(toUnixTimestamp(timestamp), {width}) * {width}) * 1000 AS t,
             toString(variant) AS variant,
             sumIf(count, kind = 'evaluation') AS evaluations,
             sumIf(count, kind = 'exposure') AS exposures
         FROM flag_events
         WHERE site_id = ? AND flag_key = ?
           AND timestamp >= toDateTime64(?, 3, 'UTC')
         GROUP BY t, variant
         ORDER BY t, variant"
    );
    ch.client()
        .query(&sql)
        .bind(site)
        .bind(key)
        .bind(micaforge_query::sql::fmt_ts(start))
        .fetch_all::<StatRow>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse flag stats: {e}")))
}

/// Zero-fill the rows into one aligned series per variant.
fn shape_stats(
    key: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    granularity: &str,
    buckets: &[i64],
    rows: &[StatRow],
) -> Value {
    let index: BTreeMap<i64, usize> = buckets.iter().enumerate().map(|(i, t)| (*t, i)).collect();
    let mut series: BTreeMap<String, (Vec<u64>, Vec<u64>)> = BTreeMap::new();
    for row in rows {
        let Some(&at) = index.get(&(row.t as i64)) else {
            continue;
        };
        let entry = series
            .entry(row.variant.clone())
            .or_insert_with(|| (vec![0; buckets.len()], vec![0; buckets.len()]));
        entry.0[at] += row.evaluations;
        entry.1[at] += row.exposures;
    }
    let totals: Vec<Value> = series
        .iter()
        .map(|(variant, (e, x))| {
            json!({
                "variant": variant,
                "evaluations": e.iter().sum::<u64>(),
                "exposures": x.iter().sum::<u64>(),
            })
        })
        .collect();
    let series: Vec<Value> = series
        .into_iter()
        .map(|(variant, (e, x))| json!({ "variant": variant, "evaluations": e, "exposures": x }))
        .collect();
    json!({
        "key": key,
        "range": { "start": start, "end": end },
        "granularity": granularity,
        "buckets": buckets,
        "series": series,
        "totals": totals,
    })
}

// ---------------------------------------------------------------------------
// Shaping
// ---------------------------------------------------------------------------

/// The editable definition as one JSON value: what a change stores.
fn snapshot(fields: &FlagFields) -> Value {
    json!({
        "key": fields.key,
        "name": fields.name,
        "description": fields.description,
        "enabled": fields.enabled,
        "rollout_pct": fields.rollout_pct,
        "variants": fields.variants,
        "conditions": fields.conditions,
        "target_groups": fields.target_groups,
        "dependencies": fields.dependencies,
        "environments": fields.environments,
    })
}

/// Read a snapshot back, taking any field it lacks from the flag as it is.
fn fields_from_snapshot(snap: &Value, current: &FeatureFlag) -> FlagFields {
    let now = FlagFields::of(current);
    let text = |name: &str, fallback: String| {
        snap.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(fallback)
    };
    let value = |name: &str, fallback: Value| snap.get(name).cloned().unwrap_or(fallback);
    FlagFields {
        key: text("key", now.key),
        name: text("name", now.name),
        description: text("description", now.description),
        enabled: snap
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(now.enabled),
        rollout_pct: snap
            .get("rollout_pct")
            .and_then(Value::as_i64)
            .map(|p| p.clamp(0, 100) as i32)
            .unwrap_or(now.rollout_pct),
        variants: value("variants", now.variants),
        conditions: value("conditions", now.conditions),
        target_groups: value("target_groups", now.target_groups),
        dependencies: value("dependencies", now.dependencies),
        environments: value("environments", now.environments),
    }
}

/// The fields two snapshots disagree on.
fn diff(before: Option<&Value>, after: Option<&Value>) -> Vec<Value> {
    let empty = Value::Null;
    SNAPSHOT_FIELDS
        .iter()
        .filter_map(|field| {
            let b = before.and_then(|v| v.get(*field)).unwrap_or(&empty);
            let a = after.and_then(|v| v.get(*field)).unwrap_or(&empty);
            (b != a).then(|| json!({ "field": field, "before": b, "after": a }))
        })
        .collect()
}

/// An [`Actor`] as a change row stores it.
fn change_actor(actor: &Actor) -> ChangeActor {
    let (kind, id, label) = actor.parts();
    ChangeActor {
        kind: kind.to_string(),
        id,
        label: label.to_string(),
    }
}

/// One change on the wire.
fn change_json(change: &FlagChange) -> Value {
    json!({
        "id": change.id,
        "flag_id": change.flag_id,
        "action": change.action,
        "actor": { "kind": change.actor_kind, "id": change.actor_id, "label": change.actor_label },
        "created_at": change.created_at,
        "diff": diff(change.before.as_ref(), change.after.as_ref()),
        "before": change.before,
        "after": change.after,
        "revertable": change.before.is_some(),
    })
}

/// One flag on the wire, for an authenticated reader.
pub(crate) fn flag_json(flag: &FeatureFlag, latest: Option<&FlagChange>) -> Value {
    json!({
        "id": flag.id,
        "site_id": flag.site_id,
        "key": flag.key,
        "name": flag.name,
        "description": flag.description,
        "enabled": flag.enabled,
        "rollout_pct": flag.rollout_pct,
        "variants": flag.variants,
        "conditions": flag.conditions,
        "target_groups": flag.target_groups,
        "dependencies": flag.dependencies,
        "environments": flag.environments,
        "created_at": flag.created_at,
        "updated_at": flag.updated_at,
        "updated_by": latest.map(|c| json!({
            "kind": c.actor_kind, "id": c.actor_id, "label": c.actor_label,
        })),
    })
}

/// One target group on the wire, with the keys of the flags that reach it.
fn group_json(group: &TargetGroup, site_flags: &[FeatureFlag]) -> Value {
    let id = group.id.to_string();
    let using: Vec<&str> = site_flags
        .iter()
        .filter(|f| {
            f.target_groups
                .as_array()
                .is_some_and(|ids| ids.iter().any(|v| v.as_str() == Some(id.as_str())))
        })
        .map(|f| f.key.as_str())
        .collect();
    json!({
        "id": group.id,
        "site_id": group.site_id,
        "name": group.name,
        "description": group.description,
        "conditions": group.conditions,
        "flags": using,
        "created_at": group.created_at,
        "updated_at": group.updated_at,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn every_template_is_a_valid_starting_point() {
        for template in template_list() {
            let id = template["id"].as_str().unwrap();
            let preset = template_fields(id).unwrap();
            if let Some(conditions) = preset.get("conditions") {
                assert!(check_conditions(conditions, "conditions").is_ok(), "{id}");
            }
            if let Some(variants) = preset.get("variants") {
                assert!(check_variants(variants, false).is_ok(), "{id}");
            }
            let rollout = preset.get("rollout_pct").and_then(Value::as_i64).unwrap();
            assert!((0..=100).contains(&rollout), "{id}");
        }
        assert!(template_fields("nonsense").is_err());
    }

    #[test]
    fn an_input_lays_only_what_it_sets_over_the_base() {
        let base = blank_fields();
        let fields = FlagInput {
            key: Some("new-nav".into()),
            rollout_pct: Some(25),
            ..FlagInput::default()
        }
        .over(base.clone())
        .unwrap();
        assert_eq!(fields.key, "new-nav");
        assert_eq!(fields.rollout_pct, 25);
        assert_eq!(fields.enabled, base.enabled);
        assert_eq!(fields.variants, base.variants);

        let refused = FlagInput {
            rollout_pct: Some(101),
            ..FlagInput::default()
        }
        .over(base);
        assert_eq!(refused.unwrap_err().field(), Some("rollout_pct"));
    }

    #[test]
    fn a_diff_lists_only_the_fields_that_changed() {
        let mut fields = blank_fields();
        fields.key = "new-nav".into();
        let before = snapshot(&fields);
        fields.rollout_pct = 50;
        fields.environments = json!({ "staging": { "enabled": true } });
        let after = snapshot(&fields);

        let changed = diff(Some(&before), Some(&after));
        let names: Vec<&str> = changed
            .iter()
            .map(|d| d["field"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["rollout_pct", "environments"]);
        assert_eq!(changed[0]["before"], json!(100));
        assert_eq!(changed[0]["after"], json!(50));

        // A creation lists every field it set.
        assert_eq!(diff(None, Some(&after)).len(), SNAPSHOT_FIELDS.len());
    }

    #[test]
    fn stats_windows_are_the_four_the_screen_offers() {
        assert_eq!(stats_window(None).unwrap().2, "day");
        assert_eq!(stats_window(Some("24h")).unwrap(), (86_400, 3_600, "hour"));
        assert!(stats_window(Some("1y")).is_err());
    }

    #[test]
    fn stats_are_zero_filled_per_variant() {
        let buckets = vec![1_000, 2_000, 3_000];
        let rows = vec![
            StatRow {
                t: 1_000,
                variant: "on".into(),
                evaluations: 4,
                exposures: 1,
            },
            StatRow {
                t: 3_000,
                variant: "on".into(),
                evaluations: 2,
                exposures: 0,
            },
            StatRow {
                t: 2_000,
                variant: "off".into(),
                evaluations: 7,
                exposures: 0,
            },
            // A row outside the window is ignored rather than misplaced.
            StatRow {
                t: 9_000,
                variant: "on".into(),
                evaluations: 99,
                exposures: 99,
            },
        ];
        let now = Utc::now();
        let shaped = shape_stats("k", now, now, "hour", &buckets, &rows);
        assert_eq!(shaped["series"][0]["variant"], json!("off"));
        assert_eq!(shaped["series"][0]["evaluations"], json!([0, 7, 0]));
        assert_eq!(shaped["series"][1]["evaluations"], json!([4, 0, 2]));
        assert_eq!(shaped["totals"][1]["evaluations"], json!(6));
        assert_eq!(shaped["totals"][1]["exposures"], json!(1));
    }

    #[test]
    fn a_snapshot_reads_back_into_the_same_fields() {
        let mut fields = blank_fields();
        fields.key = "k".into();
        fields.dependencies = json!([{ "flag": "base", "enabled": true, "variant": null }]);
        let flag = FeatureFlag {
            id: Uuid::nil(),
            site_id: Uuid::nil(),
            key: "other".into(),
            name: String::new(),
            description: String::new(),
            enabled: false,
            rollout_pct: 0,
            variants: json!([]),
            conditions: json!({}),
            target_groups: json!([]),
            dependencies: json!([]),
            environments: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(fields_from_snapshot(&snapshot(&fields), &flag), fields);
        // A field the snapshot lacks comes from the flag as it is now.
        assert_eq!(fields_from_snapshot(&json!({}), &flag).key, "other");
    }

    /// A throwaway database on a real ClickHouse, with the whole schema in it.
    /// Opt in with `MICAFORGE_TEST_CLICKHOUSE_URL=http://user:password@host:8123`
    /// and `cargo test -p micaforge-server -- --ignored`; the database is
    /// dropped by [`drop_clickhouse`].
    pub(crate) async fn live_clickhouse() -> micaforge_store::Ch {
        let raw = std::env::var("MICAFORGE_TEST_CLICKHOUSE_URL")
            .expect("set MICAFORGE_TEST_CLICKHOUSE_URL to run this test");
        let url = url::Url::parse(&raw).expect("a URL");
        let base = format!(
            "{}://{}:{}",
            url.scheme(),
            url.host_str().unwrap(),
            url.port().unwrap_or(8123)
        );
        let db = format!("mf_test_{}", micaforge_core::crypto::numeric_code(9));
        let ch =
            micaforge_store::Ch::connect(&base, &db, url.username(), url.password().unwrap_or(""))
                .unwrap();
        ch.init_schema().await.unwrap();
        ch
    }

    /// Drop a database made by [`live_clickhouse`].
    pub(crate) async fn drop_clickhouse(ch: &micaforge_store::Ch) {
        let _ = ch.exec(&format!("DROP DATABASE IF EXISTS {}", ch.db)).await;
    }

    #[tokio::test]
    #[ignore = "needs a ClickHouse server"]
    async fn stats_read_back_per_variant_in_aligned_buckets() {
        use micaforge_store::ch::FlagEventRow;
        let ch = live_clickhouse().await;
        let now = Utc::now();
        let row = |site: u16, key: &str, variant: &str, kind: &str, count: u32| FlagEventRow {
            site_id: site,
            timestamp: now,
            flag_key: key.into(),
            variant: variant.into(),
            enabled: u8::from(variant != "off"),
            kind: kind.into(),
            source: "browser".into(),
            environment: String::new(),
            count,
        };
        let mut insert = ch.client().insert::<FlagEventRow>("flag_events").unwrap();
        for r in [
            row(9, "hero", "a", "evaluation", 3),
            row(9, "hero", "a", "evaluation", 2),
            row(9, "hero", "b", "exposure", 4),
            row(9, "hero", "off", "evaluation", 1),
            row(9, "other", "a", "evaluation", 50),
            row(8, "hero", "a", "evaluation", 70),
        ] {
            insert.write(&r).await.unwrap();
        }
        insert.end().await.unwrap();

        let rows = read_stats(&ch, 9, "hero", now - chrono::Duration::hours(2), 3_600)
            .await
            .unwrap();
        let sum = |variant: &str, f: fn(&StatRow) -> u64| -> u64 {
            rows.iter().filter(|r| r.variant == variant).map(f).sum()
        };
        assert_eq!(sum("a", |r| r.evaluations), 5);
        assert_eq!(sum("b", |r| r.exposures), 4);
        assert_eq!(sum("off", |r| r.evaluations), 1);
        assert!(rows.iter().all(|r| r.t % 3_600_000 == 0), "{rows:?}");
        assert!(
            rows.iter().all(|r| r.evaluations < 50),
            "another flag or site leaked in"
        );
        drop_clickhouse(&ch).await;
    }
}
