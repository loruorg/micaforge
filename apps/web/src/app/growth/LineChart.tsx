/**
 * A small line chart for the Growth screens: clicks on one short link, and
 * revenue over time.
 *
 * Why not the shared Drum chart: Drum is built around the two audiences (a
 * smooth people line against a stepped AI line, with a scale per series), and
 * neither chart here is about AI. Clicks split into people and bots, and money
 * runs below zero when refunds outweigh sales, which Drum's baselines do not
 * allow. So this chart does three things and no more:
 *
 *   1. ONE SHARED SCALE THAT CAN CROSS ZERO. Refunds are drawn as negative
 *      values under a zero line drawn stronger than the grid, so "money went
 *      out" reads as "below the line".
 *   2. LINE STYLE AS WELL AS COLOUR. Every series is solid or dashed, the
 *      legend swatch repeats that style, and the hover dot is filled for a
 *      solid line and hollow for a dashed one. Colour is never the only cue.
 *   3. A READING YOU CAN REACH BY KEYBOARD. The plot takes focus; the left and
 *      right arrow keys move a reading line across the points and the legend
 *      prints the values at that point. Without a reading, the legend prints
 *      each series' total for the period.
 *
 * The lines are in the DOM on first paint. The one motion is the left-to-right
 * reveal the rest of the product's charts use, and only when the reader has
 * not asked for reduced motion (growth.css).
 */
import type { KeyboardEvent, PointerEvent as ReactPointerEvent } from "react";
import { useEffect, useMemo, useRef, useState } from "react";

export interface LineSeries {
  key: string;
  label: string;
  /** A CSS colour token, e.g. var(--mf-people). */
  color: string;
  dashed?: boolean;
  values: number[];
}

const DAY = 86_400_000;
const LABEL_CHAR = 6.7;

function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(Math.round(el.getBoundingClientRect().width));
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0]?.contentRect.width;
      if (w != null) setWidth(Math.round(w));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width] as const;
}

/** Round tick values that cover [min, max], about `count` steps apart. */
function niceTicks(min: number, max: number, count: number, integer: boolean): number[] {
  let lo = min;
  let hi = max;
  if (hi === lo) hi = lo + 1;
  const raw = (hi - lo) / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const norm = raw / mag;
  let step = (norm <= 1 ? 1 : norm <= 2 ? 2 : norm <= 2.5 ? 2.5 : norm <= 5 ? 5 : 10) * mag;
  if (integer) step = Math.max(1, Math.ceil(step));
  lo = Math.floor(lo / step) * step;
  hi = Math.ceil(hi / step) * step;
  const out: number[] = [];
  for (let v = lo; v <= hi + step / 2; v += step) out.push(Math.round(v / step) * step);
  return out;
}

function timeText(t: number, stepMs: number): string {
  const d = new Date(t);
  return stepMs < DAY
    ? d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString(undefined, { day: "numeric", month: "short" });
}

function timeLong(t: number, stepMs: number): string {
  const d = new Date(t);
  return stepMs < DAY
    ? d.toLocaleString(undefined, { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })
    : d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short", year: "numeric" });
}

export function LineChart({
  times,
  series,
  format,
  formatAxis,
  label,
  integer = false,
  height = 220,
}: {
  times: number[];
  series: LineSeries[];
  /** Writes a value in the legend and the reading. */
  format: (n: number) => string;
  /** Writes a value on the axis, where width is scarce. Defaults to `format`. */
  formatAxis?: (n: number) => string;
  /** What the chart measures, in words, for a screen reader: "Clicks per day". */
  label: string;
  /** Counts: ticks land on whole numbers. */
  integer?: boolean;
  height?: number;
}) {
  const [wrapRef, width] = useWidth<HTMLDivElement>();
  const [at, setAt] = useState<number | null>(null);
  const axis = formatAxis ?? format;
  const n = times.length;
  const stepMs = n > 1 ? times[1] - times[0] : DAY;

  const scale = useMemo(() => {
    let min = 0;
    let max = 0;
    for (const s of series)
      for (const v of s.values) {
        if (!Number.isFinite(v)) continue;
        if (v < min) min = v;
        if (v > max) max = v;
      }
    const ticks = niceTicks(min, max, 4, integer);
    return { ticks, lo: ticks[0], hi: ticks[ticks.length - 1] };
  }, [series, integer]);

  const totals = useMemo(
    () => series.map((s) => s.values.reduce((a, v) => a + (Number.isFinite(v) ? v : 0), 0)),
    [series],
  );

  const longest = Math.max(...scale.ticks.map((t) => axis(t).length), 1);
  const M = { top: 10, right: 12, bottom: 24, left: Math.ceil(longest * LABEL_CHAR) + 12 };
  const innerW = Math.max(60, width - M.left - M.right);
  const innerH = Math.max(60, height - M.top - M.bottom);
  const x = (i: number) => M.left + (n <= 1 ? innerW / 2 : (i / (n - 1)) * innerW);
  const y = (v: number) => M.top + ((scale.hi - v) / (scale.hi - scale.lo || 1)) * innerH;

  const labelCount = Math.max(2, Math.min(n, Math.floor(innerW / 92)));
  const timeIdx = useMemo(() => {
    if (n === 0) return [];
    if (n === 1) return [0];
    const set = new Set<number>();
    for (let j = 0; j < labelCount; j++) set.add(Math.round((j * (n - 1)) / (labelCount - 1)));
    return [...set];
  }, [n, labelCount]);

  const pathOf = (values: number[]) =>
    values
      .map((v, i) => `${i === 0 ? "M" : "L"}${x(i).toFixed(1)},${y(Number.isFinite(v) ? v : 0).toFixed(1)}`)
      .join("");

  const pick = (e: ReactPointerEvent<SVGRectElement>) => {
    if (n === 0) return;
    const box = e.currentTarget.getBoundingClientRect();
    const px = e.clientX - box.left;
    const i = n <= 1 ? 0 : Math.round((px / box.width) * (n - 1));
    setAt(Math.max(0, Math.min(n - 1, i)));
  };

  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (n === 0) return;
    const move: Record<string, (i: number) => number> = {
      ArrowRight: (i) => Math.min(n - 1, i + 1),
      ArrowLeft: (i) => Math.max(0, i - 1),
      Home: () => 0,
      End: () => n - 1,
    };
    if (e.key === "Escape") {
      setAt(null);
      return;
    }
    const f = move[e.key];
    if (!f) return;
    e.preventDefault();
    setAt((cur) => f(cur ?? (e.key === "ArrowLeft" ? n : -1)));
  };

  const summary = `${label}. ${series
    .map((s, i) => `${s.label}: ${format(totals[i])} in this period`)
    .join("; ")}.`;

  return (
    <div className="mf-gr-chart">
      <div
        ref={wrapRef}
        className="mf-gr-chart__plot"
        tabIndex={0}
        role="group"
        aria-label={`${summary} Use the left and right arrow keys to read each point.`}
        onKeyDown={onKey}
        onBlur={() => setAt(null)}
        style={{ height }}
      >
        {width > 0 ? (
          <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true">
            {scale.ticks.map((t) => (
              <g key={t}>
                <line
                  x1={M.left}
                  x2={M.left + innerW}
                  y1={y(t)}
                  y2={y(t)}
                  stroke={t === 0 ? "var(--mf-rule-strong)" : "var(--mf-rule-minor)"}
                  strokeWidth={1}
                  shapeRendering="crispEdges"
                />
                <text className="mf-gr-chart__axis" x={M.left - 8} y={y(t)} dy="0.32em" textAnchor="end">
                  {axis(t)}
                </text>
              </g>
            ))}
            {timeIdx.map((i, k) => (
              <text
                key={i}
                className="mf-gr-chart__axis"
                x={x(i)}
                y={height - 6}
                textAnchor={timeIdx.length > 1 && k === 0 ? "start" : k === timeIdx.length - 1 && timeIdx.length > 1 ? "end" : "middle"}
              >
                {timeText(times[i], stepMs)}
              </text>
            ))}

            <g className="mf-gr-chart__ink">
              {series.map((s) => (
                <path
                  key={s.key}
                  d={pathOf(s.values)}
                  fill="none"
                  stroke={s.color}
                  strokeWidth={2}
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeDasharray={s.dashed ? "5 4" : undefined}
                />
              ))}
            </g>

            {at != null ? (
              <g>
                <line
                  x1={x(at)}
                  x2={x(at)}
                  y1={M.top}
                  y2={M.top + innerH}
                  stroke="var(--mf-ink-3)"
                  strokeWidth={1}
                  shapeRendering="crispEdges"
                />
                {series.map((s) => (
                  <circle
                    key={s.key}
                    cx={x(at)}
                    cy={y(Number.isFinite(s.values[at]) ? s.values[at] : 0)}
                    r={3.5}
                    fill={s.dashed ? "var(--mf-ground-2)" : s.color}
                    stroke={s.color}
                    strokeWidth={1.5}
                  />
                ))}
              </g>
            ) : null}

            <rect
              x={M.left}
              y={M.top}
              width={innerW}
              height={innerH}
              fill="transparent"
              onPointerMove={pick}
              onPointerDown={pick}
              onPointerLeave={() => setAt(null)}
            />
          </svg>
        ) : null}
      </div>

      <div className="mf-gr-chart__legend" aria-live="polite">
        <span className="mf-gr-chart__when">
          {at != null && times[at] != null ? timeLong(times[at], stepMs) : "Whole period"}
        </span>
        {series.map((s, i) => (
          <span key={s.key} className="mf-gr-chart__key">
            <svg width="20" height="8" viewBox="0 0 20 8" aria-hidden="true">
              <line
                x1="1"
                x2="19"
                y1="4"
                y2="4"
                stroke={s.color}
                strokeWidth="2"
                strokeLinecap="round"
                strokeDasharray={s.dashed ? "4 3" : undefined}
              />
            </svg>
            <span className="mf-gr-chart__name">{s.label}</span>
            <span className="mf-gr-chart__val mf-data">
              {format(at != null ? (Number.isFinite(s.values[at]) ? s.values[at] : 0) : totals[i])}
            </span>
          </span>
        ))}
      </div>
    </div>
  );
}

/** The chart's loading shape: grey bars where the axis and lines will be. */
export function ChartWaiting({ height = 220 }: { height?: number }) {
  return (
    <div className="mf-gr-chart mf-gr-chart--wait" aria-busy="true" aria-live="polite" style={{ height }}>
      <span className="mf-sr">Loading the chart…</span>
      {Array.from({ length: 4 }).map((_, i) => (
        <span key={i} className="mf-wait" style={{ width: "100%", height: 1 }} />
      ))}
      <span className="mf-wait" style={{ width: "38%" }} />
    </div>
  );
}
