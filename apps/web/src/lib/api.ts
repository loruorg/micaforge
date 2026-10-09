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
  put: <T>(path: string, body?: unknown) =>
    request<T>(path, { method: "PUT", body: body == null ? undefined : JSON.stringify(body) }),
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

/* ── a streamed answer ───────────────────────────────────────────────────── */

/** One Server-Sent Event: its name, and its data read as JSON where it parses. */
export interface StreamEvent {
  event: string;
  data: unknown;
}

/**
 * True when this browser can read a response body as it arrives. Without it a
 * streamed answer would sit invisible until the last byte, which is no better
 * than the plain JSON answer, so callers ask for JSON instead.
 */
export const canStream = (): boolean =>
  typeof ReadableStream !== "undefined" && typeof TextDecoder !== "undefined";

/** Reads one `event:` / `data:` block of a Server-Sent Events body. */
function readEventBlock(block: string): StreamEvent | null {
  let event = "message";
  const data: string[] = [];
  for (const line of block.split(/\r?\n/)) {
    if (!line || line.startsWith(":")) continue;
    const cut = line.indexOf(":");
    const field = cut < 0 ? line : line.slice(0, cut);
    const value = cut < 0 ? "" : line.slice(cut + 1).replace(/^ /, "");
    if (field === "event") event = value;
    else if (field === "data") data.push(value);
  }
  if (!data.length) return null;
  const raw = data.join("\n");
  try {
    return { event, data: JSON.parse(raw) };
  } catch {
    return { event, data: raw };
  }
}

/**
 * POSTs a JSON body and reads the answer as a Server-Sent Events stream,
 * handing each event to `onEvent` the moment it arrives.
 *
 * It goes through the same door as `request`: the same base address, the
 * session cookie, and the same `ApiError` for a refusal, so a 429 or a 503 reads
 * exactly as it does on every other call. A server (or a proxy in front of it)
 * that replies with plain JSON instead of a stream is still answered rather
 * than dropped: the body is handed over as one `json` event carrying `data`.
 * A stream that breaks part-way throws `stream_broken`, so the caller can tell
 * "never reached the server" from "reached it, and lost the line".
 */
export async function postEventStream(
  path: string,
  body: unknown,
  onEvent: (e: StreamEvent) => void,
  signal?: AbortSignal,
): Promise<void> {
  const headers = new Headers({ accept: "text/event-stream", "content-type": "application/json" });

  let res: Response;
  try {
    res = await fetch(`${API_BASE}${withShare(path)}`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      credentials: "include",
      signal,
    });
  } catch (cause) {
    if (signal?.aborted) throw cause;
    throw new ApiError(0, "network", "Could not reach the Micaforge server.", undefined);
  }

  const type = res.headers.get("content-type") ?? "";
  if (!res.ok || !type.includes("text/event-stream") || !res.body) {
    const text = await res.text();
    let parsed: unknown;
    try {
      parsed = text ? JSON.parse(text) : {};
    } catch {
      throw new ApiError(res.status, "bad_response", "The server returned something that was not JSON.");
    }
    if (!res.ok) {
      const e = (parsed as { error?: { code?: string; message?: string; field?: string } }).error;
      throw new ApiError(
        res.status,
        e?.code ?? "error",
        e?.message ?? `Request failed with status ${res.status}.`,
        e?.field,
      );
    }
    onEvent({ event: "json", data: (parsed as { data?: unknown }).data });
    return;
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  for (;;) {
    let chunk: ReadableStreamReadResult<Uint8Array>;
    try {
      chunk = await reader.read();
    } catch (cause) {
      if (signal?.aborted) throw cause;
      throw new ApiError(0, "stream_broken", "The answer stream stopped before it finished.");
    }
    if (chunk.done) break;
    buffer += decoder.decode(chunk.value, { stream: true });
    let cut = buffer.search(/\r?\n\r?\n/);
    while (cut >= 0) {
      const ev = readEventBlock(buffer.slice(0, cut));
      buffer = buffer.slice(cut).replace(/^\r?\n\r?\n/, "");
      if (ev) onEvent(ev);
      cut = buffer.search(/\r?\n\r?\n/);
    }
  }
  buffer += decoder.decode();
  const last = readEventBlock(buffer);
  if (last) onEvent(last);
}

/* ── the AI analyst ──────────────────────────────────────────────────────── */

export interface AiStatus {
  configured: boolean;
  model: string | null;
  /** Questions per organisation per UTC day; `null` means no limit. */
  daily_limit: number | null;
}

/** One ClickHouse read a tool ran: each "?" in `sql` is the next of `params`. */
export interface SqlTrace {
  report: string;
  sql: string;
  params: (string | number)[];
}

/** One tool call the model made, with the exact reads behind it. */
export interface ToolTrace {
  tool: string;
  arguments: Record<string, unknown>;
  ok: boolean;
  error: string | null;
  /** The equivalent stats API request, e.g. `GET /api/stats/breakdown?site=3&range=7d`. */
  request: string;
  queries: SqlTrace[];
  rows: number | null;
  took_ms: number;
}

export interface AiMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  tool_calls: ToolTrace[];
  error: string | null;
  created_at: string;
}

export interface AiConversationSummary {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  messages: number;
}

export interface AiConversation {
  id: string;
  site: number;
  title: string;
  created_at: string;
  updated_at: string;
  messages: AiMessage[];
}

export interface AskBody {
  site: number | string;
  question: string;
  conversation_id?: string;
  range?: string;
  start?: string;
  end?: string;
  tz?: string;
  filters?: Filter[];
  audience?: Audience;
}

/** The JSON answer to `POST /api/ai/ask`, and the `json` event a stream falls back to. */
export interface AskAnswer {
  conversation_id: string;
  message: AiMessage;
}

/* ── insights and investigations ─────────────────────────────────────────── */

export type InvestigationStatus = "open" | "monitoring" | "resolved";
export type InvestigationSeverity = "info" | "warning" | "critical";

export interface InvestigationFinding {
  label: string;
  current: number;
  baseline: number;
  /** In percent: 25 means +25%. */
  change_pct: number | null;
}

export interface Investigation {
  id: string;
  site: number;
  signal_key: string;
  title: string;
  status: InvestigationStatus;
  severity: InvestigationSeverity;
  /** visitors | pageviews | bounce_rate | agent_fetches | answer_arrivals */
  metric: string;
  dimension: string | null;
  dimension_value: string | null;
  direction: "up" | "down";
  current: number;
  baseline: number;
  /** In percent: 25 means +25%. */
  change_pct: number | null;
  window: string;
  findings: InvestigationFinding[];
  /** The statistical queries first, then the AI tool calls. */
  evidence: ToolTrace[];
  explanation: string | null;
  explanation_status: "ready" | "not_configured" | "failed";
  created_at: string;
  updated_at: string;
  next_check_at: string | null;
  resolved_at: string | null;
}

export interface InvestigationEvent {
  id: string;
  kind: "opened" | "checked" | "explained" | "status" | "posted";
  summary: string;
  data: Record<string, unknown>;
  created_at: string;
}

export type InvestigationDetail = Investigation & { events: InvestigationEvent[] };

export interface InsightSubscription {
  weekly_digest: boolean;
  mail_enabled: boolean;
}

/* ── chat apps ───────────────────────────────────────────────────────────── */

export type ChatPlatform = "slack" | "discord" | "telegram";

export interface ChatStatus {
  slack: { configured: boolean; commands_url: string; events_url: string };
  discord: { configured: boolean; interactions_url: string; application_id: string | null };
  telegram: { configured: boolean; webhook_url: string };
  ai_configured: boolean;
}

export interface ChatLink {
  id: string;
  platform: ChatPlatform;
  workspace_id: string;
  channel_id: string;
  label: string;
  created_at: string;
  /** The name of whoever linked it. */
  created_by: string | null;
}

export interface ChatLinkCode {
  /** Eight characters of A-Z and 2-9; works once, for fifteen minutes. */
  code: string;
  expires_at: string;
  commands: Record<ChatPlatform, string>;
}

/* ── feedback ────────────────────────────────────────────────────────────── */

export type FeedbackKind = "bug" | "problem" | "feature" | "other";
export type FeedbackStatus = "new" | "triaged" | "planned" | "done" | "wont_fix";
export type FeedbackSeverity = "low" | "medium" | "high" | "critical";
export type FeedbackSource = "dashboard" | "mcp" | "api" | "chat";

export interface FeedbackBody {
  kind: FeedbackKind;
  title: string;
  body: string;
  severity?: FeedbackSeverity;
  site?: number | string;
  page_url?: string;
  app_version?: string;
}

export interface Feedback {
  id: string;
  kind: FeedbackKind;
  title: string;
  body: string;
  status: FeedbackStatus;
  severity: FeedbackSeverity | null;
  source: FeedbackSource;
  reporter: {
    user_id: string | null;
    email: string | null;
    name: string | null;
    /** The MCP client that sent it, when it came over MCP. */
    client_name: string | null;
    api_key_id: string | null;
    /** The chat platform and channel, when it came from a chat app. */
    chat: string | null;
  };
  org: { id: string; name: string } | null;
  site: { id: number; domain: string } | null;
  page_url: string | null;
  app_version: string | null;
  user_agent: string | null;
  created_at: string;
  updated_at: string;
  /** The GitHub issue it was opened as, when it was. */
  github_issue: { url: string; number: number; repo: string | null } | null;
}

/* ── annotations ─────────────────────────────────────────────────────────── */

/** Never blue (people) or orange (AI): a note is neither audience. */
export type AnnotationColor = "neutral" | "green" | "red";

export interface Annotation {
  id: string;
  site: number;
  starts_at: string;
  ends_at: string | null;
  text: string;
  color: AnnotationColor;
  category: string | null;
  source: "dashboard" | "api" | "mcp" | "github";
  /** The object on GitHub, for a note the GitHub App wrote. */
  url: string | null;
  author: { user_id: string; name: string } | null;
  created_at: string;
  updated_at: string;
}

export interface AnnotationBody {
  starts_at?: string;
  ends_at?: string | null;
  text?: string;
  color?: AnnotationColor;
  category?: string;
}

/* ── MCP OAuth: the consent screen and connected apps ────────────────────── */

export interface OAuthScope {
  scope: string;
  label: string;
  description: string;
  required: boolean;
}

export interface OAuthConsent {
  client: { client_id: string; name: string; redirect_uri: string; registered_at: string };
  scopes_requested: string[];
  scopes_available: OAuthScope[];
  organisations: { id: string; name: string; role: string }[];
  resource: string;
}

export interface OAuthGrant {
  id: string;
  client_id: string;
  client_name: string;
  org: { id: string; name: string };
  scopes: string[];
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
}

/**
 * Who made a change: a person, an API key, or the server itself (a scheduled
 * job, a webhook). The audit log and a flag's history both name the author
 * this way, so the two screens describe the same change in the same words.
 */
export interface Actor {
  kind: "user" | "api_key" | "system";
  id: string | null;
  label: string;
}

/* ── the GitHub App ──────────────────────────────────────────────────────── */

export interface GitHubInstallation {
  id: string;
  installation_id: number;
  account: string;
  account_type: string;
  repository_selection: "all" | "selected" | null;
  suspended: boolean;
  suspended_at: string | null;
  created_at: string;
}

export interface GitHubLink {
  id: string;
  installation: string;
  account: string;
  repo: { id: number; full_name: string; default_branch: string; html_url: string };
  deploys: boolean;
  releases: boolean;
  pushes: boolean;
  /** Deployment environments recorded; empty records every environment. */
  environments: string[];
  /** Agents with manage:write may open issues from their own feedback. */
  agent_issues: boolean;
  suspended: boolean;
  created_at: string;
  updated_at: string;
}

export interface GitHubStatus {
  configured: boolean;
  app: { slug: string; url: string } | null;
  webhook_url: string;
  callback_url: string;
  can_install: boolean;
  can_link: boolean;
  installations: GitHubInstallation[];
  link: GitHubLink | null;
}

export interface GitHubRepo {
  id: number;
  full_name: string;
  private: boolean;
  default_branch: string;
  html_url: string;
}

export type GitHubEventKind = "deploy" | "release" | "push";

export interface GitHubEvent {
  id: string;
  kind: GitHubEventKind;
  title: string;
  repo: string;
  ref: string | null;
  sha: string | null;
  environment: string | null;
  actor: string | null;
  url: string;
  commits: { sha: string; message: string; url: string; author: string | null }[];
  occurred_at: string;
  annotated: boolean;
}

export interface GitHubDelivery {
  id: string;
  event: string;
  action: string | null;
  repo: string | null;
  outcome: "processing" | "recorded" | "ignored" | "failed";
  detail: string | null;
  received_at: string;
}

export interface GitHubIssue {
  url: string;
  number: number;
  repo: string;
  created: boolean;
}
