//! The batching event writer.
//!
//! ClickHouse wants few, large inserts. A browser sends one small hit at a time.
//! This module is the buffer between those two facts, and its single hard
//! requirement is that **the request path never waits on it**: `push_event`
//! hands a row to a channel and returns, so a slow or unreachable ClickHouse
//! shows up as a lag in the dashboard and never as a slow `POST /api/track`.
//!
//! One lane per table, each with its own task and its own buffer. Lanes are
//! independent on purpose: a pathological `replay_events` payload must not delay
//! a pageview, and a failing insert on one table must not stall another.
//!
//! A lane writes when its buffer reaches [`WriterCfg::batch_rows`], when
//! [`WriterCfg::flush_interval`] elapses with anything buffered, when someone
//! calls [`EventWriter::flush`], and while it drains at shutdown.
//!
//! # ClickHouse being away is normal
//!
//! A restart of the ClickHouse container takes tens of seconds, and an upgrade
//! can take minutes. Every hit that arrives in that window is kept: a batch
//! that fails is put back at the front of the buffer and tried again on a capped
//! exponential backoff ([`backoff`]), for as long as ClickHouse stays away. The
//! lane keeps reading its channel the whole time, so the channel itself never
//! grows; the buffer is the only place rows wait.
//!
//! The buffer is bounded, per lane, by [`WriterCfg::max_buffer_rows`] and by an
//! approximate byte size, [`WriterCfg::max_buffer_bytes`]
//! (`MICAFORGE_WRITER_BUFFER_MB`). Above either bound the **oldest** rows are
//! dropped, counted, and reported at `warn` no more than once every few
//! seconds. That is the chosen trade: a long outage costs its oldest minutes
//! rather than the whole process, which on a 4 GB machine shared with three
//! databases is what an unbounded queue would eventually cost.
//!
//! Every insert carries its own send and response deadlines, so a ClickHouse
//! that accepts the connection and then says nothing is a failed attempt
//! within a minute rather than a lane that waits forever.
//!
//! A batch that ClickHouse rejects for its *content* (a type mismatch, a
//! column that does not exist) is not an outage, and retrying it forever would
//! wedge the lane behind one bad row. It is dropped after a few attempts and
//! logged at `error`.
//!
//! A retried insert is at-least-once: an attempt that timed out after
//! ClickHouse had in fact committed it is written a second time. A duplicate
//! in a rare timeout is the smaller failure next to losing every hit of a
//! restart.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clickhouse::Row;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::ch::{AgentEventRow, BotEventRow, Ch, ErrorRow, EventRow, PageIndexRow, ReplayRow};

/// One mebibyte, for the buffer budget.
const MIB: usize = 1024 * 1024;

/// Default per-lane buffer budget, in mebibytes (`MICAFORGE_WRITER_BUFFER_MB`).
///
/// Thirty-two. Six lanes at the ceiling is under 200 MB, which a 4 GB machine
/// can spare beside Postgres, ClickHouse and Valkey. At a busy 50 hits a second
/// it holds about ten minutes of pageviews, which is longer than a ClickHouse
/// restart takes by an order of magnitude.
pub const DEFAULT_BUFFER_MB: usize = 32;

/// Default per-lane row ceiling, whatever the rows weigh.
///
/// The byte budget is the bound that normally applies. This one stops a flood
/// of tiny rows from costing more in per-row overhead than the estimate sees.
const DEFAULT_BUFFER_ROWS: usize = 200_000;

/// The most rows one insert carries.
///
/// Larger than [`WriterCfg::batch_rows`] so that catching up after an outage
/// takes a few large inserts, which is what ClickHouse wants, rather than
/// hundreds of small ones.
const MAX_INSERT_ROWS: usize = 10_000;

/// The most approximate bytes one insert carries.
const MAX_INSERT_BYTES: usize = 8 * MIB;

/// How long one chunk of an insert may take to reach the socket.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long ClickHouse may take to answer an insert once the rows are sent.
///
/// Thirty seconds includes the session view that every `events` insert feeds,
/// with a wide margin. An insert slower than that is not going to succeed.
const END_TIMEOUT: Duration = Duration::from_secs(30);

/// An outer bound on one whole attempt, connection included.
///
/// The driver's client has no connect timeout of its own, so this is what
/// stops a host that drops packets from holding an attempt open for as long as
/// the kernel keeps retrying the handshake.
const ATTEMPT_DEADLINE: Duration = Duration::from_secs(45);

/// The first retry delay after a failed insert.
const BACKOFF_FIRST: Duration = Duration::from_millis(250);

/// The longest delay between two attempts while ClickHouse is away.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// The longest delay between two attempts while the lane drains at shutdown.
const DRAIN_BACKOFF_CAP: Duration = Duration::from_secs(1);

/// How many times a batch ClickHouse rejected for its content is tried.
const MAX_REJECTIONS: u32 = 3;

/// How long a lane drains when its channel closes without a shutdown call.
const CLOSED_DRAIN: Duration = Duration::from_secs(10);

/// The least time between two log lines about the same problem on one lane.
const LOG_EVERY: Duration = Duration::from_secs(10);

/// How many messages a lane takes off its channel per wake-up.
const RECV_CHUNK: usize = 512;

/// ClickHouse error codes that mean the rows themselves are wrong, so retrying
/// the same batch can never succeed.
///
/// Anything not listed here (memory pressure, too many parts, a read-only
/// table during startup, a proxy's 5xx) is treated as ClickHouse being away and
/// retried without limit.
const DATA_ERROR_CODES: &[u32] = &[
    6,   // CANNOT_PARSE_TEXT
    16,  // NO_SUCH_COLUMN_IN_TABLE
    26,  // CANNOT_PARSE_QUOTED_STRING
    27,  // CANNOT_PARSE_INPUT_ASSERTION_FAILED
    32,  // ATTEMPT_TO_READ_AFTER_EOF
    33,  // CANNOT_READ_ALL_DATA
    38,  // CANNOT_PARSE_DATE
    41,  // CANNOT_PARSE_DATETIME
    53,  // TYPE_MISMATCH
    62,  // SYNTAX_ERROR
    69,  // ARGUMENT_OUT_OF_BOUND
    70,  // CANNOT_CONVERT_TYPE
    72,  // CANNOT_PARSE_NUMBER
    117, // INCORRECT_DATA
    131, // TOO_LARGE_STRING_SIZE
    349, // CANNOT_INSERT_NULL_IN_ORDINARY_COLUMN
];

/// How the writer batches, and how much it may hold.
#[derive(Debug, Clone, Copy)]
pub struct WriterCfg {
    /// Write once this many rows are buffered on a lane.
    pub batch_rows: usize,
    /// Write a non-empty buffer after this long, whatever its size.
    pub flush_interval: Duration,
    /// The most rows one lane holds before it drops its oldest.
    pub max_buffer_rows: usize,
    /// The most approximate bytes one lane holds before it drops its oldest.
    pub max_buffer_bytes: usize,
}

impl Default for WriterCfg {
    /// A thousand rows or two seconds, and [`DEFAULT_BUFFER_MB`] per lane.
    ///
    /// Two seconds is the number that matters. It is the worst-case lag between
    /// a hit arriving and appearing in the realtime view, and the product's
    /// promise is that data shows up "within seconds of the first pageview". A
    /// larger batch would be cheaper for ClickHouse and would break that.
    fn default() -> Self {
        Self {
            batch_rows: 1_000,
            flush_interval: Duration::from_secs(2),
            max_buffer_rows: DEFAULT_BUFFER_ROWS,
            max_buffer_bytes: DEFAULT_BUFFER_MB * MIB,
        }
    }
}

impl WriterCfg {
    /// The same configuration with a per-lane byte budget of `mb` mebibytes.
    pub fn with_buffer_mb(self, mb: usize) -> Self {
        Self {
            max_buffer_bytes: mb.max(1).saturating_mul(MIB),
            ..self
        }
    }
}

/// The delay before attempt `failures + 1`, after `failures` failed in a row.
///
/// 250 ms doubling to a 30 second ceiling: 0.25, 0.5, 1, 2, 4, 8, 16, 30, 30,
/// and so on for as long as ClickHouse stays away. The early steps make a blip
/// invisible; the ceiling keeps a long outage to two attempts a minute per lane.
pub fn backoff(failures: u32) -> Duration {
    let steps = failures.saturating_sub(1).min(16);
    BACKOFF_FIRST.saturating_mul(1u32 << steps).min(BACKOFF_CAP)
}

/// What travels down a lane.
enum Msg<T> {
    /// A row to buffer. Boxed because `EventRow` is sixty fields wide and the
    /// channel would otherwise move all of it on every send.
    Row(Box<T>),
    /// Write everything buffered so far, then answer whether it all landed.
    ///
    /// Sent through the same channel as the rows rather than signalled out of
    /// band, so it cannot overtake a row that was pushed before it.
    Flush(oneshot::Sender<bool>),
    /// Write everything, retrying until the deadline, then stop. The answer is
    /// how many rows were still unwritten at the deadline.
    Drain(Instant, oneshot::Sender<u64>),
}

/// One lane's counters, shared between its task and the health route.
#[derive(Default)]
struct LaneStats {
    queued_rows: AtomicU64,
    queued_bytes: AtomicU64,
    dropped_rows: AtomicU64,
    written_rows: AtomicU64,
    failing: AtomicBool,
}

/// A point-in-time view of one lane, for `GET /api/health/ready`.
#[derive(Debug, Clone, Serialize)]
pub struct LaneSnapshot {
    /// The table this lane writes.
    pub table: &'static str,
    /// Rows held in memory, including any batch currently being written.
    pub queued_rows: u64,
    /// Approximate bytes those rows occupy.
    pub queued_bytes: u64,
    /// Rows dropped since boot: oldest-first when the buffer was full, a batch
    /// ClickHouse rejected for its content, or rows left at the shutdown
    /// deadline.
    pub dropped_rows: u64,
    /// Rows written since boot.
    pub written_rows: u64,
    /// True while the last insert attempt on this lane failed.
    pub failing: bool,
}

/// One table's channel.
struct Lane<T> {
    table: &'static str,
    tx: mpsc::UnboundedSender<Msg<T>>,
    stats: Arc<LaneStats>,
}

impl<T> Lane<T> {
    /// Buffer a row.
    ///
    /// A send only fails once the lane's task is gone, which happens after
    /// shutdown. Logging rather than returning is deliberate: the caller is an
    /// HTTP handler that has already answered the browser, and there is nothing
    /// it could do with the error.
    fn push(&self, row: T) {
        if self.tx.send(Msg::Row(Box::new(row))).is_err() {
            tracing::error!(
                table = self.table,
                "event writer lane is closed; row dropped"
            );
        }
    }

    /// Ask the lane to write what it holds, and wait for the answer.
    async fn flush(&self) -> bool {
        let (ack, wait) = oneshot::channel();
        if self.tx.send(Msg::Flush(ack)).is_err() {
            return false;
        }
        wait.await.unwrap_or(false)
    }

    /// Ask the lane to drain by `deadline` and stop. Returns the rows lost.
    async fn drain(&self, deadline: Instant) -> u64 {
        let (ack, wait) = oneshot::channel();
        if self.tx.send(Msg::Drain(deadline, ack)).is_err() {
            return 0;
        }
        wait.await.unwrap_or(0)
    }

    fn snapshot(&self) -> LaneSnapshot {
        let s = &self.stats;
        LaneSnapshot {
            table: self.table,
            queued_rows: s.queued_rows.load(Ordering::Relaxed),
            queued_bytes: s.queued_bytes.load(Ordering::Relaxed),
            dropped_rows: s.dropped_rows.load(Ordering::Relaxed),
            written_rows: s.written_rows.load(Ordering::Relaxed),
            failing: s.failing.load(Ordering::Relaxed),
        }
    }
}

/// The batching writer.
///
/// Clone it freely; every clone shares the same lanes. Dropping the last clone
/// closes the channels, which each lane task takes as "drain briefly and exit".
/// Prefer [`EventWriter::shutdown`], which drains to a deadline and says what it
/// could not write.
#[derive(Clone)]
pub struct EventWriter {
    events: Arc<Lane<EventRow>>,
    agents: Arc<Lane<AgentEventRow>>,
    bots: Arc<Lane<BotEventRow>>,
    replays: Arc<Lane<ReplayRow>>,
    errors: Arc<Lane<ErrorRow>>,
    pages: Arc<Lane<PageIndexRow>>,
}

impl Debug for EventWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventWriter")
    }
}

impl EventWriter {
    /// Start one background task per table.
    ///
    /// The tasks live as long as the returned writer. There is no handle to
    /// await: the flush path is [`EventWriter::flush`], and the stop path is
    /// [`EventWriter::shutdown`].
    pub fn spawn(ch: Ch, cfg: WriterCfg) -> EventWriter {
        EventWriter {
            events: Arc::new(spawn_lane::<EventRow>(ch.clone(), "events", cfg)),
            agents: Arc::new(spawn_lane::<AgentEventRow>(ch.clone(), "agent_events", cfg)),
            bots: Arc::new(spawn_lane::<BotEventRow>(ch.clone(), "bot_events", cfg)),
            replays: Arc::new(spawn_lane::<ReplayRow>(ch.clone(), "replay_events", cfg)),
            errors: Arc::new(spawn_lane::<ErrorRow>(ch.clone(), "errors", cfg)),
            pages: Arc::new(spawn_lane::<PageIndexRow>(ch, "page_index", cfg)),
        }
    }

    /// Queue a human or custom event. Returns immediately.
    pub fn push_event(&self, row: EventRow) {
        self.events.push(row);
    }

    /// Queue a named agent fetch. Returns immediately.
    pub fn push_agent(&self, row: AgentEventRow) {
        self.agents.push(row);
    }

    /// Queue an unnamed or suspected non-human hit. Returns immediately.
    pub fn push_bot(&self, row: BotEventRow) {
        self.bots.push(row);
    }

    /// Queue one rrweb replay event. Returns immediately.
    pub fn push_replay(&self, row: ReplayRow) {
        self.replays.push(row);
    }

    /// Queue a client-side error. Returns immediately.
    pub fn push_error(&self, row: ErrorRow) {
        self.errors.push(row);
    }

    /// Queue a content-register entry. Returns immediately.
    pub fn push_page(&self, row: PageIndexRow) {
        self.pages.push(row);
    }

    /// Try to write everything currently buffered, on every lane, and wait.
    ///
    /// Returns `true` when every row pushed before the call has been written.
    /// `false` means at least one lane's attempt failed: those rows are still
    /// buffered and will be retried on the lane's own backoff. A flush is one
    /// attempt, not a wait for ClickHouse to come back, so it returns within
    /// one insert deadline even when the store is down.
    ///
    /// Lanes are flushed concurrently because they are independent; doing it
    /// in series would make a caller wait for six round trips.
    pub async fn flush(&self) -> bool {
        let results = tokio::join!(
            self.events.flush(),
            self.agents.flush(),
            self.bots.flush(),
            self.replays.flush(),
            self.errors.flush(),
            self.pages.flush(),
        );
        results.0 && results.1 && results.2 && results.3 && results.4 && results.5
    }

    /// Drain every lane, retrying until `within` has passed, then stop them.
    ///
    /// Returns how many rows were still unwritten at the deadline; those are
    /// logged at `error` by their lane. Consuming `self` is the point: after
    /// this returns the lanes have exited, and a push through any surviving
    /// clone is refused and logged.
    pub async fn shutdown(self, within: Duration) -> u64 {
        let deadline = Instant::now() + within;
        let lost = tokio::join!(
            self.events.drain(deadline),
            self.agents.drain(deadline),
            self.bots.drain(deadline),
            self.replays.drain(deadline),
            self.errors.drain(deadline),
            self.pages.drain(deadline),
        );
        lost.0 + lost.1 + lost.2 + lost.3 + lost.4 + lost.5
    }

    /// Every lane's counters, for the readiness route.
    pub fn stats(&self) -> Vec<LaneSnapshot> {
        vec![
            self.events.snapshot(),
            self.agents.snapshot(),
            self.bots.snapshot(),
            self.replays.snapshot(),
            self.errors.snapshot(),
            self.pages.snapshot(),
        ]
    }
}

/// Start one lane and return its sender.
fn spawn_lane<T>(ch: Ch, table: &'static str, cfg: WriterCfg) -> Lane<T>
where
    T: Row + Serialize + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel::<Msg<T>>();
    let stats = Arc::new(LaneStats::default());

    let task = LaneTask {
        ch,
        table,
        cfg,
        stats: Arc::clone(&stats),
        buf: Buffer::new(cfg.max_buffer_rows, cfg.max_buffer_bytes),
        inflight: None,
        inflight_rows: 0,
        inflight_bytes: 0,
        inflight_first: None,
        want: false,
        failures: 0,
        rejections: 0,
        retry_at: None,
        flushes: Vec::new(),
        drain: None,
        drops: DropLog::new(LOG_EVERY),
        failure_log: Throttle::new(LOG_EVERY),
    };
    tokio::spawn(task.run(rx));

    Lane { table, tx, stats }
}

// ---------------------------------------------------------------------------
// The lane task
// ---------------------------------------------------------------------------

/// What one insert attempt hands back: its rows, and whether they landed.
type Attempt<T> =
    Pin<Box<dyn Future<Output = (Vec<Entry<T>>, Result<(), clickhouse::error::Error>)> + Send>>;

/// One lane's state. Owned by its task; nothing here is shared but `stats`.
struct LaneTask<T> {
    ch: Ch,
    table: &'static str,
    cfg: WriterCfg,
    stats: Arc<LaneStats>,
    /// Rows waiting, oldest first.
    buf: Buffer<T>,
    /// The insert in progress, if any. Its rows are out of `buf` while it runs.
    inflight: Option<Attempt<T>>,
    inflight_rows: usize,
    inflight_bytes: usize,
    inflight_first: Option<u64>,
    /// Whether the buffer should be written as soon as nothing forbids it.
    want: bool,
    /// Failed attempts in a row, for the backoff.
    failures: u32,
    /// Content rejections of the current front batch, in a row.
    rejections: u32,
    /// No attempt starts before this, after a failure.
    retry_at: Option<Instant>,
    /// Callers of `flush`, each with the first sequence number it does not
    /// cover.
    flushes: Vec<(u64, oneshot::Sender<bool>)>,
    /// The drain deadline, and who to tell, once a drain has begun.
    drain: Option<(Instant, Option<oneshot::Sender<u64>>)>,
    drops: DropLog,
    failure_log: Throttle,
}

impl<T> LaneTask<T>
where
    T: Row + Serialize + Send + Sync + 'static,
{
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg<T>>) {
        let mut ticker = tokio::time::interval(self.cfg.flush_interval);
        // A tick missed while a slow insert was in flight must not fire a burst
        // of catch-up writes the moment it completes.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut inbox: Vec<Msg<T>> = Vec::with_capacity(RECV_CHUNK);
        let mut open = true;

        loop {
            self.maybe_start();

            if self.drain.is_some() && self.buf.is_empty() && self.inflight.is_none() {
                self.finish(0);
                break;
            }

            let retry_at = self.retry_at;
            let deadline = self.drain.as_ref().map(|(at, _)| *at);
            let idle = self.inflight.is_none();

            // Every branch keeps the channel moving. That is the property the
            // memory bound rests on: rows arriving during a slow insert or a
            // backoff land in the bounded buffer, never in the channel.
            tokio::select! {
                received = rx.recv_many(&mut inbox, RECV_CHUNK), if open => {
                    if received == 0 {
                        open = false;
                        self.on_closed();
                    } else {
                        for msg in inbox.drain(..) {
                            self.on_msg(msg);
                        }
                        self.publish();
                    }
                }
                (batch, result) = wait_attempt(&mut self.inflight) => {
                    self.on_result(batch, result);
                }
                () = sleep_until(retry_at), if idle => {
                    self.retry_at = None;
                }
                _ = ticker.tick() => {
                    if !self.buf.is_empty() {
                        self.want = true;
                    }
                }
                () = sleep_until(deadline) => {
                    self.abandon();
                    break;
                }
            }
        }

        tracing::debug!(table = self.table, "event writer lane closed");
    }

    /// Start an insert when one is wanted and nothing forbids it.
    fn maybe_start(&mut self) {
        if self.inflight.is_some() || self.retry_at.is_some() || self.buf.is_empty() {
            return;
        }
        if !(self.want || self.drain.is_some() || !self.flushes.is_empty()) {
            return;
        }

        let batch = self.buf.take_batch(MAX_INSERT_ROWS, MAX_INSERT_BYTES);
        self.inflight_rows = batch.len();
        self.inflight_bytes = batch.iter().map(|e| e.bytes).sum();
        self.inflight_first = batch.first().map(|e| e.seq);

        let ch = self.ch.clone();
        let table = self.table;
        self.inflight = Some(Box::pin(async move {
            let result = insert_all(&ch, table, &batch).await;
            (batch, result)
        }));
    }

    fn on_msg(&mut self, msg: Msg<T>) {
        match msg {
            Msg::Row(row) => {
                let bytes = approx_size(row.as_ref());
                let shed = self
                    .buf
                    .push(row, bytes, self.inflight_rows, self.inflight_bytes);
                self.note_dropped(shed);
                if self.buf.len() >= self.cfg.batch_rows {
                    self.want = true;
                }
            }
            Msg::Flush(ack) => {
                let upto = self.buf.next_seq();
                if self.nothing_held_before(upto) {
                    let _ = ack.send(true);
                } else {
                    self.flushes.push((upto, ack));
                    // A flush is "try now", backoff or not.
                    self.retry_at = None;
                    self.want = true;
                }
            }
            Msg::Drain(deadline, ack) => {
                let deadline = match &self.drain {
                    Some((earlier, _)) => deadline.min(*earlier),
                    None => deadline,
                };
                self.drain = Some((deadline, Some(ack)));
                self.retry_at = None;
                self.want = true;
            }
        }
    }

    /// Every sender is gone. Drain briefly, then stop.
    fn on_closed(&mut self) {
        if self.drain.is_none() {
            self.drain = Some((Instant::now() + CLOSED_DRAIN, None));
            self.retry_at = None;
            self.want = true;
        }
    }

    fn on_result(&mut self, batch: Vec<Entry<T>>, result: Result<(), clickhouse::error::Error>) {
        self.inflight = None;
        self.inflight_rows = 0;
        self.inflight_bytes = 0;
        self.inflight_first = None;

        match result {
            Ok(()) => {
                if self.failures > 0 {
                    tracing::info!(
                        table = self.table,
                        failed_attempts = self.failures,
                        "clickhouse is accepting inserts again"
                    );
                }
                self.failures = 0;
                self.rejections = 0;
                self.stats.failing.store(false, Ordering::Relaxed);
                self.stats
                    .written_rows
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
                tracing::trace!(table = self.table, rows = batch.len(), "batch written");
                drop(batch);

                self.want = !self.buf.is_empty()
                    && (self.buf.len() >= self.cfg.batch_rows
                        || !self.flushes.is_empty()
                        || self.drain.is_some());
                self.publish();
                self.settle_flushes(false);
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                self.stats.failing.store(true, Ordering::Relaxed);

                let rejected = failure_kind(&e) == Failure::Rejected;
                self.rejections = if rejected { self.rejections + 1 } else { 0 };

                if rejected && self.rejections >= MAX_REJECTIONS {
                    tracing::error!(
                        table = self.table,
                        rows = batch.len(),
                        attempts = self.rejections,
                        error = %e,
                        "clickhouse rejected this batch's content; batch dropped"
                    );
                    self.stats
                        .dropped_rows
                        .fetch_add(batch.len() as u64, Ordering::Relaxed);
                    self.rejections = 0;
                } else {
                    self.buf.requeue(batch);
                    let shed = self.buf.shed(0, 0);
                    self.note_dropped(shed);
                }

                let mut wait = backoff(self.failures);
                if let Some((deadline, _)) = &self.drain {
                    wait = wait
                        .min(DRAIN_BACKOFF_CAP)
                        .min(deadline.saturating_duration_since(Instant::now()));
                }
                self.retry_at = Some(Instant::now() + wait);
                self.want = true;

                if let Some(suppressed) = self.failure_log.ready(Instant::now()) {
                    tracing::warn!(
                        table = self.table,
                        attempt = self.failures,
                        queued_rows = self.buf.len(),
                        retry_in_ms = wait.as_millis() as u64,
                        suppressed,
                        error = %e,
                        "clickhouse insert failed; rows kept for retry"
                    );
                }

                self.publish();
                // A flush is one attempt. Whoever is waiting learns that their
                // rows are not in yet; the lane keeps retrying regardless.
                self.settle_flushes(true);
            }
        }
    }

    /// The drain deadline passed. Count what is left as lost, and stop.
    fn abandon(&mut self) {
        let lost = (self.buf.len() + self.inflight_rows) as u64;
        self.inflight = None;
        if lost > 0 {
            tracing::error!(
                table = self.table,
                rows = lost,
                "the shutdown deadline passed before these rows could be written; they are lost"
            );
            self.stats.dropped_rows.fetch_add(lost, Ordering::Relaxed);
        }
        self.finish(lost);
    }

    /// Stop: report any unreported drops, answer every waiter.
    fn finish(&mut self, lost: u64) {
        let pending = self.drops.take_pending();
        if pending > 0 {
            tracing::warn!(
                table = self.table,
                dropped = pending,
                "event writer buffer was full; oldest rows were dropped"
            );
        }
        self.stats.queued_rows.store(0, Ordering::Relaxed);
        self.stats.queued_bytes.store(0, Ordering::Relaxed);
        for (_, ack) in self.flushes.drain(..) {
            let _ = ack.send(lost == 0);
        }
        if let Some((_, Some(ack))) = self.drain.take() {
            let _ = ack.send(lost);
        }
    }

    /// Answer the flushes whose rows have all left the buffer. With
    /// `fail_rest`, answer every other flush `false`.
    fn settle_flushes(&mut self, fail_rest: bool) {
        let lowest = self.lowest_held();
        let mut waiting = Vec::new();
        for (upto, ack) in self.flushes.drain(..) {
            let done = lowest.is_none_or(|seq| seq >= upto);
            if done {
                let _ = ack.send(true);
            } else if fail_rest {
                let _ = ack.send(false);
            } else {
                waiting.push((upto, ack));
            }
        }
        self.flushes = waiting;
    }

    /// The oldest sequence number still held, in flight or buffered.
    fn lowest_held(&self) -> Option<u64> {
        self.inflight_first.or_else(|| self.buf.oldest_seq())
    }

    fn nothing_held_before(&self, upto: u64) -> bool {
        self.lowest_held().is_none_or(|seq| seq >= upto)
    }

    fn note_dropped(&mut self, rows: u64) {
        if rows == 0 {
            return;
        }
        self.stats.dropped_rows.fetch_add(rows, Ordering::Relaxed);
        if let Some(dropped) = self.drops.record(rows, Instant::now()) {
            tracing::warn!(
                table = self.table,
                dropped,
                total_dropped = self.stats.dropped_rows.load(Ordering::Relaxed),
                max_rows = self.cfg.max_buffer_rows,
                max_bytes = self.cfg.max_buffer_bytes,
                "event writer buffer is full; oldest rows dropped"
            );
        }
    }

    /// Copy the queue depth into the shared counters.
    fn publish(&self) {
        self.stats.queued_rows.store(
            (self.buf.len() + self.inflight_rows) as u64,
            Ordering::Relaxed,
        );
        self.stats.queued_bytes.store(
            (self.buf.bytes() + self.inflight_bytes) as u64,
            Ordering::Relaxed,
        );
    }
}

/// Wait for the insert in flight, or forever when there is none.
async fn wait_attempt<T>(
    slot: &mut Option<Attempt<T>>,
) -> (Vec<Entry<T>>, Result<(), clickhouse::error::Error>) {
    match slot.as_mut() {
        Some(attempt) => attempt.await,
        None => std::future::pending().await,
    }
}

/// Sleep until `at`, or forever when there is no `at`.
async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// One attempt at writing a batch, under every deadline this module promises.
async fn insert_all<T>(
    ch: &Ch,
    table: &str,
    batch: &[Entry<T>],
) -> std::result::Result<(), clickhouse::error::Error>
where
    T: Row + Serialize + Send + Sync + 'static,
{
    let attempt = async {
        let mut insert = ch
            .client()
            .insert::<T>(table)?
            .with_timeouts(Some(SEND_TIMEOUT), Some(END_TIMEOUT));
        for entry in batch {
            insert.write(entry.row.as_ref()).await?;
        }
        insert.end().await
    };
    match tokio::time::timeout(ATTEMPT_DEADLINE, attempt).await {
        Ok(result) => result,
        Err(_) => Err(clickhouse::error::Error::TimedOut),
    }
}

/// Why an insert failed, as far as retrying is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// ClickHouse is away, slow or overloaded. Retry without limit.
    Unavailable,
    /// The rows themselves were refused. Retry a few times, then drop.
    Rejected,
}

/// Classify a driver error.
fn failure_kind(e: &clickhouse::error::Error) -> Failure {
    use clickhouse::error::Error as E;
    match e {
        E::BadResponse(message) => match ch_error_code(message) {
            Some(code) if DATA_ERROR_CODES.contains(&code) => Failure::Rejected,
            _ => Failure::Unavailable,
        },
        // Client-side encoding failures: these rows cannot be written by
        // this build, however long ClickHouse is given.
        E::InvalidParams(_)
        | E::Custom(_)
        | E::SequenceMustHaveLength
        | E::NotEnoughData
        | E::InvalidUtf8Encoding(_)
        | E::InvalidTagEncoding(_)
        | E::VariantDiscriminatorIsOutOfBound(_)
        | E::DeserializeAnyNotSupported
        | E::Unsupported(_) => Failure::Rejected,
        _ => Failure::Unavailable,
    }
}

/// The numeric code in a ClickHouse error message: `Code: 53. DB::Exception`.
fn ch_error_code(message: &str) -> Option<u32> {
    let rest = &message[message.find("Code: ")? + "Code: ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Roughly how much memory a buffered row holds.
///
/// The struct itself plus the length of its JSON encoding, measured without
/// allocating. JSON over-counts numbers a little and that is the right side to
/// err on: the budget is a ceiling, and an estimate that ran low would let the
/// real footprint pass it.
fn approx_size<T: Serialize>(row: &T) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut count = Count(0);
    let encoded = match serde_json::to_writer(&mut count, row) {
        Ok(()) => count.0,
        Err(_) => 0,
    };
    std::mem::size_of::<T>() + encoded
}

// ---------------------------------------------------------------------------
// The bounded buffer
// ---------------------------------------------------------------------------

/// One buffered row, with its arrival order and its estimated weight.
struct Entry<T> {
    seq: u64,
    bytes: usize,
    row: Box<T>,
}

/// A FIFO of rows with a row ceiling and a byte ceiling, shedding oldest-first.
///
/// Pure bookkeeping with no I/O, so the bound and the drop order are tested
/// directly rather than through a ClickHouse that has to be made to fail.
struct Buffer<T> {
    rows: VecDeque<Entry<T>>,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
    next_seq: u64,
}

impl<T> Buffer<T> {
    fn new(max_rows: usize, max_bytes: usize) -> Self {
        Self {
            rows: VecDeque::new(),
            bytes: 0,
            max_rows: max_rows.max(1),
            max_bytes: max_bytes.max(1),
            next_seq: 0,
        }
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn bytes(&self) -> usize {
        self.bytes
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The sequence number the next pushed row will get.
    fn next_seq(&self) -> u64 {
        self.next_seq
    }

    fn oldest_seq(&self) -> Option<u64> {
        self.rows.front().map(|e| e.seq)
    }

    /// Append a row, then shed oldest rows until the lane fits.
    ///
    /// `held_rows` and `held_bytes` are the batch currently in flight. It
    /// counts against the bound because its memory is just as real, but it is
    /// not shed: those rows are already on their way to ClickHouse. Returns how
    /// many rows were shed.
    fn push(&mut self, row: Box<T>, bytes: usize, held_rows: usize, held_bytes: usize) -> u64 {
        self.rows.push_back(Entry {
            seq: self.next_seq,
            bytes,
            row,
        });
        self.next_seq += 1;
        self.bytes += bytes;
        self.shed(held_rows, held_bytes)
    }

    /// Drop oldest rows until this buffer plus what is held fits both bounds.
    fn shed(&mut self, held_rows: usize, held_bytes: usize) -> u64 {
        let mut shed = 0u64;
        while !self.rows.is_empty()
            && (self.rows.len() + held_rows > self.max_rows
                || self.bytes + held_bytes > self.max_bytes)
        {
            if let Some(oldest) = self.rows.pop_front() {
                self.bytes -= oldest.bytes;
                shed += 1;
            }
        }
        shed
    }

    /// Take a batch from the front: at least one row, then up to either cap.
    fn take_batch(&mut self, max_rows: usize, max_bytes: usize) -> Vec<Entry<T>> {
        let mut batch = Vec::new();
        let mut bytes = 0usize;
        while let Some(front) = self.rows.front() {
            let fits = batch.len() < max_rows && bytes + front.bytes <= max_bytes;
            if !batch.is_empty() && !fits {
                break;
            }
            if let Some(entry) = self.rows.pop_front() {
                bytes += entry.bytes;
                self.bytes -= entry.bytes;
                batch.push(entry);
            }
        }
        batch
    }

    /// Put a batch that failed back at the front, in its original order.
    fn requeue(&mut self, batch: Vec<Entry<T>>) {
        for entry in batch.into_iter().rev() {
            self.bytes += entry.bytes;
            self.rows.push_front(entry);
        }
    }
}

// ---------------------------------------------------------------------------
// Log rate limiting
// ---------------------------------------------------------------------------

/// Allows one log line per window and counts the ones it held back.
struct Throttle {
    every: Duration,
    last: Option<Instant>,
    suppressed: u64,
}

impl Throttle {
    fn new(every: Duration) -> Self {
        Self {
            every,
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(held back since the last line)` when a line may be written now.
    fn ready(&mut self, now: Instant) -> Option<u64> {
        match self.last {
            Some(last) if now.saturating_duration_since(last) < self.every => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// Accumulates dropped-row counts between rate-limited log lines.
struct DropLog {
    throttle: Throttle,
    pending: u64,
}

impl DropLog {
    fn new(every: Duration) -> Self {
        Self {
            throttle: Throttle::new(every),
            pending: 0,
        }
    }

    /// Record `rows` dropped. `Some(total since the last line)` when a line
    /// may be written now.
    fn record(&mut self, rows: u64, now: Instant) -> Option<u64> {
        self.pending += rows;
        self.throttle.ready(now).map(|_| self.take_pending())
    }

    /// Whatever has been dropped and not yet reported.
    fn take_pending(&mut self) -> u64 {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(max_rows: usize, max_bytes: usize) -> Buffer<u32> {
        Buffer::new(max_rows, max_bytes)
    }

    fn seqs(buf: &Buffer<u32>) -> Vec<u64> {
        buf.rows.iter().map(|e| e.seq).collect()
    }

    #[test]
    fn the_default_flush_interval_keeps_the_realtime_promise() {
        let cfg = WriterCfg::default();
        assert_eq!(cfg.batch_rows, 1_000);
        // The product promises data "within seconds". Anything above a couple of
        // seconds here breaks that promise silently.
        assert!(cfg.flush_interval <= Duration::from_secs(2));
        assert_eq!(cfg.max_buffer_bytes, DEFAULT_BUFFER_MB * MIB);
        assert!(cfg.max_buffer_rows >= MAX_INSERT_ROWS);
        assert_eq!(cfg.with_buffer_mb(8).max_buffer_bytes, 8 * MIB);
        // Zero would drop every row on arrival.
        assert_eq!(cfg.with_buffer_mb(0).max_buffer_bytes, MIB);
    }

    #[test]
    fn the_backoff_doubles_and_then_holds_at_its_ceiling() {
        let ms = |n| backoff(n).as_millis() as u64;
        assert_eq!(ms(1), 250);
        assert_eq!(ms(2), 500);
        assert_eq!(ms(3), 1_000);
        assert_eq!(ms(4), 2_000);
        assert_eq!(ms(7), 16_000);
        assert_eq!(ms(8), 30_000);
        assert_eq!(ms(9), 30_000);
        // A zero or absurd count still produces a sane delay.
        assert_eq!(ms(0), 250);
        assert_eq!(ms(u32::MAX), 30_000);
        for n in 1..64 {
            assert!(backoff(n) <= backoff(n + 1), "backoff shrank at {n}");
        }
    }

    #[test]
    fn a_full_buffer_drops_its_oldest_rows_by_count() {
        let mut buf = buffer(3, usize::MAX);
        let mut shed = 0;
        for n in 0..5u32 {
            shed += buf.push(Box::new(n), 1, 0, 0);
        }
        assert_eq!(shed, 2);
        assert_eq!(buf.len(), 3);
        // The two oldest are the ones gone.
        assert_eq!(seqs(&buf), vec![2, 3, 4]);
        assert_eq!(buf.bytes(), 3);
    }

    #[test]
    fn a_full_buffer_drops_its_oldest_rows_by_weight() {
        let mut buf = buffer(usize::MAX, 100);
        assert_eq!(buf.push(Box::new(1), 40, 0, 0), 0);
        assert_eq!(buf.push(Box::new(2), 40, 0, 0), 0);
        assert_eq!(buf.push(Box::new(3), 40, 0, 0), 1);
        assert_eq!(seqs(&buf), vec![1, 2]);
        assert_eq!(buf.bytes(), 80);
    }

    #[test]
    fn rows_in_flight_count_against_the_bound_but_are_not_shed() {
        let mut buf = buffer(3, usize::MAX);
        // Two rows are already being written, so only one more fits.
        assert_eq!(buf.push(Box::new(1), 1, 2, 0), 0);
        assert_eq!(buf.push(Box::new(2), 1, 2, 0), 1);
        assert_eq!(buf.len(), 1);
        assert_eq!(seqs(&buf), vec![1]);
    }

    #[test]
    fn a_row_heavier_than_the_whole_budget_is_dropped_on_arrival() {
        let mut buf = buffer(10, 100);
        assert_eq!(buf.push(Box::new(1), 500, 0, 0), 1);
        assert!(buf.is_empty());
        assert_eq!(buf.bytes(), 0);
    }

    #[test]
    fn batches_come_from_the_front_and_go_back_there_in_order() {
        let mut buf = buffer(100, usize::MAX);
        for n in 0..10u32 {
            buf.push(Box::new(n), 10, 0, 0);
        }

        let batch = buf.take_batch(4, usize::MAX);
        assert_eq!(
            batch.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert_eq!(buf.bytes(), 60);

        // A byte cap stops a batch early, but never below one row.
        let mut other = buffer(100, usize::MAX);
        other.push(Box::new(0), 50, 0, 0);
        other.push(Box::new(1), 50, 0, 0);
        assert_eq!(other.take_batch(10, 10).len(), 1);

        buf.requeue(batch);
        assert_eq!(seqs(&buf), (0..10).collect::<Vec<_>>());
        assert_eq!(buf.bytes(), 100);
        assert_eq!(buf.oldest_seq(), Some(0));
        assert_eq!(buf.next_seq(), 10);
    }

    #[test]
    fn a_requeued_batch_is_the_first_thing_shed_when_the_bound_tightens() {
        let mut buf = buffer(5, usize::MAX);
        for n in 0..3u32 {
            buf.push(Box::new(n), 1, 0, 0);
        }
        let batch = buf.take_batch(3, usize::MAX);
        // While the batch was out, three more rows arrived.
        for n in 3..6u32 {
            buf.push(Box::new(n), 1, batch.len(), 0);
        }
        // Only two of the three fitted beside the batch in flight.
        assert_eq!(seqs(&buf), vec![4, 5]);

        buf.requeue(batch);
        assert_eq!(buf.shed(0, 0), 0);
        assert_eq!(seqs(&buf), vec![0, 1, 2, 4, 5]);
    }

    #[test]
    fn the_throttle_lets_one_line_through_per_window() {
        let start = Instant::now();
        let mut log = Throttle::new(Duration::from_secs(10));
        assert_eq!(log.ready(start), Some(0));
        assert_eq!(log.ready(start + Duration::from_secs(1)), None);
        assert_eq!(log.ready(start + Duration::from_secs(2)), None);
        assert_eq!(log.ready(start + Duration::from_secs(11)), Some(2));
    }

    #[test]
    fn dropped_rows_are_summed_between_log_lines() {
        let start = Instant::now();
        let mut drops = DropLog::new(Duration::from_secs(10));
        assert_eq!(drops.record(3, start), Some(3));
        assert_eq!(drops.record(4, start + Duration::from_secs(1)), None);
        assert_eq!(drops.record(5, start + Duration::from_secs(2)), None);
        assert_eq!(drops.record(1, start + Duration::from_secs(12)), Some(10));
        assert_eq!(drops.record(2, start + Duration::from_secs(13)), None);
        assert_eq!(drops.take_pending(), 2);
    }

    #[test]
    fn content_errors_are_told_apart_from_an_absent_server() {
        use clickhouse::error::Error as E;
        assert_eq!(failure_kind(&E::TimedOut), Failure::Unavailable);
        assert_eq!(
            failure_kind(&E::BadResponse(
                "Code: 53. DB::Exception: Type mismatch in IN or VALUES section".into()
            )),
            Failure::Rejected
        );
        // Too many parts is ClickHouse asking for patience, not a bad row.
        assert_eq!(
            failure_kind(&E::BadResponse(
                "Code: 252. DB::Exception: Too many parts (300)".into()
            )),
            Failure::Unavailable
        );
        // A proxy's error page carries no code at all.
        assert_eq!(
            failure_kind(&E::BadResponse("502 Bad Gateway".into())),
            Failure::Unavailable
        );
        assert_eq!(failure_kind(&E::NotEnoughData), Failure::Rejected);
        assert_eq!(ch_error_code("Code: 241. DB::Exception"), Some(241));
        assert_eq!(ch_error_code("no code here"), None);
    }

    #[test]
    fn the_size_estimate_grows_with_the_row() {
        let small = EventRow::default();
        let large = EventRow {
            pathname: "/a".repeat(1_000),
            ..EventRow::default()
        };
        assert!(approx_size(&small) > std::mem::size_of::<EventRow>());
        assert!(approx_size(&large) >= approx_size(&small) + 2_000);
    }

    /// A ClickHouse handle for which every insert fails at once. Port 1
    /// refuses instantly, which is the fastest way to model a dead server.
    fn dead_clickhouse() -> Ch {
        Ch::connect("http://127.0.0.1:1", "micaforge", "default", "").unwrap()
    }

    #[tokio::test]
    async fn pushing_never_blocks_when_clickhouse_is_unreachable() {
        let writer = EventWriter::spawn(dead_clickhouse(), WriterCfg::default());

        for _ in 0..50 {
            writer.push_event(EventRow::default());
        }
        // A flush against a dead server must still complete rather than hang,
        // and must say that the rows are not in.
        let landed = tokio::time::timeout(Duration::from_secs(30), writer.flush())
            .await
            .expect("flush must not hang when the store is down");
        assert!(!landed);

        // The rows were kept for the retry rather than thrown away.
        let events = &writer.stats()[0];
        assert_eq!(events.table, "events");
        assert_eq!(events.queued_rows, 50);
        assert_eq!(events.dropped_rows, 0);
        assert!(events.failing);
    }

    #[tokio::test]
    async fn an_outage_holds_the_buffer_to_its_bound() {
        let cfg = WriterCfg {
            max_buffer_rows: 100,
            ..WriterCfg::default()
        };
        let writer = EventWriter::spawn(dead_clickhouse(), cfg);

        for _ in 0..1_000 {
            writer.push_event(EventRow::default());
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), writer.flush()).await;

        let events = &writer.stats()[0];
        assert!(events.queued_rows <= 100, "{events:?}");
        assert_eq!(events.queued_rows + events.dropped_rows, 1_000);
        assert_eq!(events.written_rows, 0);
    }

    #[tokio::test]
    async fn shutdown_honours_its_deadline_and_counts_what_it_lost() {
        let writer = EventWriter::spawn(dead_clickhouse(), WriterCfg::default());
        for _ in 0..10 {
            writer.push_event(EventRow::default());
        }

        let started = std::time::Instant::now();
        let lost = tokio::time::timeout(
            Duration::from_secs(10),
            writer.clone().shutdown(Duration::from_millis(600)),
        )
        .await
        .expect("shutdown must end at its deadline");

        assert_eq!(lost, 10);
        assert!(started.elapsed() < Duration::from_secs(5));
        // The lanes are gone; a late push is refused rather than queued.
        writer.push_event(EventRow::default());
        assert!(!writer.flush().await);
    }
}
