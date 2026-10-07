/**
 * Micaforge SDK: the public types.
 *
 * The wire payload below is the one in `docs/CONTRACT.md` section 4. Keys are one or
 * two characters because the whole payload rides inside a `sendBeacon`. Nothing here
 * may be renamed or added to without the contract changing first.
 */

/** A JSON value that can travel in `props`. */
export type PropValue = string | number | boolean | null | PropValue[] | { [k: string]: PropValue };

/** Custom event properties (`props` in ClickHouse, `p` on the wire). */
export type Props = Record<string, PropValue | undefined>;

/** `kind` in the `events` table. */
export type EventKind =
  | "pageview"
  | "custom"
  | "outbound"
  | "file_download"
  | "form"
  | "error"
  | "web_vitals"
  | "engagement";

/** Core Web Vitals, sent as `e`. Every value is a number; CLS is unitless, the rest ms. */
export interface Vitals {
  lcp?: number;
  cls?: number;
  inp?: number;
  fcp?: number;
  ttfb?: number;
}

/**
 * `POST /api/track` body. CONTRACT.md section 4, exactly.
 * Empty, zero and absent values are omitted rather than sent, because every column
 * they map to carries a DEFAULT.
 */
export interface Payload {
  /** site id */
  s: number;
  /** event kind */
  t: EventKind;
  /** event name, custom events only */
  n?: string;
  /** full URL */
  u: string;
  /** document.referrer */
  r?: string;
  /** screen width / height */
  w?: number;
  h?: number;
  /** viewport width / height */
  vw?: number;
  vh?: number;
  /** navigator.language */
  l?: string;
  /** IANA time zone */
  tz?: string;
  /** engagement ms on the previous page */
  d?: number;
  /** max scroll percent on the previous page */
  sc?: number;
  /** custom properties */
  p?: Props;
  /** identify() id */
  id?: string;
  /** sdk version */
  v: string;
  /** web vitals */
  e?: Vitals;
}

/**
 * A value that may arrive either as a real boolean (programmatic config) or as a
 * `data-*` attribute string (script-tag config). `"false"`, `"0"`, `"off"`, `"no"`
 * read as false; a present-but-empty attribute reads as true.
 */
export type Flagish = boolean | string | null | undefined;

/** Everything `createTracker` accepts. */
export interface TrackerConfig {
  /** numeric site id (`site_id`). Required; without it nothing is sent. */
  site: number | string;
  /** ingest origin, e.g. `https://micaforge.example.com`. No trailing slash needed. */
  host?: string;
  /** send a pageview on start. Default true. */
  autoPageview?: Flagish;
  /** patch pushState/replaceState and listen to popstate + hashchange. Default true. */
  spa?: Flagish;
  /** auto-capture outbound link clicks. Default true. */
  outbound?: Flagish;
  /** auto-capture file download clicks. Default true. */
  downloads?: Flagish;
  /** extensions counted as a download. Defaults to `DOWNLOAD_EXTENSIONS`. */
  downloadExtensions?: string[];
  /** auto-capture form submissions (id/name only, never field values). Default false. */
  forms?: Flagish;
  /** report a 404 when the page carries the 404 marker. Default false. */
  track404?: Flagish;
  /** treat the URL hash as part of the page identity. Default false. */
  hash?: Flagish;
  /** glob paths that are never reported, e.g. `["/admin/**", "/preview/*"]`. */
  exclude?: string[];
  /** honour `navigator.doNotTrack === "1"`. Default false. */
  respectDnt?: Flagish;
  /** collect LCP/CLS/INP/FCP/TTFB. Default true. */
  vitals?: Flagish;
  /** track max scroll depth. Default true. */
  scroll?: Flagish;
  /** track engagement time. Default true. */
  engagement?: Flagish;
  /** report from localhost and file:// too, and log every payload. Default false. */
  debug?: Flagish;
  /** default properties merged into every event. */
  props?: Props;
  /** initial `identify()` id. */
  identity?: string;
  /** set false to build a tracker that sends nothing (a kill switch). Default true. */
  enabled?: boolean;
  /** set false to wire nothing up until `start()` is called. Default true. */
  autoStart?: boolean;
}

/** Per-call pageview overrides. */
export interface PageviewOverrides {
  /** full URL to report instead of the current one */
  url?: string;
  /** referrer to report instead of `document.referrer` */
  referrer?: string;
  /** properties for this pageview only */
  props?: Props;
}

/** The tracker instance. Every method is total: none of them throws. */
export interface TrackerApi {
  /** a custom event: `track("signup", { plan: "pro" })` */
  track(name: string, props?: Props): void;
  /** a pageview. Called for you unless `autoPageview` is off. */
  pageview(overrides?: PageviewOverrides): void;
  /** attach a stable id (and optional default props) to following events */
  identify(id: string | null, traits?: Props): void;
  /** merge default properties sent with every following event */
  setProps(props: Props): void;
  /** stop sending, on this browser, until `optIn()` */
  optOut(): void;
  /** undo `optOut()` */
  optIn(): void;
  isOptedOut(): boolean;
  /** report pending engagement time and web vitals right now */
  flush(): void;
  /** wire up listeners and send the first pageview. Idempotent. */
  start(): void;
}

/* ------------------------------------------------------------------ flags */

/** One evaluated flag. */
export interface FlagRecord {
  key: string;
  enabled: boolean;
  /** multivariate value, when the flag has variants */
  variant?: string;
  /** arbitrary JSON attached to the flag */
  payload?: PropValue;
}

/** A locally-evaluated rule, used before the network answers and when it never does. */
export interface FlagFallback {
  /** 0-100. The visitor buckets into the flag when `bucket < rolloutPct`. */
  rolloutPct?: number;
  /** weighted variants; weights are relative, they need not sum to 100. */
  variants?: { key: string; weight?: number }[];
  /** value when the visitor does not bucket in. Default false. */
  off?: boolean;
}

export interface FlagsConfig {
  site: number | string;
  /** ingest/API origin. Defaults to the tracker's host, then `location.origin`. */
  host?: string;
  /** stable bucketing subject. Without one, bucketing is stable for this page load only. */
  visitor?: string;
  /** `identified_id`, sent so server-side conditions can use it */
  identity?: string;
  /** flags rendered into the page by the server, so the first paint never flickers */
  bootstrap?: Record<string, FlagRecord | boolean | string>;
  /** in-memory cache lifetime in ms. Default 60000. */
  ttlMs?: number;
  /** deterministic local rules, keyed by flag key */
  fallback?: Record<string, FlagFallback>;
  /** called the first time a flag value is read, for exposure logging */
  onExposure?: (key: string, record: FlagRecord) => void;
  /** override the fetch used, for tests or a custom transport */
  fetchImpl?: typeof fetch;
}

export interface FlagsClient {
  /** fetch and cache the evaluated set. Never rejects. */
  load(force?: boolean): Promise<Record<string, FlagRecord>>;
  /** synchronous read: cache, then bootstrap, then the local fallback rule */
  get(key: string): FlagRecord;
  isEnabled(key: string, fallback?: boolean): boolean;
  variant(key: string, fallback?: string): string | undefined;
  payload(key: string): PropValue | undefined;
  all(): Record<string, FlagRecord>;
  /** subscribe to evaluated-set changes; returns the unsubscribe */
  onChange(cb: (flags: Record<string, FlagRecord>) => void): () => void;
  /** the visitor's 0-99 bucket for a key. Deterministic, no network. */
  bucket(key: string): number;
  /** set or change the bucketing subject */
  setVisitor(visitor: string): void;
  clearCache(): void;
}
