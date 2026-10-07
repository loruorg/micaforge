import { EventEmitter } from "node:events";

import { describe, expect, it, vi } from "vitest";

import { micaforgeExpress } from "./express.js";
import { micaforgeFastify } from "./fastify.js";
import { micaforgeHono } from "./hono.js";
import { Micaforge } from "./index.js";
import { createInstrumentation, createMiddleware, trackRequest } from "./next.js";
import type { EdgePayload } from "./types.js";

const GPTBOT = "Mozilla/5.0 (compatible; GPTBot/1.2; +https://openai.com/gptbot)";
const CHROME = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/128.0 Safari/537.36";

function client() {
  const bodies: EdgePayload[] = [];
  const fetchStub = vi.fn(async (_url: unknown, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body ?? "{}")) as EdgePayload);
    return new Response(null, { status: 204 });
  });
  const micaforge = new Micaforge({
    host: "https://mf.example.com",
    siteId: 1,
    ingestKey: "k",
    logger: null,
    flushOnExit: false,
    fetch: fetchStub as unknown as typeof globalThis.fetch,
  });
  return { micaforge, bodies };
}

/** A `node:http` response, near enough. */
class FakeResponse extends EventEmitter {
  statusCode = 200;
  headers: Record<string, string> = { "content-type": "text/html", "content-length": "512" };
  getHeader(name: string) {
    return this.headers[name.toLowerCase()];
  }
  finish() {
    this.emit("finish");
  }
}

/* ------------------------------------------------------------------ express */

describe("the Express adapter", () => {
  const req = (ua: string, extra: Record<string, unknown> = {}) => ({
    method: "GET",
    url: "/guide",
    headers: { host: "docs.example.com", "user-agent": ua },
    socket: { remoteAddress: "203.0.113.9" },
    ...extra,
  });

  it("records a crawl and calls next()", async () => {
    const { micaforge, bodies } = client();
    const next = vi.fn();
    const res = new FakeResponse();
    micaforgeExpress(micaforge)(req(GPTBOT), res, next);
    res.finish();
    await micaforge.flush();

    expect(next).toHaveBeenCalledTimes(1);
    expect(bodies[0]!.agent_events![0]!.pathname).toBe("/guide");
    expect(bodies[0]!.agent_events![0]!.status).toBe(200);
    expect(bodies[0]!.agent_events![0]!.bytes).toBe(512);
  });

  it("uses originalUrl, so a mounted router does not lose the mount path", async () => {
    const { micaforge, bodies } = client();
    const res = new FakeResponse();
    // Inside app.use("/docs", router), Express rewrites req.url to "/guide".
    micaforgeExpress(micaforge)(
      req(GPTBOT, { url: "/guide", originalUrl: "/docs/guide?a=1" }),
      res,
      () => {},
    );
    res.finish();
    await micaforge.flush();
    expect(bodies[0]!.agent_events![0]!.pathname).toBe("/docs/guide");
    expect(bodies[0]!.agent_events![0]!.querystring).toBe("a=1");
  });

  it("records a browser as a pageview", async () => {
    const { micaforge, bodies } = client();
    const res = new FakeResponse();
    micaforgeExpress(micaforge)(req(CHROME), res, () => {});
    res.finish();
    await micaforge.flush();
    expect(bodies[0]!.events).toHaveLength(1);
    expect(bodies[0]!.agent_events).toBeUndefined();
  });
});

/* ------------------------------------------------------------------ fastify */

describe("the Fastify plugin", () => {
  /** Just enough of a Fastify instance to register against. */
  function fakeFastify() {
    const hooks: Record<string, ((...args: never[]) => unknown)[]> = {};
    return {
      hooks,
      addHook(name: string, handler: (...args: never[]) => unknown) {
        (hooks[name] ??= []).push(handler);
        return this;
      },
    };
  }

  const request = (ua: string) => ({
    method: "GET",
    url: "/guide?a=1",
    headers: { host: "docs.example.com", "user-agent": ua },
    ip: "203.0.113.9",
  });
  const reply = () => ({
    statusCode: 200,
    elapsedTime: 12.7,
    getHeader: (name: string) =>
      ({ "content-type": "text/html; charset=utf-8", "content-length": "812" })[name.toLowerCase()],
  });

  it("registers one onResponse hook and one onClose hook", async () => {
    const { micaforge } = client();
    const app = fakeFastify();
    await micaforgeFastify(app as never, { client: micaforge });
    expect(app.hooks.onResponse).toHaveLength(1);
    expect(app.hooks.onClose).toHaveLength(1);
  });

  it("opts out of encapsulation the way fastify-plugin does", () => {
    expect((micaforgeFastify as unknown as Record<symbol, boolean>)[Symbol.for("skip-override")]).toBe(true);
  });

  it("records a crawl with the reply's real status, size and time", async () => {
    const { micaforge, bodies } = client();
    const app = fakeFastify();
    await micaforgeFastify(app as never, { client: micaforge });
    await (app.hooks.onResponse![0] as (r: unknown, p: unknown) => unknown)(request(GPTBOT), reply());
    await micaforge.flush();

    const row = bodies[0]!.agent_events![0]!;
    expect(row.pathname).toBe("/guide");
    expect(row.querystring).toBe("a=1");
    expect(row.status).toBe(200);
    expect(row.bytes).toBe(812);
    expect(row.duration_ms).toBe(13);
    expect(row.content_type).toBe("text/html");
  });

  it("honours exclude and humans", async () => {
    const { micaforge, bodies } = client();
    const app = fakeFastify();
    await micaforgeFastify(app as never, { client: micaforge, humans: false, exclude: ["/guide"] });
    const hook = app.hooks.onResponse![0] as (r: unknown, p: unknown) => unknown;
    await hook(request(GPTBOT), reply()); // excluded path
    await hook({ ...request(CHROME), url: "/other" }, reply()); // human, humans: false
    await micaforge.flush();
    expect(bodies).toHaveLength(0);
  });

  it("refuses to register without a client, rather than failing silently later", async () => {
    await expect(micaforgeFastify(fakeFastify() as never, {} as never)).rejects.toThrow(/client/);
  });
});

/* --------------------------------------------------------------------- hono */

describe("the Hono middleware", () => {
  const context = (ua: string, waitUntil?: (p: Promise<unknown>) => void) => ({
    req: { raw: new Request("https://docs.example.com/guide?a=1", { headers: { "user-agent": ua } }) },
    res: new Response("hello", { status: 200, headers: { "content-type": "text/html", "content-length": "5" } }),
    executionCtx: waitUntil ? { waitUntil } : undefined,
  });

  it("records after the handler has answered", async () => {
    const { micaforge, bodies } = client();
    const c = context(GPTBOT);
    const order: string[] = [];
    await micaforgeHono(micaforge)(c, async () => {
      order.push("handler");
    });
    order.push("after");
    await micaforge.flush();

    expect(order).toEqual(["handler", "after"]);
    const row = bodies[0]!.agent_events![0]!;
    expect(row.hostname).toBe("docs.example.com");
    expect(row.pathname).toBe("/guide");
    expect(row.querystring).toBe("a=1");
    expect(row.status).toBe(200);
    expect(row.bytes).toBe(5);
  });

  it("hands the flush to waitUntil, which is what keeps a Worker alive for the send", async () => {
    const { micaforge } = client();
    const waitUntil = vi.fn();
    await micaforgeHono(micaforge)(context(GPTBOT, waitUntil), async () => {});
    expect(waitUntil).toHaveBeenCalledTimes(1);
    await waitUntil.mock.calls[0]![0];
  });

  it("does not swallow an error from the handler", async () => {
    const { micaforge } = client();
    await expect(
      micaforgeHono(micaforge)(context(GPTBOT), async () => {
        throw new Error("handler blew up");
      }),
    ).rejects.toThrow("handler blew up");
  });
});

/* --------------------------------------------------------------------- next */

describe("the Next adapter", () => {
  const nextRequest = (ua: string) => ({
    method: "GET",
    url: "https://docs.example.com/docs/guide?a=1",
    headers: new Headers({ "user-agent": ua, "x-forwarded-for": "203.0.113.9" }),
    nextUrl: { pathname: "/docs/guide" },
  });

  it("returns undefined so it composes with your own middleware", () => {
    const { micaforge } = client();
    expect(createMiddleware(micaforge)(nextRequest(GPTBOT))).toBeUndefined();
  });

  it("records a crawl and uses the fetch event's waitUntil", async () => {
    const { micaforge, bodies } = client();
    const waitUntil = vi.fn();
    createMiddleware(micaforge)(nextRequest(GPTBOT), { waitUntil });
    expect(waitUntil).toHaveBeenCalledTimes(1);
    await waitUntil.mock.calls[0]![0];
    expect(bodies[0]!.agent_events![0]!.pathname).toBe("/docs/guide");
    expect(bodies[0]!.agent_events![0]!.ip).toBe("203.0.113.9");
  });

  it("respects exclude against nextUrl.pathname", async () => {
    const { micaforge, bodies } = client();
    createMiddleware(micaforge, { exclude: ["/docs/**"] })(nextRequest(GPTBOT));
    await micaforge.flush();
    expect(bodies).toHaveLength(0);
  });

  it("records from a server component through trackRequest", async () => {
    const { micaforge, bodies } = client();
    trackRequest(micaforge, new Headers({ "user-agent": CHROME, referer: "https://chatgpt.com/" }), "/pricing", {
      title: "Pricing",
    });
    await micaforge.flush();
    expect(bodies[0]!.events![0]!.pathname).toBe("/pricing");
    expect(bodies[0]!.events![0]!.page_title).toBe("Pricing");
    expect(bodies[0]!.events![0]!.referrer).toBe("https://chatgpt.com/");
  });

  it("builds a client and a register() that can be called safely", async () => {
    const { register, client: instance } = createInstrumentation({
      host: "https://mf.example.com",
      siteId: 1,
      ingestKey: "k",
      logger: null,
      flushOnExit: false,
    });
    expect(instance).toBeInstanceOf(Micaforge);
    await expect(register()).resolves.toBeUndefined();
    // The handlers it installs must not be left behind for the next test file.
    process.removeAllListeners("SIGTERM");
    process.removeAllListeners("SIGINT");
  });
});
