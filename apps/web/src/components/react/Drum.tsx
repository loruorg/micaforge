/**
 * The time-series chart used across the dashboard.
 *
 * Series are drawn as lines over a fixed time axis with a tick at each calendar
 * boundary, and notes are written in the right margin with a leader line back
 * to the moment they describe. The historical name "Drum" (and the "pen" and
 * "channel" names inside) come from the product's first design; read "pen" as
 * "series line" and "channel" as "series".
 *
 * Three behaviours here are deliberate and must not be "simplified" away:
 *
 *  1. TWO LINE STYLES. People are drawn as a smooth line and AI crawlers as a
 *     stepped line, so the two series differ by shape as well as by colour.
 *  2. THE MIRROR. In `mirrored` mode the AI series rises from the centre line
 *     and the people series falls from it. The difference between the two
 *     sides is the Citation Gap, shown as one picture.
 *  3. THE SCALE. A series pressed flat against its baseline shows nothing. The
 *     scale is therefore chosen so every series stays readable, and whatever
 *     that choice costs (a clipped peak, separate scales) is PRINTED rather
 *     than hidden. See `solveScale`.
 *
 * Content is never gated on animation: the full chart is in the DOM on first
 * paint. The only live motion is the tick at the newest sample, which draws
 * once and holds, and it is disabled under `prefers-reduced-motion`.
 */
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";

export type PenStyle = "written" | "stepped";

export interface DrumChannel {
  id: string;
  label: string;
  /** A CSS colour, normally var(--mf-people) or var(--mf-ai). */
  color: string;
  values: (number | null)[];
  /** `stepped` for AI crawler fetches (discrete jumps), `written` (smooth) for people. */
  pen?: PenStyle;
  /** Ghost trace for a comparison period; drawn dashed and unfilled. */
  ghost?: boolean;
  /** Formats the value shown in the legend and under the hover line. */
  format?: (n: number) => string;
}

export interface DrumAnnotation {
  /** Index into `times`. */
  at: number;
  label: string;
  tone?: "neutral" | "warn" | "good";
  /**
   * Who wrote the note. `auto` (the default) is one read off the data, by this
   * chart or by the screen: a dot on the moment and a dotted leader to the
   * margin. `user` is one a person wrote ("Launched v2"): a dashed rule across
   * the whole plot with a flag at its top, a light band when it covers a range,
   * and a solid leader ending in a flag. The two kinds differ by shape, so the
   * difference never rests on colour alone.
   */
  kind?: "auto" | "user";
  /** Inclusive end index, for a `user` note that covers a range. */
  to?: number;
  /** A CSS colour for a `user` note's rule, band, flag and words. */
  color?: string;
  /** A stable key for a `user` note, so two notes with the same words stay two. */
  id?: string;
}

/** An annotation plus the colour of its on-plot mark, set internally so an
 *  over-range note points at the pen it belongs to. */
type MarginNote = DrumAnnotation & { mark?: string };

/** A time label, solved once so the collision test and the render agree on
 *  where its glyphs sit. */
type TimeLabel = {
  i: number;
  px: number;
  anchor: "start" | "middle" | "end";
  text: string;
};

export interface DrumProps {
  times: number[];
  channels: DrumChannel[];
  /** `stacked` writes each channel in its own band; `mirrored` opposes two. */
  mode?: "stacked" | "mirrored";
  /**
   * `shared` puts every pen on one scale; `per-channel` gives each its own gain
   * and prints it. Defaults to `per-channel` for mirrored records, because two
   * channels two orders of magnitude apart leave one pen flat on a shared scale.
   */
  scale?: "shared" | "per-channel";
  /**
   * How the top of each gain is chosen. `robust` (the default) reads it off a
   * high quantile when the single largest sample is a genuine outlier, so one
   * spike cannot press every other sample flat; `max` forces the raw maximum.
   */
  ceilingFrom?: "robust" | "max";
  height?: number;
  /**
   * Forces a fiducial notch every N samples. Left unset, notches land on real
   * calendar boundaries read off the bucket granularity.
   */
  notchEvery?: number;
  annotations?: DrumAnnotation[];
  /**
   * The Drum writes its own note where a pen ran off the top of the paper. Set
   * false when the calling screen annotates that sample itself, so one moment
   * is never written twice in the margin.
   */
  overRangeNotes?: boolean;
  /**
   * The one-line note under a mirrored record saying what the distance between
   * two pens does and does not measure. Set false when the screen states it in
   * its own words, so the reader is not given the same sentence twice.
   */
  gainNote?: boolean;
  /** Called when the reader drags out a range, with the inclusive sample indices. */
  onMeasure?: (from: number, to: number) => void;
  /**
   * Called when the reader presses and releases on one sample without
   * dragging, with that sample's index. A drag is still a measured range; only
   * a press that stays on one sample is a pick.
   */
  onPickSample?: (index: number) => void;
  /** Marks the newest sample with a dot, for a chart that updates live. */
  live?: boolean;
  formatTime?: (t: number) => string;
  emptyLabel?: string;
  className?: string;
  /** Marks the whole record as illustrative rather than measured. */
  demo?: boolean;
  /**
   * The pen writes the record again, left to right, whenever the data it is
   * recording changes: a new range, a new filter, a new audience. Off for a
   * record that is refreshed in place on a timer, where a rewrite every poll
   * would be noise rather than news.
   */
  write?: boolean;
  /**
   * Write the record when it first appears. Off for a record that was already
   * painted by the server, where hiding it again at hydration would flicker.
   */
  writeOnMount?: boolean;
  /**
   * Hold the record unwritten until it scrolls into view, then write it. Only
   * for an island that hydrates before it is seen; a record already on screen
   * when this mounts is left exactly as it is.
   */
  writeOnView?: boolean;
  /**
   * A live record's step counter. Each increment means the data advanced by
   * one sample, and the paper slides one step left under a pen that stays put.
   */
  advance?: number;
}

const MARGIN = { top: 18, right: 176, bottom: 30, left: 52 };
/* The narrow gutter still has to carry a top-of-scale label, so the left margin
   is sized for the widest number `gainNum` can print, not squeezed to zero. */
const MARGIN_NARROW = { top: 14, right: 14, bottom: 24, left: 46 };

/* --- gain -----------------------------------------------------------------
   A single spike is a real reading, but letting it set the ceiling spends all
   of the paper's vertical travel on one bucket and leaves the other twenty-nine
   as a hairline on the baseline. A station operator turns the gain down until
   the RECORD is readable and accepts that the peak runs off the top, where the
   pen leaves a stop mark against the paper's edge. These constants are that
   judgement: read the ceiling off the 96th percentile, but only when the
   maximum is far enough above it to be an outlier and only when there are
   enough samples for a quantile to mean anything. On a short or an even series
   the maximum IS the right ceiling and is used unchanged. */
const ROBUST_QUANTILE = 0.96;
const OUTLIER_FACTOR = 2.5;
const ROBUST_MIN_SAMPLES = 8;

/* --- notches --------------------------------------------------------------
   Fiducial notches mark interval boundaries, so they are calendar facts, not
   every seventh sample. The finest unit that does not crowd the paper wins. */
type CalendarUnit = "hour" | "day" | "week" | "month";
const CAL_UNITS: CalendarUnit[] = ["hour", "day", "week", "month"];
const NOTCH_MAX = 8;
const NOTCH_MIN = 3;
/** Clear space demanded between the printed EXTENTS of two time labels, in plot
 *  pixels. Thinning on the distance between notches is not enough: a label at
 *  the paper's edge is anchored to start or to end, so its glyphs run to one
 *  side of its own notch and can reach a neighbour that is nominally far
 *  enough away. The test therefore measures where the words actually land. */
const LABEL_CLEAR = 14;
const LABEL_CLEAR_NARROW = 10;
/** Advance of one glyph in the 11px data face. It is a monospace, so a
 *  character count is an exact width, not an estimate. */
const LABEL_CHAR_W = 6.6;
/** When even the coarsest unit is crowded, notch every Nth boundary, and keep N
 *  to a divisor a reader can hold: every other month, every quarter, every year. */
const NICE_STRIDES = [1, 2, 3, 4, 6, 12];

/* --- printed gain ---------------------------------------------------------
   A ceiling label belongs to the band it measures. Written ON the band's top
   rule it sits in the gutter between two bands and attaches itself to the band
   above, which is how a column of gains stops being monotonic and starts being
   a puzzle. Every label is therefore written INSIDE its own band: the ceiling
   just under the band's top rule, the baseline just above its foot. The mid and
   the baseline are printed only where the band is deep enough to hold them
   apart, because a scale the reader cannot separate is not a scale. */
const GAIN_DROP = 10;
const GAIN_LIFT = 3;
const GAIN_MID = 3.5;
const GAIN_MIN_MID = 48;
const GAIN_MIN_ZERO = 84;

function useWidth<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [w, setW] = useState(960);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width;
      if (next && Math.abs(next - w) > 1) setW(next);
    });
    ro.observe(el);
    setW(el.clientWidth || 960);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return [ref, w] as const;
}

/**
 * Hold a record unwritten until it is seen, then write it.
 *
 * `pending` only from the moment the record is found below or above the
 * viewport until it scrolls in; `write` from then on. A record that was on
 * screen when this ran stays `idle` and is never hidden, and so does every
 * record under reduced motion or without IntersectionObserver.
 */
function useWriteOnView(ref: { current: HTMLElement | null }, enabled: boolean) {
  const [state, setState] = useState<"idle" | "pending" | "write">("idle");
  useEffect(() => {
    const el = ref.current;
    if (!enabled || !el || typeof IntersectionObserver === "undefined") return;
    if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
    const box = el.getBoundingClientRect();
    if (box.top < window.innerHeight && box.bottom > 0) return;
    setState("pending");
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setState("write");
          io.disconnect();
        }
      },
      { threshold: 0.3 },
    );
    io.observe(el);
    return () => io.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled]);
  return state;
}

/**
 * Catmull-Rom through the points, converted to cubic béziers. Written inline
 * rather than pulled from a curve library so the signature artifact carries no
 * dependency it does not need.
 */
function writtenPath(pts: [number, number][], tension = 0.5): string {
  if (pts.length === 0) return "";
  // One sample cannot be a line, and a bare `M` strokes nothing: the pen would
  // be listed in the readout while the paper stayed blank. Draw the mark the
  // pen would actually leave if it touched down once and did not travel.
  if (pts.length === 1) return singlePointMark(pts[0]);
  let d = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i === 0 ? 0 : i - 1];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const c1x = p1[0] + ((p2[0] - p0[0]) / 6) * tension * 2;
    const c1y = p1[1] + ((p2[1] - p0[1]) / 6) * tension * 2;
    const c2x = p2[0] - ((p3[0] - p1[0]) / 6) * tension * 2;
    const c2y = p2[1] - ((p3[1] - p1[1]) / 6) * tension * 2;
    d += `C${c1x},${c1y} ${c2x},${c2y} ${p2[0]},${p2[1]}`;
  }
  return d;
}

/**
 * The mark a pen leaves when it touches down once and does not travel.
 *
 * A short horizontal stroke centred on the sample, not a dot: a dot reads as a
 * data point in a scatter, and this instrument does not draw scatters. Its
 * width is the stroke's own, so it holds at any gain.
 */
function singlePointMark([x, y]: [number, number]): string {
  return `M${x - 3},${y}L${x + 3},${y}`;
}

/** A crawler fetches in discrete jumps, so its pen steps rather than curves. */
function steppedPath(pts: [number, number][]): string {
  if (!pts.length) return "";
  if (pts.length === 1) return singlePointMark(pts[0]);
  let d = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 1; i < pts.length; i++) {
    const mid = (pts[i - 1][0] + pts[i][0]) / 2;
    d += `L${mid},${pts[i - 1][1]}L${mid},${pts[i][1]}L${pts[i][0]},${pts[i][1]}`;
  }
  return d;
}

/** Linear-interpolated quantile of an ascending series. */
function quantile(sorted: number[], p: number): number {
  if (!sorted.length) return 0;
  const pos = (sorted.length - 1) * p;
  const lo = Math.floor(pos);
  const hi = Math.ceil(pos);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
}

/** Monday-based week number, computed from the sample's own local date. */
function weekIndex(d: Date): number {
  const dayNo = Math.floor((d.getTime() - d.getTimezoneOffset() * 60_000) / 86_400_000);
  /* 1 January 1970 was a Thursday, so +3 puts the boundary on Mondays. */
  return Math.floor((dayNo + 3) / 7);
}

/** True when `cur` opens a new calendar unit relative to `prev`, in the
 *  reader's own time zone. Boundaries are dates, never millisecond strides. */
function opensUnit(prev: Date, cur: Date, unit: CalendarUnit): boolean {
  if (cur.getFullYear() !== prev.getFullYear()) return true;
  if (cur.getMonth() !== prev.getMonth()) return true;
  if (unit === "month") return false;
  if (unit === "week") return weekIndex(cur) !== weekIndex(prev);
  if (cur.getDate() !== prev.getDate()) return true;
  if (unit === "day") return false;
  return cur.getHours() !== prev.getHours();
}

function boundaryIndices(times: number[], unit: CalendarUnit): number[] {
  const out: number[] = [];
  let prev: Date | null = null;
  for (let i = 0; i < times.length; i++) {
    const cur = new Date(times[i]);
    if (prev && opensUnit(prev, cur, unit)) out.push(i);
    prev = cur;
  }
  return out;
}

const defaultNum = (n: number) =>
  n >= 1_000_000
    ? `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M`
    : n >= 1000
      ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k`
      : `${Math.round(n)}`;

/* A gain is a scale, and a scale has to survive being halved: a ceiling printed
   as 15 must print its mid as 7.5, never as 8. defaultNum rounds, which is
   right for one figure standing on its own and wrong for a column of numbers
   the reader is expected to add up. */
const trimNum = (n: number) => n.toFixed(2).replace(/\.?0+$/, "");
const gainNum = (n: number): string => {
  if (!Number.isFinite(n)) return "0";
  const abs = Math.abs(n);
  if (abs >= 1_000_000) return `${trimNum(n / 1_000_000)}M`;
  if (abs >= 1000) return `${trimNum(n / 1000)}k`;
  return trimNum(n);
};

/**
 * The legend swatch IS the pen: the same line character, the same dash and the
 * same weight the trace is drawn with, so a comparison ghost is told apart from
 * the pen it shadows with no colour at all. Drawn here rather than styled in
 * the stylesheet because the mark has to follow the trace, and the trace is
 * drawn here.
 */
function PenSwatch({ channel }: { channel: DrumChannel }) {
  const d =
    channel.pen === "stepped"
      ? "M0,8.5 H5.5 V4 H12 V6.5 H20"
      : "M0,8.5 C4,8.5 5,2.5 10,2.5 C15,2.5 16,8 20,4.5";
  return (
    <svg
      className="mf-drum__pen"
      width={20}
      height={11}
      viewBox="0 0 20 11"
      aria-hidden="true"
      focusable="false"
      style={{ alignSelf: "center", flex: "none" }}
    >
      <path
        d={d}
        fill="none"
        stroke={channel.color}
        strokeWidth={channel.ghost ? 1.25 : 2}
        strokeLinecap="round"
        strokeLinejoin="round"
        strokeDasharray={channel.ghost ? "3 3" : undefined}
        opacity={channel.ghost ? 0.55 : 1}
      />
    </svg>
  );
}

export function Drum({
  times,
  channels,
  mode = "stacked",
  scale,
  ceilingFrom = "robust",
  height = 264,
  notchEvery,
  annotations = [],
  overRangeNotes = true,
  gainNote = true,
  onMeasure,
  onPickSample,
  live = false,
  formatTime,
  emptyLabel = "No data for this period yet.",
  className,
  demo = false,
  write = true,
  writeOnMount = true,
  writeOnView = false,
  advance,
}: DrumProps) {
  const uid = useId().replace(/:/g, "");
  const [wrapRef, width] = useWidth<HTMLDivElement>();
  const pending = useWriteOnView(wrapRef, writeOnView);
  const narrow = width < 620;
  const M = narrow ? MARGIN_NARROW : MARGIN;

  const [hover, setHover] = useState<number | null>(null);
  const [caliper, setCaliper] = useState<{ a: number; b: number } | null>(null);
  const dragging = useRef<number | null>(null);

  const innerW = Math.max(80, width - M.left - M.right);
  const innerH = Math.max(70, height - M.top - M.bottom);
  const n = times.length;

  const x = useCallback(
    (i: number) => (n <= 1 ? innerW / 2 : (i / (n - 1)) * innerW),
    [n, innerW],
  );

  /** Nice round ceiling so the gain labels are readable numbers, not noise. */
  const niceCeiling = useCallback((m: number) => {
    if (!(m > 0)) return 1;
    const mag = Math.pow(10, Math.floor(Math.log10(m)));
    const step = m / mag <= 2 ? 0.5 : m / mag <= 5 ? 1 : 2;
    return Math.ceil(m / (mag * step)) * mag * step;
  }, []);

  /**
   * Solve one gain from the pool of samples that share it. Returns the ceiling,
   * the true maximum, and whether the ceiling crops anything, which is the fact
   * the readout and the margin have to state. Nothing is ever cropped silently.
   */
  const solveScale = useCallback(
    (pool: number[]) => {
      if (!pool.length) return { ceil: 1, max: 0, clipped: false };
      const sorted = [...pool].sort((a, b) => a - b);
      const max = sorted[sorted.length - 1];
      let head = max;
      if (ceilingFrom === "robust" && sorted.length >= ROBUST_MIN_SAMPLES) {
        const q = quantile(sorted, ROBUST_QUANTILE);
        if (q > 0 && max > OUTLIER_FACTOR * q) head = q;
      }
      const ceil = niceCeiling(head);
      return { ceil, max, clipped: max > ceil };
    },
    [ceilingFrom, niceCeiling],
  );

  /**
   * Gain. A multi-channel recorder does not force two channels onto one scale;
   * it gives each its own gain and PRINTS that gain in the margin, because a
   * pen pinned flat against its baseline records nothing at all. Micaforge's
   * two channels differ by two orders of magnitude (that difference is the
   * finding), so `per-channel` is the honest default for a mirrored record and
   * the full-scale value is written beside each pen. The magnitude itself is
   * never left to the eye: it is stated as a number in the readout.
   */
  const perChannel = (scale ?? (mode === "mirrored" ? "per-channel" : "shared")) === "per-channel";

  /**
   * The channel each pen is scaled against. A comparison trace is the same
   * measurement in an earlier window, so it reads on its host's gain; a ghost
   * scaled to itself would look identical to its host however different the two
   * windows actually were.
   */
  const attachIndex = useMemo(() => {
    const out: number[] = [];
    let lastSolid = -1;
    channels.forEach((c, i) => {
      if (!c.ghost) lastSolid = i;
      out.push(lastSolid < 0 ? i : lastSolid);
    });
    return out;
  }, [channels]);

  /** One solved gain per channel; channels sharing a gain share the object. */
  const scales = useMemo(() => {
    const pools = new Map<number, number[]>();
    channels.forEach((c, i) => {
      const key = perChannel ? attachIndex[i] : 0;
      let pool = pools.get(key);
      if (!pool) {
        pool = [];
        pools.set(key, pool);
      }
      for (const v of c.values) if (v != null && Number.isFinite(v)) pool.push(v);
    });
    const solved = new Map<number, ReturnType<typeof solveScale>>();
    pools.forEach((pool, key) => solved.set(key, solveScale(pool)));
    const fallback = { ceil: 1, max: 0, clipped: false };
    return channels.map((_, i) => solved.get(perChannel ? attachIndex[i] : 0) ?? fallback);
  }, [channels, perChannel, attachIndex, solveScale]);

  const ceiling = scales[0]?.ceil ?? 1;

  /**
   * Where each pen ran off the top of the paper, and how far. A clipped sample
   * is drawn ON the ceiling line and marked there, never dropped: the mark says
   * the reading left the paper and the margin note says what it actually was.
   */
  const overRange = useMemo(
    () =>
      channels.map((c, i) => {
        const ceil = scales[i]?.ceil ?? 1;
        let at = -1;
        let peak = 0;
        c.values.forEach((v, k) => {
          if (v == null || !(v > ceil)) return;
          if (v > peak) {
            peak = v;
            at = k;
          }
        });
        return { at, peak };
      }),
    [channels, scales],
  );

  /**
   * What each pen actually wrote in this window, kept per channel and never
   * read off the pool a channel shares its gain with. A ghost is drawn on its
   * host's scale, so the host's ceiling, clipping and peak all live in that
   * pool: printed beside the ghost they would be another channel's numbers.
   */
  const wrote = useMemo(
    () => channels.map((c) => c.values.some((v) => v != null && Number.isFinite(v))),
    [channels],
  );
  const penPeak = useMemo(
    () =>
      channels.map((c) => {
        let peak = Number.NEGATIVE_INFINITY;
        for (const v of c.values) if (v != null && Number.isFinite(v) && v > peak) peak = v;
        return Number.isFinite(peak) ? peak : null;
      }),
    [channels],
  );

  /**
   * A comparison trace belongs ON the band it is being compared against, not in
   * a band of its own. Giving a ghost its own band draws an empty strip and
   * prints a second, meaningless gain scale beside it.
   */
  const bands = useMemo(() => {
    const solids = channels.filter((c) => !c.ghost);
    const geom = (i: number, count: number) => {
      if (mode === "mirrored") {
        const mid = innerH / 2;
        return { base: mid, span: mid - 4, dir: (i === 0 ? -1 : 1) as -1 | 1 };
      }
      const gap = 12;
      const each = (innerH - gap * (count - 1)) / Math.max(1, count);
      return { base: (each + gap) * i + each, span: each, dir: -1 as -1 | 1 };
    };

    let solidIndex = -1;
    return channels.map((c, i) => {
      if (!c.ghost) solidIndex++;
      const attachTo = Math.max(0, solidIndex);
      const g = geom(attachTo, Math.max(1, solids.length));
      return {
        channel: c,
        ...g,
        ceil: scales[i]?.ceil ?? 1,
        clipped: scales[i]?.clipped ?? false,
        index: i,
        band: attachTo,
      };
    });
  }, [channels, innerH, mode, scales]);

  /** One entry per drawn band, for the gain scale and the paper ruling. */
  const scaleBands = useMemo(() => {
    const seen = new Set<number>();
    return bands.filter((b) => {
      if (b.channel.ghost || seen.has(b.band)) return false;
      seen.add(b.band);
      return true;
    });
  }, [bands]);

  const empty = n === 0 || channels.every((c) => c.values.every((v) => !v));

  /* ── motion ──────────────────────────────────────────────────────────
     One epoch per thing worth showing: a new record, written in, or a live
     record that moved on by a sample, advanced. The group carrying the pens is
     keyed by the epoch, so its animation runs once per epoch, and a hover, a
     resize or any other re-render leaves a running animation alone. */
  const signature = useMemo(
    () =>
      `${n}|${times[0] ?? ""}|${times[n - 1] ?? ""}|${channels
        .map((c) => `${c.id}:${c.values.reduce<number>((a, v) => a + (v ?? 0), 0)}`)
        .join(",")}`,
    [n, times, channels],
  );
  const epoch = useRef<{
    sig: string | null;
    adv: number | undefined;
    view: string;
    key: number;
    kind: "write" | "advance" | null;
  }>({ sig: null, adv: advance, view: pending, key: 0, kind: null });
  {
    const e = epoch.current;
    if (e.sig === null) {
      epoch.current = { sig: signature, adv: advance, view: pending, key: 1, kind: write && writeOnMount ? "write" : null };
    } else if (pending !== e.view) {
      epoch.current = { sig: signature, adv: advance, view: pending, key: e.key + 1, kind: pending === "write" ? "write" : null };
    } else if (advance !== e.adv) {
      epoch.current = { sig: signature, adv: advance, view: pending, key: e.key + 1, kind: "advance" };
    } else if (signature !== e.sig) {
      epoch.current = { sig: signature, adv: advance, view: pending, key: e.key + 1, kind: write ? "write" : null };
    }
  }
  const motion = epoch.current;
  const motionClass =
    motion.kind === "write" ? "mf-drum__write" : motion.kind === "advance" ? "mf-drum__adv" : undefined;

  const fmtT = useCallback(
    (t: number) =>
      formatTime
        ? formatTime(t)
        : new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" }),
    [formatTime],
  );

  /**
   * Fiducial notches. The recorder marks interval boundaries, so these are real
   * calendar boundaries chosen from the bucket granularity: the finest unit
   * that does not crowd the paper, strided when even the coarsest would. The
   * list is then thinned to whatever the width can carry without two labels
   * touching, which is what keeps a long window sane.
   */
  const notches = useMemo<TimeLabel[]>(() => {
    if (!n) return [];
    let picked: number[];
    if (notchEvery && notchEvery > 0) {
      picked = [];
      for (let i = 0; i < n; i += notchEvery) picked.push(i);
    } else {
      const byUnit = CAL_UNITS.map((u) => boundaryIndices(times, u));
      let chosen = byUnit.find(
        (idx) => idx.length + 1 >= NOTCH_MIN && idx.length + 1 <= NOTCH_MAX,
      );
      if (!chosen) {
        /* Nothing lands in range, so stride the COARSEST crowded unit: on a
           year of daily buckets that notches every second month, not every
           fifty-second hour, which would land on no boundary a reader knows. */
        const crowded = [...byUnit].reverse().find((idx) => idx.length + 1 > NOTCH_MAX);
        if (crowded && crowded.length) {
          const raw = Math.max(1, Math.ceil(crowded.length / (NOTCH_MAX - 1)));
          const stride = NICE_STRIDES.find((v) => v >= raw) ?? raw;
          chosen = crowded.filter((_, k) => k % stride === 0);
        }
      }
      if (!chosen || !chosen.length) {
        /* A window shorter than an hour crosses no calendar boundary at all, so
           an even stride is the only honest option left for the time axis. */
        const every = Math.max(1, Math.round(n / 6));
        chosen = [];
        for (let i = every; i < n; i += every) chosen.push(i);
      }
      picked = [0, ...chosen];
    }
    /* Thin on the words, not on the notches. A label is centred on its own
       notch wherever it fits and anchored to an edge only when centring would
       run it off the paper, so its extent is solved here and the render reuses
       the same anchor: the two can never disagree about where it landed. */
    const clear = narrow ? LABEL_CLEAR_NARROW : LABEL_CLEAR;
    const out: TimeLabel[] = [];
    let lastRight = Number.NEGATIVE_INFINITY;
    for (const i of picked) {
      const px = x(i);
      const text = fmtT(times[i]);
      const w = text.length * LABEL_CHAR_W;
      const anchor: TimeLabel["anchor"] =
        px - w / 2 < 0 ? "start" : px + w / 2 > innerW ? "end" : "middle";
      const left = anchor === "start" ? px : anchor === "end" ? px - w : px - w / 2;
      if (left - lastRight < clear) continue;
      out.push({ i, px, anchor, text });
      lastRight = left + w;
    }
    return out;
  }, [times, n, notchEvery, narrow, innerW, x, fmtT]);

  /**
   * The printed gain, solved per band. Each ceiling is written inside the band
   * it measures, so it can never be read as the foot of the band above it, and
   * a mirrored record prints ONE zero on the centre rule: the rule is where
   * both pens rest, and saying so is what makes the two sides read as two
   * scales opposed about it rather than one broken axis.
   */
  const gainLabels = useMemo(() => {
    const out: { key: string; y: number; text: string }[] = [];
    scaleBands.forEach((b) => {
      const span = Math.abs(b.span);
      const full = b.base + b.dir * b.span;
      out.push({
        key: `${b.channel.id}-full`,
        y: full + (b.dir === -1 ? GAIN_DROP : -GAIN_LIFT),
        text: `${gainNum(b.ceil)}${b.clipped ? "+" : ""}`,
      });
      if (!narrow && span >= GAIN_MIN_MID) {
        out.push({
          key: `${b.channel.id}-mid`,
          y: b.base + b.dir * b.span * 0.5 + GAIN_MID,
          text: gainNum(b.ceil / 2),
        });
      }
      if (mode !== "mirrored" && span >= GAIN_MIN_ZERO) {
        out.push({ key: `${b.channel.id}-zero`, y: b.base - GAIN_LIFT, text: "0" });
      }
    });
    if (mode === "mirrored" && scaleBands.length) {
      out.push({ key: "rule-zero", y: scaleBands[0].base + GAIN_MID, text: "0" });
    }
    return out;
  }, [scaleBands, mode, narrow]);

  /**
   * The margin. An over-range note is written first because it explains the
   * gain the rest of the record is drawn at, and its leader is drawn in the
   * pen's own colour so it points at the channel that ran off the top.
   */
  const marginNotes = useMemo<MarginNote[]>(() => {
    const over: MarginNote[] = [];
    if (overRangeNotes) {
      overRange.forEach((o, i) => {
        if (o.at < 0) return;
        const c = channels[i];
        over.push({
          at: o.at,
          label: `Peak ${(c.format ?? defaultNum)(o.peak)}`,
          tone: "neutral",
          mark: c.color,
        });
      });
    }
    /* One moment, one note.
       A caller that annotates a bucket owns that bucket: its note is written
       for this screen and says something more specific than the generic
       over-range note can. So a caller annotation SUPPRESSES an automatic note
       on the same sample, and it does so by bucket rather than by matching the
       text. Matching the text is what failed before: one screen wrote
       "Peak 1.2k fetches" against this component's "Peak 1,213", the keys never
       collided, and two identical marks landed on one sample and read as a
       fault in the instrument.
       A note a person wrote claims nothing: "Launched v2" and "Peak 1,213"
       say different things about the same sample, and both are true. */
    const claimed = new Set(annotations.filter((a) => a.kind !== "user").map((a) => a.at));
    const seen = new Set<string>();
    return [...over.filter((o) => !claimed.has(o.at)), ...annotations]
      .filter((a) => {
        const key = a.kind === "user" && a.id ? `user|${a.id}` : `${a.at}|${a.label}`;
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      })
      .slice(0, 5);
  }, [overRange, channels, annotations, overRangeNotes]);

  const indexFromEvent = useCallback(
    (clientX: number, rect: DOMRect) => {
      const px = clientX - rect.left - M.left;
      const i = Math.round((px / innerW) * (n - 1));
      return Math.min(n - 1, Math.max(0, i));
    },
    [M.left, innerW, n],
  );

  const svgRef = useRef<SVGSVGElement | null>(null);

  /** The sample the pointer is over now; read on release to tell a pick from a drag. */
  const lastIndex = useRef<number | null>(null);

  const onPointerDown = (e: ReactPointerEvent) => {
    if ((!onMeasure && !onPickSample) || empty) return;
    const rect = svgRef.current!.getBoundingClientRect();
    const i = indexFromEvent(e.clientX, rect);
    dragging.current = i;
    lastIndex.current = i;
    if (onMeasure) setCaliper({ a: i, b: i });
    (e.target as Element).setPointerCapture?.(e.pointerId);
  };
  const onPointerMove = (e: ReactPointerEvent) => {
    const rect = svgRef.current!.getBoundingClientRect();
    const i = indexFromEvent(e.clientX, rect);
    setHover(i);
    lastIndex.current = i;
    if (dragging.current != null && onMeasure) setCaliper({ a: dragging.current, b: i });
  };
  /**
   * Ends a press. A release on the sample it started on is a pick; a pointer
   * that leaves the chart mid-press is never one, because nobody chose a sample
   * by walking off the edge.
   */
  const finishPress = (pick: boolean) => {
    const start = dragging.current;
    if (start != null && caliper && onMeasure) {
      const from = Math.min(caliper.a, caliper.b);
      const to = Math.max(caliper.a, caliper.b);
      if (to > from) onMeasure(from, to);
      else {
        setCaliper(null);
        if (pick) onPickSample?.(start);
      }
    } else if (start != null && pick && lastIndex.current === start) {
      onPickSample?.(start);
    }
    dragging.current = null;
  };
  const onPointerUp = () => finishPress(true);

  /**
   * The notes a person wrote, held to the samples this chart actually has. A
   * note that starts before the window is drawn from its first sample, and a
   * range is a band only when it covers more than one sample.
   */
  const userNotes = useMemo(() => {
    if (!n) return [];
    const clamp = (i: number) => Math.min(n - 1, Math.max(0, Math.round(i)));
    return annotations
      .filter((a) => a.kind === "user")
      .map((a) => {
        const at = clamp(a.at);
        const to = a.to != null ? clamp(a.to) : null;
        return { ...a, at, to: to != null && to > at ? to : null, ink: a.color ?? "var(--mf-ink-2)" };
      });
  }, [annotations, n]);

  /* Read off each pen's own samples: a pool's maximum can belong to a channel
     the label is not describing. */
  const anyClipped = channels.some((c, i) => {
    const peak = penPeak[i];
    return !c.ghost && peak != null && peak > (scales[i]?.ceil ?? Number.POSITIVE_INFINITY);
  });

  return (
    <div ref={wrapRef} className={`mf-drum${className ? ` ${className}` : ""}`}>
      <svg
        ref={svgRef}
        width="100%"
        height={height}
        viewBox={`0 0 ${Math.max(width, 200)} ${height}`}
        role="img"
        aria-label={`Chart of ${channels.map((c) => c.label).join(" and ")} over ${n} intervals${
          anyClipped ? ". The scale is clipped; the true peak is given under the chart." : ""
        }${
          userNotes.length
            ? `. ${userNotes.length} ${userNotes.length === 1 ? "annotation" : "annotations"}: ${userNotes
                .map((a) => a.label)
                .join("; ")}.`
            : ""
        }`}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={() => {
          setHover(null);
          finishPress(false);
        }}
        style={{
          touchAction: "pan-y",
          cursor: onMeasure && !empty ? "col-resize" : onPickSample && !empty ? "pointer" : "default",
        }}
      >
        <defs>
          {/* The plot area. Grid lines and series are clipped to it. */}
          <clipPath id={`clip-${uid}`}>
            <rect x={0} y={0} width={innerW} height={innerH} />
          </clipPath>
          {bands.map((b) => (
            <linearGradient
              key={b.channel.id}
              id={`fill-${uid}-${b.channel.id}`}
              x1="0"
              y1={b.dir === -1 ? "1" : "0"}
              x2="0"
              y2={b.dir === -1 ? "0" : "1"}
            >
              <stop offset="0%" stopColor={b.channel.color} stopOpacity={0.16} />
              <stop offset="70%" stopColor={b.channel.color} stopOpacity={0.05} />
              <stop offset="100%" stopColor={b.channel.color} stopOpacity={0.01} />
            </linearGradient>
          ))}
          {/* The hatch for the difference between the two mirrored series. */}
          <pattern
            id={`hatch-${uid}`}
            width="6"
            height="6"
            patternUnits="userSpaceOnUse"
            patternTransform="rotate(45)"
          >
            <line x1="0" y1="0" x2="0" y2="6" stroke="var(--mf-ink-3)" strokeWidth="1" opacity="0.34" />
          </pattern>
        </defs>

        <g transform={`translate(${M.left},${M.top})`}>
          {/* ── horizontal grid lines ────────────────────────────────── */}
          {mode === "stacked"
            ? scaleBands.map((b) =>
                [0, 0.25, 0.5, 0.75, 1].map((f, k) => (
                  <line
                    key={`${b.channel.id}-${k}`}
                    x1={0}
                    x2={innerW}
                    y1={b.base - b.span * f}
                    y2={b.base - b.span * f}
                    stroke={k === 0 ? "var(--mf-rule-major)" : "var(--mf-rule-minor)"}
                    strokeWidth={1}
                    shapeRendering="crispEdges"
                  />
                )),
              )
            : scaleBands.flatMap((b) =>
                [0, 0.25, 0.5, 0.75, 1].map((f, k) => (
                  <line
                    key={`${b.channel.id}-rule-${k}`}
                    x1={0}
                    x2={innerW}
                    y1={b.base + b.dir * b.span * f}
                    y2={b.base + b.dir * b.span * f}
                    stroke={f === 0 ? "var(--mf-rule-major)" : "var(--mf-rule-minor)"}
                    strokeWidth={1}
                    shapeRendering="crispEdges"
                  />
                )),
              )}

          {/* ── gain, read off the left edge. Each pen's own full-scale value
                 is printed against its own band, because the channels are not
                 on one scale and the reader must never have to assume they
                 are. A narrow record drops the mid-scale label but keeps the
                 top of scale: a trace with no printed ceiling has no vertical
                 scale at all, and the readout below is not a substitute for
                 one. A clipped ceiling prints as "N+", which is exactly what
                 the top line means once samples are drawn against it. ── */}
          {gainLabels.map((g) => (
            <text
              key={g.key}
              x={narrow ? -7 : -10}
              y={g.y}
              textAnchor="end"
              className="mf-drum__gain"
            >
              {g.text}
            </text>
          ))}

          {/* ── time axis ticks and labels ───────────────────────────── */}
          {!empty &&
            notches.map(({ i, px, anchor, text }) => (
              <g key={`notch-${i}`}>
                <line
                  x1={px}
                  x2={px}
                  y1={innerH}
                  y2={innerH + 5}
                  stroke="var(--mf-rule-strong)"
                  strokeWidth={1}
                  shapeRendering="crispEdges"
                />
                <text x={px} y={innerH + 17} textAnchor={anchor} className="mf-drum__time">
                  {text}
                </text>
              </g>
            ))}

          <g clipPath={`url(#clip-${uid})`}>
            {/* A note a person wrote over a range: a light band behind the
                series, so the lines stay readable through it. */}
            {!empty &&
              userNotes.map((a) =>
                a.to != null ? (
                  <rect
                    key={`band-${a.id ?? `${a.at}-${a.label}`}`}
                    x={x(a.at)}
                    y={0}
                    width={Math.max(1, x(a.to) - x(a.at))}
                    height={innerH}
                    fill={a.ink}
                    opacity={0.07}
                  />
                ) : null,
              )}
            <g
              key={motion.key}
              className={motionClass}
              data-pen={pending === "pending" ? "pending" : undefined}
              style={
                motion.kind === "advance"
                  ? ({ "--mf-adv": `${x(1) - x(0)}px` } as CSSProperties)
                  : undefined
              }
            >
            {/* ── the mirrored gap, hatched between the two pens.
                   The hatch means "the interval between these two readings", so
                   it is only true when both pens share one gain. At per-channel
                   gain it is not drawn, and the readout says why. ────────── */}
            {mode === "mirrored" && !perChannel && channels.length >= 2 && !empty && (
              <path
                d={(() => {
                  const up = bands[0];
                  const dn = bands[1];
                  const top: [number, number][] = [];
                  const bot: [number, number][] = [];
                  for (let i = 0; i < n; i++) {
                    const a = Math.min(up.channel.values[i] ?? 0, ceiling);
                    const b = Math.min(dn.channel.values[i] ?? 0, ceiling);
                    top.push([x(i), up.base - (a / ceiling) * up.span]);
                    bot.push([x(i), dn.base + (b / ceiling) * dn.span]);
                  }
                  return `${writtenPath(top)} L${bot[bot.length - 1][0]},${bot[bot.length - 1][1]} ${writtenPath([...bot].reverse()).replace(/^M/, "L")} Z`;
                })()}
                fill={`url(#hatch-${uid})`}
                stroke="none"
              />
            )}

            {/* ── the series ─────────────────────────────────────────── */}
            {bands.map(({ channel, base, span, dir, ceil }) => {
              const pts: [number, number][] = [];
              const stops: [number, number][] = [];
              for (let i = 0; i < n; i++) {
                const v = channel.values[i];
                if (v == null) continue;
                const held = Math.min(v, ceil);
                const py = base + dir * (held / ceil) * span;
                pts.push([x(i), py]);
                if (v > ceil) stops.push([x(i), py]);
              }
              if (!pts.length) return null;
              const line =
                channel.pen === "stepped" ? steppedPath(pts) : writtenPath(pts);
              const area = `${line} L${pts[pts.length - 1][0]},${base} L${pts[0][0]},${base} Z`;
              const last = pts[pts.length - 1];
              return (
                <g key={channel.id}>
                  {!channel.ghost && mode !== "mirrored" && (
                    <path d={area} fill={`url(#fill-${uid}-${channel.id})`} stroke="none" />
                  )}
                  {/* the line itself */}
                  <path
                    d={line}
                    fill="none"
                    stroke={channel.color}
                    strokeWidth={channel.ghost ? 1.25 : 2}
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    strokeDasharray={channel.ghost ? "3 3" : undefined}
                    opacity={channel.ghost ? 0.55 : 1}
                  />
                  {/* Over-range marks. A sample above the top of the scale is
                      held on the top line and marked with a short bar and a
                      tick. Its real value is written in the margin, never
                      cropped away in silence. */}
                  {stops.map(([sx, sy], k) => (
                    <g key={`${channel.id}-stop-${k}`} opacity={channel.ghost ? 0.55 : 1}>
                      <line
                        x1={sx - 3.5}
                        x2={sx + 3.5}
                        y1={sy - dir * 1}
                        y2={sy - dir * 1}
                        stroke={channel.color}
                        strokeWidth={2}
                        strokeLinecap="butt"
                        shapeRendering="crispEdges"
                      />
                      <line
                        x1={sx}
                        x2={sx}
                        y1={sy}
                        y2={sy - dir * 6}
                        stroke={channel.color}
                        strokeWidth={1.4}
                        strokeLinecap="butt"
                      />
                    </g>
                  ))}
                  {/* The newest sample of a live chart: a dot and a short tick
                      that draws once and holds. No pulse. */}
                  {live && !channel.ghost && (
                    <g className="mf-drum__head">
                      <path
                        className="mf-drum__tick"
                        d={`M${last[0]},${last[1]} L${last[0]},${last[1] - dir * 8}`}
                        pathLength={1}
                        fill="none"
                        stroke={channel.color}
                        strokeWidth={1.2}
                        opacity={0.7}
                      />
                      <circle cx={last[0]} cy={last[1]} r={2.4} fill={channel.color} />
                    </g>
                  )}
                </g>
              );
            })}
            </g>

            {/* ── the dragged range selection ─────────────────────────── */}
            {caliper && (
              <g>
                <rect
                  x={Math.min(x(caliper.a), x(caliper.b))}
                  y={0}
                  width={Math.abs(x(caliper.b) - x(caliper.a))}
                  height={innerH}
                  fill="var(--mf-accent)"
                  opacity={0.1}
                />
                {[caliper.a, caliper.b].map((i, k) => (
                  <line
                    key={k}
                    x1={x(i)}
                    x2={x(i)}
                    y1={0}
                    y2={innerH}
                    stroke="var(--mf-accent)"
                    strokeWidth={1.2}
                    shapeRendering="crispEdges"
                  />
                ))}
              </g>
            )}

            {/* ── the hover line under the pointer ────────────────────── */}
            {hover != null && !empty && dragging.current == null && (
              <line
                x1={x(hover)}
                x2={x(hover)}
                y1={0}
                y2={innerH}
                stroke="var(--mf-ink-3)"
                strokeWidth={1}
                shapeRendering="crispEdges"
              />
            )}

          </g>

          {/* A note a person wrote: a dashed rule across the whole plot, a
              second fainter rule where a range ends, and a small flag standing
              on the top edge. Drawn over the series, outside the clip, so the
              flag sits in the top margin at any width. The words are in the
              margin (or, on a narrow chart, in the flag's own title). */}
          {!empty &&
            userNotes.map((a) => {
              const px = x(a.at);
              return (
                <g key={`rule-${a.id ?? `${a.at}-${a.label}`}`}>
                  <title>{a.label}</title>
                  <line
                    x1={px}
                    x2={px}
                    y1={0}
                    y2={innerH}
                    stroke={a.ink}
                    strokeWidth={1.2}
                    strokeDasharray="4 3"
                  />
                  {a.to != null && (
                    <line
                      x1={x(a.to)}
                      x2={x(a.to)}
                      y1={0}
                      y2={innerH}
                      stroke={a.ink}
                      strokeWidth={1}
                      strokeDasharray="2 3"
                      opacity={0.7}
                    />
                  )}
                  <path
                    d={`M${px},0 V-11 h7 l-2,2.6 l2,2.6 h-7`}
                    fill={a.ink}
                    stroke={a.ink}
                    strokeWidth={1}
                    strokeLinejoin="round"
                  />
                </g>
              );
            })}

          {/* A narrow chart has no margin to write in, so a noted moment keeps
              its mark on the time axis, longer than a tick and in the note's
              own tone, even though the words cannot fit. It sits beside the
              axis ticks, so it is never read as part of a series, and the
              over-range notes are left out here because the legend below
              already states every clipped peak in words. */}
          {narrow &&
            !empty &&
            annotations
              .filter((a) => a.kind !== "user")
              .slice(0, 3)
              .map((a) => (
                <line
                  key={`mark-${a.at}-${a.label}`}
                  x1={x(a.at)}
                  x2={x(a.at)}
                  y1={innerH}
                  y2={innerH + 6}
                  stroke={
                    a.tone === "warn"
                      ? "var(--mf-danger)"
                      : a.tone === "good"
                        ? "var(--mf-success)"
                        : "var(--mf-drum-label)"
                  }
                  strokeWidth={2}
                  strokeLinecap="butt"
                >
                  <title>{a.label}</title>
                </line>
              ))}

          {/* ── margin notes, written beside the chart. A note a person
                 wrote has a solid leader ending in a flag on its own rule; a
                 note read off the data has a dotted leader ending in a dot. ── */}
          {!narrow &&
            marginNotes.map((a, k) => {
              const user = a.kind === "user";
              const px = x(Math.min(n - 1, Math.max(0, a.at)));
              const py = 11 + k * ((innerH - 22) / Math.max(1, marginNotes.length - 1 || 1));
              const label = a.label.length > 22 ? `${a.label.slice(0, 21)}…` : a.label;
              const tone = user
                ? (a.color ?? "var(--mf-ink-2)")
                : a.tone === "warn"
                  ? "var(--mf-danger)"
                  : a.tone === "good"
                    ? "var(--mf-success)"
                    : "var(--mf-drum-label)";
              const mark = a.mark ?? tone;
              return (
                <g key={user && a.id ? `user-${a.id}` : `${a.at}-${a.label}`}>
                  {user && a.label.length > 22 ? <title>{a.label}</title> : null}
                  <line
                    x1={px}
                    y1={py}
                    x2={innerW + 8}
                    y2={py}
                    stroke={mark}
                    strokeWidth={1}
                    strokeDasharray={user ? undefined : "2 3"}
                    opacity={user ? 0.5 : 0.7}
                  />
                  {user ? (
                    <path
                      d={`M${px},${py + 4} V${py - 5} h6 l-1.8,2.2 l1.8,2.2 h-6`}
                      fill={mark}
                      stroke={mark}
                      strokeWidth={1}
                      strokeLinejoin="round"
                    />
                  ) : (
                    <circle cx={px} cy={py} r={2} fill={mark} />
                  )}
                  <text x={innerW + 13} y={py + 3.5} className="mf-drum__annot" fill={tone}>
                    {label}
                  </text>
                </g>
              );
            })}
        </g>

        {empty && (
          <text
            x={M.left + innerW / 2}
            y={M.top + innerH / 2}
            textAnchor="middle"
            className="mf-drum__empty"
          >
            {emptyLabel}
          </text>
        )}
      </svg>

      {/* Legend and values. Sits below the chart rather than floating over it
          as a tooltip, and shows the hovered sample, or the newest one. */}
      {/* A live chart moves every few seconds; announcing each step would
          drown a screen reader, so its legend is not a live region. */}
      <div className="mf-drum__readout" aria-live={advance == null ? "polite" : "off"}>
        {demo && <span className="mf-drum__stamp">Demo data, not real traffic</span>}
        {channels.map((c, ci) => {
          /* A ghost with nothing to compare against is not drawn on the paper,
             so it is not listed here either: an entry for a trace that does not
             exist could only print another channel's numbers. */
          if (c.ghost && !wrote[ci]) return null;
          const i = hover ?? n - 1;
          const raw = c.values[i];
          const read = raw != null && Number.isFinite(raw);
          const sc = scales[ci];
          const peak = penPeak[ci];
          const write = c.format ?? defaultNum;
          /* A ghost reads on its host's gain. The host states that gain, and
             the ghost never restates it, because the ceiling, the clipping and
             the peak in that pool are the host's and not its own. */
          let note: string | null = null;
          if (!c.ghost && sc) {
            if (peak != null && peak > sc.ceil) {
              note = `scale 0–${gainNum(sc.ceil)}, peak ${write(peak)} is off the chart`;
            } else if (perChannel && scaleBands.length > 1) {
              note = `scale 0–${gainNum(sc.ceil)}`;
            }
          }
          return (
            <span key={c.id} className="mf-drum__chan">
              <PenSwatch channel={c} />
              {/* The figure sits in the label's own text flow. Held apart as a
                  sibling it wraps out of its phrase on a narrow sheet and the
                  number is stranded inside the words that name it. */}
              <span className="mf-drum__chan-label">
                {mode === "mirrored" && (
                  /* Which way this pen writes off the centre rule. Stated, so
                     "further from the rule" is never read as "falling". */
                  <span className="mf-drum__dir" aria-hidden="true">
                    {bands.find((b) => b.channel.id === c.id)?.dir === -1 ? "▲" : "▼"}
                  </span>
                )}
                {c.label}{" "}
                <span className="mf-drum__chan-val mf-data">
                  {read ? write(raw as number) : "—"}
                </span>
                {!read && <span className="mf-sr">no value in this interval</span>}
              </span>
              {note && <span className="mf-drum__gain-note mf-data">{note}</span>}
            </span>
          );
        })}
        {hover != null && times[hover] != null && (
          <span className="mf-drum__at mf-data">
            {new Date(times[hover]).toLocaleString(undefined, {
              month: "short",
              day: "numeric",
              hour: "2-digit",
              minute: "2-digit",
            })}
          </span>
        )}
        {/* What the space between the two series does and does not mean. The
            hatch measures a difference, so it can only be drawn when both
            series share one scale; at separate scales the distance means nothing. */}
        {gainNote && mode === "mirrored" && channels.length >= 2 && !empty && (
          <span className="mf-drum__note">
            {perChannel
              ? "Separate scales. Each series has its own scale, so compare the shapes, not the heights."
              : "Shared scale. The hatched area is the difference between the two series."}
          </span>
        )}
        {caliper && Math.abs(caliper.b - caliper.a) > 0 && (
          <button
            type="button"
            className="mf-drum__clear"
            onClick={() => setCaliper(null)}
          >
            Clear selection
          </button>
        )}
      </div>
    </div>
  );
}

export default Drum;
