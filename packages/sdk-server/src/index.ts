/**
 * `@micaforge/sdk-server`: the Micaforge server-side SDK.
 *
 * # Why a server SDK exists at all
 *
 * The browser tracker cannot see the audience this product is about. AI crawlers do not
 * run JavaScript, so every fetch by GPTBot, ClaudeBot, PerplexityBot and their peers is
 * invisible to a script tag, by construction and forever. The only place those requests
 * can be observed is the server that answered them. That is this package: the path by
 * which a customer's real traffic reaches `POST /api/log/edge` and the agent side of the
 * Citation Gap stops being empty.
 *
 * # The three promises
 *
 * 1. **It never throws into your request path.** Every public method is wrapped. A bad
 *    ingest key, a DNS failure, a 500 from the collector: all of them end as one line
 *    through the injected logger and a dropped event. Analytics is not allowed to take
 *    a page down.
 * 2. **It never blocks your response.** Hits are queued and sent on a 200 ms window, or
 *    as soon as 50 are waiting. Nothing awaits the collector.
 * 3. **It sends only what the schema has a column for.** Every key on the wire is a
 *    column from `docs/CONTRACT.md` 1.1 or 1.2, with two documented transport fields
 *    (`user_agent`, `ip`) that the server reads and drops.
 *
 * @packageDocumentation
 */
import { createHmac, randomBytes } from "node:crypto";

import { classify } from "./agents.js";
import { readRequest, splitUrl } from "./request.js";
import type {
  AgentHit,
  EdgePayload,
  HitExtras,
  Logger,
  McpCall,
  McpMethod,
  McpPayload,
  MicaforgeOptions,
  QueuedEvent,
  RequestLike,
  TrackEvent,
  Timestampish,
  WireAgentEvent,
  WireEvent,
  WireMcpCall,
} from "./types.js";

export * from "./types.js";
export { classify, isAgent, mirrorTokens } from "./agents.js";
export type { AgentGuess } from "./agents.js";
export { clientIp, header, isConditional, readRequest, requestHost, splitUrl, userAgent } from "./request.js";
export { createFlagsClient } from "./flags.js";
export type { FlagsClient, FlagsClientConfig, FlagUser } from "./flags.js";
export { compileRuleset, evaluateFlag, evaluateFlags, flagBucket } from "./flag-eval.js";
export type {
  CompiledRuleset,
  FlagDecision,
  FlagInput,
  FlagReason,
  FlagRule,
  FlagRuleset,
} from "./flag-eval.js";

/** Written to `sdk_version` on every human row, so a bad release can be found later. */
export const VERSION = "0.1.0";

/** The one endpoint this package talks to. CONTRACT.md 3.3. */
export const EDGE_PATH = "/api/log/edge";

/** The signature header. CONTRACT.md 3.3. */
export const SIGNATURE_HEADER = "x-micaforge-signature";

/** Where MCP server calls go: signed like an edge batch, written to `mcp_calls`. */
export const MCP_PATH = "/api/log/mcp";

/** The methods an MCP call row can be. Anything else is not sent. */
export const MCP_METHODS: readonly McpMethod[] = ["tools/call", "resources/read", "prompts/get"];

/** Largest opted-in arguments document sent, serialised. The server keeps no more. */
export const MAX_MCP_ARGUMENTS = 4 * 1024;

const DEFAULT_FLUSH_INTERVAL = 200;
const DEFAULT_MAX_BATCH = 50;
const DEFAULT_TIMEOUT = 10_000;
const DEFAULT_RETRIES = 2;

/* ----------------------------------------------------------------- helpers */

/** `/admin/*` matches one segment, `/admin/**` any depth, `?` one character. */
function globToRegExp(glob: string): RegExp {
  return new RegExp(
    "^" +
      glob.replace(/\*\*|[*?]|[.+^${}()|[\]\\]/g, (m) =>
        m == "**" ? ".*" : m == "*" ? "[^/]*" : m == "?" ? "[^/]" : "\\" + m,
      ) +
      "$",
  );
}

function isExcluded(path: string, globs: RegExp[]): boolean {
  return globs.some((re) => re.test(path));
}

/**
 * Compile a list of path globs into one predicate.
 *
 * Exported because every adapter needs the same rule and three copies of a regular
 * expression builder is three chances for them to disagree about what `**` means.
 */
export function pathMatcher(globs: string[] | undefined): (path: string) => boolean {
  if (!globs || globs.length === 0) return () => false;
  const compiled = globs.map(globToRegExp);
  return (path: string) => isExcluded(path, compiled);
}

/**
 * Normalise anything timestamp-shaped to the `DateTime64(3)` wire format.
 *
 * An unparseable value falls back to now rather than to the epoch. A hit at
 * 1970-01-01 is not a missing timestamp, it is a wrong one, and a wrong number is the
 * thing this product refuses to print.
 */
function isoTime(value: Timestampish | undefined): string {
  if (value === undefined) return new Date().toISOString();
  const date = value instanceof Date ? value : new Date(value);
  return Number.isNaN(date.getTime()) ? new Date().toISOString() : date.toISOString();
}

/**
 * Drop keys the server would only fill with the column's own DEFAULT.
 *
 * `keep` is the short list of columns that are declared without a DEFAULT in
 * CONTRACT.md 1.1 and 1.2, so they are always sent even when empty. Everything else is
 * omitted rather than sent as `""`, which keeps a batch of fifty hits small.
 */
function compact<T extends object>(row: T, keep: readonly string[] = []): T {
  const bag = row as Record<string, unknown>;
  for (const key of Object.keys(bag)) {
    if (keep.includes(key)) continue;
    const value = bag[key];
    if (
      value === undefined ||
      value === null ||
      value === "" ||
      (typeof value == "number" && !Number.isFinite(value)) ||
      (typeof value == "object" && !Array.isArray(value) && Object.keys(value as object).length === 0)
    ) {
      delete bag[key];
    }
  }
  return row;
}

/** Columns declared with no DEFAULT, so always present on the wire. */
const EVENT_REQUIRED = ["timestamp", "kind", "hostname", "pathname", "sdk_version"] as const;
const AGENT_REQUIRED = ["timestamp", "user_agent", "method", "hostname", "pathname"] as const;

/** A logger that says something by default, because a silent SDK hides a bad key. */
function defaultLogger(): Logger {
  return {
    warn: (message, detail) => console.warn(`[micaforge] ${message}`, detail ?? ""),
    error: (message, detail) => console.error(`[micaforge] ${message}`, detail ?? ""),
  };
}

const SILENT: Logger = { warn: () => {}, error: () => {} };

/**
 * Sign a request body. CONTRACT.md 3.3.
 *
 * HMAC-SHA256, keyed with the site's ingest key, over `${t}.${body}`: the Unix time in
 * whole seconds, one `.`, then the exact bytes that go on the wire. Rendered as
 * `t=<unix seconds>,v1=<hex>`. The timestamp is inside the signed material, so a
 * captured batch cannot be replayed under a fresh `t`, and the collector refuses a `t`
 * more than five minutes from its own clock. Every attempt is signed afresh for that
 * reason.
 *
 * The body is signed byte for byte, which is why {@link Micaforge.flush} serialises once
 * and sends that same string: signing a re-serialised object would eventually disagree
 * with itself over key order.
 */
export function signBody(ingestKey: string, body: string, atSeconds?: number): string {
  const t = Math.floor(atSeconds ?? Date.now() / 1000);
  const v1 = createHmac("sha256", ingestKey).update(`${t}.`, "utf8").update(body, "utf8").digest("hex");
  return `t=${t},v1=${v1}`;
}

/* ------------------------------------------------------------- the mappers */

/**
 * A {@link TrackEvent} as a row for `events`.
 *
 * The one place a public camelCase name becomes a schema column. Anything not listed
 * here has no column and is therefore not sent.
 */
export function toWireEvent(event: TrackEvent, sdkVersion = VERSION): WireEvent {
  const parts = event.url
    ? splitUrl(event.url, event.hostname ?? "")
    : {
        hostname: event.hostname ?? "",
        pathname: event.pathname ?? "/",
        querystring: (event.querystring ?? "").replace(/^\?/, ""),
        hash: (event.hash ?? "").replace(/^#/, ""),
      };

  return compact<WireEvent>({
    timestamp: isoTime(event.timestamp),
    kind: event.kind ?? (event.name ? "custom" : "pageview"),
    event_name: event.name,
    hostname: parts.hostname,
    pathname: parts.pathname || "/",
    page_title: event.title,
    querystring: parts.querystring,
    hash: parts.hash,
    referrer: event.referrer,
    language: event.language,
    timezone: event.timezone,
    identified_id: event.identifiedId,
    props: event.props as WireEvent["props"],
    revenue: event.revenue,
    currency: event.currency,
    duration_ms: event.durationMs,
    scroll_pct: event.scrollPct,
    experiment: event.experiment,
    variant: event.variant,
    flags: event.flags,
    sdk_version: sdkVersion,
    user_agent: event.userAgent,
    ip: event.ip,
  }, EVENT_REQUIRED);
}

/**
 * An {@link AgentHit} as a row for `agent_events`.
 *
 * `agent_id`, `operator`, `purpose`, `verified`, `asn`, `asn_org`, `country`,
 * `ip_hash`, `robots_allowed` and `llms_listed` are absent on purpose. Every one of
 * them is a fact the server establishes against the catalog, the operator's published
 * ranges and the site's own policy files. A client guessing them would be a number
 * this product did not earn.
 */
export function toWireAgentEvent(hit: AgentHit): WireAgentEvent {
  const parts = hit.url
    ? splitUrl(hit.url, hit.hostname ?? "")
    : {
        hostname: hit.hostname ?? "",
        pathname: hit.pathname ?? "/",
        querystring: (hit.querystring ?? "").replace(/^\?/, ""),
      };

  return compact<WireAgentEvent>({
    timestamp: isoTime(hit.timestamp),
    user_agent: hit.userAgent,
    method: (hit.method ?? "GET").toUpperCase(),
    hostname: parts.hostname,
    pathname: parts.pathname || "/",
    querystring: parts.querystring,
    status: hit.status,
    bytes: hit.bytes,
    duration_ms: hit.durationMs,
    content_type: hit.contentType ? hit.contentType.split(";")[0]?.trim() : undefined,
    conditional: hit.conditional ? 1 : undefined,
    ip: hit.ip,
  }, AGENT_REQUIRED);
}

/**
 * A keyed digest of an MCP session id: 32 hex characters.
 *
 * Keyed with a random secret drawn once per process and never sent anywhere, so the
 * digest groups one session's calls together and nothing, Micaforge included, can turn
 * it back into the id. That matters because on the Streamable HTTP transport the session
 * id is what lets a client speak in an open session. A process restart draws a new key,
 * which is harmless: a session does not survive the restart either.
 */
export function hashSessionId(id: string): string {
  sessionKey ??= randomBytes(32);
  return createHmac("sha256", sessionKey).update(String(id), "utf8").digest("hex").slice(0, 32);
}

let sessionKey: Buffer | undefined;

/** UTF-8 byte length without allocating, for the sizes the SDK reports. */
export function utf8Length(text: string): number {
  let bytes = 0;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    if (code < 0x80) bytes += 1;
    else if (code < 0x800) bytes += 2;
    else if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      // A surrogate pair is one four-byte character.
      bytes += 4;
      i++;
    } else bytes += 3;
  }
  return bytes;
}

/** JSON text of anything, or `undefined` for a value JSON cannot hold (a cycle, a BigInt). */
export function jsonText(value: unknown): string | undefined {
  try {
    return value === undefined ? undefined : JSON.stringify(value);
  } catch {
    return undefined;
  }
}

/**
 * An {@link McpCall} as a row for `mcp_calls`, or `null` for a method no report reads.
 *
 * The session id is replaced by {@link hashSessionId} here, before the row is queued, so
 * the raw id never sits in a buffer, a log line or a request body. A resource URI loses
 * its query string and fragment, which is where a one-off value would be. Arguments are
 * sent only when the caller passed them, and as a note instead when they are larger
 * than the server would keep.
 */
export function toWireMcpCall(call: McpCall, sdkVersion = VERSION): WireMcpCall | null {
  if (!call || !MCP_METHODS.includes(call.method)) return null;
  const errorCode =
    typeof call.errorCode == "number" && Number.isInteger(call.errorCode) ? call.errorCode : undefined;
  const status = call.status ?? (call.errorType || errorCode ? "error" : "ok");
  const name =
    call.method === "resources/read" && typeof call.name == "string"
      ? call.name.replace(/[?#][\s\S]*$/, "")
      : call.name;
  const whole = (n: number | undefined) =>
    typeof n == "number" && Number.isFinite(n) ? Math.max(0, Math.round(n)) : undefined;

  let args: unknown;
  if (call.arguments !== undefined) {
    const text = jsonText(call.arguments);
    if (text !== undefined) {
      args =
        utf8Length(text) > MAX_MCP_ARGUMENTS
          ? { _dropped: "arguments exceeded the size limit" }
          : call.arguments;
    }
  }

  return compact<WireMcpCall>(
    {
      timestamp: isoTime(call.timestamp),
      method: call.method,
      name: name ? String(name).slice(0, 200) : undefined,
      server_name: call.serverName,
      server_version: call.serverVersion,
      client_name: call.clientName,
      client_version: call.clientVersion,
      protocol_version: call.protocolVersion,
      transport: call.transport,
      session_hash: call.sessionId ? hashSessionId(call.sessionId) : undefined,
      duration_ms: whole(call.durationMs) ?? 0,
      status,
      error_type: call.errorType,
      error_code: errorCode,
      arg_bytes: whole(call.argBytes),
      result_bytes: whole(call.resultBytes),
      arguments: args,
      sdk_version: `sdk-server/${sdkVersion}`,
    },
    MCP_REQUIRED,
  );
}

/** Columns of an MCP row that are always sent. */
const MCP_REQUIRED = ["timestamp", "method", "duration_ms", "status", "sdk_version"] as const;

/* ------------------------------------------------------------- the middleware types */

/** A `node:http` response, structurally. Not imported, so nothing is pulled in. */
export interface ResponseLike {
  statusCode?: number;
  getHeader?(name: string): number | string | string[] | undefined;
  once?(event: string, listener: () => void): unknown;
  on?(event: string, listener: () => void): unknown;
  write?: (...args: never[]) => boolean;
  end?: (...args: never[]) => unknown;
}

/** Options for {@link Micaforge.middleware} and the framework adapters. */
export interface MiddlewareOptions {
  /**
   * Paths never reported, on top of the client's own `excludePaths`. Same glob rules.
   *
   * Nothing is excluded by default. An agent fetching `/sitemap.xml` or a `.md` file is
   * a real crawl, and filtering assets away would understate the "taken" side of the
   * Citation Gap. Exclude a health check, not a content type.
   */
  exclude?: string[];

  /**
   * Report human requests as well as agent ones. Default `true`.
   *
   * Set `false` when the browser tracker is already on the page: the tracker sees
   * engagement, scroll and client-side navigation that the server cannot, and running
   * both would count every page twice.
   */
  humans?: boolean;

  /**
   * Count the exact response body size by wrapping `write` and `end`. Default `false`.
   *
   * Off by default because `content-length` already gives the exact figure for most
   * responses and reading a header cannot break anything, while wrapping a stream can.
   * Turn it on when you serve chunked responses and want `bytes` to be right for them.
   */
  countBytes?: boolean;

  /**
   * Only report these methods. Default `["GET", "HEAD"]`.
   *
   * A crawl is a read. A `POST` to your API is not a page fetch and putting it in the
   * page report would make every table wrong. Pass `[]` to report every method.
   */
  methods?: string[];
}

/* ---------------------------------------------------------------- the class */

/**
 * The Micaforge server client.
 *
 * ```ts
 * const micaforge = new Micaforge({
 *   host: "https://micaforge.example.com",
 *   siteId: 1,
 *   ingestKey: process.env.MICAFORGE_INGEST_KEY!,
 * });
 * ```
 *
 * One instance per process is right. It holds a queue, a timer and nothing else.
 */
export class Micaforge {
  readonly siteId: number;
  readonly endpoint: string;
  /** Where {@link Micaforge.recordMcpCall} rows are sent. */
  readonly mcpEndpoint: string;

  private readonly ingestKey: string;
  private readonly flushInterval: number;
  private readonly maxBatch: number;
  private readonly timeout: number;
  private readonly retries: number;
  private readonly log: Logger;
  private readonly enabled: boolean;
  private readonly trustProxy: boolean;
  private readonly excluded: RegExp[];
  private readonly doFetch: typeof globalThis.fetch;

  private events: WireEvent[] = [];
  private agents: WireAgentEvent[] = [];
  private mcp: WireMcpCall[] = [];
  private timer: ReturnType<typeof setTimeout> | null = null;
  private inflight = new Set<Promise<void>>();
  private closed = false;
  private exitHook: (() => void) | null = null;

  constructor(options: MicaforgeOptions) {
    this.log = options.logger === null ? SILENT : (options.logger ?? defaultLogger());
    this.siteId = Number(options.siteId);
    this.ingestKey = String(options.ingestKey ?? "");
    this.flushInterval = Math.max(0, options.flushInterval ?? DEFAULT_FLUSH_INTERVAL);
    this.maxBatch = Math.max(1, options.maxBatch ?? DEFAULT_MAX_BATCH);
    this.timeout = Math.max(1, options.timeout ?? DEFAULT_TIMEOUT);
    this.retries = Math.max(0, options.retries ?? DEFAULT_RETRIES);
    this.trustProxy = options.trustProxy !== false;
    this.excluded = (options.excludePaths ?? []).map(globToRegExp);
    // Bound to the global on purpose. A detached `fetch` throws "Illegal invocation" in
    // a browser-shaped runtime, and Workers is exactly that shape, so pulling the
    // reference off `globalThis` without binding it would break the one runtime this
    // package most needs to work on.
    this.doFetch = options.fetch ?? globalThis.fetch?.bind(globalThis);
    this.endpoint = normaliseHost(options.host) + EDGE_PATH;
    this.mcpEndpoint = normaliseHost(options.host) + MCP_PATH;

    // Misconfiguration is reported once, at construction, and then the client stays
    // quiet and disabled. The alternative, one warning per event, buries a real error
    // under its own repetitions.
    let enabled = options.enabled !== false;
    if (!Number.isInteger(this.siteId) || this.siteId < 0 || this.siteId > 65535) {
      this.log.error(`siteId must be an integer from 0 to 65535, got ${String(options.siteId)}`);
      enabled = false;
    }
    if (!this.ingestKey) {
      this.log.error("ingestKey is missing; nothing will be sent");
      enabled = false;
    }
    if (!options.host) {
      this.log.error("host is missing; nothing will be sent");
      enabled = false;
    }
    if (typeof this.doFetch != "function") {
      this.log.error("no global fetch in this runtime; pass options.fetch");
      enabled = false;
    }
    this.enabled = enabled;

    if (enabled && options.flushOnExit !== false) this.installExitHook();
  }

  /* ------------------------------------------------------------- recording */

  /**
   * Record one event.
   *
   * `track` takes you at your word, with a single exception: a `pageview` whose user
   * agent the local mirror can name is recorded as an agent fetch instead. A crawler
   * counted as a reader is the exact error this product exists to correct, and it would
   * be strange to make it here. Named custom events pass through untouched.
   */
  track(event: TrackEvent): void {
    try {
      if (!this.enabled || this.closed) return;
      const kind = event.kind ?? (event.name ? "custom" : "pageview");
      if (kind === "pageview" && classify(event.userAgent)) {
        this.agentHit({
          userAgent: event.userAgent as string,
          url: event.url,
          hostname: event.hostname,
          pathname: event.pathname,
          querystring: event.querystring,
          ip: event.ip,
          timestamp: event.timestamp,
        });
        return;
      }
      this.enqueueEvent({ ...event, kind });
    } catch (error) {
      this.log.error("track failed", error);
    }
  }

  /**
   * Record a request as a pageview, deciding for itself what sent it.
   *
   * Pass whatever your framework calls a request: a Node `IncomingMessage`, an Express
   * `Request`, a Fetch `Request`, Hono's `c.req.raw`. The user agent is read from the
   * headers and matched against the local mirror; a named client becomes an
   * `agent_events` row and everything else becomes an `events` row, which the server
   * may still reclassify with the full catalog.
   */
  pageview(req: RequestLike, extras: HitExtras = {}): void {
    try {
      if (!this.enabled || this.closed) return;
      const read = readRequest(req, this.trustProxy);
      if (this.skip(read.url.pathname)) return;

      if (classify(read.userAgent)) {
        this.enqueueAgent({
          userAgent: read.userAgent,
          hostname: read.url.hostname,
          pathname: read.url.pathname,
          querystring: read.url.querystring,
          method: read.method,
          status: extras.status,
          bytes: extras.bytes,
          durationMs: extras.durationMs,
          contentType: extras.contentType,
          conditional: read.conditional,
          ip: read.ip,
          timestamp: extras.timestamp,
        });
        return;
      }

      // `status`, `bytes`, `contentType` and the response duration are dropped here on
      // purpose: `events` has no column for any of them, and `duration_ms` on a human
      // row means engagement time on the previous page, not how fast you answered.
      this.enqueueEvent({
        kind: extras.name ? "custom" : "pageview",
        name: extras.name,
        hostname: read.url.hostname,
        pathname: read.url.pathname,
        querystring: read.url.querystring,
        referrer: read.referrer,
        language: read.language,
        userAgent: read.userAgent,
        ip: read.ip,
        title: extras.title,
        props: extras.props,
        identifiedId: extras.identifiedId,
        timestamp: extras.timestamp,
      });
    } catch (error) {
      this.log.error("pageview failed", error);
    }
  }

  /**
   * Record one fetch by a non-human client.
   *
   * Use it when you already know the request was an agent: from a log line, from a
   * reverse proxy, from your own edge rules. The server still resolves which agent it
   * was, so an unfamiliar user agent is worth sending rather than dropping.
   */
  agentHit(hit: AgentHit): void {
    try {
      if (!this.enabled || this.closed) return;
      this.enqueueAgent(hit);
    } catch (error) {
      this.log.error("agentHit failed", error);
    }
  }

  /**
   * Record many at once.
   *
   * Each item says which table it belongs in, because a batch read out of a log file
   * is usually a mix. This is the call the log shipper is built on.
   */
  batch(events: QueuedEvent[]): void {
    try {
      if (!this.enabled || this.closed || !Array.isArray(events)) return;
      for (const item of events) {
        if (!item) continue;
        if (item.type === "agent") this.agentHit(item);
        else this.track(item);
      }
    } catch (error) {
      this.log.error("batch failed", error);
    }
  }

  /**
   * Record one request an MCP server answered.
   *
   * `instrumentMcpServer` calls this for the official SDK; call it yourself from any
   * other MCP server. The call joins the same queue and flush window as every hit and
   * goes to `POST /api/log/mcp`, signed with the same key. A method other than
   * `tools/call`, `resources/read` and `prompts/get` is not recorded.
   */
  recordMcpCall(call: McpCall): void {
    try {
      if (!this.enabled || this.closed) return;
      const row = toWireMcpCall(call);
      if (!row) return;
      this.mcp.push(row);
      this.afterEnqueue();
    } catch (error) {
      this.log.error("recordMcpCall failed", error);
    }
  }

  /* ------------------------------------------------------------- lifecycle */

  /** How many hits are waiting to be sent. Useful in a test, and in a health probe. */
  get queued(): number {
    return this.events.length + this.agents.length + this.mcp.length;
  }

  /**
   * Send everything queued now, and wait for it and for anything already in flight.
   *
   * Never rejects. A send that fails past its retries is logged and dropped, because
   * the alternative is an unhandled rejection in someone's shutdown path.
   */
  async flush(): Promise<void> {
    try {
      this.clearTimer();
      const events = this.events;
      const agents = this.agents;
      const mcp = this.mcp;
      this.events = [];
      this.agents = [];
      this.mcp = [];
      if (events.length || agents.length) this.startSend(events, agents);
      if (mcp.length) this.startMcpSend(mcp);
      // Copy first: a settling promise removes itself from the set while we wait.
      await Promise.all([...this.inflight]);
    } catch (error) {
      this.log.error("flush failed", error);
    }
  }

  /**
   * Flush, then stop accepting events and release the exit hook.
   *
   * Call this from your own `SIGTERM` handler. `beforeExit` does not fire when a
   * process is signalled, so a container that is being rolled would otherwise lose
   * whatever was still inside the flush window.
   */
  async shutdown(): Promise<void> {
    await this.flush();
    this.closed = true;
    this.removeExitHook();
  }

  /* ------------------------------------------------------------ middleware */

  /**
   * A `node:http` middleware: `(req, res, next?)`.
   *
   * It reports on `finish`, never before, so `status`, `bytes` and the response time
   * are the real ones. `next()` is called synchronously first, so nothing in this
   * package sits between the request and your handler.
   */
  middleware(options: MiddlewareOptions = {}) {
    const exclude = (options.exclude ?? []).map(globToRegExp);
    const humans = options.humans !== false;
    const methods = (options.methods ?? ["GET", "HEAD"]).map((m) => m.toUpperCase());

    return (req: RequestLike, res: ResponseLike, next?: (error?: unknown) => void): void => {
      // Observe first, then hand over. A synchronous handler can finish the response
      // inside `next()`, and a `finish` listener attached after that never fires: the
      // hit would be silently lost on exactly the fast paths a crawler hits most.
      // `observe` only reads headers and attaches one listener, so this costs
      // microseconds and cannot block.
      try {
        this.observe(req, res, { exclude, humans, methods, countBytes: !!options.countBytes });
      } catch (error) {
        this.log.error("middleware failed", error);
      }
      next?.();
    };
  }

  /**
   * The same middleware, typed for `app.use()`.
   *
   * Express middleware is Node middleware, so this is {@link Micaforge.middleware}
   * under the name that reads correctly at the call site.
   */
  express(options: MiddlewareOptions = {}) {
    return this.middleware(options);
  }

  /* --------------------------------------------------------------- internals */

  /** Attach to `finish` and record once the response is actually written. */
  private observe(
    req: RequestLike,
    res: ResponseLike,
    options: { exclude: RegExp[]; humans: boolean; methods: string[]; countBytes: boolean },
  ): void {
    const method = (req.method ?? "GET").toUpperCase();
    if (options.methods.length && !options.methods.includes(method)) return;

    const read = readRequest(req, this.trustProxy);
    if (this.skip(read.url.pathname) || isExcluded(read.url.pathname, options.exclude)) return;

    const agent = classify(read.userAgent);
    if (!agent && !options.humans) return;

    const started = Date.now();
    const counter = options.countBytes ? countResponseBytes(res, this.log) : null;
    const listen = res.once ?? res.on;

    const record = () => {
      const durationMs = Date.now() - started;
      if (agent) {
        this.enqueueAgent({
          userAgent: read.userAgent,
          hostname: read.url.hostname,
          pathname: read.url.pathname,
          querystring: read.url.querystring,
          method,
          status: res.statusCode ?? 200,
          bytes: counter ? counter() : responseBytes(res),
          durationMs,
          contentType: headerValue(res, "content-type"),
          conditional: read.conditional,
          ip: read.ip,
        });
      } else {
        this.enqueueEvent({
          kind: "pageview",
          hostname: read.url.hostname,
          pathname: read.url.pathname,
          querystring: read.url.querystring,
          referrer: read.referrer,
          language: read.language,
          userAgent: read.userAgent,
          ip: read.ip,
        });
      }
    };

    if (typeof listen == "function") {
      listen.call(res, "finish", () => {
        try {
          record();
        } catch (error) {
          this.log.error("recording a response failed", error);
        }
      });
    } else {
      // No event emitter (a stub, or an exotic runtime). Record what is known now.
      record();
    }
  }

  private enqueueEvent(event: TrackEvent): void {
    const row = toWireEvent(event);
    if (this.skip(row.pathname)) return;
    this.events.push(row);
    this.afterEnqueue();
  }

  private enqueueAgent(hit: AgentHit): void {
    const row = toWireAgentEvent(hit);
    if (!row.user_agent) return; // Nothing to classify against; the row would be noise.
    if (this.skip(row.pathname)) return;
    this.agents.push(row);
    this.afterEnqueue();
  }

  private skip(pathname: string): boolean {
    return this.excluded.length > 0 && isExcluded(pathname, this.excluded);
  }

  /** Send at the batch size, otherwise arm the window. */
  private afterEnqueue(): void {
    if (this.queued >= this.maxBatch) {
      void this.flush();
      return;
    }
    if (this.timer || this.flushInterval === 0) {
      if (this.flushInterval === 0) void this.flush();
      return;
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.flush();
    }, this.flushInterval);
    // A queued analytics event must never be the reason a CLI stays alive.
    (this.timer as { unref?: () => void }).unref?.();
  }

  private clearTimer(): void {
    if (this.timer) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  private startSend(events: WireEvent[], agents: WireAgentEvent[]): void {
    const payload: EdgePayload = { site_id: this.siteId };
    if (events.length) payload.events = events;
    if (agents.length) payload.agent_events = agents;
    this.hold(this.send(this.endpoint, JSON.stringify(payload), events.length + agents.length, "hit"));
  }

  /** MCP calls go in a request of their own, to the endpoint that writes `mcp_calls`. */
  private startMcpSend(calls: WireMcpCall[]): void {
    const payload: McpPayload = { site_id: this.siteId, mcp_calls: calls };
    this.hold(this.send(this.mcpEndpoint, JSON.stringify(payload), calls.length, "MCP call"));
  }

  /** Keep a send in the in-flight set until it settles, so `flush()` can wait for it. */
  private hold(sending: Promise<void>): void {
    const promise = sending.finally(() => {
      this.inflight.delete(promise);
    });
    this.inflight.add(promise);
  }

  /**
   * One signed POST, with bounded retries.
   *
   * A `4xx` other than `429` is not retried: it means this SDK built a request the
   * collector will not accept, and sending it again only repeats the mistake. A
   * timeout, a network fault, a `429` and any `5xx` are all worth a second try.
   */
  private async send(url: string, body: string, count: number, noun: string): Promise<void> {
    // `body` was serialised once, by the caller. The signature covers these exact bytes,
    // so the body sent and the body signed can never drift apart.
    const what = `${count} ${noun}(s)`;

    for (let attempt = 0; attempt <= this.retries; attempt++) {
      const controller = new AbortController();
      const abort = setTimeout(() => controller.abort(), this.timeout);
      (abort as { unref?: () => void }).unref?.();
      try {
        const response = await this.doFetch(url, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "user-agent": `micaforge-sdk-server/${VERSION}`,
            [SIGNATURE_HEADER]: signBody(this.ingestKey, body),
          },
          body,
          signal: controller.signal,
          keepalive: false,
        });
        if (response.ok) {
          this.log.debug?.(`sent ${what}`);
          return;
        }
        const retryable = response.status === 429 || response.status >= 500;
        if (!retryable) {
          this.log.error(
            `collector rejected ${what} with ${response.status}; dropped`,
            await safeText(response),
          );
          return;
        }
        if (attempt === this.retries) {
          this.log.warn(`collector returned ${response.status}; dropped ${what}`);
          return;
        }
      } catch (error) {
        if (attempt === this.retries) {
          this.log.warn(`could not reach ${url}; dropped ${what}`, error);
          return;
        }
      } finally {
        clearTimeout(abort);
      }
      await sleep(backoff(attempt));
    }
  }

  private installExitHook(): void {
    const proc = (globalThis as { process?: NodeJS.Process }).process;
    if (!proc || typeof proc.on != "function") return; // Workers and Deno Deploy.
    this.exitHook = () => {
      if (this.queued > 0 || this.inflight.size > 0) void this.flush();
    };
    proc.on("beforeExit", this.exitHook);
  }

  private removeExitHook(): void {
    const proc = (globalThis as { process?: NodeJS.Process }).process;
    if (this.exitHook && proc?.off) proc.off("beforeExit", this.exitHook);
    this.exitHook = null;
  }
}

/* ------------------------------------------------------------ small pieces */

/** `https://example.com/mf/` becomes `https://example.com/mf`; a bare host gets https. */
function normaliseHost(host: string): string {
  const raw = String(host ?? "").trim();
  if (!raw) return "";
  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(raw) ? raw : `https://${raw}`;
  return withScheme.replace(/\/+$/, "");
}

function headerValue(res: ResponseLike, name: string): string | undefined {
  try {
    const value = res.getHeader?.(name);
    if (value === undefined || value === null) return undefined;
    return Array.isArray(value) ? value[0] : String(value);
  } catch {
    return undefined;
  }
}

/** Exact body size when `content-length` was set, and nothing at all when it was not. */
function responseBytes(res: ResponseLike): number | undefined {
  const raw = headerValue(res, "content-length");
  if (raw === undefined) return undefined;
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : undefined;
}

/**
 * Wrap `write` and `end` to count the body exactly, for chunked responses.
 *
 * Opt-in, and defensive: if anything about the response is not what it should be, the
 * wrap is abandoned and the caller falls back to `content-length`. Breaking a customer's
 * response to measure it would be an absurd trade.
 */
function countResponseBytes(res: ResponseLike, log: Logger): (() => number | undefined) | null {
  try {
    const originalWrite = res.write;
    const originalEnd = res.end;
    if (typeof originalWrite != "function" || typeof originalEnd != "function") return null;

    let total = 0;
    const measure = (chunk: unknown, encoding?: unknown): void => {
      if (!chunk) return;
      if (typeof chunk == "string") {
        const enc = typeof encoding == "string" ? encoding : "utf8";
        total += Buffer.byteLength(chunk, enc as BufferEncoding);
      } else if (typeof (chunk as { length?: unknown }).length == "number") {
        total += (chunk as { length: number }).length;
      }
    };

    (res as Record<string, unknown>).write = function patched(this: unknown, ...args: unknown[]) {
      try {
        measure(args[0], args[1]);
      } catch {
        /* measurement is never allowed to break the write */
      }
      return (originalWrite as unknown as (...a: unknown[]) => boolean).apply(this, args);
    };
    (res as Record<string, unknown>).end = function patched(this: unknown, ...args: unknown[]) {
      try {
        if (typeof args[0] != "function") measure(args[0], args[1]);
      } catch {
        /* as above */
      }
      return (originalEnd as unknown as (...a: unknown[]) => unknown).apply(this, args);
    };

    return () => total;
  } catch (error) {
    log.debug?.("byte counting unavailable", error);
    return null;
  }
}

/** 250 ms, 500 ms, 1 s, ... with jitter, capped at 5 s. */
function backoff(attempt: number): number {
  const base = Math.min(250 * 2 ** attempt, 5000);
  return base + Math.random() * (base / 2);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    (timer as { unref?: () => void }).unref?.();
  });
}

async function safeText(response: { text(): Promise<string> }): Promise<string> {
  try {
    return (await response.text()).slice(0, 500);
  } catch {
    return "";
  }
}

export default Micaforge;
