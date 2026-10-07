/**
 * Hono middleware. Runs on Node, Bun, Deno and Cloudflare Workers.
 *
 * ```ts
 * import { Hono } from "hono";
 * import { Micaforge } from "@micaforge/sdk-server";
 * import { micaforgeHono } from "@micaforge/sdk-server/hono";
 *
 * const app = new Hono();
 * const micaforge = new Micaforge({ host: HOST, siteId: 1, ingestKey: KEY });
 *
 * app.use("*", micaforgeHono(micaforge));
 * ```
 *
 * # Two things about the edge
 *
 * **`waitUntil`.** On Workers, anything still running when the response is returned is
 * cancelled. A queued hit would be thrown away with the isolate. So this middleware
 * hands the flush to `c.executionCtx.waitUntil()` when there is one, which keeps the
 * isolate alive exactly long enough to finish the send and no longer. On Node and Bun
 * there is no execution context and the client's own 200 ms window does the work.
 *
 * **`node:crypto`.** Signing is HMAC-SHA256 through `node:crypto`, which on Workers
 * means `compatibility_flags = ["nodejs_compat"]` in `wrangler.toml`. Bun and Deno ship
 * `node:crypto` natively and need nothing.
 *
 * On Workers the client is also worth constructing per request rather than at module
 * scope, because an isolate can be recycled between requests and a queue that outlives
 * the request it belongs to is a queue that gets dropped. Per request plus `waitUntil`
 * is the shape that always sends.
 */
import { classify } from "./agents.js";
import { pathMatcher } from "./index.js";
import type { Micaforge, MiddlewareOptions } from "./index.js";
import { header } from "./request.js";

/** Just enough of a Hono context to read a request and a response out of it. */
export interface HonoContextLike {
  req: { raw: Request; method?: string; path?: string; url?: string };
  res: Response;
  executionCtx?: { waitUntil(promise: Promise<unknown>): void } | undefined;
}

/** Hono's `next`. */
export type HonoNext = () => Promise<void>;

/** Options for {@link micaforgeHono}. */
export interface HonoOptions extends MiddlewareOptions {
  /**
   * Hand the flush to `executionCtx.waitUntil` when one exists. Default `true`.
   *
   * There is almost never a reason to turn this off. The one case is a runtime whose
   * `waitUntil` is a stub that rejects, where the client's own timer is the better bet.
   */
  waitUntil?: boolean;
}

/**
 * Build the Hono middleware.
 *
 * It awaits `next()` first, so `status`, `content-type` and `content-length` are read
 * from the finished response rather than guessed at. `bytes` comes from
 * `content-length`; a streamed response without one reports no size rather than an
 * invented one.
 */
export function micaforgeHono(client: Micaforge, options: HonoOptions = {}) {
  const isExcluded = pathMatcher(options.exclude);
  const humans = options.humans !== false;
  const methods = (options.methods ?? ["GET", "HEAD"]).map((m) => m.toUpperCase());
  const useWaitUntil = options.waitUntil !== false;

  return async (c: HonoContextLike, next: HonoNext): Promise<void> => {
    const started = Date.now();
    await next();

    try {
      const request = c.req.raw;
      const method = (request.method ?? "GET").toUpperCase();
      if (methods.length && !methods.includes(method)) return;

      const pathname = safePath(request.url);
      if (isExcluded(pathname)) return;

      const agent = classify(header(request.headers, "user-agent"));
      if (!agent && !humans) return;

      const response = c.res;
      client.pageview(request, {
        status: response?.status,
        bytes: contentLength(response),
        durationMs: Date.now() - started,
        contentType: response?.headers?.get("content-type") ?? undefined,
      });

      // Keep the isolate alive for the send, then let it go.
      if (useWaitUntil && c.executionCtx?.waitUntil) {
        c.executionCtx.waitUntil(client.flush());
      }
    } catch {
      // The client reports its own faults through its logger. A middleware that throws
      // after the handler has already answered would turn a clean 200 into a 500.
    }
  };
}

function safePath(url: string | undefined): string {
  if (!url) return "/";
  try {
    return new URL(url).pathname || "/";
  } catch {
    const q = url.indexOf("?");
    return (q === -1 ? url : url.slice(0, q)) || "/";
  }
}

function contentLength(response: Response | undefined): number | undefined {
  const raw = response?.headers?.get("content-length");
  if (!raw) return undefined;
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : undefined;
}

export default micaforgeHono;
