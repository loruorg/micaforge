/**
 * The server-side flag client: the ruleset fetch, local decisions, and the stats batch.
 * The transport is a stubbed `fetch`, so every request the client makes is visible.
 */
import { describe, expect, it, vi } from "vitest";
import { createFlagsClient } from "./flags.js";
import { evaluateFlag, type FlagRuleset } from "./flag-eval.js";

const RULESET: FlagRuleset = {
  flags: [
    { key: "new-nav", enabled: true, rollout_pct: 50 },
    { key: "beta", enabled: false, environments: { staging: { enabled: true } } },
    { key: "pro", enabled: true, conditions: [{ dimension: "plan", op: "is", value: "pro" }] },
    { key: "hero", enabled: true, variants: [{ key: "a" }, { key: "b" }] },
  ],
};

type Call = { url: string; init?: RequestInit };

/** A fetch that answers the ruleset and records every call. */
function stub(answers: (() => Response | Promise<Response>)[] = []) {
  const calls: Call[] = [];
  const fetchImpl = vi.fn(async (url: string, init?: RequestInit) => {
    calls.push({ url, init });
    if (url.includes("/api/flags/events")) return new Response(null, { status: 204 });
    const next = answers.shift();
    if (next) return next();
    return Response.json({ data: { site: 3, version: "v1", ttl_seconds: 30, ruleset: RULESET } });
  });
  return { calls, fetchImpl: fetchImpl as unknown as typeof fetch };
}

const make = (extra: Partial<Parameters<typeof createFlagsClient>[0]> = {}) =>
  createFlagsClient({ host: "https://mf.example.com/", apiKey: "mfg_secret", site: 3, ...extra });

describe("the ruleset", () => {
  it("is fetched with the key in the header and nowhere else", async () => {
    const { calls, fetchImpl } = stub();
    const flags = make({ fetchImpl });
    await flags.ready();
    expect(calls[0].url).toBe("https://mf.example.com/api/flags/ruleset?site=3");
    expect((calls[0].init?.headers as Record<string, string>).authorization).toBe("Bearer mfg_secret");
    expect(calls[0].url).not.toContain("mfg_");
    await flags.close();
  });

  it("decides locally exactly as the shared evaluator does", async () => {
    const { fetchImpl } = stub();
    const flags = make({ fetchImpl, environment: "staging" });
    await flags.ready();
    for (let i = 0; i < 30; i++) {
      const visitor = "visitor-" + i;
      expect(flags.evaluate("new-nav", { visitor })).toEqual(
        evaluateFlag(RULESET, "new-nav", { subject: visitor, environment: "staging" }),
      );
    }
    expect(flags.isEnabled("beta", { visitor: "x" })).toBe(true);
    expect(flags.isEnabled("pro", { visitor: "x", context: { plan: "pro" } })).toBe(true);
    expect(flags.isEnabled("pro", { visitor: "x", context: { plan: "free" } })).toBe(false);
    expect(["a", "b"]).toContain(flags.variant("hero", { identity: "u_1", visitor: "ignored" }));
    // The identity buckets ahead of the visitor.
    expect(flags.evaluate("new-nav", { identity: "u_1", visitor: "v" })).toEqual(
      flags.evaluate("new-nav", { identity: "u_1", visitor: "w" }),
    );
    expect(Object.keys(flags.all({ visitor: "x" }))).toEqual(["new-nav", "beta", "pro", "hero"]);
    await flags.close();
  });

  it("answers the fallback before any ruleset and for an unknown key", async () => {
    const { fetchImpl } = stub([() => new Response("nope", { status: 503 })]);
    const onError = vi.fn();
    const flags = make({ fetchImpl, onError });
    expect(flags.isEnabled("new-nav", { visitor: "x" }, true)).toBe(true);
    expect(flags.evaluate("new-nav").reason).toBe("unknown");
    await flags.ready();
    expect(onError).toHaveBeenCalledTimes(1);
    expect(String(onError.mock.calls[0][0])).not.toContain("mfg_secret");
    await flags.refresh();
    expect(flags.isEnabled("missing", { visitor: "x" }, true)).toBe(true);
    await flags.close();
  });

  it("keeps the last good ruleset when a refresh fails", async () => {
    const { fetchImpl } = stub([
      () => Response.json({ data: { version: "v1", ruleset: RULESET } }),
      () => {
        throw new Error("offline");
      },
      () => Response.json({ data: { version: "v2" } }),
    ]);
    const onError = vi.fn();
    const flags = make({ fetchImpl, onError });
    await flags.ready();
    await flags.refresh();
    await flags.refresh();
    expect(onError).toHaveBeenCalledTimes(2);
    expect(flags.isEnabled("pro", { context: { plan: "pro" } })).toBe(true);
    await flags.close();
  });
});

describe("stats", () => {
  it("counts evaluations and one exposure per subject, and sends them on close", async () => {
    const { calls, fetchImpl } = stub();
    const flags = make({ fetchImpl, environment: "production" });
    await flags.ready();
    flags.isEnabled("pro", { visitor: "a", context: { plan: "pro" } });
    flags.isEnabled("pro", { visitor: "a", context: { plan: "pro" } });
    flags.isEnabled("pro", { visitor: "b", context: { plan: "pro" } });
    flags.isEnabled("missing", { visitor: "a" });
    await flags.close();

    const sent = calls.filter((c) => c.url.endsWith("/api/flags/events"));
    expect(sent).toHaveLength(1);
    expect((sent[0].init?.headers as Record<string, string>).authorization).toBeUndefined();
    const body = JSON.parse(String(sent[0].init?.body));
    expect(body).toMatchObject({ site: 3, environment: "production", source: "server" });
    expect(body.events).toEqual([
      { key: "pro", variant: null, enabled: true, kind: "evaluation", count: 3 },
      { key: "pro", variant: null, enabled: true, kind: "exposure", count: 2 },
    ]);
  });

  it("splits a large batch and sends nothing with stats off", async () => {
    const many: FlagRuleset = {
      flags: Array.from({ length: 250 }, (_, i) => ({ key: "f" + i, enabled: true })),
    };
    const { calls, fetchImpl } = stub([() => Response.json({ data: { ruleset: many } })]);
    const flags = make({ fetchImpl });
    await flags.ready();
    flags.all({ visitor: "a" });
    await flags.flush();
    const batches = calls.filter((c) => c.url.endsWith("/api/flags/events"));
    expect(batches.map((c) => JSON.parse(String(c.init?.body)).events.length)).toEqual([200, 200, 100]);
    await flags.close();

    const quiet = stub();
    const off = make({ fetchImpl: quiet.fetchImpl, stats: false });
    await off.ready();
    off.isEnabled("pro", { visitor: "a" });
    await off.close();
    expect(quiet.calls.some((c) => c.url.endsWith("/api/flags/events"))).toBe(false);
  });

  it("never throws when the network is gone", async () => {
    const fetchImpl = (async () => {
      throw new Error("offline");
    }) as unknown as typeof fetch;
    const flags = make({ fetchImpl });
    await expect(flags.ready()).resolves.toBeUndefined();
    expect(() => flags.isEnabled("x")).not.toThrow();
    await expect(flags.close()).resolves.toBeUndefined();
  });
});
