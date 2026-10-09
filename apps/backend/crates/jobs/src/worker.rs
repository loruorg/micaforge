//! The worker loop.
//!
//! A lane claims the oldest due job nobody else holds, runs it under a per-kind
//! timeout, and marks it done or failed. `concurrency` lanes do that at once, and
//! a janitor task returns abandoned rows to the queue and sweeps finished ones
//! away. That is the whole of it; everything interesting is in the jobs.
//!
//! # Why the loop looks like this
//!
//! **Claiming is one statement.** `UPDATE ... WHERE id = (SELECT ... FOR UPDATE
//! SKIP LOCKED LIMIT 1)` in [`micaforge_store::models::jobs::claim_next`] is what
//! makes several lanes, and several replicas of the whole binary, safe against
//! each other with no coordination. Two lanes polling in the same millisecond
//! lock different rows rather than blocking on each other.
//!
//! **Every job has a ceiling.** A fetch against a server that accepts the
//! connection and then says nothing would otherwise hold a lane until the process
//! restarts. [`crate::Kind::timeout`] bounds each kind by what that kind actually
//! does.
//!
//! **A failure is never silent.** It is logged with the attempt count and the
//! retry time, retried on the [`crate::schedule::retry_delay`] curve, and after
//! [`crate::schedule::MAX_ATTEMPTS`] parked as `failed` and left in the table.
//! Deleting work that could not be done would make the queue lie about what
//! happened.
//!
//! **A recurring job reschedules itself even when it fails.** A site that was
//! down all night must still be crawled tomorrow. See
//! [`crate::schedule::enqueue_next`].
//!
//! **A running job proves it is alive.** Its lane moves `jobs.locked_at`
//! forward every [`HEARTBEAT_EVERY`], and the janitor reclaims a row whose
//! heartbeat is older than [`STALE_AFTER`]. A process killed mid-job therefore
//! costs its jobs about two minutes, not the length of the longest timeout.
//!
//! **A panic is a failed job, not a dead lane.** The job body runs under
//! [`CatchUnwind`], and every lane runs under a supervisor that restarts it if
//! it ever exits while the worker is still meant to be running.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use micaforge_core::Result;
use micaforge_store::models::jobs::{self, Job};
use serde_json::Value as Json;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::schedule::{self, JobRef};
use crate::{JobCtx, Kind};

/// How long an idle lane waits before asking for work again.
///
/// One second. The queue's most urgent tenant is the alert sweep, which runs
/// every five minutes, so this is already an order of magnitude tighter than
/// anything needs; going lower would only add Postgres round trips to an
/// otherwise idle install.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long a lane waits after the queue itself failed.
///
/// Longer than the poll, because the failure is almost always "Postgres is
/// restarting" and hammering it while it comes back helps nobody.
const ERROR_BACKOFF: Duration = Duration::from_secs(5);

/// How often the janitor runs.
///
/// Every thirty seconds, so a row abandoned by a killed process is back in the
/// queue within [`STALE_AFTER`] plus this.
const JANITOR_INTERVAL: Duration = Duration::from_secs(30);

/// How often a lane renews the lock on the job it is running.
const HEARTBEAT_EVERY: Duration = Duration::from_secs(30);

/// How old a `running` row's heartbeat may be before it is presumed abandoned.
///
/// Four heartbeats. A live lane renews every [`HEARTBEAT_EVERY`] however long
/// its job runs, so only a lane that has stopped (a killed process, a crashed
/// task) misses four in a row; reclaiming a job that is merely slow would run
/// it twice, concurrently, which is the one thing `SKIP LOCKED` exists to
/// prevent.
const STALE_AFTER: Duration = Duration::from_secs(2 * 60);

/// How long a supervisor waits before restarting a lane that exited.
///
/// Long enough that a lane which dies on every start cannot spin a core.
const RESTART_DELAY: Duration = Duration::from_secs(1);

/// Lanes currently inside their loop, across the process.
static LIVE_LANES: AtomicUsize = AtomicUsize::new(0);

/// Lanes the worker was started with.
static CONFIGURED_LANES: AtomicUsize = AtomicUsize::new(0);

/// How many worker lanes are running right now, for the readiness route.
pub fn live_lanes() -> usize {
    LIVE_LANES.load(Ordering::Relaxed)
}

/// How many worker lanes this process was configured to run.
pub fn configured_lanes() -> usize {
    CONFIGURED_LANES.load(Ordering::Relaxed)
}

/// How often the janitor looks for import scratch files nobody will read.
///
/// Hourly, and once at boot. The files are measured in gigabytes and are rare,
/// so listing one directory an hour is the whole cost.
const ORPHAN_SWEEP_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// How long a finished job row is kept.
///
/// A week is enough for an operator to answer "did last Monday's report go out?"
/// and short enough that the table stays small on an install that has been
/// running for years.
const KEEP_FINISHED_DAYS: i64 = 7;

/// The default bound on how long [`Worker::shutdown`] waits for in-flight jobs.
///
/// The drain is bounded because an import may legitimately be an hour into a
/// two-hour run, and a deploy cannot wait for it. What happens then is not a
/// loss: the row stays `running`, its heartbeat stops with the process, and a
/// janitor returns it to the queue within a few minutes, where the importer
/// resumes from what it already staged.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

/// A running pool of worker lanes.
///
/// Held by whoever spawned it, usually `main`, so that it can be shut down.
/// Dropping it without calling [`Worker::shutdown`] closes the stop channel,
/// which the lanes read as "stop after the current job" and act on without
/// anybody waiting for them. Prefer [`Worker::shutdown`], which waits.
pub struct Worker {
    /// Set to `true` to ask every lane to stop after its current job.
    stop: watch::Sender<bool>,
    /// One handle per lane, plus the janitor.
    handles: Vec<JoinHandle<()>>,
    /// This process's worker identity, written into `jobs.locked_by`.
    id: String,
}

impl Worker {
    /// Start `concurrency` lanes and the janitor.
    ///
    /// Must be called from inside a Tokio runtime. `concurrency` is clamped to at
    /// least one lane: a worker configured with zero would look healthy and do
    /// nothing, which is the worst failure mode available.
    ///
    /// Every lane handles every kind. Dedicated lanes per kind were considered
    /// and rejected: the queue's whole load is hundreds of jobs a day, and a lane
    /// reserved for imports would sit idle for weeks while a lane reserved for
    /// crawls fell behind.
    pub fn spawn(ctx: JobCtx, concurrency: usize) -> Worker {
        let lanes = concurrency.max(1);
        let ctx = Arc::new(ctx);
        let id = worker_id();
        let (stop, _) = watch::channel(false);

        let kinds: Vec<String> = Kind::ALL.iter().map(|k| k.as_str().to_string()).collect();
        let mut handles = Vec::with_capacity(lanes + 1);
        CONFIGURED_LANES.store(lanes, Ordering::Relaxed);

        for lane in 0..lanes {
            let ctx = Arc::clone(&ctx);
            let kinds = kinds.clone();
            let lane_id = format!("{id}/{lane}");
            let rx = stop.subscribe();
            handles.push(tokio::spawn(async move {
                // Stagger the lanes so a fresh boot does not send every lane at
                // the same row in the same millisecond, where all but one would
                // skip-lock and immediately poll again.
                tokio::time::sleep(Duration::from_millis(50 * lane as u64)).await;
                let name = lane_id.clone();
                supervise(name, rx.clone(), move || {
                    run_lane(Arc::clone(&ctx), lane_id.clone(), kinds.clone(), rx.clone())
                })
                .await;
            }));
        }

        let janitor_ctx = Arc::clone(&ctx);
        let janitor_stop = stop.subscribe();
        handles.push(tokio::spawn(supervise(
            "janitor".to_string(),
            stop.subscribe(),
            move || run_janitor(Arc::clone(&janitor_ctx), janitor_stop.clone()),
        )));

        tracing::info!(worker = %id, lanes, kinds = Kind::ALL.len(), "worker started");
        Worker { stop, handles, id }
    }

    /// This process's worker identity, as it appears in `jobs.locked_by`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Ask every lane to stop, and wait for the ones that are mid-job.
    ///
    /// Lanes check for the stop signal between jobs, never inside one, so a job
    /// that has started always runs to its own completion or its own timeout.
    /// That is what "drains in-flight jobs" means here: no job is cancelled
    /// halfway through writing to ClickHouse.
    ///
    /// The wait itself is bounded by `within` (normally [`DRAIN_TIMEOUT`]). If
    /// a long job is still running when that expires, the row is left
    /// `running`; its heartbeat ends with the process, and a janitor reclaims
    /// it once it is [`STALE_AFTER`] old, which is exactly the case that
    /// machinery exists for. Returns whether every lane stopped in time.
    pub async fn shutdown(self, within: Duration) -> bool {
        tracing::info!(worker = %self.id, "worker draining");
        let _ = self.stop.send(true);

        let drain = async {
            for handle in self.handles {
                let _ = handle.await;
            }
        };

        match tokio::time::timeout(within, drain).await {
            Ok(()) => {
                tracing::info!(worker = %self.id, "worker stopped");
                true
            }
            Err(_) => {
                tracing::warn!(
                    worker = %self.id,
                    seconds = within.as_secs(),
                    "a job was still running at the drain deadline; it will be reclaimed after restart"
                );
                false
            }
        }
    }
}

/// Run a lane, and start it again if it ever exits while the worker is meant
/// to be running.
///
/// A lane exits normally only on the stop signal. Any other exit, a panic in
/// the claim loop above all, would otherwise leave the worker one lane short
/// for the rest of the process's life with nothing but one log line to show for
/// it. The restart is delayed by [`RESTART_DELAY`] so a lane that dies on every
/// start cannot spin.
async fn supervise<F, Fut>(name: String, stop: watch::Receiver<bool>, start: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        let outcome = tokio::spawn(start()).await;
        if *stop.borrow() || stop.has_changed().is_err() {
            break;
        }
        match outcome {
            Err(e) if e.is_panic() => {
                let message = panic_message(e.into_panic().as_ref());
                tracing::error!(lane = %name, panic = %message, "worker lane panicked; restarting it");
            }
            Err(_) => {
                // Cancelled: the runtime is shutting down.
                break;
            }
            Ok(()) => {
                tracing::error!(lane = %name, "worker lane exited unexpectedly; restarting it")
            }
        }
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

/// Counts a lane as live for as long as it exists, panics included.
struct LiveLane;

impl LiveLane {
    fn enter() -> Self {
        LIVE_LANES.fetch_add(1, Ordering::Relaxed);
        LiveLane
    }
}

impl Drop for LiveLane {
    fn drop(&mut self) {
        LIVE_LANES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A future that turns a panic while polling into an `Err`, the way
/// `futures::FutureExt::catch_unwind` does.
///
/// Written out here because this crate does not otherwise depend on `futures`.
/// `AssertUnwindSafe` is sound for its use: a job that panics is abandoned and
/// recorded as failed, and nothing it was holding is read again.
struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> CatchUnwind<F> {
    fn new(future: F) -> Self {
        CatchUnwind(Box::pin(future))
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = std::thread::Result<F::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.get_mut().0.as_mut();
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

/// The text of a panic payload, when it has one.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a panic with no message".to_string()
    }
}

/// Renews a job's lock in the background until dropped.
///
/// A separate task rather than a branch beside the job, so a slow Postgres
/// round trip never pauses the job itself. Dropping the guard stops it, on
/// every exit path, a panic included.
struct Heartbeat(JoinHandle<()>);

impl Heartbeat {
    fn start(ctx: &JobCtx, job: uuid::Uuid, lane: &str) -> Self {
        let pg = ctx.pg.clone();
        let lane = lane.to_string();
        Heartbeat(tokio::spawn(async move {
            let first = tokio::time::Instant::now() + HEARTBEAT_EVERY;
            let mut beat = tokio::time::interval_at(first, HEARTBEAT_EVERY);
            beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                beat.tick().await;
                match jobs::heartbeat(&pg, job, &lane).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(%job, lane = %lane, "job lock was reclaimed while it ran");
                        break;
                    }
                    Err(e) => tracing::warn!(%job, error = %e, "could not renew the job lock"),
                }
            }
        }))
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// This process's identity in `jobs.locked_by`.
///
/// The process id alone is not enough: two containers on the same host, and a
/// restarted container reusing a pid, would both claim to be the same worker,
/// and the janitor's "is this lock abandoned?" question would get the wrong
/// answer. The random suffix makes each *run* of the process distinct.
fn worker_id() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        micaforge_core::crypto::random_token(4)
    )
}

/// One lane: claim, run, repeat, until asked to stop.
async fn run_lane(
    ctx: Arc<JobCtx>,
    id: String,
    kinds: Vec<String>,
    mut stop: watch::Receiver<bool>,
) {
    let _live = LiveLane::enter();
    loop {
        if *stop.borrow() {
            break;
        }

        let quiet = match jobs::claim_next(&ctx.pg, &id, &kinds).await {
            Ok(Some(job)) => {
                let job_id = job.id;
                // The job body has its own panic guard inside `execute`. This
                // one covers the bookkeeping around it, so nothing a job row
                // can provoke takes the lane down; the row is left `running`
                // and the janitor reclaims it once its heartbeat is stale.
                if let Err(panic) = CatchUnwind::new(run_one(&ctx, job, &id)).await {
                    tracing::error!(
                        lane = %id,
                        job = %job_id,
                        panic = %panic_message(panic.as_ref()),
                        "job bookkeeping panicked; the row will be reclaimed"
                    );
                }
                continue;
            }
            Ok(None) => POLL_INTERVAL,
            Err(e) => {
                tracing::error!(lane = %id, error = %e, "could not claim a job");
                ERROR_BACKOFF
            }
        };

        if wait_or_stop(quiet, &mut stop).await {
            break;
        }
    }

    tracing::debug!(lane = %id, "lane stopped");
}

/// Sleep, unless the stop signal arrives first. Returns whether to stop.
///
/// A closed channel counts as a stop. Without that, a [`Worker`] dropped instead
/// of shut down would leave every lane with a `changed()` that resolves
/// instantly, and the poll loop would spin against Postgres as fast as it could
/// issue a query.
async fn wait_or_stop(how_long: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    let closed = tokio::select! {
        _ = tokio::time::sleep(how_long) => false,
        result = stop.changed() => result.is_err(),
    };
    closed || *stop.borrow()
}

/// Run one claimed job to a conclusion, and record what that conclusion was.
///
/// Every exit from this function writes to the row. A job that is claimed and
/// then left `running` with nobody working on it is the one state the queue
/// cannot reason about, so the only path that produces it is the process dying,
/// which the janitor cleans up.
async fn run_one(ctx: &JobCtx, job: Job, lane: &str) {
    // An unknown kind is a row written by a newer build. It cannot be retried
    // into working, and it is not this build's to delete, so it is parked
    // immediately under its own real name.
    let Ok(kind) = Kind::parse(&job.kind) else {
        let reference = JobRef {
            id: job.id,
            kind: &job.kind,
            attempts: schedule::MAX_ATTEMPTS,
        };
        let _ = schedule::fail_with_backoff(
            &ctx.pg,
            &reference,
            "this build does not know this job kind",
        )
        .await;
        return;
    };

    let span = tracing::info_span!(
        "job",
        kind = %kind,
        job = %job.id,
        attempt = job.attempts,
    );

    // The whole of the job, including its bookkeeping, runs inside the span, so
    // every line a job logs carries the kind and the id. Instrumenting the
    // future is the only correct way to do that across an await point: entering
    // a span and then awaiting would leak it into whatever the runtime schedules
    // next on this thread.
    execute(ctx, job, kind, lane).instrument(span).await;
}

/// The body of one job: run it under its timeout and its heartbeat, then write
/// down what happened.
async fn execute(ctx: &JobCtx, job: Job, kind: Kind, lane: &str) {
    let started = std::time::Instant::now();

    let heartbeat = Heartbeat::start(ctx, job.id, lane);
    let outcome = CatchUnwind::new(tokio::time::timeout(
        kind.timeout(),
        dispatch(ctx, kind, &job.payload),
    ))
    .await;
    drop(heartbeat);
    let took_ms = started.elapsed().as_millis() as u64;

    let failure = match outcome {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(e))) => Some(e.to_string()),
        Ok(Err(_)) => Some(format!(
            "timed out after {} seconds",
            kind.timeout().as_secs()
        )),
        Err(panic) => {
            let message = panic_message(panic.as_ref());
            tracing::error!(took_ms, panic = %message, "job panicked");
            Some(format!("panicked: {message}"))
        }
    };

    let Some(error) = failure else {
        if let Err(e) = jobs::complete(&ctx.pg, job.id).await {
            // The work is done and only the bookkeeping failed. Saying so is the
            // honest thing: the janitor will reclaim the row and the job will run
            // a second time, which every job here tolerates by design.
            tracing::error!(error = %e, "job finished but could not be marked done");
        }
        tracing::info!(took_ms, "job finished");
        reschedule(ctx, kind, &job.payload).await;
        return;
    };

    if kind.failure_is_loud() {
        // A failed crawl retries tonight and nobody needs to know. A failed
        // import or report has somebody waiting for a result that will not
        // arrive on its own.
        tracing::error!(took_ms, error, "job failed");
    }

    let reference = JobRef {
        id: job.id,
        kind: &job.kind,
        attempts: job.attempts,
    };
    match schedule::fail_with_backoff(&ctx.pg, &reference, &error).await {
        // Parked for good. The cadence still has to survive it, and an import's
        // upload is now a file no retry will read.
        Ok(true) => {
            if kind == Kind::RunImport {
                crate::run_import::discard_after_failure(&ctx.pg, &job.payload, &error).await;
            }
            reschedule(ctx, kind, &job.payload).await
        }
        Ok(false) => {}
        Err(e) => tracing::error!(error = %e, "could not record a job failure"),
    }
}

/// Queue the next occurrence of a repeating job, and never let that failure
/// become the job's failure.
async fn reschedule(ctx: &JobCtx, kind: Kind, payload: &Json) {
    if let Err(e) = schedule::enqueue_next(&ctx.pg, kind, payload).await {
        tracing::error!(kind = %kind, error = %e, "could not queue the next occurrence");
    }
}

/// Hand a job to its module.
///
/// The one place the crate's ten job modules are named together, so adding a
/// kind is a variant, a module and a line here, and the compiler names the line
/// if it is forgotten.
async fn dispatch(ctx: &JobCtx, kind: Kind, payload: &Json) -> Result<()> {
    use crate::decode;

    match kind {
        Kind::CrawlSitePolicy => crate::crawl_site_policy::run(ctx, decode(kind, payload)?).await,
        Kind::IndexSitemap => crate::index_sitemap::run(ctx, decode(kind, payload)?).await,
        Kind::RefreshAgentRanges => {
            crate::refresh_agent_ranges::run(ctx, decode(kind, payload)?).await
        }
        Kind::RollupSessions => crate::rollup_sessions::run(ctx, decode(kind, payload)?).await,
        Kind::PruneRetention => crate::prune_retention::run(ctx, decode(kind, payload)?).await,
        Kind::CheckAlerts => crate::check_alerts::run(ctx, decode(kind, payload)?).await,
        Kind::SendReport => crate::send_report::run(ctx, decode(kind, payload)?).await,
        Kind::RunImport => crate::run_import::run(ctx, decode(kind, payload)?).await,
        Kind::DetectDecay => crate::detect_decay::run(ctx, decode(kind, payload)?).await,
        Kind::CrawlPageMeta => crate::crawl_page_meta::run(ctx, decode(kind, payload)?).await,
        Kind::Investigate => crate::investigate::run(ctx, decode(kind, payload)?).await,
        Kind::InsightDigest => crate::insight_digest::run(ctx, decode(kind, payload)?).await,
        Kind::RunUptimeChecks => crate::uptime::run(ctx, decode(kind, payload)?).await,
        Kind::NotifyBlockedTraffic => {
            crate::notify_blocked_traffic::run(ctx, decode(kind, payload)?).await
        }
        Kind::MeterUsage => crate::meter_usage::run(ctx, decode(kind, payload)?).await,
        Kind::SyncSearchConsole => crate::search_console::run(ctx, decode(kind, payload)?).await,
    }
}

/// Return abandoned rows to the queue, sweep finished ones away, and remove
/// import scratch files that have outlived any import that could read them.
///
/// Reclaiming uses [`micaforge_store::models::jobs::reap_stale`] rather than a
/// second copy of the SQL, and parks at the same
/// [`crate::schedule::MAX_ATTEMPTS`] the worker does: a row that has been
/// abandoned that many times has taken a worker down that many times, and
/// parking it is a better answer than handing it to another lane. A recurring
/// row parked here still gets its next occurrence, exactly as one parked by
/// the worker does, or one bad night would end the schedule.
async fn run_janitor(ctx: Arc<JobCtx>, mut stop: watch::Receiver<bool>) {
    let mut next_sweep = tokio::time::Instant::now();
    loop {
        if *stop.borrow() {
            break;
        }

        if tokio::time::Instant::now() >= next_sweep {
            next_sweep = tokio::time::Instant::now() + ORPHAN_SWEEP_INTERVAL;
            match crate::run_import::sweep_orphans(crate::run_import::ORPHAN_AFTER).await {
                0 => {}
                n => tracing::info!(files = n, "removed orphaned import scratch files"),
            }
        }

        match jobs::reap_stale(&ctx.pg, STALE_AFTER.as_secs() as i64).await {
            Ok(rows) if rows.is_empty() => {}
            Ok(rows) => {
                tracing::warn!(
                    jobs = rows.len(),
                    "reclaimed jobs from a worker that stopped"
                );
                for row in rows.iter().filter(|row| row.status == "failed") {
                    tracing::error!(
                        job = %row.id,
                        kind = %row.kind,
                        attempts = row.attempts,
                        "job abandoned too many times; parked"
                    );
                    if let Ok(kind) = Kind::parse(&row.kind) {
                        if kind == Kind::RunImport {
                            crate::run_import::discard_after_failure(
                                &ctx.pg,
                                &row.payload,
                                "the import stopped its worker too many times and was given up",
                            )
                            .await;
                        }
                        reschedule(&ctx, kind, &row.payload).await;
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "could not reclaim stale jobs"),
        }

        let before = chrono::Utc::now() - chrono::Duration::days(KEEP_FINISHED_DAYS);
        match jobs::purge_finished(&ctx.pg, before).await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(jobs = n, "purged finished job rows"),
            Err(e) => tracing::error!(error = %e, "could not purge finished jobs"),
        }

        if wait_or_stop(JANITOR_INTERVAL, &mut stop).await {
            break;
        }
    }

    tracing::debug!("janitor stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_identity_is_unique_per_run() {
        // Two processes on one host, or one process restarted into the same pid,
        // must not look like the same worker to the janitor.
        assert_ne!(worker_id(), worker_id());
        assert!(worker_id().starts_with(&format!("{}-", std::process::id())));
    }

    #[tokio::test]
    async fn a_closed_stop_channel_stops_a_lane_rather_than_spinning() {
        // The bug this guards against is not a hang, it is the opposite: a lane
        // whose `changed()` resolves instantly forever, polling Postgres as fast
        // as it can issue a query.
        let (stop, mut rx) = watch::channel(false);
        drop(stop);

        let started = tokio::time::Instant::now();
        assert!(wait_or_stop(Duration::from_secs(3_600), &mut rx).await);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn a_stop_signal_interrupts_the_poll_wait() {
        let (stop, mut rx) = watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = stop.send(true);
        });
        assert!(wait_or_stop(Duration::from_secs(3_600), &mut rx).await);
    }

    #[tokio::test]
    async fn an_ordinary_quiet_moment_does_not_stop_a_lane() {
        let (_stop, mut rx) = watch::channel(false);
        assert!(!wait_or_stop(Duration::from_millis(5), &mut rx).await);
    }

    #[test]
    fn a_live_lane_always_renews_before_its_lock_goes_stale() {
        // A job still running when the janitor decides its lock is abandoned
        // would be executed twice at once, which is the one outcome the queue is
        // built to prevent. Several heartbeats must fit inside the window, so
        // one slow round trip cannot make a live job look dead.
        assert!(HEARTBEAT_EVERY * 3 <= STALE_AFTER);
        // And the window must be short, or a killed process stops recurring
        // schedules for as long as it lasts.
        assert!(STALE_AFTER + JANITOR_INTERVAL <= Duration::from_secs(5 * 60));
    }

    #[test]
    fn the_worker_drain_fits_inside_the_container_grace_period() {
        // Compose gives the process 45 seconds after SIGTERM; HTTP takes ten
        // of them before the worker is asked to stop.
        assert!(DRAIN_TIMEOUT <= Duration::from_secs(25));
    }

    #[tokio::test]
    async fn a_panicking_future_becomes_an_error_with_its_message() {
        let outcome = CatchUnwind::new(async {
            if true {
                panic!("boom at {}", 42);
            }
        })
        .await;
        let panic = outcome.expect_err("the panic must be caught");
        assert_eq!(panic_message(panic.as_ref()), "boom at 42");

        let fine = CatchUnwind::new(async { 7 }).await;
        assert_eq!(fine.ok(), Some(7));
    }

    #[tokio::test]
    async fn a_lane_that_panics_is_restarted_until_the_worker_stops() {
        let (stop, rx) = watch::channel(false);
        let starts = Arc::new(AtomicUsize::new(0));

        let counter = Arc::clone(&starts);
        let supervisor = tokio::spawn(supervise("test".to_string(), rx, move || {
            let counter = Arc::clone(&counter);
            async move {
                if counter.fetch_add(1, Ordering::SeqCst) < 2 {
                    panic!("lane died");
                }
                // The third start behaves: it runs until it is cancelled.
                std::future::pending::<()>().await;
            }
        }));

        tokio::time::timeout(Duration::from_secs(10), async {
            while starts.load(Ordering::SeqCst) < 3 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a panicked lane must be started again");

        // Once the worker is stopping, an exit is final.
        let _ = stop.send(true);
        supervisor.abort();
    }

    #[tokio::test]
    async fn a_lane_counts_as_live_only_while_it_exists() {
        let before = live_lanes();
        let guard = LiveLane::enter();
        assert_eq!(live_lanes(), before + 1);
        drop(guard);
        assert_eq!(live_lanes(), before);
    }
}
