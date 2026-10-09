//! `/api/goals/*`, `/api/funnels/*`, `/api/segments/*`, `/api/dashboards/*`.
//!
//! Four resources with one shape: a name, a site, and a JSON document that the
//! query engine later compiles. They live in one module because they share the
//! rule that matters, which is **what gets validated and when**.
//!
//! A goal's `conditions`, a funnel's `steps` and a segment's `filters` are all
//! the filter shape from `docs/CONTRACT.md` §3.2. If a malformed one is stored,
//! nothing fails at write time; it fails later, inside a report, for whoever
//! happens to open that screen, and the error names a filter they never wrote.
//! So every document is compiled *here*, at the boundary, with the same code the
//! query engine will use — [`micaforge_query::GoalDef`] and
//! [`micaforge_query::FunnelStepDef`] — and a definition that will not compile
//! is refused with the field named.
//!
//! ## Who may write
//!
//! `member` and above. These objects change what every number on the site means:
//! renaming a goal renames a column in somebody's emailed report, and editing a
//! segment silently changes the answer a saved view gives. A `viewer` reads them
//! and nothing more.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use micaforge_core::{filters, new_id, Error, Result};
use micaforge_query::{FunnelStepDef, GoalDef};
use micaforge_store::models::{dashboards, funnels, goals, segments};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{clean_name, require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// Longest funnel a report can render usefully.
///
/// Twelve steps. Past that the chart is unreadable and every step's population
/// is too small to say anything about, so the limit is a kindness rather than a
/// technical bound.
const MAX_FUNNEL_STEPS: usize = 12;

/// Widest completion window a funnel may use, in hours. One year.
const MAX_FUNNEL_WINDOW_HOURS: i32 = 24 * 365;

/// Largest value one conversion may be worth, in minor units.
///
/// Ten million units — a hundred thousand pounds a conversion. A bound exists so
/// a mistyped figure cannot overflow the revenue total of a whole report.
const MAX_GOAL_VALUE_CENTS: i64 = 1_000_000_000;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every goal, funnel, segment and dashboard route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/goals", get(list_goals).post(create_goal))
        .route(
            "/api/goals/{id}",
            get(get_goal).patch(update_goal).delete(delete_goal),
        )
        .route("/api/funnels", get(list_funnels).post(create_funnel))
        .route(
            "/api/funnels/{id}",
            get(get_funnel).patch(update_funnel).delete(delete_funnel),
        )
        .route("/api/segments", get(list_segments).post(create_segment))
        .route(
            "/api/segments/{id}",
            get(get_segment)
                .patch(update_segment)
                .delete(delete_segment),
        )
        .route(
            "/api/dashboards",
            get(list_dashboards).post(create_dashboard),
        )
        .route(
            "/api/dashboards/{id}",
            get(get_dashboard)
                .patch(update_dashboard)
                .delete(delete_dashboard),
        )
}

// ---------------------------------------------------------------------------
// Goals
// ---------------------------------------------------------------------------

/// `GET /api/goals?site=…`
async fn list_goals(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = goals::list(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(goal_json).collect::<Vec<_>>()))
}

/// A goal, as the settings screen sends it. Also what the MCP `create_goal`
/// tool builds, so both front doors validate one shape.
#[derive(Debug, Deserialize)]
pub(crate) struct GoalBody {
    pub(crate) name: String,
    /// `path` | `event` | `outbound` | `file` | `revenue`.
    pub(crate) kind: String,
    /// A filter group in the §3.2 wire shape. Absent means "any event of this
    /// kind counts", which is what a bare revenue goal is.
    #[serde(default)]
    pub(crate) conditions: Option<Value>,
    /// What one conversion is worth, in minor units.
    #[serde(default)]
    pub(crate) value_cents: Option<i64>,
}

/// `POST /api/goals?site=…`
async fn create_goal(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<GoalBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let (name, kind, conditions, value) = check_goal(&body)?;

    let goal = goals::create(
        &st.pg,
        new_id(),
        access.site.id,
        &name,
        kind,
        &conditions,
        value,
    )
    .await?;
    Ok(ok(goal_json(&goal)))
}

/// `GET /api/goals/{id}`
async fn get_goal(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let goal = goals::get(&st.pg, id).await?;
    reachable(&st, goal.site_id, auth.user.id, VIEWER, "goal").await?;
    Ok(ok(goal_json(&goal)))
}

/// `PATCH /api/goals/{id}`
async fn update_goal(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<GoalBody>,
) -> ApiResult<Json<Value>> {
    let goal = goals::get(&st.pg, id).await?;
    reachable(&st, goal.site_id, auth.user.id, MEMBER, "goal").await?;

    let (name, kind, conditions, value) = check_goal(&body)?;
    let updated = goals::update(&st.pg, goal.id, &name, kind, &conditions, value).await?;
    Ok(ok(goal_json(&updated)))
}

/// `DELETE /api/goals/{id}`
///
/// The conversions already recorded against it are unaffected: a goal is a
/// question asked of the event stream, not a column written into it.
async fn delete_goal(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let goal = goals::get(&st.pg, id).await?;
    reachable(&st, goal.site_id, auth.user.id, MEMBER, "goal").await?;
    goals::delete(&st.pg, goal.id).await?;
    Ok(ok(json!({ "deleted": goal.id })))
}

/// Validate a goal body by compiling it exactly as a report will.
///
/// Returns the pieces the repository takes, so a caller cannot accidentally
/// store the unvalidated originals.
pub(crate) fn check_goal(body: &GoalBody) -> Result<(String, &'static str, Value, i64)> {
    let name = clean_name(&body.name, "name")?;
    let kind = micaforge_query::stats::GoalKind::parse(&body.kind)?;
    let conditions = body.conditions.clone().unwrap_or_else(|| json!({}));

    let value = body.value_cents.unwrap_or(0);
    if !(0..=MAX_GOAL_VALUE_CENTS).contains(&value) {
        return Err(Error::validation(
            "value_cents",
            format!("must be between 0 and {MAX_GOAL_VALUE_CENTS}"),
        ));
    }

    // The real check: build the engine's own definition. If this fails the goal
    // could never have been reported on, and refusing it now names the field.
    GoalDef::parse(
        "check",
        &name,
        kind.as_str(),
        &conditions.to_string(),
        value,
    )
    .map_err(|e| Error::validation("conditions", e.to_string()))?
    .predicate()
    .map_err(|e| Error::validation("conditions", e.to_string()))?;

    Ok((name, kind.as_str(), conditions, value))
}

/// One goal on the wire.
pub(crate) fn goal_json(goal: &goals::Goal) -> Value {
    json!({
        "id": goal.id,
        "site_id": goal.site_id,
        "name": goal.name,
        "kind": goal.kind,
        "conditions": goal.conditions,
        "value_cents": goal.value_cents,
    })
}

// ---------------------------------------------------------------------------
// Funnels
// ---------------------------------------------------------------------------

/// `GET /api/funnels?site=…`
async fn list_funnels(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = funnels::list(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(funnel_json).collect::<Vec<_>>()))
}

/// A funnel, as the editor sends it.
#[derive(Debug, Deserialize)]
struct FunnelBody {
    name: String,
    /// Ordered steps: `[{ "label": "…", "conditions": { … } }, …]`.
    steps: Value,
    /// How long a visitor has to finish the whole sequence.
    #[serde(default)]
    window_hours: Option<i32>,
}

/// `POST /api/funnels?site=…`
async fn create_funnel(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<FunnelBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let (name, steps, window) = check_funnel(&body)?;

    let funnel = funnels::create(&st.pg, new_id(), access.site.id, &name, &steps, window).await?;
    Ok(ok(funnel_json(&funnel)))
}

/// `GET /api/funnels/{id}`
async fn get_funnel(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let funnel = funnels::get(&st.pg, id).await?;
    reachable(&st, funnel.site_id, auth.user.id, VIEWER, "funnel").await?;
    Ok(ok(funnel_json(&funnel)))
}

/// `PATCH /api/funnels/{id}`
async fn update_funnel(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<FunnelBody>,
) -> ApiResult<Json<Value>> {
    let funnel = funnels::get(&st.pg, id).await?;
    reachable(&st, funnel.site_id, auth.user.id, MEMBER, "funnel").await?;

    let (name, steps, window) = check_funnel(&body)?;
    let updated = funnels::update(&st.pg, funnel.id, &name, &steps, window).await?;
    Ok(ok(funnel_json(&updated)))
}

/// `DELETE /api/funnels/{id}`
async fn delete_funnel(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let funnel = funnels::get(&st.pg, id).await?;
    reachable(&st, funnel.site_id, auth.user.id, MEMBER, "funnel").await?;
    funnels::delete(&st.pg, funnel.id).await?;
    Ok(ok(json!({ "deleted": funnel.id })))
}

/// Validate a funnel by compiling every step.
///
/// A step with no conditions is refused by the engine, not tolerated: it would
/// be true for every row, which draws a flat funnel and hides the mistake.
fn check_funnel(body: &FunnelBody) -> Result<(String, Value, i32)> {
    let name = clean_name(&body.name, "name")?;

    let Some(steps) = body.steps.as_array() else {
        return Err(Error::validation("steps", "must be an array of steps"));
    };
    if steps.len() < 2 {
        return Err(Error::validation(
            "steps",
            "a funnel needs at least two steps to measure a drop-off",
        ));
    }
    if steps.len() > MAX_FUNNEL_STEPS {
        return Err(Error::validation(
            "steps",
            format!("cannot have more than {MAX_FUNNEL_STEPS} steps"),
        ));
    }

    for (index, step) in steps.iter().enumerate() {
        let label = step
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .ok_or_else(|| {
                Error::validation("steps", format!("step {} has no label", index + 1))
            })?;
        let conditions = step.get("conditions").cloned().unwrap_or_else(|| json!({}));

        FunnelStepDef::parse(label, &conditions.to_string())
            .map_err(|e| Error::validation("steps", e.to_string()))?
            .predicate()
            .map_err(|e| Error::validation("steps", e.to_string()))?;
    }

    let window = body.window_hours.unwrap_or(24);
    if !(1..=MAX_FUNNEL_WINDOW_HOURS).contains(&window) {
        return Err(Error::validation(
            "window_hours",
            format!("must be between 1 and {MAX_FUNNEL_WINDOW_HOURS}"),
        ));
    }

    Ok((name, body.steps.clone(), window))
}

/// One funnel on the wire.
fn funnel_json(funnel: &funnels::Funnel) -> Value {
    json!({
        "id": funnel.id,
        "site_id": funnel.site_id,
        "name": funnel.name,
        "steps": funnel.steps,
        "window_hours": funnel.window_hours,
        "step_count": funnel.steps.as_array().map(Vec::len).unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Segments
// ---------------------------------------------------------------------------

/// `GET /api/segments?site=…`
async fn list_segments(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = segments::list(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(segment_json).collect::<Vec<_>>()))
}

/// A saved filter set.
#[derive(Debug, Deserialize)]
struct SegmentBody {
    name: String,
    /// A filter group in the §3.2 wire shape.
    filters: Value,
    /// True when everyone on the site may use it.
    #[serde(default)]
    shared: Option<bool>,
}

/// `POST /api/segments?site=…`
async fn create_segment(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<SegmentBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let (name, group) = check_segment(&body)?;

    let segment = segments::create(
        &st.pg,
        new_id(),
        access.site.id,
        &name,
        &group,
        body.shared.unwrap_or(false),
    )
    .await?;
    Ok(ok(segment_json(&segment)))
}

/// `GET /api/segments/{id}`
async fn get_segment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let segment = segments::get(&st.pg, id).await?;
    reachable(&st, segment.site_id, auth.user.id, VIEWER, "segment").await?;
    Ok(ok(segment_json(&segment)))
}

/// `PATCH /api/segments/{id}`
async fn update_segment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SegmentBody>,
) -> ApiResult<Json<Value>> {
    let segment = segments::get(&st.pg, id).await?;
    reachable(&st, segment.site_id, auth.user.id, MEMBER, "segment").await?;

    let (name, group) = check_segment(&body)?;
    let updated = segments::update(
        &st.pg,
        segment.id,
        &name,
        &group,
        body.shared.unwrap_or(segment.shared),
    )
    .await?;
    Ok(ok(segment_json(&updated)))
}

/// `DELETE /api/segments/{id}`
async fn delete_segment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let segment = segments::get(&st.pg, id).await?;
    reachable(&st, segment.site_id, auth.user.id, MEMBER, "segment").await?;
    segments::delete(&st.pg, segment.id).await?;
    Ok(ok(json!({ "deleted": segment.id })))
}

/// Validate a segment by parsing its filters.
fn check_segment(body: &SegmentBody) -> Result<(String, Value)> {
    let name = clean_name(&body.name, "name")?;

    let group = filters::parse_filters(&body.filters.to_string())
        .map_err(|e| Error::validation("filters", e.to_string()))?;
    if group.is_empty() {
        return Err(Error::validation(
            "filters",
            "a segment with no filters would select every visitor",
        ));
    }
    group
        .validate()
        .map_err(|e| Error::validation("filters", e.to_string()))?;

    Ok((name, body.filters.clone()))
}

/// One segment on the wire.
fn segment_json(segment: &segments::Segment) -> Value {
    json!({
        "id": segment.id,
        "site_id": segment.site_id,
        "name": segment.name,
        "filters": segment.filters,
        "shared": segment.shared,
    })
}

// ---------------------------------------------------------------------------
// Dashboards
// ---------------------------------------------------------------------------

/// `GET /api/dashboards?site=…`
async fn list_dashboards(State(st): State<AppState>, access: SiteAccess) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = dashboards::list(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(dashboard_json).collect::<Vec<_>>()))
}

/// A composed report grid.
#[derive(Debug, Deserialize)]
struct DashboardBody {
    name: String,
    /// Widgets and their positions. The editor owns this shape.
    #[serde(default)]
    layout: Option<Value>,
}

/// `POST /api/dashboards?site=…`
async fn create_dashboard(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<DashboardBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let name = clean_name(&body.name, "name")?;
    let layout = check_layout(body.layout.clone())?;

    let dashboard = dashboards::create(&st.pg, new_id(), access.site.id, &name, &layout).await?;
    Ok(ok(dashboard_json(&dashboard)))
}

/// `GET /api/dashboards/{id}`
async fn get_dashboard(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let dashboard = dashboards::get(&st.pg, id).await?;
    reachable(&st, dashboard.site_id, auth.user.id, VIEWER, "dashboard").await?;
    Ok(ok(dashboard_json(&dashboard)))
}

/// `PATCH /api/dashboards/{id}`
async fn update_dashboard(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DashboardBody>,
) -> ApiResult<Json<Value>> {
    let dashboard = dashboards::get(&st.pg, id).await?;
    reachable(&st, dashboard.site_id, auth.user.id, MEMBER, "dashboard").await?;

    let name = clean_name(&body.name, "name")?;
    let layout = match body.layout.clone() {
        Some(value) => check_layout(Some(value))?,
        None => dashboard.layout.clone(),
    };
    let updated = dashboards::update(&st.pg, dashboard.id, &name, &layout).await?;
    Ok(ok(dashboard_json(&updated)))
}

/// `DELETE /api/dashboards/{id}`
async fn delete_dashboard(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let dashboard = dashboards::get(&st.pg, id).await?;
    reachable(&st, dashboard.site_id, auth.user.id, MEMBER, "dashboard").await?;
    dashboards::delete(&st.pg, dashboard.id).await?;
    Ok(ok(json!({ "deleted": dashboard.id })))
}

/// A layout is opaque, but it still has to be a container.
///
/// The editor owns the widget shape, so validating it here would mean a layout
/// change needed a backend release. What is checked is only that a scalar was
/// not stored where a grid belongs, which would break the editor on next open.
fn check_layout(layout: Option<Value>) -> Result<Value> {
    let value = layout.unwrap_or_else(|| json!({ "widgets": [] }));
    if !(value.is_object() || value.is_array()) {
        return Err(Error::validation(
            "layout",
            "must be a JSON object or array of widgets",
        ));
    }
    Ok(value)
}

/// One dashboard on the wire.
fn dashboard_json(dashboard: &dashboards::Dashboard) -> Value {
    json!({
        "id": dashboard.id,
        "site_id": dashboard.site_id,
        "name": dashboard.name,
        "layout": dashboard.layout,
    })
}

// ---------------------------------------------------------------------------
// Shared authorisation
// ---------------------------------------------------------------------------

/// Assert the caller may act on an object belonging to `site_id`.
///
/// Objects are addressed by their own UUID, which the caller may have guessed,
/// so an object on a site they cannot see is reported as *that object* being
/// missing. Reporting `403` would confirm the id exists and leak which sites
/// hold which configuration.
async fn reachable(
    st: &AppState,
    site_id: Uuid,
    user_id: Uuid,
    needed: &str,
    what: &str,
) -> Result<()> {
    match authorize_site(&st.pg, site_id, user_id, needed).await {
        Ok(_) => Ok(()),
        Err(Error::NotFound(_)) => Err(Error::not_found(what.to_string())),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goal(kind: &str, conditions: Value, value: i64) -> GoalBody {
        GoalBody {
            name: "Signed up".into(),
            kind: kind.into(),
            conditions: Some(conditions),
            value_cents: Some(value),
        }
    }

    #[test]
    fn a_goal_is_compiled_before_it_is_stored() {
        let ok = goal(
            "path",
            json!([{ "dimension": "pathname", "op": "is", "value": "/thanks" }]),
            0,
        );
        assert!(check_goal(&ok).is_ok());

        // A dimension the engine does not have would fail inside a report, so
        // it fails here instead, naming the field.
        let bad = goal(
            "path",
            json!([{ "dimension": "moon_phase", "op": "is", "value": "waxing" }]),
            0,
        );
        assert_eq!(check_goal(&bad).unwrap_err().field(), Some("conditions"));
    }

    #[test]
    fn a_bare_revenue_goal_needs_no_conditions() {
        let body = GoalBody {
            name: "Any purchase".into(),
            kind: "revenue".into(),
            conditions: None,
            value_cents: None,
        };
        let (_, kind, _, value) = check_goal(&body).unwrap();
        assert_eq!(kind, "revenue");
        assert_eq!(value, 0);
    }

    #[test]
    fn goal_kinds_and_values_are_bounded() {
        assert!(check_goal(&goal("teleport", json!({}), 0)).is_err());
        assert_eq!(
            check_goal(&goal("path", json!({}), -1))
                .unwrap_err()
                .field(),
            Some("value_cents")
        );
        assert!(check_goal(&goal("path", json!({}), MAX_GOAL_VALUE_CENTS + 1)).is_err());
    }

    #[test]
    fn a_funnel_needs_at_least_two_steps_that_actually_filter() {
        let one_step = FunnelBody {
            name: "Checkout".into(),
            steps: json!([{ "label": "Cart", "conditions": [] }]),
            window_hours: None,
        };
        assert_eq!(check_funnel(&one_step).unwrap_err().field(), Some("steps"));

        // Two steps, but the second matches everything: a flat funnel that
        // hides its own misconfiguration.
        let empty_step = FunnelBody {
            name: "Checkout".into(),
            steps: json!([
                { "label": "Cart", "conditions": [{ "dimension": "pathname", "op": "is", "value": "/cart" }] },
                { "label": "Paid", "conditions": [] }
            ]),
            window_hours: None,
        };
        assert_eq!(
            check_funnel(&empty_step).unwrap_err().field(),
            Some("steps")
        );
    }

    #[test]
    fn a_well_formed_funnel_passes_with_its_window_defaulted() {
        let steps = json!([
            { "label": "Cart", "conditions": [{ "dimension": "pathname", "op": "is", "value": "/cart" }] },
            { "label": "Paid", "conditions": [{ "dimension": "pathname", "op": "is", "value": "/thanks" }] }
        ]);

        let body = FunnelBody {
            name: "Checkout".into(),
            steps: steps.clone(),
            window_hours: None,
        };
        let (_, _, window) = check_funnel(&body).unwrap();
        assert_eq!(window, 24);

        let too_wide = FunnelBody {
            name: "Checkout".into(),
            steps,
            window_hours: Some(MAX_FUNNEL_WINDOW_HOURS + 1),
        };
        assert_eq!(
            check_funnel(&too_wide).unwrap_err().field(),
            Some("window_hours")
        );
    }

    #[test]
    fn an_empty_segment_would_select_everybody_and_is_refused() {
        let empty = SegmentBody {
            name: "Everyone".into(),
            filters: json!([]),
            shared: None,
        };
        assert_eq!(check_segment(&empty).unwrap_err().field(), Some("filters"));

        let real = SegmentBody {
            name: "From ChatGPT".into(),
            filters: json!([{ "dimension": "source", "op": "is", "value": "chatgpt" }]),
            shared: Some(true),
        };
        assert!(check_segment(&real).is_ok());
    }

    #[test]
    fn a_layout_must_be_a_container() {
        assert!(check_layout(None).unwrap().is_object());
        assert!(check_layout(Some(json!([]))).is_ok());
        assert_eq!(
            check_layout(Some(json!("widgets"))).unwrap_err().field(),
            Some("layout")
        );
    }
}
