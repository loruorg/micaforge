/**
 * The words and shapes of a status page, shared by its two readers.
 *
 * A status page is written in the dashboard and read on a public page by
 * people who have never seen the dashboard. If the two sides named a state
 * differently ("Identified" in one, "Cause found" in the other), the person
 * posting an update would not know what the reader sees. So every state word,
 * every impact word and the one rule for printing an uptime figure live here,
 * and both sides import them.
 *
 * This module is plain data and pure functions with no imports, on purpose:
 * the public status page is a small standalone island, and anything it pulls
 * in is weight that every visitor to a customer's status page downloads.
 */

/* ── the API's vocabulary (A2 API spec §2) ───────────────────────────────── */

export type MonitorState = "unknown" | "up" | "down";
export type IncidentKind = "incident" | "maintenance";
export type IncidentStatus =
  | "investigating"
  | "identified"
  | "monitoring"
  | "resolved"
  | "scheduled"
  | "in_progress"
  | "completed";
export type Impact = "none" | "minor" | "major" | "critical";
export type Overall = "operational" | "degraded" | "outage" | "maintenance" | "unknown";

/** The fields an incident has on both sides, before either side adds its own. */
export interface IncidentBase {
  id: string;
  kind: IncidentKind;
  title: string;
  status: IncidentStatus;
  impact: Impact;
  /** Affected monitors. Empty means the whole page. */
  monitor_ids: string[];
  scheduled_start: string | null;
  scheduled_end: string | null;
  created_at: string;
  resolved_at: string | null;
}

export interface PublicUpdate {
  status: IncidentStatus;
  body: string;
  created_at: string;
}

export type StatusIncidentPublic = IncidentBase & { updates: PublicUpdate[] };

export interface PublicDay {
  /** "YYYY-MM-DD", a UTC day. */
  date: string;
  checks: number;
  failures: number;
}

export interface PublicMonitor {
  id: string;
  name: string;
  state: MonitorState;
  uptime_90d: number | null;
  /** 90 entries, oldest first. */
  days: PublicDay[];
}

/** What `GET /api/status/{slug}` answers. */
export interface PublicStatus {
  slug: string;
  title: string;
  description: string;
  logo_url: string | null;
  accent_color: string;
  footer: { label: string; url: string } | null;
  overall: Overall;
  monitors: PublicMonitor[];
  active: StatusIncidentPublic[];
  scheduled: StatusIncidentPublic[];
  history: StatusIncidentPublic[];
  generated_at: string;
}

/* ── words ───────────────────────────────────────────────────────────────── */

export const STATUS_WORD: Record<IncidentStatus, string> = {
  investigating: "Investigating",
  identified: "Identified",
  monitoring: "Monitoring",
  resolved: "Resolved",
  scheduled: "Scheduled",
  in_progress: "In progress",
  completed: "Completed",
};

/** The statuses an incident of each kind can move through, in order. */
export const STATUSES_FOR: Record<IncidentKind, IncidentStatus[]> = {
  incident: ["investigating", "identified", "monitoring", "resolved"],
  maintenance: ["scheduled", "in_progress", "completed"],
};

export const IMPACT_WORD: Record<Impact, string> = {
  none: "No impact",
  minor: "Minor impact",
  major: "Major impact",
  critical: "Critical impact",
};

export const IMPACTS: Impact[] = ["none", "minor", "major", "critical"];

export const KIND_WORD: Record<IncidentKind, string> = {
  incident: "Incident",
  maintenance: "Maintenance",
};

/** The two statuses that close an incident. */
export const isClosing = (s: IncidentStatus): boolean => s === "resolved" || s === "completed";

/* ── figures ─────────────────────────────────────────────────────────────── */

/**
 * An uptime fraction as a percentage with two decimals, rounded DOWN.
 *
 * Rounding to nearest prints 99.996% as "100.00%", which claims a perfect
 * record that did not happen. Rounding down never claims more than was
 * measured, and only a true 1 prints as "100%".
 */
export function uptimeText(x: number | null | undefined): string {
  if (x == null || !Number.isFinite(x)) return "—";
  if (x >= 1) return "100%";
  const floored = Math.floor(Math.max(0, x) * 10000) / 100;
  return `${floored.toFixed(2)}%`;
}

const dateTime = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
});

const dateTimeZoned = new Intl.DateTimeFormat(undefined, {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  timeZoneName: "short",
});

/** A timestamp in the reader's own locale and time zone. */
export function when(iso: string | null | undefined, zoned = false): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  return (zoned ? dateTimeZoned : dateTime).format(t);
}
