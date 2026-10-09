/**
 * Ninety days of one monitor, one bar per day.
 *
 * Each day falls into one of four groups, and each group has its own HEIGHT
 * as well as its own tone, so the row reads in greyscale and to a reader who
 * cannot tell red from green: a full bar is a day when every check passed, a
 * shorter bar is a day with some failures, a short bar is a day that was
 * mostly down, and a low grey stub is a day with no checks at all. The eye
 * reads a dip as trouble without being taught.
 *
 * Every bar carries its own spoken label ("12 Oct: 288 checks, 0 failed"), so
 * a screen reader can walk the row day by day. The row is also one keyboard
 * stop: the arrow keys move along it and the line under the bars prints the
 * day in focus. Pointing at a bar prints the same line. Ninety separate tab
 * stops per monitor would make the page impossible to cross by keyboard.
 */
import { useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
import type { PublicDay } from "./words";

export type DayLevel = "up" | "some" | "most" | "none";

export const LEVEL_WORD: Record<DayLevel, string> = {
  up: "All up",
  some: "Some failures",
  most: "Mostly down",
  none: "No data",
};

export const LEVELS: DayLevel[] = ["up", "some", "most", "none"];

export function levelOf(d: PublicDay): DayLevel {
  if (!d.checks) return "none";
  if (!d.failures) return "up";
  return d.failures / d.checks > 0.5 ? "most" : "some";
}

/** "YYYY-MM-DD" is a UTC day, so it is printed in UTC: the day the server counted. */
const dayFmt = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", timeZone: "UTC" });

function dayWord(date: string): string {
  const t = Date.parse(`${date}T00:00:00Z`);
  return Number.isFinite(t) ? dayFmt.format(t) : date;
}

export function dayLabel(d: PublicDay): string {
  if (!d.checks) return `${dayWord(d.date)}: no checks`;
  return `${dayWord(d.date)}: ${d.checks} ${d.checks === 1 ? "check" : "checks"}, ${d.failures} failed`;
}

export function DayBars({ days, name }: { days: PublicDay[]; name: string }) {
  const [at, setAt] = useState<number | null>(null);
  const n = days.length;

  const pick = (e: PointerEvent<HTMLDivElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    if (box.width <= 0 || !n) return;
    const i = Math.floor(((e.clientX - box.left) / box.width) * n);
    setAt(Math.max(0, Math.min(n - 1, i)));
  };

  const keys = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!n) return;
    const cur = at ?? n - 1;
    let next: number | null = cur;
    if (e.key === "ArrowLeft") next = Math.max(0, cur - 1);
    else if (e.key === "ArrowRight") next = Math.min(n - 1, cur + 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    else if (e.key === "Escape") next = null;
    else return;
    e.preventDefault();
    setAt(next);
  };

  return (
    <div className="mf-status-days">
      <div
        className="mf-status-days__bars"
        role="list"
        tabIndex={0}
        aria-label={`${name}, one bar per day for the last ${n} days. Use the arrow keys to read each day.`}
        style={{ gridTemplateColumns: `repeat(${Math.max(1, n)}, minmax(0, 1fr))` }}
        onPointerMove={pick}
        onPointerLeave={() => setAt(null)}
        onFocus={() => setAt((v) => v ?? n - 1)}
        onBlur={() => setAt(null)}
        onKeyDown={keys}
      >
        {days.map((d, i) => (
          <span
            key={d.date}
            role="listitem"
            aria-label={dayLabel(d)}
            className="mf-status-day"
            data-level={levelOf(d)}
            data-active={i === at ? "true" : undefined}
          />
        ))}
      </div>
      <p className="mf-status-days__read" aria-live="polite">
        {at != null && days[at] ? (
          <span>
            {dayLabel(days[at])}. {LEVEL_WORD[levelOf(days[at])]}.
          </span>
        ) : (
          <>
            <span>{n} days ago</span>
            <span>Today</span>
          </>
        )}
      </p>
    </div>
  );
}

/** The key to the four bar heights, drawn with the bars themselves. */
export function DayLegend() {
  return (
    <ul className="mf-status-legend" aria-label="Key to the daily bars">
      {LEVELS.map((l) => (
        <li key={l}>
          <span className="mf-status-day mf-status-legend__bar" data-level={l} aria-hidden="true" />
          {LEVEL_WORD[l]}
        </li>
      ))}
    </ul>
  );
}
