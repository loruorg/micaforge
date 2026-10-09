//! What one sync run fetches, decided by pure functions.
//!
//! Every rule about *which days* lives here, apart from the network, so each
//! one is a unit test: the days Google is still revising, the first month a
//! new connection reads, the backfill toward sixteen months, the per-run
//! budget that keeps one run short, and the paging through Google's 25 000-row
//! pages up to its daily ceiling.
//!
//! ## Days are Google's days
//!
//! Search Console reports whole days in Pacific time, so "today" is today in
//! `America/Los_Angeles` ([`google_today`]), whatever the site's own zone.
//!
//! ## The window of one run
//!
//! A connection remembers two dates: `synced_through` (the newest day fetched)
//! and `backfilled_from` (the oldest). A run plans, in this order:
//!
//! 1. **Forward**: from `synced_through` minus the provisional days to today.
//!    Google revises the last [`PROVISIONAL_DAYS`] days, so the days that were
//!    provisional when last fetched are fetched again, and today's partial
//!    figures with them. A first run has no `synced_through` and reads the last
//!    [`FIRST_DAYS`] days instead, so a new connection fills the usual 30-day
//!    view on its first pass.
//! 2. **Backward**: from the day before `backfilled_from` toward
//!    `backfill_until` (sixteen months before the property was chosen, the
//!    oldest data Google keeps), newest first.
//!
//! Both share one budget of [`DAYS_PER_RUN`] days. A run that leaves work over
//! says so ([`Plan::more`]) and the job queues a follow-up for that site, so a
//! sixteen-month backfill is a handful of short runs rather than one long one.
//! Each direction is cut into chunks of at most [`CHUNK_DAYS`] days; a chunk is
//! the unit of progress, recorded only once every row of it is written.

use chrono::{DateTime, Duration, Months, NaiveDate, Utc};

/// Days Google may still revise, counting today.
pub const PROVISIONAL_DAYS: i64 = 3;

/// Days a new connection reads on its first run, counting today.
pub const FIRST_DAYS: i64 = 30;

/// Most days one run fetches, forward and backward together.
pub const DAYS_PER_RUN: i64 = 62;

/// Most days in one chunk.
pub const CHUNK_DAYS: i64 = 7;

/// How far back a backfill reaches: Google keeps sixteen months.
pub const BACKFILL_MONTHS: u32 = 16;

/// Most rows read for one grain over one request window. Google returns at
/// most fifty thousand rows a day for one search type, the busiest first, so
/// asking past that only spends requests on pages that come back empty.
pub const MAX_ROWS_PER_WINDOW: u32 = 50_000;

/// Today in Google's calendar.
pub fn google_today(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&chrono_tz::America::Los_Angeles)
        .date_naive()
}

/// The oldest day a backfill started on `today` will reach.
pub fn backfill_until(today: NaiveDate) -> NaiveDate {
    today
        .checked_sub_months(Months::new(BACKFILL_MONTHS))
        .unwrap_or(today)
}

/// Which way a chunk moves the connection's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Toward today: moves `synced_through`.
    Forward,
    /// Toward the past: moves `backfilled_from`.
    Backward,
}

/// A run of consecutive days, both ends included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// The first day.
    pub start: NaiveDate,
    /// The last day.
    pub end: NaiveDate,
    /// Which date it advances.
    pub direction: Direction,
}

impl Chunk {
    /// Every day in it, oldest first.
    pub fn days(&self) -> Vec<NaiveDate> {
        let n = (self.end - self.start).num_days();
        (0..=n)
            .filter_map(|i| self.start.checked_add_signed(Duration::days(i)))
            .collect()
    }
}

/// Where a connection's window stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// The oldest day fetched so far.
    pub backfilled_from: Option<NaiveDate>,
    /// The newest day fetched so far.
    pub synced_through: Option<NaiveDate>,
    /// The oldest day to reach.
    pub backfill_until: NaiveDate,
}

impl Window {
    /// The window after a chunk was written.
    pub fn after(self, chunk: &Chunk) -> Window {
        let older = |a: Option<NaiveDate>, b: NaiveDate| Some(a.map_or(b, |a| a.min(b)));
        let newer = |a: Option<NaiveDate>, b: NaiveDate| Some(a.map_or(b, |a| a.max(b)));
        match chunk.direction {
            // A forward chunk also widens the start on the very first run.
            Direction::Forward => Window {
                synced_through: newer(self.synced_through, chunk.end),
                backfilled_from: older(self.backfilled_from, chunk.start),
                ..self
            },
            Direction::Backward => Window {
                backfilled_from: older(self.backfilled_from, chunk.start),
                ..self
            },
        }
    }
}

/// What one run fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The chunks, in the order to fetch them.
    pub chunks: Vec<Chunk>,
    /// True when work is left for another run.
    pub more: bool,
}

/// Plan one run. See the module notes.
pub fn plan(today: NaiveDate, window: Window, budget: i64) -> Plan {
    let mut left = budget.max(1);
    let mut chunks = Vec::new();
    let mut more = false;

    // Forward, oldest first, so `synced_through` only ever moves on.
    let forward_start = match window.synced_through {
        Some(through) => (through - Duration::days(PROVISIONAL_DAYS - 1)).min(today),
        None => today - Duration::days(FIRST_DAYS - 1),
    }
    .max(window.backfill_until);
    let forward_days = (today - forward_start).num_days() + 1;
    let taken = forward_days.min(left);
    if taken < forward_days {
        more = true;
    }
    let forward_end = forward_start + Duration::days(taken - 1);
    chunks.extend(cut(forward_start, forward_end, Direction::Forward));
    left -= taken;

    // Backward, newest first, so `backfilled_from` only ever moves back. The
    // first run's forward chunk sets where the backfill begins.
    let oldest = window
        .backfilled_from
        .unwrap_or(forward_start)
        .min(forward_start);
    if oldest > window.backfill_until {
        let end = oldest - Duration::days(1);
        let want = (end - window.backfill_until).num_days() + 1;
        let taken = want.min(left);
        if taken < want {
            more = true;
        }
        if taken > 0 {
            let start = end - Duration::days(taken - 1);
            let mut back = cut(start, end, Direction::Backward);
            back.reverse();
            chunks.extend(back);
        }
    }

    Plan { chunks, more }
}

/// Cut `start..=end` into chunks of at most [`CHUNK_DAYS`], oldest first.
fn cut(start: NaiveDate, end: NaiveDate, direction: Direction) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut at = start;
    while at <= end {
        let last = (at + Duration::days(CHUNK_DAYS - 1)).min(end);
        out.push(Chunk {
            start: at,
            end: last,
            direction,
        });
        at = last + Duration::days(1);
    }
    out
}

/// The pages to ask for after a page of `got` rows, when `so_far` rows (that
/// page included) have been read: the next `startRow`, or `None` to stop.
///
/// Stops on a short page (Google has no more) and at the window ceiling.
pub fn next_page(got: u32, so_far: u32, row_limit: u32) -> Option<u32> {
    (got >= row_limit && so_far < MAX_ROWS_PER_WINDOW).then_some(so_far)
}

/// Read every page of one request window through `fetch(start_row)`.
///
/// Returns the rows, and whether the ceiling cut the window short (said in
/// the job's log, because a site that busy loses its long tail of queries).
pub async fn page_through<T, E, F, Fut>(row_limit: u32, mut fetch: F) -> Result<(Vec<T>, bool), E>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<T>, E>>,
{
    let mut rows: Vec<T> = Vec::new();
    let mut start_row = 0u32;
    loop {
        let page = fetch(start_row).await?;
        let got = page.len() as u32;
        rows.extend(page);
        let so_far = rows.len() as u32;
        match next_page(got, so_far, row_limit) {
            Some(next) => start_row = next,
            None => {
                let capped = got >= row_limit && so_far >= MAX_ROWS_PER_WINDOW;
                return Ok((rows, capped));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn google_days_are_pacific_days() {
        // 05:00 UTC on 2 October is still 1 October in California.
        let now = Utc.with_ymd_and_hms(2026, 10, 2, 5, 0, 0).unwrap();
        assert_eq!(google_today(now), d("2026-10-01"));
        assert_eq!(backfill_until(d("2026-10-01")), d("2025-06-01"));
    }

    #[test]
    fn a_first_run_reads_the_last_month_then_starts_the_backfill() {
        let today = d("2026-10-09");
        let window = Window {
            backfilled_from: None,
            synced_through: None,
            backfill_until: backfill_until(today),
        };
        let p = plan(today, window, DAYS_PER_RUN);
        let forward: Vec<&Chunk> = p
            .chunks
            .iter()
            .filter(|c| c.direction == Direction::Forward)
            .collect();
        assert_eq!(
            forward.first().unwrap().start,
            d("2026-09-10"),
            "thirty days counting today"
        );
        assert_eq!(forward.last().unwrap().end, today);
        // The rest of the budget goes to the backfill, newest first.
        let back: Vec<&Chunk> = p
            .chunks
            .iter()
            .filter(|c| c.direction == Direction::Backward)
            .collect();
        assert_eq!(back.first().unwrap().end, d("2026-09-09"));
        assert!(back.windows(2).all(|w| w[1].end < w[0].start));
        let days: i64 = p.chunks.iter().map(|c| c.days().len() as i64).sum();
        assert_eq!(days, DAYS_PER_RUN);
        assert!(p.more, "sixteen months do not fit in one run");
        assert!(p.chunks.iter().all(|c| c.days().len() as i64 <= CHUNK_DAYS));
    }

    #[test]
    fn the_provisional_days_are_fetched_again_every_run() {
        let today = d("2026-10-09");
        let window = Window {
            backfilled_from: Some(d("2025-06-09")),
            synced_through: Some(today),
            backfill_until: d("2025-06-09"),
        };
        let p = plan(today, window, DAYS_PER_RUN);
        assert_eq!(
            p.chunks,
            vec![Chunk {
                start: d("2026-10-07"),
                end: today,
                direction: Direction::Forward
            }],
            "the last three days, counting today, and nothing else"
        );
        assert!(!p.more);

        // A sync last done four days ago re-reads the days that were
        // provisional then, and every day since.
        let behind = Window {
            synced_through: Some(d("2026-10-05")),
            ..window
        };
        let p = plan(today, behind, DAYS_PER_RUN);
        assert_eq!(p.chunks[0].start, d("2026-10-03"));
        assert_eq!(p.chunks.last().unwrap().end, today);
    }

    #[test]
    fn a_long_absence_is_caught_up_over_several_runs_without_a_gap() {
        let today = d("2026-10-09");
        let mut window = Window {
            backfilled_from: Some(d("2025-06-09")),
            synced_through: Some(d("2026-06-01")),
            backfill_until: d("2025-06-09"),
        };
        let mut runs = 0;
        loop {
            let p = plan(today, window, DAYS_PER_RUN);
            for chunk in &p.chunks {
                let next = window.after(chunk);
                if chunk.direction == Direction::Forward {
                    // Never a gap: each forward chunk starts at or before the
                    // day after the newest day already fetched.
                    let through = window.synced_through.unwrap();
                    assert!(chunk.start <= through + Duration::days(1));
                }
                window = next;
            }
            runs += 1;
            if !p.more {
                break;
            }
            assert!(runs < 10);
        }
        assert_eq!(window.synced_through, Some(today));
        assert!(runs > 1);
    }

    #[test]
    fn the_backfill_stops_at_sixteen_months() {
        let today = d("2026-10-09");
        let until = backfill_until(today);
        let mut window = Window {
            backfilled_from: None,
            synced_through: None,
            backfill_until: until,
        };
        let mut runs = 0;
        loop {
            let p = plan(today, window, DAYS_PER_RUN);
            for chunk in &p.chunks {
                assert!(chunk.start >= until, "never older than Google keeps");
                window = window.after(chunk);
            }
            runs += 1;
            if !p.more {
                break;
            }
        }
        assert_eq!(window.backfilled_from, Some(until));
        // Sixteen months of about 487 days at 62 a run, plus the provisional
        // days each run re-reads.
        assert!((8..=10).contains(&runs), "{runs} runs");
        // Once complete, a run only re-reads the provisional days.
        let steady = plan(today, window, DAYS_PER_RUN);
        assert_eq!(steady.chunks.len(), 1);
        assert!(!steady.more);
    }

    #[test]
    fn paging_stops_on_a_short_page_or_at_the_ceiling() {
        assert_eq!(next_page(25_000, 25_000, 25_000), Some(25_000));
        assert_eq!(
            next_page(24_999, 24_999, 25_000),
            None,
            "a short page is the last"
        );
        assert_eq!(next_page(25_000, 50_000, 25_000), None, "the daily ceiling");
        assert_eq!(next_page(0, 0, 25_000), None);
    }

    #[tokio::test]
    async fn every_page_is_read_in_order() {
        // Three pages of a 4-row limit: 4, 4, then 1.
        let all: Vec<u32> = (0..9).collect();
        let mut asked = Vec::new();
        let (rows, capped) = page_through::<u32, (), _, _>(4, |start| {
            asked.push(start);
            let page: Vec<u32> = all.iter().copied().skip(start as usize).take(4).collect();
            async move { Ok(page) }
        })
        .await
        .unwrap();
        assert_eq!(rows, all);
        assert_eq!(asked, vec![0, 4, 8]);
        assert!(!capped);

        // A failure on any page fails the window.
        let failed = page_through::<u32, &str, _, _>(4, |start| async move {
            if start == 0 {
                Ok(vec![1, 2, 3, 4])
            } else {
                Err("google")
            }
        })
        .await;
        assert_eq!(failed, Err("google"));
    }
}
