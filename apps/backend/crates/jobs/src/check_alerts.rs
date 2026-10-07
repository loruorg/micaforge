//! `check_alerts` — decide whether anything is worth interrupting somebody for.
//!
//! Runs every five minutes over every configured alert. Six kinds, from
//! `docs/CONTRACT.md` §2: `spike`, `drop`, `goal`, `error`, `agent_surge` and
//! `citation_gap`. The last two are the ones no other analytics tool can offer,
//! and they are the reason this job exists in the shape it does.
//!
//! # The baseline is the same hour last week, not the last hour
//!
//! Traffic is periodic on a weekly cycle. Tuesday at 09:00 looks like last
//! Tuesday at 09:00, and looks nothing at all like Tuesday at 03:00 or like
//! Sunday at 09:00. An alert that compares an hour against a rolling average of
//! recent hours fires every weekday morning and stays silent through every
//! weekend collapse, which trains the operator to ignore it — and an alert that
//! is ignored is worse than no alert, because it occupies the place where a real
//! one would have gone.
//!
//! So the comparison is against the mean and standard deviation of *the same
//! weekday and hour* over the preceding weeks. See
//! [`crate::metrics::weekly_baseline`].
//!
//! # It refuses to fire on thin evidence
//!
//! Three guards, all of them there because the failure they prevent is a false
//! alarm sent to a customer:
//!
//! - Fewer than two comparable periods and nothing fires at all. A "spike"
//!   measured against one previous Tuesday is a coin toss dressed as a statistic.
//! - `min_events` sets a floor. Three visitors where there is usually one is a
//!   200% spike and is nothing.
//! - The cooldown is claimed in SQL by
//!   [`micaforge_store::models::alerts::claim_alert`], so two workers
//!   evaluating the same alert in the same second cannot both send.
//!
//! # A delivery that reached nobody is a failure
//!
//! An alert whose every channel failed is reported as an error, because the
//! alternative is a queue that looks healthy while the notifications go nowhere.
//! The cooldown claim is handed back
//! ([`micaforge_store::models::alerts::release_alert`]), so the alert is tried
//! again rather than staying silent for its whole cooldown about something
//! nobody was told, and the sweep returns an error so the worker retries it
//! within a minute.
//!
//! Only a *fresh* failure fails the sweep. An alert whose channel is broken for
//! good (a deleted Slack app, a webhook that answers 404) would otherwise keep
//! the five-minute sweep on the worker's retry backoff, and every other alert on
//! the install would be checked late for as long as that one stayed broken. A
//! short-lived Valkey marker remembers that an alert already failed; while it
//! is there, the alert is still retried on every sweep, but it no longer fails
//! the job.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use micaforge_core::{Error, QueryEnvelope, Result, TimeRange};
use micaforge_mail::AlertPayload;
use micaforge_query::{gap, stats};
use micaforge_store::models::{alerts, goals, sites};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::metrics::{self, Baseline, Pulse};
use crate::notify::{self, Channels};
use crate::JobCtx;

/// Which alerts to evaluate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One alert, by `alerts.id`. Absent means every alert on the install, which
    /// is what the five-minute sweep queues.
    #[serde(default)]
    pub alert: Option<Uuid>,
}

/// The `alerts.config` document.
///
/// Every field has a default, so an alert created with `{}` is a working
/// visitor-spike rule rather than an error. The names are the ones the settings
/// screen writes; `k` is accepted as an alias for `sigma` because that is what
/// the statistic is usually called in the literature and somebody will type it.
#[derive(Debug, Clone, Deserialize)]
pub struct AlertConfig {
    /// Set to `false` to keep the rule without evaluating it. `alerts` has no
    /// `enabled` column in `docs/CONTRACT.md` §2, so the switch lives here.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// How many standard deviations past the mean counts as abnormal.
    #[serde(default = "default_sigma", alias = "k")]
    pub sigma: f64,
    /// The window being judged, ending now.
    #[serde(default = "default_window")]
    pub window_minutes: u32,
    /// How many comparable periods make the baseline.
    #[serde(default = "default_weeks")]
    pub weeks: u32,
    /// Minimum quiet time between two firings of the same rule.
    #[serde(default = "default_cooldown")]
    pub cooldown_minutes: i64,
    /// Floor below which a proportional change is noise.
    #[serde(default = "default_min_events")]
    pub min_events: u64,
    /// `visitors` or `pageviews`, for `spike` and `drop`.
    #[serde(default)]
    pub metric: Option<String>,
    /// The agent slug an `agent_surge` rule watches.
    #[serde(default)]
    pub agent: Option<String>,
    /// How many times its own baseline an agent's rate must reach.
    #[serde(default = "default_factor")]
    pub factor: f64,
    /// A hard threshold: the gap ratio for `citation_gap`, an absolute count for
    /// `error`.
    #[serde(default)]
    pub threshold: Option<f64>,
    /// The goal a `goal` rule watches.
    #[serde(default)]
    pub goal: Option<Uuid>,
    /// Fire when conversions fall below this.
    #[serde(default)]
    pub below: Option<f64>,
    /// Fire when conversions rise above this.
    #[serde(default)]
    pub above: Option<f64>,
    /// Restrict a `citation_gap` rule to one page.
    #[serde(default)]
    pub pathname: Option<String>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        AlertConfig {
            enabled: true,
            sigma: default_sigma(),
            window_minutes: default_window(),
            weeks: default_weeks(),
            cooldown_minutes: default_cooldown(),
            min_events: default_min_events(),
            metric: None,
            agent: None,
            factor: default_factor(),
            threshold: None,
            goal: None,
            below: None,
            above: None,
            pathname: None,
        }
    }
}

fn yes() -> bool {
    true
}

/// Three sigma. Roughly one hour in a thousand on normally-distributed traffic,
/// which on a five-minute sweep is a false alarm every few days at two sigma and
/// every few months at three. Traffic is not normally distributed, so this is a
/// calibration rather than a probability, and it is the one that stopped the
/// sweep from crying wolf.
fn default_sigma() -> f64 {
    3.0
}

/// One hour, matching the unit the baseline is built in.
fn default_window() -> u32 {
    60
}

/// Four weeks. Enough for a mean to mean something, short enough that a site
/// which genuinely tripled last month is not still being compared with the
/// before.
fn default_weeks() -> u32 {
    4
}

/// An hour of quiet after a firing. A spike that lasts three hours is one event,
/// not thirty-six notifications.
fn default_cooldown() -> i64 {
    60
}

/// Ten. Below this, a proportional change is arithmetic rather than news.
fn default_min_events() -> u64 {
    10
}

/// Three times the baseline, from the product brief.
fn default_factor() -> f64 {
    3.0
}

/// How long an alert counts as "already failed to deliver" after a failure.
///
/// An hour, the default cooldown. Past that, a still-broken channel fails the
/// sweep once more, which puts it back in front of the operator in the logs.
const UNDELIVERED_MARK_SECS: u32 = 60 * 60;

/// What evaluating one rule came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Not tripped, disabled, or inside its cooldown.
    Quiet,
    /// Tripped and delivered (or tripped with nowhere to deliver to).
    Fired,
    /// Tripped, and no channel accepted it. `fresh` is false when the same
    /// alert already failed recently.
    Undelivered {
        /// Whether this is the first failure in [`UNDELIVERED_MARK_SECS`].
        fresh: bool,
    },
}

/// Evaluate one alert, or every alert on the install.
///
/// Fails when an alert fired and could not be delivered for the first time in
/// a while, so the worker retries the sweep soon. See the module docs for why a
/// repeated failure does not.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let rules = match payload.alert {
        Some(id) => vec![alerts::get_alert(&ctx.pg, id).await?],
        None => alerts::list_all_alerts(&ctx.pg).await?,
    };

    let mut evaluated = 0usize;
    let mut fired = 0usize;
    let mut undelivered = 0usize;
    let mut retry = 0usize;

    for rule in &rules {
        match evaluate(ctx, rule).await {
            Ok(Verdict::Fired) => {
                evaluated += 1;
                fired += 1;
            }
            Ok(Verdict::Quiet) => evaluated += 1,
            Ok(Verdict::Undelivered { fresh }) => {
                evaluated += 1;
                undelivered += 1;
                if fresh {
                    retry += 1;
                }
            }
            Err(e) => {
                // One misconfigured alert must not stop the other thirty being
                // evaluated.
                tracing::warn!(alert = %rule.id, kind = %rule.kind, error = %e, "alert not evaluated");
            }
        }
    }

    tracing::info!(
        alerts = rules.len(),
        evaluated,
        fired,
        undelivered,
        "alert sweep finished"
    );

    if retry > 0 {
        return Err(Error::Upstream(format!(
            "{retry} alert(s) fired and no channel accepted them; retrying"
        )));
    }
    Ok(())
}

/// Evaluate one rule, and deliver it if it tripped.
async fn evaluate(ctx: &JobCtx, rule: &alerts::Alert) -> Result<Verdict> {
    let config = read_config(&rule.config)?;
    if !config.enabled {
        return Ok(Verdict::Quiet);
    }

    let site = sites::get(&ctx.pg, rule.site_id).await?;
    let site_id = site.site_id_u16();
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);

    let now = Utc::now();
    let window = Duration::minutes(i64::from(config.window_minutes.clamp(5, 24 * 60)));
    let start = now - window;

    let tripped = match rule.kind.as_str() {
        "spike" => threshold_crossed(ctx, site_id, tz, &config, now, start, true).await?,
        "drop" => threshold_crossed(ctx, site_id, tz, &config, now, start, false).await?,
        "error" => error_surge(ctx, site_id, tz, &config, now, start).await?,
        "agent_surge" => agent_surge(ctx, site_id, tz, &config, now, start).await?,
        "citation_gap" => citation_gap(ctx, site_id, tz, &config, now).await?,
        "goal" => goal_crossed(ctx, &site, site_id, tz, &config, now, start).await?,
        other => {
            return Err(Error::validation(
                "kind",
                format!("`{other}` is not an alert kind this build evaluates"),
            ))
        }
    };

    let Some(finding) = tripped else {
        return Ok(Verdict::Quiet);
    };

    // The cooldown is claimed, not checked. Whichever worker's update matches the
    // row is the one that sends; a second worker evaluating the same rule in the
    // same second gets `None` and stops here. The claim is handed back below if
    // the delivery reaches nobody.
    let cooldown = config.cooldown_minutes.max(0) * 60;
    let Some(claimed_at) = alerts::claim_alert(&ctx.pg, rule.id, cooldown).await? else {
        tracing::debug!(alert = %rule.id, "alert tripped but is inside its cooldown");
        return Ok(Verdict::Quiet);
    };

    let payload = AlertPayload {
        kind: rule.kind.clone(),
        headline: finding.headline,
        detail: finding.detail,
        metric: finding.metric,
        observed: finding.observed,
        expected: finding.expected,
        link: notify::dashboard_link(ctx, site_id, finding.screen),
    };

    let channels = Channels::from_json(&rule.channels);
    if channels.is_empty() {
        tracing::warn!(alert = %rule.id, "alert fired with no channel configured");
        return Ok(Verdict::Fired);
    }

    let delivered = notify::deliver_alert(ctx, &site.name, &channels, &payload).await;
    if delivered == 0 {
        // Nobody was told, so the cooldown has not been earned. Handing it back
        // lets the next sweep, or the retry of this one, fire it again.
        if let Err(e) =
            alerts::release_alert(&ctx.pg, rule.id, claimed_at, rule.last_fired_at).await
        {
            tracing::warn!(alert = %rule.id, error = %e, "could not release the alert's cooldown");
        }
        let fresh = mark_undelivered(ctx, rule.id).await;
        tracing::error!(
            alert = %rule.id,
            kind = %rule.kind,
            first_failure = fresh,
            "the alert fired and no channel accepted it; it will be retried"
        );
        return Ok(Verdict::Undelivered { fresh });
    }

    clear_undelivered(ctx, rule.id).await;
    tracing::info!(alert = %rule.id, kind = %rule.kind, delivered, "alert fired");
    Ok(Verdict::Fired)
}

/// Valkey key that remembers an alert recently failed to deliver.
fn undelivered_key(id: Uuid) -> String {
    format!("mf:alert:undelivered:{id}")
}

/// Note an undelivered alert. Returns whether this is a fresh failure.
///
/// A Valkey that cannot be read counts as fresh: failing the sweep once too
/// often is a smaller harm than never retrying an alert nobody received.
async fn mark_undelivered(ctx: &JobCtx, id: Uuid) -> bool {
    let key = undelivered_key(id);
    if let Ok(Some(_)) = ctx.kv.get_json::<i64>(&key).await {
        return false;
    }
    let _ = ctx
        .kv
        .set_json(&key, &Utc::now().timestamp(), Some(UNDELIVERED_MARK_SECS))
        .await;
    true
}

/// Forget an earlier delivery failure once the alert has been delivered.
async fn clear_undelivered(ctx: &JobCtx, id: Uuid) {
    let _ = ctx.kv.del(&undelivered_key(id)).await;
}

/// Read an `alerts.config` document.
///
/// An absent or empty document is the default rule, because every field has a
/// default and `{}` is a legitimate way to write "watch visitors, the usual way".
///
/// A document that will not parse is an **error**, not a fallback to the default.
/// Substituting defaults for a config nobody can read would mean an alert firing
/// on a rule the operator never wrote — a notification about visitors to somebody
/// who configured a gap threshold and mistyped it. The sweep logs the failure
/// against the alert's own id and moves on, which leaves the rule visibly
/// unevaluated instead of quietly redefined.
fn read_config(raw: &serde_json::Value) -> Result<AlertConfig> {
    if raw.is_null() {
        return Ok(AlertConfig::default());
    }
    serde_json::from_value(raw.clone()).map_err(|e| {
        Error::validation(
            "config",
            format!("this alert's configuration is not usable: {e}"),
        )
    })
}

/// What tripped, in the words the notification will use.
struct Finding {
    headline: String,
    detail: String,
    metric: String,
    observed: f64,
    expected: f64,
    /// The dashboard screen that shows this, relative to the site.
    screen: &'static str,
}

/// `spike` and `drop`: traffic outside the normal range for this hour.
///
/// The baseline is built in hours, so a window shorter or longer than an hour is
/// scaled to match. That scaling is linear and therefore an approximation — half
/// an hour of a busy hour is not reliably half its traffic — which is why the
/// default window is exactly one hour and the approximation never applies unless
/// somebody deliberately changed it.
async fn threshold_crossed(
    ctx: &JobCtx,
    site: u16,
    tz: Tz,
    config: &AlertConfig,
    now: DateTime<Utc>,
    start: DateTime<Utc>,
    upward: bool,
) -> Result<Option<Finding>> {
    let pulse = match config.metric.as_deref() {
        Some("pageviews") => Pulse::Pageviews,
        _ => Pulse::Visitors,
    };

    let observed = metrics::pulse_in(&ctx.ch, site, &pulse, start, now).await? as f64;
    let baseline = scaled_baseline(ctx, site, &pulse, tz, now, config).await?;
    if !baseline.is_usable() {
        return Ok(None);
    }

    let metric = pulse.label();
    if upward {
        let edge = baseline.upper(config.sigma);
        if observed <= edge || observed < config.min_events as f64 {
            return Ok(None);
        }
        return Ok(Some(Finding {
            headline: format!("{metric} spiked to {}", round(observed)),
            detail: format!(
                "{} {metric} in the last {} minutes. The same hour on the previous {} weeks averaged {}.",
                round(observed),
                config.window_minutes,
                baseline.samples,
                round(baseline.mean)
            ),
            metric,
            observed,
            expected: baseline.mean,
            screen: "",
        }));
    }

    let edge = baseline.lower(config.sigma);
    // There has to have been something to fall from. A site that normally sees
    // four visitors an hour cannot meaningfully "drop".
    if observed >= edge || baseline.mean < config.min_events as f64 {
        return Ok(None);
    }
    Ok(Some(Finding {
        headline: format!("{metric} fell to {}", round(observed)),
        detail: format!(
            "{} {metric} in the last {} minutes. The same hour on the previous {} weeks averaged {}.",
            round(observed),
            config.window_minutes,
            baseline.samples,
            round(baseline.mean)
        ),
        metric,
        observed,
        expected: baseline.mean,
        screen: "",
    }))
}

/// `error`: more client-side exceptions than this hour usually carries.
///
/// An absolute `threshold` short-circuits the statistics when it is configured,
/// because "tell me when anything at all breaks after a release" is a legitimate
/// rule and no baseline can express it.
async fn error_surge(
    ctx: &JobCtx,
    site: u16,
    tz: Tz,
    config: &AlertConfig,
    now: DateTime<Utc>,
    start: DateTime<Utc>,
) -> Result<Option<Finding>> {
    let observed = metrics::pulse_in(&ctx.ch, site, &Pulse::Errors, start, now).await? as f64;

    let (expected, tripped) = match config.threshold {
        Some(limit) => (limit, observed > limit),
        None => {
            let baseline = scaled_baseline(ctx, site, &Pulse::Errors, tz, now, config).await?;
            if !baseline.is_usable() {
                return Ok(None);
            }
            let edge = baseline.upper(config.sigma);
            (
                baseline.mean,
                observed > edge && observed >= config.min_events as f64,
            )
        }
    };

    if !tripped {
        return Ok(None);
    }

    Ok(Some(Finding {
        headline: format!(
            "{} errors in the last {} minutes",
            round(observed),
            config.window_minutes
        ),
        detail: format!(
            "Usually {} in this window. Something shipped, or something upstream changed.",
            round(expected)
        ),
        metric: "errors".to_string(),
        observed,
        expected,
        screen: "errors",
    }))
}

/// `agent_surge`: one named agent crawling far harder than it normally does.
///
/// A multiple of its own baseline rather than a standard-deviation test, because
/// crawl traffic is bursty by nature: an agent that fetches nothing for six days
/// and then takes a thousand pages has a standard deviation that makes every
/// crawl look abnormal. Three times its own trailing rate is the brief's own
/// threshold and it is the one that reads as news.
async fn agent_surge(
    ctx: &JobCtx,
    site: u16,
    tz: Tz,
    config: &AlertConfig,
    now: DateTime<Utc>,
    start: DateTime<Utc>,
) -> Result<Option<Finding>> {
    let agent = config.agent.clone().ok_or_else(|| {
        Error::validation("config.agent", "an agent_surge alert must name an agent")
    })?;

    let pulse = Pulse::AgentFetches(agent.clone());
    let observed = metrics::pulse_in(&ctx.ch, site, &pulse, start, now).await? as f64;
    let baseline = scaled_baseline(ctx, site, &pulse, tz, now, config).await?;
    if !baseline.is_usable() {
        return Ok(None);
    }

    let factor = if config.factor > 0.0 {
        config.factor
    } else {
        default_factor()
    };
    let edge = baseline.mean * factor;

    if observed <= edge || observed < config.min_events as f64 {
        return Ok(None);
    }

    Ok(Some(Finding {
        headline: format!("{agent} is crawling {}x its usual rate", round(observed / baseline.mean.max(1.0))),
        detail: format!(
            "{} fetches in the last {} minutes, against a usual {}. Check the policy screen if this is not welcome.",
            round(observed),
            config.window_minutes,
            round(baseline.mean)
        ),
        metric: pulse.label(),
        observed,
        expected: edge,
        screen: "agents",
    }))
}

/// `citation_gap`: a page being taken far more than it gives back.
///
/// The signature metric, as an alert. It reads the worst page in the window
/// rather than the site average, because the site average hides exactly the case
/// this is for: one documentation page feeding every answer engine and returning
/// nobody.
///
/// The rows come from [`micaforge_query::gap::by_page`], the same call behind
/// `GET /api/citation-gap`, so the number in the notification is the number on
/// the screen the notification links to.
async fn citation_gap(
    ctx: &JobCtx,
    site: u16,
    tz: Tz,
    config: &AlertConfig,
    now: DateTime<Utc>,
) -> Result<Option<Finding>> {
    /// The gap is a slow-moving number. Measuring it over the alert's own short
    /// window would report noise; a week is the shortest span over which "this
    /// page is being taken for free" is a fact rather than a bad afternoon.
    const GAP_WINDOW_DAYS: i64 = 7;

    /// Pages considered. The engine ranks by fetches, so the worst offender is
    /// always inside the first page of results.
    const SCAN_ROWS: u32 = 200;

    let threshold = config.threshold.unwrap_or(100.0);
    let start = now - Duration::days(GAP_WINDOW_DAYS);

    let mut envelope = QueryEnvelope::new(site, TimeRange::new(start, now, tz));
    envelope.limit = SCAN_ROWS;

    let worst = gap::by_page(&ctx.ch, &envelope)
        .await?
        .into_iter()
        .filter(|row| {
            config
                .pathname
                .as_ref()
                .is_none_or(|only| &row.path == only)
        })
        .filter(|row| row.agent_fetches >= config.min_events && row.ratio >= threshold)
        .max_by(|a, b| {
            a.ratio
                .partial_cmp(&b.ratio)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

    let Some(row) = worst else {
        return Ok(None);
    };

    Ok(Some(Finding {
        headline: format!(
            "{} is being taken {} times for every reader it returns",
            row.path,
            round(row.ratio)
        ),
        detail: format!(
            "{} agent fetches and {} answer-engine arrivals over the last {GAP_WINDOW_DAYS} days.",
            row.agent_fetches, row.answer_arrivals
        ),
        metric: "gap ratio".to_string(),
        observed: row.ratio,
        expected: threshold,
        screen: "citation-gap",
    }))
}

/// `goal`: conversions outside the range the operator expects.
///
/// A goal alert with neither bound configured does nothing, and says so once at
/// debug rather than firing on every sweep. Guessing a threshold on the
/// operator's behalf would mean sending them a notification about a number they
/// never asked to be told about.
///
/// The conversion count comes from [`micaforge_query::stats::goals`], which
/// compiles the goal's own `conditions` filter tree — the same document the goals
/// screen shows. Reimplementing even a subset of that here would mean an alert
/// could fire on a definition of the goal that nobody had ever seen.
async fn goal_crossed(
    ctx: &JobCtx,
    site: &sites::Site,
    site_id: u16,
    tz: Tz,
    config: &AlertConfig,
    now: DateTime<Utc>,
    start: DateTime<Utc>,
) -> Result<Option<Finding>> {
    let goal_id = config
        .goal
        .ok_or_else(|| Error::validation("config.goal", "a goal alert must name a goal"))?;

    if config.below.is_none() && config.above.is_none() {
        tracing::debug!(site = %site.domain, "goal alert has no threshold; nothing to test");
        return Ok(None);
    }

    let goal = goals::get(&ctx.pg, goal_id).await?;
    let definition = stats::GoalDef::parse(
        goal.id.to_string(),
        goal.name.clone(),
        &goal.kind,
        &goal.conditions.to_string(),
        goal.value_cents,
    )?;

    let envelope = QueryEnvelope::new(site_id, TimeRange::new(start, now, tz));
    let rows = stats::goals(&ctx.ch, &envelope, std::slice::from_ref(&definition)).await?;
    let observed = rows.first().map(|row| row.conversions).unwrap_or(0) as f64;

    let breached = match (config.below, config.above) {
        (Some(floor), _) if observed < floor => Some((floor, "Below")),
        (_, Some(ceiling)) if observed > ceiling => Some((ceiling, "Above")),
        _ => None,
    };

    let Some((edge, direction)) = breached else {
        return Ok(None);
    };

    Ok(Some(Finding {
        headline: format!("{} converted {} times", goal.name, round(observed)),
        detail: format!(
            "{direction} the {} you asked to be told about, over the last {} minutes.",
            round(edge),
            config.window_minutes
        ),
        metric: goal.name.clone(),
        observed,
        expected: edge,
        screen: "goals",
    }))
}

/// The weekly baseline, scaled to the configured window length.
async fn scaled_baseline(
    ctx: &JobCtx,
    site: u16,
    pulse: &Pulse,
    tz: Tz,
    now: DateTime<Utc>,
    config: &AlertConfig,
) -> Result<Baseline> {
    let hourly = metrics::weekly_baseline(&ctx.ch, site, pulse, tz, now, config.weeks).await?;
    let scale = f64::from(config.window_minutes.clamp(5, 24 * 60)) / 60.0;

    Ok(Baseline {
        mean: hourly.mean * scale,
        stddev: hourly.stddev * scale,
        samples: hourly.samples,
    })
}

/// A number as a person would write it in a sentence.
///
/// Whole numbers lose their decimal point, and everything else keeps one place. A
/// notification that says "visitors fell to 41.00000000000001" has undermined
/// itself before it has been read.
fn round(value: f64) -> String {
    if (value - value.round()).abs() < 0.05 {
        format!("{}", value.round() as i64)
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value as Json};

    #[test]
    fn undelivered_markers_are_per_alert_and_short_lived() {
        let a = Uuid::nil();
        let b = Uuid::from_u128(1);
        assert_ne!(undelivered_key(a), undelivered_key(b));
        assert!(undelivered_key(a).starts_with("mf:alert:undelivered:"));
        // Longer than the sweep, or every sweep would be a "fresh" failure.
        const { assert!(UNDELIVERED_MARK_SECS > 5 * 60) };
    }

    #[test]
    fn an_empty_config_is_a_working_visitor_rule() {
        let config: AlertConfig = serde_json::from_value(json!({})).unwrap();
        assert!(config.enabled);
        assert_eq!(config.sigma, 3.0);
        assert_eq!(config.window_minutes, 60);
        assert_eq!(config.weeks, 4);
        assert_eq!(config.factor, 3.0);
        assert!(config.metric.is_none());
    }

    #[test]
    fn the_statistician_spelling_of_sigma_is_accepted() {
        let config: AlertConfig = serde_json::from_value(json!({ "k": 2.5 })).unwrap();
        assert_eq!(config.sigma, 2.5);
    }

    #[test]
    fn a_config_nobody_can_read_is_refused_rather_than_defaulted() {
        // The failure this prevents: a mistyped gap threshold silently becoming a
        // visitor-spike rule, which then fires at somebody who never asked for it.
        assert!(read_config(&json!({ "threshold": "high" })).is_err());
        assert!(read_config(&json!("nonsense")).is_err());

        // An absent or empty document is still the default rule.
        assert!(read_config(&Json::Null).unwrap().enabled);
        assert_eq!(read_config(&json!({})).unwrap().sigma, 3.0);
    }

    #[test]
    fn a_rule_can_be_kept_without_being_evaluated() {
        let config: AlertConfig = serde_json::from_value(json!({ "enabled": false })).unwrap();
        assert!(!config.enabled);
    }

    #[test]
    fn numbers_are_written_the_way_a_sentence_would_write_them() {
        assert_eq!(round(41.0), "41");
        assert_eq!(round(41.000000000001), "41");
        assert_eq!(round(41.37), "41.4");
        assert_eq!(round(0.0), "0");
        assert_eq!(round(-3.0), "-3");
    }

    #[test]
    fn a_shorter_window_scales_the_baseline_it_is_compared_with() {
        // The arithmetic that makes a fifteen-minute window comparable with an
        // hourly baseline, checked directly because the query it wraps cannot be.
        let hourly = Baseline {
            mean: 120.0,
            stddev: 20.0,
            samples: 4,
        };
        let scale = 15.0 / 60.0;
        assert_eq!(hourly.mean * scale, 30.0);
        assert_eq!(hourly.stddev * scale, 5.0);
    }
}
