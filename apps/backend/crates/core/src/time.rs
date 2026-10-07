//! Time ranges, buckets and comparison periods.
//!
//! Every analytics read starts here. The query string carries `range=7d` or a
//! pair of ISO timestamps; this module turns either into a
//! [`TimeRange`] with UTC bounds and a remembered zone,
//! and nothing downstream is allowed to reinterpret it.
//!
//! The single idea worth stating: named ranges are **local calendar** windows,
//! not offsets from now. `7d` is the last seven days of the site's own calendar
//! including today, so the chart has seven day-shaped bars and the leftmost one
//! is a whole day. A rolling `now - 168h` would put a partial day at each end
//! and make every daily comparison slightly wrong.

use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::error::{Error, Result};
use crate::types::{Compare, Granularity, TimeRange};

/// Used when a caller supplies neither a named range nor explicit bounds.
///
/// A day is the honest default for a dashboard opened after a deploy, which is
/// the moment `PRODUCT.md` says this audience opens it.
const DEFAULT_RANGE: &str = "24h";

/// Most buckets one series may hold.
///
/// A day of minutes is 1,440, the densest series a chart can usefully draw, so
/// every ordinary request fits. `range=all&granularity=minute` asks for close to
/// thirty million, which is a ClickHouse query and a JSON document nobody can
/// use; [`fit_granularity`] coarsens such a request instead of running it.
pub const MAX_BUCKETS: i64 = 1_500;

/// Resolve a range from the query string.
///
/// Precedence: explicit `start` (and optional `end`) wins; otherwise the named
/// `range`; otherwise a rolling 24 hours.
///
/// Accepted named ranges are the contract's list (`today`, `yesterday`, `24h`,
/// `7d`, `30d`, `90d`, `12mo`, `mtd`, `ytd`, `all`), plus the general `<n>h`,
/// `<n>d` and `<n>mo` forms, which cost nothing to
/// support and save a caller from a two-request workaround.
///
/// Accepted timestamp forms for `start` and `end`:
///
/// - RFC-3339 with an offset (`2026-08-21T09:00:00Z`), taken at face value.
/// - A naive datetime (`2026-08-21T09:00:00`), read in `tz`.
/// - A bare date (`2026-08-21`), read in `tz`. As `start` it means that day's
///   midnight; as `end` it means the **following** midnight, so a caller asking
///   for `start=2026-08-01&end=2026-08-31` gets all of the 31st rather than none
///   of it. Getting this wrong is the classic off-by-one-day analytics bug.
pub fn resolve_range(
    range: Option<&str>,
    start: Option<&str>,
    end: Option<&str>,
    tz: Tz,
) -> Result<TimeRange> {
    let now = Utc::now();

    let start_raw = start.map(str::trim).filter(|s| !s.is_empty());
    let end_raw = end.map(str::trim).filter(|s| !s.is_empty());

    if let Some(raw) = start_raw {
        let start_at = parse_bound(raw, tz, Bound::Start, "start")?;
        let end_at = match end_raw {
            Some(raw) => parse_bound(raw, tz, Bound::End, "end")?,
            // A start with no end means "since then", which is what an open
            // date picker shows before the second click.
            None => now,
        };
        if end_at <= start_at {
            return Err(Error::validation("end", "must be after `start`"));
        }
        return Ok(TimeRange::new(start_at, end_at, tz));
    }

    if end_raw.is_some() {
        return Err(Error::validation(
            "start",
            "is required when `end` is given",
        ));
    }

    named_range(range.unwrap_or(DEFAULT_RANGE).trim(), tz, now)
}

/// Which end of the window a bare date is being read as.
#[derive(Clone, Copy)]
enum Bound {
    Start,
    End,
}

fn parse_bound(raw: &str, tz: Tz, bound: Bound, field: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Naive datetime, in the site's zone. Both the `T` and space separators
    // appear in the wild, and seconds are frequently omitted.
    for fmt in [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Ok(to_utc(tz, naive));
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let date = match bound {
            Bound::Start => date,
            // Exclusive end: the day the caller named has to be included whole.
            Bound::End => date
                .succ_opt()
                .ok_or_else(|| Error::validation(field, "date is out of range"))?,
        };
        return Ok(to_utc(tz, midnight(date)));
    }

    Err(Error::validation(
        field,
        "must be an ISO-8601 date or timestamp",
    ))
}

fn named_range(name: &str, tz: Tz, now: DateTime<Utc>) -> Result<TimeRange> {
    let local = now.with_timezone(&tz);
    let today = local.date_naive();

    let range = match name {
        "today" => TimeRange::new(start_of_day(tz, today), now, tz),
        "yesterday" => {
            let yesterday = today
                .pred_opt()
                .ok_or_else(|| Error::validation("range", "date is out of range"))?;
            TimeRange::new(start_of_day(tz, yesterday), start_of_day(tz, today), tz)
        }
        // The only genuinely rolling window, and the one realtime-adjacent view
        // where a partial first bucket is what the reader wants.
        "24h" => TimeRange::new(now - Duration::hours(24), now, tz),
        "mtd" => {
            let first = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)
                .ok_or_else(|| Error::validation("range", "date is out of range"))?;
            TimeRange::new(start_of_day(tz, first), now, tz)
        }
        "ytd" => {
            let first = NaiveDate::from_ymd_opt(today.year(), 1, 1)
                .ok_or_else(|| Error::validation("range", "date is out of range"))?;
            TimeRange::new(start_of_day(tz, first), now, tz)
        }
        // Bounded at the epoch rather than left open: an unbounded scan defeats
        // the partition pruning every other query relies on.
        "all" => TimeRange::new(DateTime::<Utc>::UNIX_EPOCH, now, tz),
        other => return relative_range(other, tz, now),
    };

    Ok(range)
}

/// `7d`, `90d`, `12mo`, `6h`, and the contract's named members of those families.
fn relative_range(name: &str, tz: Tz, now: DateTime<Utc>) -> Result<TimeRange> {
    let invalid = || {
        Error::validation(
            "range",
            format!(
                "unknown range `{name}`; expected today, yesterday, 24h, 7d, 30d, 90d, \
                 12mo, mtd, ytd, all, or a form like 14d"
            ),
        )
    };

    let (digits, unit) = if let Some(d) = name.strip_suffix("mo") {
        (d, "mo")
    } else if let Some(d) = name.strip_suffix('d') {
        (d, "d")
    } else if let Some(d) = name.strip_suffix('h') {
        (d, "h")
    } else {
        return Err(invalid());
    };

    let n: i64 = digits.parse().map_err(|_| invalid())?;
    if n <= 0 || n > 3650 {
        return Err(invalid());
    }

    let local = now.with_timezone(&tz);
    let today = local.date_naive();

    let start = match unit {
        // Hours stay rolling: nobody asking for "6h" wants it rounded to a day.
        "h" => return Ok(TimeRange::new(now - Duration::hours(n), now, tz)),
        // `7d` includes today, so it starts six days back.
        "d" => today - Duration::days(n - 1),
        // `12mo` likewise includes the current month.
        _ => {
            let first_of_month =
                NaiveDate::from_ymd_opt(today.year(), today.month(), 1).ok_or_else(invalid)?;
            first_of_month
                .checked_sub_months(Months::new((n - 1) as u32))
                .ok_or_else(invalid)?
        }
    };

    Ok(TimeRange::new(start_of_day(tz, start), now, tz))
}

/// Choose a bucket width that gives a readable number of points.
///
/// The thresholds aim at roughly 24 to 180 buckets: fewer and the shape of the
/// data disappears, more and the chart is a solid block of ink at any realistic
/// width.
pub fn auto_granularity(range: &TimeRange) -> Granularity {
    let hours = range.duration().num_hours();

    if hours <= 3 {
        Granularity::Minute
    } else if hours <= 24 * 4 {
        Granularity::Hour
    } else if hours <= 24 * 90 {
        Granularity::Day
    } else if hours <= 24 * 180 {
        Granularity::Week
    } else {
        Granularity::Month
    }
}

/// The granularity a series is computed at: the one requested, or the finest
/// coarser one that keeps the series within [`MAX_BUCKETS`].
///
/// Coarsened rather than refused because the request is not wrong, only too
/// fine for its window, and the dashboard's granularity picker and its range
/// picker are two separate controls. The bucket timestamps in the series show
/// the width actually used. A window too
/// long even for months (only an explicit `start` centuries ago can do that)
/// stays at months, the coarsest width there is.
pub fn fit_granularity(range: &TimeRange, requested: Granularity) -> Granularity {
    let mut granularity = requested;
    while bucket_count(range, granularity) > MAX_BUCKETS {
        match coarser(granularity) {
            Some(next) => granularity = next,
            None => break,
        }
    }
    granularity
}

/// How many buckets `range` holds at `granularity`, rounding a partial one up.
pub fn bucket_count(range: &TimeRange, granularity: Granularity) -> i64 {
    let span = range.duration().num_seconds().max(0);
    let width = granularity.approx_duration().num_seconds().max(1);
    (span + width - 1) / width
}

/// The next wider bucket, or `None` past months.
fn coarser(granularity: Granularity) -> Option<Granularity> {
    match granularity {
        Granularity::Minute => Some(Granularity::Hour),
        Granularity::Hour => Some(Granularity::Day),
        Granularity::Day => Some(Granularity::Week),
        Granularity::Week => Some(Granularity::Month),
        Granularity::Month => None,
    }
}

/// The window to compare `range` against.
///
/// [`Compare::Previous`] is the equal-length window ending exactly where this
/// one starts, so the two never overlap. [`Compare::Year`] shifts by calendar
/// months rather than 365 days, so "August" compares with "August" even across a
/// leap year.
pub fn previous_period(range: &TimeRange, compare: Compare) -> Option<TimeRange> {
    match compare {
        Compare::None => None,
        Compare::Previous => {
            let span = range.duration();
            Some(TimeRange::new(range.start - span, range.start, range.tz))
        }
        Compare::Year => {
            let start = range.start.checked_sub_months(Months::new(12))?;
            let end = range.end.checked_sub_months(Months::new(12))?;
            Some(TimeRange::new(start, end, range.tz))
        }
    }
}

/// The ClickHouse expression that assigns a row to its bucket.
///
/// Every variant converts to the site's zone first, so day, week and month
/// boundaries fall where the operator's calendar says they do. Week and month
/// are wrapped back into a `DateTime` because ClickHouse's `toStartOfWeek` and
/// `toStartOfMonth` return a `Date`, and a series whose bucket type changes with
/// its granularity is a trap for every client that reads it.
///
/// Weeks start on Monday (mode 1), matching ISO-8601 and the rest of the
/// product.
///
/// The zone name comes from a parsed [`Tz`], so it is one of a fixed set of IANA
/// identifiers and never caller-supplied text.
pub fn bucket_expr(g: Granularity, tz: Tz) -> String {
    let name = tz.name();
    match g {
        Granularity::Minute => format!("toStartOfMinute(toTimeZone(timestamp, '{name}'))"),
        Granularity::Hour => format!("toStartOfHour(toTimeZone(timestamp, '{name}'))"),
        Granularity::Day => format!("toStartOfDay(toTimeZone(timestamp, '{name}'))"),
        Granularity::Week => {
            format!("toDateTime(toStartOfWeek(toTimeZone(timestamp, '{name}'), 1), '{name}')")
        }
        Granularity::Month => {
            format!("toDateTime(toStartOfMonth(toTimeZone(timestamp, '{name}')), '{name}')")
        }
    }
}

fn midnight(date: NaiveDate) -> NaiveDateTime {
    date.and_hms_opt(0, 0, 0)
        .expect("00:00:00 is a valid time on every date")
}

fn start_of_day(tz: Tz, date: NaiveDate) -> DateTime<Utc> {
    to_utc(tz, midnight(date))
}

/// Convert a local wall-clock time to UTC, surviving both DST edges.
///
/// A spring-forward gap has no local midnight at all in a few zones (Santiago,
/// Beirut), and an autumn overlap has two. Taking the earliest of an ambiguous
/// pair, and stepping forward to the first real instant across a gap, keeps a
/// range contiguous with its neighbour instead of dropping or double-counting an
/// hour once a year.
fn to_utc(tz: Tz, naive: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&naive).earliest() {
        Some(dt) => dt.with_timezone(&Utc),
        None => {
            // The local time does not exist. Walk forward in minutes to the
            // first instant that does; a DST gap is at most a couple of hours.
            for minutes in 1..=180 {
                let shifted = naive + Duration::minutes(minutes);
                if let Some(dt) = tz.from_local_datetime(&shifted).earliest() {
                    return dt.with_timezone(&Utc);
                }
            }
            // Unreachable for any real zone; treating the wall clock as UTC is
            // wrong by hours at worst and never panics.
            Utc.from_utc_datetime(&naive)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Tz::{Europe__Lisbon, Pacific__Auckland, UTC};

    #[test]
    fn explicit_bounds_win_and_are_read_in_the_site_zone() {
        let r = resolve_range(
            Some("30d"),
            Some("2026-08-01T00:00:00"),
            Some("2026-08-02T00:00:00"),
            Europe__Lisbon,
        )
        .unwrap();
        // Lisbon is UTC+1 in August, so local midnight is 23:00 the day before.
        assert_eq!(r.start.to_rfc3339(), "2026-07-31T23:00:00+00:00");
        assert_eq!(r.duration().num_hours(), 24);
        assert_eq!(r.tz, Europe__Lisbon);
    }

    #[test]
    fn a_bare_end_date_includes_that_whole_day() {
        let r = resolve_range(None, Some("2026-08-01"), Some("2026-08-31"), UTC).unwrap();
        assert_eq!(r.end.to_rfc3339(), "2026-09-01T00:00:00+00:00");
        assert_eq!(r.days(), 31);
    }

    #[test]
    fn rfc3339_bounds_are_taken_at_face_value() {
        let r = resolve_range(
            None,
            Some("2026-08-21T09:00:00+02:00"),
            Some("2026-08-21T11:00:00Z"),
            Pacific__Auckland,
        )
        .unwrap();
        assert_eq!(r.start.to_rfc3339(), "2026-08-21T07:00:00+00:00");
        assert_eq!(r.duration().num_hours(), 4);
    }

    #[test]
    fn bad_bounds_are_named_not_guessed() {
        assert_eq!(
            resolve_range(None, Some("last tuesday"), None, UTC)
                .unwrap_err()
                .field(),
            Some("start")
        );
        assert_eq!(
            resolve_range(None, None, Some("2026-08-21"), UTC)
                .unwrap_err()
                .field(),
            Some("start")
        );
        assert_eq!(
            resolve_range(None, Some("2026-08-21"), Some("2026-08-01"), UTC)
                .unwrap_err()
                .field(),
            Some("end")
        );
        assert_eq!(
            resolve_range(Some("fortnight"), None, None, UTC)
                .unwrap_err()
                .field(),
            Some("range")
        );
    }

    #[test]
    fn named_ranges_are_local_calendar_windows() {
        let tz = Pacific__Auckland;

        let today = resolve_range(Some("today"), None, None, tz).unwrap();
        assert_eq!(
            today.start.with_timezone(&tz).time().to_string(),
            "00:00:00",
            "`today` starts at local midnight, not UTC midnight"
        );

        let yesterday = resolve_range(Some("yesterday"), None, None, tz).unwrap();
        assert_eq!(yesterday.end, today.start, "the two must be contiguous");
        assert_eq!(yesterday.duration().num_hours(), 24);

        // `7d` covers seven local days, the last of them still running.
        let week = resolve_range(Some("7d"), None, None, tz).unwrap();
        assert_eq!(week.days(), 7);
        assert_eq!(week.start.with_timezone(&tz).time().to_string(), "00:00:00");

        // `24h` is the one rolling window.
        let day = resolve_range(Some("24h"), None, None, tz).unwrap();
        assert_eq!(day.duration().num_hours(), 24);

        assert!(resolve_range(Some("mtd"), None, None, tz).unwrap().days() >= 1);
        assert!(resolve_range(Some("ytd"), None, None, tz).unwrap().days() >= 1);
        assert_eq!(
            resolve_range(Some("all"), None, None, tz).unwrap().start,
            DateTime::<Utc>::UNIX_EPOCH
        );
        // The general forms.
        assert_eq!(
            resolve_range(Some("14d"), None, None, tz).unwrap().days(),
            14
        );
        assert_eq!(
            resolve_range(Some("6h"), None, None, tz)
                .unwrap()
                .duration()
                .num_hours(),
            6
        );
    }

    #[test]
    fn twelve_months_covers_twelve_calendar_months() {
        let r = resolve_range(Some("12mo"), None, None, UTC).unwrap();
        assert!(r.days() >= 335 && r.days() <= 366, "got {} days", r.days());
        assert_eq!(r.start.day(), 1, "starts on the first of a month");
        assert_eq!(auto_granularity(&r), Granularity::Month);
    }

    #[test]
    fn no_arguments_falls_back_to_the_default_range() {
        let r = resolve_range(None, None, None, UTC).unwrap();
        assert_eq!(r.duration().num_hours(), 24);
    }

    #[test]
    fn granularity_scales_with_the_window() {
        let base = Utc::now();
        let g = |hours: i64| {
            auto_granularity(&TimeRange::new(base - Duration::hours(hours), base, UTC))
        };
        assert_eq!(g(1), Granularity::Minute);
        assert_eq!(g(24), Granularity::Hour);
        assert_eq!(g(24 * 7), Granularity::Day);
        assert_eq!(g(24 * 90), Granularity::Day);
        assert_eq!(g(24 * 120), Granularity::Week);
        assert_eq!(g(24 * 365), Granularity::Month);
    }

    #[test]
    fn a_series_never_exceeds_the_bucket_cap() {
        // The defect: every minute since 1970.
        let all = resolve_range(Some("all"), None, None, UTC).unwrap();
        let fitted = fit_granularity(&all, Granularity::Minute);
        assert_eq!(fitted, Granularity::Month);
        assert!(bucket_count(&all, fitted) <= MAX_BUCKETS);

        // An ordinary request is left exactly as asked.
        let day = resolve_range(None, Some("2026-08-21"), Some("2026-08-21"), UTC).unwrap();
        assert_eq!(
            fit_granularity(&day, Granularity::Minute),
            Granularity::Minute
        );
        let quarter = resolve_range(Some("90d"), None, None, UTC).unwrap();
        assert_eq!(
            fit_granularity(&quarter, Granularity::Hour),
            Granularity::Day
        );
        assert_eq!(
            fit_granularity(&quarter, Granularity::Day),
            Granularity::Day
        );
        assert_eq!(
            fit_granularity(&quarter, Granularity::Month),
            Granularity::Month
        );

        // Coarsened one step at a time, never further than needed.
        let week = resolve_range(None, Some("2026-08-01"), Some("2026-08-07"), UTC).unwrap();
        assert_eq!(
            fit_granularity(&week, Granularity::Minute),
            Granularity::Hour
        );

        // Even a window too long for months stops at months.
        let ancient = resolve_range(None, Some("0001-01-01"), None, UTC).unwrap();
        assert_eq!(
            fit_granularity(&ancient, Granularity::Minute),
            Granularity::Month
        );

        // Every automatic choice is already within the cap.
        for range in [&all, &day, &quarter, &week] {
            assert!(bucket_count(range, auto_granularity(range)) <= MAX_BUCKETS);
        }
    }

    #[test]
    fn comparison_windows_abut_or_shift_a_year() {
        let r = resolve_range(None, Some("2026-08-08"), Some("2026-08-15"), UTC).unwrap();

        assert!(previous_period(&r, Compare::None).is_none());

        let prev = previous_period(&r, Compare::Previous).unwrap();
        assert_eq!(prev.end, r.start, "no overlap and no gap");
        assert_eq!(prev.duration(), r.duration());

        // The bare end date includes the 15th whole, so the exclusive bound is
        // the 16th, and a year earlier it is the 16th too.
        let year = previous_period(&r, Compare::Year).unwrap();
        assert_eq!(year.start.to_rfc3339(), "2025-08-08T00:00:00+00:00");
        assert_eq!(year.end.to_rfc3339(), "2025-08-16T00:00:00+00:00");
    }

    #[test]
    fn bucket_expressions_carry_the_zone() {
        assert_eq!(
            bucket_expr(Granularity::Day, Europe__Lisbon),
            "toStartOfDay(toTimeZone(timestamp, 'Europe/Lisbon'))"
        );
        assert_eq!(
            bucket_expr(Granularity::Week, UTC),
            "toDateTime(toStartOfWeek(toTimeZone(timestamp, 'UTC'), 1), 'UTC')"
        );
        assert!(bucket_expr(Granularity::Month, Pacific__Auckland).contains("toStartOfMonth"));
        // Zone names come from a parsed Tz, so no expression can carry a quote.
        for g in [
            Granularity::Minute,
            Granularity::Hour,
            Granularity::Day,
            Granularity::Week,
            Granularity::Month,
        ] {
            assert_eq!(bucket_expr(g, Europe__Lisbon).matches('\'').count() % 2, 0);
        }
    }

    #[test]
    fn a_dst_gap_still_resolves_to_a_real_instant() {
        // Havana changes its clocks at midnight, so on a transition date the
        // local 00:00 a bare date implies can be missing or doubled.
        let r = resolve_range(
            None,
            Some("2026-03-08"),
            Some("2026-03-09"),
            chrono_tz::Tz::America__Havana,
        );
        assert!(r.is_ok(), "a DST gap must not fail a range");
        let r = r.unwrap();
        assert!(r.start < r.end);
    }
}
