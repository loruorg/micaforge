/**
 * Micaforge core engine.
 *
 * Shared by the script-tag tracker (`dist/mf.js`) and the programmatic SDK. No
 * dependencies, no cookies, no storage beyond the single opt-out key, no
 * fingerprinting beyond the fields CONTRACT.md section 4 lists.
 *
 * Every public entry point, every API method and every DOM listener, is wrapped in
 * try/catch by `guard()`. This script must never throw a visible error into someone's
 * site. That is a product promise, not a nicety.
 *
 * Written tight on purpose: the built IIFE has a hard 3072-byte gzipped budget, and
 * `npm run size` fails the build when it is broken.
 */
import type {
  EventKind,
  Flagish,
  PageviewOverrides,
  Payload,
  Props,
  TrackerApi,
  TrackerConfig,
  Vitals,
} from "./types.js";

declare const __MF_VERSION__: string;

/** The `v` field on every payload (`sdk_version`). */
export const VERSION = typeof __MF_VERSION__ != "undefined" ? __MF_VERSION__ : "0.1.0";

/** The only key this SDK ever writes to storage. */
export const OPT_OUT_KEY = "mf-opt-out";

/** Extensions counted as a file download unless `downloadExtensions` overrides them. */
export const DOWNLOAD_EXTENSIONS =
  "pdf,csv,txt,docx,xlsx,pptx,zip,gz,rar,7z,dmg,pkg,exe,msi,deb,rpm,apk,mp3,mp4,mov,wav,webm".split(",");

/** The realm. Shared with the tracker entry so the bundle carries one copy. */
export const G: any = globalThis;
const R = Math.round;

/* ------------------------------------------------------------ pure helpers */

/** `/admin/*` matches one segment, `/admin/**` any depth, `?` one character. */
export const globToRegExp = (glob: string): RegExp =>
  new RegExp(
    "^" +
      glob.replace(/\*\*|[*?]|[.+^${}()|[\]\\]/g, (m) =>
        m == "**" ? ".*" : m == "*" ? "[^/]*" : m == "?" ? "[^/]" : "\\" + m,
      ) +
      "$",
  );

/** True when `path` matches any glob in `globs`. */
export const isExcluded = (path: string, globs?: string[] | null): boolean =>
  !!globs && globs.some((g) => globToRegExp(g).test(path));

// URL parsers already lowercase the host, so only the www prefix needs removing.
const bare = (h: string) => h.replace(/^www\./, "");

/**
 * True when `href` leaves this site. `docs.example.com` and `example.com` are the same
 * site; `example.com` and `notexample.com` are not. Only http(s) links count, so
 * `mailto:`, `tel:` and `javascript:` are never reported as outbound.
 */
export const isOutboundUrl = (currentHost: string, href: string): boolean => {
  try {
    const u = new URL(href),
      a = bare(currentHost),
      b = bare(u.hostname);
    return /^https?:$/.test(u.protocol) && !(a == b || a.endsWith("." + b) || b.endsWith("." + a));
  } catch {
    return false;
  }
};

/** True when the link points at a file with one of `exts`. */
export const hasDownloadExtension = (href: string, exts: string[]): boolean => {
  const m = /\.([a-z0-9]{1,8})(?:[?#]|$)/i.exec(href);
  return !!m && exts.indexOf(m[1].toLowerCase()) > -1;
};

/** `data-*` attributes arrive as strings; programmatic config arrives as booleans. */
export const flagOn = (v: Flagish, dflt: boolean): boolean =>
  v == null ? dflt : v !== false && !/^(false|0|off|no)$/i.test(v as string);

/** Wrap a function so it can never throw into the host page. */
const guard =
  (fn: any) =>
  (...a: any[]) => {
    try {
      return fn(...a);
    } catch {}
  };

/* ---------------------------------------------------------------- tracker */

/**
 * Build a tracker. Starts immediately unless `autoStart` is false.
 * Safe on a server: with no `window` it wires nothing up and sends nothing.
 */
export function createTracker(config: TrackerConfig): TrackerApi {
  const W: any = G.window || {};
  const D: any = W.document || {};
  const N: any = W.navigator || {};
  const SC: any = W.screen || {};

  const LOC: any = W.location;
  const site = +(config.site as any) || 0;
  const endpoint = (config.host || "").replace(/\/$/, "") + "/api/track";
  const pixel = endpoint + "/pixel.gif";
  const exts = config.downloadExtensions || DOWNLOAD_EXTENSIONS;
  const exclude = config.exclude;
  const autoPageview = flagOn(config.autoPageview, true);
  const spa = flagOn(config.spa, true);
  const outbound = flagOn(config.outbound, true);
  const downloads = flagOn(config.downloads, true);
  const forms = flagOn(config.forms, false);
  const track404 = flagOn(config.track404, false);
  const useHash = flagOn(config.hash, false);
  const respectDnt = flagOn(config.respectDnt, false);
  const doVitals = flagOn(config.vitals, true);
  const doScroll = flagOn(config.scroll, true);
  const doEngagement = flagOn(config.engagement, true);
  const debug = flagOn(config.debug, false);

  let defaults: Props = { ...config.props };
  let identity = config.identity || "";
  let lastUrl = "";
  let engagedMs = 0;
  let visibleSince = 0;
  let scrollPct = 0;
  let ticking = false;
  let vitals: Vitals = {};
  let vitalsSent = false;
  let started = false;
  let tz = "";
  let navTimer: any;

  // Safari with storage disabled throws on the property itself, so every read is guarded.
  const isOptedOut = (): boolean => {
    try {
      return W.localStorage.getItem(OPT_OUT_KEY) == "1";
    } catch {
      return false;
    }
  };

  const blocked = (): boolean =>
    !site ||
    config.enabled === false ||
    !LOC ||
    isOptedOut() ||
    (respectDnt && N.doNotTrack == "1") ||
    // A developer's own refreshes are not data.
    (!debug &&
      (LOC.protocol == "file:" ||
        /^(localhost|127\.|\[?::1|0\.0)|\.local(host)?$/i.test(LOC.hostname || "")));

  const currentUrl = (): string => {
    const h = LOC.href;
    return useHash ? h : h.split("#")[0];
  };

  /* transport: sendBeacon, then fetch(keepalive), then a 1x1 pixel GET */

  const query = (p: any): string =>
    Object.keys(p)
      .map((k) => k + "=" + encodeURIComponent(typeof p[k] == "object" ? JSON.stringify(p[k]) : p[k]))
      .join("&");

  const post = (p: Payload): void => {
    const body = JSON.stringify(p);
    try {
      if (N.sendBeacon(endpoint, body)) return;
    } catch {}
    try {
      // A string body is sent as text/plain, which keeps this a simple CORS request:
      // no preflight, no cost to the page.
      W.fetch(endpoint, { method: "POST", body: body, keepalive: true }).catch(() => {});
      return;
    } catch {}
    try {
      new W.Image().src = pixel + "?" + query(p);
    } catch {}
  };

  /**
   * Build one payload and hand it to the transport.
   * The shape is CONTRACT.md section 4, exactly: nothing added, nothing renamed.
   */
  const emit = (t: EventKind, o: any): void => {
    try {
      if (blocked() || isExcluded(LOC.pathname || "", exclude)) return;
      const p: any = { s: site, t: t, u: o.url || currentUrl(), v: VERSION },
        props: any = { ...defaults, ...o.props },
        // Everything else is optional: an empty, zero or absent value is left out,
        // because every column it maps to already carries that DEFAULT.
        rest: any = {
          n: o.name,
          r: o.referrer ?? D.referrer,
          w: SC.width,
          h: SC.height,
          vw: W.innerWidth,
          vh: W.innerHeight,
          l: N.language,
          tz: tz,
          d: o.d,
          sc: o.sc,
          id: identity,
          e: o.vitals,
        };
      for (const k in rest) if (rest[k]) p[k] = rest[k];
      if (Object.keys(props).length) p.p = props;
      if (debug) W.console.log("[micaforge]", p);
      post(p as Payload);
    } catch {}
  };

  /** Whether a pageview has been sent, so the first one reports no prior page. */
  let sentPageview = false;

  /* engagement time and scroll depth, both per page */

  const isVisible = () => D.visibilityState != "hidden";

  const stopClock = () => {
    if (visibleSince) {
      engagedMs += Date.now() - visibleSince;
      visibleSince = 0;
    }
  };

  const startClock = () => {
    if (doEngagement && !visibleSince && isVisible()) visibleSince = Date.now();
  };

  const measure = () => {
    if (!doScroll) return;
    const de = D.documentElement,
      vh = W.innerHeight || 0,
      h = Math.max(de.scrollHeight || 0, vh),
      pct = h <= vh ? 100 : Math.min(100, R((((W.scrollY || 0) + vh) / h) * 100));
    if (pct > scrollPct) scrollPct = pct;
  };

  const onScroll = () => {
    if (ticking) return;
    ticking = true;
    W.requestAnimationFrame(
      guard(() => {
        ticking = false;
        measure();
      }),
    );
  };

  /** Web vitals ride out once per document; engagement rides out on every hide. */
  const reportHidden = () => {
    stopClock();
    if (doVitals && !vitalsSent) {
      vitalsSent = true;
      // FCP and TTFB are already on the timeline, so neither needs an observer.
      try {
        const perf = W.performance,
          nav = perf.getEntriesByType("navigation")[0],
          fcp = perf.getEntriesByName("first-contentful-paint")[0];
        if (nav) vitals.ttfb = R(nav.responseStart);
        if (fcp) vitals.fcp = R(fcp.startTime);
      } catch {}
      if (Object.keys(vitals).length) emit("web_vitals", { vitals: vitals });
    }
    // Reported once. `pagehide` follows `visibilitychange` on the way out, and the
    // same page must not arrive twice.
    if (engagedMs || scrollPct) {
      emit("engagement", { d: engagedMs, sc: scrollPct });
      engagedMs = scrollPct = 0;
    }
  };

  /* web vitals, read straight off PerformanceObserver: no library, no polyfill */

  const observe = (type: string, cb: (e: any) => void, extra?: any) => {
    try {
      new W.PerformanceObserver((list: any) => list.getEntries().forEach(guard(cb))).observe({
        type: type,
        buffered: true,
        ...extra,
      });
    } catch {}
  };

  const collectVitals = () => {
    observe("largest-contentful-paint", (e) => (vitals.lcp = R(e.renderTime || e.startTime)));
    observe(
      "event",
      (e) => {
        if (e.interactionId && e.duration > (vitals.inp || 0)) vitals.inp = R(e.duration);
      },
      { durationThreshold: 40 },
    );
    // CLS as the worst 5s session window, the way the metric is defined.
    let val = 0,
      first = 0,
      last = 0;
    observe("layout-shift", (e) => {
      if (e.hadRecentInput) return;
      if (val && e.startTime - last < 1000 && e.startTime - first < 5000) {
        val += e.value;
        last = e.startTime;
      } else {
        val = e.value;
        first = last = e.startTime;
      }
      if (val > (vitals.cls || 0)) vitals.cls = val;
    });
  };

  /* pageviews */

  const pageview = (ov?: PageviewOverrides): void => {
    const url = (ov && ov.url) || currentUrl();
    lastUrl = url;
    stopClock();
    // `d` and `sc` describe the page being LEFT, so the first pageview of a
    // document has nothing to report: the clock started at boot, and reporting
    // it would attribute the tracker's own startup time to a previous page that
    // never existed. It is a small number, one or two milliseconds, which is
    // exactly what makes it dangerous: it is not obviously wrong in a payload,
    // and it lands on the engagement figure of every first pageview.
    const d = sentPageview ? engagedMs : 0,
      sc = sentPageview ? scrollPct : 0;
    sentPageview = true;
    engagedMs = 0;
    scrollPct = 0;
    startClock();
    emit("pageview", { url: url, referrer: ov && ov.referrer, props: ov && ov.props, d: d, sc: sc });
    measure();
  };

  /** History calls arrive in bursts; collapse them, and never repeat a URL. */
  const spaNav = guard(() => {
    clearTimeout(navTimer);
    navTimer = setTimeout(
      guard(() => {
        if (currentUrl() != lastUrl) safePageview();
      }),
      0,
    );
  });

  const patch = (k: string) => {
    const h = W.history,
      orig = h && h[k];
    if (!orig || orig.mf) return;
    const wrapped: any = (...a: any[]) => {
      const r = orig.apply(h, a);
      spaNav();
      return r;
    };
    wrapped.mf = 1;
    h[k] = wrapped;
  };

  /* auto-capture */

  const onClick = (ev: any) => {
    const a = ev.target.closest("a[href]");
    if (!a) return;
    const href: string = a.href || "";
    if (!href || href[0] == "#") return;
    if (downloads && hasDownloadExtension(href, exts)) emit("file_download", { name: href });
    else if (outbound && isOutboundUrl(LOC.hostname || "", href))
      emit("outbound", { name: href });
  };

  /** Form id or name only. Field values never leave the page. */
  const onSubmit = (ev: any) => emit("form", { name: ev.target.id || ev.target.name || "" });

  const safePageview = guard(pageview);

  const start = () => {
    if (started) return;
    started = true;
    try {
      tz = Intl.DateTimeFormat().resolvedOptions().timeZone || "";
    } catch {}
    if (!D.addEventListener) return;
    const on = (t: any, ev: string, fn: any, opt?: any) => t.addEventListener(ev, guard(fn), opt);
    startClock();
    if (doVitals) collectVitals();
    // No measure() here: the first pageview must not carry a scroll depth, and
    // pageview() takes a reading of its own once it has sent.
    if (doScroll) on(W, "scroll", onScroll, { passive: true });
    on(D, "visibilitychange", () => (isVisible() ? startClock() : reportHidden()));
    on(W, "pagehide", reportHidden);
    if (spa) {
      patch("pushState");
      patch("replaceState");
      on(W, "popstate", spaNav);
      on(W, "hashchange", spaNav);
    }
    if (outbound || downloads) on(D, "click", onClick, true);
    if (forms) on(D, "submit", onSubmit, true);
    if (autoPageview) pageview();
    if (track404 && D.querySelector("[data-mf-404]")) emit("error", { name: "404" });
  };

  const api: any = {
    track: (name: string, props?: Props) => emit("custom", { name: "" + name, props: props }),
    pageview: pageview,
    identify: (id: string | null, traits?: Props) => {
      identity = id == null ? "" : "" + id;
      if (traits) defaults = { ...defaults, ...traits };
    },
    setProps: (props: Props) => (defaults = { ...defaults, ...props }),
    optOut: () => W.localStorage.setItem(OPT_OUT_KEY, "1"),
    optIn: () => W.localStorage.removeItem(OPT_OUT_KEY),
    isOptedOut: isOptedOut,
    flush: reportHidden,
    start: start,
  };
  for (const k in api) api[k] = guard(api[k]);

  if (config.autoStart !== false) api.start();
  return api as TrackerApi;
}
