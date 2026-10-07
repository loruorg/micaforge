/**
 * The dashboard's single door to the API.
 *
 * Every read endpoint takes the same query envelope (docs/CONTRACT.md §3.1), so
 * it is built once here rather than assembled by hand at forty call sites.
 */

/**
 * Where API requests go. Empty in a build, so every request is same-origin
 * (`/api/...`): Caddy serves the dashboard and proxies the API on one address,
 * and one built image then works on every domain. `PUBLIC_API_URL` overrides it
 * for a dashboard served from a different origin; `pnpm dev` falls back to the
 * local API port.
 */
export const API_BASE: string =
  (typeof import.meta !== "undefined" && import.meta.env?.PUBLIC_API_URL) ||
  (typeof import.meta !== "undefined" && import.meta.env?.DEV ? "http://localhost:8330" : "");

/**
 * The absolute address of the API, for text a user copies into another site: a
 * script tag, a curl line, a shipper command. A relative path there would point
 * at the wrong host, so this resolves the same-origin case to the page's origin.
 */
export function publicOrigin(): string {
  if (API_BASE) return API_BASE;
  return typeof window !== "undefined" ? window.location.origin : "";
}

export type Audience = "human" | "agent" | "all";
export type Compare = "none" | "previous" | "year";
export type Granularity = "minute" | "hour" | "day" | "week" | "month";

export interface Filter {
  dimension: string;
  op:
    | "is"
    | "is_not"
    | "contains"
    | "not_contains"
    | "starts_with"
    | "ends_with"
    | "gt"
    | "lt"
    | "gte"
    | "lte"
    | "matches"
    | "is_set"
    | "is_not_set";
  value: string | number | (string | number)[];
}

export interface Envelope {
  site: number | string;
  range?: string;
  start?: string;
  end?: string;
  compare?: Compare;
  tz?: string;
  granularity?: Granularity;
  filters?: Filter[];
  segment?: string;
  audience?: Audience;
  limit?: number;
  page?: number;
  [extra: string]: unknown;
}

export interface ApiMeta {
  query?: Record<string, unknown>;
  rows?: number;
  sampled?: boolean;
  took_ms?: number;
}

export class ApiError extends Error {
  code: string;
  status: number;
  field?: string;
  constructor(status: number, code: string, message: string, field?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.field = field;
  }
}

function toQuery(env: Envelope): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(env)) {
    if (v == null || v === "") continue;
    if (k === "filters") {
      const f = v as Filter[];
      if (f.length) q.set("filters", JSON.stringify(f));
      continue;
    }
    q.set(k, String(v));
  }
  if (!q.has("tz")) {
    try {
      q.set("tz", Intl.DateTimeFormat().resolvedOptions().timeZone);
    } catch {
      /* an environment with no Intl falls back to the site's own timezone */
    }
  }
  return q.toString();
}

/**
 * The share credential, when the dashboard is being read through a share URL:
 * the link's slug, or the unlock token a password-protected link handed back.
 * It travels as `?share=` on every request (docs/CONTRACT.md §3.4), the one
 * mechanism the server reads.
 */
let shareKey: string | null = null;
export function setShareKey(k: string | null) {
  shareKey = k;
}

/** Append the share credential to a path, when one is set. */
function withShare(path: string): string {
  if (!shareKey) return path;
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}share=${encodeURIComponent(shareKey)}`;
}

async function request<T>(path: string, init?: RequestInit): Promise<{ data: T; meta: ApiMeta }> {
  const headers = new Headers(init?.headers);
  headers.set("accept", "application/json");
  if (init?.body && !headers.has("content-type")) headers.set("content-type", "application/json");

  let res: Response;
  try {
    res = await fetch(`${API_BASE}${withShare(path)}`, { ...init, headers, credentials: "include" });
  } catch (cause) {
    // A network failure is not a 500. The operator needs to know which it was.
    throw new ApiError(0, "network", "Could not reach the Micaforge server.", undefined);
  }

  if (res.status === 204) return { data: undefined as T, meta: {} };

  const text = await res.text();
  let body: unknown;
  try {
    body = text ? JSON.parse(text) : {};
  } catch {
    throw new ApiError(res.status, "bad_response", "The server returned something that was not JSON.");
  }

  if (!res.ok) {
    const e = (body as { error?: { code?: string; message?: string; field?: string } }).error;
    throw new ApiError(
      res.status,
      e?.code ?? "error",
      e?.message ?? `Request failed with status ${res.status}.`,
      e?.field,
    );
  }

  const ok = body as { data: T; meta?: ApiMeta };
  return { data: ok.data, meta: ok.meta ?? {} };
}

export const api = {
  get: <T>(path: string, env?: Envelope) =>
    request<T>(env ? `${path}?${toQuery(env)}` : path),
  post: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "POST", body: body == null ? undefined : JSON.stringify(body) }),
  patch: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "PATCH", body: body == null ? undefined : JSON.stringify(body) }),
  del: <T>(path: string) => request<T>(path, { method: "DELETE" }),
  /** Builds a download URL that carries the envelope; used for CSV export. */
  exportUrl: (path: string, env: Envelope) => `${API_BASE}${path}?${toQuery(env)}`,
};

/* ── response shapes the dashboard reads ─────────────────────────────────── */

export interface Overview {
  visitors: number;
  sessions: number;
  pageviews: number;
  bounce_rate: number;
  avg_duration_s: number;
  views_per_session: number;
  compare?: Omit<Overview, "compare"> | null;
}

export interface Bucket {
  t: number;
  v: number;
}

export interface Series {
  buckets: Bucket[];
  compare?: Bucket[] | null;
}

export interface BreakdownRow {
  key: string;
  label: string;
  visitors: number;
  sessions: number;
  pageviews: number;
  bounce_rate: number;
  avg_duration_s: number;
  share: number;
}

export interface AgentOverview {
  fetches: number;
  unique_agents: number;
  verified_pct: number;
  bytes: number;
  unique_paths: number;
  top_operators: [string, number][];
  top_purposes: [string, number][];
  delta_pct: number | null;
}

export interface CoverageRow {
  path: string;
  title: string;
  fetch_count: number;
  first_crawl: number | null;
  last_crawl: number | null;
  freshness_gap_hours: number | null;
  agents: string[];
  in_sitemap: boolean;
  in_llms_txt: boolean;
  robots_allowed: number;
}

export type GapVerdict = "extracted" | "compounding" | "invisible" | "quiet";

export interface GapRow {
  path: string;
  title: string;
  agent_fetches: number;
  answer_arrivals: number;
  human_pageviews: number;
  ratio: number;
  verdict: GapVerdict;
}

export interface GapSummary {
  agent_fetches: number;
  answer_arrivals: number;
  ratio: number;
  trend: [number, number, number][];
  verdict_counts: Record<GapVerdict, number>;
}

export interface Realtime {
  visitors_now: number;
  agents_now: number;
  per_minute: [number, number, number][];
  top_pages: { path: string; n: number }[];
  top_sources: { source: string; n: number }[];
  recent: {
    t: number;
    kind: string;
    path: string;
    country: string;
    source: string;
    agent?: string;
  }[];
}

export interface SiteSummary {
  id: string;
  site_id: number;
  domain: string;
  name: string;
  timezone: string;
  public: boolean;
  has_data: boolean;
}
