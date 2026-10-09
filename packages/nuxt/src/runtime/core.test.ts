/**
 * The runtime logic against the real tracker: the tag's attributes, calls made before the
 * tag has run, router pageviews without double counting, and the flag binding.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ref } from "vue";
import { createFlags, type FlagRecord } from "@micaforge/sdk";
import { bindFlag, connectFlag, createApi, normaliseBootstrap, normalisePlugins, trackerScript, trackRoutes, type PublicConfig, type RouterLike } from "./core.js";

const G: any = globalThis;
const HOST = "https://mf.example.com";

const config = (over: Partial<PublicConfig> = {}): PublicConfig => ({
  siteKey: "1",
  host: HOST,
  scriptSrc: "",
  enabled: true,
  routeTracking: true,
  plugins: {},
  tracker: {},
  flags: {},
  ...over,
});

describe("the tracker tag", () => {
  it("carries the site, the host, and switches the tracker's history listener off", () => {
    expect(trackerScript(config())).toEqual({
      key: "micaforge",
      src: HOST + "/mf.js",
      defer: true,
      "data-site": "1",
      "data-host": HOST,
      "data-spa": "false",
    });
    expect(trackerScript(config({ routeTracking: false }))).not.toHaveProperty("data-spa");
  });

  it("turns plugins on and vitals off by the tracker's attribute names", () => {
    const script = trackerScript(config({ plugins: normalisePlugins(["errors", "track404", "nonsense" as never]) }));
    expect(script).toMatchObject({ "data-errors": "true", "data-404": "true" });
    expect(script).not.toHaveProperty("data-vitals");
    expect(trackerScript(config({ plugins: normalisePlugins({ vitals: false, tags: true }) }))).toMatchObject({ "data-vitals": "false", "data-tags": "true" });
  });

  it("writes the tracker's other settings", () => {
    const script = trackerScript(
      config({
        scriptSrc: "/stats/mf.js",
        tracker: { exclude: ["/admin/**"], mask: ["/users/*", "/orders/*"], stripQuery: ["token"], sample: 25, respectDnt: true, outbound: false, release: "v42" },
      }),
    );
    expect(script).toMatchObject({
      src: "/stats/mf.js",
      "data-exclude": "/admin/**",
      "data-mask": "/users/*,/orders/*",
      "data-strip-query": "token",
      "data-sample": "25",
      "data-respect-dnt": "true",
      "data-outbound": "false",
      "data-release": "v42",
    });
    expect(trackerScript(config({ tracker: { stripQuery: true } }))).toMatchObject({ "data-strip-query": "true" });
  });

  it("is left out when the module is off or the site or host is missing", () => {
    expect(trackerScript(config({ enabled: false }))).toBeNull();
    expect(trackerScript(config({ siteKey: "" }))).toBeNull();
    expect(trackerScript(config({ siteKey: "abc" }))).toBeNull();
    expect(trackerScript(config({ host: "" }))).toBeNull();
  });
});

describe("calls and routes, with the real tracker", () => {
  let beacons: any[];

  beforeEach(() => {
    beacons = [];
    Object.defineProperty(window.navigator, "sendBeacon", {
      configurable: true,
      writable: true,
      value: (_url: string, body: string) => {
        beacons.push(JSON.parse(body));
        return true;
      },
    });
  });

  afterEach(() => {
    delete G.micaforge;
    delete G.__mf;
  });

  it("queues calls made before the tag runs, then reports each route change once", async () => {
    const api = createApi(() => window);
    api.track("early", { plan: "pro" });
    api.identify("u_1");
    expect(typeof G.micaforge).toBe("function");
    expect(G.micaforge.q).toHaveLength(2);

    // The tag, as the plugin renders it, then the real tracker entry.
    const script = document.createElement("script");
    for (const [k, v] of Object.entries(trackerScript(config()) as Record<string, string>)) {
      if (k.startsWith("data-")) script.setAttribute(k, v);
    }
    document.head.append(script);
    await import("../../../sdk/src/tracker/index.js");
    expect(G.micaforge.mf).toBe(1);
    expect(beacons.map((b) => [b.t, b.n || b.u])).toEqual([
      ["pageview", "https://example.com/"],
      ["custom", "early"],
      ["identify", "https://example.com/"],
    ]);

    // The router: the tracker no longer follows history itself (data-spa="false").
    let guard: (to: unknown, from: unknown, failure?: unknown) => unknown = () => {};
    const router: RouterLike = { afterEach: (g) => ((guard = g), () => {}) };
    trackRoutes(router, window, false);
    history.pushState(null, "", "/pricing");
    await new Promise((r) => setTimeout(r, 10));
    expect(beacons).toHaveLength(3); // the tracker did not count the pushState
    guard({}, {});
    expect(beacons[3]).toMatchObject({ t: "pageview", u: "https://example.com/pricing" });
    guard({}, {}); // the same address again
    history.pushState(null, "", "/pricing#faq");
    guard({}, {}); // only the hash changed
    history.pushState(null, "", "/docs");
    guard({}, {}, new Error("aborted")); // a failed navigation
    expect(beacons).toHaveLength(4);
    guard({}, {});
    expect(beacons[4]).toMatchObject({ t: "pageview", u: "https://example.com/docs" });

    expect(api.isOptedOut()).toBe(false);
    api.optOut();
    expect(api.isOptedOut()).toBe(true);
    api.optIn();
    script.remove();
  });

  it("sends nothing from the router before the tracker has started", () => {
    let guard: (to: unknown, from: unknown, failure?: unknown) => unknown = () => {};
    trackRoutes({ afterEach: (g) => ((guard = g), () => {}) }, window, false);
    history.pushState(null, "", "/early");
    guard({}, {});
    expect(beacons).toHaveLength(0);
  });

  it("does nothing during server rendering", () => {
    const api = createApi(() => undefined);
    expect(() => api.track("x")).not.toThrow();
    expect(api.isOptedOut()).toBe(false);
  });
});

describe("useFlag's binding", () => {
  it("starts from the bootstrap, then takes the client's answer once the app is ready", async () => {
    const state = ref<Record<string, FlagRecord>>(normaliseBootstrap({ "new-nav": "control", beta: false }));
    const binding = bindFlag(state, "new-nav", false);
    expect(binding.enabled.value).toBe(true);
    expect(binding.variant.value).toBe("control");
    expect(bindFlag(state, "missing", true).enabled.value).toBe(true);

    const fetchImpl = vi.fn(async (_url: string) =>
      new Response(JSON.stringify({ data: { flags: { "new-nav": { enabled: true, variant: "treatment", reason: "match" } } } }), { status: 200 }),
    );
    const client = createFlags({ site: 1, host: HOST, visitor: "v1", fetchImpl: fetchImpl as unknown as typeof fetch, stats: false });
    let ready: () => void = () => {};
    connectFlag(client, state, "new-nav", (fn) => (ready = fn));
    expect(fetchImpl).not.toHaveBeenCalled(); // nothing before the app is ready: hydration sees the server's value
    ready();
    await new Promise((r) => setTimeout(r, 10));
    expect(String(fetchImpl.mock.calls[0][0])).toContain("/api/flags/evaluate?site=1&visitor=v1");
    expect(binding.variant.value).toBe("treatment");
  });
});
