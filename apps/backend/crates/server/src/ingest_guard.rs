//! Ingest guards: a per-site allow list and deny list of addresses, checked
//! on every ingest path before anything is stored.
//!
//! An operator uses them to keep their own office, an uptime monitor, a load
//! test or an abusive scraper out of a site's numbers (deny list), or to make a
//! staging site record only the team's own addresses (allow list). The rules:
//!
//! 1. **The deny list wins.** An address inside any deny entry is blocked,
//!    whatever the allow list says.
//! 2. **A non-empty allow list is exclusive.** When it has entries, only
//!    addresses inside one of them are recorded; a hit with no address at all
//!    (an edge row the shipper sent without one) is blocked too, because it
//!    cannot be shown to be inside the list.
//! 3. **Blocked is silent.** A blocked hit is answered exactly as an accepted
//!    one (`204`, the pixel, the edge batch's `accepted` count aside), so the
//!    page is never broken and a scraper learns nothing from the answer. It is
//!    counted, by reason, in Valkey ([`count_blocked`]), and the
//!    `notify_blocked_traffic` job mails the site's admins when a day's count
//!    passes the site's threshold.
//!
//! IPv4 addresses that arrive as IPv4-mapped IPv6 (`::ffff:203.0.113.7`, which
//! a dual-stack socket reports) are compared as the IPv4 address they are, so
//! one IPv4 entry covers both spellings.
//!
//! ## For new ingest paths
//!
//! Every path that stores a hit calls one of two functions before it writes:
//!
//! - [`admit`] with the [`Guard`] it already parsed (the browser path keeps
//!   one on its cached site context), or
//! - [`admit_site`] with just the numeric site id, which reads the guard
//!   through a short process cache.
//!
//! Both count a blocked hit and answer `false` for it; the caller then
//! answers as if it had stored the hit and stores nothing.
//!
//! The lists live in the site's settings document under `ingest_guards`,
//! written only through `PATCH /api/sites/{site}/ingest-guards`
//! (`routes::guards`), which validates every entry. A settings `PATCH` cannot
//! write the key.

use std::collections::HashMap;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, Utc};
use ipnet::IpNet;
use micaforge_core::{Error, Result};
use micaforge_store::kv::blocked_key;
use micaforge_store::models::sites;
use serde_json::{json, Value};

use crate::middleware::{LogGate, LIMITER_BUDGET};
use crate::state::AppState;

/// Where the guard lives inside `sites.settings`.
pub const SETTINGS_KEY: &str = "ingest_guards";

/// Most entries one list may hold.
pub const MAX_ENTRIES: usize = 200;

/// Longest note on one entry, in characters.
const MAX_NOTE: usize = 120;

/// How long a blocked-request counter is kept: the screen shows a week, and
/// the daily digest reads yesterday, so a day of slack covers both.
const COUNTER_TTL_SECS: u32 = 9 * 24 * 60 * 60;

/// How long [`admit_site`] trusts a guard it read.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Most sites [`admit_site`] holds a guard for.
const CACHE_MAX: usize = 4096;

/// Why a hit was not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Blocked {
    /// The address is on the deny list.
    Denylist,
    /// The site has an allow list and the address is not on it.
    NotAllowlisted,
    /// The hit named a hostname that is not one of the site's.
    Hostname,
}

impl Blocked {
    /// Every reason, in the order the screen lists them.
    pub const ALL: [Blocked; 3] = [
        Blocked::Denylist,
        Blocked::NotAllowlisted,
        Blocked::Hostname,
    ];

    /// The wire and counter spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Blocked::Denylist => "denylist",
            Blocked::NotAllowlisted => "not_allowlisted",
            Blocked::Hostname => "hostname",
        }
    }
}

/// A site's two lists, parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Guard {
    allow: Vec<IpNet>,
    deny: Vec<IpNet>,
}

impl Guard {
    /// Read the guard out of a site's settings document.
    ///
    /// Lenient: an entry that does not parse is skipped. Entries are checked
    /// when they are written, so this only meets one that somebody wrote into
    /// Postgres by hand.
    pub fn from_settings(settings: &Value) -> Guard {
        let section = settings.get(SETTINGS_KEY);
        let list = |name: &str| -> Vec<IpNet> {
            section
                .and_then(|s| s.get(name))
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.get("cidr").and_then(Value::as_str))
                        .filter_map(|raw| parse_net(raw).ok())
                        .take(MAX_ENTRIES)
                        .collect()
                })
                .unwrap_or_default()
        };
        Guard {
            allow: list("allow"),
            deny: list("deny"),
        }
    }

    /// True when the site has no guard at all.
    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.deny.is_empty()
    }

    /// Why this address is blocked, or `None` to let it through.
    pub fn check(&self, ip: Option<IpAddr>) -> Option<Blocked> {
        if self.is_empty() {
            return None;
        }
        let ip = ip.map(|ip| ip.to_canonical());
        if let Some(ip) = ip {
            if self.deny.iter().any(|net| net.contains(&ip)) {
                return Some(Blocked::Denylist);
            }
        }
        if self.allow.is_empty() {
            return None;
        }
        match ip {
            Some(ip) if self.allow.iter().any(|net| net.contains(&ip)) => None,
            _ => Some(Blocked::NotAllowlisted),
        }
    }
}

/// Parse one entry: an address or a CIDR block, IPv4 or IPv6, normalised to
/// its network (`203.0.113.9/24` is `203.0.113.0/24`) so two spellings of one
/// block compare equal. An IPv4-mapped IPv6 entry is read as IPv4.
pub fn parse_net(raw: &str) -> Result<IpNet> {
    let text = raw.trim();
    let net = match IpNet::from_str(text) {
        Ok(net) => net,
        Err(_) => {
            let ip = IpAddr::from_str(text).map_err(|_| {
                Error::validation(
                    "cidr",
                    format!("`{text}` is not an IP address or a CIDR block such as 203.0.113.0/24"),
                )
            })?;
            IpNet::from(ip)
        }
    };
    Ok(match net {
        IpNet::V6(v6) => match v6.addr().to_ipv4_mapped() {
            Some(v4) => {
                let prefix = v6.prefix_len().saturating_sub(96).min(32);
                IpNet::new(IpAddr::V4(v4), prefix).unwrap_or(IpNet::from(IpAddr::V4(v4)))
            }
            None => IpNet::V6(v6.trunc()),
        },
        IpNet::V4(v4) => IpNet::V4(v4.trunc()),
    })
}

/// Validate one list from a request and return it normalised:
/// `[{ cidr, note }]`, duplicates refused.
pub fn clean_list(raw: &Value, field: &'static str) -> Result<Value> {
    let items = match raw {
        Value::Null => return Ok(json!([])),
        Value::Array(items) => items,
        _ => return Err(Error::validation(field, "must be a list of { cidr, note }")),
    };
    if items.len() > MAX_ENTRIES {
        return Err(Error::validation(
            field,
            format!("holds more than {MAX_ENTRIES} entries"),
        ));
    }
    let mut seen = Vec::with_capacity(items.len());
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let (raw_cidr, note) = match item {
            Value::String(s) => (s.as_str(), ""),
            Value::Object(map) => (
                map.get("cidr").and_then(Value::as_str).unwrap_or(""),
                map.get("note").and_then(Value::as_str).unwrap_or(""),
            ),
            _ => return Err(Error::validation(field, "every entry needs a `cidr`")),
        };
        let net = parse_net(raw_cidr).map_err(|e| {
            Error::validation(
                field,
                e.to_string().trim_start_matches("cidr: ").to_string(),
            )
        })?;
        if seen.contains(&net) {
            return Err(Error::validation(field, format!("`{net}` is listed twice")));
        }
        let note = note.trim();
        if note.chars().count() > MAX_NOTE {
            return Err(Error::validation(
                field,
                format!("a note can be at most {MAX_NOTE} characters"),
            ));
        }
        seen.push(net);
        out.push(json!({ "cidr": net.to_string(), "note": note }));
    }
    Ok(Value::Array(out))
}

/// The alert threshold stored with the guard: blocked requests per day that
/// trigger the daily email. `0` is off.
pub fn alert_threshold(settings: &Value) -> u64 {
    settings
        .get(SETTINGS_KEY)
        .and_then(|s| s.get("alert_threshold"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Checking, and counting
// ---------------------------------------------------------------------------

/// Check an address against a guard the caller already holds, counting a
/// blocked hit. `true` means store the hit.
pub fn admit(st: &AppState, site_id: u16, guard: &Guard, ip: Option<IpAddr>) -> bool {
    match guard.check(ip) {
        None => true,
        Some(reason) => {
            count_blocked(st, site_id, reason);
            false
        }
    }
}

/// [`admit`] for a path that has only the numeric site id. The guard is read
/// from the site row through a process cache of [`CACHE_TTL`]; when the row
/// cannot be read at all, the hit is admitted, because an ingest guard is a
/// filter on good data and not an access control, and a Postgres outage must
/// not become a collection outage.
pub async fn admit_site(st: &AppState, site_id: u16, ip: Option<IpAddr>) -> bool {
    let guard = cached_guard(st, site_id).await;
    admit(st, site_id, &guard, ip)
}

/// Count one blocked request, without waiting for Valkey.
///
/// Spawned rather than awaited: the request is answered the same way whether
/// or not the count lands, so it should not wait for it. Bounded by the
/// limiter's budget so a stuck Valkey cannot pile up tasks.
pub fn count_blocked(st: &AppState, site_id: u16, reason: Blocked) {
    static FAILED: LogGate = LogGate::new();
    let kv = st.kv.clone();
    let key = blocked_key(site_id, Utc::now().date_naive(), reason.as_str());
    tokio::spawn(async move {
        let counted =
            tokio::time::timeout(LIMITER_BUDGET, kv.count(&key, 1, COUNTER_TTL_SECS)).await;
        if !matches!(counted, Ok(Ok(_))) && FAILED.open(Duration::from_secs(60)) {
            tracing::warn!(site_id, "a blocked request could not be counted");
        }
    });
}

/// One day of blocked counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayCounts {
    /// The UTC day.
    pub day: NaiveDate,
    /// By reason, in [`Blocked::ALL`] order.
    pub counts: [u64; 3],
}

/// The last `days` UTC days of blocked counts for a site, oldest first. A
/// Valkey that cannot answer reads as zero, and says so in the log.
pub async fn blocked_days(st: &AppState, site_id: u16, days: u32) -> Vec<DayCounts> {
    let today = Utc::now().date_naive();
    let dates: Vec<NaiveDate> = (0..days)
        .rev()
        .filter_map(|back| today.checked_sub_days(chrono::Days::new(u64::from(back))))
        .collect();
    let keys: Vec<String> = dates
        .iter()
        .flat_map(|day| {
            Blocked::ALL
                .iter()
                .map(move |reason| blocked_key(site_id, *day, reason.as_str()))
        })
        .collect();
    let values = match st.kv.read_counts(&keys).await {
        Ok(values) => values,
        Err(e) => {
            tracing::warn!(site_id, error = %e, "blocked counts could not be read");
            vec![0; keys.len()]
        }
    };
    dates
        .iter()
        .enumerate()
        .map(|(i, day)| DayCounts {
            day: *day,
            counts: [
                values.get(i * 3).copied().unwrap_or(0),
                values.get(i * 3 + 1).copied().unwrap_or(0),
                values.get(i * 3 + 2).copied().unwrap_or(0),
            ],
        })
        .collect()
}

/// One cached guard.
struct Cached {
    until: Instant,
    guard: Arc<Guard>,
}

/// A site's guard through the process cache. See [`admit_site`].
async fn cached_guard(st: &AppState, site_id: u16) -> Arc<Guard> {
    static CACHE: OnceLock<RwLock<HashMap<u16, Cached>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(HashMap::new()));

    if let Ok(map) = cache.read() {
        if let Some(hit) = map.get(&site_id) {
            if hit.until > Instant::now() {
                return Arc::clone(&hit.guard);
            }
        }
    }

    let guard = match tokio::time::timeout(
        Duration::from_secs(1),
        sites::get_by_site_id(&st.pg, site_id),
    )
    .await
    {
        Ok(Ok(site)) => Arc::new(Guard::from_settings(&site.settings)),
        _ => return Arc::new(Guard::default()),
    };

    if let Ok(mut map) = cache.write() {
        if map.len() >= CACHE_MAX {
            let now = Instant::now();
            map.retain(|_, entry| entry.until > now);
            if map.len() >= CACHE_MAX {
                map.clear();
            }
        }
        map.insert(
            site_id,
            Cached {
                until: Instant::now() + CACHE_TTL,
                guard: Arc::clone(&guard),
            },
        );
    }
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(allow: &[&str], deny: &[&str]) -> Guard {
        let entries = |list: &[&str]| -> Vec<Value> {
            list.iter()
                .map(|c| json!({ "cidr": c, "note": "" }))
                .collect()
        };
        Guard::from_settings(&json!({
            SETTINGS_KEY: { "allow": entries(allow), "deny": entries(deny) }
        }))
    }

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn cidr_blocks_match_their_own_addresses_only() {
        let g = guard(&[], &["203.0.113.0/24", "2001:db8::/32", "198.51.100.7"]);
        assert_eq!(g.check(ip("203.0.113.0")), Some(Blocked::Denylist));
        assert_eq!(g.check(ip("203.0.113.255")), Some(Blocked::Denylist));
        assert_eq!(g.check(ip("203.0.114.1")), None);
        assert_eq!(g.check(ip("2001:db8:1::5")), Some(Blocked::Denylist));
        assert_eq!(g.check(ip("2001:db9::1")), None);
        // A bare address is a /32.
        assert_eq!(g.check(ip("198.51.100.7")), Some(Blocked::Denylist));
        assert_eq!(g.check(ip("198.51.100.8")), None);
    }

    #[test]
    fn a_mapped_ipv4_address_is_its_ipv4_address() {
        let g = guard(&[], &["203.0.113.0/24"]);
        assert_eq!(g.check(ip("::ffff:203.0.113.9")), Some(Blocked::Denylist));
        // And a mapped entry covers the plain spelling.
        let mapped = guard(&[], &["::ffff:198.51.100.0/120"]);
        assert_eq!(mapped.check(ip("198.51.100.20")), Some(Blocked::Denylist));
    }

    #[test]
    fn the_deny_list_wins_over_the_allow_list() {
        let g = guard(&["10.0.0.0/8"], &["10.1.0.0/16"]);
        assert_eq!(g.check(ip("10.2.3.4")), None);
        assert_eq!(g.check(ip("10.1.3.4")), Some(Blocked::Denylist));
        assert_eq!(g.check(ip("192.0.2.1")), Some(Blocked::NotAllowlisted));
    }

    #[test]
    fn an_allow_list_refuses_a_hit_with_no_address() {
        assert_eq!(
            guard(&["10.0.0.0/8"], &[]).check(None),
            Some(Blocked::NotAllowlisted)
        );
        // A deny list alone has nothing to say about an unknown address.
        assert_eq!(guard(&[], &["10.0.0.0/8"]).check(None), None);
        assert_eq!(Guard::default().check(None), None);
    }

    #[test]
    fn entries_are_normalised_and_bad_ones_refused() {
        let cleaned = clean_list(
            &json!([
                { "cidr": " 203.0.113.9/24 ", "note": " office " },
                "2001:DB8::1",
                { "cidr": "::ffff:192.0.2.1" }
            ]),
            "deny",
        )
        .unwrap();
        assert_eq!(
            cleaned,
            json!([
                { "cidr": "203.0.113.0/24", "note": "office" },
                { "cidr": "2001:db8::1/128", "note": "" },
                { "cidr": "192.0.2.1/32", "note": "" }
            ])
        );

        for bad in [
            json!([{ "cidr": "not an address" }]),
            json!([{ "cidr": "203.0.113.0/33" }]),
            json!([{ "cidr": "203.0.113.0/24" }, { "cidr": "203.0.113.7/24" }]),
            json!([{ "cidr": "10.0.0.1", "note": "x".repeat(MAX_NOTE + 1) }]),
            json!([42]),
            json!("10.0.0.0/8"),
        ] {
            assert_eq!(
                clean_list(&bad, "deny").unwrap_err().field(),
                Some("deny"),
                "{bad}"
            );
        }
        let too_many: Vec<Value> = (0..=MAX_ENTRIES)
            .map(|i| json!({ "cidr": format!("10.0.{}.{}", i / 256, i % 256) }))
            .collect();
        assert!(clean_list(&Value::Array(too_many), "allow").is_err());
    }

    #[test]
    fn the_threshold_reads_as_off_when_absent() {
        assert_eq!(alert_threshold(&json!({})), 0);
        assert_eq!(
            alert_threshold(&json!({ SETTINGS_KEY: { "alert_threshold": 500 } })),
            500
        );
    }
}
