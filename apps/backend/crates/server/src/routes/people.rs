//! Sessions, visitors and recordings: the reports where a row is a person's
//! visit rather than a total. `docs/CONTRACT.md` §3.3.
//!
//! ## A record is addressed by its id, not by a window
//!
//! The list endpoints are scoped by the envelope exactly like every other read.
//! The two *detail* endpoints are not, and that is deliberate: a caller who
//! followed a link to one session already knows which session it wants, and
//! answering `404` because the caliper happens to be set to "last 24 hours"
//! would be a worse answer than the record itself. So a detail read widens the
//! window to the site's whole retained history — bounded by
//! `MICAFORGE_RETENTION_DAYS`, so the scan covers the rows that exist and no
//! more — while the filters and the audience the caller sent still apply.
//!
//! ## Why the replay routes build their own statements
//!
//! `micaforge-query` reports on events. A recording is not a report: it is a
//! blob of rrweb frames read back in order for one session and never aggregated,
//! never ranked, never compared. There is no report function to call, so these
//! three routes read `replay_events` directly — under the same rules the engine
//! holds itself to. Every caller value is a bind parameter, every integer
//! formatted into the text is clamped here first, and nothing else about a
//! request reaches the SQL.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};

use micaforge_core::{Error, QueryEnvelope, TimeRange};
use micaforge_query::bind_all;
use micaforge_query::exec::fetch_all;
use micaforge_query::sql::{fmt_ts, Param};
use micaforge_query::stats;

use super::agents::{json_body, same_site};
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::response::{ok, ok_meta};
use crate::routes::orgs::{require_at_least, ADMIN};
use crate::state::AppState;

/// Session lists, visitor lists, their detail records, and the replay store.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/sessions", get(sessions))
        // Written `:id` in the contract; axum 0.8 spells a path parameter
        // `{id}`. Same route.
        .route("/api/stats/sessions/{id}", get(session_detail))
        .route("/api/stats/visitors", get(visitors))
        .route(
            "/api/stats/visitors/{id}",
            get(visitor_detail).delete(forget_visitor),
        )
        .route("/api/replay", get(replay_list))
        .route(
            "/api/replay/{session}",
            get(replay_detail).delete(replay_delete),
        )
}

/// Most rrweb frames one response will carry.
///
/// A long recording of a busy single-page app can run to hundreds of thousands
/// of frames, and the whole array has to be in memory to be checked for validity
/// before it is sent. This is the point past which a recording is truncated and
/// says so, rather than the point past which the process runs out of memory.
const MAX_REPLAY_EVENTS: u64 = 50_000;

/// Recordings listed on one page, at most.
const MAX_REPLAY_ROWS: u32 = 500;

/// How far either side of a recording its own session's events are looked for.
///
/// The `events` sort key leads with the timestamp, so scanning for one session
/// id across every partition would read the whole table. A session's events are
/// by definition contemporaneous with its recording, so an hour of slack on each
/// side both finds them and prunes the read down to one or two partitions.
const REPLAY_CONTEXT_SLACK_HOURS: i64 = 1;

/// Session ids collected in one erasure pass.
///
/// Erasure is rare and manual, so this is a guard against an unbounded array
/// being built in a bind parameter rather than a limit anybody should hit.
const MAX_ERASURE_SESSIONS: u32 = 10_000;

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

/// The envelope widened to everything the install still holds.
///
/// The start is the retention horizon, not the epoch: partitions older than the
/// retention window do not exist, and asking ClickHouse to consider them costs a
/// planning pass for nothing. With retention disabled (`0`, meaning "forever")
/// there is no horizon, so the epoch is the honest bound.
pub(super) fn whole_history(env: &QueryEnvelope, retention_days: u32) -> QueryEnvelope {
    let end = Utc::now() + Duration::hours(1);
    let start = if retention_days == 0 {
        DateTime::<Utc>::from_timestamp(0, 0).unwrap_or(end)
    } else {
        end - Duration::days(i64::from(retention_days) + 1)
    };

    let mut widened = env.clone();
    widened.range = TimeRange::new(start, end, env.range.tz);
    widened
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// `GET /api/stats/sessions` — one page of sessions, newest first.
async fn sessions(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let page = stats::sessions(&st.ch, &env).await?;
    let meta = json!({ "rows": page.rows.len(), "total": page.total });
    Ok(ok_meta(
        json!({ "rows": json_body(&page.rows)?, "total": page.total }),
        meta,
    ))
}

/// `GET /api/stats/sessions/:id` — the session's summary and every hit in it.
async fn session_detail(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::validation("id", "name the session to read").into());
    }

    let env = whole_history(&env, st.cfg.retention_days);
    let record = stats::session_detail(&st.ch, &env, id).await?;

    Ok(ok(json!({
        "session": json_body(&record.meta)?,
        "hits": record.hits.iter().map(hit_json).collect::<Vec<Value>>(),
    })))
}

/// One hit, with its instant under both names.
///
/// The engine writes a chart-axis instant as `t`, in epoch milliseconds. The
/// record screen sorts by `timestamp`. They are the same number.
fn hit_json(hit: &stats::Hit) -> Value {
    json!({
        "t": hit.t,
        "timestamp": hit.t,
        "kind": hit.kind,
        "event_name": hit.event_name,
        "pathname": hit.pathname,
        "page_title": hit.page_title,
        "referrer": hit.referrer,
        "duration_ms": hit.duration_ms,
        "scroll_pct": hit.scroll_pct,
        "props": hit.props,
    })
}

// ---------------------------------------------------------------------------
// Visitors
// ---------------------------------------------------------------------------

/// `GET /api/stats/visitors` — one page of visitors.
async fn visitors(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let page = stats::visitors(&st.ch, &env).await?;
    let meta = json!({ "rows": page.rows.len(), "total": page.total });
    Ok(ok_meta(
        json!({ "rows": json_body(&page.rows)?, "total": page.total }),
        meta,
    ))
}

/// `GET /api/stats/visitors/:id` — one visitor's profile, sessions and hits.
///
/// The profile is spread across the top level rather than nested under a
/// `profile` key, so a client reading `data.visitor_id` and one reading
/// `data.sessions` are both reading the object they expected.
async fn visitor_detail(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let id = id.trim();
    if id.is_empty() {
        return Err(Error::validation("id", "name the visitor to read").into());
    }

    let env = whole_history(&env, st.cfg.retention_days);
    let record = stats::visitor_detail(&st.ch, &env, id).await?;

    let mut body = json_body(&record.profile)?;
    if let Value::Object(map) = &mut body {
        map.insert("sessions".into(), json_body(&record.sessions)?);
        map.insert(
            "recent".into(),
            Value::Array(record.recent.iter().map(hit_json).collect()),
        );
    }

    Ok(ok(body))
}

/// `DELETE /api/stats/visitors/:id` — erase everything held about one visitor.
/// Requires `admin` on the site.
///
/// What this can and cannot reach is worth stating plainly, because the person
/// pressing the key is usually acting on a legal request. `visitor_id` is a
/// hash that rotates at midnight in the site's own zone
/// (`docs/CONTRACT.md` §7.2), so a bare visitor hash identifies one day of that
/// person's activity and nothing before it. An `identified_id` — supplied by the
/// site's own `identify()` call — does not rotate, and erasing by one reaches
/// every day. Both are matched here, and a caller erasing on request should be
/// given the identified id when there is one.
///
/// Erasing an identified id also erases the anonymous activity the site tied to
/// it: the rows of every daily hash an `identify()` call linked to the id that
/// carry no id of their own (what the person did that day before signing in).
/// The profile screen shows those sessions as theirs, so erasure must reach
/// them too. Rows of a linked hash that carry somebody else's id are left: on a
/// shared machine they are that other person's.
///
/// Recordings are erased first and events last, because the recordings are found
/// *through* the events: reversing the order would leave the heaviest rows on
/// disk with nothing left pointing at them.
async fn forget_visitor(
    State(st): State<AppState>,
    access: SiteAccess,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // Erasure destroys history for good and is not undone by anything, so it
    // sits with the people who administer the site, not with every reader.
    require_at_least(access.role.as_str(), ADMIN)?;

    let id = id.trim().to_string();
    if id.is_empty() {
        return Err(Error::validation("id", "name the visitor to erase").into());
    }

    let site_id = access.site_id;

    // The daily hashes the site linked to this id. Empty for a bare hash.
    let linked: Vec<String> = fetch_all::<String>(
        &st.ch,
        "erasure_linked",
        &format!(
            "SELECT DISTINCT visitor_id FROM events \
             WHERE site_id = ? AND kind = 'identify' AND identified_id = ? AND visitor_id != '' \
             LIMIT {MAX_ERASURE_SESSIONS}"
        ),
        &[Param::U16(site_id), Param::Str(id.clone())],
    )
    .await?;

    let mut session_ids: Vec<String> = fetch_all::<String>(
        &st.ch,
        "erasure_sessions",
        &format!(
            "SELECT DISTINCT session_id FROM events \
             WHERE site_id = ? AND (visitor_id = ? OR identified_id = ?) \
             LIMIT {MAX_ERASURE_SESSIONS}"
        ),
        &[
            Param::U16(site_id),
            Param::Str(id.clone()),
            Param::Str(id.clone()),
        ],
    )
    .await?;

    if !linked.is_empty() {
        let anonymous: Vec<String> = bind_all(
            st.ch.client().query(&format!(
                "SELECT DISTINCT session_id FROM events \
                 WHERE site_id = ? AND identified_id = '' AND has(?, visitor_id) \
                 LIMIT {MAX_ERASURE_SESSIONS}"
            )),
            &[Param::U16(site_id)],
        )
        .bind(&linked)
        .fetch_all::<String>()
        .await
        .map_err(|e| Error::Database(format!("erasure_linked_sessions: {e}")))?;
        for session in anonymous {
            if !session_ids.contains(&session) {
                session_ids.push(session);
            }
        }
    }

    if !session_ids.is_empty() {
        let query = bind_all(
            st.ch
                .client()
                .query("DELETE FROM replay_events WHERE site_id = ? AND has(?, session_id)"),
            &[Param::U16(site_id)],
        )
        .bind(&session_ids);
        query
            .execute()
            .await
            .map_err(|e| Error::Database(format!("erase_replays: {e}")))?;
    }

    // `errors` carries `visitor_id` but not `identified_id`, so the identified
    // half of the match is reached through the sessions collected above.
    for (report, sql, params) in [
        (
            "erase_errors",
            "DELETE FROM errors WHERE site_id = ? AND visitor_id = ?",
            vec![Param::U16(site_id), Param::Str(id.clone())],
        ),
        (
            "erase_events",
            "DELETE FROM events WHERE site_id = ? AND (visitor_id = ? OR identified_id = ?)",
            vec![
                Param::U16(site_id),
                Param::Str(id.clone()),
                Param::Str(id.clone()),
            ],
        ),
    ] {
        bind_all(st.ch.client().query(sql), &params)
            .execute()
            .await
            .map_err(|e| Error::Database(format!("{report}: {e}")))?;
    }

    if !linked.is_empty() {
        bind_all(
            st.ch.client().query(
                "DELETE FROM events WHERE site_id = ? AND identified_id = '' AND has(?, visitor_id)",
            ),
            &[Param::U16(site_id)],
        )
        .bind(&linked)
        .execute()
        .await
        .map_err(|e| Error::Database(format!("erase_linked_events: {e}")))?;
    }

    if !session_ids.is_empty() {
        let query = bind_all(
            st.ch
                .client()
                .query("DELETE FROM errors WHERE site_id = ? AND has(?, session_id)"),
            &[Param::U16(site_id)],
        )
        .bind(&session_ids);
        query
            .execute()
            .await
            .map_err(|e| Error::Database(format!("erase_session_errors: {e}")))?;
    }

    tracing::info!(
        site = %site_id,
        sessions = session_ids.len(),
        linked = linked.len(),
        "visitor erased"
    );

    Ok(ok(json!({
        "erased": true,
        "sessions": session_ids.len(),
        "linked_visitors": linked.len(),
    })))
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// `GET /api/replay` — the recordings held for a site, newest first.
///
/// The envelope's window applies; its filters and its audience do not, and the
/// reason is not laziness. A recording carries no dimensions of its own — no
/// path, no channel, no country — so "recordings where channel is search" would
/// have to be answered by joining a video to an event stream and guessing which
/// rows to keep. Recordings are a human record: the machine trace has none.
async fn replay_list(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;

    let window = [
        Param::U16(env.site),
        Param::Str(fmt_ts(env.range.start)),
        Param::Str(fmt_ts(env.range.end)),
    ];
    let scope = "site_id = ? \
                 AND timestamp >= toDateTime64(?, 3, 'UTC') \
                 AND timestamp < toDateTime64(?, 3, 'UTC')";

    let total: u64 = fetch_all::<u64>(
        &st.ch,
        "replay_total",
        &format!("SELECT toUInt64(uniqExact(session_id)) FROM replay_events WHERE {scope}"),
        &window,
    )
    .await?
    .into_iter()
    .next()
    .unwrap_or(0);

    let limit = env.limit.clamp(1, MAX_REPLAY_ROWS);
    let offset = env.offset();

    // `(session_id, started, ended, frames, bytes)`.
    let recordings: Vec<(String, i64, i64, u64, u64)> = fetch_all(
        &st.ch,
        "replay_list",
        &format!(
            "SELECT session_id, \
                    toUnixTimestamp64Milli(min(timestamp)) AS started, \
                    toUnixTimestamp64Milli(max(timestamp)) AS ended, \
                    toUInt64(count()) AS frames, \
                    toUInt64(sum(length(data))) AS held \
             FROM replay_events WHERE {scope} \
             GROUP BY session_id \
             ORDER BY started DESC \
             LIMIT {limit} OFFSET {offset}"
        ),
        &window,
    )
    .await?;

    let ids: Vec<String> = recordings.iter().map(|r| r.0.clone()).collect();
    let context = session_context(&st, env.site, env.range.start, env.range.end, &ids).await?;

    let rows: Vec<Value> = recordings
        .iter()
        .map(|(session_id, started, ended, frames, held)| {
            let ctx = context.iter().find(|c| &c.0 == session_id);
            json!({
                "session_id": session_id,
                "started": started,
                "ended": ended,
                "duration_s": ((ended - started).max(0) as f64) / 1000.0,
                "frames": frames,
                "bytes": held,
                "pageviews": ctx.map(|c| c.1).unwrap_or(0),
                "events": ctx.map(|c| c.2).unwrap_or(0),
                // Engagement time, which is not the recording's wall-clock
                // length above: a tab left open records minutes nobody spent.
                "engaged_s": ctx.map(|c| c.3 as f64 / 1000.0).unwrap_or(0.0),
                "identified_id": ctx.map(|c| c.4.as_str()).unwrap_or(""),
                "device_type": ctx.map(|c| c.5.as_str()).unwrap_or(""),
                "browser": ctx.map(|c| c.6.as_str()).unwrap_or(""),
                "os": ctx.map(|c| c.7.as_str()).unwrap_or(""),
                "country": ctx.map(|c| c.8.as_str()).unwrap_or(""),
                "expires_at": expires_at(*started, st.cfg.replay_retention_days),
                "is_agent": false,
            })
        })
        .collect();

    let meta = json!({ "rows": rows.len(), "total": total });
    Ok(ok_meta(
        json!({
            "rows": rows,
            "total": total,
            "retention_days": st.cfg.replay_retention_days,
        }),
        meta,
    ))
}

/// When a recording made at `started_ms` falls out of the replay budget.
///
/// `null` when replay retention is disabled, because "never" is not a date and
/// printing one would be an invention.
fn expires_at(started_ms: i64, replay_retention_days: u32) -> Value {
    if replay_retention_days == 0 {
        return Value::Null;
    }
    json!(started_ms + i64::from(replay_retention_days) * 86_400_000)
}

/// `GET /api/replay/:session` — the recording itself.
///
/// The response is `{ session, events }`: a summary row for the header, then the
/// rrweb frames in the order they were recorded, each one's `data` inlined as
/// the JSON object it is rather than as a string a client would have to parse
/// again.
///
/// The array is built in memory rather than streamed. Every frame's payload is
/// parsed on the way out, which is what guarantees the response is valid JSON
/// even if a frame was stored malformed, and a parsed frame cannot be handed to
/// a socket without first existing. [`MAX_REPLAY_EVENTS`] is what keeps that
/// bounded; a truncated recording says so in `truncated` rather than quietly
/// ending early.
async fn replay_detail(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Path(session): Path<String>,
) -> Result<Json<Value>, ApiError> {
    same_site(&access, &env)?;
    let session = session.trim().to_string();
    if session.is_empty() {
        return Err(Error::validation("session", "name the recording to play").into());
    }

    // `(timestamp_ms, rrweb type, payload)`. Read by the replay table's own sort
    // key — site, then session — so this touches only that session's granules.
    let frames: Vec<(i64, u8, String)> = fetch_all(
        &st.ch,
        "replay_detail",
        &format!(
            "SELECT toUnixTimestamp64Milli(timestamp) AS t, `type` AS kind, data \
             FROM replay_events \
             WHERE site_id = ? AND session_id = ? \
             ORDER BY timestamp ASC, seq ASC \
             LIMIT {MAX_REPLAY_EVENTS}"
        ),
        &[Param::U16(env.site), Param::Str(session.clone())],
    )
    .await?;

    if frames.is_empty() {
        return Err(Error::not_found("recording").into());
    }

    let started = frames.first().map(|f| f.0).unwrap_or_default();
    let ended = frames.last().map(|f| f.0).unwrap_or(started);
    let held: usize = frames.iter().map(|f| f.2.len()).sum();

    let events: Vec<Value> = frames
        .iter()
        .map(|(t, kind, data)| {
            json!({
                "type": kind,
                "timestamp": t,
                // A frame that will not parse is reported as an empty payload
                // rather than dropped: the player can skip it, and a silently
                // missing frame would look like a gap in what happened.
                "data": serde_json::from_str::<Value>(data).unwrap_or(Value::Null),
            })
        })
        .collect();

    // The session's own events sit in the same hour or two as its frames, which
    // is what makes looking them up cheap. See `REPLAY_CONTEXT_SLACK_HOURS`.
    let slack = Duration::hours(REPLAY_CONTEXT_SLACK_HOURS);
    let from = DateTime::<Utc>::from_timestamp_millis(started).unwrap_or_else(Utc::now) - slack;
    let to = DateTime::<Utc>::from_timestamp_millis(ended).unwrap_or_else(Utc::now) + slack;
    let context = session_context(&st, env.site, from, to, std::slice::from_ref(&session)).await?;
    let ctx = context.first();

    Ok(ok(json!({
        "session": {
            "session_id": session,
            "started": started,
            "ended": ended,
            "duration_s": ((ended - started).max(0) as f64) / 1000.0,
            "frames": frames.len(),
            "bytes": held,
            "pageviews": ctx.map(|c| c.1).unwrap_or(0),
            "events": ctx.map(|c| c.2).unwrap_or(0),
            "identified_id": ctx.map(|c| c.4.as_str()).unwrap_or(""),
            "device_type": ctx.map(|c| c.5.as_str()).unwrap_or(""),
            "browser": ctx.map(|c| c.6.as_str()).unwrap_or(""),
            "os": ctx.map(|c| c.7.as_str()).unwrap_or(""),
            "country": ctx.map(|c| c.8.as_str()).unwrap_or(""),
            "expires_at": expires_at(started, st.cfg.replay_retention_days),
        },
        "events": events,
        "truncated": frames.len() as u64 >= MAX_REPLAY_EVENTS,
    })))
}

/// `DELETE /api/replay/:session`: destroy one recording. Requires `admin`.
///
/// A lightweight delete, so the row stops being returned by the very next read
/// rather than whenever a background mutation finishes. That matters here more
/// than it would elsewhere: somebody pressed this key because a recording should
/// not exist, and a list that still shows it afterwards reads as a refusal.
async fn replay_delete(
    State(st): State<AppState>,
    access: SiteAccess,
    Path(session): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // The same floor as erasing a visitor, for the same reason.
    require_at_least(access.role.as_str(), ADMIN)?;

    let session = session.trim().to_string();
    if session.is_empty() {
        return Err(Error::validation("session", "name the recording to erase").into());
    }

    bind_all(
        st.ch
            .client()
            .query("DELETE FROM replay_events WHERE site_id = ? AND session_id = ?"),
        &[Param::U16(access.site_id), Param::Str(session.clone())],
    )
    .execute()
    .await
    .map_err(|e| Error::Database(format!("replay_delete: {e}")))?;

    tracing::info!(site = %access.site_id, %session, "recording erased");

    Ok(ok(json!({ "erased": true, "session_id": session })))
}

// ---------------------------------------------------------------------------
// Shared ClickHouse reads
// ---------------------------------------------------------------------------

/// `(session_id, pageviews, events, engagement_ms, identified_id, device,
/// browser, os, country)` for a set of sessions.
///
/// A tuple rather than a named row because the driver decodes a row positionally
/// either way, and nine columns with the `SELECT` list two lines above them is
/// clearer here than a struct nobody else constructs.
type SessionContext = (
    String,
    u64,
    u64,
    u64,
    String,
    String,
    String,
    String,
    String,
);

/// Who was on the other end of a set of recordings.
///
/// A recording knows only its own frames, so everything a list shows beside it —
/// the browser, the country, how many pages were seen — comes from the event
/// stream. An empty set of ids skips the read entirely rather than sending a
/// statement that can only match nothing.
async fn session_context(
    st: &AppState,
    site: u16,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    ids: &[String],
) -> Result<Vec<SessionContext>, Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let sql = "SELECT session_id, \
                      toUInt64(countIf(kind = 'pageview')) AS pv, \
                      toUInt64(count()) AS ev, \
                      toUInt64(sum(duration_ms)) AS engaged, \
                      argMax(identified_id, timestamp) AS ident, \
                      any(toString(device_type)) AS dev, \
                      any(toString(browser)) AS brw, \
                      any(toString(os)) AS osn, \
                      any(trimBoth(toString(country))) AS cc \
               FROM events \
               WHERE site_id = ? \
                 AND timestamp >= toDateTime64(?, 3, 'UTC') \
                 AND timestamp < toDateTime64(?, 3, 'UTC') \
                 AND has(?, session_id) \
               GROUP BY session_id";

    // The id list is bound as a ClickHouse array by the driver, after the three
    // scalar parameters `bind_all` writes, which is the order the placeholders
    // appear in.
    let query = bind_all(
        st.ch.client().query(sql),
        &[
            Param::U16(site),
            Param::Str(fmt_ts(from)),
            Param::Str(fmt_ts(to)),
        ],
    )
    .bind(ids);

    query
        .fetch_all::<SessionContext>()
        .await
        .map_err(|e| Error::Database(format!("replay_context: {e}")))
}
