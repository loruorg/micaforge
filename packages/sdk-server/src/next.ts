/**
 * Next.js adapter: a middleware and an instrumentation hook.
 *
 * # Which one you want
 *
 * **Middleware** sees every request, including the ones a crawler makes for a static
 * page that no React code runs for. That is the whole point for this product, so it is
 * the recommended install.
 *
 * **The instrumentation hook** is how the client gets built once per server process and
 * shut down cleanly. Use it with the middleware, not instead of it.
 *
 * ```ts
 * // instrumentation.ts
 * import { createInstrumentation } from "@micaforge/sdk-server/next";
 *
 * export const { register, client: micaforge } = createInstrumentation({
 *   host: process.env.MICAFORGE_HOST!,
 *   siteId: Number(process.env.MICAFORGE_SITE_ID),
 *   ingestKey: process.env.MICAFORGE_INGEST_KEY!,
 * });
 * ```
 *
 * ```ts
 * // middleware.ts
 * import { createMiddleware } from "@micaforge/sdk-server/next";
 * import { micaforge } from "./instrumentation";
 *
 * export const middleware = createMiddleware(micaforge, {
 *   exclude: ["/_next/**", "/favicon.ico"],
 * });
 *
 * // Signing is HMAC-SHA256 through `node:crypto`, which the Edge runtime does not
 * // have. Middleware must run on Node.
 * export const config = { runtime: "nodejs", matcher: ["/((?!_next/static).*)"] };
 * ```
 *
 * # If you cannot run middleware on Node
 *
 * On an older Next, or on a deployment target that only runs middleware at the edge,
 * skip the middleware and call {@link trackRequest} from a route handler or a server
 * component instead, passing the headers you already have. You lose the requests that
 * never reach your code, which for a fully static page is most of the crawls, so treat
 * it as the fallback it is. The log shipper covers that gap completely and is the better
 * answer when middleware is not available.
 */
import { classify } from "./agents.js";
import { pathMatcher } from "./index.js";
import { Micaforge } from "./index.js";
import type { MicaforgeOptions, MiddlewareOptions } from "./index.js";
import { header } from "./request.js";
import type { HeadersLike, HitExtras, RequestLike } from "./types.js";

/** A `NextRequest`, as much of one as this adapter reads. */
export interface NextRequestLike extends RequestLike {
  headers?: HeadersLike;
  nextUrl?: { pathname?: string; search?: string; hostname?: string; href?: string };
}

/** A `NextFetchEvent`. Only `waitUntil` is used. */
export interface NextFetchEventLike {
  waitUntil?(promise: Promise<unknown>): void;
}

/**
 * Build the Next middleware.
 *
 * It returns `undefined`, which Next reads as "carry on", so it composes with your own
 * middleware: call it, then return whatever response you were going to return.
 *
 * ```ts
 * const record = createMiddleware(micaforge);
 *
 * export function middleware(request: NextRequest, event: NextFetchEvent) {
 *   record(request, event);
 *   return NextResponse.next();
 * }
 * ```
 *
 * There is no status, no byte count and no response time here, because middleware runs
 * before the response exists. An agent row from this path carries the fetch, the path
 * and the user agent, and leaves the rest at the column defaults rather than guessing.
 */
export function createMiddleware(client: Micaforge, options: MiddlewareOptions = {}) {
  const isExcluded = pathMatcher(options.exclude);
  const humans = options.humans !== false;
  const methods = (options.methods ?? ["GET", "HEAD"]).map((m) => m.toUpperCase());

  return (request: NextRequestLike, event?: NextFetchEventLike): undefined => {
    try {
      const method = (request.method ?? "GET").toUpperCase();
      if (methods.length && !methods.includes(method)) return undefined;

      const pathname = request.nextUrl?.pathname ?? pathOf(request.url);
      if (isExcluded(pathname)) return undefined;
      if (!humans && !classify(header(request.headers, "user-agent"))) return undefined;

      client.pageview(request);

      // On a serverless deployment the invocation can be frozen the moment the response
      // is returned. `waitUntil` is what buys the send its few milliseconds.
      event?.waitUntil?.(client.flush());
    } catch {
      // The client logs its own faults. Middleware that throws returns a 500 for a page
      // that was about to render perfectly well.
    }
    return undefined;
  };
}

/**
 * Record one request from a route handler, a server component or a server action.
 *
 * ```ts
 * import { headers } from "next/headers";
 * import { trackRequest } from "@micaforge/sdk-server/next";
 *
 * export default async function Page() {
 *   trackRequest(micaforge, await headers(), "/docs/getting-started");
 *   ...
 * }
 * ```
 *
 * `next/headers` is not imported here: importing it would make this module unusable
 * outside a request scope and would tie the package to one Next version. Pass the
 * headers in.
 */
export function trackRequest(
  client: Micaforge,
  headers: HeadersLike,
  url: string,
  extras: HitExtras = {},
): void {
  client.pageview({ url, method: "GET", headers }, extras);
}

/** What {@link createInstrumentation} hands back. */
export interface Instrumentation {
  /** Export this as `register` from `instrumentation.ts`. */
  register: () => Promise<void>;
  /** The client. Import it wherever you record something. */
  client: Micaforge;
}

/**
 * Build the client once and wire it into the server's lifecycle.
 *
 * Next calls `register()` once per server process, before any request is handled, which
 * makes it the one honest place to construct something that owns a timer.
 *
 * The signal handlers matter more than they look. `beforeExit` fires on a clean exit
 * with an empty event loop, which a web server never has, and a platform rolling a
 * container sends `SIGTERM`. Without these, the last flush window is lost on every
 * deploy. The handlers are added with `once` and re-emit the signal afterwards, so they
 * do not swallow a shutdown that something else is also listening for.
 */
export function createInstrumentation(options: MicaforgeOptions): Instrumentation {
  const client = new Micaforge(options);

  const register = async (): Promise<void> => {
    const proc = (globalThis as { process?: NodeJS.Process }).process;
    if (!proc || typeof proc.once != "function") return;
    for (const signal of ["SIGTERM", "SIGINT"] as const) {
      proc.once(signal, () => {
        void client.shutdown().finally(() => {
          proc.kill?.(proc.pid as number, signal);
        });
      });
    }
  };

  return { register, client };
}

function pathOf(url: string | undefined): string {
  if (!url) return "/";
  try {
    return new URL(url).pathname || "/";
  } catch {
    const q = url.indexOf("?");
    return (q === -1 ? url : url.slice(0, q)) || "/";
  }
}

export default createMiddleware;
