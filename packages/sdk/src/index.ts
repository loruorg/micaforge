/**
 * Micaforge SDK: the programmatic entry point for bundlers.
 *
 * `import { init, track } from "@micaforge/sdk"`.
 *
 * Isomorphic-safe: on a server every call is a no-op, because there is no `window` to
 * read and nothing to send. Tree-shakeable: the flag client only lands in your bundle
 * if you import it.
 */
import { createTracker, G, VERSION } from "./core.js";
import { createFlags } from "./flags.js";
import { requestCheckoutRef } from "./checkout.js";
import type {
  FlagsClient,
  FlagsConfig,
  PageviewOverrides,
  Props,
  TrackerApi,
  TrackerConfig,
} from "./types.js";

export * from "./types.js";
export {
  bucket,
  createTracker,
  globToRegExp,
  inSample,
  maskUrl,
  hasDownloadExtension,
  isExcluded,
  isOutboundUrl,
  DOWNLOAD_EXTENSIONS,
  OPT_OUT_KEY,
  VERSION,
} from "./core.js";
export { createFlags, hashBucket } from "./flags.js";
export { compileRuleset, evaluateFlag, evaluateFlags, flagBucket } from "./flag-eval.js";
export type {
  CompiledRuleset,
  FlagCondition,
  FlagConditionGroup,
  FlagConditions,
  FlagDecision,
  FlagDependency,
  FlagEnvironment,
  FlagInput,
  FlagReason,
  FlagRule,
  FlagRuleset,
  FlagVariant,
} from "./flag-eval.js";

interface Registry {
  i?: TrackerApi;
  site?: number | string;
  host?: string;
}

/** One registry per realm, so a page holding both this bundle and `/mf.js` shares one tracker. */
const registry = (): Registry => (G.__micaforge__ = G.__micaforge__ || {});

/** Everything `init()` takes. `host` defaults to the current origin. */
export interface InitConfig extends Omit<TrackerConfig, "site"> {
  site: number | string;
}

/**
 * Start the tracker.
 *
 * Calling it twice returns the first instance, and so does calling it on a page that
 * already loaded `/mf.js`: one page reports once. Pass `{ autoStart: false }` and call
 * `start()` yourself when you need to defer.
 */
export function init(config: InitConfig): TrackerApi {
  const existing = getTracker();
  if (existing) return existing;
  const r = registry();
  const w: any = G.window;
  const host = config.host || (w && w.location ? w.location.origin : "");
  r.site = config.site;
  r.host = host;
  r.i = createTracker({ ...config, host: host });
  return r.i;
}

/** The live tracker, adopting `window.micaforge` when the script tag started first. */
export function getTracker(): TrackerApi | undefined {
  const r = registry();
  if (!r.i) {
    const w: any = G.window;
    if (w && w.micaforge && w.micaforge.mf) {
      r.i = w.micaforge;
      r.site = w.micaforge.site;
      r.host = w.micaforge.host;
    }
  }
  return r.i;
}

/** True once a tracker exists in this realm. */
export const isReady = (): boolean => !!getTracker();

const call = (m: keyof TrackerApi, args: any[]): void => {
  try {
    const t: any = getTracker();
    if (t) t[m].apply(t, args);
  } catch {}
};

/** A custom event: `track("signup", { plan: "pro" })`. */
export const track = (name: string, props?: Props): void => call("track", [name, props]);

/** A pageview. Sent for you on `init()` and on SPA navigations unless you turn them off. */
export const pageview = (overrides?: PageviewOverrides): void => call("pageview", [overrides]);

/** Attach a stable id (and optional traits) to following events. */
export const identify = (id: string | null, traits?: Props): void => call("identify", [id, traits]);

/**
 * Record traits on the profile (`{ plan: "pro" }`) without changing the id or the
 * default properties. Each change is kept with its time, so the profile shows
 * the history rather than only the latest value.
 */
export const setTraits = (traits: Props): void => call("setTraits", [traits]);

/** Merge default properties into every following event. */
export const setProps = (props: Props): void => call("setProps", [props]);

/** Stop sending from this browser until `optIn()`. Survives reloads. */
export const optOut = (): void => call("optOut", []);

/** Undo `optOut()`. */
export const optIn = (): void => call("optIn", []);

export function isOptedOut(): boolean {
  try {
    const t = getTracker();
    return t ? t.isOptedOut() : false;
  } catch {
    return false;
  }
}

/** Report pending engagement time and web vitals now. */
export const flush = (): void => call("flush", []);

let flagClient: FlagsClient | undefined;

/**
 * The feature-flag client, bound to the tracker's site and host on first use.
 * `flags().isEnabled("new-nav")` is synchronous and never flickers: it answers from
 * the bootstrap payload or the deterministic local rule until the network replies.
 */
export function flags(config?: Partial<FlagsConfig>): FlagsClient {
  if (!flagClient || config) {
    getTracker();
    const r = registry();
    const w: any = G.window;
    flagClient = createFlags({
      site: r.site || 0,
      host: r.host || (w && w.location ? w.location.origin : ""),
      ...config,
    } as FlagsConfig);
  }
  return flagClient;
}

export { VERSION as version };

export { requestCheckoutRef } from "./checkout.js";
export type { CheckoutRefConfig } from "./checkout.js";

/**
 * This visit's checkout reference, for Stripe or Paddle: `mfr_…`, or `null`.
 *
 * Uses the site and host the tracker was started with. Pass the result to your checkout
 * as Stripe `client_reference_id` (or `metadata.mf_ref`) or Paddle `customData.mf_ref`.
 * See `requestCheckoutRef` in `./checkout.ts`.
 */
export async function checkoutRef(): Promise<string | null> {
  getTracker();
  const r = registry();
  const w: any = G.window;
  return requestCheckoutRef({
    site: r.site || "",
    host: r.host || (w && w.location ? w.location.origin : ""),
  });
}
