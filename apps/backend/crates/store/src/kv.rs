//! Redis: the state that is allowed to disappear.
//!
//! Everything here is derived, short-lived, or both. A cold Redis costs a gap in
//! the live visitor count, a re-issued session window and a reset rate-limit
//! counter. It never costs a row of history, because nothing in this module is a
//! system of record.
//!
//! The one entry that matters for the privacy claim is [`Kv::daily_salt`]. The
//! visitor hash is `blake3(daily_salt || site_id || ip || user_agent)`, and the
//! reason it cannot be reversed into a person is that the salt is random, unique
//! per site per day, and expires. The TTL here is a hard 48 hours: after that
//! window the salt is gone from the only place it ever existed, and no amount of
//! access to the event store can undo the hash. That is
//! `docs/CONTRACT.md` §7.2, implemented.
//!
//! Every command is issued through `redis::cmd`, not the typed command helpers.
//! The typed helpers change shape between driver releases; `cmd("SETEX")` is the
//! Redis protocol, and the protocol does not. The one exception is
//! [`Kv::swap_json`], a three-line Lua script, because a compare-and-swap is the
//! one thing the plain commands cannot do atomically on a shared connection.

use std::future::Future;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use chrono::{NaiveDate, Utc};
use micaforge_core::{crypto, Error, Result};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use serde::{de::DeserializeOwned, Serialize};

/// How long one attempt to open a connection to Valkey may take.
const CONNECTION_TIMEOUT: Duration = Duration::from_millis(500);

/// How long an open connection may take to answer one command.
///
/// Every command this module sends is O(1) or close to it on a key with a few
/// hundred members, so anything slower than this is a Valkey that is stuck,
/// not one that is busy.
const RESPONSE_TIMEOUT: Duration = Duration::from_millis(300);

/// Reconnect attempts after the first, each time the connection is lost.
///
/// One. The driver waits about a second before a retry, and every caller that
/// arrives meanwhile waits on the same reconnect, so more retries would turn a
/// Valkey outage into requests that each take many seconds. The next command
/// after a failed reconnect starts a fresh one, so a recovered Valkey is picked
/// up on the next request anyway.
const RECONNECT_RETRIES: usize = 1;

/// The longest any single call into this module waits, reconnect included.
///
/// The reconnect itself carries on in the background past this deadline; the
/// caller just stops waiting for it. This is the number the request path can
/// rely on: a missing or hung Valkey costs each command at most half a second
/// and an `Upstream` error, never a hang.
const CALL_DEADLINE: Duration = Duration::from_millis(500);

/// How long a daily visitor salt survives.
///
/// Compare-and-swap for [`Kv::swap_json`]. `KEYS[1]` is the key, `ARGV[1]` the
/// value as it was read, `ARGV[2]` either `set` or `del`, and `ARGV[3]` the
/// value to write for `set`. Answers 1 when it acted, 0 when the key no longer
/// holds what was read (a missing key reads as `false`, which equals nothing).
///
/// `KEEPTTL` (Redis 6.0, every Valkey) is what keeps a rewrite from extending
/// the key's life: a value that was given ten minutes keeps the ten minutes it
/// was given, however often it is updated inside them.
const SWAP_SCRIPT: &str = r"
if redis.call('GET', KEYS[1]) ~= ARGV[1] then return 0 end
if ARGV[2] == 'del' then redis.call('DEL', KEYS[1]) else redis.call('SET', KEYS[1], ARGV[3], 'KEEPTTL') end
return 1
";

/// Two days, not one: a site in UTC+13 is still writing rows against
/// "yesterday's" salt while a site in UTC-11 has already rotated. Anything
/// longer would widen the window in which a salt could correlate a visitor
/// across days, which is the whole thing the rotation exists to prevent.
const SALT_TTL_SECS: u64 = 48 * 60 * 60;

/// Bytes of entropy in a daily salt.
const SALT_BYTES: usize = 32;

/// The realtime view's window, in seconds. `docs/CONTRACT.md` §3.3 defines
/// "realtime" as the last five minutes.
const REALTIME_WINDOW_SECS: i64 = 300;

/// How long a realtime sorted set survives with no writes.
///
/// Twice the window, so a site that goes quiet drops out of Redis on its own
/// rather than accumulating one empty key per site forever.
const REALTIME_TTL_SECS: i64 = REALTIME_WINDOW_SECS * 2;

/// A JSON value as [`Kv::get_snapshot`] read it, kept byte for byte so that
/// [`Kv::swap_json`] can tell whether anybody wrote the key in between.
///
/// The bytes are private. A caller reads `value` and hands the snapshot back,
/// and only this module ever compares the two.
#[derive(Debug, Clone)]
pub struct Snapshot<T> {
    /// What the key held.
    pub value: T,
    /// The exact encoding that was read.
    raw: String,
}

/// A Redis handle.
///
/// [`ConnectionManager`] is a cheap clone over a multiplexed connection that
/// reconnects by itself, so this is cloned per call rather than pooled. There is
/// no pool because there is no need for one: Redis pipelines on a single socket.
#[derive(Clone)]
pub struct Kv {
    conn: ConnectionManager,
}

impl std::fmt::Debug for Kv {
    /// The connection carries the URL, and the URL carries the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kv")
    }
}

/// Open a connection manager for `url` and verify it with a `PING`.
///
/// The manager reconnects on its own afterwards, so this is the only point at
/// which a bad `MICAFORGE_REDIS_URL` is reported, and it happens at boot rather
/// than on the first hit. An unreachable Valkey fails here within a few
/// seconds (two connection attempts of [`CONNECTION_TIMEOUT`] each), with an
/// error that says which variable to check.
pub async fn connect(url: &str) -> Result<Kv> {
    if url.trim().is_empty() {
        return Err(Error::validation(
            "redis_url",
            "MICAFORGE_REDIS_URL is empty",
        ));
    }

    let client = redis::Client::open(url)
        .map_err(|e| Error::validation("redis_url", format!("not a Redis URL: {e}")))?;
    let conn = ConnectionManager::new_with_config(client, manager_config())
        .await
        .map_err(|e| {
            Error::Upstream(format!(
                "redis: could not connect to MICAFORGE_REDIS_URL: {e}"
            ))
        })?;

    let kv = Kv { conn };
    kv.ping().await?;
    Ok(kv)
}

/// The connection manager's timeouts and reconnect policy.
///
/// The driver's defaults have no timeouts at all and six reconnect attempts on
/// a backoff that reaches a minute, so a Valkey outage made every request wait
/// for minutes and a hung Valkey made them wait forever.
fn manager_config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new()
        .set_connection_timeout(CONNECTION_TIMEOUT)
        .set_response_timeout(RESPONSE_TIMEOUT)
        .set_number_of_retries(RECONNECT_RETRIES)
        .set_max_delay(1_000)
}

/// How long calls fail at once after Valkey stopped answering.
///
/// One ingest hit makes several Valkey calls in a row. Without this, each of
/// them waits out [`CALL_DEADLINE`] while Valkey is down, and a beacon takes
/// seconds instead of milliseconds; with it, the first failure pays the
/// deadline and the rest of the window costs nothing. After the window one call
/// tries again, which is also what lets the connection manager reconnect.
const BREAK_FOR: Duration = Duration::from_secs(2);

/// Until when calls fail at once, in milliseconds since the Unix epoch. Zero
/// while Valkey is answering. Process-wide, because there is one Valkey.
static OPEN_UNTIL_MS: AtomicI64 = AtomicI64::new(0);

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Did this failure mean Valkey is unreachable, rather than that one command
/// was wrong? Only the first kind opens the breaker.
fn is_outage(e: &redis::RedisError) -> bool {
    e.is_io_error() || e.is_connection_dropped() || e.is_connection_refusal() || e.is_timeout()
}

/// Run one command under [`CALL_DEADLINE`], unless the breaker is open.
async fn bounded<T>(command: impl Future<Output = redis::RedisResult<T>>) -> Result<T> {
    if now_ms() < OPEN_UNTIL_MS.load(Ordering::Relaxed) {
        return Err(Error::Upstream(
            "redis: unavailable, not retrying for a moment".to_string(),
        ));
    }
    let trip = || OPEN_UNTIL_MS.store(now_ms() + BREAK_FOR.as_millis() as i64, Ordering::Relaxed);
    match tokio::time::timeout(CALL_DEADLINE, command).await {
        Ok(Ok(value)) => {
            OPEN_UNTIL_MS.store(0, Ordering::Relaxed);
            Ok(value)
        }
        Ok(Err(e)) => {
            if is_outage(&e) {
                trip();
            }
            Err(e.into())
        }
        Err(_) => {
            trip();
            Err(Error::Upstream(format!(
                "redis: no answer within {}ms",
                CALL_DEADLINE.as_millis()
            )))
        }
    }
}

impl Kv {
    /// Open a connection manager for `url`. See the module-level [`connect`].
    pub async fn connect(url: &str) -> Result<Kv> {
        connect(url).await
    }

    /// A cheap readiness probe for `GET /api/health/ready`.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: String = bounded(redis::cmd("PING").query_async(&mut conn)).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // The visitor salt
    // -----------------------------------------------------------------------

    /// The current salt for `site_id`, generating one if today has none.
    ///
    /// Keyed by the UTC date. A site whose own timezone rolls over at a
    /// different hour should call [`Kv::daily_salt_for`] with its local date, so
    /// the rotation lands at local midnight as `docs/CONTRACT.md` §7.2 requires;
    /// this is the convenience form for callers with no site context.
    pub async fn daily_salt(&self, site_id: u16) -> Result<Vec<u8>> {
        self.daily_salt_for(site_id, Utc::now().date_naive()).await
    }

    /// The salt for `site_id` on a specific calendar day.
    ///
    /// Generation is a `SET ... NX`, so two ingest workers racing on the first
    /// hit of the day cannot end up hashing the same visitor with two different
    /// salts: the loser reads the winner's value back rather than keeping its
    /// own.
    pub async fn daily_salt_for(&self, site_id: u16, day: NaiveDate) -> Result<Vec<u8>> {
        let key = format!("mf:salt:{site_id}:{day}");
        let mut conn = self.conn.clone();

        let existing: Option<Vec<u8>> =
            bounded(redis::cmd("GET").arg(&key).query_async(&mut conn)).await?;
        if let Some(salt) = existing {
            if !salt.is_empty() {
                return Ok(salt);
            }
        }

        let salt = crypto::random_token(SALT_BYTES).into_bytes();
        let claimed: Option<String> = bounded(
            redis::cmd("SET")
                .arg(&key)
                .arg(&salt)
                .arg("NX")
                .arg("EX")
                .arg(SALT_TTL_SECS)
                .query_async(&mut conn),
        )
        .await?;

        if claimed.is_some() {
            return Ok(salt);
        }

        // Someone else won the race. Their salt is the one already in use.
        let winner: Option<Vec<u8>> =
            bounded(redis::cmd("GET").arg(&key).query_async(&mut conn)).await?;
        winner.ok_or_else(|| Error::Upstream("redis: salt vanished between SET and GET".into()))
    }

    // -----------------------------------------------------------------------
    // The realtime window
    // -----------------------------------------------------------------------

    /// Record that `visitor` was seen just now.
    ///
    /// A sorted set scored by timestamp, one key per site per audience. Scoring
    /// by time is what makes the window sliding rather than a bucket that empties
    /// on the minute: re-adding an existing member updates its score, so a
    /// visitor stays live exactly five minutes past their last hit.
    ///
    /// Humans and agents are counted in separate keys because they are separate
    /// answers. Folding agents into the live visitor number is precisely the
    /// mistake this product exists to correct.
    pub async fn bump_realtime(&self, site: u16, visitor: &str, is_agent: bool) -> Result<()> {
        let key = realtime_key(site, is_agent);
        let now = Utc::now().timestamp();
        let mut conn = self.conn.clone();

        bounded(
            redis::pipe()
                .atomic()
                .cmd("ZADD")
                .arg(&key)
                .arg(now)
                .arg(visitor)
                .ignore()
                .cmd("ZREMRANGEBYSCORE")
                .arg(&key)
                .arg("-inf")
                .arg(now - REALTIME_WINDOW_SECS)
                .ignore()
                .cmd("EXPIRE")
                .arg(&key)
                .arg(REALTIME_TTL_SECS)
                .ignore()
                .query_async::<()>(&mut conn),
        )
        .await?;

        Ok(())
    }

    /// Live humans and live agents in the last five minutes.
    ///
    /// Trims both windows before counting, so a site that stopped receiving
    /// traffic reports zero rather than whatever was last written.
    pub async fn realtime_count(&self, site: u16) -> Result<(u64, u64)> {
        let humans = realtime_key(site, false);
        let agents = realtime_key(site, true);
        let cutoff = Utc::now().timestamp() - REALTIME_WINDOW_SECS;
        let mut conn = self.conn.clone();

        let (human_count, agent_count): (u64, u64) = bounded(
            redis::pipe()
                .atomic()
                .cmd("ZREMRANGEBYSCORE")
                .arg(&humans)
                .arg("-inf")
                .arg(cutoff)
                .ignore()
                .cmd("ZREMRANGEBYSCORE")
                .arg(&agents)
                .arg("-inf")
                .arg(cutoff)
                .ignore()
                .cmd("ZCARD")
                .arg(&humans)
                .cmd("ZCARD")
                .arg(&agents)
                .query_async(&mut conn),
        )
        .await?;

        Ok((human_count, agent_count))
    }

    // -----------------------------------------------------------------------
    // Rate limiting
    // -----------------------------------------------------------------------

    /// Consume one unit against `key`. `true` means the caller may proceed.
    ///
    /// A fixed window, not a token bucket, and the window index is baked into
    /// the Redis key rather than maintained with `EXPIRE`. That detail is the
    /// whole design: because each window has its own key, the counter can be
    /// incremented and given a TTL without any risk of a long-running client
    /// pushing the expiry forward forever and turning the limit into a
    /// permanent lockout.
    ///
    /// The pipeline is atomic, so `INCR` and `EXPIRE` cannot be separated by a
    /// crash and leave a key with no TTL.
    pub async fn rate_limit(&self, key: &str, limit: u32, window_secs: u32) -> Result<bool> {
        if limit == 0 {
            return Ok(false);
        }
        let window = window_secs.max(1);
        let index = Utc::now().timestamp() / i64::from(window);
        let redis_key = format!("mf:rl:{key}:{index}");
        let mut conn = self.conn.clone();

        let (count,): (u64,) = bounded(
            redis::pipe()
                .atomic()
                .cmd("INCR")
                .arg(&redis_key)
                // One second past the window, so the key outlives the last request
                // it counts and is gone before the same index can come round again.
                .cmd("EXPIRE")
                .arg(&redis_key)
                .arg(window + 1)
                .ignore()
                .query_async(&mut conn),
        )
        .await?;

        Ok(count <= u64::from(limit))
    }

    // -----------------------------------------------------------------------
    // The session window
    // -----------------------------------------------------------------------

    /// The session a visitor is currently inside, if any.
    pub async fn session_for(&self, site: u16, visitor: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let session: Option<String> = bounded(
            redis::cmd("GET")
                .arg(session_key(site, visitor))
                .query_async(&mut conn),
        )
        .await?;
        Ok(session)
    }

    /// Open or extend a visitor's session window.
    ///
    /// `ttl` is the inactivity gap that ends a session, thirty minutes in this
    /// product. Writing the key on every hit is what makes it a gap rather than
    /// a fixed session length: the expiry moves forward while the visitor is
    /// active, and the session ends when they stop.
    pub async fn set_session(
        &self,
        site: u16,
        visitor: &str,
        session: &str,
        ttl: u32,
    ) -> Result<()> {
        let mut conn = self.conn.clone();
        bounded(
            redis::cmd("SET")
                .arg(session_key(site, visitor))
                .arg(session)
                .arg("EX")
                .arg(ttl.max(1))
                .query_async::<()>(&mut conn),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Generic cache
    // -----------------------------------------------------------------------

    /// Read a JSON-encoded value.
    ///
    /// A value that fails to parse is reported as absent rather than as an
    /// error. Everything cached here is derived and re-computable, so a stale
    /// encoding from an older release should cost one recomputation, not a
    /// failed request.
    pub async fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let mut conn = self.conn.clone();
        let raw: Option<String> =
            bounded(redis::cmd("GET").arg(key).query_async(&mut conn)).await?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        match serde_json::from_str(&raw) {
            Ok(value) => Ok(Some(value)),
            Err(e) => {
                tracing::warn!(key, error = %e, "discarding unparseable cache entry");
                Ok(None)
            }
        }
    }

    /// Write a JSON-encoded value, optionally with a TTL in seconds.
    ///
    /// `None` stores it without expiry, which is only correct for values that
    /// are explicitly invalidated. Prefer a TTL.
    pub async fn set_json<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl_secs: Option<u32>,
    ) -> Result<()> {
        let payload = serde_json::to_string(value)?;
        let mut conn = self.conn.clone();
        let mut cmd = redis::cmd("SET");
        cmd.arg(key).arg(payload);
        if let Some(ttl) = ttl_secs {
            cmd.arg("EX").arg(ttl.max(1));
        }
        bounded(cmd.query_async::<()>(&mut conn)).await?;
        Ok(())
    }

    /// Delete a key. Deleting a key that is not there is not an error.
    pub async fn del(&self, key: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        bounded(redis::cmd("DEL").arg(key).query_async::<()>(&mut conn)).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Compare-and-swap
    // -----------------------------------------------------------------------

    /// Read a JSON value for a later [`Kv::swap_json`].
    ///
    /// Unlike [`Kv::get_json`], this is for state that is updated in place by
    /// requests that may race each other, such as a counter of wrong guesses
    /// inside a value. An unparseable value reads as absent, as it does there.
    pub async fn get_snapshot<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<Snapshot<T>>> {
        let mut conn = self.conn.clone();
        let raw: Option<String> =
            bounded(redis::cmd("GET").arg(key).query_async(&mut conn)).await?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        match serde_json::from_str(&raw) {
            Ok(value) => Ok(Some(Snapshot { value, raw })),
            Err(e) => {
                tracing::warn!(key, error = %e, "discarding unparseable snapshot entry");
                Ok(None)
            }
        }
    }

    /// Replace a value with `next`, or delete it when `next` is `None`, but
    /// only if the key still holds exactly what `seen` read. `true` means this
    /// call acted.
    ///
    /// Atomic on the server, so of two requests that read the same value and
    /// both try to move it on, exactly one succeeds and the other is told so.
    /// That is the whole point: a read-modify-write with `GET` and `SET` would
    /// let the slower request write back a value the faster one had already
    /// replaced, and a single-use value deleted with `DEL` would be used by
    /// every request that read it before the first `DEL` landed. `WATCH` is not
    /// an option either, because the connection is multiplexed and shared.
    ///
    /// The key's remaining time to live is kept as it is (see [`SWAP_SCRIPT`]).
    pub async fn swap_json<T: Serialize>(
        &self,
        key: &str,
        seen: &Snapshot<T>,
        next: Option<&T>,
    ) -> Result<bool> {
        let (op, payload) = match next {
            Some(value) => ("set", serde_json::to_string(value)?),
            None => ("del", String::new()),
        };
        let script = redis::Script::new(SWAP_SCRIPT);
        let mut invocation = script.key(key);
        invocation.arg(&seen.raw).arg(op).arg(payload);
        let mut conn = self.conn.clone();
        let acted: i64 = bounded(invocation.invoke_async(&mut conn)).await?;
        Ok(acted == 1)
    }
}

/// `mf:rt:h:<site>` for humans, `mf:rt:a:<site>` for agents.
fn realtime_key(site: u16, is_agent: bool) -> String {
    let audience = if is_agent { 'a' } else { 'h' };
    format!("mf:rt:{audience}:{site}")
}

/// `mf:sess:<site>:<visitor>`.
fn session_key(site: u16, visitor: &str) -> String {
    format!("mf:sess:{site}:{visitor}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_keys_separate_the_two_audiences() {
        // Folding agents into the human count is the exact mistake this product
        // exists to correct, so the two must never share a key.
        assert_ne!(realtime_key(1, false), realtime_key(1, true));
        assert_eq!(realtime_key(7, false), "mf:rt:h:7");
        assert_eq!(realtime_key(7, true), "mf:rt:a:7");
    }

    #[test]
    fn the_salt_ttl_is_short_enough_to_be_a_privacy_claim() {
        // §7.2: the salt is "never stored beyond 48 h". A longer TTL would let
        // one salt correlate a visitor across more than one rotation.
        const { assert!(SALT_TTL_SECS <= 48 * 60 * 60) };
    }

    #[test]
    fn the_realtime_window_is_the_five_minutes_the_contract_promises() {
        assert_eq!(REALTIME_WINDOW_SECS, 300);
        const { assert!(REALTIME_TTL_SECS > REALTIME_WINDOW_SECS) };
    }

    #[test]
    fn session_keys_are_scoped_per_site() {
        assert_ne!(session_key(1, "abc"), session_key(2, "abc"));
    }

    #[test]
    fn every_valkey_wait_is_short() {
        // The request path calls this module several times per hit. Each call
        // has to fail fast when Valkey is gone, or an outage becomes a stall.
        const { assert!(CALL_DEADLINE.as_millis() <= 1_000) };
        const { assert!(RESPONSE_TIMEOUT.as_millis() < CALL_DEADLINE.as_millis()) };
        const { assert!(CONNECTION_TIMEOUT.as_millis() <= CALL_DEADLINE.as_millis()) };
        const { assert!(RECONNECT_RETRIES <= 2) };
    }

    /// Held by every test that goes through [`bounded`], whose breaker is
    /// process-wide state.
    static BREAKER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn a_hung_command_becomes_an_error_at_the_deadline() {
        let _serial = BREAKER.lock().await;
        OPEN_UNTIL_MS.store(0, Ordering::Relaxed);
        let started = std::time::Instant::now();
        let result: Result<()> = bounded(std::future::pending()).await;
        assert!(result.is_err());
        assert!(started.elapsed() < CALL_DEADLINE * 4);
    }

    #[tokio::test]
    async fn an_outage_opens_the_breaker_and_a_success_closes_it() {
        let _serial = BREAKER.lock().await;
        OPEN_UNTIL_MS.store(0, Ordering::Relaxed);

        // A command that outlives the deadline opens the breaker...
        let slow = async {
            tokio::time::sleep(CALL_DEADLINE * 2).await;
            Ok::<u8, redis::RedisError>(1)
        };
        assert!(bounded(slow).await.is_err());

        // ...so the next call fails at once, without being run.
        let started = std::time::Instant::now();
        let refused = bounded(async { Ok::<u8, redis::RedisError>(1) }).await;
        assert!(refused.is_err(), "an open breaker refuses");
        assert!(started.elapsed() < Duration::from_millis(50));

        // Once the window has passed, a call runs, and its success closes it.
        OPEN_UNTIL_MS.store(now_ms() - 1, Ordering::Relaxed);
        assert_eq!(
            bounded(async { Ok::<u8, redis::RedisError>(1) })
                .await
                .unwrap(),
            1
        );
        assert_eq!(OPEN_UNTIL_MS.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_wrong_command_does_not_open_the_breaker() {
        let wrong = redis::RedisError::from((redis::ErrorKind::TypeError, "WRONGTYPE"));
        assert!(!is_outage(&wrong));
        let refused =
            redis::RedisError::from(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(is_outage(&refused));
    }

    /// Against a real Valkey, because the property under test is the server's
    /// atomicity, which nothing in this process can stand in for. Opt in with
    /// `MICAFORGE_TEST_REDIS_URL=redis://127.0.0.1:6379` and
    /// `cargo test -p micaforge-store -- --ignored`. It writes one key, under
    /// `mf:test:`, and deletes it.
    #[tokio::test]
    #[ignore = "needs a Valkey or Redis server"]
    async fn a_swap_acts_once_on_what_was_read_and_keeps_the_ttl() {
        let _serial = BREAKER.lock().await;
        OPEN_UNTIL_MS.store(0, Ordering::Relaxed);
        let url = std::env::var("MICAFORGE_TEST_REDIS_URL")
            .expect("set MICAFORGE_TEST_REDIS_URL to run this test");
        let kv = connect(&url).await.expect("a live server");
        let key = format!("mf:test:swap:{}", crypto::random_token(9));

        kv.set_json(&key, &1u32, Some(120)).await.unwrap();
        let first: Snapshot<u32> = kv.get_snapshot(&key).await.unwrap().unwrap();
        let racer: Snapshot<u32> = kv.get_snapshot(&key).await.unwrap().unwrap();
        assert_eq!(first.value, 1);

        // Two requests read the same value; only the first to swap wins.
        assert!(kv.swap_json(&key, &first, Some(&2)).await.unwrap());
        assert!(!kv.swap_json(&key, &racer, Some(&9)).await.unwrap());
        assert_eq!(kv.get_json::<u32>(&key).await.unwrap(), Some(2));

        // The rewrite kept the time to live it was given, not a fresh one,
        // and did not drop it.
        let mut conn = kv.conn.clone();
        let ttl: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!((1..=120).contains(&ttl), "ttl {ttl}");

        // A delete is single-use in the same way.
        let current: Snapshot<u32> = kv.get_snapshot(&key).await.unwrap().unwrap();
        let late = current.clone();
        assert!(kv.swap_json(&key, &current, None).await.unwrap());
        assert!(!kv.swap_json(&key, &late, None).await.unwrap());
        assert_eq!(kv.get_json::<u32>(&key).await.unwrap(), None);

        // A key that was rewritten by somebody else is not swapped.
        kv.set_json(&key, &5u32, Some(120)).await.unwrap();
        assert!(!kv.swap_json(&key, &late, Some(&6)).await.unwrap());
        kv.del(&key).await.unwrap();
    }

    #[tokio::test]
    async fn an_unreachable_valkey_fails_boot_quickly_and_names_the_variable() {
        // Port 1 refuses at once; the bound under test is the reconnect policy.
        let started = std::time::Instant::now();
        let err = connect("redis://127.0.0.1:1").await.unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(err.to_string().contains("MICAFORGE_REDIS_URL"), "{err}");
    }
}
