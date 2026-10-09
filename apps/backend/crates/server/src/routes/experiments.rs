//! `/api/experiments/*`.
//!
//! An **experiment** is a measurement: this hypothesis, these variants, *this
//! goal*, this window. Feature flags, the runtime switches an experiment's
//! variants are usually served through, live in [`super::flags`] and
//! [`super::flag_delivery`].
//!
//! The goal is named when the experiment is created and cannot be swapped
//! afterwards, which is the point: choosing the metric after seeing the data is
//! how experiments lie, and an analytics tool that made it easy would be selling
//! a way to be wrong on purpose.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use micaforge_core::{new_id, Error, Result};
use micaforge_query::{bind_all, GoalDef, Param};
use micaforge_store::models::{flags, goals};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::ApiError;
use crate::extract::{AuthUser, SiteAccess};
use crate::response::ok;
use crate::routes::orgs::{require_at_least, MEMBER, VIEWER};
use crate::routes::sites::authorize_site;
use crate::state::AppState;

/// What every handler in this module returns.
///
/// `micaforge_core::Result` is a one-parameter alias, so the two-parameter form
/// has to be spelled out somewhere; naming it once here keeps `Result<T>` free
/// to mean the core alias in the helpers below, which is what they all use.
type ApiResult<T> = std::result::Result<T, ApiError>;

/// The four states an experiment moves through.
const STATUSES: &[&str] = &["draft", "running", "paused", "finished"];

/// Below this many visitors in a variant, no significance is reported.
///
/// A z-test on thirty visitors will happily produce a confident-looking number,
/// and reporting it would be the most harmful thing this endpoint could do.
const MIN_VARIANT_VISITORS: u64 = 100;

/// The threshold at which a result is called significant.
const SIGNIFICANCE_P: f64 = 0.05;

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Every experiment route.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/experiments",
            get(list_experiments).post(create_experiment),
        )
        .route(
            "/api/experiments/{id}",
            get(get_experiment)
                .patch(update_experiment)
                .delete(delete_experiment),
        )
        .route("/api/experiments/{id}/status", post(set_status))
        .route("/api/experiments/{id}/results", get(results))
}

// ---------------------------------------------------------------------------
// Experiments
// ---------------------------------------------------------------------------

/// `GET /api/experiments?site=…`
async fn list_experiments(
    State(st): State<AppState>,
    access: SiteAccess,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), VIEWER)?;
    let rows = flags::list_experiments(&st.pg, access.site.id).await?;
    Ok(ok(rows.iter().map(experiment_json).collect::<Vec<_>>()))
}

/// An experiment, as the editor sends it.
#[derive(Debug, Deserialize)]
struct ExperimentBody {
    key: String,
    hypothesis: String,
    /// `[{ "key": "control", "weight": 50 }, { "key": "treatment", "weight": 50 }]`.
    variants: Value,
    /// The goal that decides the outcome.
    #[serde(default)]
    goal_id: Option<Uuid>,
}

/// `POST /api/experiments?site=…` — create one, in `draft`.
async fn create_experiment(
    State(st): State<AppState>,
    access: SiteAccess,
    Json(body): Json<ExperimentBody>,
) -> ApiResult<Json<Value>> {
    require_at_least(access.role.as_str(), MEMBER)?;
    let (key, hypothesis, variants) = check_experiment(&body)?;
    check_goal_belongs(&st, body.goal_id, access.site.id).await?;

    let experiment = flags::create_experiment(
        &st.pg,
        new_id(),
        access.site.id,
        &key,
        &hypothesis,
        &variants,
        body.goal_id,
        "draft",
    )
    .await?;
    Ok(ok(experiment_json(&experiment)))
}

/// `GET /api/experiments/{id}`
async fn get_experiment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    reachable(&st, experiment.site_id, auth.user.id, VIEWER, "experiment").await?;
    Ok(ok(experiment_json(&experiment)))
}

/// `PATCH /api/experiments/{id}`
///
/// Refused once the experiment has started. Changing the variants or the goal
/// mid-flight would mean the data collected before the change and the data
/// collected after it are answering different questions, and the report would
/// average them into a number that means nothing.
async fn update_experiment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<ExperimentBody>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    let site = reachable(&st, experiment.site_id, auth.user.id, MEMBER, "experiment").await?;

    if experiment.started_at.is_some() {
        return Err(Error::Conflict(
            "an experiment that has collected data cannot be redefined; finish it and start another"
                .into(),
        )
        .into());
    }

    let (key, hypothesis, variants) = check_experiment(&body)?;
    check_goal_belongs(&st, body.goal_id, site.id).await?;

    let updated = flags::update_experiment(
        &st.pg,
        experiment.id,
        &key,
        &hypothesis,
        &variants,
        body.goal_id,
    )
    .await?;
    Ok(ok(experiment_json(&updated)))
}

/// Where an experiment is being moved to.
#[derive(Debug, Deserialize)]
struct StatusBody {
    status: String,
}

/// `POST /api/experiments/{id}/status` — start, pause, resume or finish.
async fn set_status(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    reachable(&st, experiment.site_id, auth.user.id, MEMBER, "experiment").await?;

    let status = body.status.trim();
    if !STATUSES.contains(&status) {
        return Err(
            Error::validation("status", format!("must be one of {}", STATUSES.join(", "))).into(),
        );
    }
    if experiment.status == "finished" && status != "finished" {
        return Err(Error::Conflict(
            "a finished experiment cannot be restarted; its window has closed".into(),
        )
        .into());
    }
    if status == "running" && experiment.goal_id.is_none() {
        return Err(Error::validation(
            "goal_id",
            "an experiment needs a goal before it starts, so the metric cannot be chosen afterwards",
        )
        .into());
    }

    let updated = flags::set_experiment_status(&st.pg, experiment.id, status).await?;
    tracing::info!(experiment = %experiment.id, key = %experiment.key, status, "experiment moved");
    Ok(ok(experiment_json(&updated)))
}

/// `DELETE /api/experiments/{id}`
async fn delete_experiment(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    reachable(&st, experiment.site_id, auth.user.id, MEMBER, "experiment").await?;
    flags::delete_experiment(&st.pg, experiment.id).await?;
    Ok(ok(json!({
        "deleted": experiment.id,
        "note": "the exposures stay on the event rows; only the definition is gone",
    })))
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// One variant's counts, as ClickHouse reports them.
#[derive(Debug, clickhouse::Row, Deserialize)]
struct VariantRow {
    variant: String,
    visitors: u64,
    exposures: u64,
    conversions: u64,
}

/// `GET /api/experiments/{id}/results` — the computation hook.
///
/// **What is counted.** A visitor is *in* a variant when an event row carries
/// this experiment's key and that variant. A *conversion* is a row that both
/// carries the variant and satisfies the experiment's goal, which works because
/// the tracker stamps the active experiment and variant onto every event it
/// sends while the experiment is running. Both are `uniqExact` over
/// `visitor_id`, so one person converting twice is one conversion.
///
/// **The window** is `started_at` to `ended_at`, or to now for a running one. It
/// is never the caller's date range: an experiment's result is a property of the
/// experiment, not of whatever the reader happens to be looking at.
async fn results(
    State(st): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let experiment = flags::get_experiment(&st.pg, id).await?;
    let site = reachable(&st, experiment.site_id, auth.user.id, VIEWER, "experiment").await?;

    let Some(started) = experiment.started_at else {
        return Ok(ok(json!({
            "experiment": experiment_json(&experiment),
            "status": "not_started",
            "variants": Vec::<Value>::new(),
        })));
    };
    let ended = experiment.ended_at.unwrap_or_else(Utc::now);

    let goal = match experiment.goal_id {
        Some(goal_id) => Some(goals::get(&st.pg, goal_id).await?),
        None => None,
    };
    let (predicate, predicate_params) = match goal.as_ref() {
        Some(g) => GoalDef::parse(
            g.id.to_string(),
            g.name.as_str(),
            &g.kind,
            &g.conditions.to_string(),
            g.value_cents,
        )?
        .predicate()?,
        // No goal: the exposure counts are still real and worth showing, and a
        // conversion count of zero is honestly labelled by `goal: null`.
        None => ("1 = 0".to_string(), Vec::new()),
    };

    let rows = variant_counts(
        &st,
        site.site_id_u16(),
        &experiment.key,
        started,
        ended,
        &predicate,
        predicate_params,
    )
    .await?;

    let value_cents = goal.as_ref().map(|g| g.value_cents).unwrap_or(0);
    let control = choose_control(&rows);
    let baseline = control.and_then(|name| rows.iter().find(|r| r.variant == *name));

    let variants: Vec<Value> = rows
        .iter()
        .map(|row| variant_result(row, baseline, value_cents))
        .collect();

    Ok(ok(json!({
        "experiment": experiment_json(&experiment),
        "goal": goal.as_ref().map(|g| json!({
            "id": g.id,
            "name": g.name,
            "kind": g.kind,
            "value_cents": g.value_cents,
        })),
        "window": { "start": started, "end": ended },
        "control": control,
        "variants": variants,
        "significance_threshold": SIGNIFICANCE_P,
        "minimum_visitors_per_variant": MIN_VARIANT_VISITORS,
    })))
}

/// Read exposure and conversion counts per variant.
///
/// The goal predicate is interpolated into the `SELECT`, so its bind parameters
/// come **first**: ClickHouse binds positionally, and a list that is one out
/// does not fail cleanly, it silently answers a different question.
async fn variant_counts(
    st: &AppState,
    site: u16,
    experiment_key: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    predicate: &str,
    mut params: Vec<Param>,
) -> Result<Vec<VariantRow>> {
    let sql = format!(
        "SELECT
             variant,
             uniqExact(visitor_id) AS visitors,
             count() AS exposures,
             uniqExactIf(visitor_id, {predicate}) AS conversions
         FROM events
         WHERE site_id = ?
           AND experiment = ?
           AND variant != ''
           AND timestamp >= toDateTime64(?, 3, 'UTC')
           AND timestamp <  toDateTime64(?, 3, 'UTC')
         GROUP BY variant
         ORDER BY variant"
    );

    params.push(Param::U16(site));
    params.push(Param::Str(experiment_key.to_string()));
    params.push(Param::Str(micaforge_query::sql::fmt_ts(start)));
    params.push(Param::Str(micaforge_query::sql::fmt_ts(end)));

    bind_all(st.ch.client().query(&sql), &params)
        .fetch_all::<VariantRow>()
        .await
        .map_err(|e| Error::Database(format!("clickhouse experiment results: {e}")))
}

/// Which variant everything else is measured against.
///
/// A variant literally called `control`, otherwise the first alphabetically.
/// Something has to be the baseline, and picking the *best-performing* one would
/// manufacture a winner out of noise.
fn choose_control(rows: &[VariantRow]) -> Option<&String> {
    rows.iter()
        .find(|r| r.variant.eq_ignore_ascii_case("control"))
        .or_else(|| rows.first())
        .map(|r| &r.variant)
}

/// One variant's result, against the baseline.
fn variant_result(row: &VariantRow, baseline: Option<&VariantRow>, value_cents: i64) -> Value {
    let rate = ratio(row.conversions, row.visitors);
    let is_control = baseline.is_some_and(|b| b.variant == row.variant);

    let (lift, p_value, significant) = match baseline {
        Some(base) if !is_control => {
            let base_rate = ratio(base.conversions, base.visitors);
            let lift = if base_rate > 0.0 {
                Some((rate - base_rate) / base_rate)
            } else {
                None
            };
            // Below the floor no significance is reported at all. A p-value on
            // a handful of visitors is a confident-looking lie.
            let enough =
                row.visitors >= MIN_VARIANT_VISITORS && base.visitors >= MIN_VARIANT_VISITORS;
            let p = if enough {
                two_proportion_p(
                    base.conversions,
                    base.visitors,
                    row.conversions,
                    row.visitors,
                )
            } else {
                None
            };
            (lift, p, p.map(|p| p < SIGNIFICANCE_P))
        }
        _ => (None, None, None),
    };

    json!({
        "variant": row.variant,
        "is_control": is_control,
        "visitors": row.visitors,
        "exposures": row.exposures,
        "conversions": row.conversions,
        "conversion_rate": rate,
        "revenue_cents": row.conversions as i64 * value_cents,
        "lift": lift,
        "p_value": p_value,
        "significant": significant,
        "confidence": p_value.map(|p| 1.0 - p),
    })
}

/// A conversion rate, with an empty variant reported as zero rather than NaN.
fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// Two-tailed p-value for a difference between two proportions.
///
/// The pooled two-proportion z-test: the standard test for "did this variant
/// convert at a different rate from the control", and the one every A/B
/// calculator on the internet implements, so the number here matches the number
/// an operator gets when they check it elsewhere.
///
/// `None` when the test is undefined, which happens when both arms converted at
/// exactly 0% or exactly 100% and there is no variance to test.
fn two_proportion_p(
    control_conversions: u64,
    control_visitors: u64,
    variant_conversions: u64,
    variant_visitors: u64,
) -> Option<f64> {
    if control_visitors == 0 || variant_visitors == 0 {
        return None;
    }
    let n1 = control_visitors as f64;
    let n2 = variant_visitors as f64;
    let p1 = control_conversions as f64 / n1;
    let p2 = variant_conversions as f64 / n2;

    let pooled = (control_conversions + variant_conversions) as f64 / (n1 + n2);
    let variance = pooled * (1.0 - pooled) * (1.0 / n1 + 1.0 / n2);
    if variance <= 0.0 {
        return None;
    }

    let z = (p2 - p1) / variance.sqrt();
    Some((2.0 * (1.0 - normal_cdf(z.abs()))).clamp(0.0, 1.0))
}

/// The standard normal cumulative distribution.
fn normal_cdf(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

/// Abramowitz and Stegun 7.1.26.
///
/// Maximum absolute error 1.5e-7, which is four orders of magnitude finer than
/// any p-value this endpoint reports to two decimal places. Written out rather
/// than pulled from a statistics crate because it is nine lines and the
/// dependency would be the larger cost.
fn erf(x: f64) -> f64 {
    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    const P: f64 = 0.327_591_1;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + P * x);
    let y = 1.0 - (((((A5 * t + A4) * t) + A3) * t + A2) * t + A1) * t * (-x * x).exp();
    sign * y
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate an experiment body.
fn check_experiment(body: &ExperimentBody) -> Result<(String, String, Value)> {
    let key = clean_key(&body.key, "key")?;
    let hypothesis = body.hypothesis.trim();
    if hypothesis.is_empty() {
        return Err(Error::validation(
            "hypothesis",
            "say what you expect to happen; a result with no prediction proves nothing",
        ));
    }
    if hypothesis.chars().count() > 1000 {
        return Err(Error::validation("hypothesis", "is too long"));
    }

    check_variants(&body.variants, true)?;
    Ok((key, hypothesis.to_string(), body.variants.clone()))
}

/// A flag or experiment key: lower-case, url and code safe.
pub(crate) fn clean_key(raw: &str, field: &str) -> Result<String> {
    let key = raw.trim().to_lowercase();
    let usable = (1..=64).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !usable {
        return Err(Error::validation(
            field,
            "must be 1 to 64 characters of letters, digits, hyphens, underscores or dots",
        ));
    }
    Ok(key)
}

/// Validate a variant list.
///
/// `required` is true for an experiment, which is meaningless with fewer than
/// two arms, and false for a flag, where an empty list means a plain on/off
/// switch. Weights must be whole numbers from zero up, so the three evaluators
/// (`micaforge_core::flags` and the two SDK copies) read them identically.
pub(crate) fn check_variants(variants: &Value, required: bool) -> Result<()> {
    let Some(list) = variants.as_array() else {
        return Err(Error::validation("variants", "must be an array"));
    };
    if list.is_empty() {
        if required {
            return Err(Error::validation(
                "variants",
                "an experiment needs at least two variants to compare",
            ));
        }
        return Ok(());
    }
    if required && list.len() < 2 {
        return Err(Error::validation(
            "variants",
            "an experiment needs at least two variants to compare",
        ));
    }

    let mut seen: Vec<String> = Vec::with_capacity(list.len());
    let mut total = 0u64;
    for entry in list {
        let name = entry
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .ok_or_else(|| Error::validation("variants", "every variant needs a `key`"))?;
        if seen.iter().any(|s| s == name) {
            return Err(Error::validation(
                "variants",
                format!("`{name}` appears twice"),
            ));
        }
        seen.push(name.to_string());
        let weight = match entry.get("weight") {
            None | Some(Value::Null) => 1,
            Some(w) => w.as_u64().ok_or_else(|| {
                Error::validation(
                    "variants",
                    format!("the weight of `{name}` must be a whole number from 0 up"),
                )
            })?,
        };
        total += weight;
    }

    if total == 0 {
        return Err(Error::validation(
            "variants",
            "at least one variant needs a weight above zero",
        ));
    }
    Ok(())
}

/// Refuse a goal that belongs to another site.
///
/// An experiment judged against a goal on somebody else's site would report a
/// conversion rate assembled from two different populations.
async fn check_goal_belongs(st: &AppState, goal_id: Option<Uuid>, site_id: Uuid) -> Result<()> {
    let Some(goal_id) = goal_id else {
        return Ok(());
    };
    let goal = goals::get(&st.pg, goal_id).await?;
    if goal.site_id != site_id {
        return Err(Error::validation("goal_id", "belongs to another site"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shaping and shared authorisation
// ---------------------------------------------------------------------------

/// One experiment on the wire.
fn experiment_json(experiment: &flags::Experiment) -> Value {
    json!({
        "id": experiment.id,
        "site_id": experiment.site_id,
        "key": experiment.key,
        "hypothesis": experiment.hypothesis,
        "variants": experiment.variants,
        "goal_id": experiment.goal_id,
        "status": experiment.status,
        "started_at": experiment.started_at,
        "ended_at": experiment.ended_at,
        "editable": experiment.started_at.is_none(),
    })
}

/// Assert the caller may act on an object belonging to `site_id`, and hand back
/// the site so the caller can reach its numeric id.
pub(crate) async fn reachable(
    st: &AppState,
    site_id: Uuid,
    user_id: Uuid,
    needed: &str,
    what: &str,
) -> Result<micaforge_store::models::sites::Site> {
    match authorize_site(&st.pg, site_id, user_id, needed).await {
        Ok(site) => Ok(site),
        Err(Error::NotFound(_)) => Err(Error::not_found(what.to_string())),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_normal_tail_matches_the_textbook() {
        // Values every statistics table carries, to four decimal places.
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!((normal_cdf(-1.96) - 0.025).abs() < 1e-3);
        assert!(erf(0.0).abs() < 1e-7);
    }

    #[test]
    fn a_clear_win_is_significant_and_a_tie_is_not() {
        // 10% against 15% on a thousand each: a textbook significant result.
        let p = two_proportion_p(100, 1_000, 150, 1_000).unwrap();
        assert!(p < SIGNIFICANCE_P, "p was {p}");

        // Identical rates: nothing to see.
        let tie = two_proportion_p(100, 1_000, 100, 1_000).unwrap();
        assert!(tie > 0.9, "p was {tie}");

        // No variance at all: the test is undefined rather than certain.
        assert!(two_proportion_p(0, 500, 0, 500).is_none());
        assert!(two_proportion_p(0, 0, 0, 0).is_none());
    }

    #[test]
    fn a_small_variant_reports_no_significance_at_all() {
        let control = VariantRow {
            variant: "control".into(),
            visitors: 20,
            exposures: 30,
            conversions: 2,
        };
        let treatment = VariantRow {
            variant: "treatment".into(),
            visitors: 20,
            exposures: 31,
            conversions: 8,
        };

        let shaped = variant_result(&treatment, Some(&control), 0);
        assert_eq!(
            shaped["p_value"],
            Value::Null,
            "20 visitors cannot be significant"
        );
        assert_eq!(shaped["significant"], Value::Null);
        // The counts themselves are still reported honestly.
        assert_eq!(shaped["conversions"], json!(8));
    }

    #[test]
    fn the_control_is_chosen_by_name_not_by_performance() {
        let rows = vec![
            VariantRow {
                variant: "a-treatment".into(),
                visitors: 10,
                exposures: 10,
                conversions: 9,
            },
            VariantRow {
                variant: "control".into(),
                visitors: 10,
                exposures: 10,
                conversions: 1,
            },
        ];
        assert_eq!(choose_control(&rows).unwrap(), "control");

        // With nothing named `control`, the first alphabetically is the
        // baseline, because ClickHouse returned them ordered by variant.
        let unnamed = vec![
            VariantRow {
                variant: "blue".into(),
                visitors: 10,
                exposures: 10,
                conversions: 1,
            },
            VariantRow {
                variant: "green".into(),
                visitors: 10,
                exposures: 10,
                conversions: 9,
            },
        ];
        assert_eq!(choose_control(&unnamed).unwrap(), "blue");
        assert!(choose_control(&[]).is_none());
    }

    #[test]
    fn an_experiment_needs_two_distinct_weighted_variants() {
        assert!(check_variants(&json!([]), true).is_err());
        assert!(check_variants(&json!([{ "key": "only" }]), true).is_err());
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 1 }, { "key": "a", "weight": 1 }]),
            true
        )
        .is_err());
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 0 }, { "key": "b", "weight": 0 }]),
            true
        )
        .is_err());
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 1 }, { "key": "b", "weight": 3 }]),
            true
        )
        .is_ok());
        // Weights are whole numbers, so every evaluator reads them the same.
        assert!(check_variants(
            &json!([{ "key": "a", "weight": 2.5 }, { "key": "b", "weight": 1 }]),
            true
        )
        .is_err());
        assert!(check_variants(&json!([{ "key": "a", "weight": -1 }]), false).is_err());

        // A flag is allowed to have none: that is an on/off switch.
        assert!(check_variants(&json!([]), false).is_ok());
    }

    #[test]
    fn keys_are_lower_case_and_code_safe() {
        assert_eq!(clean_key(" New-Nav.v2 ", "key").unwrap(), "new-nav.v2");
        assert_eq!(clean_key("", "key").unwrap_err().field(), Some("key"));
        assert!(clean_key("has space", "key").is_err());
        assert!(clean_key(&"x".repeat(65), "key").is_err());
    }
}
