/**
 * Micaforge server SDK: the public types and the wire shapes.
 *
 * Two vocabularies live in this file and the difference between them matters.
 *
 * - The **public** types ([`TrackEvent`], [`AgentHit`], [`MicaforgeOptions`]) are
 *   camelCase, because that is what reads well in the caller's code.
 * - The **wire** types ([`WireEvent`], [`WireAgentEvent`]) use the exact column names
 *   from `docs/CONTRACT.md` sections 1.1 and 1.2. Every key on the wire is a column,
 *   with two documented exceptions (`user_agent` and `ip` on an events row) that the
 *   server consumes and drops.
 *
 * The mapping between them is written out once, in `toWireEvent` / `toWireAgentEvent`
 * in `index.ts`. Nothing else in this package is allowed to invent a key.
 */

/* --------------------------------------------------------------- primitives */

/** A JSON value that can travel in `props`. */
export type PropValue = string | number | boolean | null | PropValue[] | { [k: string]: PropValue };

/** Custom event properties. Becomes the `props` JSON column. */
export type Props = Record<string, PropValue | undefined>;

/** `kind` in the `events` table (CONTRACT.md 1.1). */
export type EventKind =
  | "pageview"
  | "custom"
  | "outbound"
  | "file_download"
  | "form"
  | "error"
  | "web_vitals"
  | "engagement";

/**
 * A timestamp the SDK will accept.
 *
 * A `number` is milliseconds since the epoch, matching `Date.now()`. A string is
 * parsed by `Date`, so an ISO-8601 string from a log line works unchanged.
 */
export type Timestampish = Date | number | string;

/* ------------------------------------------------------------------ logging */

/**
 * Where this SDK reports its own failures.
 *
 * Injectable because the one thing this package must never do is throw into a
 * request that a real visitor is waiting on. Every failure is caught, handed to the
 * logger, and dropped. A silent default would hide a misconfigured ingest key for
 * weeks, so the default writes warnings to `console`; pass `logger: null` to silence
 * it entirely, or your own structured logger to fold it into your log stream.
 */
export interface Logger {
  debug?(message: string, detail?: unknown): void;
  warn(message: string, detail?: unknown): void;
  error(message: string, detail?: unknown): void;
}

/* ------------------------------------------------------------------ options */

/** Constructor options for {@link Micaforge}. */
export interface MicaforgeOptions {
  /**
   * Your Micaforge origin, for example `https://micaforge.example.com`. A path is
   * allowed and is kept, so an install behind a prefix (`https://example.com/mf`)
   * works. Requests go to `<host>/api/log/edge`.
   */
  host: string;

  /** The numeric site id (the `site_id` UInt16 that every table is keyed by). */
  siteId: number;

  /**
   * The site's ingest key. Every request body is signed with it (CONTRACT.md 3.3).
   *
   * This is a server-side secret. It is not the site id, it is not safe in a browser
   * bundle, and a build that ships it to the client should be treated as a leak: read
   * it from the environment.
   */
  ingestKey: string;

  /**
   * Milliseconds to hold a partial batch before sending it. Default `200`.
   *
   * The window exists so that a burst of hits becomes one request. It is short on
   * purpose: real-time is checked after every ship, and a visitor who arrives and
   * leaves inside a second should still appear.
   */
  flushInterval?: number;

  /** Send as soon as this many hits are queued. Default `50`. */
  maxBatch?: number;

  /** Milliseconds before a send is abandoned. Default `10000`. */
  timeout?: number;

  /**
   * How many times a failed send is retried. Default `2` (so three attempts).
   * Only a timeout, a network fault, `429` and `5xx` are retried; a `4xx` is a
   * request this SDK built wrongly and retrying it would only repeat the mistake.
   */
  retries?: number;

  /** Where failures are reported. `null` silences the SDK completely. */
  logger?: Logger | null;

  /**
   * Set `false` to build every event and then send none of them. Useful in tests and
   * in a local dev server, and it is the one switch that turns this package off
   * without deleting the calls. Default `true`.
   */
  enabled?: boolean;

  /**
   * Flush what is queued when the process is about to go idle. Default `true`.
   *
   * Hooks `beforeExit`, which fires only on a clean exit with an empty event loop.
   * A `SIGTERM` from an orchestrator does not fire it, so a long-lived server should
   * also `await micaforge.flush()` in its own shutdown path. {@link Micaforge.shutdown}
   * does both.
   */
  flushOnExit?: boolean;

  /**
   * Replace the HTTP transport. The default is the global `fetch`, which exists on
   * Node 20+, Workers, Deno and Bun. Supply your own to route through a proxy agent
   * or to assert on requests in a test.
   */
  fetch?: typeof globalThis.fetch;

  /**
   * Read the client's address from the usual proxy headers (`x-forwarded-for`,
   * `cf-connecting-ip`, `x-real-ip`, `true-client-ip`) before falling back to the
   * socket. Default `true`.
   *
   * Set it to `false` when nothing sits in front of your process. Those headers are
   * client-supplied, so an app that is directly exposed can have its geography lied to
   * one hit at a time; behind a proxy, they are the only way to see past the proxy.
   */
  trustProxy?: boolean;

  /**
   * Paths that are never reported. Matched against the pathname with `*` for one
   * segment and `**` for any depth, so `/_next/**` and `/health` both work.
   *
   * The middlewares default this to nothing: an asset fetch by a crawler is a real
   * crawl and hiding it would understate the agent side of the Citation Gap.
   */
  excludePaths?: string[];
}

/* ------------------------------------------------------------ public events */

/**
 * A human or custom event, destined for the `events` table (CONTRACT.md 1.1).
 *
 * Give either `url` or the parts (`hostname`, `pathname`, ...). When `url` is present
 * the SDK splits it locally and sends the parts, so nothing on the wire is a key that
 * the schema does not have.
 */
export interface TrackEvent {
  /** `kind`. Default `"custom"` when `name` is set, `"pageview"` otherwise. */
  kind?: EventKind;
  /** `event_name`. Required in practice for a custom event; ignored for a pageview. */
  name?: string;

  /** Full URL. Split locally into hostname, pathname, querystring and hash. */
  url?: string;
  /** `hostname`, when you are not passing a full `url`. */
  hostname?: string;
  /** `pathname`, when you are not passing a full `url`. */
  pathname?: string;
  /** `querystring`, without the leading `?`. */
  querystring?: string;
  /** `hash`, without the leading `#`. */
  hash?: string;

  /** `page_title`. */
  title?: string;
  /**
   * `referrer`, as the browser sent it. The server resolves it into `referrer_host`,
   * `channel` and `source`, which is how a ChatGPT or Perplexity referral becomes an
   * `answer_engine` arrival. Do not resolve it yourself.
   */
  referrer?: string;

  /**
   * The end client's user agent, not your server's.
   *
   * The server derives `browser`, `os` and `device_type` from it, and it is one of the
   * four inputs to `visitor_id` (CONTRACT.md 7.2). Without it a server-rendered
   * pageview has no device breakdown and a weaker visitor hash.
   */
  userAgent?: string;

  /**
   * The end client's IP address, not your server's.
   *
   * Transport only. The server resolves it to `country`, `region`, `city`, `asn` and
   * `asn_org`, folds it into `visitor_id`, and then drops it: no raw IP is ever
   * persisted on a human event row (CONTRACT.md 7.1). Omit it and you lose geography,
   * nothing else.
   */
  ip?: string;

  /** `language`, for example `en-GB`. */
  language?: string;
  /** `timezone`, an IANA name. */
  timezone?: string;

  /** `identified_id`. Opt-in, and only ever an id you already own. */
  identifiedId?: string;
  /** `props`. */
  props?: Props;
  /** `revenue`. */
  revenue?: number;
  /** `currency`, an ISO-4217 code. */
  currency?: string;
  /** `duration_ms`: engagement time on the previous page, not your response time. */
  durationMs?: number;
  /** `scroll_pct`, 0 to 100. */
  scrollPct?: number;
  /** `experiment` key. */
  experiment?: string;
  /** `variant` within the experiment. */
  variant?: string;
  /** `flags`: feature flag exposures, as key to variant. */
  flags?: Record<string, string>;

  /** `timestamp`. Defaults to now. */
  timestamp?: Timestampish;
}

/**
 * One fetch by a named non-human client, destined for `agent_events`
 * (CONTRACT.md 1.2).
 *
 * The fields this type does *not* have are the point. `agent_id`, `operator`,
 * `purpose`, `verified`, `asn`, `asn_org`, `country`, `ip_hash`, `robots_allowed` and
 * `llms_listed` are all resolved by the server, against the catalog, the operator's
 * published IP ranges and the site's own `robots.txt` and `llms.txt`. A client cannot
 * do that work honestly, so it is not offered the chance to guess.
 */
export interface AgentHit {
  /** The raw `User-Agent` header. This is what the server classifies on. */
  userAgent: string;

  /** Full URL. Split locally into hostname, pathname and querystring. */
  url?: string;
  /** `hostname`, when you are not passing a full `url`. */
  hostname?: string;
  /** `pathname`, when you are not passing a full `url`. */
  pathname?: string;
  /** `querystring`, without the leading `?`. */
  querystring?: string;

  /** `method`. Default `GET`. */
  method?: string;
  /** `status`, the response code you returned. Default `200`. */
  status?: number;
  /** `bytes` written in the response body. */
  bytes?: number;
  /** `duration_ms`: how long you took to answer. */
  durationMs?: number;
  /** `content_type`, without parameters (`text/html`, not `text/html; charset=utf-8`). */
  contentType?: string;

  /**
   * `conditional`: true when the request carried `If-None-Match` or
   * `If-Modified-Since`, which makes it a re-crawl check rather than a fresh read.
   * The middlewares set this for you.
   */
  conditional?: boolean;

  /**
   * The agent's IP address. Transport only: the server verifies it against the
   * operator's published ranges to set `verified`, resolves `asn` and `country`, and
   * stores only the salted `ip_hash`.
   */
  ip?: string;

  /** `timestamp`. Defaults to now. */
  timestamp?: Timestampish;
}

/** One queued item, either kind. Accepted by {@link Micaforge.batch}. */
export type QueuedEvent =
  | ({ type: "event" } & TrackEvent)
  | ({ type: "agent" } & AgentHit);

/* -------------------------------------------------------------- wire shapes */

/**
 * A row for the `events` table.
 *
 * Every key is a column in CONTRACT.md 1.1, except `user_agent` and `ip`, which are
 * transport fields: the server reads them to derive the columns it does have and never
 * writes either one down.
 */
export interface WireEvent {
  timestamp: string;
  kind: EventKind;
  event_name?: string;
  hostname: string;
  pathname: string;
  page_title?: string;
  querystring?: string;
  hash?: string;
  referrer?: string;
  language?: string;
  timezone?: string;
  identified_id?: string;
  props?: Props;
  revenue?: number;
  currency?: string;
  duration_ms?: number;
  scroll_pct?: number;
  experiment?: string;
  variant?: string;
  flags?: Record<string, string>;
  sdk_version: string;
  /** Transport: derives `browser`, `os`, `device_type` and part of `visitor_id`. */
  user_agent?: string;
  /** Transport: derives `country`, `region`, `city`, `asn`. Never persisted (7.1). */
  ip?: string;
}

/**
 * A row for the `agent_events` table.
 *
 * Every key is a column in CONTRACT.md 1.2, except `ip`, which is transport: the
 * server verifies it and stores only `ip_hash`.
 */
export interface WireAgentEvent {
  timestamp: string;
  user_agent: string;
  method: string;
  hostname: string;
  pathname: string;
  querystring?: string;
  status?: number;
  bytes?: number;
  duration_ms?: number;
  content_type?: string;
  conditional?: 0 | 1;
  /** Transport: verified against published ranges, then hashed into `ip_hash`. */
  ip?: string;
}

/**
 * The `POST /api/log/edge` body.
 *
 * Two arrays named after the two tables they land in, so the request says exactly what
 * it is asking the server to write. An empty array is omitted rather than sent.
 */
export interface EdgePayload {
  site_id: number;
  events?: WireEvent[];
  agent_events?: WireAgentEvent[];
}

/* -------------------------------------------------------------- request-ish */

/**
 * The shape this SDK can read a request out of.
 *
 * Deliberately structural rather than a framework type. A Node `IncomingMessage`, an
 * Express `Request`, a Fetch `Request`, a Hono `c.req.raw` and a hand-built object all
 * satisfy it, and none of them are imported, so this package keeps its promise of zero
 * dependencies.
 */
export interface RequestLike {
  /** Node style: `/docs/x?a=1`. Fetch style requests use `url` as an absolute URL. */
  url?: string;
  method?: string;
  /** Node style: a plain object. Fetch style: a `Headers`. Both are read. */
  headers?: HeadersLike;
  /** Express fills these in; they are used when the headers do not settle the host. */
  hostname?: string;
  protocol?: string;
  path?: string;
  query?: unknown;
  /** Node's socket, read only for `remoteAddress` when no proxy header is present. */
  socket?: { remoteAddress?: string | undefined } | undefined;
  connection?: { remoteAddress?: string | undefined } | undefined;
  /** Express and some proxies set this. */
  ip?: string;
}

/** A plain header bag or anything with a `get(name)`, which covers `Headers`. */
export type HeadersLike =
  | Record<string, string | string[] | undefined>
  | { get(name: string): string | null };

/** Extra facts about the response, known only after it was written. */
export interface HitExtras {
  /** `status` on an agent row. Dropped for a human row: `events` has no status column. */
  status?: number;
  /** `bytes` on an agent row. Dropped for a human row. */
  bytes?: number;
  /** `duration_ms` on an agent row: your response time. Dropped for a human row. */
  durationMs?: number;
  /** `content_type` on an agent row. Dropped for a human row. */
  contentType?: string;
  /** `event_name`, human rows only. */
  name?: string;
  /** `page_title`, human rows only. */
  title?: string;
  /** `props`, human rows only. */
  props?: Props;
  /** `identified_id`, human rows only. */
  identifiedId?: string;
  /** Overrides the clock. */
  timestamp?: Timestampish;
}
