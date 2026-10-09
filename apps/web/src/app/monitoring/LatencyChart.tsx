/**
 * One monitor's response time over a range, with its failures marked.
 *
 * Three things are drawn, and each has its own cue besides colour:
 *
 *   - the AVERAGE response time is a solid line,
 *   - the 95th PERCENTILE is a dashed line of the same blue, above it,
 *   - a period with FAILED CHECKS is a light band behind the lines AND a block
 *     in its own labelled track under the plot, as tall as the share of that
 *     period's checks that failed.
 *
 * The lines are people blue: a monitor's request is not AI traffic, and orange
 * means AI everywhere in this product.
 *
 * The plot is an SVG stretched to its box with `preserveAspectRatio="none"` and
 * non-scaling strokes, so it fills any width without measuring the page. Every
 * word (axis labels, the reading under the plot) is HTML laid over it, so text
 * is never stretched with the drawing.
 *
 * Pointing at the plot, or focusing it and pressing the arrow keys, picks one
 * period and prints its figures in the line under the chart. That line is a
 * live region, so a screen reader hears each period as the keys move.
 */
import { useMemo, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";
import { ms, num } from "../../lib/format";
import type { SeriesPoint, SeriesRange } from "./types";

const W = 1000;
const H = 180;

const RANGE_WORD: Record<SeriesRange, string> = {
  "24h": "the last 24 hours",
  "7d": "the last 7 days",
  "30d": "the last 30 days",
};

/** A tick step of 1, 2, 2.5 or 5 times a power of ten. */
function niceStep(raw: number): number {
  if (!(raw > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(raw));
  const f = raw / p;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10) * p;
}

/**
 * A line through the non-null values. A gap in the data is a gap in the line,
 * never a slope drawn across time nobody measured. A value with no neighbour
 * is drawn as a short flat stroke across its own period, so it is still seen.
 */
function linePath(values: (number | null)[], x: (i: number) => number, y: (v: number) => number, half: number) {
  let d = "";
  for (let i = 0; i < values.length; i++) {
    const v = values[i];
    if (v == null) continue;
    const prev = i > 0 ? values[i - 1] : null;
    const next = i < values.length - 1 ? values[i + 1] : null;
    const px = x(i).toFixed(1);
    const py = y(v).toFixed(1);
    if (prev == null && next == null) {
      d += `M${(x(i) - half).toFixed(1)},${py}L${(x(i) + half).toFixed(1)},${py}`;
    } else {
      d += `${prev == null ? "M" : "L"}${px},${py}`;
    }
  }
  return d;
}

export function LatencyChart({ points, range }: { points: SeriesPoint[]; range: SeriesRange }) {
  const [at, setAt] = useState<number | null>(null);
  const n = points.length;

  const model = useMemo(() => {
    const avgs = points.map((p) => p.latency_avg_ms);
    const p95s = points.map((p) => p.latency_p95_ms);
    const peak = Math.max(1, ...avgs.map((v) => v ?? 0), ...p95s.map((v) => v ?? 0));
    const step = niceStep(peak / 3);
    const top = Math.ceil(peak / step) * step;
    const ticks: number[] = [];
    for (let v = 0; v <= top + step / 2; v += step) ticks.push(v);

    const bw = W / Math.max(1, n);
    const x = (i: number) => (i + 0.5) * bw;
    const y = (v: number) => H - 4 - (v / top) * (H - 12);

    const bucketMs = n > 1 ? points[1].t - points[0].t : 3_600_000;
    const fmtPoint = new Intl.DateTimeFormat(
      undefined,
      bucketMs < 86_400_000
        ? { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" }
        : { month: "short", day: "numeric" },
    );
    const fmtTick = new Intl.DateTimeFormat(
      undefined,
      range === "24h" ? { hour: "2-digit", minute: "2-digit" } : { month: "short", day: "numeric" },
    );

    const tickIdx = n <= 1 ? [0] : Array.from(new Set([0, 1, 2, 3, 4].map((k) => Math.round((k * (n - 1)) / 4))));

    const measured = avgs.filter((v): v is number => v != null);
    const meanAvg = measured.length ? measured.reduce((a, b) => a + b, 0) / measured.length : null;
    const p95Measured = p95s.filter((v): v is number => v != null);
    const maxP95 = p95Measured.length ? Math.max(...p95Measured) : null;
    const failing = points.filter((p) => p.failures > 0).length;

    return {
      avgPath: linePath(avgs, x, y, bw * 0.35),
      p95Path: linePath(p95s, x, y, bw * 0.35),
      ticks,
      top,
      bw,
      x,
      y,
      fmtPoint,
      fmtTick,
      tickIdx,
      meanAvg,
      maxP95,
      failing,
    };
  }, [points, n, range]);

  const pick = (e: PointerEvent<HTMLDivElement>) => {
    const box = e.currentTarget.getBoundingClientRect();
    if (box.width <= 0) return;
    const i = Math.floor(((e.clientX - box.left) / box.width) * n);
    setAt(Math.max(0, Math.min(n - 1, i)));
  };

  const keys = (e: KeyboardEvent<HTMLDivElement>) => {
    const last = n - 1;
    const cur = at ?? last;
    let next: number | null = cur;
    if (e.key === "ArrowLeft") next = Math.max(0, cur - 1);
    else if (e.key === "ArrowRight") next = Math.min(last, cur + 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = last;
    else if (e.key === "Escape") next = null;
    else return;
    e.preventDefault();
    setAt(next);
  };

  const summary = `Response time over ${RANGE_WORD[range]}. Average ${ms(model.meanAvg)}, highest 95th percentile ${ms(model.maxP95)}. ${num(model.failing)} of ${num(n)} periods had failed checks.`;

  const active = at != null ? points[at] : null;
  const reading = active
    ? `${model.fmtPoint.format(active.t)}: average ${ms(active.latency_avg_ms)}, 95th percentile ${ms(active.latency_p95_ms)}, ${num(active.checks)} ${active.checks === 1 ? "check" : "checks"}, ${num(active.failures)} failed.`
    : "Point at the chart, or focus it and use the arrow keys, to read one period.";

  return (
    <div className="mf-mon-chart">
      <ul className="mf-mon-chart__legend" aria-label="Legend">
        <li>
          <span className="mf-mon-chart__swatch" data-pen="solid" aria-hidden="true" />
          Average
        </li>
        <li>
          <span className="mf-mon-chart__swatch" data-pen="dashed" aria-hidden="true" />
          95th percentile
        </li>
        <li>
          <span className="mf-mon-chart__swatch" data-pen="fail" aria-hidden="true" />
          Failed checks
        </li>
      </ul>

      <div className="mf-mon-chart__frame">
        <div className="mf-mon-chart__yaxis" aria-hidden="true">
          {model.ticks.map((v) => (
            <span key={v} className="mf-mon-chart__ylabel" style={{ top: `${(model.y(v) / H) * 100}%` }}>
              {ms(v)}
            </span>
          ))}
        </div>
        <span className="mf-mon-chart__tracklabel" aria-hidden="true">
          Failed
        </span>

        <div
          className="mf-mon-chart__plot"
          role="group"
          tabIndex={0}
          aria-label={summary}
          onPointerMove={pick}
          onPointerLeave={() => setAt(null)}
          onKeyDown={keys}
          onBlur={() => setAt(null)}
        >
          <svg
            key={range}
            className="mf-mon-chart__svg"
            viewBox={`0 0 ${W} ${H}`}
            preserveAspectRatio="none"
            aria-hidden="true"
            focusable="false"
          >
            {model.ticks.map((v) => (
              <line
                key={v}
                x1={0}
                x2={W}
                y1={model.y(v)}
                y2={model.y(v)}
                stroke={v === 0 ? "var(--mf-rule-major)" : "var(--mf-rule-minor)"}
                strokeWidth={1}
                vectorEffect="non-scaling-stroke"
                shapeRendering="crispEdges"
              />
            ))}
            {points.map((p, i) =>
              p.failures > 0 ? (
                <rect
                  key={p.t}
                  x={i * model.bw}
                  y={0}
                  width={model.bw}
                  height={H}
                  fill="var(--mf-danger-wash)"
                />
              ) : null,
            )}
            <g className="mf-mon-chart__lines">
              <path
                d={model.p95Path}
                fill="none"
                stroke="var(--mf-people)"
                strokeWidth={1.5}
                strokeDasharray="5 4"
                strokeLinejoin="round"
                vectorEffect="non-scaling-stroke"
              />
              <path
                d={model.avgPath}
                fill="none"
                stroke="var(--mf-people)"
                strokeWidth={2}
                strokeLinejoin="round"
                strokeLinecap="round"
                vectorEffect="non-scaling-stroke"
              />
            </g>
          </svg>
          {at != null ? (
            <span
              className="mf-mon-chart__cross"
              style={{ left: `${(model.x(at) / W) * 100}%` }}
              aria-hidden="true"
            />
          ) : null}
        </div>

        <div className="mf-mon-chart__track" aria-hidden="true">
          <svg viewBox={`0 0 ${W} 14`} preserveAspectRatio="none" focusable="false">
            <line
              x1={0}
              x2={W}
              y1={13.5}
              y2={13.5}
              stroke="var(--mf-rule)"
              strokeWidth={1}
              vectorEffect="non-scaling-stroke"
            />
            {points.map((p, i) => {
              if (p.failures <= 0 || p.checks <= 0) return null;
              const share = Math.max(0.3, Math.min(1, p.failures / p.checks));
              const h = 13 * share;
              return (
                <rect
                  key={p.t}
                  x={i * model.bw + model.bw * 0.1}
                  y={13 - h}
                  width={Math.max(model.bw * 0.8, 2)}
                  height={h}
                  fill="var(--mf-danger)"
                />
              );
            })}
          </svg>
        </div>

        <div className="mf-mon-chart__xaxis" aria-hidden="true">
          {model.tickIdx.map((i, k) => (
            <span
              key={i}
              className="mf-mon-chart__xlabel"
              data-edge={k === 0 ? "start" : k === model.tickIdx.length - 1 ? "end" : undefined}
              data-minor={k % 2 === 1 ? "true" : undefined}
              style={{ left: `${(model.x(i) / W) * 100}%` }}
            >
              {points[i] ? model.fmtTick.format(points[i].t) : ""}
            </span>
          ))}
        </div>
      </div>

      <p className="mf-mon-chart__reading" aria-live="polite">
        {reading}
      </p>
    </div>
  );
}

export default LatencyChart;
