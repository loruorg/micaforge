/**
 * Tracker tests.
 *
 * Deliberately jsdom-free: every browser global the tracker touches is stubbed by hand
 * here. That keeps the test honest about the tracker's real surface: if it ever
 * reaches for a global this file does not define, the test fails rather than quietly
 * passing on jsdom's much larger DOM.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  bucket,
  createTracker,
  flagOn,
  globToRegExp,
  hasDownloadExtension,
  inSample,
  isExcluded,
  isOutboundUrl,
  maskUrl,
} from "../core.js";
import { readScriptConfig } from "./index.js";
import type { TrackerConfig } from "../types.js";

const HOST = "https://mf.example.com";
const ENDPOINT = HOST + "/api/track";

interface Sent {
  url: string;
  via: "beacon" | "fetch" | "pixel";
  body: any;
}

/** A hand-built browser, exactly as large as the tracker needs it to be. */
function stubBrowser(
  opts: {
    href?: string;
    hostname?: string;
    protocol?: string;
    dnt?: string;
    optedOut?: boolean;
    beacon?: "ok" | "full" | "throw" | "missing";
    fetch?: "ok" | "throw" | "missing";
    image?: "ok" | "throw";
    referrer?: string;
  } = {},
) {
  const sent: Sent[] = [];
  const scripts: string[] = [];
  const handlers: Record<string, ((e?: any) => void)[]> = {};
  const storage: Record<string, string> = {};
  if (opts.optedOut) storage["mf-opt-out"] = "1";

  const href = opts.href || "https://example.com/docs/x?a=1#h";
  const u = new URL(href);
  const location: any = {
    href: href,
    origin: u.origin,
    protocol: opts.protocol || u.protocol,
    hostname: opts.hostname || u.hostname,
    pathname: u.pathname,
    search: u.search,
    hash: u.hash,
  };

  const listen =
    (prefix: string) =>
    (type: string, fn: (e?: any) => void): void => {
      (handlers[prefix + type] = handlers[prefix + type] || []).push(fn);
    };

  const document: any = {
    referrer: opts.referrer === undefined ? "https://chatgpt.com/" : opts.referrer,
    visibilityState: "visible",
    documentElement: { scrollHeight: 2000, clientHeight: 800 },
    addEventListener: listen("d:"),
    querySelector: () => null,
    currentScript: null,
    // Just enough for the plugin loader: it makes a <script> and appends it.
    createElement: (tag: string) => ({ tagName: tag }),
    head: { append: (el: any) => scripts.push(el.src) },
  };

  const navigator: any = { language: "en-GB", userAgent: "Mozilla/5.0 (test)" };
  if (opts.dnt) navigator.doNotTrack = opts.dnt;
  if (opts.beacon !== "missing")
    navigator.sendBeacon = (url: string, body: string) => {
      if (opts.beacon === "throw") throw new Error("beacon blew up");
      if (opts.beacon === "full") return false;
      sent.push({ url, via: "beacon", body: JSON.parse(body) });
      return true;
    };

  const window: any = {
    document,
    navigator,
    screen: { width: 1512, height: 982 },
    innerWidth: 1280,
    innerHeight: 720,
    scrollY: 0,
    location,
    localStorage: {
      getItem: (k: string) => (k in storage ? storage[k] : null),
      setItem: (k: string, v: string) => (storage[k] = "" + v),
      removeItem: (k: string) => delete storage[k],
    },
    history: { pushState: () => {}, replaceState: () => {} },
    addEventListener: listen("w:"),
    requestAnimationFrame: (cb: () => void) => cb(),
    console: { log: () => {} },
    Image: class {
      set src(v: string) {
        if (opts.image === "throw") throw new Error("no images here");
        const [url, qs] = v.split("?");
        sent.push({ url, via: "pixel", body: Object.fromEntries(new URLSearchParams(qs)) });
      }
    },
  };
  if (opts.fetch !== "missing")
    window.fetch = (url: string, init: any) => {
      if (opts.fetch === "throw") throw new Error("fetch blew up");
      sent.push({ url, via: "fetch", body: JSON.parse(init.body) });
      return Promise.resolve({ ok: true });
    };
  window.window = window;

  vi.stubGlobal("window", window);
  vi.stubGlobal("Intl", {
    DateTimeFormat: () => ({ resolvedOptions: () => ({ timeZone: "Europe/Lisbon" }) }),
  });

  return {
    sent,
    scripts,
    storage,
    window,
    document,
    location,
    /** Dispatch to the listeners the tracker registered on window ("w:") or document ("d:"). */
    fire(key: string, event?: any) {
      (handlers[key] || []).forEach((fn) => fn(event));
    },
    /** Move the fake browser to a new URL, the way a router would. */
    goto(path: string) {
      const next = new URL(path, location.origin);
      location.href = next.href;
      location.pathname = next.pathname;
      location.search = next.search;
      location.hash = next.hash;
    },
  };
}

const start = (cfg: Partial<TrackerConfig> = {}) =>
  createTracker({ site: 1, host: HOST, vitals: false, ...cfg } as TrackerConfig);

/**
 * Let the tracker's own deferred work run.
 *
 * `spaNav` collapses a burst of history calls behind `setTimeout(..., 0)`. Racing
 * that with a single `setTimeout(r, 1)` is not deterministic: Node clamps nested
 * zero-delay timers to 1ms, so under load both land in the same expiry bucket and
 * the assertion can run before the tracker's callback has. Yielding several full
 * turns of the macrotask queue removes the race instead of narrowing it, and a
 * test that fails once in twenty runs is worse than no test at all.
 */
const tick = async () => {
  for (let i = 0; i < 5; i++) await new Promise((r) => setTimeout(r, 0));
};


afterEach(() => vi.unstubAllGlobals());

describe("payload construction", () => {
  it("sends exactly the keys CONTRACT.md section 4 defines", () => {
    const b = stubBrowser();
    start();

    expect(b.sent).toHaveLength(1);
    expect(b.sent[0].url).toBe(ENDPOINT);
    expect(b.sent[0].via).toBe("beacon");
    expect(b.sent[0].body).toEqual({
      s: 1,
      t: "pageview",
      u: "https://example.com/docs/x?a=1",
      v: "0.1.0",
      r: "https://chatgpt.com/",
      w: 1512,
      h: 982,
      vw: 1280,
      vh: 720,
      l: "en-GB",
      tz: "Europe/Lisbon",
    });
  });

  it("omits every empty, zero and absent field rather than sending it", () => {
    const b = stubBrowser({ referrer: "" });
    b.window.screen = { width: 0, height: 0 };
    b.window.innerWidth = 0;
    b.window.innerHeight = 0;
    delete b.window.navigator.language;
    start();

    expect(Object.keys(b.sent[0].body).sort()).toEqual(["s", "t", "tz", "u", "v"]);
  });

  it("carries the hash only when data-hash is on", () => {
    const withHash = stubBrowser();
    start({ hash: true });
    expect(withHash.sent[0].body.u).toBe("https://example.com/docs/x?a=1#h");
  });

  it("puts custom events, props and identity in n, p and id", () => {
    const b = stubBrowser();
    const mf = start({ autoPageview: false });
    mf.identify("u_123", { plan: "pro" });
    mf.setProps({ theme: "dark" });
    mf.track("signup", { step: 2 });

    // identify() itself sends one `identify` event first; the custom event follows.
    expect(b.sent[0].body.t).toBe("identify");
    const p = b.sent[1].body;
    expect(p.t).toBe("custom");
    expect(p.n).toBe("signup");
    expect(p.id).toBe("u_123");
    expect(p.p).toEqual({ plan: "pro", theme: "dark", step: 2 });
    expect(p).not.toHaveProperty("e");
  });

  it("never invents a key the contract does not list", () => {
    const b = stubBrowser();
    const mf = start();
    mf.track("x", { a: 1 });
    const allowed = "s t n u r w h vw vh l tz d sc p id v e sr a".split(" ");
    for (const hit of b.sent) for (const k of Object.keys(hit.body)) expect(allowed).toContain(k);
  });

  it("reports engagement time and scroll depth on hide, then a pageview carries what is left", async () => {
    const b = stubBrowser();
    // A real clock never reads 0; the tracker uses 0 as its "stopped" sentinel.
    let clock = 1000;
    vi.spyOn(Date, "now").mockImplementation(() => clock);

    const mf = start({ autoPageview: false });
    mf.pageview();
    clock = 5210;
    b.window.scrollY = 1000;
    b.fire("w:scroll");
    b.document.visibilityState = "hidden";
    b.fire("d:visibilitychange");

    const hide = b.sent[1].body;
    expect(hide.t).toBe("engagement");
    expect(hide.d).toBe(4210);
    expect(hide.sc).toBe(86);

    // Back to visible, away again: the time already reported is not reported twice.
    b.document.visibilityState = "visible";
    b.fire("d:visibilitychange");
    clock = 6000;
    b.document.visibilityState = "hidden";
    b.fire("d:visibilitychange");
    expect(b.sent[2].body.d).toBe(790);

    // And the next pageview carries only what has not been reported yet.
    b.document.visibilityState = "visible";
    b.fire("d:visibilitychange");
    clock = 7000;
    mf.pageview();
    expect(b.sent[3].body.t).toBe("pageview");
    expect(b.sent[3].body.d).toBe(1000);
    vi.restoreAllMocks();
  });

  it("reports the page it is leaving once, not once per hide event", () => {
    const b = stubBrowser();
    let clock = 1000;
    vi.spyOn(Date, "now").mockImplementation(() => clock);
    start();
    clock = 3000;

    b.document.visibilityState = "hidden";
    b.fire("d:visibilitychange");
    b.fire("w:pagehide"); // browsers fire both on the way out

    expect(b.sent.filter((hit) => hit.body.t === "engagement")).toHaveLength(1);
    vi.restoreAllMocks();
  });

  it("measures nothing when engagement and scroll are switched off", () => {
    const b = stubBrowser();
    let clock = 1000;
    vi.spyOn(Date, "now").mockImplementation(() => clock);
    const mf = start({ engagement: false, scroll: false });
    clock = 9000;
    b.window.scrollY = 1500;
    b.fire("w:scroll");
    b.document.visibilityState = "hidden";
    b.fire("d:visibilitychange");
    mf.pageview();

    for (const hit of b.sent) {
      expect(hit.body).not.toHaveProperty("d");
      expect(hit.body).not.toHaveProperty("sc");
      expect(hit.body.t).not.toBe("engagement");
    }
    vi.restoreAllMocks();
  });
});

describe("the first pageview", () => {
  it("reports no engagement time and no scroll depth, because there is no previous page", () => {
    const b = stubBrowser();
    start();

    // `d` and `sc` describe the page being left. On the first pageview of a
    // document there is no such page, and the only thing the clock has measured
    // is how long the tracker itself took to boot. That figure is one or two
    // milliseconds, which is small enough to look plausible in a payload and
    // wrong enough to skew the engagement figure of every first pageview. It
    // once shipped, and it showed up as an intermittently failing assertion
    // rather than as an obviously bad number.
    expect(b.sent).toHaveLength(1);
    expect(b.sent[0].body).not.toHaveProperty("d");
    expect(b.sent[0].body).not.toHaveProperty("sc");
  });

  it("reports engagement time on the SECOND pageview, which does have a previous page", async () => {
    const b = stubBrowser();
    const t = start();
    b.goto("/next");
    t.pageview();
    await tick();

    expect(b.sent).toHaveLength(2);
    // The key is present or absent depending on whether any visible time was
    // actually accumulated; what matters is that the guard applies only to the
    // first pageview and never suppresses a real reading afterwards.
    expect(b.sent[1].body.u).toBe("https://example.com/next");
  });
});

describe("SPA navigation", () => {
  it("reports one pageview per distinct URL and never repeats one", async () => {
    const b = stubBrowser();
    start();
    expect(b.sent).toHaveLength(1);

    b.goto("/pricing");
    b.window.history.pushState({}, "", "/pricing");
    b.window.history.replaceState({}, "", "/pricing");
    await tick();
    expect(b.sent).toHaveLength(2);
    expect(b.sent[1].body.u).toBe("https://example.com/pricing");

    // Same URL again, by every route the tracker listens on.
    b.window.history.pushState({}, "", "/pricing");
    b.fire("w:popstate");
    b.fire("w:hashchange");
    await tick();
    expect(b.sent).toHaveLength(2);

    b.goto("/docs/y");
    b.fire("w:popstate");
    await tick();
    expect(b.sent).toHaveLength(3);
    expect(b.sent[2].body.u).toBe("https://example.com/docs/y");
  });

  it("collapses a burst of history calls into the last URL", async () => {
    const b = stubBrowser();
    start({ autoPageview: false });

    b.goto("/a");
    b.window.history.replaceState({}, "", "/a");
    b.goto("/b");
    b.window.history.pushState({}, "", "/b");
    await tick();

    expect(b.sent).toHaveLength(1);
    expect(b.sent[0].body.u).toBe("https://example.com/b");
  });

  it("leaves history alone when the SPA listener is off", () => {
    const b = stubBrowser();
    const original = b.window.history.pushState;
    start({ spa: false });
    expect(b.window.history.pushState).toBe(original);
  });

  it("skips excluded paths, globs and all", () => {
    const b = stubBrowser({ href: "https://example.com/admin/users" });
    start({ exclude: ["/admin/**", "/preview/*"] });
    expect(b.sent).toHaveLength(0);
  });
});

describe("opt-out precedence", () => {
  it("honours the stored opt-out above every other setting", () => {
    const b = stubBrowser({ optedOut: true });
    const mf = start({ debug: true });
    mf.track("signup");
    expect(b.sent).toHaveLength(0);
    expect(mf.isOptedOut()).toBe(true);
  });

  it("opts back in, and out again, through storage", () => {
    const b = stubBrowser({ optedOut: true });
    const mf = start({ autoPageview: false });

    mf.optIn();
    expect(mf.isOptedOut()).toBe(false);
    mf.pageview();
    expect(b.sent).toHaveLength(1);

    mf.optOut();
    expect(b.storage["mf-opt-out"]).toBe("1");
    mf.track("after");
    expect(b.sent).toHaveLength(1);
  });

  it("honours do-not-track only when asked to", () => {
    const off = stubBrowser({ dnt: "1" });
    start();
    expect(off.sent).toHaveLength(1);
    vi.unstubAllGlobals();

    const on = stubBrowser({ dnt: "1" });
    start({ respectDnt: true });
    expect(on.sent).toHaveLength(0);
  });

  it("treats a developer's own machine as noise unless data-debug says otherwise", () => {
    const local = stubBrowser({ href: "http://localhost:3000/", hostname: "localhost" });
    start();
    expect(local.sent).toHaveLength(0);
    vi.unstubAllGlobals();

    const debugging = stubBrowser({ href: "http://localhost:3000/", hostname: "localhost" });
    start({ debug: true });
    expect(debugging.sent).toHaveLength(1);
    vi.unstubAllGlobals();

    const file = stubBrowser({ href: "file:///Users/me/site/index.html", protocol: "file:" });
    start();
    expect(file.sent).toHaveLength(0);
  });

  it("sends nothing at all without a site id", () => {
    const b = stubBrowser();
    createTracker({ site: 0, host: HOST } as TrackerConfig).track("x");
    expect(b.sent).toHaveLength(0);
  });
});

describe("outbound links and downloads", () => {
  const click = (b: ReturnType<typeof stubBrowser>, href: string) =>
    b.fire("d:click", { target: { closest: () => ({ href }) } });

  it("knows which hosts leave the site", () => {
    expect(isOutboundUrl("example.com", "https://other.example/pricing")).toBe(true);
    expect(isOutboundUrl("example.com", "https://notexample.com/x")).toBe(true);
    expect(isOutboundUrl("example.com", "https://docs.example.com/x")).toBe(false);
    expect(isOutboundUrl("docs.example.com", "https://example.com/x")).toBe(false);
    expect(isOutboundUrl("example.com", "https://www.example.com/x")).toBe(false);
    expect(isOutboundUrl("www.example.com", "https://example.com/x")).toBe(false);
    expect(isOutboundUrl("example.com", "mailto:hi@other.com")).toBe(false);
    expect(isOutboundUrl("example.com", "tel:+351000000")).toBe(false);
    expect(isOutboundUrl("example.com", "javascript:void 0")).toBe(false);
    expect(isOutboundUrl("example.com", "not a url")).toBe(false);
  });

  it("reports an outbound click with the destination as the event name", () => {
    const b = stubBrowser();
    start({ autoPageview: false });
    click(b, "https://other.example/pricing");

    expect(b.sent[0].body.t).toBe("outbound");
    expect(b.sent[0].body.n).toBe("https://other.example/pricing");
  });

  it("stays quiet on internal links, anchors and unknown schemes", () => {
    const b = stubBrowser();
    start({ autoPageview: false });
    click(b, "https://docs.example.com/guide");
    click(b, "#section");
    click(b, "mailto:hi@example.com");
    expect(b.sent).toHaveLength(0);
  });

  it("reports downloads by extension, internal or not", () => {
    const b = stubBrowser();
    start({ autoPageview: false });
    click(b, "https://example.com/files/report.PDF?v=2");

    expect(b.sent[0].body.t).toBe("file_download");
    expect(hasDownloadExtension("/a/b.zip", ["zip"])).toBe(true);
    expect(hasDownloadExtension("/a/b.zip?x=1", ["zip"])).toBe(true);
    expect(hasDownloadExtension("/a.zip/b", ["zip"])).toBe(false);
    expect(hasDownloadExtension("/a/b.html", ["zip"])).toBe(false);
  });

  it("takes a custom extension list", () => {
    const b = stubBrowser();
    start({ autoPageview: false, downloadExtensions: ["dmg"] });
    click(b, "https://example.com/app.pdf");
    expect(b.sent).toHaveLength(0);
    click(b, "https://example.com/app.dmg");
    expect(b.sent[0].body.t).toBe("file_download");
  });

  it("leaves form submissions to the forms plugin, and loads it only when asked", () => {
    const off = stubBrowser();
    start({ autoPageview: false });
    off.fire("d:submit", { target: { id: "signup" } });
    expect(off.sent).toHaveLength(0);
    expect(off.scripts).toEqual([]);
    vi.unstubAllGlobals();

    const on = stubBrowser();
    start({ autoPageview: false, forms: true });
    expect(on.scripts).toEqual([ENDPOINT + "/p/forms.js"]);
  });
});

describe("plugins", () => {
  it("loads web vitals by default and every other plugin only when turned on", () => {
    const b = stubBrowser();
    createTracker({ site: 1, host: HOST } as TrackerConfig);
    expect(b.scripts).toEqual([ENDPOINT + "/p/vitals.js"]);
    vi.unstubAllGlobals();

    const all = stubBrowser();
    createTracker({
      site: 1,
      host: HOST + "/",
      vitals: "false",
      errors: "",
      frustration: true,
      forms: "1",
      track404: "",
      tags: "on",
    } as TrackerConfig);
    expect(all.scripts).toEqual(
      ["errors", "frustration", "forms", "track404", "tags"].map((k) => ENDPOINT + "/p/" + k + ".js"),
    );
  });

  it("loads nothing for a browser that would not report", () => {
    const b = stubBrowser({ optedOut: true });
    createTracker({ site: 1, host: HOST, errors: true } as TrackerConfig);
    expect(b.scripts).toEqual([]);
  });

  it("asks the revenue route for a checkout reference, and never rejects", async () => {
    const b = stubBrowser();
    const asked: string[] = [];
    b.window.fetch = (url: string) => {
      asked.push(url);
      return Promise.resolve({ json: () => Promise.resolve({ data: { ref: "mfr_abc" } }) });
    };
    const mf = start({ autoPageview: false });
    await expect(mf.checkoutRef()).resolves.toBe("mfr_abc");
    expect(asked).toEqual([HOST + "/api/revenue/ref?s=1"]);

    // An error envelope, a body that is not JSON, and a network failure all
    // resolve to nothing.
    b.window.fetch = () => Promise.resolve({ json: () => Promise.resolve({ error: { code: "not_found" } }) });
    await expect(mf.checkoutRef()).resolves.toBeUndefined();
    b.window.fetch = () => Promise.resolve({ json: () => Promise.reject(new Error("not json")) });
    await expect(mf.checkoutRef()).resolves.toBeUndefined();
    b.window.fetch = () => Promise.reject(new Error("offline"));
    await expect(mf.checkoutRef()).resolves.toBeUndefined();
  });

  it("leaves itself on __mf for the plugin it loads", () => {
    stubBrowser();
    const mf = start({ errors: true });
    expect((globalThis as any).__mf).toBe(mf);
  });

  it("sends a plugin's event through every gate a pageview passes, raw or merged", () => {
    const b = stubBrowser();
    const mf = start({ autoPageview: false, props: { theme: "dark" }, mask: ["/docs/*"] });
    mf.x("error", { name: "TypeError", props: { message: "x" }, raw: true });
    mf.x("form", { name: "signup" });

    expect(b.sent[0].body).toMatchObject({ t: "error", n: "TypeError", p: { message: "x" } });
    expect(b.sent[0].body.u).toBe("https://example.com/docs/*?a=1");
    expect(b.sent[1].body).toMatchObject({ t: "form", n: "signup", p: { theme: "dark" } });

    mf.optOut();
    mf.x("error", { name: "TypeError", raw: true });
    expect(b.sent).toHaveLength(2);
  });

  it("gives a plugin the moment the page hides, and flush() too", () => {
    const b = stubBrowser();
    const mf = start({ autoPageview: false });
    const calls: string[] = [];
    mf.h = () => calls.push("hidden");
    b.document.visibilityState = "hidden";
    b.fire("d:visibilitychange");
    mf.flush();
    expect(calls).toEqual(["hidden", "hidden"]);
  });
});

describe("sampling", () => {
  it("buckets the same seed the same way every time, across the whole range", () => {
    expect(bucket("Thu Oct 08 2026Mozilla")).toBe(bucket("Thu Oct 08 2026Mozilla"));
    const seen = new Set<number>();
    for (let i = 0; i < 2000; i++) {
      const b = bucket("seed-" + i);
      expect(b).toBeGreaterThanOrEqual(0);
      expect(b).toBeLessThan(100);
      seen.add(b);
    }
    // FNV-1a spreads short, similar strings over every bucket.
    expect(seen.size).toBe(100);
  });

  it("keeps everyone without a rate, nobody at 0, and about the rate between", () => {
    expect(inSample(NaN, "x")).toBe(true);
    expect(inSample(100, "x")).toBe(true);
    expect(inSample(0, "x")).toBe(false);
    let kept = 0;
    for (let i = 0; i < 10000; i++) if (inSample(25, "visitor-" + i)) kept++;
    expect(kept).toBeGreaterThan(2300);
    expect(kept).toBeLessThan(2700);
  });

  it("decides once per visitor and day, the same way on every page", () => {
    const decide = (ua: string) => {
      const b = stubBrowser();
      b.window.navigator.userAgent = ua;
      start({ sample: "50" });
      vi.unstubAllGlobals();
      return b.sent.length;
    };
    // Same browser, same day: the same answer on every page load.
    for (let i = 0; i < 20; i++) {
      const ua = "Mozilla/5.0 browser-" + i;
      expect(decide(ua)).toBe(decide(ua));
    }
  });

  it("sends the rate with every hit of a sampled visitor, and nothing at 0%", () => {
    const b = stubBrowser();
    // Find a user agent that is in a 50% sample today, the way the tracker draws.
    let ua = "";
    for (let i = 0; !ua; i++) {
      const candidate = "Mozilla/5.0 candidate-" + i;
      if (bucket(Date().slice(0, 15) + candidate) < 50) ua = candidate;
    }
    b.window.navigator.userAgent = ua;
    start({ sample: 50 });
    expect(b.sent).toHaveLength(1);
    expect(b.sent[0].body.sr).toBe(50);
    vi.unstubAllGlobals();

    const none = stubBrowser();
    start({ sample: "0" });
    expect(none.sent).toHaveLength(0);
    vi.unstubAllGlobals();

    const all = stubBrowser();
    start({ sample: "100" });
    expect(all.sent).toHaveLength(1);
    expect(all.sent[0].body).not.toHaveProperty("sr");
  });
});

describe("masking", () => {
  it("replaces a matching path with its glob and leaves the rest alone", () => {
    const c = { mask: ["/users/*", "/orders/*/items", "/docs/**"] };
    expect(maskUrl("https://example.com/users/123", c)).toBe("https://example.com/users/*");
    expect(maskUrl("https://example.com/users/123?tab=2#h", c)).toBe("https://example.com/users/*?tab=2#h");
    expect(maskUrl("https://example.com/orders/9/items", c)).toBe("https://example.com/orders/*/items");
    expect(maskUrl("https://example.com/docs/a/b/c", c)).toBe("https://example.com/docs/**");
    // One star is one segment, as in data-exclude.
    expect(maskUrl("https://example.com/users/1/edit", c)).toBe("https://example.com/users/1/edit");
    expect(maskUrl("https://example.com/pricing", c)).toBe("https://example.com/pricing");
    expect(maskUrl("", c)).toBe("");
  });

  it("strips the whole query string, or only the parameters named", () => {
    const url = "https://example.com/reset?token=abc&email=a%40b.c&utm_source=x";
    expect(maskUrl(url, { stripQuery: true })).toBe("https://example.com/reset");
    expect(maskUrl(url, { stripQuery: ["token", "email"] })).toBe("https://example.com/reset?utm_source=x");
    expect(maskUrl(url, {})).toBe(url);
  });

  it("masks the page and a same-site referrer before either leaves the page", () => {
    const b = stubBrowser({
      href: "https://example.com/users/42?token=s3cret",
      referrer: "https://example.com/users/41?token=old",
    });
    start({ mask: ["/users/*"], stripQuery: ["token"] });
    expect(b.sent[0].body.u).toBe("https://example.com/users/*");
    expect(b.sent[0].body.r).toBe("https://example.com/users/*");
    expect(JSON.stringify(b.sent[0].body)).not.toContain("42");
    expect(JSON.stringify(b.sent[0].body)).not.toContain("s3cret");
  });
});

describe("identify and traits", () => {
  it("sends one identify event per change, carrying the traits and nothing else", () => {
    const b = stubBrowser();
    const mf = start({ autoPageview: false, props: { theme: "dark" } });
    mf.identify("u_1", { email: "a@example.com" });
    mf.identify("u_1", { email: "a@example.com" }); // a router effect, again
    mf.setTraits({ plan: "pro" });
    mf.setTraits({ plan: "pro" });
    mf.identify("u_2");

    const ids = b.sent.filter((hit) => hit.body.t === "identify").map((hit) => hit.body);
    expect(ids).toHaveLength(3);
    expect(ids[0]).toMatchObject({ id: "u_1", p: { email: "a@example.com" } });
    expect(ids[0].p).not.toHaveProperty("theme");
    expect(ids[1]).toMatchObject({ id: "u_1", p: { plan: "pro" } });
    expect(ids[2].id).toBe("u_2");
    expect(ids[2]).not.toHaveProperty("p");
  });

  it("keeps identify's traits as default properties, and setTraits out of them", () => {
    const b = stubBrowser();
    const mf = start({ autoPageview: false });
    mf.identify("u_1", { plan: "pro" });
    mf.setTraits({ email: "a@example.com" });
    mf.track("signup");
    const custom = b.sent.find((hit) => hit.body.t === "custom")!.body;
    expect(custom.p).toEqual({ plan: "pro" });
  });

  it("sends nothing when an id is cleared", () => {
    const b = stubBrowser();
    const mf = start({ autoPageview: false });
    mf.identify(null);
    expect(b.sent).toHaveLength(0);
  });
});

describe("automation bits", () => {
  const bits = (setup: (b: ReturnType<typeof stubBrowser>) => void) => {
    const b = stubBrowser();
    setup(b);
    start();
    return b.sent[0].body.a;
  };

  it("sends no `a` for a plain browser", () => {
    expect(bits(() => {})).toBeUndefined();
    expect(bits((b) => (b.window.navigator.webdriver = false))).toBeUndefined();
    expect(
      bits((b) => (b.window.navigator.userAgentData = { brands: [{ brand: "Google Chrome", version: "128" }] })),
    ).toBeUndefined();
  });

  it("sets 1 for navigator.webdriver", () => {
    expect(bits((b) => (b.window.navigator.webdriver = true))).toBe(1);
  });

  it("sets 2 for CDP-driver globals", () => {
    expect(bits((b) => (b.document.$cdc_asdjflasutopfhvcZLmcfl_ = {}))).toBe(2);
    expect(bits((b) => (b.window.cdc_adoQpoasnfa76pfcZLmcfl_Array = []))).toBe(2);
    expect(bits((b) => (b.window.__playwright__binding__ = 1))).toBe(2);
    expect(bits((b) => (b.window.__pw_manual = 1))).toBe(2);
    expect(bits((b) => (b.window.__PW_inspect = 1))).toBe(2);
    expect(bits((b) => (b.window.__puppeteer_evaluation_script__ = 1))).toBe(2);
    expect(bits((b) => (b.window.abcdc_x = 1))).toBeUndefined();
  });

  it("sets 4 for Selenium, Phantom and Nightmare globals", () => {
    for (const k of ["_phantom", "callPhantom", "__nightmare", "domAutomation", "domAutomationController"])
      expect(bits((b) => (b.window[k] = 1))).toBe(4);
    for (const k of ["_Selenium_IDE_Recorder", "__webdriverFunc", "__driver_evaluate", "__fxdriver_unwrapped"])
      expect(bits((b) => (b.window[k] = 1))).toBe(4);
    for (const k of ["__webdriver_script_fn", "__selenium_unwrapped", "__driver-evaluate"])
      expect(bits((b) => (b.document[k] = 1))).toBe(4);
  });

  it("sets 8 for a HeadlessChrome brand, and combines bits", () => {
    expect(
      bits((b) => (b.window.navigator.userAgentData = { brands: [{ brand: "HeadlessChrome", version: "128" }] })),
    ).toBe(8);
    expect(
      bits((b) => {
        b.window.navigator.webdriver = true;
        b.document.$cdc_x = 1;
      }),
    ).toBe(3);
  });

  it("never throws on a hostile navigator", () => {
    const b = stubBrowser();
    Object.defineProperty(b.window.navigator, "userAgentData", {
      get() {
        throw new Error("no");
      },
    });
    start();
    expect(b.sent[0].body.a).toBeUndefined();
    expect(b.sent[0].body.t).toBe("pageview");
  });
});

describe("transport", () => {
  it("falls back from sendBeacon to fetch", () => {
    const beacon = stubBrowser();
    start();
    expect(beacon.sent[0].via).toBe("beacon");
    vi.unstubAllGlobals();

    const full = stubBrowser({ beacon: "full" });
    start();
    expect(full.sent[0].via).toBe("fetch");
    vi.unstubAllGlobals();

    // No pixel fallback: a browser that parses the bundle has fetch.
    const none = stubBrowser({ beacon: "missing", fetch: "missing" });
    expect(() => start()).not.toThrow();
    expect(none.sent).toHaveLength(0);
  });

  it("swallows a transport that fails at every level", () => {
    const b = stubBrowser({ beacon: "throw", fetch: "throw", image: "throw" });
    const mf = start({ autoPageview: false });

    expect(() => mf.track("signup", { plan: "pro" })).not.toThrow();
    expect(() => mf.pageview()).not.toThrow();
    expect(() => mf.flush()).not.toThrow();
    expect(b.sent).toHaveLength(0);
  });

  it("swallows a browser that has lost its storage", () => {
    const b = stubBrowser();
    Object.defineProperty(b.window, "localStorage", {
      get() {
        throw new Error("storage blocked");
      },
    });
    const mf = start({ autoPageview: false });

    expect(() => mf.optOut()).not.toThrow();
    expect(() => mf.optIn()).not.toThrow();
    expect(mf.isOptedOut()).toBe(false);
    mf.track("still works");
    expect(b.sent).toHaveLength(1);
  });

  it("swallows a broken listener rather than throwing into the page", () => {
    const b = stubBrowser();
    start({ autoPageview: false });
    expect(() => b.fire("d:click", {})).not.toThrow();
    expect(() => b.fire("d:click", { target: {} })).not.toThrow();
    expect(() => b.fire("w:pagehide")).not.toThrow();
  });
});

describe("script tag configuration", () => {
  const el = (attrs: Record<string, string>, src = HOST + "/mf.js") => ({
    src,
    getAttribute: (k: string) => (k in attrs ? attrs[k] : null),
  });

  it("reads every documented data-* attribute", () => {
    const cfg = readScriptConfig(
      el({
        "data-site": "7",
        "data-auto-pageview": "false",
        "data-spa": "auto",
        "data-outbound": "false",
        "data-downloads": "pdf,.DMG",
        "data-forms": "",
        "data-404": "1",
        "data-hash": "true",
        "data-exclude": "/admin/**, /preview/*",
        "data-respect-dnt": "",
        "data-vitals": "off",
        "data-scroll": "no",
        "data-engagement": "0",
        "data-debug": "true",
        "data-sample": "25",
        "data-mask": "/users/*, /orders/*",
        "data-strip-query": "token,email",
        "data-errors": "",
        "data-frustration": "",
        "data-tags": "",
      }),
      "https://example.com",
    )!;

    expect(cfg.site).toBe(7);
    expect(cfg.host).toBe(HOST);
    expect(cfg.exclude).toEqual(["/admin/**", "/preview/*"]);
    expect(cfg.downloadExtensions).toEqual(["pdf", "dmg"]);
    expect(flagOn(cfg.autoPageview, true)).toBe(false);
    expect(flagOn(cfg.spa, true)).toBe(true);
    expect(flagOn(cfg.forms, false)).toBe(true);
    expect(flagOn(cfg.track404, false)).toBe(true);
    expect(flagOn(cfg.vitals, true)).toBe(false);
    expect(flagOn(cfg.scroll, true)).toBe(false);
    expect(flagOn(cfg.engagement, true)).toBe(false);
    expect(flagOn(cfg.debug, false)).toBe(true);
    expect(cfg.sample).toBe("25");
    expect(cfg.mask).toEqual(["/users/*", "/orders/*"]);
    expect(cfg.stripQuery).toEqual(["token", "email"]);
    expect(flagOn(cfg.errors, false)).toBe(true);
    expect(flagOn(cfg.frustration, false)).toBe(true);
    expect(flagOn(cfg.tags, false)).toBe(true);
  });

  it("reads data-strip-query alone as the whole query string", () => {
    expect(readScriptConfig(el({ "data-site": "1", "data-strip-query": "" }), "")!.stripQuery).toBe(true);
    expect(readScriptConfig(el({ "data-site": "1", "data-strip-query": "true" }), "")!.stripQuery).toBe(true);
    expect(readScriptConfig(el({ "data-site": "1" }), "")!.stripQuery).toBeUndefined();
    expect(readScriptConfig(el({ "data-site": "1" }), "")!.mask).toEqual([]);
  });

  it("defaults the ingest origin to the script's own origin", () => {
    const cfg = readScriptConfig(el({ "data-site": "1" }), "https://fallback.example")!;
    expect(cfg.host).toBe(HOST);
    expect(readScriptConfig(el({ "data-site": "1" }, ""), "https://fallback.example")!.host).toBe(
      "https://fallback.example",
    );
  });

  it("refuses to configure itself without a numeric site", () => {
    expect(readScriptConfig(el({}), "https://example.com")).toBe(null);
    expect(readScriptConfig(el({ "data-site": "abc" }), "https://example.com")).toBe(null);
  });
});

describe("path globs", () => {
  it("matches one segment with * and any depth with **", () => {
    expect(globToRegExp("/admin/*").test("/admin/users")).toBe(true);
    expect(globToRegExp("/admin/*").test("/admin/users/7")).toBe(false);
    expect(globToRegExp("/admin/**").test("/admin/users/7")).toBe(true);
    expect(globToRegExp("/p?ge").test("/page")).toBe(true);
    expect(globToRegExp("/a.b").test("/axb")).toBe(false);
    expect(isExcluded("/admin/x", ["/admin/**"])).toBe(true);
    expect(isExcluded("/docs/x", ["/admin/**"])).toBe(false);
    expect(isExcluded("/docs/x", undefined)).toBe(false);
  });
});
