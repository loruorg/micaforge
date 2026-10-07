/**
 * Flag tests. The claim under test is "a flag never flickers": every read is
 * synchronous and deterministic before, during and after the network call.
 */
import { describe, expect, it, vi } from "vitest";
import { createFlags, hashBucket } from "./flags.js";
import type { FlagsConfig } from "./types.js";

const make = (cfg: Partial<FlagsConfig> = {}) =>
  createFlags({ site: 1, host: "https://mf.example.com", visitor: "u_42", ...cfg } as FlagsConfig);

describe("bucketing", () => {
  it("is deterministic and evenly spread", () => {
    expect(hashBucket("u_42:new-nav")).toBe(hashBucket("u_42:new-nav"));
    expect(hashBucket("u_42:new-nav")).not.toBe(hashBucket("u_43:new-nav"));

    const seen = new Set<number>();
    for (let i = 0; i < 2000; i++) seen.add(hashBucket("visitor-" + i + ":flag"));
    expect(seen.size).toBeGreaterThan(95);
    for (const b of seen) expect(b).toBeGreaterThanOrEqual(0), expect(b).toBeLessThan(100);
  });

  it("answers the same way every time, with no network at all", () => {
    const f = make({ fallback: { "new-nav": { rolloutPct: 100 } } });
    const first = f.isEnabled("new-nav");
    expect(f.isEnabled("new-nav")).toBe(first);
    expect(first).toBe(true);
    expect(make({ fallback: { "new-nav": { rolloutPct: 0 } } }).isEnabled("new-nav")).toBe(false);
  });

  it("splits variants deterministically", () => {
    const f = make({
      fallback: { hero: { rolloutPct: 100, variants: [{ key: "a" }, { key: "b" }] } },
    });
    expect(f.variant("hero")).toBe(f.variant("hero"));
    expect(["a", "b"]).toContain(f.variant("hero"));
  });

  it("holds an unknown flag off rather than guessing", () => {
    const f = make();
    expect(f.isEnabled("never-heard-of-it")).toBe(false);
    expect(f.variant("never-heard-of-it", "control")).toBe("control");
  });
});

describe("bootstrap and cache", () => {
  it("answers from the bootstrap payload before anything is fetched", () => {
    const f = make({ bootstrap: { "new-nav": true, hero: "b", off: false } });
    expect(f.isEnabled("new-nav")).toBe(true);
    expect(f.variant("hero")).toBe("b");
    expect(f.isEnabled("off")).toBe(false);
  });

  it("takes the server's answer over the bootstrap, and caches it", async () => {
    const fetchImpl = vi.fn(async (url: string) => {
      expect(url).toBe("https://mf.example.com/api/flags/evaluate?site=1&visitor=u_42");
      return { ok: true, json: async () => ({ data: { flags: { "new-nav": { enabled: false } } } }) };
    });
    const f = make({ bootstrap: { "new-nav": true }, fetchImpl: fetchImpl as any });

    expect(f.isEnabled("new-nav")).toBe(true);
    await f.load();
    expect(f.isEnabled("new-nav")).toBe(false);
    await f.load();
    expect(fetchImpl).toHaveBeenCalledTimes(1);

    f.clearCache();
    await f.load();
    expect(fetchImpl).toHaveBeenCalledTimes(2);
  });

  it("keeps answering when the network fails, and never rejects", async () => {
    const f = make({
      bootstrap: { "new-nav": true },
      fetchImpl: (() => Promise.reject(new Error("offline"))) as any,
    });
    await expect(f.load()).resolves.toBeTruthy();
    expect(f.isEnabled("new-nav")).toBe(true);
  });

  it("reports each exposure once", () => {
    const onExposure = vi.fn();
    const f = make({ bootstrap: { hero: "b" }, onExposure });
    f.isEnabled("hero");
    f.variant("hero");
    f.isEnabled("other");
    expect(onExposure).toHaveBeenCalledTimes(2);
    expect(onExposure.mock.calls[0][1]).toMatchObject({ key: "hero", variant: "b" });
  });

  it("notifies subscribers when the evaluated set changes", async () => {
    const seen: any[] = [];
    const f = make({
      fetchImpl: (async () => ({ ok: true, json: async () => ({ hero: "a" }) })) as any,
    });
    const stop = f.onChange((all) => seen.push(all));
    await f.load();
    stop();
    await f.load(true);
    expect(seen).toHaveLength(1);
    expect(seen[0].hero.variant).toBe("a");
  });

  it("re-evaluates when the bucketing subject changes", async () => {
    const f = make({ fallback: { hero: { rolloutPct: 50 } } });
    const before = f.bucket("hero");
    f.setVisitor("someone-else");
    expect(f.bucket("hero")).not.toBe(before);
  });
});
