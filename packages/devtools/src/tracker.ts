/**
 * What the tracker on this page is set to do, and why it would not send.
 *
 * # Why the rules are replayed here instead of reported by the tracker
 *
 * The tracker core is held to a 3 KB gzipped budget and sits one byte under it, so it has
 * no room to explain itself: when it decides not to send (opted out, outside the sample,
 * on localhost, an excluded path) it simply returns. The overlay therefore reads the same
 * configuration the tracker read (its `data-*` attributes, or the config the running
 * tracker carries) and applies the same rules, using the tracker's own exported helpers
 * from `@micaforge/sdk` wherever they exist, so there is one definition of each rule and
 * not a copy that drifts. Only `blocked()`'s short inline conditions are restated, and
 * each one cites the line of `core.ts` it mirrors.
 */
import { bucket, globToRegExp, isExcluded, maskUrl, OPT_OUT_KEY } from "@micaforge/sdk";

/** The tracker's configuration, as far as the overlay needs it. */
export interface TrackerSetup {
  /** where the configuration was read from */
  source: "script" | "running" | "none";
  site: number;
  host: string;
  debug: boolean;
  respectDnt: boolean;
  /** NaN when there is no `data-sample` */
  sample: number;
  exclude: string[];
  mask: string[];
  stripQuery: boolean | string[] | undefined;
  hash: boolean;
  spa: boolean;
  autoPageview: boolean;
}

const split = (v: unknown): string[] =>
  Array.isArray(v)
    ? v.map(String)
    : String(v ?? "")
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);

/** `core.ts` `flagOn`: an attribute string or a boolean, with a default. */
export const flagOn = (v: unknown, dflt: boolean): boolean =>
  v == null ? dflt : v !== false && !/^(false|0|off|no)$/i.test(String(v));

/**
 * Read the configuration: from the running tracker when it has started (it keeps its
 * config on `window.micaforge`), else from the tracker's script tag, else nothing.
 *
 * A tracker started from the npm package (`init()`) keeps only its site and host where a
 * page can read them, so an app using it passes the rest as `override` (the
 * `mountDevtools({ tracker })` option), with the same values it passed to `init()`.
 */
export function readSetup(win: any, override?: Partial<TrackerSetup>): TrackerSetup {
  let raw: any = null;
  let source: TrackerSetup["source"] = "none";
  const mf = win.micaforge;
  if (mf && mf.mf) {
    raw = mf;
    source = "running";
  } else {
    const el = win.document && win.document.querySelector("script[data-site]");
    if (el) {
      const at = (k: string) => el.getAttribute("data-" + k);
      const q = at("strip-query");
      raw = {
        site: at("site"),
        host: at("host") || String(el.src || "").replace(/^(\w+:\/\/[^/]*).*/, "$1"),
        debug: at("debug"),
        respectDnt: at("respect-dnt"),
        sample: at("sample"),
        exclude: split(at("exclude")),
        mask: split(at("mask")),
        stripQuery: q == null ? undefined : /^(true)?$/i.test(q) || split(q),
        hash: at("hash"),
        spa: at("spa"),
        autoPageview: at("auto-pageview"),
      };
      source = "script";
    } else {
      const registry = win.__micaforge__;
      if (registry && registry.i) raw = { site: registry.site, host: registry.host };
      if (raw) source = "running";
    }
  }
  raw = raw || {};
  const setup: TrackerSetup = {
    source,
    site: +(raw.site || 0) || 0,
    host: String(raw.host || ""),
    debug: flagOn(raw.debug, false),
    respectDnt: flagOn(raw.respectDnt, false),
    sample: parseFloat(raw.sample),
    exclude: split(raw.exclude),
    mask: split(raw.mask),
    stripQuery: raw.stripQuery,
    hash: flagOn(raw.hash, false),
    spa: flagOn(raw.spa, true),
    autoPageview: flagOn(raw.autoPageview, true),
  };
  return override ? { ...setup, ...override, source: setup.source == "none" && override.site ? "running" : setup.source } : setup;
}

/** `core.ts` `isOptedOut`, guarded the same way (Safari throws on the property itself). */
function optedOut(win: any): boolean {
  try {
    return win.localStorage.getItem(OPT_OUT_KEY) == "1";
  } catch {
    return false;
  }
}

/**
 * Why the tracker would not send right now, or `null` when it would. One sentence, and
 * where the rule has a fix, the fix.
 *
 * The order is `core.ts` `blocked()`'s, then the exclusion check `emit()` makes.
 */
export function gateReason(setup: TrackerSetup, win: any): string | null {
  const loc = win.location || {};
  const nav = win.navigator || {};
  if (!setup.site) return "No site id: the script tag needs data-site.";
  // `const sampled = !(rate < 100) || bucket(Date().slice(0, 15) + N.userAgent) < rate`
  if (setup.sample < 100 && !(bucket(Date().slice(0, 15) + nav.userAgent) < setup.sample)) {
    return `This browser is outside today's ${setup.sample}% sample (data-sample). Another day, or another browser, may be inside it.`;
  }
  if (optedOut(win)) return `This browser opted out (localStorage "${OPT_OUT_KEY}"). Call micaforge.optIn() to undo it.`;
  if (setup.respectDnt && nav.doNotTrack == "1") return "Do Not Track is on in this browser, and the tag sets data-respect-dnt.";
  if (!setup.debug && (loc.protocol == "file:" || /^(localhost|127\.|\[?::1|0\.0)|\.local(host)?$/i.test(loc.hostname || ""))) {
    return "This is localhost: the tracker does not report a developer's own page loads. Add data-debug to the tag to send from here.";
  }
  if (isExcluded(loc.pathname || "", setup.exclude)) return `The path ${loc.pathname} matches data-exclude.`;
  return null;
}

/**
 * Warnings about what the server will do with a hit this browser sends, which no rule in
 * the browser reveals.
 */
export function serverWarnings(win: any): string[] {
  const nav = win.navigator || {};
  const out: string[] = [];
  const ua = String(nav.userAgent || "");
  if (nav.webdriver === true || /headless|phantomjs|puppeteer|playwright|selenium|electron\//i.test(ua)) {
    out.push("This browser is automated (webdriver or a headless user agent). The server files its hits as a suspected bot, not as a visitor.");
  }
  if (nav.doNotTrack == "1" || nav.globalPrivacyControl === true) {
    out.push("Do Not Track or Global Privacy Control is on. If the site's \"Respect Do Not Track\" setting is on, the server drops these hits.");
  }
  return out;
}

/** The address the tracker reports for the current page: the URL without the hash, unless `data-hash`. */
export const currentUrl = (setup: TrackerSetup, win: any): string => {
  const href = String((win.location || {}).href || "");
  return setup.hash ? href : href.split("#")[0];
};

/**
 * When the address sent is not the address in the bar, say why: a mask glob replaced the
 * path, the query was stripped, or the call passed an address of its own.
 */
export function rewriteOf(sent: string, setup: TrackerSetup, win: any): { from: string; to: string; why: string } | undefined {
  const here = currentUrl(setup, win);
  if (!sent || sent == here) return undefined;
  const expected = maskUrl(here, setup);
  if (sent == expected) {
    let path = "";
    try {
      path = new URL(here).pathname;
    } catch {}
    const glob = setup.mask.find((g) => globToRegExp(g).test(path));
    const why = glob ? `masked by data-mask "${glob}"` : "query parameters removed by data-strip-query";
    return { from: here, to: sent, why };
  }
  return { from: here, to: sent, why: "the call passed its own address (or the page changed before it was sent)" };
}

/** What the tracker would have sent for a call it refused, built the way `emit()` builds it. */
export function preview(kind: string, setup: TrackerSetup, win: any, extra: { name?: string; url?: string; props?: unknown; id?: string } = {}): Record<string, unknown> {
  const p: Record<string, unknown> = { s: setup.site, t: kind, u: maskUrl(extra.url || currentUrl(setup, win), setup) };
  if (extra.name) p.n = extra.name;
  if (extra.props && typeof extra.props == "object" && Object.keys(extra.props as object).length) p.p = extra.props;
  if (extra.id) p.id = extra.id;
  return p;
}
