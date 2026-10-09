/**
 * Svelte bindings: one action and one store.
 *
 * Imports nothing from `svelte`: the store implements the store contract by hand, so
 * this works in Svelte 4, Svelte 5 and SvelteKit without a version pin.
 *
 * ```svelte
 * <script>
 *   import { micaforge, trackClick } from "@micaforge/sdk/svelte";
 *   micaforge.init({ site: 1, host: "https://micaforge.example.com" });
 * </script>
 *
 * <button use:trackClick={{ event: "cta_click", props: { plan: "pro" } }}>Start</button>
 * ```
 */
import {
  flush,
  getTracker,
  identify,
  init,
  optIn,
  optOut,
  pageview,
  setProps,
  setTraits,
  track,
  type InitConfig,
} from "../index.js";
import type { Props, TrackerApi } from "../types.js";

type Subscriber = (value: TrackerApi | undefined) => void;

const subscribers: Subscriber[] = [];

const publish = () => {
  const t = getTracker();
  for (const s of subscribers.slice()) {
    try {
      s(t);
    } catch {}
  }
};

/**
 * A readable store holding the tracker, plus the SDK surface as methods.
 * `$micaforge?.track("signup")` in a component, or `micaforge.track("signup")` anywhere.
 */
export const micaforge = {
  subscribe(run: Subscriber): () => void {
    subscribers.push(run);
    try {
      run(getTracker());
    } catch {}
    return () => {
      const i = subscribers.indexOf(run);
      if (i > -1) subscribers.splice(i, 1);
    };
  },
  init(config: InitConfig): TrackerApi | undefined {
    try {
      const t = init(config);
      publish();
      return t;
    } catch {
      return undefined;
    }
  },
  track: track,
  pageview: pageview,
  identify: identify,
  setTraits: setTraits,
  setProps: setProps,
  optIn: optIn,
  optOut: optOut,
  flush: flush,
  get current(): TrackerApi | undefined {
    return getTracker();
  },
};

export interface TrackClickParams {
  event: string;
  props?: Props;
  /** listen for this event instead of `click` */
  on?: string;
}

/** `use:trackClick={{ event: "cta_click" }}`, a Svelte action. */
export function trackClick(node: HTMLElement, params: TrackClickParams | string) {
  let current = typeof params == "string" ? { event: params } : params;
  const handler = () => {
    try {
      track(current.event, current.props);
    } catch {}
  };
  const bind = (p: TrackClickParams) => {
    try {
      node.addEventListener(p.on || "click", handler);
    } catch {}
  };
  const unbind = (p: TrackClickParams) => {
    try {
      node.removeEventListener(p.on || "click", handler);
    } catch {}
  };
  bind(current);
  return {
    update(next: TrackClickParams | string) {
      const parsed = typeof next == "string" ? { event: next } : next;
      if (parsed.on !== current.on) {
        unbind(current);
        current = parsed;
        bind(current);
      } else {
        current = parsed;
      }
    },
    destroy() {
      unbind(current);
    },
  };
}

/** `use:pageviewOn={$page.url.pathname}` sends a pageview when the value changes. */
export function pageviewOn(_node: HTMLElement, location: string) {
  let last = location;
  try {
    pageview();
  } catch {}
  return {
    update(next: string) {
      if (next === last) return;
      last = next;
      try {
        pageview();
      } catch {}
    },
  };
}

export default micaforge;
export { init, getTracker, track, pageview, identify, setTraits, setProps, optIn, optOut, flush } from "../index.js";
export type { InitConfig } from "../index.js";
export type { Props, TrackerApi } from "../types.js";
