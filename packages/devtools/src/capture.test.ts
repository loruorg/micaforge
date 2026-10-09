/**
 * The capture, driven by the real tracker (`createTracker` from the SDK's source) on a
 * jsdom page at https://example.com: what it records when the tracker sends, and what it
 * records, and why, when the tracker refuses.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createFlags, createTracker } from "@micaforge/sdk";
import { installCapture, payloadFromPixel, routeOf, type Capture } from "./capture.js";
import { Store } from "./store.js";

const G: any = globalThis;
const HOST = "https://mf.example.com";
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

let beacons: { url: string; body: unknown }[];
let capture: Capture | undefined;

beforeEach(() => {
  beacons = [];
  Object.defineProperty(window.navigator, "sendBeacon", {
    configurable: true,
    writable: true,
    value: vi.fn((url: string, body: unknown) => {
      beacons.push({ url, body });
      return true;
    }),
  });
  // A tracker from an earlier test patched history; take its patch away so only this
  // test's tracker follows navigation.
  delete (history as any).pushState;
  delete (history as any).replaceState;
  window.history.replaceState(null, "", "/docs/start?ref=x");
  localStorage.clear();
});

afterEach(() => {
  capture?.uninstall();
  capture = undefined;
  delete G.__mf;
  delete G.micaforge;
  delete G.__micaforge__;
});

/**
 * Start the overlay, then a tracker, the way an npm app would: the overlay is handed the
 * same settings the tracker was (`mountDevtools({ tracker })`).
 */
const start = (store: Store, config: Record<string, any> = {}) => {
  const override: Record<string, unknown> = { site: 1, host: HOST };
  for (const k of ["mask", "exclude", "stripQuery"]) if (config[k] !== undefined) override[k] = config[k];
  if (config.sample !== undefined) override.sample = parseFloat(config.sample);
  capture = installCapture(window, store, undefined, override);
  return createTracker({ site: 1, host: HOST, vitals: false, ...config } as any);
};

describe("what the tracker sends", () => {
  it("records the first pageview and later custom events, with the payload as sent", async () => {
    const store = new Store();
    const tracker = start(store);
    tracker.track("signup", { plan: "pro" });
    expect(beacons).toHaveLength(2);
    expect(store.events.map((e) => [e.kind, e.status, e.via])).toEqual([
      ["pageview", "sent", "beacon"],
      ["custom", "sent", "beacon"],
    ]);
    const custom = store.events[1];
    expect(custom.name).toBe("signup");
    expect(custom.props).toEqual({ plan: "pro" });
    expect(custom.url).toBe("https://example.com/docs/start?ref=x");
    expect(custom.payload).toMatchObject({ s: 1, t: "custom", n: "signup", v: expect.any(String) });
    expect(custom.warnings).toEqual([]);
    expect(custom.sincePageStart).toBeGreaterThanOrEqual(0);
  });

  it("follows client-side navigation the tracker reports", async () => {
    const store = new Store();
    start(store);
    history.pushState(null, "", "/pricing");
    await wait(50);
    expect(store.events.map((e) => e.url)).toEqual(["https://example.com/docs/start?ref=x", "https://example.com/pricing"]);
  });

  it("says when an address was masked or stripped", () => {
    window.history.replaceState(null, "", "/users/42?token=abc&tab=2");
    const store = new Store();
    start(store, { mask: ["/users/*"], stripQuery: ["token"] });
    const page = store.events[0];
    expect(page.url).toBe("https://example.com/users/*?tab=2");
    expect(page.rewritten).toMatchObject({ from: "https://example.com/users/42?token=abc&tab=2", why: 'masked by data-mask "/users/*"' });
  });

  it("records identify and keeps the identity", () => {
    const store = new Store();
    const tracker = start(store);
    tracker.identify("u_1", { plan: "pro" });
    expect(store.identity).toMatchObject({ id: "u_1", traits: { plan: "pro" } });
    expect(store.events[store.events.length - 1]).toMatchObject({ kind: "identify", status: "sent" });
  });

  it("reads the fetch fallback and marks a failed request", async () => {
    Object.defineProperty(window.navigator, "sendBeacon", { configurable: true, writable: true, value: () => false });
    const fetchMock = vi.fn(() => Promise.reject(new TypeError("offline")));
    vi.stubGlobal("fetch", fetchMock);
    const store = new Store();
    start(store);
    expect(fetchMock).toHaveBeenCalled();
    await wait(10);
    expect(store.events[0]).toMatchObject({ kind: "pageview", via: "fetch", status: "failed" });
  });

  it("reads the pixel fallback", () => {
    Object.defineProperty(window.navigator, "sendBeacon", { configurable: true, writable: true, value: () => false });
    vi.stubGlobal("fetch", undefined);
    const store = new Store();
    start(store);
    expect(store.events[0]).toMatchObject({ kind: "pageview", via: "pixel", status: "sent" });
    expect(store.events[0].payload).toMatchObject({ s: 1, t: "pageview" });
  });
});

describe("what the tracker refuses, and why", () => {
  it("names the opt-out", async () => {
    localStorage.setItem("mf-opt-out", "1");
    const store = new Store();
    const tracker = start(store);
    // No wait: the overlay hooks the tracker the moment it publishes itself.
    tracker.track("signup");
    expect(beacons).toHaveLength(0);
    const refused = store.events.find((e) => e.kind == "custom");
    expect(refused).toMatchObject({ status: "blocked", name: "signup" });
    expect(refused?.reason).toMatch(/opted out/);
  });

  it("names the sample, the excluded path and localhost", async () => {
    const store = new Store();
    const tracker = start(store, { sample: "0" });
    tracker.track("x");
    expect(store.events[store.events.length - 1]?.reason).toMatch(/outside today's 0% sample/);

    capture?.uninstall();
    delete G.__mf;
    window.history.replaceState(null, "", "/admin/users");
    const store2 = new Store();
    const tracker2 = start(store2, { exclude: ["/admin/**"] });
    tracker2.track("y");
    expect(store2.events[store2.events.length - 1]?.reason).toMatch(/matches data-exclude/);
  });

  it("records a refused pageview on navigation when the tracker would have sent one", async () => {
    localStorage.setItem("mf-opt-out", "1");
    document.body.innerHTML = `<script data-site="1" data-host="${HOST}"></script>`;
    const store = new Store();
    capture = installCapture(window, store);
    expect(store.events[0]).toMatchObject({ kind: "pageview", status: "blocked" });
    history.pushState(null, "", "/next");
    await wait(60);
    expect(store.events[store.events.length - 1]).toMatchObject({ kind: "pageview", status: "blocked", url: "https://example.com/next" });
    document.body.innerHTML = "";
  });

  it("explains an identify the tracker skipped because nothing changed", async () => {
    const store = new Store();
    const tracker = start(store);
    tracker.identify("u_1");
    tracker.identify("u_1");
    expect(store.events[store.events.length - 1]).toMatchObject({ kind: "identify", status: "blocked" });
    expect(store.events[store.events.length - 1]?.reason).toMatch(/sends a change once/);
  });
});

describe("flags", () => {
  it("reads the server's answers, reasons included, and the SDK's counts", async () => {
    const answer = {
      data: { site: 1, flags: { "new-nav": { enabled: true, variant: "b", reason: "match" }, beta: { enabled: false, variant: null, reason: "rollout" } } },
    };
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify(answer), { status: 200, headers: { "content-type": "application/json" } })));
    const store = new Store();
    capture = installCapture(window, store);
    const flags = createFlags({ site: 1, host: HOST, visitor: "v1" });
    await flags.load();
    await wait(10);
    expect(store.flags.get("new-nav")).toMatchObject({ enabled: true, variant: "b", reason: "match" });
    expect(store.flags.get("beta")?.explanation).toMatch(/outside the rollout/);

    flags.isEnabled("new-nav");
    flags.flushStats();
    await wait(10);
    expect(store.flags.get("new-nav")?.exposures).toBe(1);
    expect(store.flags.get("new-nav")?.evaluations).toBeGreaterThan(0);
  });
});

describe("hooks", () => {
  it("restore the page's globals on uninstall, and stay inert under a later wrapper", () => {
    const beacon = navigator.sendBeacon;
    const store = new Store();
    capture = installCapture(window, store);
    expect(navigator.sendBeacon).not.toBe(beacon);
    capture.uninstall();
    capture = undefined;
    expect(navigator.sendBeacon).toBe(beacon);
  });

  it("recognise every collection route and the pixel encoding", () => {
    const base = "https://example.com/";
    expect(routeOf("https://mf.example.com/api/track", base)).toBe("track");
    expect(routeOf("/api/track/batch", base)).toBe("batch");
    expect(routeOf("https://mf.example.com/api/track/pixel.gif?s=1", base)).toBe("pixel");
    expect(routeOf("/api/flags/evaluate?site=1", base)).toBe("flags");
    expect(routeOf("/api/flags/events", base)).toBe("flag-events");
    expect(routeOf("/api/tracking", base)).toBeNull();
    expect(payloadFromPixel("/api/track/pixel.gif?s=1&t=custom&n=x&p=%7B%22a%22%3A1%7D", base)).toEqual({ s: 1, t: "custom", n: "x", p: { a: 1 } });
  });
});
