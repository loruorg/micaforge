/**
 * Fastify plugin.
 *
 * ```ts
 * import Fastify from "fastify";
 * import { Micaforge } from "@micaforge/sdk-server";
 * import { micaforgeFastify } from "@micaforge/sdk-server/fastify";
 *
 * const app = Fastify();
 * const micaforge = new Micaforge({ host, siteId: 1, ingestKey });
 *
 * await app.register(micaforgeFastify, { client: micaforge });
 * ```
 *
 * The plugin registers one `onResponse` hook. That hook is the right one and the others
 * are not: `onSend` runs before the socket has taken the body, and `onRequest` runs
 * before there is a status code to report. `onResponse` fires once the reply is out,
 * which is when `status`, `bytes` and the response time are all facts.
 *
 * Fastify's encapsulation is opted out of the same way `fastify-plugin` does it, with
 * the `skip-override` symbol, so registering the plugin at the top level covers every
 * route in the app rather than only the routes in one scope. That symbol is Fastify's
 * documented interface for exactly this, and using it directly is what lets this package
 * keep its zero-dependency promise.
 */
import { classify } from "./agents.js";
import { pathMatcher } from "./index.js";
import type { Micaforge, MiddlewareOptions } from "./index.js";
import { header } from "./request.js";
import type { RequestLike } from "./types.js";

/** A Fastify request, as much of one as this adapter reads. */
export interface FastifyRequestLike extends RequestLike {
  /** The underlying `node:http` message. Read only as a fallback. */
  raw?: RequestLike;
}

/** A Fastify reply, as much of one as this adapter reads. */
export interface FastifyReplyLike {
  statusCode?: number;
  /** Milliseconds from request to reply, which Fastify measures for you. */
  elapsedTime?: number;
  getHeader?(name: string): number | string | string[] | undefined;
}

/** Just enough of a Fastify instance to add a hook to it. */
export interface FastifyInstanceLike {
  addHook(
    name: "onResponse",
    handler: (request: FastifyRequestLike, reply: FastifyReplyLike) => void | Promise<void>,
  ): unknown;
  addHook(name: "onClose", handler: () => void | Promise<void>): unknown;
}

/** Options for {@link micaforgeFastify}. */
export interface FastifyPluginOptions extends MiddlewareOptions {
  /** The client to report through. Required. */
  client: Micaforge;
  /**
   * Flush and close the client when Fastify closes. Default `true`.
   *
   * Fastify's `onClose` runs on a graceful shutdown, which `beforeExit` does not, so
   * this is what saves the last flush window when a container is rolled.
   */
  closeWithApp?: boolean;
}

const SKIP_OVERRIDE = Symbol.for("skip-override");

/**
 * The plugin. Register it once, at the top level.
 *
 * It never awaits anything on the request path: the hook records into the client's
 * queue and returns, and the queue is drained on its own timer.
 */
export async function micaforgeFastify(
  fastify: FastifyInstanceLike,
  options: FastifyPluginOptions,
): Promise<void> {
  const { client, closeWithApp, ...middlewareOptions } = options ?? ({} as FastifyPluginOptions);
  if (!client) throw new Error("micaforgeFastify needs a `client`; pass the Micaforge instance");

  const humans = middlewareOptions.humans !== false;
  const methods = (middlewareOptions.methods ?? ["GET", "HEAD"]).map((m) => m.toUpperCase());
  const isExcluded = pathMatcher(middlewareOptions.exclude);

  fastify.addHook("onResponse", (request, reply) => {
    try {
      const method = (request.method ?? "GET").toUpperCase();
      if (methods.length && !methods.includes(method)) return;

      // `request` already carries url, method, headers and ip. `raw` is the fallback for
      // an old Fastify or a decorated request that lost one of them.
      const source: RequestLike = request.url ? request : (request.raw ?? request);
      if (isExcluded(pathOf(source.url))) return;
      if (!humans && !classify(header(source.headers, "user-agent"))) return;

      client.pageview(source, {
        status: reply.statusCode,
        bytes: numberOf(reply.getHeader?.("content-length")),
        durationMs: typeof reply.elapsedTime == "number" ? Math.round(reply.elapsedTime) : undefined,
        contentType: stringOf(reply.getHeader?.("content-type")),
      });
    } catch {
      // The client logs its own faults. A hook that throws would turn an analytics
      // problem into a 500, which is the one thing this package promises never to do.
    }
  });

  if (closeWithApp !== false) {
    fastify.addHook("onClose", async () => {
      await client.shutdown();
    });
  }
}

// Fastify reads this symbol to decide whether the plugin gets its own scope. Setting it
// makes the hook apply to the whole application, which is what an analytics plugin has
// to do to be worth anything.
(micaforgeFastify as unknown as Record<symbol, boolean>)[SKIP_OVERRIDE] = true;

function pathOf(url: string | undefined): string {
  if (!url) return "/";
  const q = url.indexOf("?");
  return (q === -1 ? url : url.slice(0, q)) || "/";
}

function numberOf(value: unknown): number | undefined {
  const parsed = Number(Array.isArray(value) ? value[0] : value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function stringOf(value: unknown): string | undefined {
  if (value === undefined || value === null) return undefined;
  return String(Array.isArray(value) ? value[0] : value);
}

export default micaforgeFastify;
