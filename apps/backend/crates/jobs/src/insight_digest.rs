//! `insight_digest` — Monday morning's summary of what changed on a site.
//!
//! Runs every hour and sends nothing most of the time. For each site with at
//! least one subscriber it asks one question: is it Monday, eight o'clock or
//! later, *in the site's own timezone*, and has this week's digest not gone
//! yet? Hourly rather than weekly because "Monday morning" is a different
//! instant in Lisbon and in Auckland, and a single weekly run would deliver
//! one of them in the middle of the night.
//!
//! "Not gone yet" is a Valkey marker per site and ISO week, claimed before the
//! mail is sent. A worker that crashes after the claim loses that week's
//! digest rather than sending it twice; for a summary that will be followed by
//! another next week, a missed one is the cheaper failure.
//!
//! A subscription outlives a membership, so each subscriber is checked for a
//! role on the site at send time, and somebody who has lost access receives
//! nothing about it.

use chrono::{DateTime, Datelike, Duration, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use micaforge_core::Result;
use micaforge_mail::{DigestItem, DigestPayload};
use micaforge_store::models::investigations::{self, Investigation};
use micaforge_store::models::{orgs, sites};
use serde::{Deserialize, Serialize};

use crate::JobCtx;

/// Nothing to configure: the sweep reads the subscriptions itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// The local hour from which Monday's digest may go.
const SEND_FROM_HOUR: u32 = 8;

/// How long a sent marker lives: past the end of its week, so it can never be
/// mistaken for next week's.
const MARKER_TTL: u32 = 8 * 24 * 60 * 60;

/// True when `local` is Monday at or after the sending hour.
pub fn is_due(local_weekday: Weekday, local_hour: u32) -> bool {
    local_weekday == Weekday::Mon && local_hour >= SEND_FROM_HOUR
}

/// The marker for a site's digest in one ISO week.
pub fn marker_key(site: uuid::Uuid, iso_year: i32, iso_week: u32) -> String {
    format!("mf:insights:digest:{site}:{iso_year}-W{iso_week:02}")
}

/// Send the digests that are due.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    if !ctx.mailer.is_sending() {
        // No transport: rendering forty digests to log one line each would be
        // noise. The settings screen already says mail is not configured.
        return Ok(());
    }
    let now = Utc::now();
    for site_id in investigations::sites_with_subscribers(&ctx.pg).await? {
        if let Err(e) = digest_site(ctx, site_id, now).await {
            tracing::warn!(site = %site_id, error = %e, "insight digest failed");
        }
    }
    Ok(())
}

async fn digest_site(ctx: &JobCtx, site_id: uuid::Uuid, now: DateTime<Utc>) -> Result<()> {
    let site = sites::get(&ctx.pg, site_id).await?;
    let tz: Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let local = now.with_timezone(&tz);
    if !is_due(local.weekday(), local.hour()) {
        return Ok(());
    }
    let week = local.iso_week();
    let marker = marker_key(site.id, week.year(), week.week());
    if ctx.kv.get_json::<bool>(&marker).await?.is_some() {
        return Ok(());
    }
    ctx.kv.set_json(&marker, &true, Some(MARKER_TTL)).await?;

    let since = now - Duration::days(7);
    let changed = investigations::changed_since(&ctx.pg, site.id, since).await?;
    let digest = payload(ctx, &site, &changed, since, now, tz);

    for subscriber in investigations::subscribers(&ctx.pg, site.id).await? {
        if !can_read(ctx, &site, subscriber.user_id).await? {
            continue;
        }
        if let Err(e) = ctx
            .mailer
            .send_insight_digest(&subscriber.email, &site.name, &digest)
            .await
        {
            tracing::warn!(site = site.site_id, error = %e, "a digest could not be sent");
        }
    }
    Ok(())
}

/// Whether `user_id` holds any role on the site today: a per-site override,
/// or a membership of its organisation.
async fn can_read(ctx: &JobCtx, site: &sites::Site, user_id: uuid::Uuid) -> Result<bool> {
    if sites::get_access(&ctx.pg, site.id, user_id)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(orgs::get_member(&ctx.pg, site.org_id, user_id)
        .await?
        .is_some())
}

/// Build the digest from the week's investigations.
fn payload(
    ctx: &JobCtx,
    site: &sites::Site,
    changed: &[Investigation],
    since: DateTime<Utc>,
    now: DateTime<Utc>,
    tz: Tz,
) -> DigestPayload {
    let opened = changed.iter().filter(|i| i.created_at >= since).count() as u64;
    let resolved = changed
        .iter()
        .filter(|i| i.resolved_at.is_some_and(|at| at >= since))
        .count() as u64;
    let items = changed
        .iter()
        .take(12)
        .map(|i| DigestItem {
            title: i.title.clone(),
            status: i.status.clone(),
            change: match i.change_pct {
                Some(pct) => format!(
                    "{}{pct}% ({})",
                    if pct > 0.0 { "+" } else { "" },
                    i.window_label
                ),
                None => format!("from nothing ({})", i.window_label),
            },
        })
        .collect();
    let period = format!(
        "{} to {}",
        since.with_timezone(&tz).format("%-d %B"),
        now.with_timezone(&tz).format("%-d %B")
    );
    DigestPayload {
        period,
        opened,
        resolved,
        items,
        link: crate::notify::dashboard_link(ctx, site.site_id_u16(), "insights"),
        ai_configured: ctx.cfg.llm.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_monday_morning_is_due() {
        assert!(is_due(Weekday::Mon, 8));
        assert!(is_due(Weekday::Mon, 23));
        assert!(!is_due(Weekday::Mon, 7));
        assert!(!is_due(Weekday::Tue, 9));
        assert!(!is_due(Weekday::Sun, 9));
    }

    #[test]
    fn a_marker_names_its_site_and_week() {
        let site = uuid::Uuid::from_u128(5);
        assert_eq!(
            marker_key(site, 2026, 3),
            format!("mf:insights:digest:{site}:2026-W03")
        );
        assert_ne!(marker_key(site, 2026, 3), marker_key(site, 2026, 4));
    }
}
