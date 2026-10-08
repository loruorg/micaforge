/**
 * EXPERIMENT RESULTS.
 *
 * The single rule this file exists to enforce: NEVER RENDER A WINNER THE DATA
 * DOES NOT SUPPORT. A dashboard that declares a false winner is worse than one
 * that says "not enough data yet", because the false winner gets shipped.
 *
 * So every variant is reported with three things together (the observed rate,
 * a 95% Wilson interval around it, and the sample it came from), and the call at
 * the top is made from the interval on the DIFFERENCE (Newcombe's method, which
 * behaves at small samples), not from whichever rate happens to be higher today.
 * When that interval includes zero the screen says so plainly and estimates how
 * much more traffic would be needed. The word "winner" appears nowhere.
 *
 * The main view is the interval plot: every variant on one shared rate axis,
 * with its interval drawn as a real measured span. Two intervals that overlap
 * are visibly overlapping, so you do not have to read the numbers to see it.
 *
 * This file owns the experiment model; `ExperimentsScreen` imports it.
 */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import type { BreakdownRow, Filter } from "../../lib/api";
import { Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { num, pct } from "../../lib/format";
import { useAppState } from "../state";
import { useSite } from "../providers";
import { Reading, statsKey } from "./QueryScreen";

/* ═══ the experiment model ════════════════════════════════════════════════ */

export interface ExperimentVariant {
  key: string;
  weight: number;
  /** Exactly one variant is the baseline every other is measured against. */
  control?: boolean;
}

export type ExperimentStatus = "draft" | "running" | "stopped";

export interface Experiment {
  id: string;
  site_id: number;
  key: string;
  hypothesis: string;
  variants: ExperimentVariant[];
  goal_id: string | null;
  status: ExperimentStatus;
  started_at: string | null;
  ended_at: string | null;
  created_at?: string;
  updated_at?: string;
}

export interface Goal {
  id: string;
  site_id: number;
  name: string;
  kind: string;
  value_cents?: number | null;
}

export const experimentsKey = (siteId: number | undefined) => ["bench", "experiments", siteId];
export const goalsKey = (siteId: number | undefined) => ["bench", "goals", siteId];

const parseJson = <T,>(v: unknown, fallback: T): T => {
  if (v == null) return fallback;
  if (typeof v === "string") {
    try {
      return JSON.parse(v) as T;
    } catch {
      return fallback;
    }
  }
  return v as T;
};

export function normaliseExperiment(raw: Record<string, unknown>): Experiment {
  const variants = parseJson<ExperimentVariant[]>(raw.variants, []);
  const status = String(raw.status ?? "draft");
  return {
    id: String(raw.id ?? ""),
    site_id: Number(raw.site_id ?? 0),
    key: String(raw.key ?? ""),
    hypothesis: String(raw.hypothesis ?? ""),
    variants: Array.isArray(variants)
      ? variants.map((v) => ({
          key: String(v?.key ?? ""),
          weight: Number(v?.weight ?? 0) || 0,
          control: !!v?.control,
        }))
      : [],
    goal_id: (raw.goal_id as string | null) ?? null,
    status: status === "running" || status === "stopped" ? status : "draft",
    started_at: (raw.started_at as string | null) ?? null,
    ended_at: (raw.ended_at as string | null) ?? null,
    created_at: raw.created_at as string | undefined,
    updated_at: raw.updated_at as string | undefined,
  };
}

export function blankExperiment(siteId: number | undefined): Experiment {
  return {
    id: "",
    site_id: siteId ?? 0,
    key: "",
    hypothesis: "",
    variants: [
      { key: "control", weight: 50, control: true },
      { key: "treatment", weight: 50 },
    ],
    goal_id: null,
    status: "draft",
    started_at: null,
    ended_at: null,
  };
}

export const controlOf = (e: Experiment): ExperimentVariant | undefined =>
  e.variants.find((v) => v.control) ?? e.variants[0];

/* ═══ statistics ══════════════════════════════════════════════════════════ */

const Z95 = 1.959964;
const Z80POWER = 0.8416212;

export interface Interval {
  p: number;
  lo: number;
  hi: number;
}

/**
 * Wilson score interval. Chosen over the normal approximation because an A/B
 * test spends most of its life at small counts, where the normal interval runs
 * off the end of the scale and reports impossible bounds.
 */
export function wilson(k: number, n: number, z = Z95): Interval {
  if (!(n > 0)) return { p: 0, lo: 0, hi: 0 };
  const p = k / n;
  const z2 = z * z;
  const denom = 1 + z2 / n;
  const centre = (p + z2 / (2 * n)) / denom;
  const half = (z / denom) * Math.sqrt((p * (1 - p)) / n + z2 / (4 * n * n));
  return { p, lo: Math.max(0, centre - half), hi: Math.min(1, centre + half) };
}

/**
 * Newcombe's score interval for the difference of two proportions. Built from
 * the two Wilson intervals, so it keeps their behaviour at small samples.
 */
export function diffInterval(k1: number, n1: number, k2: number, n2: number): Interval {
  if (!(n1 > 0) || !(n2 > 0)) return { p: 0, lo: 0, hi: 0 };
  const a = wilson(k1, n1);
  const b = wilson(k2, n2);
  const d = a.p - b.p;
  const lo = d - Math.sqrt((a.p - a.lo) ** 2 + (b.hi - b.p) ** 2);
  const hi = d + Math.sqrt((a.hi - a.p) ** 2 + (b.p - b.lo) ** 2);
  return { p: d, lo, hi };
}

function normalCdf(z: number): number {
  const t = 1 / (1 + 0.2316419 * Math.abs(z));
  const d = 0.3989422804014327 * Math.exp((-z * z) / 2);
  const tail =
    d *
    t *
    (0.319381530 +
      t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
  return z >= 0 ? 1 - tail : tail;
}

/** Two-sided p-value from a pooled two-proportion z-test. */
export function twoProportionP(k1: number, n1: number, k2: number, n2: number): number | null {
  if (!(n1 > 0) || !(n2 > 0)) return null;
  const p1 = k1 / n1;
  const p2 = k2 / n2;
  const pool = (k1 + k2) / (n1 + n2);
  const se = Math.sqrt(pool * (1 - pool) * (1 / n1 + 1 / n2));
  if (!(se > 0)) return null;
  const z = (p1 - p2) / se;
  return Math.max(0, Math.min(1, 2 * (1 - normalCdf(Math.abs(z)))));
}

/**
 * Rough sample per variant to resolve the difference currently on screen, at 95%
 * confidence and 80% power. It is an estimate and is always labelled as one.
 */
export function sampleNeeded(p1: number, p2: number): number | null {
  const d = Math.abs(p1 - p2);
  if (!(d > 0)) return null;
  const pbar = (p1 + p2) / 2;
  const n =
    (Z95 * Math.sqrt(2 * pbar * (1 - pbar)) +
      Z80POWER * Math.sqrt(p1 * (1 - p1) + p2 * (1 - p2))) **
      2 /
    d ** 2;
  return Number.isFinite(n) ? Math.ceil(n) : null;
}

/** Below this, nothing is claimed at all, whatever the arithmetic says. */
const MIN_EXPOSURES = 100;
const MIN_CONVERSIONS = 5;

export interface Arm {
  key: string;
  isControl: boolean;
  exposed: number;
  converted: number;
  ci: Interval;
  /** Absent on the control. */
  diff?: Interval;
  p?: number | null;
  configured: boolean;
}

export type Call =
  | { kind: "no-goal" }
  | { kind: "no-data" }
  | { kind: "insufficient"; need: number | null; text: string }
  | { kind: "inconclusive"; arm: Arm; need: number | null; text: string }
  | { kind: "difference"; arm: Arm; text: string };

export function callIt(arms: Arm[], hasGoal: boolean): Call {
  if (!hasGoal) return { kind: "no-goal" };
  const control = arms.find((a) => a.isControl);
  const others = arms.filter((a) => !a.isControl);
  if (!control || arms.every((a) => a.exposed === 0)) return { kind: "no-data" };

  const thin = arms.filter((a) => a.exposed < MIN_EXPOSURES || a.converted < MIN_CONVERSIONS);
  if (thin.length) {
    const worst = thin[0];
    const need = others.length ? sampleNeeded(others[0].ci.p, control.ci.p) : null;
    return {
      kind: "insufficient",
      need,
      text: `Not enough data yet. ${worst.key} has ${num(worst.exposed)} ${
        worst.exposed === 1 ? "exposure" : "exposures"
      } and ${num(worst.converted)} ${worst.converted === 1 ? "conversion" : "conversions"}. Nothing here supports preferring one variant over another.`,
    };
  }

  // The strongest separation from control, measured on the interval and not on
  // the point estimate.
  const separated = others
    .filter((a) => a.diff && (a.diff.lo > 0 || a.diff.hi < 0))
    .sort((a, b) => Math.abs(b.diff!.p) - Math.abs(a.diff!.p));

  if (separated.length) {
    const a = separated[0];
    const d = a.diff!;
    const dir = d.p > 0 ? "above" : "below";
    return {
      kind: "difference",
      arm: a,
      text: `${a.key} converts ${pts(Math.abs(d.p))} ${dir} ${control.key}. The 95% interval on that difference runs ${pts(d.lo)} to ${pts(d.hi)} and does not include zero.${
        a.p != null ? ` Two-proportion test: p = ${a.p < 0.001 ? "under 0.001" : a.p.toFixed(3)}.` : ""
      }`,
    };
  }

  const widest = others
    .slice()
    .sort((a, b) => Math.abs(b.diff?.p ?? 0) - Math.abs(a.diff?.p ?? 0))[0];
  const need = widest ? sampleNeeded(widest.ci.p, control.ci.p) : null;
  return {
    kind: "inconclusive",
    arm: widest,
    need,
    text: widest?.diff
      ? `Not conclusive. ${widest.key} sits ${pts(widest.diff.p)} from ${control.key}, but the 95% interval on that difference runs ${pts(widest.diff.lo)} to ${pts(widest.diff.hi)}, which still includes zero.`
      : "Not conclusive. There is only one arm reporting, so there is nothing to compare it against.",
  };
}

/** Percentage points, always signed, because a difference has a direction. */
function pts(x: number): string {
  const v = x * 100;
  const s = Math.abs(v) < 0.05 ? "0.0" : Math.abs(v).toFixed(Math.abs(v) < 1 ? 2 : 1);
  return `${v > 0 ? "+" : v < 0 ? "−" : ""}${s} pts`;
}

/* ═══ the interval plot ═══════════════════════════════════════════════════ */

/**
 * Marks differ by SHAPE as well as tone, and every row carries its variant key
 * in text, so the plot reads with no colour at all.
 */
const MARK_TONES = ["var(--mf-people)", "var(--mf-s2)", "var(--mf-s3)", "var(--mf-s4)", "var(--mf-s5)"];

function IntervalPlot({ arms }: { arms: Arm[] }) {
  const control = arms.find((a) => a.isControl);
  const top = Math.max(0.001, ...arms.map((a) => a.ci.hi));
  // A round ceiling above the widest interval, so the scale reads as numbers.
  const mag = Math.pow(10, Math.floor(Math.log10(top)));
  const axisMax = Math.min(1, Math.ceil((top * 1.12) / (mag / 2)) * (mag / 2));
  const at = (v: number) => `${Math.max(0, Math.min(100, (v / axisMax) * 100))}%`;
  const guide = control && control.exposed > 0 ? at(control.ci.p) : null;

  return (
    <div className="mf-bn-iv">
      {/* One grid: every row's track occupies the same column, so the scale
          under it and the baseline guide through it are genuinely aligned. */}
      <div className="mf-bn-iv__grid">
        {arms.map((a, i) => (
          <div className="mf-bn-iv__row" key={a.key}>
            <span className="mf-bn-iv__name mf-data">
              {a.key}
              {a.isControl ? <span className="mf-bn-iv__base"> baseline</span> : null}
            </span>
            <div
              className="mf-bn-iv__track"
              role="img"
              aria-label={
                a.exposed > 0
                  ? `${a.key}: ${pct(a.ci.p, 2)} of ${num(a.exposed)}, 95% interval ${pct(a.ci.lo, 2)} to ${pct(a.ci.hi, 2)}`
                  : `${a.key}: no exposures recorded`
              }
            >
              {guide && !a.isControl ? (
                <span className="mf-bn-iv__guide" style={{ left: guide }} aria-hidden="true" />
              ) : null}
              {a.exposed > 0 ? (
                <>
                  <span
                    className="mf-bn-iv__span"
                    style={{ left: at(a.ci.lo), width: `calc(${at(a.ci.hi)} - ${at(a.ci.lo)})` }}
                  />
                  <span
                    className="mf-bn-iv__pt"
                    data-mark={i % MARK_TONES.length}
                    style={{ left: at(a.ci.p), background: MARK_TONES[i % MARK_TONES.length] }}
                  />
                </>
              ) : (
                <span className="mf-bn-iv__nothing">no exposures recorded</span>
              )}
            </div>
            <span className="mf-bn-iv__rate mf-data">{a.exposed > 0 ? pct(a.ci.p, 2) : "\u2014"}</span>
          </div>
        ))}

        <div className="mf-bn-iv__axis" aria-hidden="true">
          {[0, 0.25, 0.5, 0.75, 1].map((f) => (
            <span key={f} className="mf-bn-iv__tick" style={{ left: `${f * 100}%` }}>
              <span className="mf-bn-iv__tick-n">{pct(axisMax * f, axisMax < 0.05 ? 2 : 1)}</span>
            </span>
          ))}
        </div>
      </div>

      <p className="mf-bn-iv__legend">
        The bar is the 95% interval and the mark is the observed rate. Two bars that overlap are two
        rates the data cannot tell apart. The vertical line is the baseline&rsquo;s rate.
      </p>
    </div>
  );
}

/* ═══ results ═════════════════════════════════════════════════════════════ */

export function ExperimentResults({
  experiment,
  goal,
}: {
  experiment: Experiment;
  goal?: Goal | null;
}) {
  const { siteId } = useSite();
  const { range } = useAppState();

  // An experiment is read over its own window, not the screen's, because that is
  // the only window in which its variants were being served.
  const span = useMemo(() => {
    if (experiment.started_at) {
      return {
        start: experiment.started_at,
        end: experiment.ended_at ?? undefined,
        label: experiment.ended_at
          ? `from ${new Date(experiment.started_at).toLocaleDateString()} to ${new Date(experiment.ended_at).toLocaleDateString()}`
          : `since ${new Date(experiment.started_at).toLocaleDateString()}`,
        token: `${experiment.started_at}..${experiment.ended_at ?? "now"}`,
      };
    }
    return { start: undefined, end: undefined, label: `over the selected period (${range})`, token: range };
  }, [experiment.started_at, experiment.ended_at, range]);

  const baseEnv = useMemo(
    () => ({
      site: siteId ?? 0,
      ...(span.start ? { start: span.start, end: span.end } : { range }),
      // An experiment measures people. Crawler fetches never see a variant.
      audience: "human" as const,
      dimension: "variant",
      limit: 20,
    }),
    [siteId, span.start, span.end, range],
  );

  const expFilters: Filter[] = useMemo(
    () => [{ dimension: "experiment", op: "is", value: experiment.key }],
    [experiment.key],
  );

  const exposed = useQuery({
    queryKey: statsKey("/api/stats/breakdown", siteId, span.token, "human", expFilters, {
      of: "exposed",
      exp: experiment.key,
    }),
    enabled: siteId != null && !!experiment.key,
    queryFn: () => api.get<BreakdownRow[]>("/api/stats/breakdown", { ...baseEnv, filters: expFilters }),
  });

  const goalFilters: Filter[] = useMemo(
    () =>
      goal
        ? [...expFilters, { dimension: "goal", op: "is", value: goal.id } as Filter]
        : expFilters,
    [expFilters, goal],
  );

  const converted = useQuery({
    queryKey: statsKey("/api/stats/breakdown", siteId, span.token, "human", goalFilters, {
      of: "converted",
      exp: experiment.key,
      goal: goal?.id,
    }),
    enabled: siteId != null && !!experiment.key && !!goal,
    queryFn: () => api.get<BreakdownRow[]>("/api/stats/breakdown", { ...baseEnv, filters: goalFilters }),
  });

  const arms = useMemo<Arm[]>(() => {
    const exp = exposed.data?.data ?? [];
    const conv = converted.data?.data ?? [];
    const count = (rows: BreakdownRow[], key: string) => {
      const r = rows.find((x) => x.key === key || x.label === key);
      return r ? r.visitors || r.sessions || 0 : 0;
    };
    const control = controlOf(experiment);
    const configured = experiment.variants.map<Arm>((v) => {
      const n = count(exp, v.key);
      const k = Math.min(n, count(conv, v.key));
      return {
        key: v.key,
        isControl: control ? v.key === control.key : false,
        exposed: n,
        converted: k,
        ci: wilson(k, n),
        configured: true,
      };
    });
    // Anything the data holds that the experiment does not declare is shown
    // rather than dropped: a stray variant is usually a stale deploy.
    for (const r of exp) {
      if (!configured.some((a) => a.key === r.key) && r.key) {
        const n = r.visitors || r.sessions || 0;
        const k = Math.min(n, count(conv, r.key));
        configured.push({
          key: r.key,
          isControl: false,
          exposed: n,
          converted: k,
          ci: wilson(k, n),
          configured: false,
        });
      }
    }
    const base = configured.find((a) => a.isControl);
    if (base) {
      for (const a of configured) {
        if (a.isControl) continue;
        a.diff = diffInterval(a.converted, a.exposed, base.converted, base.exposed);
        a.p = twoProportionP(a.converted, a.exposed, base.converted, base.exposed);
      }
    }
    return configured;
  }, [exposed.data, converted.data, experiment]);

  const call = callIt(arms, !!goal);
  const loading = exposed.isLoading || (!!goal && converted.isLoading);
  const error = exposed.isError ? exposed.error : converted.isError ? converted.error : undefined;

  const columns: Column<Arm>[] = [
    {
      key: "variant",
      head: "Variant",
      render: (a) => (
        <span className="mf-data" title={a.configured ? undefined : "Recorded, but not declared on this experiment"}>
          {a.key}
          {a.isControl ? " (baseline)" : ""}
          {a.configured ? "" : ", not declared"}
        </span>
      ),
    },
    { key: "exposed", head: "Exposed", numeric: true, render: (a) => <span className="mf-data">{num(a.exposed)}</span> },
    { key: "converted", head: "Converted", numeric: true, render: (a) => <span className="mf-data">{num(a.converted)}</span> },
    {
      key: "rate",
      head: "Rate",
      numeric: true,
      render: (a) => <span className="mf-data">{a.exposed > 0 ? pct(a.ci.p, 2) : "—"}</span>,
    },
    {
      key: "ci",
      head: "95% interval",
      numeric: true,
      render: (a) => (
        <span className="mf-data">
          {a.exposed > 0 ? `${pct(a.ci.lo, 2)} – ${pct(a.ci.hi, 2)}` : "—"}
        </span>
      ),
    },
    {
      key: "diff",
      head: "Against baseline",
      numeric: true,
      render: (a) =>
        a.isControl ? (
          <span className="mf-bn-res__base">baseline</span>
        ) : a.diff && a.exposed > 0 ? (
          <span className="mf-data">
            {pts(a.diff.p)} <span className="mf-bn-res__ci">({pts(a.diff.lo)} to {pts(a.diff.hi)})</span>
          </span>
        ) : (
          <span className="mf-data">—</span>
        ),
    },
  ];

  return (
    <Panel className="mf-bn-res">
      <PanelHead
        title={`Results: ${experiment.key}`}
        note={
          goal
            ? `Measuring ${goal.name} ${span.label}. People only; an AI crawler never sees a variant.`
            : `No goal attached, so there is nothing to measure ${span.label}.`
        }
      />

      {experiment.hypothesis ? (
        <p className="mf-bn-res__hyp">
          <span className="mf-bn-res__hyp-lead">The claim under test:</span> {experiment.hypothesis}
        </p>
      ) : (
        <p className="mf-bn-res__hyp mf-bn-res__hyp--none">
          No hypothesis was written down. An experiment without one tends to find whatever it is
          asked to find afterwards.
        </p>
      )}

      <div className="mf-bn-res__body">
        <Reading
          loading={loading}
          error={error}
          onRetry={() => {
            void exposed.refetch();
            void converted.refetch();
          }}
          isEmpty={arms.length === 0 || arms.every((a) => a.exposed === 0)}
          lines={4}
          empty={
            <div className="mf-bn-res__empty">
              <p className="mf-bn-res__call" data-call="waiting">
                {experiment.status === "draft"
                  ? "Not started. Once the experiment is running and your code reads its variants, exposures land here."
                  : "No exposures recorded in this period. Check that your code sends the experiment and variant with its events."}
              </p>
            </div>
          }
        >
          <>
            <p
              className="mf-bn-res__call"
              data-call={call.kind}
              role="status"
            >
              {call.kind === "no-goal"
                ? "No goal attached. An experiment cannot report until it is measuring something. Attach the goal it is meant to move."
                : call.kind === "no-data"
                  ? "No exposures recorded in this period."
                  : call.text}
            </p>

            {(call.kind === "insufficient" || call.kind === "inconclusive") && call.need ? (
              <p className="mf-bn-res__need">
                Roughly <span className="mf-data">{num(call.need)}</span> exposures per variant would
                be needed to resolve a difference of the size currently on screen, at 95% confidence
                and 80% power. That figure is an estimate from today&rsquo;s rates and moves as they do.
              </p>
            ) : null}

            <IntervalPlot arms={arms} />

            <Sheet rows={arms} columns={columns} getKey={(a) => a.key} />

            <p className="mf-bn-res__caveat">
              One goal, one window, and no correction for repeated looks. Stopping an experiment the
              moment an interval clears zero is how a false result gets shipped, so decide the sample
              before you start and read it once.
            </p>
          </>
        </Reading>
      </div>
    </Panel>
  );
}

export default ExperimentResults;
