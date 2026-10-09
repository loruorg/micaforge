//! `run_uptime_checks` — check every monitor that is due, once a minute.
//!
//! A monitor is a URL a site's operator asked this server to request on a
//! schedule. The sweep claims the monitors that are due, checks them, writes
//! one row per check to ClickHouse (`uptime_checks`), and moves each monitor
//! through a small state machine ([`step`]) that opens an incident after
//! enough failures in a row and closes it on the first success. Opening and
//! closing notify the monitor's channels through [`crate::notify`], and an
//! incident on a monitor shown on a published status page opens or resolves
//! an incident on that page too.
//!
//! # Three promises
//!
//! - **No monitor, no request.** The first thing the sweep asks Postgres is
//!   whether any enabled monitor exists, and with none it stops there. A
//!   self-hosted install that never adds a monitor never opens a socket for
//!   this feature.
//! - **One check of one monitor at a time.** Monitors are claimed with a lease
//!   in one statement ([`uptime::claim_due`]), so a slow check, a second sweep
//!   and a second replica can never overlap on the same monitor.
//! - **The worker's width is the check's width.** At most
//!   `MICAFORGE_WORKER_CONCURRENCY` checks run at once, and a monitor is
//!   claimed only when a slot is free for it, so a claimed check always starts
//!   at once and its lease never runs out while it waits.
//!
//! # A failed delivery is not a failed sweep
//!
//! The monitor's state and incident are written before anyone is told, so a
//! channel that is down costs that notification and nothing else: the sweep
//! does not fail, and the next minute's checks still run. Retrying the sweep
//! would not resend it anyway, because the state has already moved.

pub mod cert;
pub mod probe;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use micaforge_mail::AlertPayload;
use micaforge_store::ch::{insert_rows, UptimeCheckRow};
use micaforge_store::models::sites::{self, Site};
use micaforge_store::models::status_pages;
use micaforge_store::models::uptime::{self, CheckRecord, IncidentChange, Monitor};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::notify::{self, Channels};
use crate::JobCtx;

/// The sweep takes no arguments: every due monitor on the install is its work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// How long one sweep keeps claiming new checks.
///
/// Under the job's own timeout with room for the slowest check (thirty
/// seconds) to finish and be recorded. Monitors still due when it runs out are
/// the first claimed by the next sweep a minute later.
const SWEEP_BUDGET: Duration = Duration::from_secs(150);

/// A monitor's state, as the state machine reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Never checked, or not checked since it was created.
    Unknown,
    /// The last decisive check passed.
    Up,
    /// An incident is open.
    Down,
}

impl State {
    /// Read `uptime_monitors.state`. Anything unrecognised is `Unknown`.
    pub fn parse(raw: &str) -> State {
        match raw {
            "up" => State::Up,
            "down" => State::Down,
            _ => State::Unknown,
        }
    }

    /// The column value.
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Unknown => "unknown",
            State::Up => "up",
            State::Down => "down",
        }
    }
}

/// Where one check leaves a monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    /// The new state.
    pub state: State,
    /// Failed checks in a row, this one included.
    pub failures: i32,
    /// Open an incident now.
    pub open: bool,
    /// Close the open incident now.
    pub close: bool,
}

/// The incident state machine.
///
/// - A pass resets the failure count, makes the monitor `up`, and closes the
///   incident when it was `down`.
/// - A failure counts. The monitor keeps its state while the count is under
///   the threshold, so one dropped request never pages anybody, and turns
///   `down`, opening an incident, the moment the count reaches it.
/// - Further failures while `down` only count: the incident is already open.
///
/// A threshold below one is read as one; a monitor cannot be configured to
/// open an incident without a failure.
pub fn step(state: State, failures: i32, threshold: i32, ok: bool) -> Step {
    if ok {
        return Step {
            state: State::Up,
            failures: 0,
            open: false,
            close: state == State::Down,
        };
    }
    let failures = failures.max(0).saturating_add(1);
    if state == State::Down {
        return Step {
            state,
            failures,
            open: false,
            close: false,
        };
    }
    let reached = failures >= threshold.max(1);
    Step {
        state: if reached { State::Down } else { state },
        failures,
        open: reached,
        close: false,
    }
}

/// The check a monitor asks for.
pub fn spec_of(monitor: &Monitor) -> probe::Spec {
    probe::Spec {
        url: monitor.url.clone(),
        method: monitor.method.clone(),
        timeout: Duration::from_millis(u64::try_from(monitor.timeout_ms).unwrap_or(10_000)),
        status_min: u16::try_from(monitor.status_min).unwrap_or(200),
        status_max: u16::try_from(monitor.status_max).unwrap_or(299),
        keyword: monitor.keyword.clone(),
        json_path: monitor.json_path.clone(),
        json_expected: monitor.json_expected.clone(),
        ssl_check: monitor.ssl_check,
        ssl_expiry_days: monitor.ssl_expiry_days,
    }
}

/// Run one sweep.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    if !uptime::any_enabled(&ctx.pg).await? {
        return Ok(());
    }

    let width = ctx.cfg.worker_concurrency.max(1);
    let started = Instant::now();
    let region = ctx.cfg.region.clone();
    let mut running: JoinSet<(Monitor, probe::Outcome)> = JoinSet::new();
    let mut exhausted = false;
    let mut sites: HashMap<uuid::Uuid, Site> = HashMap::new();
    let mut rows: Vec<UptimeCheckRow> = Vec::new();
    let mut checked = 0usize;

    loop {
        if !exhausted && started.elapsed() < SWEEP_BUDGET && running.len() < width {
            let room = i64::try_from(width - running.len()).unwrap_or(1);
            let claimed = uptime::claim_due(&ctx.pg, room).await?;
            if claimed.is_empty() {
                exhausted = true;
            }
            for monitor in claimed {
                let spec = spec_of(&monitor);
                let region = region.clone();
                running.spawn(async move {
                    let outcome = probe::check(&spec, &region).await;
                    (monitor, outcome)
                });
            }
        }

        let Some(joined) = running.join_next().await else {
            break;
        };
        let (monitor, outcome) = match joined {
            Ok(done) => done,
            Err(e) => {
                // A panicking check loses its result; its lease expires and
                // the monitor is checked again on a later sweep.
                tracing::error!(error = %e, "an uptime check task failed");
                continue;
            }
        };
        checked += 1;

        let site = match site_of(ctx, &mut sites, monitor.site_id).await {
            Ok(site) => site,
            Err(e) => {
                tracing::warn!(monitor = %monitor.id, error = %e, "uptime check without a site");
                uptime::release(&ctx.pg, monitor.id).await?;
                continue;
            }
        };
        rows.push(check_row(&site, &monitor, &outcome));
        record(ctx, &site, &monitor, &outcome).await?;
    }

    if let Err(e) = insert_rows(&ctx.ch, "uptime_checks", &rows).await {
        // The states and incidents are already right; only the history of
        // this minute is missing. Failing the sweep would not bring it back.
        tracing::error!(rows = rows.len(), error = %e, "could not store uptime checks");
    }
    tracing::debug!(checked, "uptime sweep finished");
    Ok(())
}

/// The site a monitor belongs to, read once per sweep.
async fn site_of(
    ctx: &JobCtx,
    cache: &mut HashMap<uuid::Uuid, Site>,
    id: uuid::Uuid,
) -> Result<Site> {
    if let Some(site) = cache.get(&id) {
        return Ok(site.clone());
    }
    let site = sites::get(&ctx.pg, id).await?;
    cache.insert(id, site.clone());
    Ok(site)
}

/// The ClickHouse row for one check.
fn check_row(site: &Site, monitor: &Monitor, outcome: &probe::Outcome) -> UptimeCheckRow {
    UptimeCheckRow {
        site_id: site.site_id_u16(),
        timestamp: outcome.checked_at,
        monitor_id: monitor.id,
        region: outcome.region.clone(),
        ok: u8::from(outcome.ok),
        status_code: outcome.status_code.unwrap_or(0),
        latency_ms: outcome.latency_ms,
        error: outcome.error.clone().unwrap_or_default(),
        ssl_days_left: outcome.ssl_days_left,
    }
}

/// Move the monitor through the state machine, store it, and tell whoever
/// needs telling.
async fn record(
    ctx: &JobCtx,
    site: &Site,
    monitor: &Monitor,
    outcome: &probe::Outcome,
) -> Result<()> {
    let next = step(
        State::parse(&monitor.state),
        monitor.consecutive_failures,
        monitor.failure_threshold,
        outcome.ok,
    );
    let cause = outcome.error.clone().unwrap_or_default();
    let change = uptime::record_check(
        &ctx.pg,
        monitor,
        &CheckRecord {
            state: next.state.as_str(),
            consecutive_failures: next.failures,
            checked_at: Utc::now(),
            open: next.open.then_some(cause.as_str()),
            close: next.close,
        },
    )
    .await?;

    match change {
        IncidentChange::None => {}
        IncidentChange::Opened(incident) => {
            tracing::warn!(monitor = %monitor.id, %incident, cause = %cause, "monitor is down");
            // No time in the text: the update carries its own timestamp, which
            // the page shows in each reader's zone.
            if let Err(e) = status_pages::auto_open(&ctx.pg, monitor.id, incident, |name| {
                (
                    format!("{name} is down"),
                    format!("Automatic checks found {name} not responding as expected."),
                )
            })
            .await
            {
                tracing::warn!(monitor = %monitor.id, error = %e, "could not open status page incidents");
            }
            notify_change(
                ctx,
                site,
                monitor,
                down_payload(ctx, site, monitor, next.failures, &cause),
            )
            .await;
        }
        IncidentChange::Closed(incident) => {
            tracing::info!(monitor = %monitor.id, %incident, "monitor recovered");
            if let Err(e) = status_pages::auto_resolve(
                &ctx.pg,
                incident,
                "Automatic checks show this service responding as expected again.",
            )
            .await
            {
                tracing::warn!(monitor = %monitor.id, error = %e, "could not resolve status page incidents");
            }
            let since = uptime::get_incident(&ctx.pg, incident)
                .await
                .map(|i| i.started_at)
                .ok();
            notify_change(ctx, site, monitor, up_payload(ctx, site, monitor, since)).await;
        }
    }
    Ok(())
}

/// Send one notification to the monitor's channels, best effort.
async fn notify_change(ctx: &JobCtx, site: &Site, monitor: &Monitor, payload: AlertPayload) {
    let channels = Channels::from_json(&monitor.channels);
    if channels.is_empty() {
        return;
    }
    let delivered = notify::deliver_alert(ctx, &site.name, &channels, &payload).await;
    if delivered == 0 {
        tracing::warn!(monitor = %monitor.id, kind = %payload.kind, "an uptime notification reached no channel");
    }
}

/// The notification for a monitor that just went down.
fn down_payload(
    ctx: &JobCtx,
    site: &Site,
    monitor: &Monitor,
    failures: i32,
    cause: &str,
) -> AlertPayload {
    AlertPayload {
        kind: "uptime_down".to_string(),
        headline: format!("{} is down", monitor.name),
        detail: format!(
            "{} failed {} {} in a row. The last check said: {}. Checked from {}.",
            monitor.url,
            failures,
            if failures == 1 { "check" } else { "checks" },
            if cause.is_empty() {
                "no reason given"
            } else {
                cause
            },
            ctx.cfg.region
        ),
        metric: "failed checks in a row".to_string(),
        observed: f64::from(failures),
        expected: 0.0,
        link: notify::dashboard_link(ctx, site.site_id_u16(), "uptime"),
    }
}

/// The notification for a monitor that just came back.
fn up_payload(
    ctx: &JobCtx,
    site: &Site,
    monitor: &Monitor,
    since: Option<DateTime<Utc>>,
) -> AlertPayload {
    let minutes = since
        .map(|at| (Utc::now() - at).num_minutes().max(0))
        .unwrap_or(0);
    AlertPayload {
        kind: "uptime_recovered".to_string(),
        headline: format!("{} is up again", monitor.name),
        detail: format!(
            "{} answers as expected again after about {} {} down. Checked from {}.",
            monitor.url,
            minutes,
            if minutes == 1 { "minute" } else { "minutes" },
            ctx.cfg.region
        ),
        metric: "minutes down".to_string(),
        observed: minutes as f64,
        expected: 0.0,
        link: notify::dashboard_link(ctx, site.site_id_u16(), "uptime"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_under_the_threshold_change_nothing_but_the_count() {
        let s = step(State::Up, 0, 3, false);
        assert_eq!(
            s,
            Step {
                state: State::Up,
                failures: 1,
                open: false,
                close: false
            }
        );
        let s = step(State::Up, 1, 3, false);
        assert_eq!((s.state, s.failures, s.open), (State::Up, 2, false));
    }

    #[test]
    fn reaching_the_threshold_opens_exactly_one_incident() {
        let s = step(State::Up, 2, 3, false);
        assert_eq!(
            (s.state, s.failures, s.open, s.close),
            (State::Down, 3, true, false)
        );
        // Further failures count and do not open a second one.
        let s = step(State::Down, 3, 3, false);
        assert_eq!(
            (s.state, s.failures, s.open, s.close),
            (State::Down, 4, false, false)
        );
    }

    #[test]
    fn a_new_monitor_goes_down_from_unknown_too() {
        let s = step(State::Unknown, 0, 1, false);
        assert_eq!((s.state, s.open), (State::Down, true));
        // And stays unknown, not up, while it has failed but not enough.
        let s = step(State::Unknown, 0, 2, false);
        assert_eq!((s.state, s.open), (State::Unknown, false));
    }

    #[test]
    fn the_first_pass_closes_the_incident_and_resets_the_count() {
        let s = step(State::Down, 7, 3, true);
        assert_eq!(
            s,
            Step {
                state: State::Up,
                failures: 0,
                open: false,
                close: true
            }
        );
        // A pass while up closes nothing.
        let s = step(State::Up, 2, 3, true);
        assert_eq!((s.state, s.failures, s.close), (State::Up, 0, false));
    }

    #[test]
    fn a_full_outage_walks_up_down_up() {
        let mut state = State::Unknown;
        let mut failures = 0;
        let mut opened = 0;
        let mut closed = 0;
        for ok in [true, false, true, false, false, false, false, true, true] {
            let s = step(state, failures, 2, ok);
            state = s.state;
            failures = s.failures;
            opened += usize::from(s.open);
            closed += usize::from(s.close);
        }
        assert_eq!((opened, closed), (1, 1), "one blip, one outage");
        assert_eq!(state, State::Up);
    }

    #[test]
    fn a_threshold_below_one_still_needs_a_failure() {
        assert!(!step(State::Up, 0, 0, true).open);
        assert!(step(State::Up, 0, 0, false).open);
        // A corrupt negative count still counts up from zero.
        assert_eq!(step(State::Up, -5, 3, false).failures, 1);
    }

    #[test]
    fn states_round_trip_and_junk_is_unknown() {
        for s in [State::Unknown, State::Up, State::Down] {
            assert_eq!(State::parse(s.as_str()), s);
        }
        assert_eq!(State::parse("degraded"), State::Unknown);
    }
}
