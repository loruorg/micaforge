/**
 * The parts of the Nuxt integration that do not need Nuxt: the tracker tag's attributes,
 * the router-driven pageviews, the `useMicaforge()` calls and the reactive flag binding.
 *
 * They live apart from the files that import `#imports` so they can be tested without a
 * Nuxt app, and so the module (build time) and the plugins (run time) share one copy of
 * the attribute rules.
 */
import type { FlagRecord, FlagsClient, PageviewOverrides, PropValue, Props } from "@micaforge/sdk";
import { computed, type ComputedRef, type Ref } from "vue";

/** The tracker plugins a site can turn on, by the names the core loads them under. */
export type PluginName = "vitals" | "errors" | "frustration" | "forms" | "track404" | "tags";

export const PLUGIN_NAMES: readonly PluginName[] = ["vitals", "errors", "frustration", "forms", "track404", "tags"];

/** The tracker's own settings, the `data-*` attributes of the script tag. */
export interface TrackerOptions {
  /** path globs never reported (`data-exclude`) */
  exclude?: string[];
  /** path globs reported as the glob itself (`data-mask`) */
  mask?: string[];
  /** `true` drops the whole query string; a list drops those parameters (`data-strip-query`) */
  stripQuery?: boolean | string[];
  /** percent of visitors to keep, 0-100 (`data-sample`) */
  sample?: number;
  /** send nothing when the browser sets Do Not Track (`data-respect-dnt`) */
  respectDnt?: boolean;
  /** treat the hash as part of the page (`data-hash`) */
  hash?: boolean;
  /** log every payload and report from localhost (`data-debug`) */
  debug?: boolean;
  /** report outbound link clicks (`data-outbound`), default on */
  outbound?: boolean;
  /** report file downloads (`data-downloads`), default on */
  downloads?: boolean;
  /** track scroll depth (`data-scroll`), default on */
  scroll?: boolean;
  /** track engagement time (`data-engagement`), default on */
  engagement?: boolean;
  /** your release name, sent with captured errors (`data-release`) */
  release?: string;
}

/** Flag settings for `useFlag()`. */
export interface FlagOptions {
  /** values rendered into the first paint, keyed by flag key: `true`, a variant name, or a record */
  bootstrap?: Record<string, boolean | string | FlagRecord>;
  /** the environment the app runs in, such as `production` */
  environment?: string;
  /** attributes the flags' conditions test, such as `{ plan: "pro" }` */
  context?: Record<string, PropValue>;
  /** how long an answer is reused, in ms. Default 60000. */
  ttlMs?: number;
  /** send evaluation and exposure counts. Default true. */
  stats?: boolean;
}

/** What the module writes to `runtimeConfig.public.micaforge`, and the plugins read. */
export interface PublicConfig {
  siteKey: string;
  host: string;
  scriptSrc: string;
  enabled: boolean;
  /** report route changes from the Nuxt router (and switch the tracker's own listener off) */
  routeTracking: boolean;
  plugins: Partial<Record<PluginName, boolean>>;
  tracker: TrackerOptions;
  flags: FlagOptions;
}


/**
 * The tracker's `<script>` for `useHead`, or `null` when there is nothing to load (the
 * module is disabled, or has no site or host).
 *
 * `data-spa="false"` is set when the router reports pageviews: the tracker otherwise
 * patches `history.pushState` itself, and both reporting one navigation would count it
 * twice.
 */
export function trackerScript(config: PublicConfig): Record<string, string | boolean> | null {
  const site = String(config.siteKey ?? "").trim();
  const host = String(config.host || "").replace(/\/+$/, "");
  if (!config.enabled || !/^\d+$/.test(site) || +site < 1 || !host) return null;
  const t = config.tracker || {};
  const attrs: Record<string, string | boolean | undefined> = {
    key: "micaforge",
    src: config.scriptSrc || host + "/mf.js",
    defer: true,
    "data-site": site,
    "data-host": host,
    "data-spa": config.routeTracking ? "false" : undefined,
    "data-exclude": t.exclude && t.exclude.length ? t.exclude.join(",") : undefined,
    "data-mask": t.mask && t.mask.length ? t.mask.join(",") : undefined,
    "data-strip-query": t.stripQuery === true ? "true" : Array.isArray(t.stripQuery) && t.stripQuery.length ? t.stripQuery.join(",") : undefined,
    "data-sample": typeof t.sample == "number" && t.sample < 100 ? String(t.sample) : undefined,
    "data-respect-dnt": t.respectDnt ? "true" : undefined,
    "data-hash": t.hash ? "true" : undefined,
    "data-debug": t.debug ? "true" : undefined,
    "data-outbound": t.outbound === false ? "false" : undefined,
    "data-downloads": t.downloads === false ? "false" : undefined,
    "data-scroll": t.scroll === false ? "false" : undefined,
    "data-engagement": t.engagement === false ? "false" : undefined,
    "data-release": t.release || undefined,
  };
  for (const name of PLUGIN_NAMES) {
    const on = config.plugins ? config.plugins[name] : undefined;
    // vitals is on unless switched off; every other plugin is off unless switched on.
    const value = name == "vitals" ? (on === false ? "false" : undefined) : on ? "true" : undefined;
    attrs[name == "track404" ? "data-404" : "data-" + name] = value;
  }
  const out: Record<string, string | boolean> = {};
  for (const k of Object.keys(attrs)) if (attrs[k] !== undefined) out[k] = attrs[k] as string | boolean;
  return out;
}

/** `["errors"]`, `{ errors: true, vitals: false }` or nothing, as one map. Unknown names are dropped. */
export function normalisePlugins(plugins: PluginName[] | Partial<Record<PluginName, boolean>> | undefined): Partial<Record<PluginName, boolean>> {
  const out: Partial<Record<PluginName, boolean>> = {};
  if (Array.isArray(plugins)) {
    for (const p of plugins) if (PLUGIN_NAMES.includes(p)) out[p] = true;
  } else if (plugins && typeof plugins == "object") {
    for (const p of PLUGIN_NAMES) if (typeof plugins[p] == "boolean") out[p] = plugins[p];
  }
  return out;
}

/* ------------------------------------------------------------------ calls */

/** What `useMicaforge()` returns. Every method is safe during server rendering, where it does nothing. */
export interface MicaforgeApi {
  /** a custom event: `track("signup", { plan: "pro" })` */
  track(name: string, props?: Props): void;
  /** a pageview; the router already reports route changes, so this is for anything else */
  pageview(overrides?: PageviewOverrides): void;
  /** attach a stable id (and traits) to following events; flags follow the id too */
  identify(id: string, traits?: Props): void;
  /** record traits on the profile without changing the id */
  setTraits(traits: Props): void;
  /** merge default properties into every following event */
  setProps(props: Props): void;
  optOut(): void;
  optIn(): void;
  isOptedOut(): boolean;
  /** report pending engagement time now */
  flush(): void;
}

/**
 * The calls, through the tracker's own queue convention: `micaforge("event", name,
 * props)`. Before the deferred tag has run, the first call installs the documented queue
 * stub and the tracker replays it when it starts, so nothing called early is lost.
 */
export function createApi(getWindow: () => any, onIdentify?: (id: string) => void): MicaforgeApi {
  const call = (...args: unknown[]) => {
    try {
      const w = getWindow();
      if (!w) return;
      if (typeof w.micaforge != "function") {
        const stub: any = function (...a: unknown[]) {
          (stub.q = stub.q || []).push(a);
        };
        w.micaforge = stub;
      }
      w.micaforge(...args);
    } catch {}
  };
  return {
    track: (name, props) => call("event", name, props),
    pageview: (overrides) => call("pageview", overrides),
    identify: (id, traits) => {
      call("identify", id, traits);
      if (onIdentify) onIdentify(id);
    },
    setTraits: (traits) => call("setTraits", traits),
    setProps: (props) => call("setProps", props),
    optOut: () => call("optOut"),
    optIn: () => call("optIn"),
    isOptedOut: () => {
      try {
        const w = getWindow();
        if (!w) return false;
        if (w.micaforge && w.micaforge.mf) return !!w.micaforge.isOptedOut();
        return w.localStorage.getItem("mf-opt-out") == "1";
      } catch {
        return false;
      }
    },
    flush: () => call("flush"),
  };
}

/* ----------------------------------------------------------------- routes */

/** The slice of Vue Router this needs. */
export interface RouterLike {
  afterEach(guard: (to: unknown, from: unknown, failure?: unknown) => unknown): () => void;
}

/**
 * Report a pageview after every completed navigation that changed the address.
 *
 * The rule is the tracker's own SPA rule, so switching from its history listener to the
 * router changes nothing in the numbers: the address is the URL without its hash (unless
 * `hash`), a navigation that lands on the same address sends nothing, and a failed or
 * aborted navigation sends nothing. The first address is the one the tracker reports when
 * it starts, so it is never sent from here; and while the deferred tag has not started yet
 * nothing is sent either, because the tracker's first pageview will report wherever the
 * visitor is by then.
 */
export function trackRoutes(router: RouterLike, win: any, hash: boolean): () => void {
  const address = () => {
    const href = String((win.location || {}).href || "");
    return hash ? href : href.split("#")[0];
  };
  let last = address();
  return router.afterEach((_to, _from, failure) => {
    if (failure) return;
    try {
      const now = address();
      if (now == last) return;
      last = now;
      const mf = win.micaforge;
      if (mf && mf.mf) mf.pageview();
    } catch {}
  });
}

/* ------------------------------------------------------------------ flags */

/** A bootstrap map as flag records. */
export function normaliseBootstrap(raw: FlagOptions["bootstrap"]): Record<string, FlagRecord> {
  const out: Record<string, FlagRecord> = {};
  for (const key of Object.keys(raw || {})) {
    const v = (raw as Record<string, boolean | string | FlagRecord>)[key];
    if (v && typeof v == "object") out[key] = { ...v, key };
    else if (typeof v == "string") out[key] = { key, enabled: true, variant: v };
    else out[key] = { key, enabled: !!v };
  }
  return out;
}

/** What `useFlag()` returns: read-only and reactive. */
export interface FlagBinding {
  record: ComputedRef<FlagRecord>;
  enabled: ComputedRef<boolean>;
  variant: ComputedRef<string | undefined>;
  payload: ComputedRef<PropValue | undefined>;
}

/** The reactive view of one flag over the shared state. */
export function bindFlag(state: Ref<Record<string, FlagRecord>>, key: string, fallback: boolean): FlagBinding {
  const record = computed<FlagRecord>(() => state.value[key] || { key, enabled: fallback });
  return {
    record,
    enabled: computed(() => !!record.value.enabled),
    variant: computed(() => (record.value.enabled && record.value.variant) || undefined),
    payload: computed(() => record.value.payload),
  };
}

/**
 * Feed a flag client's answers into the shared state, once the app is ready.
 *
 * The state starts as the server rendered it (the bootstrap), on the server and on the
 * client alike, so hydration sees the same value on both sides; the client's answers are
 * applied only after `ready` has fired, which the Nuxt glue wires to `onNuxtReady`. The
 * first read of the key goes through the client so it is counted as an exposure.
 */
export function connectFlag(client: FlagsClient, state: Ref<Record<string, FlagRecord>>, key: string, ready: (fn: () => void) => void): void {
  ready(() => {
    try {
      client.get(key);
      client
        .load()
        .then((all) => {
          state.value = { ...state.value, ...all };
        })
        .catch(() => {});
    } catch {}
  });
}
