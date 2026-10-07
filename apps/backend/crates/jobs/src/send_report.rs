//! `send_report` — the scheduled digest.
//!
//! Daily, weekly or monthly, per `reports.cadence`. The sweep runs every fifteen
//! minutes and sends whatever has come due, which is what lets a report leave at
//! 08:00 in the site's own timezone rather than at 08:00 in the server's.
//!
//! # The window is a calendar window, in the site's zone
//!
//! "Last week" means Monday to Sunday where the operator lives, not the last 168
//! hours. A weekly report that covers Wednesday to Wednesday is arithmetically
//! defensible and useless: nobody plans in rolling weeks, and the comparison with
//! "the week before" only means anything if both are the same seven days.
//!
//! # The schedule moves only when the send worked
//!
//! `next_run_at` is advanced after a successful delivery, so an SMTP relay that
//! was down at 08:00 costs a retry rather than a missed week. The one exception
//! is a *permanent* failure — an address that is not an address — where the
//! schedule is advanced anyway, because retrying it eight times and then again
//! every fifteen minutes forever would turn one typo into a permanent queue
//! failure.
//!
//! # A report is claimed before it is built
//!
//! Two sweeps can read the same due report: two replicas, or a retry that
//! overlaps an operator's "send now". Each report is claimed first with a
//! conditional update that moves `next_run_at` to a short lease
//! ([`alerts::claim_report`]); only the worker whose update matched sends it.
//! The claim ends with the next scheduled run after a delivery, or with the old
//! value put back after a failure worth retrying. A worker that dies mid-send
//! leaves the lease, and the report comes due again when it expires. An
//! operator's "send now" claims whatever the row holds at that moment, lease
//! included, because it is an explicit request to send and is never refused.
//!
//! # The numbers include the wedge
//!
//! Agent fetches, answer-engine arrivals and the gap ratio sit in the digest next
//! to visitors and pageviews. That is the point of the product being one product:
//! the operator does not have to go and look for the part nobody else measures.

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use micaforge_core::{Compare, Dimension, Error, QueryEnvelope, Result, TimeRange};
use micaforge_mail::ReportPayload;
use micaforge_query::{agents_q, gap, stats};
use micaforge_store::models::{alerts, sites};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::notify::{self, Channels};
use crate::schedule::next_report_run;
use crate::JobCtx;

/// How many rows each ranked list in the digest carries.
///
/// Five. A digest is read on a phone at breakfast, and a ten-row table is a
/// table nobody reads. The dashboard has the rest.
const LIST_ROWS: u32 = 5;

/// How long a claimed report is held before another sweep may take it.
///
/// Three times the job's own ten-minute timeout, so a lease never expires
/// under a send that is still running.
const REPORT_LEASE_SECS: i64 = 30 * 60;

/// Which reports to send.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One report, by `reports.id` — an operator asking for it now. Absent means
    /// "everything that has come due", which is what the sweep queues.
    #[serde(default)]
    pub report: Option<Uuid>,
}

/// Send the reports that are due, or one named report.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let now = Utc::now();
    let due = match payload.report {
        Some(id) => vec![alerts::get_report(&ctx.pg, id).await?],
        None => alerts::due_reports(&ctx.pg, now).await?,
    };

    let mut sent = 0usize;
    let mut failed = 0usize;

    for report in &due {
        let Some(lease) =
            alerts::claim_report(&ctx.pg, report.id, report.next_run_at, REPORT_LEASE_SECS).await?
        else {
            tracing::debug!(report = %report.id, "report is being sent by another worker");
            continue;
        };

        match send_one(ctx, report, now, lease).await {
            Ok(()) => sent += 1,
            Err(e) => {
                failed += 1;
                tracing::error!(report = %report.id, error = %e, "report not sent");

                // A permanent failure still advances the schedule. Leaving it in
                // the past would make every sweep from now until somebody fixes
                // the configuration fail on the same row. Anything else puts the
                // schedule back, so the retry finds the report due again.
                let next = if is_permanent(&e) {
                    let next = next_run_for(ctx, report, now)
                        .await
                        .unwrap_or_else(|| now + chrono::Duration::hours(24));
                    tracing::warn!(
                        report = %report.id,
                        next_run = %next,
                        "report configuration is broken; schedule advanced anyway"
                    );
                    Some(next)
                } else {
                    report.next_run_at
                };
                if let Err(e) = alerts::finish_report_claim(&ctx.pg, report.id, lease, next).await {
                    // The lease expires on its own; the report is late, not lost.
                    tracing::warn!(report = %report.id, error = %e, "could not release the report claim");
                }
            }
        }
    }

    if failed > 0 {
        // Loud, and a real failure: a digest nobody received is the whole job not
        // done. The retry is safe because a report that *did* send has already
        // moved its own schedule and will not be picked up again.
        return Err(Error::Upstream(format!(
            "{failed} of {} reports could not be sent",
            due.len()
        )));
    }

    tracing::info!(reports = due.len(), sent, "report sweep finished");
    Ok(())
}

/// Build and send one report claimed under `lease`.
async fn send_one(
    ctx: &JobCtx,
    report: &alerts::Report,
    now: DateTime<Utc>,
    lease: DateTime<Utc>,
) -> Result<()> {
    let site = sites::get(&ctx.pg, report.site_id).await?;
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let period = period_for(&report.cadence, now, tz);

    let payload = build(ctx, site.site_id_u16(), tz, &period).await?;

    let channels =
        Channels::from_json(&report.recipients).merge(Channels::from_json(&report.channels));
    if channels.is_empty() {
        return Err(Error::validation(
            "recipients",
            "this report has no recipient and no channel",
        ));
    }

    let mut delivered = 0usize;
    let mut last_error: Option<Error> = None;

    for address in &channels.emails {
        match ctx
            .mailer
            .send_report(address, &site.name, &period.label, &payload)
            .await
        {
            Ok(()) => delivered += 1,
            Err(e) => {
                tracing::warn!(recipient = %address, error = %e, "report email failed");
                last_error = Some(e);
            }
        }
    }

    if let Some(url) = &channels.webhook {
        let body = json!({
            "site": site.name,
            "period": period.label,
            "cadence": report.cadence,
            "report": payload,
        });
        match notify::post_json(ctx, url, &body).await {
            Ok(()) => delivered += 1,
            Err(e) => {
                tracing::warn!(error = %e, "report webhook failed");
                last_error = Some(e);
            }
        }
    }

    if let Some(url) = &channels.slack {
        match notify::post_slack(
            ctx,
            url,
            &slack_summary(&site.name, &period.label, &payload),
        )
        .await
        {
            Ok(()) => delivered += 1,
            Err(e) => {
                tracing::warn!(error = %e, "report slack message failed");
                last_error = Some(e);
            }
        }
    }

    if delivered == 0 {
        return Err(last_error
            .unwrap_or_else(|| Error::Upstream("the report reached no destination".to_string())));
    }

    // `None` only on a daylight-saving gap at 08:00; a day later is the
    // documented fallback, and anything is better than leaving the lease to
    // expire into a second send.
    let next = next_report_run(now, &report.cadence, tz)
        .unwrap_or_else(|| now + chrono::Duration::hours(24));
    alerts::finish_report_claim(&ctx.pg, report.id, lease, Some(next)).await?;

    tracing::info!(
        report = %report.id,
        site = %site.domain,
        period = %period.label,
        delivered,
        visitors = payload.visitors,
        agent_fetches = payload.agent_fetches,
        "report sent"
    );
    Ok(())
}

/// The next scheduled time for a report, resolved in its site's zone.
async fn next_run_for(
    ctx: &JobCtx,
    report: &alerts::Report,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let site = sites::get(&ctx.pg, report.site_id).await.ok()?;
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    next_report_run(now, &report.cadence, tz)
}

/// Is this failure one that retrying will never fix?
///
/// A bad address, a deleted site or a malformed configuration will fail
/// identically on every attempt. A refused SMTP connection or a ClickHouse
/// timeout will not.
fn is_permanent(error: &Error) -> bool {
    matches!(
        error,
        Error::Validation { .. } | Error::NotFound(_) | Error::BadRequest(_)
    )
}

/// The window a report covers, and how to name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Period {
    /// Start of the window, inclusive.
    pub start: DateTime<Utc>,
    /// End of the window, exclusive.
    pub end: DateTime<Utc>,
    /// How a person would say it: "20 August 2026", "12 to 18 August 2026".
    pub label: String,
}

/// Resolve the calendar window for a cadence, in the site's zone.
///
/// Every boundary is a local midnight, converted to UTC afterwards. A window
/// computed in UTC and then labelled with a local date is the classic way to
/// produce a "yesterday" report that is missing its first two hours.
pub fn period_for(cadence: &str, now: DateTime<Utc>, tz: Tz) -> Period {
    let today = now.with_timezone(&tz).date_naive();

    let (from, to, label) = match cadence {
        "weekly" => {
            let this_monday = today
                .checked_sub_days(Days::new(u64::from(today.weekday().num_days_from_monday())))
                .unwrap_or(today);
            let last_monday = this_monday
                .checked_sub_days(Days::new(7))
                .unwrap_or(this_monday);
            let last_sunday = this_monday.checked_sub_days(Days::new(1)).unwrap_or(today);
            // "10 to 16 August 2026" when the week sits inside one month, and
            // "27 July to 2 August 2026" when it straddles two. Naming the month
            // once is how a person writes a date range; naming it twice when it
            // has not changed reads as a machine filling a template.
            let label = if last_monday.month() == last_sunday.month()
                && last_monday.year() == last_sunday.year()
            {
                format!(
                    "{} to {}",
                    last_monday.format("%-d"),
                    last_sunday.format("%-d %B %Y")
                )
            } else {
                format!(
                    "{} to {}",
                    last_monday.format("%-d %B"),
                    last_sunday.format("%-d %B %Y")
                )
            };
            (last_monday, this_monday, label)
        }
        "monthly" => {
            let first_this = today.with_day(1).unwrap_or(today);
            let first_prev = first_this
                .checked_sub_months(Months::new(1))
                .unwrap_or(first_this);
            let label = first_prev.format("%B %Y").to_string();
            (first_prev, first_this, label)
        }
        // Daily, and any cadence this build does not know. Sending yesterday's
        // numbers is the least surprising answer to an unrecognised cadence.
        _ => {
            let yesterday = today.checked_sub_days(Days::new(1)).unwrap_or(today);
            (yesterday, today, yesterday.format("%-d %B %Y").to_string())
        }
    };

    Period {
        start: local_midnight(tz, from),
        end: local_midnight(tz, to),
        label,
    }
}

/// Local midnight on a date, as an instant.
///
/// Midnight does not exist on every date in every zone: a spring-forward that
/// jumps 00:00 to 01:00 leaves the day with no midnight at all, which is why
/// several real zones need this and why `.unwrap()` on a local datetime is a bug
/// waiting for a March morning. The first hour that does exist is used instead.
fn local_midnight(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    for hour in 0..4 {
        if let Some(naive) = date.and_hms_opt(hour, 0, 0) {
            // `earliest` rather than `single`: a fall-back makes 00:30 happen
            // twice, and the first of the two is the start of the day.
            if let Some(local) = tz.from_local_datetime(&naive).earliest() {
                return local.with_timezone(&Utc);
            }
        }
    }
    // Unreachable for any real zone: no timezone on earth skips four consecutive
    // hours. Written as UTC midnight rather than as a panic all the same.
    DateTime::<Utc>::from_naive_utc_and_offset(
        date.and_hms_opt(0, 0, 0)
            .expect("00:00:00 is a valid time on every date"),
        Utc,
    )
}

/// Measure everything the digest shows.
///
/// Every number here comes from `micaforge-query`, the same code behind the
/// endpoints. That is the point: an emailed report whose visitor count disagreed
/// with the dashboard's would destroy trust in both, and the only way to
/// guarantee it cannot is for there to be one implementation.
///
/// # Units
///
/// [`micaforge_mail::ReportPayload`] takes `bounce_rate` and `visitors_delta` as
/// *fractions*; the query engine answers percentages, because that is what
/// [`micaforge_core::MetricUnit::Percent`] means on the wire. The division by a
/// hundred is the conversion between two documented conventions, not a fudge, and
/// it is done here because this is the boundary between them.
async fn build(ctx: &JobCtx, site: u16, tz: Tz, period: &Period) -> Result<ReportPayload> {
    let mut envelope = QueryEnvelope::new(site, TimeRange::new(period.start, period.end, tz));
    // The comparison the digest prints beside the visitor count. The engine
    // derives the earlier window from this one, so the two are the same length by
    // construction.
    envelope.compare = Compare::Previous;
    envelope.limit = LIST_ROWS;

    let overview = stats::overview(&ctx.ch, &envelope).await?;
    let citation_gap = gap::summary(&ctx.ch, &envelope).await?;
    let pages = stats::pages(&ctx.ch, &envelope).await?;
    let sources = stats::sources(&ctx.ch, &envelope).await?;
    let agents = agents_q::breakdown(&ctx.ch, &envelope, &Dimension::AgentId).await?;

    Ok(ReportPayload {
        visitors: overview.visitors,
        // Absent rather than zero when the earlier window had no visitors: the
        // engine refuses to call growth from nothing a percentage, and a report
        // saying "+0.0%" beside a site's first week would be a lie.
        visitors_delta: overview
            .delta
            .as_ref()
            .and_then(|d| d.visitors)
            .unwrap_or(0.0)
            / 100.0,
        pageviews: overview.pageviews,
        sessions: overview.sessions,
        bounce_rate: overview.bounce_rate.unwrap_or(0.0) / 100.0,
        agent_fetches: citation_gap.agent_fetches,
        answer_arrivals: citation_gap.answer_arrivals,
        gap_ratio: citation_gap.ratio,
        top_pages: pages
            .into_iter()
            .take(LIST_ROWS as usize)
            .map(|row| (row.path, row.visitors))
            .collect(),
        top_sources: sources
            .sources
            .into_iter()
            .take(LIST_ROWS as usize)
            .map(|row| (source_label(&row), row.visitors))
            .collect(),
        top_agents: agents
            .into_iter()
            .take(LIST_ROWS as usize)
            .map(|row| (row.label, row.fetches))
            .collect(),
    })
}

/// How a source is named in a five-row list.
///
/// The resolved name first, then the referrer host, then `direct`. A row labelled
/// with an empty string is a real bucket with an unreadable name, which in a
/// digest is worse than no row at all.
fn source_label(row: &stats::SourceRow) -> String {
    for candidate in [row.source.as_str(), row.referrer_host.as_str()] {
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }
    "direct".to_string()
}

/// The digest as a few lines of Slack text.
///
/// The same numbers as the email, in the order the email puts them, so an
/// operator reading one and then the other is not re-learning a layout.
fn slack_summary(site: &str, period: &str, report: &ReportPayload) -> String {
    format!(
        "*{site}* - {period}\n\
         {} visitors ({}), {} pageviews, {} sessions, {:.0}% bounce\n\
         {} agent fetches, {} answer-engine arrivals, gap {:.0} : 1",
        report.visitors,
        signed_percent(report.visitors_delta),
        report.pageviews,
        report.sessions,
        report.bounce_rate * 100.0,
        report.agent_fetches,
        report.answer_arrivals,
        report.gap_ratio,
    )
}

/// A signed fraction as a percentage, the way a report reads it.
fn signed_percent(fraction: f64) -> String {
    let pct = fraction * 100.0;
    if pct >= 0.0 {
        format!("+{pct:.1}%")
    } else {
        format!("{pct:.1}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lisbon() -> Tz {
        "Europe/Lisbon".parse().unwrap()
    }

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_lease_outlives_the_job_that_holds_it() {
        // An expired lease under a send still in progress is a second send.
        assert!(
            std::time::Duration::from_secs(REPORT_LEASE_SECS as u64)
                > crate::Kind::SendReport.timeout()
        );
    }

    #[test]
    fn a_daily_report_covers_yesterday_in_the_sites_own_zone() {
        // 08:00 UTC on Friday 21 August 2026, which is 09:00 in Lisbon.
        let period = period_for("daily", at("2026-08-21T08:00:00Z"), lisbon());
        assert_eq!(period.label, "20 August 2026");
        assert_eq!(period.start.to_rfc3339(), "2026-08-19T23:00:00+00:00");
        assert_eq!(period.end.to_rfc3339(), "2026-08-20T23:00:00+00:00");
        assert_eq!(period.end - period.start, chrono::Duration::days(1));
    }

    #[test]
    fn a_weekly_report_covers_monday_to_sunday() {
        // Friday.
        let period = period_for("weekly", at("2026-08-21T08:00:00Z"), lisbon());
        assert_eq!(period.label, "10 to 16 August 2026");
        assert_eq!(period.end - period.start, chrono::Duration::days(7));
    }

    #[test]
    fn a_weekly_report_run_on_a_monday_covers_the_week_just_finished() {
        // Monday 24 August. The week that just ended is 17 to 23.
        let period = period_for("weekly", at("2026-08-24T08:00:00Z"), lisbon());
        assert_eq!(period.label, "17 to 23 August 2026");
    }

    #[test]
    fn a_monthly_report_covers_the_month_just_finished() {
        let period = period_for("monthly", at("2026-08-21T08:00:00Z"), lisbon());
        assert_eq!(period.label, "July 2026");
        assert_eq!(period.end - period.start, chrono::Duration::days(31));
    }

    #[test]
    fn an_unknown_cadence_still_produces_a_window() {
        let period = period_for("fortnightly", at("2026-08-21T08:00:00Z"), lisbon());
        assert_eq!(period.label, "20 August 2026");
    }

    #[test]
    fn a_zone_with_no_midnight_still_has_a_first_hour() {
        // Santiago's spring-forward skips midnight itself on this date, which is
        // exactly the case that makes a naive `.unwrap()` a bug once a year.
        let santiago: Tz = "America/Santiago".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 9, 6).unwrap();
        let start = local_midnight(santiago, date);
        assert_eq!(start.with_timezone(&santiago).date_naive(), date);
    }

    #[test]
    fn deltas_are_written_with_their_sign() {
        assert_eq!(signed_percent(0.031), "+3.1%");
        assert_eq!(signed_percent(-0.031), "-3.1%");
        assert_eq!(signed_percent(0.0), "+0.0%");
    }

    #[test]
    fn a_transient_failure_is_retried_and_a_broken_address_is_not() {
        assert!(!is_permanent(&Error::Upstream("smtp refused".into())));
        assert!(!is_permanent(&Error::Database("clickhouse down".into())));
        assert!(is_permanent(&Error::validation(
            "recipients",
            "not an address"
        )));
        assert!(is_permanent(&Error::not_found("site")));
    }
}
