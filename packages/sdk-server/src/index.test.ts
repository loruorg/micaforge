import { createHmac } from "node:crypto";
import { EventEmitter } from "node:events";

import { describe, expect, it, vi } from "vitest";

import {
  EDGE_PATH,
  Micaforge,
  SIGNATURE_HEADER,
  signBody,
  toWireAgentEvent,
  toWireEvent,
} from "./index.js";
import type { EdgePayload } from "./types.js";

const KEY = "mfg_ingest_test_key";
const HOST = "https://micaforge.example.com";

/** A fetch stub that records every call and always succeeds. */
function recorder(status = 204) {
  const calls: { url: string; headers: Record<string, string>; body: EdgePayload; raw: string }[] = [];
  const fetchStub = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
    const raw = String(init?.body ?? "");
    calls.push({
      url: String(url),
      headers: (init?.headers ?? {}) as Record<string, string>,
      body: JSON.parse(raw) as EdgePayload,
      raw,
    });
    return new Response(null, { status });
  });
  return { calls, fetchStub: fetchStub as unknown as typeof globalThis.fetch };
}

function client(overrides: Partial<ConstructorParameters<typeof Micaforge>[0]> = {}) {
  const { calls, fetchStub } = recorder();
  const micaforge = new Micaforge({
    host: HOST,
    siteId: 7,
    ingestKey: KEY,
    logger: null,
    flushOnExit: false,
    fetch: fetchStub,
    ...overrides,
  });
  return { micaforge, calls };
}

const GPTBOT =
  "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; +https://openai.com/gptbot)";
const CHROME =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0 Safari/537.36";

/* ------------------------------------------------------------------ signing */

describe("signBody()", () => {
  it("is HMAC-SHA256 over `${t}.${body}`, rendered as t=<unix>,v1=<hex>", () => {
    const body = '{"site_id":7}';
    const signature = signBody(KEY, body, 1_787_306_400);
    const expected = createHmac("sha256", KEY).update(`1787306400.${body}`, "utf8").digest("hex");
    expect(signature).toBe(`t=1787306400,v1=${expected}`);
  });

  it("does not sign the body alone", () => {
    const body = '{"site_id":7}';
    const bodyOnly = createHmac("sha256", KEY).update(body, "utf8").digest("hex");
    expect(signBody(KEY, body, 1_787_306_400)).not.toContain(bodyOnly);
  });

  it("changes when the timestamp changes, so a captured MAC cannot take a fresh t", () => {
    const a = signBody(KEY, '{"site_id":7}', 1_787_306_400).split(",v1=")[1];
    const b = signBody(KEY, '{"site_id":7}', 1_787_306_460).split(",v1=")[1];
    expect(a).not.toBe(b);
  });

  it("signs whole seconds", () => {
    expect(signBody(KEY, "x", 1_787_306_400.9)).toMatch(/^t=1787306400,v1=/);
  });

  it("changes when one byte of the body changes", () => {
    const a = signBody(KEY, '{"site_id":7}', 1);
    const b = signBody(KEY, '{"site_id":8}', 1);
    expect(a).not.toBe(b);
  });

  it("changes when the key changes", () => {
    expect(signBody("a", "x", 1)).not.toBe(signBody("b", "x", 1));
  });
});

describe("the request it sends", () => {
  it("posts to /api/log/edge with a signature over the exact bytes sent", async () => {
    const { micaforge, calls } = client();
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://docs.example.com/guide" });
    await micaforge.flush();

    expect(calls).toHaveLength(1);
    expect(calls[0]!.url).toBe(HOST + EDGE_PATH);

    const signature = calls[0]!.headers[SIGNATURE_HEADER]!;
    const match = /^t=(\d+),v1=([0-9a-f]{64})$/.exec(signature);
    expect(match).not.toBeNull();
    // The signature must verify against the timestamp and the body that actually
    // went, not a re-serialised copy of the object.
    const expected = createHmac("sha256", KEY)
      .update(`${match![1]}.${calls[0]!.raw}`, "utf8")
      .digest("hex");
    expect(match![2]).toBe(expected);
    // And the timestamp is now, not the epoch.
    expect(Math.abs(Number(match![1]) - Math.floor(Date.now() / 1000))).toBeLessThan(5);
  });

  it("names the two arrays after the two tables they land in", async () => {
    const { micaforge, calls } = client();
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://docs.example.com/a" });
    micaforge.track({ url: "https://docs.example.com/b", userAgent: CHROME });
    await micaforge.flush();

    expect(calls[0]!.body.site_id).toBe(7);
    expect(calls[0]!.body.agent_events).toHaveLength(1);
    expect(calls[0]!.body.events).toHaveLength(1);
  });

  it("keeps a trailing path on the host", () => {
    const { micaforge } = client({ host: "https://example.com/mf/" });
    expect(micaforge.endpoint).toBe("https://example.com/mf" + EDGE_PATH);
  });
});

/* ------------------------------------------------------------------ mapping */

describe("toWireEvent()", () => {
  it("maps the public names onto schema columns and nothing else", () => {
    const row = toWireEvent({
      url: "https://docs.example.com/guide?a=1#top",
      title: "Guide",
      referrer: "https://chatgpt.com/",
      name: "signup",
      props: { plan: "pro" },
      revenue: 19.5,
      currency: "EUR",
      identifiedId: "u_123",
      userAgent: CHROME,
      ip: "203.0.113.9",
      timestamp: new Date("2026-08-21T10:00:00.000Z"),
    });
    expect(row).toEqual({
      timestamp: "2026-08-21T10:00:00.000Z",
      kind: "custom",
      event_name: "signup",
      hostname: "docs.example.com",
      pathname: "/guide",
      page_title: "Guide",
      querystring: "a=1",
      hash: "top",
      referrer: "https://chatgpt.com/",
      identified_id: "u_123",
      props: { plan: "pro" },
      revenue: 19.5,
      currency: "EUR",
      sdk_version: "0.1.0",
      user_agent: CHROME,
      ip: "203.0.113.9",
    });
  });

  it("sends the client's headers as transport fields for the bot check", () => {
    const row = toWireEvent({
      pathname: "/",
      userAgent: CHROME,
      headers: { accept: "text/html", acceptLanguage: "en", secFetchMode: "navigate", secChUa: "" },
    });
    expect(row.accept).toBe("text/html");
    expect(row.accept_language).toBe("en");
    expect(row.sec_fetch_mode).toBe("navigate");
    expect("sec_ch_ua" in row).toBe(false);
  });

  it("keeps the columns that have no DEFAULT, and drops the ones that do", () => {
    const row = toWireEvent({ pathname: "/" });
    expect(Object.keys(row).sort()).toEqual(["hostname", "kind", "pathname", "sdk_version", "timestamp"]);
    expect(row.hostname).toBe("");
  });

  it("falls back to now rather than to the epoch on a broken timestamp", () => {
    const row = toWireEvent({ pathname: "/", timestamp: "not a date" });
    expect(new Date(row.timestamp).getFullYear()).toBeGreaterThan(2020);
  });
});

describe("toWireAgentEvent()", () => {
  it("sends only what the client can honestly know", () => {
    const row = toWireAgentEvent({
      userAgent: GPTBOT,
      url: "https://docs.example.com/guide?a=1",
      method: "get",
      status: 200,
      bytes: 4210,
      durationMs: 42,
      contentType: "text/html; charset=utf-8",
      conditional: true,
      ip: "203.0.113.9",
      timestamp: "2026-08-21T10:00:00.000Z",
    });
    expect(row).toEqual({
      timestamp: "2026-08-21T10:00:00.000Z",
      user_agent: GPTBOT,
      method: "GET",
      hostname: "docs.example.com",
      pathname: "/guide",
      querystring: "a=1",
      status: 200,
      bytes: 4210,
      duration_ms: 42,
      content_type: "text/html",
      conditional: 1,
      ip: "203.0.113.9",
    });
  });

  it("never sets a field the server has to establish", () => {
    const row = toWireAgentEvent({ userAgent: GPTBOT, url: "https://x.example/a" }) as unknown as Record<
      string,
      unknown
    >;
    for (const forbidden of [
      "agent_id",
      "operator",
      "purpose",
      "verified",
      "asn",
      "asn_org",
      "country",
      "ip_hash",
      "robots_allowed",
      "llms_listed",
    ]) {
      expect(row[forbidden]).toBeUndefined();
    }
  });
});

/* -------------------------------------------------------------- behaviour */

describe("classification", () => {
  it("files a crawler's pageview under agent_events, not events", async () => {
    const { micaforge, calls } = client();
    micaforge.pageview({
      method: "GET",
      url: "/docs/guide",
      headers: { host: "docs.example.com", "user-agent": GPTBOT },
    });
    await micaforge.flush();
    expect(calls[0]!.body.agent_events).toHaveLength(1);
    expect(calls[0]!.body.events).toBeUndefined();
  });

  it("files a browser's pageview under events", async () => {
    const { micaforge, calls } = client();
    micaforge.pageview({
      method: "GET",
      url: "/docs/guide",
      headers: { host: "docs.example.com", "user-agent": CHROME, referer: "https://chatgpt.com/" },
    });
    await micaforge.flush();
    expect(calls[0]!.body.events).toHaveLength(1);
    // The referrer travels unresolved: naming the channel is the server's job.
    expect(calls[0]!.body.events![0]!.referrer).toBe("https://chatgpt.com/");
    expect(calls[0]!.body.agent_events).toBeUndefined();
  });

  it("re-files a track()ed pageview from a crawler, but leaves a named event alone", async () => {
    const { micaforge, calls } = client();
    micaforge.track({ url: "https://x.example/a", userAgent: GPTBOT });
    micaforge.track({ name: "signup", url: "https://x.example/b", userAgent: GPTBOT });
    await micaforge.flush();
    expect(calls[0]!.body.agent_events).toHaveLength(1);
    expect(calls[0]!.body.events).toHaveLength(1);
    expect(calls[0]!.body.events![0]!.event_name).toBe("signup");
  });
});

describe("batching", () => {
  it("sends as soon as the batch is full, without waiting for the window", async () => {
    const { micaforge, calls } = client({ maxBatch: 3, flushInterval: 60_000 });
    for (let i = 0; i < 3; i++) micaforge.agentHit({ userAgent: GPTBOT, url: `https://x.example/${i}` });
    await vi.waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]!.body.agent_events).toHaveLength(3);
    expect(micaforge.queued).toBe(0);
  });

  it("sends a partial batch when the window closes", async () => {
    const { micaforge, calls } = client({ flushInterval: 30 });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/a" });
    expect(calls).toHaveLength(0);
    await vi.waitFor(() => expect(calls).toHaveLength(1), { timeout: 2000 });
  });

  it("takes a mixed batch and sorts it into the right two arrays", async () => {
    const { micaforge, calls } = client();
    micaforge.batch([
      { type: "agent", userAgent: GPTBOT, url: "https://x.example/a" },
      { type: "event", url: "https://x.example/b", userAgent: CHROME },
      { type: "agent", userAgent: GPTBOT, url: "https://x.example/c" },
    ]);
    await micaforge.flush();
    expect(calls[0]!.body.agent_events).toHaveLength(2);
    expect(calls[0]!.body.events).toHaveLength(1);
  });

  it("sends nothing when nothing is queued", async () => {
    const { micaforge, calls } = client();
    await micaforge.flush();
    expect(calls).toHaveLength(0);
  });
});

describe("failure", () => {
  it("never throws out of a public method", () => {
    const { micaforge } = client();
    const nonsense = { get url() { throw new Error("boom"); } } as unknown as Parameters<Micaforge["pageview"]>[0];
    expect(() => micaforge.pageview(nonsense)).not.toThrow();
    expect(() => micaforge.track(null as never)).not.toThrow();
    expect(() => micaforge.agentHit(null as never)).not.toThrow();
    expect(() => micaforge.batch("not an array" as never)).not.toThrow();
  });

  it("never rejects out of flush(), whatever the transport does", async () => {
    const fetchStub = vi.fn(async () => {
      throw new Error("network is down");
    });
    const micaforge = new Micaforge({
      host: HOST,
      siteId: 7,
      ingestKey: KEY,
      logger: null,
      flushOnExit: false,
      retries: 0,
      fetch: fetchStub as unknown as typeof globalThis.fetch,
    });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/a" });
    await expect(micaforge.flush()).resolves.toBeUndefined();
    expect(fetchStub).toHaveBeenCalledTimes(1);
  });

  it("retries a 503 and gives up after the budget", async () => {
    const fetchStub = vi.fn(async () => new Response(null, { status: 503 }));
    const micaforge = new Micaforge({
      host: HOST,
      siteId: 7,
      ingestKey: KEY,
      logger: null,
      flushOnExit: false,
      retries: 1,
      fetch: fetchStub as unknown as typeof globalThis.fetch,
    });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/a" });
    await micaforge.flush();
    expect(fetchStub).toHaveBeenCalledTimes(2);
  });

  it("does not retry a 400, because the request itself is wrong", async () => {
    const fetchStub = vi.fn(async () => new Response("bad", { status: 400 }));
    const micaforge = new Micaforge({
      host: HOST,
      siteId: 7,
      ingestKey: KEY,
      logger: null,
      flushOnExit: false,
      retries: 3,
      fetch: fetchStub as unknown as typeof globalThis.fetch,
    });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/a" });
    await micaforge.flush();
    expect(fetchStub).toHaveBeenCalledTimes(1);
  });

  it("disables itself and says why when it is misconfigured", async () => {
    const errors: string[] = [];
    const { fetchStub } = recorder();
    const micaforge = new Micaforge({
      host: HOST,
      siteId: 7,
      ingestKey: "",
      logger: { warn: () => {}, error: (m) => errors.push(m) },
      flushOnExit: false,
      fetch: fetchStub,
    });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/a" });
    await micaforge.flush();
    expect(errors.join(" ")).toMatch(/ingestKey/);
    expect(micaforge.queued).toBe(0);
    expect(fetchStub).not.toHaveBeenCalled();
  });

  it("builds nothing when enabled is false", async () => {
    const { micaforge, calls } = client({ enabled: false });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/a" });
    await micaforge.flush();
    expect(calls).toHaveLength(0);
  });
});

describe("excludePaths", () => {
  it("drops a path before it is ever queued", async () => {
    const { micaforge, calls } = client({ excludePaths: ["/health", "/_next/**"] });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/health" });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/_next/static/a/b.js" });
    micaforge.agentHit({ userAgent: GPTBOT, url: "https://x.example/docs" });
    await micaforge.flush();
    expect(calls[0]!.body.agent_events).toHaveLength(1);
    expect(calls[0]!.body.agent_events![0]!.pathname).toBe("/docs");
  });
});

/* ------------------------------------------------------------- middleware */

/** A `node:http` response, near enough for the middleware to work against. */
class FakeResponse extends EventEmitter {
  statusCode = 200;
  private headers: Record<string, string> = { "content-type": "text/html", "content-length": "4210" };
  getHeader(name: string) {
    return this.headers[name.toLowerCase()];
  }
  finish() {
    this.emit("finish");
  }
}

describe("middleware()", () => {
  const request = (ua: string) => ({
    method: "GET",
    url: "/docs/guide?a=1",
    headers: { host: "docs.example.com", "user-agent": ua },
    socket: { remoteAddress: "203.0.113.9" },
  });

  it("records a crawler once the response has finished, with its real status and size", async () => {
    const { micaforge, calls } = client();
    const res = new FakeResponse();
    micaforge.middleware()(request(GPTBOT), res);
    expect(micaforge.queued).toBe(0); // nothing recorded before the response is out
    res.finish();
    await micaforge.flush();

    const row = calls[0]!.body.agent_events![0]!;
    expect(row.pathname).toBe("/docs/guide");
    expect(row.querystring).toBe("a=1");
    expect(row.status).toBe(200);
    expect(row.bytes).toBe(4210);
    expect(row.content_type).toBe("text/html");
    expect(row.ip).toBe("203.0.113.9");
  });

  it("calls next() exactly once, after it is watching the response", () => {
    const { micaforge } = client();
    const next = vi.fn();
    const res = new FakeResponse();
    micaforge.middleware()(request(CHROME), res, next);
    expect(next).toHaveBeenCalledTimes(1);
    // The listener has to be in place before the handler can finish the response.
    expect(res.listenerCount("finish")).toBe(1);
  });

  it("still calls next() when recording throws", () => {
    const { micaforge } = client();
    const next = vi.fn();
    const broken = { get headers() { throw new Error("boom"); } } as never;
    expect(() => micaforge.middleware()(broken, new FakeResponse(), next)).not.toThrow();
    expect(next).toHaveBeenCalledTimes(1);
  });

  it("leaves human traffic to the browser tracker when humans is false", async () => {
    const { micaforge, calls } = client();
    const human = new FakeResponse();
    const bot = new FakeResponse();
    const middleware = micaforge.middleware({ humans: false });
    middleware(request(CHROME), human);
    middleware(request(GPTBOT), bot);
    human.finish();
    bot.finish();
    await micaforge.flush();
    expect(calls[0]!.body.events).toBeUndefined();
    expect(calls[0]!.body.agent_events).toHaveLength(1);
  });

  it("ignores a method that is not a read", async () => {
    const { micaforge, calls } = client();
    const res = new FakeResponse();
    micaforge.middleware()({ ...request(GPTBOT), method: "POST" }, res);
    res.finish();
    await micaforge.flush();
    expect(calls).toHaveLength(0);
  });

  it("marks a re-crawl check as conditional", async () => {
    const { micaforge, calls } = client();
    const res = new FakeResponse();
    res.statusCode = 304;
    micaforge.middleware()(
      {
        method: "GET",
        url: "/docs/guide",
        headers: { host: "d.example.com", "user-agent": GPTBOT, "if-none-match": '"v2"' },
      },
      res,
    );
    res.finish();
    await micaforge.flush();
    expect(calls[0]!.body.agent_events![0]!.conditional).toBe(1);
    expect(calls[0]!.body.agent_events![0]!.status).toBe(304);
  });
});
