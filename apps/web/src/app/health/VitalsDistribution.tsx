/**
 * The vitals printout.
 *
 * Each metric is printed as its own ruled histogram strip on one continuous
 * sheet: the same plot width, the same baseline, the same left edge, so five
 * distributions can be read against each other the way a multi-channel
 * recorder's bands are. Bars are drawn in the house grammar (a fill under a
 * bright cap line with the ridge of displaced soot beneath it), and the
 * thresholds are written into the margins rather than painted over the data.
 *
 * ---------------------------------------------------------------------------
 * THE THRESHOLDS ARE NOT OURS TO CHOOSE.
 *
 * These are the published Core Web Vitals bounds (web.dev/articles/vitals, and
 * the same numbers the Chrome `web-vitals` library reports), assessed at the
 * 75th percentile of page loads:
 *
 *   LCP   good <= 2500 ms   needs improvement <= 4000 ms   poor > 4000 ms
 *   CLS   good <= 0.10      needs improvement <= 0.25      poor > 0.25
 *   INP   good <= 200 ms    needs improvement <= 500 ms    poor > 500 ms
 *   FCP   good <= 1800 ms   needs improvement <= 3000 ms   poor > 3000 ms
 *   TTFB  good <= 800 ms    needs improvement <= 1800 ms   poor > 1800 ms
 *
 * FCP and TTFB are diagnostics rather than Core Web Vitals, but their bounds
 * are published alongside them and are reported here unchanged. Nothing below
 * is rounded, tuned or invented: a rating Micaforge prints has to mean exactly
 * what the same word means in Chrome.
 * ---------------------------------------------------------------------------
 *
 * A rating is stated as a WORD next to the figure, and the share of samples in
 * each band is written under the axis. Colour only ever agrees with a word that
 * is already there; it never carries the meaning alone.
 */
import { useEffect, useRef, useState } from "react";
import { ms, num, pct } from "../../lib/format";

/* ── the metrics ─────────────────────────────────────────────────────────── */

export type VitalKey = "lcp" | "cls" | "inp" | "fcp" | "ttfb";

export type VitalRating = "good" | "needs-improvement" | "poor" | "none";

export interface VitalSpec {
  key: VitalKey;
  abbr: string;
  name: string;
  /** Upper bound of "good". */
  good: number;
  /** Upper bound of "needs improvement". Anything above it is poor. */
  ni: number;
  format: (v: number) => string;
}

/** Layout shift is a ratio, not a duration, so it is never printed in ms. */
const layout = (v: number) => v.toFixed(2);

export const VITALS: VitalSpec[] = [
  { key: "lcp", abbr: "LCP", name: "Largest contentful paint", good: 2500, ni: 4000, format: ms },
  { key: "cls", abbr: "CLS", name: "Cumulative layout shift", good: 0.1, ni: 0.25, format: layout },
  { key: "inp", abbr: "INP", name: "Interaction to next paint", good: 200, ni: 500, format: ms },
  { key: "fcp", abbr: "FCP", name: "First contentful paint", good: 1800, ni: 3000, format: ms },
  { key: "ttfb", abbr: "TTFB", name: "Time to first byte", good: 800, ni: 1800, format: ms },
];

export const VITAL_BY_KEY: Record<VitalKey, VitalSpec> = VITALS.reduce(
  (acc, s) => {
    acc[s.key] = s;
    return acc;
  },
  {} as Record<VitalKey, VitalSpec>,
);

export const RATING_WORD: Record<VitalRating, string> = {
  good: "Good",
  "needs-improvement": "Needs work",
  poor: "Poor",
  none: "Not measured",
};

/** Every vital reads better lower, which is why the readout deltas invert. */
export function rateVital(key: VitalKey, value: number | null | undefined): VitalRating {
  if (value == null || !Number.isFinite(value)) return "none";
  const spec = VITAL_BY_KEY[key];
  if (value <= spec.good) return "good";
  if (value <= spec.ni) return "needs-improvement";
  return "poor";
}

/* ── the shapes this screen reads ────────────────────────────────────────── */

export interface VitalStat {
  p50: number | null;
  p75: number | null;
  p95: number | null;
  samples: number;
}

/** One histogram bin, half-open: `lo` inclusive, `hi` exclusive. */
export interface VitalBin {
  lo: number;
  hi: number;
  n: number;
}

export type VitalStats = Partial<Record<VitalKey, VitalStat>>;
export type VitalHistogram = Partial<Record<VitalKey, VitalBin[]>>;

/* ── geometry ────────────────────────────────────────────────────────────── */

const H = 112;
const TOP = 22; /* room for the p75 mark written above the plot */
const BOTTOM = 34; /* two rows of margin: threshold values, then band names */
const PLOT_H = H - TOP - BOTTOM;

function useWidth<T extends HTMLElement>(initial: number) {
  const ref = useRef<T | null>(null);
  const [w, setW] = useState(initial);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    setW(el.clientWidth || initial);
    const ro = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width;
      if (next && next > 0) setW(next);
    });
    ro.observe(el);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return [ref, w] as const;
}

/**
 * The share of samples falling in each rating band.
 *
 * A bin can straddle a threshold, so its count is split across the two bands in
 * proportion to how much of the bin's width sits either side. That is an
 * approximation and it is the honest one: pretending a straddling bin belongs
 * wholly to one band would overstate whichever band won.
 */
function bandShares(bins: VitalBin[], good: number, ni: number) {
  let g = 0;
  let n = 0;
  let p = 0;
  let total = 0;
  for (const b of bins) {
    const width = b.hi - b.lo;
    total += b.n;
    if (!(width > 0)) {
      // A zero-width bin is a single value; place it whole.
      if (b.lo <= good) g += b.n;
      else if (b.lo <= ni) n += b.n;
      else p += b.n;
      continue;
    }
    const part = (from: number, to: number) =>
      (Math.max(0, Math.min(b.hi, to) - Math.max(b.lo, from)) / width) * b.n;
    g += part(b.lo, good);
    n += part(good, ni);
    p += part(ni, Math.max(b.hi, ni));
  }
  if (total <= 0) return null;
  return { good: g / total, ni: n / total, poor: p / total, total };
}

/* ── one strip ───────────────────────────────────────────────────────────── */

function VitalStrip({
  spec,
  stat,
  bins,
  current,
  onSelect,
}: {
  spec: VitalSpec;
  stat?: VitalStat;
  bins?: VitalBin[];
  current: boolean;
  onSelect?: (key: VitalKey) => void;
}) {
  const [plotRef, width] = useWidth<HTMLDivElement>(640);
  const p75 = stat?.p75 ?? null;
  const rating = rateVital(spec.key, p75);

  const usable = (bins ?? []).filter((b) => Number.isFinite(b.n) && b.n >= 0);
  const peak = usable.reduce((m, b) => Math.max(m, b.n), 0);
  const shares = usable.length ? bandShares(usable, spec.good, spec.ni) : null;

  /* The domain always reaches past the poor threshold, so all three bands are
     on the paper even when every sample landed in the first bin. */
  const dataMax = usable.reduce((m, b) => Math.max(m, b.hi), 0);
  const domain = Math.max(dataMax, spec.ni * 1.2, spec.good * 1.5);
  const X = (v: number) => (Math.max(0, Math.min(domain, v)) / domain) * width;

  const Head = onSelect ? "button" : "div";

  const summary = shares
    ? `${pct(shares.good)} good, ${pct(shares.ni)} needs work, ${pct(shares.poor)} poor, across ${num(shares.total)} samples.`
    : "No distribution recorded.";

  return (
    <li className="mf-vital" data-current={current ? "true" : "false"} data-rating={rating}>
      <Head
        className="mf-vital__head"
        type={onSelect ? "button" : undefined}
        aria-pressed={onSelect ? current : undefined}
        onClick={onSelect ? () => onSelect(spec.key) : undefined}
      >
        <span className="mf-vital__abbr">{spec.abbr}</span>
        <span className="mf-vital__name">
          {spec.name}
          {stat?.samples ? ` · ${num(stat.samples)} samples` : ""}
        </span>
        <span className="mf-vital__figure mf-data">{p75 == null ? "—" : spec.format(p75)}</span>
        <span className="mf-vital__at">p75</span>
        <span className="mf-rating mf-vital__rating" data-rating={rating}>
          {RATING_WORD[rating]}
        </span>
      </Head>

      <div className="mf-vital__plot" ref={plotRef}>
        {usable.length && peak > 0 ? (
          <svg
            width="100%"
            height={H}
            viewBox={`0 0 ${Math.max(width, 120)} ${H}`}
            role="img"
            aria-label={`${spec.name} distribution. ${summary} 75th percentile ${
              p75 == null ? "not measured" : spec.format(p75)
            }, rated ${RATING_WORD[rating].toLowerCase()}.`}
          >
            {/* the paper's ruling, clipped to this strip and nowhere else */}
            {[0, 0.5, 1].map((f) => (
              <line
                key={f}
                x1={0}
                x2={width}
                y1={TOP + PLOT_H * (1 - f)}
                y2={TOP + PLOT_H * (1 - f)}
                stroke={f === 0 ? "var(--mf-rule-major)" : "var(--mf-rule-minor)"}
                strokeWidth={1}
                shapeRendering="crispEdges"
              />
            ))}

            {/* the bars: a fill under a bright cap, with the burr beneath it */}
            {usable.map((b, i) => {
              const x0 = X(b.lo);
              const x1 = X(b.hi);
              const w = Math.max(1, x1 - x0 - 1);
              const h = (b.n / peak) * PLOT_H;
              const y = TOP + PLOT_H - h;
              if (h <= 0) return null;
              return (
                <g key={`${b.lo}-${i}`}>
                  <rect x={x0} y={y} width={w} height={h} fill="var(--mf-trace-human-fill)" />
                  <line
                    x1={x0}
                    x2={x0 + w}
                    y1={y + 1.1}
                    y2={y + 1.1}
                    stroke="var(--mf-well)"
                    strokeWidth={1.6}
                  />
                  <line
                    x1={x0}
                    x2={x0 + w}
                    y1={y}
                    y2={y}
                    stroke="var(--mf-trace-human)"
                    strokeWidth={1.4}
                  />
                </g>
              );
            })}

            {/* the two thresholds, ruled through the plot and written below */}
            {[spec.good, spec.ni].map((bound) => (
              <g key={bound}>
                <line
                  x1={X(bound)}
                  x2={X(bound)}
                  y1={TOP}
                  y2={TOP + PLOT_H + 4}
                  stroke="var(--mf-rule-strong)"
                  strokeWidth={1}
                  strokeDasharray="2 3"
                  shapeRendering="crispEdges"
                />
                <text
                  /* clear the cut: a bound sitting on the plot's own edge is
                     written inward rather than half off the paper */
                  x={Math.min(Math.max(X(bound), 0), width)}
                  y={TOP + PLOT_H + 16}
                  textAnchor={X(bound) < 24 ? "start" : X(bound) > width - 24 ? "end" : "middle"}
                  className="mf-vital__tick"
                >
                  {spec.format(bound)}
                </text>
              </g>
            ))}

            {/* band names and their shares, written in the bottom margin */}
            {shares &&
              (
                [
                  { word: "Good", from: 0, to: spec.good, share: shares.good },
                  { word: "Needs work", from: spec.good, to: spec.ni, share: shares.ni },
                  { word: "Poor", from: spec.ni, to: domain, share: shares.poor },
                ] as const
              ).map((band) => {
                const mid = (X(band.from) + X(band.to)) / 2;
                const room = X(band.to) - X(band.from);
                const label = `${band.word} ${pct(band.share)}`;
                /* ~5.6px per character at 10.5px: a band that cannot hold its
                   own name goes unnamed rather than spilling into its
                   neighbour. The thresholds under it still say where it is. */
                if (room < label.length * 5.6 + 10) return null;
                return (
                  <text
                    key={band.word}
                    x={mid}
                    y={TOP + PLOT_H + 29}
                    textAnchor="middle"
                    className="mf-vital__band"
                    fill={
                      band.word === "Poor"
                        ? "var(--mf-oxide)"
                        : band.word === "Good"
                          ? "var(--mf-verdigris)"
                          : "var(--mf-ink-3)"
                    }
                  >
                    {label}
                  </text>
                );
              })}

            {/* the 75th percentile, marked and named above the plot */}
            {p75 != null && (
              <g>
                <line
                  x1={X(p75)}
                  x2={X(p75)}
                  y1={TOP - 7}
                  y2={TOP + PLOT_H}
                  stroke="var(--mf-shellac)"
                  strokeWidth={1.2}
                  shapeRendering="crispEdges"
                />
                <text
                  x={X(p75) > width - 76 ? X(p75) - 5 : X(p75) + 5}
                  y={TOP - 10}
                  textAnchor={X(p75) > width - 76 ? "end" : "start"}
                  className="mf-vital__mark"
                >
                  {`p75 ${spec.format(p75)}`}
                </text>
              </g>
            )}
          </svg>
        ) : (
          <p className="mf-vital__none">
            {stat?.samples
              ? "No distribution came back for this metric, only its percentiles."
              : "No samples for this metric in this interval."}
          </p>
        )}
      </div>
    </li>
  );
}

/* ── the printout ────────────────────────────────────────────────────────── */

export function VitalsDistribution({
  metrics,
  histogram,
  selected,
  onSelect,
}: {
  metrics?: VitalStats;
  histogram?: VitalHistogram;
  /** The metric the rest of the screen is reporting on. */
  selected?: VitalKey;
  onSelect?: (key: VitalKey) => void;
}) {
  return (
    <ol className="mf-vitals" role="list">
      {VITALS.map((spec) => (
        <VitalStrip
          key={spec.key}
          spec={spec}
          stat={metrics?.[spec.key]}
          bins={histogram?.[spec.key]}
          current={selected === spec.key}
          onSelect={onSelect}
        />
      ))}
    </ol>
  );
}

export default VitalsDistribution;
