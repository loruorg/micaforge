//! Profiles: what a site's own `identify()` and `setTraits()` calls recorded.
//!
//! Micaforge identifies nobody. A visitor is a daily-rotating hash, and a
//! profile exists only because the site called `identify("u_123")`, which the
//! tracker sends as an `identify` event: `identified_id` is the site's id,
//! `visitor_id` is that day's hash, and `props` holds the traits sent with the
//! call. Everything here is read off those rows, so a profile shows exactly what
//! the site chose to tie together and nothing the engine inferred:
//!
//! - **Trait history.** Every call, oldest first, with the keys that changed.
//!   The current traits are the calls folded in order, a later value winning.
//! - **Linked visitors.** The day hashes an `identify` call carried. Each one is
//!   a day on which the site said "this browser is u_123"; nothing links a hash
//!   the site never named, so the cookieless design holds: the link is the
//!   site's statement, not a fingerprint.
//! - **Earlier anonymous sessions.** Sessions of those linked hashes recorded
//!   before the call, with no `identified_id` on them yet: what the person did
//!   on that day before signing in.
//!
//! Search reads the same rows: an id or any trait text (an email address, a
//! company), matched without regard to case.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use clickhouse::Row;
use micaforge_core::{QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

use crate::exec::{fetch_all, ms_to_dt};
use crate::sql::{millis, scope_predicate, Param};

/// Most identify calls read for one profile.
const MAX_CALLS: u32 = 1_000;

/// Most linked visitor hashes listed, and so the most whose sessions are read.
const MAX_LINKED: usize = 500;

/// Most earlier anonymous sessions listed.
const MAX_ANON_SESSIONS: u32 = 50;

/// Most profiles a search returns.
const MAX_HITS: u32 = 50;

/// Shortest search text. One character matches nearly every profile, which is
/// a list, not a search.
pub const MIN_QUERY: usize = 2;

/// One `identify` or `setTraits` call that changed something.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraitChange {
    /// When the call was received.
    pub t: DateTime<Utc>,
    /// The traits the call sent.
    pub traits: Map<String, Json>,
    /// The keys whose value differs from the folded traits before the call.
    pub changed: Vec<String>,
}

/// One day hash the site linked to the id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LinkedVisitor {
    /// The daily visitor hash.
    pub visitor_id: String,
    /// The first call that carried it.
    pub linked_at: DateTime<Utc>,
}

/// A session recorded before the site identified the person.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AnonymousSession {
    /// The session.
    pub session_id: String,
    /// The day hash it was recorded under.
    pub visitor_id: String,
    /// Its first hit.
    pub started: DateTime<Utc>,
    /// Its last hit.
    pub ended: DateTime<Utc>,
    /// Pageviews in it.
    pub pageviews: u64,
    /// Its first page.
    pub entry_path: String,
    /// How it arrived.
    pub channel: String,
}

/// A profile, as the site's calls built it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    /// The site's id for the person.
    pub identified_id: String,
    /// Every call folded in order.
    pub traits: Map<String, Json>,
    /// The calls that changed something, oldest first.
    pub history: Vec<TraitChange>,
    /// The day hashes linked to the id, oldest first.
    pub linked: Vec<LinkedVisitor>,
    /// Sessions of those hashes from before the id was attached, newest first.
    pub anonymous_sessions: Vec<AnonymousSession>,
}

#[derive(Debug, Row, Deserialize)]
struct CallRaw {
    t: i64,
    visitor_id: String,
    props: String,
}

#[derive(Debug, Row, Deserialize)]
struct SessionRaw {
    session_id: String,
    visitor_id: String,
    started: i64,
    ended: i64,
    pageviews: u64,
    entry_path: String,
    channel: String,
}

/// `GET /api/stats/profiles/:id`. `None` when the site never identified `id`
/// inside the window.
pub async fn profile(ch: &Ch, env: &QueryEnvelope, id: &str) -> Result<Option<Profile>> {
    let (scope, mut params) = scope_predicate(env, Table::Events)?;
    params.push(Param::Str(id.to_string()));
    let calls = fetch_all::<CallRaw>(
        ch,
        "profile_calls",
        &format!(
            "SELECT {t} AS t, visitor_id, toJSONString(props) AS p
FROM events
WHERE {scope} AND kind = 'identify' AND identified_id = ?
ORDER BY timestamp ASC
LIMIT {MAX_CALLS}",
            t = millis("timestamp"),
        ),
        &params,
    )
    .await?;
    if calls.is_empty() {
        return Ok(None);
    }

    let (traits, history) = fold_traits(&calls);
    let linked = linked_visitors(&calls);

    // The sessions of the linked hashes that carry no id: the person's day
    // before the site said who they were.
    let anonymous_sessions = if linked.is_empty() {
        Vec::new()
    } else {
        let (scope, mut params) = scope_predicate(env, Table::Events)?;
        let slots = vec!["?"; linked.len()].join(", ");
        params.extend(linked.iter().map(|l| Param::Str(l.visitor_id.clone())));
        fetch_all::<SessionRaw>(
            ch,
            "profile_anonymous_sessions",
            &format!(
                "SELECT session_id, any(visitor_id) AS vid,
  {started} AS started, {ended} AS ended,
  toUInt64(countIf(kind = 'pageview')) AS pv,
  argMin(pathname, timestamp) AS entry_p,
  any(toString(channel)) AS chan
FROM events
WHERE {scope} AND visitor_id IN ({slots})
GROUP BY session_id
HAVING max(identified_id) = ''
ORDER BY started DESC
LIMIT {MAX_ANON_SESSIONS}",
                started = millis("min(timestamp)"),
                ended = millis("max(timestamp)"),
            ),
            &params,
        )
        .await?
        .into_iter()
        .map(|s| AnonymousSession {
            session_id: s.session_id,
            visitor_id: s.visitor_id,
            started: ms_to_dt(s.started),
            ended: ms_to_dt(s.ended),
            pageviews: s.pageviews,
            entry_path: s.entry_path,
            channel: s.channel,
        })
        .collect()
    };

    Ok(Some(Profile {
        identified_id: id.to_string(),
        traits,
        history,
        linked,
        anonymous_sessions,
    }))
}

/// Fold the calls, oldest first, into the current traits and the history of
/// the calls that changed something.
///
/// A call that repeats what is already known is not history: an app that calls
/// `identify` on every page load would otherwise bury the one change that
/// matters under a hundred identical lines. A call that sent no traits at all
/// (a bare `identify("u_1")`) only links a visitor and is not history either.
fn fold_traits(calls: &[CallRaw]) -> (Map<String, Json>, Vec<TraitChange>) {
    let mut current = Map::new();
    let mut history = Vec::new();
    for call in calls {
        let Ok(Json::Object(sent)) = serde_json::from_str::<Json>(&call.props) else {
            continue;
        };
        let changed: Vec<String> = sent
            .iter()
            .filter(|(k, v)| current.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .collect();
        if changed.is_empty() {
            continue;
        }
        for (k, v) in &sent {
            current.insert(k.clone(), v.clone());
        }
        history.push(TraitChange {
            t: ms_to_dt(call.t),
            traits: sent,
            changed,
        });
    }
    (current, history)
}

/// The distinct day hashes the calls carried, each at its first call.
fn linked_visitors(calls: &[CallRaw]) -> Vec<LinkedVisitor> {
    let mut first: BTreeMap<&str, i64> = BTreeMap::new();
    for call in calls.iter().filter(|c| !c.visitor_id.is_empty()) {
        first.entry(call.visitor_id.as_str()).or_insert(call.t);
    }
    let mut linked: Vec<LinkedVisitor> = first
        .into_iter()
        .map(|(visitor_id, t)| LinkedVisitor {
            visitor_id: visitor_id.to_string(),
            linked_at: ms_to_dt(t),
        })
        .collect();
    linked.sort_by_key(|l| l.linked_at);
    linked.truncate(MAX_LINKED);
    linked
}

/// One profile a search found.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProfileHit {
    /// The site's id for the person.
    pub identified_id: String,
    /// The traits of the most recent call that sent any.
    pub traits: Map<String, Json>,
    /// The most recent call.
    pub last_seen: DateTime<Utc>,
    /// Day hashes linked to the id.
    pub visitors: u64,
}

#[derive(Debug, Row, Deserialize)]
struct HitRaw {
    identified_id: String,
    traits: String,
    last_seen: i64,
    visitors: u64,
}

/// `GET /api/stats/profiles?q=`: profiles whose id or any trait contains `q`,
/// without regard to case, most recently seen first.
///
/// The match runs over the text of each call's traits, so an email address, a
/// company or a plan name all find the person, and a value the site has since
/// changed still finds them through the call that carried it.
pub async fn search_profiles(ch: &Ch, env: &QueryEnvelope, q: &str) -> Result<Vec<ProfileHit>> {
    let (scope, mut params) = scope_predicate(env, Table::Events)?;
    params.push(Param::Str(q.to_string()));
    params.push(Param::Str(q.to_string()));
    let rows = fetch_all::<HitRaw>(
        ch,
        "profile_search",
        &format!(
            "SELECT identified_id,
  argMaxIf(toJSONString(props), timestamp, toJSONString(props) != '{{}}') AS latest,
  {last} AS last_seen,
  uniq(visitor_id) AS linked
FROM events
WHERE {scope} AND kind = 'identify' AND identified_id != ''
GROUP BY identified_id
HAVING max(positionCaseInsensitiveUTF8(identified_id, ?) > 0
          OR positionCaseInsensitiveUTF8(toJSONString(props), ?) > 0)
ORDER BY last_seen DESC
LIMIT {MAX_HITS}",
            last = millis("max(timestamp)"),
        ),
        &params,
    )
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ProfileHit {
            identified_id: r.identified_id,
            traits: match serde_json::from_str::<Json>(&r.traits) {
                Ok(Json::Object(map)) => map,
                _ => Map::new(),
            },
            last_seen: ms_to_dt(r.last_seen),
            visitors: r.visitors,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(t: i64, visitor: &str, props: &str) -> CallRaw {
        CallRaw {
            t,
            visitor_id: visitor.into(),
            props: props.into(),
        }
    }

    #[test]
    fn history_keeps_the_calls_that_changed_something() {
        let calls = vec![
            call(1_000, "v1", r#"{"email":"a@example.com","plan":"free"}"#),
            call(2_000, "v1", r#"{"email":"a@example.com","plan":"free"}"#),
            call(3_000, "v1", "{}"),
            call(4_000, "v2", r#"{"plan":"pro"}"#),
            call(5_000, "v2", r#"{"plan":"pro","seats":5}"#),
        ];
        let (traits, history) = fold_traits(&calls);
        assert_eq!(history.len(), 3, "repeats and bare calls are not history");
        assert_eq!(history[0].changed, vec!["email", "plan"]);
        assert_eq!(history[1].changed, vec!["plan"]);
        assert_eq!(history[2].changed, vec!["seats"]);
        assert_eq!(traits["plan"], "pro");
        assert_eq!(traits["email"], "a@example.com");
        assert_eq!(traits["seats"], 5);
    }

    #[test]
    fn a_visitor_is_linked_only_by_a_call_that_carried_it() {
        let calls = vec![
            call(5_000, "v2", "{}"),
            call(1_000, "v1", "{}"),
            call(3_000, "v1", "{}"),
            call(4_000, "", "{}"),
        ];
        let linked = linked_visitors(&calls);
        assert_eq!(linked.len(), 2);
        assert_eq!(linked[0].visitor_id, "v1");
        assert_eq!(linked[0].linked_at, ms_to_dt(1_000));
        assert_eq!(linked[1].visitor_id, "v2");
    }

    #[test]
    fn a_broken_props_document_is_skipped_not_fatal() {
        let (traits, history) =
            fold_traits(&[call(1, "v", "not json"), call(2, "v", r#"{"a":1}"#)]);
        assert_eq!(history.len(), 1);
        assert_eq!(traits["a"], 1);
    }
}
