//! `notify_blocked_traffic` — the daily email about requests a site's ingest
//! guards blocked.
//!
//! Once a day this reads yesterday's (UTC) blocked counts for every site that
//! set an alert threshold (`settings.ingest_guards.alert_threshold`, written
//! by the Ingest guards screen), and mails the site's admins when the day's
//! total reached it. The counts are the ones the server's ingest guard keeps in
//! Valkey ([`micaforge_store::kv::blocked_key`]); a Valkey that lost them
//! means no email for that day, which is the honest answer to "how much was
//! blocked" when nobody knows.
//!
//! A day is mailed at most once per site, whichever replica runs the sweep:
//! the send is claimed with a one-shot Valkey counter first.

use chrono::{Days, Utc};
use micaforge_core::Result;
use micaforge_mail::templates::BlockedTraffic;
use micaforge_store::kv::{blocked_key, BLOCKED_REASONS};
use micaforge_store::models::{orgs, sites, users};
use micaforge_store::pg::PgPool;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::JobCtx;

/// How long the "already mailed" claim on a site's day lives, in seconds. Two
/// days covers a sweep that runs late.
const CLAIM_SECS: u32 = 2 * 24 * 60 * 60;

/// Nothing to configure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {}

/// The threshold a site set, `0` when it set none.
fn threshold(settings: &Value) -> u64 {
    settings
        .get("ingest_guards")
        .and_then(|g| g.get("alert_threshold"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// Check every site with a threshold against yesterday's counts.
pub async fn run(ctx: &JobCtx, _payload: Payload) -> Result<()> {
    let Some(day) = Utc::now().date_naive().checked_sub_days(Days::new(1)) else {
        return Ok(());
    };
    let mut mailed = 0usize;

    for site in sites::list_all(&ctx.pg).await? {
        let limit = threshold(&site.settings);
        if limit == 0 {
            continue;
        }
        let site_id = site.site_id_u16();
        let keys: Vec<String> = BLOCKED_REASONS
            .iter()
            .map(|reason| blocked_key(site_id, day, reason))
            .collect();
        let counts = match ctx.kv.read_counts(&keys).await {
            Ok(counts) => counts,
            Err(e) => {
                tracing::warn!(site_id, error = %e, "blocked counts could not be read");
                continue;
            }
        };
        let blocked = BlockedTraffic {
            day: day.format("%-d %B").to_string(),
            denylist: counts.first().copied().unwrap_or(0),
            not_allowlisted: counts.get(1).copied().unwrap_or(0),
            hostname: counts.get(2).copied().unwrap_or(0),
            threshold: limit,
            link: format!("{}/app/{site_id}/settings/guards", ctx.cfg.dashboard_url),
        };
        if blocked.total() < limit {
            continue;
        }
        // One email per site per day, whoever gets here first.
        if !ctx
            .kv
            .rate_limit(&format!("guard-digest:{site_id}:{day}"), 1, CLAIM_SECS)
            .await?
        {
            continue;
        }

        let label = if site.name.is_empty() {
            site.domain.clone()
        } else {
            site.name.clone()
        };
        for address in site_admin_emails(&ctx.pg, &site).await? {
            match ctx
                .mailer
                .send_blocked_traffic(&address, &label, &blocked)
                .await
            {
                Ok(()) => mailed += 1,
                Err(e) => {
                    tracing::warn!(site_id, error = %e, "a blocked-traffic email could not be sent")
                }
            }
        }
    }
    tracing::info!(mailed, %day, "blocked-traffic digest finished");
    Ok(())
}

/// The addresses of everybody who administers a site: the organisation's
/// owner and admins, less anybody a per-site override narrowed below admin,
/// plus anybody an override raised to admin on this site.
async fn site_admin_emails(pg: &PgPool, site: &sites::Site) -> Result<Vec<String>> {
    let overrides = sites::list_access(pg, site.id).await?;
    let mut ids = Vec::new();
    for member in orgs::list_members(pg, site.org_id).await? {
        let role = overrides
            .iter()
            .find(|o| o.user_id == member.user_id)
            .map(|o| o.role.as_str())
            .unwrap_or(member.role.as_str());
        if role == "owner" || role == "admin" {
            ids.push(member.user_id);
        }
    }
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(user) = users::get(pg, id).await {
            out.push(user.email);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_site_without_a_threshold_is_never_mailed() {
        assert_eq!(threshold(&json!({})), 0);
        assert_eq!(threshold(&json!({ "ingest_guards": { "deny": [] } })), 0);
        assert_eq!(
            threshold(&json!({ "ingest_guards": { "alert_threshold": 250 } })),
            250
        );
    }
}
