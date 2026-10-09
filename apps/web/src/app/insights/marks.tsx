/**
 * The marks and words the two Insights screens share: severity, status, the
 * metric's name, and the change from normal.
 *
 * They live in one file because an investigation is read twice, once as a row
 * in the list and once as the detail page, and the two readings must agree to
 * the character. A "Warning" that is a triangle in the list and a dot on the
 * detail page, or a change of +25% in one place and +24.6% in the other, would
 * make the reader wonder which one is true.
 *
 * Every mark is a drawn glyph AND a word. Colour is a third cue on top of
 * those two, never the only one, and none of these marks is blue or orange:
 * blue means people and orange means AI everywhere in this product, and a
 * severity is neither.
 */
import type { ReactNode, SVGProps } from "react";

import { ApiError } from "../../lib/api";
import type {
  Investigation,
  InvestigationEvent,
  InvestigationSeverity,
  InvestigationStatus,
} from "../../lib/api";
import { num, pct } from "../../lib/format";
import { asFraction } from "../record/TopSheets";
import {
  IconAlert,
  IconCheck,
  IconClock,
  IconInfo,
  IconOut,
  IconPartial,
  IconReplay,
} from "../../components/react/icons";

/* ── two glyphs the shared set has no word for ─────────────────────────────
   Drawn in the same grammar as components/react/icons.tsx (a 16-unit grid, a
   1.4 stroke, round caps), and kept here because only this area needs them. */

function Glyph({ size = 12, children, ...rest }: SVGProps<SVGSVGElement> & { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      {...rest}
    >
      {children}
    </svg>
  );
}

/** Open: a ring with nothing inside it yet. */
const GlyphOpen = ({ size }: { size?: number }) => (
  <Glyph size={size}>
    <circle cx={8} cy={8} r={5.6} />
  </Glyph>
);

/** Critical: the warning triangle, filled, so it differs from a warning by shape. */
const GlyphCritical = ({ size }: { size?: number }) => (
  <Glyph size={size}>
    <path d="M8 1.8 15 13.5H1z" fill="currentColor" />
    <path d="M8 6v3.4" stroke="var(--mf-ground-2)" />
  </Glyph>
);

/* ── severity ──────────────────────────────────────────────────────────── */

const SEVERITY: Record<InvestigationSeverity, { word: string; Glyph: (p: { size?: number }) => ReactNode }> = {
  info: { word: "Info", Glyph: IconInfo },
  warning: { word: "Warning", Glyph: IconAlert },
  critical: { word: "Critical", Glyph: GlyphCritical },
};

export function SeverityMark({ value }: { value: InvestigationSeverity }) {
  const s = SEVERITY[value] ?? SEVERITY.info;
  return (
    <span className="mf-ins-mark" data-severity={value}>
      <s.Glyph size={12} />
      {s.word}
    </span>
  );
}

/* ── status ────────────────────────────────────────────────────────────── */

export const STATUS_WORDS: Record<InvestigationStatus, string> = {
  open: "Open",
  monitoring: "Monitoring",
  resolved: "Resolved",
};

const STATUS_GLYPH: Record<InvestigationStatus, (p: { size?: number }) => ReactNode> = {
  open: GlyphOpen,
  monitoring: IconClock,
  resolved: IconCheck,
};

export function StatusMark({ value }: { value: InvestigationStatus }) {
  const G = STATUS_GLYPH[value] ?? GlyphOpen;
  return (
    <span className="mf-ins-mark" data-status={value}>
      <G size={12} />
      {STATUS_WORDS[value] ?? value}
    </span>
  );
}

/* ── what was measured ─────────────────────────────────────────────────── */

const METRIC_WORDS: Record<string, string> = {
  visitors: "Visitors",
  pageviews: "Pageviews",
  bounce_rate: "Bounce rate",
  agent_fetches: "AI crawler fetches",
  answer_arrivals: "Visits from AI answers",
};

export const metricWords = (metric: string): string => METRIC_WORDS[metric] ?? metric;

/** The one metric where a fall is the good outcome. */
const fallIsGood = (metric: string): boolean => metric === "bounce_rate";

/** True for the metrics that count AI, so a screen can say so beside them. */
export const isAiMetric = (metric: string): boolean =>
  metric === "agent_fetches" || metric === "answer_arrivals";

/**
 * A measured value in the metric's own unit. A baseline is usually an average,
 * so a small figure keeps one decimal place rather than rounding 0.4 visitors
 * an hour to nothing.
 */
export function figure(metric: string, v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  if (metric === "bounce_rate") return pct(asFraction(v));
  if (Number.isInteger(v) || Math.abs(v) >= 100) return num(v);
  return v.toFixed(1);
}

/** "Visitors · source: news.ycombinator.com · 7d vs previous 7d". */
export function scopeLine(inv: Investigation): string {
  return [
    metricWords(inv.metric),
    inv.dimension ? `${inv.dimension}: ${inv.dimension_value ?? "(none)"}` : null,
    inv.window,
  ]
    .filter(Boolean)
    .join(" · ");
}

/* ── the change from normal ────────────────────────────────────────────── */

/** A signed percentage from a value already in percent: 25 → "+25%". */
export function changeText(changePct: number | null | undefined): string {
  if (changePct == null || !Number.isFinite(changePct)) return "no baseline";
  const abs = Math.abs(changePct);
  if (abs < 0.05) return "0%";
  const sign = changePct > 0 ? "+" : "−";
  return `${sign}${abs.toFixed(abs < 10 ? 1 : 0)}%`;
}

/**
 * The change, with its direction carried three ways: a caret, a sign, and a
 * tone. The tone follows the metric, because a bounce rate going up is bad
 * news while visitors going up is good news.
 */
export function Change({
  metric,
  changePct,
  direction,
}: {
  metric: string;
  changePct: number | null;
  direction?: "up" | "down";
}) {
  const up = changePct != null && Number.isFinite(changePct) ? changePct > 0 : direction === "up";
  const flat = changePct != null && Math.abs(changePct) < 0.05;
  const good = fallIsGood(metric) ? !up : up;
  const tone = flat ? "flat" : good ? "good" : "warn";
  return (
    <span className="mf-ins-change mf-data" data-tone={tone}>
      <span aria-hidden="true">{flat ? "—" : up ? "▲" : "▼"}</span>
      {changeText(changePct)}
      <span className="mf-sr">{flat ? ", no change" : up ? ", a rise" : ", a fall"}</span>
    </span>
  );
}

/* ── the timeline ──────────────────────────────────────────────────────── */

const EVENT: Record<InvestigationEvent["kind"], { word: string; Glyph: (p: { size?: number }) => ReactNode }> = {
  opened: { word: "Opened", Glyph: GlyphOpen },
  checked: { word: "Checked", Glyph: IconReplay },
  explained: { word: "Explained", Glyph: IconInfo },
  status: { word: "Status changed", Glyph: IconPartial },
  posted: { word: "Posted to chat", Glyph: IconOut },
};

export function EventMark({ kind }: { kind: InvestigationEvent["kind"] }) {
  const e = EVENT[kind] ?? { word: kind, Glyph: IconInfo };
  return (
    <span className="mf-ins-event__kind">
      <e.Glyph size={12} />
      {e.word}
    </span>
  );
}

/* ── time ──────────────────────────────────────────────────────────────── */

/** The full date and time, for a title or a screen reader beside a relative time. */
export function fullTime(iso: string | null | undefined): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  return Number.isFinite(t)
    ? new Date(t).toLocaleString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      })
    : iso;
}

/* ── refusals, in plain words ──────────────────────────────────────────── */

/**
 * Names the problem and the way out of it. `action` is what was refused, in
 * words that fit "Your role on this site does not allow …".
 */
export function insightError(e: unknown, action: string): string {
  if (e instanceof ApiError) {
    if (e.code === "network")
      return "Could not reach the Micaforge server. Check that it is running, then try again.";
    if (e.status === 401) return "Your session has expired. Sign in again to continue.";
    if (e.status === 403)
      return `Your role on this site does not allow ${action}. A member, an admin or an owner can do it.`;
    if (e.status === 429) return "Too many requests in a short time. Wait a minute, then try again.";
    return e.message;
  }
  if (e instanceof Error && e.message) return e.message;
  return "Something failed and the server did not say what. Try again.";
}
