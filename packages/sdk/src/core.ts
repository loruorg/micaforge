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
 * `npm run size` fails the build when it is broken. That budget is also why error
 * capture and frustration signals are not in here: they are separate bundles
 * (`src/plugins/*`, served at `/mf/<name>.js`) that this file loads only when the
 * site asks for them, and they report through the one internal method, `x`.
 */
import type {
  EventKind,
  Flagish,
  PageviewOverrides,
  Payload,
  PluginEvent,
  Props,
  TrackerApi,
  TrackerConfig,
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
  return !!m && exts.includes(m[1].toLowerCase());
};

/** `data-*` attributes arrive as strings; programmatic config arrives as booleans. */
export const flagOn = (v: Flagish, dflt: boolean): boolean =>
  v == null ? dflt : v !== false && !/^(false|0|off|no)$/i.test(v as string);

/**
 * A 0-99 bucket for `seed`: FNV-1a, the same hash the flag client buckets with.
 * Deterministic, so the same seed lands in the same bucket on every page load.
 */
export const bucket = (seed: string): number => {
  let h = 0x811c9dc5;
  for (const c of seed) h = Math.imul(h ^ c.charCodeAt(0), 0x01000193);
  return (h >>> 0) % 100;
};

/**
 * `data-sample`: is this browser in today's sample?
 *
 * Decided per visitor and day without storing anything: the seed is today's
 * local date and the browser's own user agent, so every page a visitor opens
 * today agrees, and tomorrow is drawn again. The seed never leaves the page;
 * only the rate is sent (`sr`), so the server can say a report is sampled. No
 * rate, or 100 or more, keeps everyone.
 */
export const inSample = (rate: number, seed: string): boolean => !(rate < 100) || bucket(seed) < rate;

/**
 * `data-mask` and `data-strip-query`, applied to one URL before it is sent.
 *
 * A path that matches a mask glob is replaced by the glob itself, so `/users/123`
 * under the mask `/users/*` is reported as `/users/*`: the identifier never
 * leaves the page and every user's page counts as one page. The globs are the
 * ones `data-exclude` takes. `stripQuery` is `true` to drop the whole query
 * string, or the names of the parameters to drop. Anything that does not parse
 * (an empty referrer, say) is returned as it came.
 */
export const maskUrl = (href: string, c: Pick<TrackerConfig, "mask" | "stripQuery">): string => {
  try {
    const u = new URL(href),
      m = c.mask && c.mask.find((g) => globToRegExp(g).test(u.pathname)),
      q = c.stripQuery;
    if (m) u.pathname = m;
    if (q === true) u.search = "";
    else if (q) q.forEach((k) => u.searchParams.delete(k));
    return u.href;
  } catch {
    return href;
  }
};

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
  const D: any = W.document;
  const N: any = W.navigator || {};

  const LOC: any = W.location;
  const site = +(config.site as any);
  const endpoint = (config.host || "").replace(/\/$/, "") + "/api/track";
  const exts = config.downloadExtensions || DOWNLOAD_EXTENSIONS;
  const autoPageview = flagOn(config.autoPageview, true);
  const spa = flagOn(config.spa, true);
  const outbound = flagOn(config.outbound, true);
  const downloads = flagOn(config.downloads, true);
  const useHash = flagOn(config.hash, false);
  const respectDnt = flagOn(config.respectDnt, false);
  const doScroll = flagOn(config.scroll, true);
  const doEngagement = flagOn(config.engagement, true);
  const debug = flagOn(config.debug, false);
  // NaN without the attribute, which reads as "everyone": see `inSample`, whose
  // rule this is, written out here because the call would cost the budget bytes.
  const rate = parseFloat(config.sample as any);
  const sampled = !(rate < 100) || bucket(Date().slice(0, 15) + N.userAgent) < rate;

  let defaults: Props = { ...config.props };
  let identity = config.identity || "";
  let lastUrl = "";
  let engagedMs = 0;
  let visibleSince = 0;
  let scrollPct = 0;
  let ticking = false;
  let started = false;
  let tz = "";
  let auto = 0;
  let navTimer: any;
  let lastTraits = "";

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
    !sampled ||
    config.enabled === false ||
    isOptedOut() ||
    (respectDnt && N.doNotTrack == "1") ||
    // A developer's own refreshes are not data.
    (!debug &&
      (LOC.protocol == "file:" ||
        /^(localhost|127\.|\[?::1|0\.0)|\.local(host)?$/i.test(LOC.hostname)));

  // Replacing "" is a no-op, so the hash stays when `useHash` is on.
  const currentUrl = (): string => LOC.href.replace(useHash ? "" : (/#.*/ as any), "");

  /* transport: sendBeacon, then fetch(keepalive). Every browser that can parse this
     bundle has fetch, so there is no pixel fallback; the pixel is for no-JS pages. */

  const post = (p: Payload): void => {
    const body = JSON.stringify(p);
    try {
      if (N.sendBeacon(endpoint, body)) return;
    } catch {}
    // A string body is sent as text/plain, which keeps this a simple CORS request:
    // no preflight, no cost to the page. A throw here lands in emit()'s catch.
    W.fetch(endpoint, { method: "POST", body: body, keepalive: true }).catch(() => {});
  };

  /**
   * Build one payload and hand it to the transport.
   * The shape is CONTRACT.md section 4, exactly: nothing added, nothing renamed.
   */
  const emit = (t: EventKind, o: any): void => {
    try {
      if (blocked() || isExcluded(LOC.pathname, config.exclude)) return;
      const p: any = { s: site, t: t, u: maskUrl(o.url || currentUrl(), config), v: VERSION },
        // `raw` events (identify, error reports, frustration signals) carry their
        // own properties only: a trait change or a crash report is not a page
        // event that should inherit the page's default properties.
        props: any = { ...(o.raw ? 0 : defaults), ...o.props },
        // Everything else is optional: an empty, zero or absent value is left out,
        // because every column it maps to already carries that DEFAULT.
        rest: any = {
          n: o.name,
          r: maskUrl(o.referrer ?? D.referrer, config),
          w: W.screen.width,
          h: W.screen.height,
          vw: W.innerWidth,
          vh: W.innerHeight,
          l: N.language,
          tz: tz,
          d: o.d,
          sc: o.sc,
          id: identity,
          e: o.vitals,
          sr: rate < 100 && rate,
          a: auto,
        };
      for (const k in rest) if (rest[k]) p[k] = rest[k];
      if (Object.keys(props).length) p.p = props;
      if (debug) W.console.log("micaforge", p);
      post(p as Payload);
    } catch {}
  };

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
      vh = W.innerHeight,
      h = Math.max(de.scrollHeight, vh),
      // `h` is never below `vh`, so a page that does not scroll reads 100.
      pct = Math.min(100, Math.round(((W.scrollY + vh) / h) * 100));
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

  /**
   * The page is going out of sight: report the engagement it gathered, once
   * (`pagehide` follows `visibilitychange` on the way out, and the same page must
   * not arrive twice), then give a plugin that waits for this moment its turn.
   * The vitals plugin sets `h` to send its readings here, so `flush()` reports
   * both, exactly as it did when vitals lived in this file.
   */
  const reportHidden = () => {
    stopClock();
    if (engagedMs || scrollPct) {
      emit("engagement", { d: engagedMs, sc: scrollPct });
      engagedMs = scrollPct = 0;
    }
    if (api.h) api.h();
  };

  /* pageviews */

  const pageview = (ov?: PageviewOverrides): void => {
    const url = (ov && ov.url) || currentUrl();
    stopClock();
    // `d` and `sc` describe the page being LEFT, so the first pageview of a
    // document has nothing to report: the clock started at boot, and reporting
    // it would attribute the tracker's own startup time to a previous page that
    // never existed. It is a small number, one or two milliseconds, which is
    // exactly what makes it dangerous: it is not obviously wrong in a payload,
    // and it lands on the engagement figure of every first pageview.
    // `lastUrl` is empty until the first pageview, so it doubles as "one was sent".
    const d = lastUrl ? engagedMs : 0,
      sc = lastUrl ? scrollPct : 0;
    lastUrl = url;
    engagedMs = scrollPct = 0;
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
      orig = h[k];
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

  // No link throws into `guard`; an empty or `#` href is neither a file nor
  // another site, so neither check below reports it.
  const onClick = (ev: any) => {
    const href: string = ev.target.closest("a[href]").href;
    if (downloads && hasDownloadExtension(href, exts)) emit("file_download", { name: href });
    else if (outbound && isOutboundUrl(LOC.hostname, href))
      emit("outbound", { name: href });
  };

  /**
   * One `identify` event per change of id or traits, so the server can link
   * today's visitor digest to the id and keep the trait history. A repeat of the
   * same call (a router effect running on every navigation) sends nothing.
   */
  const sendTraits = (traits?: Props) => {
    const k = identity + JSON.stringify(traits || 0);
    if ((identity || traits) && k != lastTraits) {
      lastTraits = k;
      emit("identify", { props: traits, raw: 1 });
    }
  };

  const safePageview = guard(pageview);

  const start = () => {
    if (started) return;
    started = true;
    try {
      tz = Intl.DateTimeFormat().resolvedOptions().timeZone || "";
      // `a`: browser automation seen on this page, the CONTRACT.md section 4
      // bitmask. 1 navigator.webdriver, 2 CDP-driver globals, 4 Selenium,
      // Phantom or Nightmare globals, 8 a HeadlessChrome brand. Read once.
      const k = Object.keys(W) + "," + Object.keys(D);
      auto =
        ((N.webdriver === true) as any) |
        (/(^|,)\$?cdc_|__p(laywright|uppeteer|w_manual)|__PW_inspect/.test(k) as any) * 2 |
        ((/_phantom|callPhantom|__nightmare|domAutomation/.test(k) ||
          /selenium|webdriver|driver[-_]e|fxdriver/i.test(k)) as any) * 4 |
        (/HeadlessChrome/.test(JSON.stringify(N.userAgentData)) as any) * 8;
    } catch {}
    if (!D) return;
    const on = (t: any, ev: string, fn: any, opt?: any) => t.addEventListener(ev, guard(fn), opt);
    startClock();
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
    if (autoPageview) pageview();
    // The plugins: web vitals unless switched off, the rest only when asked
    // for, and none at all for a browser that would not report. They are served
    // under the ingest path, so every proxy that forwards hits forwards them with
    // no new rule. Each one finds this tracker on `__mf`. Form submissions and
    // the 404 marker live there too: both were opt-in, and neither loses anything
    // by arriving a moment after the page (a form is submitted after typing; the
    // marker is read once).
    G.__mf = api;
    if (!blocked())
      for (const k of "vitals errors frustration forms track404 tags".split(" "))
        if (flagOn((config as any)[k], k == "vitals"))
          D.head.append(Object.assign(D.createElement("script"), { src: endpoint + "/p/" + k + ".js" }));
  };

  const api: any = {
    track: (name: string, props?: Props) => emit("custom", { name: "" + name, props: props }),
    pageview: pageview,
    identify: (id: string | null, traits?: Props) => {
      identity = (id ?? "") + "";
      if (traits) defaults = { ...defaults, ...traits };
      sendTraits(traits);
    },
    setTraits: sendTraits,
    setProps: (props: Props) => (defaults = { ...defaults, ...props }),
    optOut: () => W.localStorage.setItem(OPT_OUT_KEY, "1"),
    optIn: () => W.localStorage.removeItem(OPT_OUT_KEY),
    isOptedOut: isOptedOut,
    flush: reportHidden,
    start: start,
    x: emit as (t: EventKind, o: PluginEvent) => void,
    // `GET /api/revenue/ref`: the server seals this visit into an `mfr_` token for
    // a Stripe or Paddle checkout. Resolves to it, or to nothing when the server
    // has none to give; never rejects, so a checkout cannot break on analytics.
    checkoutRef: () =>
      W.fetch(endpoint.replace("track", "revenue/ref?s=") + site)
        .then((r: any) => r.json())
        .then((b: any) => (b.data || 0).ref, () => {}),
  };
  for (const k in api) api[k] = guard(api[k]);

  if (config.autoStart !== false) api.start();
  return api as TrackerApi;
}
