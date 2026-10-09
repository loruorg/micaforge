/**
 * What the uptime and status-page endpoints answer, and the small rules the
 * two monitoring screens share.
 *
 * The shapes are copied from the A2 API spec (§1 and §2) field for field and
 * nothing is added to them. Where a screen wants a figure the API does not
 * send (an average across monitors, say), the screen computes it and says
 * that it did, rather than this file pretending the server sent it.
 *
 * The query keys live here because both screens read the same monitor list:
 * the status-page editor picks from it, and one key means one request and one
 * answer for both.
 */
import { ApiError } from "../../lib/api";
import type {
  Impact,
  IncidentBase,
  IncidentKind,
  IncidentStatus,
  MonitorState,
} from "../../components/status/words";

/* ── uptime (API spec §1) ────────────────────────────────────────────────── */

export type Method = "GET" | "HEAD" | "POST";

export interface Monitor {
  id: string;
  site_id: string;
  name: string;
  url: string;
  method: Method;
  interval_minutes: number;
  timeout_ms: number;
  status_min: number;
  status_max: number;
  keyword: string | null;
  json_path: string | null;
  json_expected: string | null;
  ssl_check: boolean;
  ssl_expiry_days: number;
  failure_threshold: number;
  channels: unknown;
  enabled: boolean;
  state: MonitorState;
  consecutive_failures: number;
  last_checked_at: string | null;
  next_check_at: string;
  region: string;
  created_at: string;
}

export type MonitorInput = Omit<
  Monitor,
  | "id"
  | "site_id"
  | "state"
  | "consecutive_failures"
  | "last_checked_at"
  | "next_check_at"
  | "region"
  | "created_at"
>;

export interface CheckResult {
  ok: boolean;
  status_code: number | null;
  latency_ms: number;
  error: string | null;
  ssl_days_left: number | null;
  region: string;
  checked_at: string;
}

export interface MonitorSummary {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  state: MonitorState;
  last_checked_at: string | null;
  last_latency_ms: number | null;
  ssl_days_left: number | null;
  uptime_24h: number | null;
  uptime_7d: number | null;
  uptime_30d: number | null;
  avg_latency_24h_ms: number | null;
}

export interface UptimeSummary {
  region: string;
  monitors: MonitorSummary[];
  open_incidents: number;
}

export type SeriesRange = "24h" | "7d" | "30d";

export interface SeriesPoint {
  t: number;
  checks: number;
  failures: number;
  latency_avg_ms: number | null;
  latency_p95_ms: number | null;
}

export interface MonitorSeries {
  range: SeriesRange;
  uptime: number | null;
  points: SeriesPoint[];
}

export interface Check {
  checked_at: string;
  ok: boolean;
  status_code: number | null;
  latency_ms: number;
  error: string | null;
  ssl_days_left: number | null;
  region: string;
}

export interface Incident {
  id: string;
  monitor_id: string;
  monitor_name: string;
  started_at: string;
  resolved_at: string | null;
  cause: string;
  failed_checks: number;
  duration_secs: number;
}

/* ── status pages (API spec §2) ──────────────────────────────────────────── */

export interface StatusPageMonitor {
  monitor_id: string;
  display_name: string | null;
  position: number;
  name: string;
}

export interface StatusPage {
  id: string;
  site_id: string;
  slug: string;
  title: string;
  description: string;
  logo_url: string | null;
  accent_color: string;
  footer_label: string | null;
  footer_url: string | null;
  published: boolean;
  public_url: string;
  monitors: StatusPageMonitor[];
  created_at: string;
}

export interface StatusPageInput {
  slug: string;
  title: string;
  description?: string;
  logo_url?: string | null;
  accent_color?: string;
  footer_label?: string | null;
  footer_url?: string | null;
  published?: boolean;
  monitors: { monitor_id: string; display_name?: string | null }[];
}

export interface StatusUpdate {
  id: string;
  status: IncidentStatus;
  body: string;
  automatic: boolean;
  created_at: string;
}

export interface StatusIncident extends IncidentBase {
  page_id: string;
  automatic: boolean;
  updates: StatusUpdate[];
}

export interface NewStatusIncident {
  kind: IncidentKind;
  title: string;
  status: IncidentStatus;
  impact: Impact;
  body: string;
  monitor_ids?: string[];
  scheduled_start?: string;
  scheduled_end?: string;
}

/* ── query keys ──────────────────────────────────────────────────────────── */

export const keys = {
  monitors: (site: number | undefined) => ["uptime", "monitors", site] as const,
  summary: (site: number | undefined) => ["uptime", "summary", site] as const,
  incidents: (site: number | undefined) => ["uptime", "incidents", site] as const,
  series: (id: string, range: SeriesRange) => ["uptime", "series", id, range] as const,
  checks: (id: string) => ["uptime", "checks", id] as const,
  pages: (site: number | undefined) => ["status-pages", site] as const,
  pageIncidents: (pageId: string) => ["status-pages", "incidents", pageId] as const,
};

/** Monitors are checked every few minutes, so their reads refresh each minute. */
export const LIVE_MS = 60_000;

/* ── words and figures ───────────────────────────────────────────────────── */

/** "Every minute", "Every 5 minutes". */
export function everyText(minutes: number): string {
  return minutes === 1 ? "Every minute" : `Every ${minutes} minutes`;
}

/**
 * A length of time in the largest honest units. The shared `duration` stops at
 * hours, and "75h 12m" makes a reader do arithmetic that "3d 3h" does not.
 */
export function spanText(secs: number | null | undefined): string {
  if (secs == null || !Number.isFinite(secs) || secs < 0) return "—";
  const s = Math.round(secs);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return s % 60 ? `${m}m ${s % 60}s` : `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}

/** "12 days", "1 day". */
export const daysText = (n: number | null | undefined): string =>
  n == null || !Number.isFinite(n) ? "—" : `${n} ${n === 1 ? "day" : "days"}`;

/**
 * The problem in words and the way out of it. A 403 keeps the server's own
 * sentence, because the server knows which role the action needs.
 */
export function problem(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network")
      return "Could not reach the Micaforge server. Check that it is running, then try again.";
    if (e.status === 401) return "Your session has expired. Sign in again to continue.";
    if (e.status === 403)
      return `${e.message} Ask an owner or admin of this organisation to change your role.`;
    if (e.status === 404) return `${e.message} Reload the page to see what exists now.`;
    return e.message;
  }
  if (e instanceof Error && e.message) return e.message;
  return "Something failed and the server did not say what. Try again.";
}

/** The field a server validation error names, when it names one. */
export const fieldOf = (e: unknown): string | undefined =>
  e instanceof ApiError && e.status >= 400 && e.status < 500 ? e.field : undefined;

/** True for an http(s) URL with a host. */
export function isHttpUrl(v: string, httpsOnly = false): boolean {
  try {
    const u = new URL(v);
    return (httpsOnly ? u.protocol === "https:" : u.protocol === "https:" || u.protocol === "http:") && !!u.hostname;
  } catch {
    return false;
  }
}

/** The local timezone name for a series request; empty when unknown. */
export function localZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return "";
  }
}
