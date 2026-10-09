/**
 * Seeing what the tracker sends, without the tracker's help.
 *
 * # Why it hooks the transport and not the tracker
 *
 * The tracker core is one byte under its 3 KB gzipped budget and has no debug hook to
 * spare, and adding one would make every site in the world pay for a tool only
 * developers run. So the overlay costs the tracker nothing: it watches the three ways a
 * hit can leave a page, which the tracker looks up on every send rather than holding a
 * reference to, so a wrapper installed before or after the tracker sees every hit:
 *
 * - `navigator.sendBeacon(url, body)`, the tracker's first choice;
 * - `fetch(url, { body })`, its fallback, and the flag client's transport (which is how the
 *   overlay reads `GET /api/flags/evaluate` answers, reasons included);
 * - the `src` of an image, the last resort (`/api/track/pixel.gif?…`).
 *
 * Only requests to the collection paths are read; everything else passes through
 * untouched, and every wrapper calls the original first and inspects afterwards inside a
 * `try`, so the overlay can never change what a request does or make one fail.
 *
 * # What it learns from the API instead
 *
 * A hit the tracker refused never reaches the transport. To show those, the overlay also
 * wraps the tracker's public methods (`track`, `pageview`, `identify`, `setTraits`): a call
 * that returns without anything reaching the transport was refused, and the replayed gate
 * rules (`tracker.ts`) say why. Client-side navigation is watched the same way, so a
 * pageview the tracker would have sent on a route change shows up as refused too.
 */
import { Store } from "./store.js";
import { currentUrl, gateReason, preview, readSetup, rewriteOf, type TrackerSetup } from "./tracker.js";
import type { DevtoolsEvent, EventStatus } from "./types.js";
import { validate } from "./validate.js";

const HOOKED = "__mfDevtools";
const METHODS = ["track", "pageview", "identify", "setTraits"] as const;

/** Which collection route a URL is, if any. */
export function routeOf(url: string, base: string): "track" | "batch" | "pixel" | "flags" | "flag-events" | null {
  let path = "";
  try {
    path = new URL(url, base).pathname;
  } catch {
    return null;
  }
  if (/\/api\/track\/?$/.test(path)) return "track";
  if (/\/api\/track\/batch\/?$/.test(path)) return "batch";
  if (/\/api\/track\/pixel\.gif$/.test(path)) return "pixel";
  if (/\/api\/flags\/evaluate\/?$/.test(path)) return "flags";
  if (/\/api\/flags\/events\/?$/.test(path)) return "flag-events";
  return null;
}

/** The pixel's query string back into a payload: `p`, `e` and `flags` hold JSON text. */
export function payloadFromPixel(url: string, base: string): Record<string, unknown> | null {
  try {
    const out: Record<string, unknown> = {};
    new URL(url, base).searchParams.forEach((value, key) => {
      if (key == "p" || key == "e" || key == "flags") {
        try {
          out[key] = JSON.parse(value);
        } catch {
          out[key] = value;
        }
      } else if (/^(s|w|h|vw|vh|d|sc|sr)$/.test(key) && value !== "" && !isNaN(+value)) out[key] = +value;
      else out[key] = value;
    });
    return out;
  } catch {
    return null;
  }
}

/** Read a request body that may be a string or a Blob (the flag client beacons a Blob). */
function bodyText(win: any, body: unknown, then: (text: string) => void): void {
  if (typeof body == "string") then(body);
  else if (body && typeof (body as Blob).text == "function") (body as Blob).text().then(then, () => {});
  else if (body && typeof win.FileReader == "function" && body instanceof win.Blob) {
    // Safari before 14 has no `Blob.text()`.
    const reader = new win.FileReader();
    reader.onload = () => then(String(reader.result || ""));
    reader.readAsText(body);
  }
}

const parse = (text: string): any => {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
};

export interface Capture {
  /** re-read the tracker's configuration (it may have started after the overlay) */
  setup(): TrackerSetup;
  uninstall(): void;
}

/**
 * Install every hook. Returns a handle that removes them all again, leaving the page's
 * globals exactly as they were found.
 */
export function installCapture(win: any, store: Store, known?: string[], override?: Partial<TrackerSetup>): Capture {
  const undo: (() => void)[] = [];
  /**
   * Cleared on uninstall. A wrapper that something else wrapped in turn (the tracker
   * patches `history.pushState` too) cannot be taken out of the chain without breaking
   * the later wrapper, so it stays in place and goes inert instead.
   */
  let active = true;
  /** Put `orig` back only if `wrapped` is still the outermost layer. */
  const restore = (owner: any, key: string, wrapped: unknown, orig: unknown) => () => {
    if (owner[key] === wrapped) owner[key] = orig;
  };
  const nav = win.navigator;
  const base = () => String((win.location || {}).href || "http://localhost/");
  const started = typeof win.performance?.now == "function" ? () => win.performance.now() : () => 0;
  let setup = readSetup(win, override);
  /** Bumped on every hit that reaches a transport; a method call that leaves it alone was refused. */
  let sends = 0;
  let lastNavUrl = currentUrl(setup, win);

  const record = (payload: Record<string, unknown>, status: EventStatus, via?: string, reason?: string): DevtoolsEvent => {
    const url = String(payload.u || "");
    const props = payload.p && typeof payload.p == "object" ? (payload.p as Record<string, unknown>) : undefined;
    const kind = String(payload.t || "pageview");
    if (kind == "identify" || payload.id !== undefined) {
      const id = typeof payload.id == "string" ? payload.id : store.identity.id;
      if (id != store.identity.id || kind == "identify") store.setIdentity(id, kind == "identify" ? props : undefined);
    }
    return store.add({
      at: Date.now(),
      sincePageStart: Math.round(started()),
      kind,
      name: typeof payload.n == "string" ? payload.n : undefined,
      url,
      props,
      payload,
      status,
      via,
      reason,
      rewritten: status == "blocked" ? undefined : rewriteOf(url, setup, win),
      warnings: validate(payload, known),
    });
  };

  /** One request to a collection route, as it leaves. */
  const seen = (url: string, body: unknown, via: string): DevtoolsEvent[] => {
    const route = routeOf(url, base());
    if (!route) return [];
    if (route == "track" || route == "batch" || route == "pixel") sends++;
    const out: DevtoolsEvent[] = [];
    if (route == "pixel") {
      const p = payloadFromPixel(url, base());
      if (p) out.push(record(p, "sent", "pixel"));
    } else if (route == "track" && typeof body == "string") {
      const p = parse(body);
      if (p && typeof p == "object") out.push(record(p, "sent", via));
    } else if (route == "batch" && typeof body == "string") {
      const b = parse(body);
      const list = Array.isArray(b) ? b : b && Array.isArray(b.events) ? b.events : [];
      for (const p of list) if (p && typeof p == "object") out.push(record(p, "sent", "batch"));
    } else if (route == "flag-events") {
      bodyText(win, body, (text) => {
        const b = parse(text);
        for (const e of (b && Array.isArray(b.events) ? b.events : []) as any[]) {
          if (e && typeof e.key == "string") store.countFlag(e.key, String(e.kind), Number(e.count) || 1, e.enabled === true, typeof e.variant == "string" ? e.variant : null);
        }
      });
    }
    return out;
  };

  /* ---- transports ---- */

  if (nav && typeof nav.sendBeacon == "function" && !nav.sendBeacon[HOOKED]) {
    const orig = nav.sendBeacon;
    const wrapped: any = function (this: unknown, url: string, data?: unknown) {
      const ok = orig.call(this ?? nav, url, data);
      // A refused beacon is retried by the tracker over fetch, which is recorded there.
      if (ok && active) {
        try {
          seen(String(url), data, "beacon");
        } catch {}
      }
      return ok;
    };
    wrapped[HOOKED] = 1;
    nav.sendBeacon = wrapped;
    undo.push(restore(nav, "sendBeacon", wrapped, orig));
  }

  if (typeof win.fetch == "function" && !win.fetch[HOOKED]) {
    const orig = win.fetch;
    const wrapped: any = function (this: unknown, input: any, init?: any) {
      const result = orig.call(this ?? win, input, init);
      if (!active) return result;
      try {
        const url = typeof input == "string" ? input : input && (input.href || input.url) ? String(input.href || input.url) : "";
        const route = routeOf(url, base());
        if (route == "flags") {
          result
            .then((res: any) => (res && res.ok && res.clone ? res.clone().json() : null))
            .then((body: any) => {
              const data = body && body.data !== undefined ? body.data : body;
              const flags = data && data.flags !== undefined ? data.flags : data;
              if (flags && typeof flags == "object" && !Array.isArray(flags)) store.setFlags(flags);
            })
            .catch(() => {});
        } else if (route) {
          const events = seen(url, init && init.body, "fetch");
          if (events.length) {
            result.catch(() => {
              for (const e of events) {
                e.status = "failed";
                e.reason = "The request failed (offline, blocked by an extension, or a network error).";
              }
              store.touch();
            });
          }
        }
      } catch {}
      return result;
    };
    wrapped[HOOKED] = 1;
    win.fetch = wrapped;
    undo.push(restore(win, "fetch", wrapped, orig));
  }

  const imgProto = win.HTMLImageElement && win.HTMLImageElement.prototype;
  const srcDesc = imgProto && Object.getOwnPropertyDescriptor(imgProto, "src");
  if (srcDesc && srcDesc.set && !(srcDesc.set as any)[HOOKED]) {
    const set = srcDesc.set;
    const hookedSet: any = function (this: unknown, value: string) {
      set.call(this, value);
      if (!active) return;
      try {
        if (String(value).indexOf("/api/track/pixel.gif") > -1) seen(String(value), null, "pixel");
      } catch {}
    };
    hookedSet[HOOKED] = 1;
    Object.defineProperty(imgProto, "src", { ...srcDesc, set: hookedSet });
    undo.push(() => {
      const now = Object.getOwnPropertyDescriptor(imgProto, "src");
      if (now && now.set === hookedSet) Object.defineProperty(imgProto, "src", srcDesc);
    });
  }

  /* ---- the tracker's API ---- */

  /** Record a call the tracker returned from without sending. */
  const refused = (method: string, args: any[]) => {
    setup = readSetup(win, override);
    const reason = gateReason(setup, win);
    if (method == "identify" || method == "setTraits") {
      const id = method == "identify" ? String(args[0] ?? "") : store.identity.id;
      const traits = (method == "identify" ? args[1] : args[0]) || undefined;
      store.setIdentity(id, traits);
      record(preview("identify", setup, win, { id, props: traits }), "blocked", undefined,
        reason || "The same id and traits as the last call: the tracker sends a change once.");
      return;
    }
    if (!reason) return;
    if (method == "track") record(preview("custom", setup, win, { name: String(args[0] ?? ""), props: args[1] }), "blocked", undefined, reason);
    else record(preview("pageview", setup, win, { url: args[0] && args[0].url, props: args[0] && args[0].props }), "blocked", undefined, reason);
  };

  const hookApi = (api: any) => {
    if (!api || api[HOOKED]) return;
    api[HOOKED] = 1;
    for (const m of METHODS) {
      const orig = api[m];
      if (typeof orig != "function") continue;
      const wrapped = function (this: unknown, ...args: any[]) {
        const before = sends;
        const r = orig.apply(this, args);
        try {
          if (active && sends == before) refused(m, args);
        } catch {}
        return r;
      };
      api[m] = wrapped;
      undo.push(restore(api, m, wrapped, orig));
    }
    undo.push(() => delete api[HOOKED]);
  };

  // The tracker publishes itself on `window.micaforge` (script tag) and `globalThis.__mf`
  // (both entries, once started); the npm SDK's registry holds the same object as `__mf`.
  // The overlay usually loads first, and a page's own script can call the tracker the
  // moment it starts, so each global is watched with an accessor that hooks whatever is
  // assigned to it, at the moment of assignment. A global that already holds the tracker
  // is hooked at once.
  const watch = (name: string, isTracker: (v: any) => boolean) => {
    const prior = Object.getOwnPropertyDescriptor(win, name);
    if (prior && !prior.configurable) return;
    let value = win[name];
    if (value && isTracker(value)) hookApi(value);
    const accessor = {
      configurable: true,
      enumerable: true,
      get: () => value,
      set: (next: any) => {
        value = next;
        try {
          if (active && next && isTracker(next)) hookApi(next);
        } catch {}
      },
    };
    Object.defineProperty(win, name, accessor);
    undo.push(() => {
      const now = Object.getOwnPropertyDescriptor(win, name);
      if (now && now.get === accessor.get) Object.defineProperty(win, name, { value, writable: true, configurable: true, enumerable: true });
    });
  };
  watch("__mf", () => true);
  watch("micaforge", (v) => !!v.mf);

  /* ---- navigation, for the pageviews a refusing tracker never sends ---- */

  const onNav = () => {
    if (!active) return;
    win.setTimeout(() => {
      try {
        setup = readSetup(win, override);
        const url = currentUrl(setup, win);
        if (url == lastNavUrl) return;
        lastNavUrl = url;
        const reason = setup.spa ? gateReason(setup, win) : null;
        if (reason) record(preview("pageview", setup, win), "blocked", undefined, reason);
      } catch {}
    }, 30);
  };
  const history = win.history;
  for (const k of ["pushState", "replaceState"]) {
    const orig = history && history[k];
    if (typeof orig != "function" || orig[HOOKED]) continue;
    const wrapped: any = function (this: unknown, ...args: any[]) {
      const r = orig.apply(this ?? history, args);
      onNav();
      return r;
    };
    // Keep the tracker's own marker visible, so a tracker that starts after the overlay
    // still recognises the patched method as its own and does not wrap it twice.
    wrapped[HOOKED] = 1;
    if (orig.mf) wrapped.mf = orig.mf;
    history[k] = wrapped;
    undo.push(restore(history, k, wrapped, orig));
  }
  win.addEventListener("popstate", onNav);
  win.addEventListener("hashchange", onNav);
  undo.push(() => {
    win.removeEventListener("popstate", onNav);
    win.removeEventListener("hashchange", onNav);
  });

  // The first pageview: a refusing tracker sends nothing when it starts, so say so once.
  const initialReason = gateReason(setup, win);
  if (setup.source != "none" && setup.autoPageview && initialReason) {
    record(preview("pageview", setup, win), "blocked", undefined, initialReason);
  }

  return {
    setup: () => (setup = readSetup(win, override)),
    uninstall: () => {
      active = false;
      while (undo.length) {
        try {
          (undo.pop() as () => void)();
        } catch {}
      }
    },
  };
}
