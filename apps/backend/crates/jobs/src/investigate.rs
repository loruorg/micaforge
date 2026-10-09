//! `investigate` — find the changes worth a look, and keep each one open until
//! it is understood or gone.
//!
//! # What counts as a change
//!
//! Three detectors, all statistical, all cheap, all run every few hours:
//!
//! - **Week over week** on the five numbers a site owner reads first:
//!   visitors, pageviews, bounce rate, AI crawler fetches and visits from AI
//!   answers. The last seven days against the seven before, ending now, so a
//!   weekday is always compared with a weekday.
//! - **Movers** inside the three dimensions that explain most changes:
//!   channel, source and page. The rows that rose or fell by half or more,
//!   against a floor that keeps a page going from two visitors to five out of
//!   anybody's inbox.
//! - **The last hour** against the same hour of the four weeks before, three
//!   standard deviations either side: exactly the test the alert sweep makes
//!   ([`crate::metrics::weekly_baseline`]), so an investigation and an alert
//!   about the same hour can never disagree about whether it was unusual.
//!
//! Every threshold here is a floor on *evidence*, not on interest. A change
//! below it may be real; it is not yet shown to be, and an investigation
//! opened on noise is a cost the reader pays and the product does not.
//!
//! # The life of an investigation
//!
//! [`next_state`] is the whole rule, and it is a pure function so it can be
//! tested without a database. A re-check that still finds the change keeps it
//! `open`; the first calm re-check moves it to `monitoring`; the second calm
//! one in a row resolves it. A change that comes back while monitoring
//! reopens the same investigation; one that comes back after it was resolved
//! opens a new one, because that is a new event.
//!
//! # Explanations
//!
//! With a model configured, a new investigation is handed to the AI analyst
//! with the change as the question and the same tools, bounds and daily
//! allowance as a person asking in the dashboard. Its tool calls are stored as
//! evidence beside the detector's own queries. Without a model the
//! investigation is still opened and updated with the statistical findings,
//! and says the explanation needs AI configured.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Timelike, Utc};
use chrono_tz::Tz;
use micaforge_ai::{BaseQuery, Llm, Registry, ToolCtx, ToolTrace};
use micaforge_core::{
    filters, new_id, Audience, Dimension, Error, Filter, FilterGroup, FilterNode, FilterOp,
    FilterValue, Metric, QueryEnvelope, Result, TimeRange,
};
use micaforge_query::{exec, stats};
use micaforge_store::models::investigations::{self, Investigation, Measure};
use micaforge_store::models::{chat, sites};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use uuid::Uuid;

use crate::metrics::{self, Pulse};
use crate::JobCtx;

/// Which sites to scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One site, from the dashboard's "Scan now". Absent is every site, which
    /// is what the recurring sweep queues.
    #[serde(default)]
    pub site: Option<Uuid>,
}

/// The week-over-week window.
const WEEK: i64 = 7;

/// How long until a live investigation is checked again.
const RECHECK: i64 = 24;

/// Calm re-checks in a row that resolve an investigation.
const CALM_TO_RESOLVE: i32 = 2;

/// Most new explanations asked for per site per sweep. The model is the
/// expensive part of this job, and a site that changed in ten ways at once
/// is better served by three good explanations than ten hurried ones.
const MAX_EXPLANATIONS: usize = 3;

/// Most movers opened per dimension per sweep.
const MAX_MOVERS: usize = 3;

/// How long one explanation may take, model calls and reads together.
const EXPLAIN_DEADLINE: StdDuration = StdDuration::from_secs(120);

/// The dimensions movers are looked for in.
const MOVER_DIMENSIONS: &[&str] = &["channel", "source", "pathname"];

/// How serious a change is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing.
    Info,
    /// Worth a look this week.
    Warning,
    /// Worth a look today.
    Critical,
}

impl Severity {
    /// The `investigations.severity` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }
}

/// The site-level numbers the week-over-week detector watches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Total {
    /// Distinct visitors.
    Visitors,
    /// Pageviews.
    Pageviews,
    /// Bounce rate, in percent.
    BounceRate,
    /// Fetches by named AI crawlers.
    AgentFetches,
    /// Sessions that arrived from an AI answer.
    AnswerArrivals,
}

impl Total {
    const ALL: [Total; 5] = [
        Total::Visitors,
        Total::Pageviews,
        Total::BounceRate,
        Total::AgentFetches,
        Total::AnswerArrivals,
    ];

    fn key(&self) -> &'static str {
        match self {
            Total::Visitors => "visitors",
            Total::Pageviews => "pageviews",
            Total::BounceRate => "bounce_rate",
            Total::AgentFetches => "agent_fetches",
            Total::AnswerArrivals => "answer_arrivals",
        }
    }

    fn words(&self) -> &'static str {
        match self {
            Total::Visitors => "Visitors",
            Total::Pageviews => "Pageviews",
            Total::BounceRate => "Bounce rate",
            Total::AgentFetches => "AI crawler fetches",
            Total::AnswerArrivals => "Visits from AI answers",
        }
    }

    fn parse(raw: &str) -> Option<Total> {
        Total::ALL.into_iter().find(|t| t.key() == raw)
    }
}

/// What one investigation measures, recoverable from its `signal_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// `wow:<total>`.
    Total(Total),
    /// `wow:<dimension>=<value>`: visitors from one row of a dimension.
    Mover {
        /// `channel`, `source` or `pathname`.
        dimension: String,
        /// The row.
        value: String,
    },
    /// `hour:visitors`: the last full hour against the same hour in past weeks.
    Hourly,
}

impl Probe {
    /// The stable key an investigation is found again by.
    pub fn key(&self) -> String {
        match self {
            Probe::Total(total) => format!("wow:{}", total.key()),
            Probe::Mover { dimension, value } => format!("wow:{dimension}={value}"),
            Probe::Hourly => "hour:visitors".to_string(),
        }
    }

    /// Read a key back. `None` for a key this build does not write.
    pub fn from_key(key: &str) -> Option<Probe> {
        if key == "hour:visitors" {
            return Some(Probe::Hourly);
        }
        let rest = key.strip_prefix("wow:")?;
        if let Some((dimension, value)) = rest.split_once('=') {
            if MOVER_DIMENSIONS.contains(&dimension) {
                return Some(Probe::Mover {
                    dimension: dimension.to_string(),
                    value: value.to_string(),
                });
            }
            return None;
        }
        Total::parse(rest).map(Probe::Total)
    }

    /// The metric column an investigation stores.
    fn metric(&self) -> &'static str {
        match self {
            Probe::Total(total) => total.key(),
            Probe::Mover { .. } | Probe::Hourly => "visitors",
        }
    }
}

/// One measurement of a probe.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// The latest value.
    pub current: f64,
    /// What it is compared with.
    pub baseline: f64,
    /// Change in percent, `None` against zero.
    pub change_pct: Option<f64>,
    /// `Some` when the change clears the evidence floor.
    pub severity: Option<Severity>,
    /// What was compared with what.
    pub window: String,
    /// The reads behind it.
    pub evidence: ToolTrace,
}

impl Measurement {
    fn direction(&self) -> &'static str {
        if self.current >= self.baseline {
            "up"
        } else {
            "down"
        }
    }
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// Change in percent, rounded to one decimal; `None` against a zero baseline.
pub fn change_pct(current: f64, baseline: f64) -> Option<f64> {
    if baseline == 0.0 {
        return None;
    }
    Some(((current - baseline) / baseline * 1000.0).round() / 10.0)
}

/// Whether a week-over-week change in a site total is significant, and how much.
///
/// The floor is on the larger of the two windows: a fall from 500 to 0 must
/// clear it as surely as a rise from 0 to 500.
pub fn judge_total(total: Total, current: f64, baseline: f64) -> Option<Severity> {
    let larger = current.max(baseline);
    if total == Total::BounceRate {
        // Percentage points, not percent of a percentage.
        let points = (current - baseline).abs();
        return match points {
            p if p >= 20.0 => Some(Severity::Critical),
            p if p >= 10.0 => Some(Severity::Warning),
            _ => None,
        };
    }
    let floor = match total {
        Total::Visitors | Total::Pageviews => 50.0,
        Total::AgentFetches => 200.0,
        Total::AnswerArrivals => 20.0,
        Total::BounceRate => 0.0,
    };
    if larger < floor {
        return None;
    }
    match change_pct(current, baseline) {
        None => Some(Severity::Warning),
        Some(pct) if pct.abs() >= 60.0 => Some(Severity::Critical),
        Some(pct) if pct.abs() >= 30.0 => Some(Severity::Warning),
        _ => None,
    }
}

/// Whether one row of a dimension moved enough to investigate.
///
/// `site_visitors` is the larger of the two windows' site totals; a row must
/// carry at least a twentieth of it (and thirty visitors) before its change
/// means anything about the site.
pub fn judge_mover(current: f64, baseline: f64, site_visitors: f64) -> Option<Severity> {
    let floor = (site_visitors * 0.05).max(30.0);
    if current.max(baseline) < floor {
        return None;
    }
    match change_pct(current, baseline) {
        None => Some(Severity::Info),
        Some(pct) if pct.abs() >= 100.0 => Some(Severity::Warning),
        Some(pct) if pct.abs() >= 50.0 => Some(Severity::Info),
        _ => None,
    }
}

/// An investigation's state after a re-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Next {
    /// `open`, `monitoring` or `resolved`.
    pub status: &'static str,
    /// Calm re-checks in a row.
    pub calm_checks: i32,
    /// Hours until the next re-check; `None` once resolved.
    pub recheck_in: Option<i64>,
}

/// The whole life-cycle rule. See the module notes.
pub fn next_state(status: &str, calm_checks: i32, still_significant: bool) -> Next {
    if status == "resolved" {
        return Next {
            status: "resolved",
            calm_checks,
            recheck_in: None,
        };
    }
    if still_significant {
        return Next {
            status: "open",
            calm_checks: 0,
            recheck_in: Some(RECHECK),
        };
    }
    let calm = calm_checks + 1;
    if calm >= CALM_TO_RESOLVE {
        Next {
            status: "resolved",
            calm_checks: calm,
            recheck_in: None,
        }
    } else {
        Next {
            status: "monitoring",
            calm_checks: calm,
            recheck_in: Some(RECHECK),
        }
    }
}

/// The title of an investigation, from its probe and measurement.
pub fn title_for(probe: &Probe, m: &Measurement) -> String {
    let verb = if m.direction() == "up" {
        "rose"
    } else {
        "fell"
    };
    match probe {
        Probe::Total(total) => format!("{} {verb}", total.words()),
        Probe::Mover { dimension, value } => {
            let shown = if value.is_empty() {
                "(not set)"
            } else {
                value.as_str()
            };
            let what = match dimension.as_str() {
                "channel" => format!("the {shown} channel"),
                _ => shown.to_string(),
            };
            match dimension.as_str() {
                "pathname" => format!("Visitors to {what} {verb}"),
                _ => format!("Visitors from {what} {verb}"),
            }
        }
        Probe::Hourly => {
            if m.direction() == "up" {
                "Visitors spiked in the last hour".to_string()
            } else {
                "Visitors dropped in the last hour".to_string()
            }
        }
    }
}

/// The change in words, for a chat post and the digest.
pub fn change_words(m: &Measurement) -> String {
    match m.change_pct {
        Some(pct) => format!(
            "{}{pct}% ({} against {}, {})",
            if pct > 0.0 { "+" } else { "" },
            round(m.current),
            round(m.baseline),
            m.window
        ),
        None => format!("{} from nothing, {}", round(m.current), m.window),
    }
}

fn round(v: f64) -> String {
    if v.fract().abs() < 0.05 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

// ---------------------------------------------------------------------------
// The job
// ---------------------------------------------------------------------------

/// Scan, then re-check what is due.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let sites = match payload.site {
        Some(id) => vec![sites::get(&ctx.pg, id).await?],
        None => sites::list_all(&ctx.pg).await?,
    };
    let llm = Llm::from_config(&ctx.cfg);
    let registry = Registry::builtin();
    let now = Utc::now();

    let mut failed = 0usize;
    for site in &sites {
        if let Err(e) = scan_site(ctx, llm.as_ref(), &registry, site, now).await {
            failed += 1;
            tracing::warn!(site = site.site_id, error = %e, "investigation scan failed");
        }
    }

    for inv in investigations::due(&ctx.pg, now, 500).await? {
        if payload.site.is_some_and(|only| only != inv.site_id) {
            continue;
        }
        if let Err(e) = recheck(ctx, &inv, now).await {
            tracing::warn!(investigation = %inv.id, error = %e, "investigation re-check failed");
        }
    }

    // Every site failing is the event store being away, and worth a retry. A
    // few failing is a few sites with a problem of their own, logged above.
    if !sites.is_empty() && failed == sites.len() {
        return Err(Error::Upstream(format!(
            "no site could be scanned ({failed} failures)"
        )));
    }
    Ok(())
}

/// Detect, open or update, explain, post.
async fn scan_site(
    ctx: &JobCtx,
    llm: Option<&Llm>,
    registry: &Registry,
    site: &sites::Site,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut found: Vec<(Probe, Measurement)> = Vec::new();

    for total in Total::ALL {
        let probe = Probe::Total(total);
        let m = measure(ctx, site, &probe, now).await?;
        if m.severity.is_some() {
            found.push((probe, m));
        }
    }
    found.extend(movers(ctx, site, now).await?);
    let hourly = measure(ctx, site, &Probe::Hourly, now).await?;
    if hourly.severity.is_some() {
        found.push((Probe::Hourly, hourly));
    }

    let mut explained = 0usize;
    for (probe, m) in found {
        let key = probe.key();
        match investigations::find_live(&ctx.pg, site.id, &key).await? {
            Some(live) => {
                let title = title_for(&probe, &m);
                let findings = findings_of(&format!("{title} ({})", m.window), &m);
                let measure = measure_row(&title, &m, &findings, &live.evidence);
                investigations::record_measure(&ctx.pg, live.id, &measure).await?;
                if live.status == "monitoring" {
                    let reopened = investigations::set_status(
                        &ctx.pg,
                        live.id,
                        "open",
                        0,
                        Some(now + Duration::hours(RECHECK)),
                    )
                    .await?;
                    let summary = format!("Back to open: {}", change_words(&m));
                    event(
                        ctx,
                        live.id,
                        "status",
                        &summary,
                        json!({ "status": "open" }),
                    )
                    .await?;
                    post(ctx, site, &reopened, &summary).await;
                }
            }
            None => {
                let explain = llm.filter(|_| explained < MAX_EXPLANATIONS);
                if explain.is_some() {
                    explained += 1;
                }
                open(ctx, explain, registry, site, &probe, &m, now).await?;
            }
        }
    }
    Ok(())
}

/// The rows of channel, source and page that moved.
async fn movers(
    ctx: &JobCtx,
    site: &sites::Site,
    now: DateTime<Utc>,
) -> Result<Vec<(Probe, Measurement)>> {
    let (current, previous) = weeks(site, now);
    let site_now = stats::overview(&ctx.ch, &current).await?;
    let site_before = stats::overview(&ctx.ch, &previous).await?;
    let site_visitors = site_now.visitors.max(site_before.visitors) as f64;
    if site_visitors < 30.0 {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for name in MOVER_DIMENSIONS {
        let dimension = Dimension::parse(name)?;
        let mut cur_env = current.clone();
        cur_env.limit = 15;
        let mut prev_env = previous.clone();
        prev_env.limit = 15;
        let metrics = [Metric::Visitors];
        let ((now_rows, before_rows), queries) = exec::recording(async {
            let now_rows = stats::breakdown(&ctx.ch, &cur_env, &dimension, &metrics).await;
            let before_rows = stats::breakdown(&ctx.ch, &prev_env, &dimension, &metrics).await;
            (now_rows, before_rows)
        })
        .await;
        let (now_rows, before_rows) = (now_rows?, before_rows?);

        let mut keys: Vec<String> = now_rows.iter().map(|r| r.key.clone()).collect();
        for row in &before_rows {
            if !keys.contains(&row.key) {
                keys.push(row.key.clone());
            }
        }
        let mut candidates: Vec<(Probe, Measurement)> = Vec::new();
        for key in keys {
            let cur = now_rows
                .iter()
                .find(|r| r.key == key)
                .map(|r| r.visitors)
                .unwrap_or(0) as f64;
            let base = before_rows
                .iter()
                .find(|r| r.key == key)
                .map(|r| r.visitors)
                .unwrap_or(0) as f64;
            let Some(severity) = judge_mover(cur, base, site_visitors) else {
                continue;
            };
            let probe = Probe::Mover {
                dimension: (*name).to_string(),
                value: key.clone(),
            };
            candidates.push((
                probe,
                Measurement {
                    current: cur,
                    baseline: base,
                    change_pct: change_pct(cur, base),
                    severity: Some(severity),
                    window: "7 days against the previous 7".to_string(),
                    evidence: detector_trace(
                        "detect_movers",
                        json!({ "dimension": name }),
                        format!(
                            "GET /api/stats/breakdown?site={}&dimension={name}&range=7d&compare=previous",
                            site.site_id
                        ),
                        queries.clone(),
                    ),
                },
            ));
        }
        // The largest absolute moves first; a dimension contributes a few.
        candidates.sort_by(|a, b| {
            let da = (a.1.current - a.1.baseline).abs();
            let db = (b.1.current - b.1.baseline).abs();
            db.total_cmp(&da)
        });
        out.extend(candidates.into_iter().take(MAX_MOVERS));
    }
    Ok(out)
}

/// The current and previous seven days, ending now, in the site's zone.
fn weeks(site: &sites::Site, now: DateTime<Utc>) -> (QueryEnvelope, QueryEnvelope) {
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let start = now - Duration::days(WEEK);
    let before = start - Duration::days(WEEK);
    let current = QueryEnvelope::new(site.site_id_u16(), TimeRange::new(start, now, tz));
    let previous = QueryEnvelope::new(site.site_id_u16(), TimeRange::new(before, start, tz));
    (current, previous)
}

/// Measure one probe now.
pub async fn measure(
    ctx: &JobCtx,
    site: &sites::Site,
    probe: &Probe,
    now: DateTime<Utc>,
) -> Result<Measurement> {
    match probe {
        Probe::Total(total) => measure_total(ctx, site, *total, now).await,
        Probe::Mover { dimension, value } => {
            let (mut current, mut previous) = weeks(site, now);
            // An empty row is the "(not set)" row, which `is ""` cannot name.
            let filter = if value.is_empty() {
                Filter::new(
                    Dimension::parse(dimension)?,
                    FilterOp::IsNotSet,
                    FilterValue::default(),
                )
            } else {
                Filter::new(
                    Dimension::parse(dimension)?,
                    FilterOp::Is,
                    FilterValue::One(value.clone()),
                )
            };
            let group = FilterGroup::and(vec![FilterNode::Leaf(filter)]);
            current.filters = Some(group.clone());
            previous.filters = Some(group.clone());
            let ((now_view, before_view), queries) = exec::recording(async {
                (
                    stats::overview(&ctx.ch, &current).await,
                    stats::overview(&ctx.ch, &previous).await,
                )
            })
            .await;
            let (cur, base) = (now_view?.visitors as f64, before_view?.visitors as f64);
            let site_total = stats::overview(&ctx.ch, &weeks(site, now).0)
                .await?
                .visitors as f64;
            let filters_text = filters::serialize_filters(&group).unwrap_or_default();
            Ok(Measurement {
                current: cur,
                baseline: base,
                change_pct: change_pct(cur, base),
                severity: judge_mover(cur, base, site_total),
                window: "7 days against the previous 7".to_string(),
                evidence: detector_trace(
                    "measure_mover",
                    json!({ "dimension": dimension, "value": value }),
                    format!(
                        "GET /api/stats/overview?site={}&range=7d&compare=previous&filters={}",
                        site.site_id,
                        url_encode(&filters_text)
                    ),
                    queries,
                ),
            })
        }
        Probe::Hourly => measure_hourly(ctx, site, now).await,
    }
}

/// A site total, this week against last.
async fn measure_total(
    ctx: &JobCtx,
    site: &sites::Site,
    total: Total,
    now: DateTime<Utc>,
) -> Result<Measurement> {
    let (mut current, mut previous) = weeks(site, now);
    let (path, extra) = match total {
        Total::AgentFetches => {
            current.audience = Audience::Agent;
            previous.audience = Audience::Agent;
            ("/api/agents/overview", "")
        }
        Total::AnswerArrivals => {
            let group = FilterGroup::and(vec![FilterNode::Leaf(Filter::new(
                Dimension::parse("channel")?,
                FilterOp::Is,
                FilterValue::One("answer_engine".to_string()),
            ))]);
            current.filters = Some(group.clone());
            previous.filters = Some(group);
            (
                "/api/stats/overview",
                "&filters=%5B%7B%22dimension%22%3A%22channel%22%2C%22op%22%3A%22is%22%2C%22value%22%3A%22answer_engine%22%7D%5D",
            )
        }
        _ => ("/api/stats/overview", ""),
    };

    let ((now_view, before_view), queries) = exec::recording(async {
        (
            stats::overview(&ctx.ch, &current).await,
            stats::overview(&ctx.ch, &previous).await,
        )
    })
    .await;
    let (now_view, before_view) = (now_view?, before_view?);
    let pick = |o: &stats::Overview| -> f64 {
        match total {
            Total::Visitors => o.visitors as f64,
            Total::Pageviews | Total::AgentFetches => o.pageviews as f64,
            Total::BounceRate => o.bounce_rate.unwrap_or(0.0),
            Total::AnswerArrivals => o.sessions as f64,
        }
    };
    let (cur, base) = (pick(&now_view), pick(&before_view));
    // A bounce rate over a handful of sessions is noise however far it moves.
    let enough_sessions =
        total != Total::BounceRate || now_view.sessions.max(before_view.sessions) >= 100;
    let severity = judge_total(total, cur, base).filter(|_| enough_sessions);

    Ok(Measurement {
        current: cur,
        baseline: base,
        change_pct: change_pct(cur, base),
        severity,
        window: "7 days against the previous 7".to_string(),
        evidence: detector_trace(
            "measure_total",
            json!({ "metric": total.key() }),
            format!(
                "GET {path}?site={}&range=7d&compare=previous{extra}",
                site.site_id
            ),
            queries,
        ),
    })
}

/// The last full hour against the same hour of the four weeks before.
async fn measure_hourly(
    ctx: &JobCtx,
    site: &sites::Site,
    now: DateTime<Utc>,
) -> Result<Measurement> {
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let hour_end = now
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(now);
    let hour_start = hour_end - Duration::hours(1);
    let site_id = site.site_id_u16();

    let observed =
        metrics::pulse_in(&ctx.ch, site_id, &Pulse::Visitors, hour_start, hour_end).await? as f64;
    let baseline =
        metrics::weekly_baseline(&ctx.ch, site_id, &Pulse::Visitors, tz, hour_start, 4).await?;

    let unusual = baseline.is_usable()
        && observed.max(baseline.mean) >= 20.0
        && (observed > baseline.upper(3.0) || observed < baseline.lower(3.0));
    let severity = unusual.then(|| {
        if baseline.mean > 0.0
            && (observed / baseline.mean >= 3.0 || observed / baseline.mean <= 0.25)
        {
            Severity::Critical
        } else {
            Severity::Warning
        }
    });

    Ok(Measurement {
        current: observed,
        baseline: (baseline.mean * 10.0).round() / 10.0,
        change_pct: change_pct(observed, baseline.mean),
        severity,
        window: "last hour against the same hour of the previous 4 weeks".to_string(),
        evidence: detector_trace(
            "measure_hour",
            json!({
                "hour_start": hour_start,
                "weeks": 4,
                "mean": baseline.mean,
                "stddev": baseline.stddev,
                "samples": baseline.samples,
                "sigma": 3.0,
            }),
            format!(
                "GET /api/stats/timeseries?site={}&metric=visitors&granularity=hour&start={}&end={}",
                site.site_id,
                url_encode(&hour_start.to_rfc3339()),
                url_encode(&hour_end.to_rfc3339())
            ),
            Vec::new(),
        ),
    })
}

/// A trace for the detector's own reads, in the shape the analyst uses.
fn detector_trace(
    tool: &str,
    arguments: Json,
    request: String,
    queries: Vec<exec::RecordedQuery>,
) -> ToolTrace {
    ToolTrace {
        tool: tool.to_string(),
        arguments,
        ok: true,
        error: None,
        request,
        queries,
        rows: None,
        took_ms: 0,
    }
}

/// Percent-encode a query value.
fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// The row an investigation stores for a measurement.
fn measure_row<'a>(
    title: &'a str,
    m: &'a Measurement,
    findings: &'a Json,
    evidence: &'a Json,
) -> Measure<'a> {
    Measure {
        title,
        severity: m.severity.unwrap_or(Severity::Info).as_str(),
        direction: m.direction(),
        current: m.current,
        baseline: m.baseline,
        change_pct: m.change_pct,
        window_label: &m.window,
        findings,
        evidence,
    }
}

/// The statistical findings of a measurement.
fn findings_of(label: &str, m: &Measurement) -> Json {
    json!([{
        "label": label,
        "current": m.current,
        "baseline": m.baseline,
        "change_pct": m.change_pct,
    }])
}

/// Open an investigation, explain it when a model is configured, post it.
async fn open(
    ctx: &JobCtx,
    llm: Option<&Llm>,
    registry: &Registry,
    site: &sites::Site,
    probe: &Probe,
    m: &Measurement,
    now: DateTime<Utc>,
) -> Result<()> {
    let title = title_for(probe, m);
    let findings = findings_of(&format!("{} ({})", title, m.window), m);
    let evidence = json!([m.evidence]);
    let measure = measure_row(&title, m, &findings, &evidence);
    let (dimension, value) = match probe {
        Probe::Mover { dimension, value } => (Some(dimension.as_str()), Some(value.as_str())),
        _ => (None, None),
    };
    let status = if llm.is_some() {
        "failed"
    } else {
        "not_configured"
    };
    let Some(inv) = investigations::open(
        &ctx.pg,
        new_id(),
        site.id,
        &probe.key(),
        probe.metric(),
        dimension,
        value,
        &measure,
        status,
        now + Duration::hours(RECHECK),
    )
    .await?
    else {
        // Another sweep opened it a moment ago; it will be updated next time.
        return Ok(());
    };
    event(
        ctx,
        inv.id,
        "opened",
        &format!("Opened: {}", change_words(m)),
        json!({ "current": m.current, "baseline": m.baseline, "change_pct": m.change_pct }),
    )
    .await?;

    let mut inv = inv;
    if let Some(llm) = llm {
        inv = explain(ctx, llm, registry, site, &inv, m).await?;
    }

    let mut text = format!("{}: {}", inv.title, change_words(m));
    if let Some(explanation) = inv.explanation.as_deref() {
        text.push_str("\n\n");
        text.push_str(explanation);
    }
    post(ctx, site, &inv, &text).await;
    Ok(())
}

/// Ask the analyst for the likely causes, and store what it said.
async fn explain(
    ctx: &JobCtx,
    llm: &Llm,
    registry: &Registry,
    site: &sites::Site,
    inv: &Investigation,
    m: &Measurement,
) -> Result<Investigation> {
    let mut evidence: Vec<ToolTrace> =
        serde_json::from_value(inv.evidence.clone()).unwrap_or_default();

    if let Some(cfg) = ctx.cfg.llm.as_ref() {
        if let Err(e) = micaforge_ai::limit::take(&ctx.kv, cfg, site.org_id).await {
            let note = format!("No explanation: {e}");
            event(ctx, inv.id, "explained", &note, json!({ "ok": false })).await?;
            return investigations::set_explanation(&ctx.pg, inv.id, None, "failed", &inv.evidence)
                .await;
        }
    }

    let tool_ctx = ToolCtx {
        ch: ctx.ch.clone(),
        pg: ctx.pg.clone(),
        http: ctx.http.clone(),
        cfg: ctx.cfg.clone(),
        site: site.clone(),
        base: BaseQuery {
            range: Some("7d".to_string()),
            ..BaseQuery::default()
        },
    };
    let question = format!(
        "Investigate this change on {}: {}. It moved {} ({}). Use the tools to find the most \
         likely causes: which channels, sources, pages, countries or devices moved with it, \
         and whether an annotation explains it. Answer with the two or three most likely \
         causes, each with the numbers that support it, and say what you could not establish.",
        site.domain,
        inv.title,
        change_words(m),
        m.window
    );

    let outcome = micaforge_ai::ask(
        llm,
        registry,
        &tool_ctx,
        &[],
        &question,
        None,
        EXPLAIN_DEADLINE,
    )
    .await;
    match outcome {
        Ok(answer) => {
            evidence.extend(answer.traces);
            let stored = serde_json::to_value(&evidence)?;
            let updated = investigations::set_explanation(
                &ctx.pg,
                inv.id,
                Some(&answer.text),
                "ready",
                &stored,
            )
            .await?;
            event(
                ctx,
                inv.id,
                "explained",
                "The AI analyst explained the likely causes",
                json!({ "ok": true, "tools": evidence.len() }),
            )
            .await?;
            Ok(updated)
        }
        Err(failed) => {
            evidence.extend(failed.traces);
            let stored = serde_json::to_value(&evidence)?;
            let reason = if failed.error.is_public() {
                failed.error.to_string()
            } else {
                tracing::warn!(error = %failed.error, "investigation explanation failed");
                "the model did not answer".to_string()
            };
            event(
                ctx,
                inv.id,
                "explained",
                &format!("No explanation: {reason}"),
                json!({ "ok": false }),
            )
            .await?;
            investigations::set_explanation(&ctx.pg, inv.id, None, "failed", &stored).await
        }
    }
}

/// Measure a due investigation again and move it along.
async fn recheck(ctx: &JobCtx, inv: &Investigation, now: DateTime<Utc>) -> Result<()> {
    let site = sites::get(&ctx.pg, inv.site_id).await?;
    let Some(probe) = Probe::from_key(&inv.signal_key) else {
        // A key from a newer build: leave it alone rather than resolve it on a
        // reading this build cannot take.
        investigations::set_status(&ctx.pg, inv.id, &inv.status, inv.calm_checks, None).await?;
        return Ok(());
    };
    let m = measure(ctx, &site, &probe, now).await?;
    let next = next_state(&inv.status, inv.calm_checks, m.severity.is_some());

    let title = title_for(&probe, &m);
    let findings = findings_of(&format!("Re-check ({})", m.window), &m);
    let mut evidence: Vec<Json> = inv.evidence.as_array().cloned().unwrap_or_default();
    evidence.push(serde_json::to_value(&m.evidence)?);
    let evidence = Json::Array(evidence);
    // A calm re-check keeps the title of the change it is watching.
    let keep_title = if m.severity.is_some() {
        title.as_str()
    } else {
        inv.title.as_str()
    };
    let measure = Measure {
        severity: m.severity.map(|s| s.as_str()).unwrap_or(&inv.severity),
        direction: if m.severity.is_some() {
            m.direction()
        } else {
            &inv.direction
        },
        ..measure_row(keep_title, &m, &findings, &evidence)
    };
    investigations::record_measure(&ctx.pg, inv.id, &measure).await?;
    let updated = investigations::set_status(
        &ctx.pg,
        inv.id,
        next.status,
        next.calm_checks,
        next.recheck_in.map(|h| now + Duration::hours(h)),
    )
    .await?;

    let summary = match (inv.status.as_str(), next.status) {
        (from, to) if from == to => format!("Checked again, still {to}: {}", change_words(&m)),
        (_, "resolved") => format!("Resolved: back to normal, {}", change_words(&m)),
        (_, "monitoring") => format!("Monitoring: the change has eased, {}", change_words(&m)),
        (_, to) => format!("Now {to}: {}", change_words(&m)),
    };
    event(
        ctx,
        inv.id,
        if inv.status == next.status {
            "checked"
        } else {
            "status"
        },
        &summary,
        json!({ "status": next.status, "current": m.current, "baseline": m.baseline }),
    )
    .await?;
    if inv.status != next.status {
        post(ctx, &site, &updated, &summary).await;
    }
    Ok(())
}

/// Append a timeline entry.
async fn event(ctx: &JobCtx, id: Uuid, kind: &str, summary: &str, data: Json) -> Result<()> {
    investigations::add_event(&ctx.pg, new_id(), id, kind, summary, &data).await?;
    Ok(())
}

/// Post to every chat channel linked to the site, in the investigation's thread.
///
/// Best effort, like every notification: a channel that cannot be reached
/// costs that channel, and the investigation is stored whatever happens here.
pub async fn post(ctx: &JobCtx, site: &sites::Site, inv: &Investigation, text: &str) {
    let links = match chat::for_site(&ctx.pg, site.id).await {
        Ok(links) => links,
        Err(e) => {
            tracing::warn!(error = %e, "could not read chat links");
            return;
        }
    };
    if links.is_empty() {
        return;
    }
    let link_to =
        crate::notify::dashboard_link(ctx, site.site_id_u16(), &format!("insights/{}", inv.id));
    let body = format!("{} · {}\n{text}\n{link_to}", site.domain, inv.status);

    for link in links {
        let thread = match chat::post_ref(&ctx.pg, inv.id, link.id).await {
            Ok(thread) => thread,
            Err(e) => {
                tracing::warn!(error = %e, "could not read an investigation's thread");
                continue;
            }
        };
        match crate::chat::post_to_link(&ctx.http, &ctx.cfg.chat, &link, &body, thread.as_deref())
            .await
        {
            Ok(reference) => {
                if thread.is_none() && !reference.is_empty() {
                    if let Err(e) = chat::record_post(&ctx.pg, inv.id, link.id, &reference).await {
                        tracing::warn!(error = %e, "could not record an investigation's thread");
                    }
                    let _ = event(
                        ctx,
                        inv.id,
                        "posted",
                        &format!("Posted to {}", link.platform),
                        json!({ "platform": link.platform, "channel": link.label }),
                    )
                    .await;
                }
            }
            Err(e) => {
                tracing::warn!(platform = %link.platform, error = %e, "investigation post failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_through_the_probe() {
        for probe in [
            Probe::Total(Total::Visitors),
            Probe::Total(Total::AnswerArrivals),
            Probe::Mover {
                dimension: "pathname".into(),
                value: "/docs/a=b:c".into(),
            },
            Probe::Mover {
                dimension: "source".into(),
                value: String::new(),
            },
            Probe::Hourly,
        ] {
            assert_eq!(Probe::from_key(&probe.key()), Some(probe));
        }
        assert_eq!(Probe::from_key("wow:browser=Chrome"), None);
        assert_eq!(Probe::from_key("wow:revenue"), None);
        assert_eq!(Probe::from_key("something-else"), None);
    }

    #[test]
    fn an_investigation_needs_two_calm_checks_to_resolve() {
        // Still significant: open, whatever it was.
        assert_eq!(next_state("open", 0, true).status, "open");
        assert_eq!(
            next_state("monitoring", 1, true),
            Next {
                status: "open",
                calm_checks: 0,
                recheck_in: Some(RECHECK)
            }
        );

        // First calm check: monitoring, checked again tomorrow.
        assert_eq!(
            next_state("open", 0, false),
            Next {
                status: "monitoring",
                calm_checks: 1,
                recheck_in: Some(RECHECK)
            }
        );
        // Second calm check in a row: resolved, no more checks.
        assert_eq!(
            next_state("monitoring", 1, false),
            Next {
                status: "resolved",
                calm_checks: 2,
                recheck_in: None
            }
        );

        // A calm, then a relapse, then a calm: the count starts again.
        let relapse = next_state("monitoring", 1, true);
        assert_eq!(
            next_state(relapse.status, relapse.calm_checks, false).status,
            "monitoring"
        );

        // Resolved stays resolved; a recurrence is a new investigation.
        assert_eq!(next_state("resolved", 2, true).status, "resolved");
        assert_eq!(next_state("resolved", 2, true).recheck_in, None);
    }

    #[test]
    fn totals_need_volume_and_a_real_change() {
        // Thin traffic is never significant, whatever the percentage.
        assert_eq!(judge_total(Total::Visitors, 20.0, 5.0), None);
        // A third up is a warning; two thirds a critical.
        assert_eq!(
            judge_total(Total::Visitors, 400.0, 300.0),
            Some(Severity::Warning)
        );
        assert_eq!(
            judge_total(Total::Visitors, 100.0, 300.0),
            Some(Severity::Critical)
        );
        assert_eq!(judge_total(Total::Visitors, 310.0, 300.0), None);
        // From nothing to something is a warning with no percentage.
        assert_eq!(
            judge_total(Total::AnswerArrivals, 40.0, 0.0),
            Some(Severity::Warning)
        );
        assert_eq!(change_pct(40.0, 0.0), None);
        // Crawlers have a higher floor.
        assert_eq!(judge_total(Total::AgentFetches, 150.0, 50.0), None);
        // Bounce rate is judged in points.
        assert_eq!(
            judge_total(Total::BounceRate, 52.0, 40.0),
            Some(Severity::Warning)
        );
        assert_eq!(judge_total(Total::BounceRate, 45.0, 40.0), None);
        assert_eq!(
            judge_total(Total::BounceRate, 70.0, 40.0),
            Some(Severity::Critical)
        );
    }

    #[test]
    fn a_mover_must_carry_part_of_the_site() {
        // 40 → 100 on a site of 5,000: under a twentieth, not a mover.
        assert_eq!(judge_mover(100.0, 40.0, 5_000.0), None);
        // 200 → 600 on the same site: tripled and big enough.
        assert_eq!(judge_mover(600.0, 200.0, 5_000.0), Some(Severity::Warning));
        assert_eq!(judge_mover(360.0, 260.0, 5_000.0), None);
        assert_eq!(judge_mover(400.0, 260.0, 5_000.0), Some(Severity::Info));
    }

    #[test]
    fn titles_and_changes_read_as_sentences() {
        let m = Measurement {
            current: 120.0,
            baseline: 300.0,
            change_pct: change_pct(120.0, 300.0),
            severity: Some(Severity::Critical),
            window: "7 days against the previous 7".into(),
            evidence: detector_trace("t", json!({}), String::new(), Vec::new()),
        };
        assert_eq!(
            title_for(&Probe::Total(Total::Visitors), &m),
            "Visitors fell"
        );
        assert_eq!(
            title_for(
                &Probe::Mover {
                    dimension: "source".into(),
                    value: "chatgpt.com".into()
                },
                &m
            ),
            "Visitors from chatgpt.com fell"
        );
        assert_eq!(
            title_for(
                &Probe::Mover {
                    dimension: "pathname".into(),
                    value: "/pricing".into()
                },
                &m
            ),
            "Visitors to /pricing fell"
        );
        assert_eq!(
            change_words(&m),
            "-60% (120 against 300, 7 days against the previous 7)"
        );
    }
}
