//! `GET /api/health/*` and `GET /api/config`: what the operator, the
//! orchestrator and the dashboard need before anything else works.
//!
//! **The health routes are read by machines, and by two kinds of them.**
//!
//! - `GET /api/health/live` answers "is this process running": always `200`
//!   while the runtime can answer a request, and it touches no store. It is
//!   the one a container healthcheck or a liveness probe should use. A
//!   liveness check that failed whenever ClickHouse restarted would get every
//!   replica restarted at once, and a restart throws away exactly the rows the
//!   event writer is holding until ClickHouse returns.
//! - `GET /api/health/ready` answers "should traffic be sent here": the store
//!   probes, `503` when Postgres is down. ClickHouse or Valkey being down is
//!   reported as `degraded` with a `200`, because collection keeps working
//!   through both: the writer buffers every hit until ClickHouse returns, the
//!   ingest path salts with an in-memory salt while Valkey is away, and a
//!   load balancer that pulled every replica out of rotation for a ClickHouse
//!   restart would turn that buffer into dropped beacons. The answer also
//!   carries the writer's queue depth and drop counts, the job queue's depth
//!   and the number of live worker lanes, since there is no metrics endpoint.
//! - `GET /api/health` is the readiness answer under its original name, for
//!   monitors configured before the split.
//!
//! Docker Compose does not restart a container whose healthcheck fails; it
//! marks it `unhealthy`, which gates `depends_on: condition: service_healthy`
//! and is what an operator sees in `docker ps`. Restarting on that state takes
//! an orchestrator or an autoheal sidecar, which is one more reason the
//! container check points at `live`.
//!
//! The probes are cached for [`READY_CACHE`] and run one set at a time, so a
//! caller hammering the route costs the stores one round of probes every two
//! seconds, not one per request. Each probe has its own deadline and they run
//! in parallel, so a hung Valkey cannot make the route itself hang.
//!
//! `live` and `ready` are mounted by `main` outside the rate limiter (see
//! [`probes`]): the limiter's counter lives in Valkey, and a liveness answer
//! must not depend on a store.
//!
//! **`/api/config` is read by the dashboard, before login.** It answers the
//! questions the entry screens have to settle before they can render anything
//! honest: may someone sign up here, is this a self-hosted install, what version
//! is it, how long is data kept. It is deliberately dull — nothing here is a
//! secret, and nothing here depends on who is asking, which is why it needs no
//! authentication and can be cached briefly at the edge.

use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value as Json};
use tokio::sync::Mutex;
use tokio::time::timeout;

use micaforge_store::models::{jobs, users};

use crate::state::AppState;

/// The version this binary was built as.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long a single dependency probe may take before it counts as failed.
///
/// Two seconds is well inside the healthcheck's own timeout, which is what
/// makes the difference between "this process reports Redis is down" and "this
/// process stopped answering", and only the first of those is useful.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long one round of probes is reused.
const READY_CACHE: Duration = Duration::from_secs(2);

/// The last round of probes, and when it ran.
///
/// A `tokio` mutex held across the probe on purpose: callers that arrive while
/// a round is running wait for it and reuse its answer instead of starting
/// their own.
static LAST_ROUND: Mutex<Option<Round>> = Mutex::const_new(None);

/// The routes `main` mounts with the rest of the API: the original health
/// name and the dashboard's boot configuration.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/health", get(ready))
        .route("/api/config", get(config))
}

/// The liveness and readiness routes.
///
/// Separate from [`router`] because `main` mounts these outside the rate
/// limiter, whose counter lives in Valkey: `live` must answer without touching
/// a store, and `ready` is already cached and bounded on its own.
pub fn probes() -> Router<AppState> {
    Router::new()
        .route("/api/health/live", get(live))
        .route("/api/health/ready", get(ready))
}

/// `GET /api/health/live`: the process is up and its runtime is answering.
///
/// No store is consulted. If this does not answer, the process is wedged and
/// restarting it is the right call; if it does, a restart would not fix
/// whatever else is wrong and would lose the writer's buffer.
async fn live() -> Response {
    no_store(crate::response::ok(json!({ "status": "ok", "version": VERSION })).into_response())
}

/// `GET /api/health/ready` (and `GET /api/health`): per-dependency status,
/// the background queues, and the build version.
///
/// `503` when Postgres is down: it holds identity and site configuration, and
/// the dashboard and every management route need it. ClickHouse or Valkey down
/// is `degraded` with a `200`, for the reason in the module docs; the `writer`
/// block shows how much is waiting for ClickHouse and whether any of it has had
/// to be dropped.
async fn ready(State(state): State<AppState>) -> Response {
    let round = current_round(&state).await;

    let lanes = state.writer.stats();
    let queued_rows: u64 = lanes.iter().map(|l| l.queued_rows).sum();
    let queued_bytes: u64 = lanes.iter().map(|l| l.queued_bytes).sum();
    let dropped_rows: u64 = lanes.iter().map(|l| l.dropped_rows).sum();

    let status = if !round.ready {
        "down"
    } else if round.degraded {
        "degraded"
    } else {
        "ok"
    };

    let body = json!({
        "status": status,
        "version": VERSION,
        "checks": round.checks,
        "writer": {
            "queued_rows": queued_rows,
            "queued_bytes": queued_bytes,
            "dropped_rows": dropped_rows,
            "lanes": lanes,
        },
        "jobs": {
            "due": round.jobs_due,
            "lanes_live": micaforge_jobs::worker::live_lanes(),
            "lanes_configured": micaforge_jobs::worker::configured_lanes(),
        },
    });

    let mut res = crate::response::ok(body).into_response();
    if !round.ready {
        *res.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    }
    no_store(res)
}

/// A cached healthcheck is not a healthcheck, at any proxy in between.
fn no_store(mut res: Response) -> Response {
    if let Ok(value) = "no-store".parse() {
        res.headers_mut().insert("cache-control", value);
    }
    res
}

/// One round of store probes.
#[derive(Clone)]
struct Round {
    at: Instant,
    /// Postgres answered.
    ready: bool,
    /// ClickHouse or Valkey did not answer.
    degraded: bool,
    checks: Json,
    /// Due jobs waiting for a lane, when Postgres could say.
    jobs_due: Option<i64>,
}

/// The last round if it is fresh, otherwise a new one.
async fn current_round(state: &AppState) -> Round {
    let mut last = LAST_ROUND.lock().await;
    if let Some(round) = last.as_ref() {
        if round.at.elapsed() < READY_CACHE {
            return round.clone();
        }
    }

    let (postgres, clickhouse, redis, jobs_due) = tokio::join!(
        probe(
            "postgres",
            PROBE_TIMEOUT,
            micaforge_store::pg::ping(&state.pg)
        ),
        probe("clickhouse", PROBE_TIMEOUT, state.ch.ping()),
        probe("redis", PROBE_TIMEOUT, state.kv.ping()),
        timeout(PROBE_TIMEOUT, jobs::due_count(&state.pg)),
    );

    let round = Round {
        at: Instant::now(),
        // Collection carries on without ClickHouse (the writer buffers) and
        // without Valkey (in-memory salt, limiters that let requests through),
        // so neither takes a replica out of a load balancer. Postgres does: the
        // dashboard and every management route need it.
        ready: postgres.ok,
        degraded: !clickhouse.ok || !redis.ok,
        checks: json!({
            "postgres": postgres.as_json(),
            "clickhouse": clickhouse.as_json(),
            "redis": redis.as_json(),
        }),
        jobs_due: jobs_due.ok().and_then(|count| count.ok()),
    };
    *last = Some(round.clone());
    round
}

/// One dependency's answer.
struct Check {
    ok: bool,
    latency_ms: u64,
    /// The failure, flattened to one sentence. Store errors carry driver text
    /// (hosts, and occasionally a password inside a connection string), so what
    /// reaches the body is the error's public form and the detail goes to the
    /// log.
    error: Option<String>,
}

impl Check {
    fn as_json(&self) -> Json {
        json!({
            "status": if self.ok { "ok" } else { "down" },
            "latency_ms": self.latency_ms,
            "error": self.error,
        })
    }
}

/// Run one probe under a deadline and time it.
///
/// The deadline is a parameter rather than the constant so a test can prove the
/// timeout path without waiting three real seconds for it.
async fn probe(
    name: &'static str,
    deadline: Duration,
    future: impl std::future::Future<Output = micaforge_core::Result<()>>,
) -> Check {
    let started = Instant::now();
    let outcome = timeout(deadline, future).await;
    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;

    match outcome {
        Ok(Ok(())) => Check {
            ok: true,
            latency_ms,
            error: None,
        },
        Ok(Err(e)) => {
            tracing::warn!(dependency = name, error = %e, "health probe failed");
            Check {
                ok: false,
                latency_ms,
                // `is_public` is the store crates' own judgement about whether a
                // message is safe to hand out; a database error never is.
                error: Some(if e.is_public() {
                    e.to_string()
                } else {
                    "the dependency reported an error".to_string()
                }),
            }
        }
        Err(_) => {
            tracing::warn!(dependency = name, "health probe timed out");
            Check {
                ok: false,
                latency_ms,
                error: Some(format!("no answer within {}ms", deadline.as_millis())),
            }
        }
    }
}

/// `GET /api/config` — what the dashboard needs to boot.
///
/// Unauthenticated on purpose: the sign-up screen has to know whether sign-up is
/// open *before* anyone has an account, and a screen that guesses would either
/// offer a form the server will refuse or hide one it would have accepted.
///
/// `registration_open` deserves its rule stated plainly. It is true when the
/// operator has not closed registration, **or** when the install has no users at
/// all. A fresh self-hosted install that set `MICAFORGE_DISABLE_REGISTRATION`
/// before its owner had signed up would otherwise be permanently unenterable,
/// with no way in short of editing the environment and restarting — a trap that
/// costs a self-hoster their first evening.
async fn config(State(state): State<AppState>) -> Response {
    let cfg = &state.cfg;

    // A failed count is treated as "there are users": guessing the other way
    // would open registration on a live install every time Postgres hiccuped.
    let user_count = users::count(&state.pg).await.unwrap_or(1);
    let no_users_yet = user_count == 0;

    let body = json!({
        "version": VERSION,
        // There is one build. `PRODUCT.md` principle 4: no feature gate, no
        // community-edition asterisk, so this is a constant and not a licence
        // check pretending to be one.
        "edition": "agpl-3.0",
        "self_hosted": cfg.self_hosted,
        "registration_open": !cfg.disable_registration || no_users_yet,
        "setup_required": no_users_yet,
        "mail_enabled": cfg.mail_enabled(),
        // `0` means forever, per `docs/CONTRACT.md` §1.9. The settings screen
        // renders the two cases differently, so the raw number travels.
        "retention_days": cfg.retention_days,
        "replay_retention_days": cfg.replay_retention_days,
        "public_url": cfg.public_url,
        "dashboard_url": cfg.dashboard_url,
        // Only the providers whose client id and secret are set, so the
        // sign-in screen offers a button only when it works.
        "oauth_providers": cfg.oauth_providers(),
        "billing": {
            "enabled": cfg.billing_enabled,
            "stripe": cfg.stripe_keys().is_some(),
        },
    });

    let mut res = crate::response::ok(body).into_response();
    // Short enough that flipping registration takes effect while the operator is
    // still looking at the screen, long enough to absorb a dashboard reload.
    if let Ok(value) = "public, max-age=60".parse() {
        res.headers_mut().insert("cache-control", value);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use micaforge_core::Error;

    /// Long enough that a probe which resolves immediately never races it.
    const QUICK: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_healthy_probe_reports_ok_with_no_error() {
        let check = probe("test", QUICK, async { Ok(()) }).await;
        assert!(check.ok);
        assert_eq!(check.as_json()["status"], "ok");
        assert!(check.error.is_none());
    }

    #[tokio::test]
    async fn a_store_error_never_reaches_the_body() {
        // A connection string in a driver message is exactly the thing that must
        // not be served from an unauthenticated route.
        let leak = "host=db user=micaforge password=hunter2";
        let check = probe(
            "test",
            QUICK,
            async move { Err(Error::Database(leak.into())) },
        )
        .await;

        assert!(!check.ok);
        let rendered = check.as_json().to_string();
        assert!(
            !rendered.contains("hunter2"),
            "the driver detail must be withheld"
        );
        assert!(!rendered.contains("password"));
        assert_eq!(check.as_json()["status"], "down");
    }

    #[tokio::test]
    async fn a_public_error_is_reported_verbatim() {
        let check = probe("test", QUICK, async { Err(Error::not_found("schema")) }).await;
        assert!(check.error.unwrap().contains("schema"));
    }

    #[tokio::test]
    async fn a_hung_dependency_fails_rather_than_hanging_the_check() {
        // The whole point of the deadline: a store that never answers must not
        // become a healthcheck that never answers.
        let check = probe("test", Duration::from_millis(10), async {
            std::future::pending::<micaforge_core::Result<()>>().await
        })
        .await;

        assert!(!check.ok);
        assert!(check.error.unwrap().contains("no answer"));
    }

    #[test]
    fn the_deadline_leaves_room_inside_the_container_healthcheck() {
        // A checker typically allows five seconds. The probes run in parallel,
        // so the worst case is one deadline plus overhead, and it has to land
        // inside that budget or a degraded install reads as a dead one.
        assert!(PROBE_TIMEOUT < Duration::from_secs(5));
        // The cache must be short enough that a recovered store shows up while
        // someone is still looking.
        assert!(READY_CACHE <= Duration::from_secs(5));
    }

    #[test]
    fn the_probe_routes_do_not_collide_with_the_others() {
        // `main` merges these beside `router()`; a shared path would be a
        // panic at boot.
        let _: Router<AppState> = router().merge(probes());
    }

    #[tokio::test]
    async fn liveness_answers_without_any_state() {
        // The handler takes no state at all, which is the guarantee that it
        // cannot touch a store.
        let res = live().await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("cache-control").map(|v| v.as_bytes()),
            Some(&b"no-store"[..])
        );
    }
}
