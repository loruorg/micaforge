/**
 * Express adapter.
 *
 * ```ts
 * import express from "express";
 * import { Micaforge } from "@micaforge/sdk-server";
 *
 * const app = express();
 * const micaforge = new Micaforge({
 *   host: process.env.MICAFORGE_HOST!,
 *   siteId: 1,
 *   ingestKey: process.env.MICAFORGE_INGEST_KEY!,
 * });
 *
 * app.use(micaforge.express());
 * ```
 *
 * `micaforge.express()` is the whole install and lives on the client itself, because
 * Express middleware is `(req, res, next)` and so is a bare `node:http` handler. This
 * module adds the Express-shaped types, so an app with `@types/express` gets a middleware
 * that fits `app.use()` without a cast, and {@link micaforgeExpress} for anyone who
 * prefers passing the client in rather than reaching a method off it.
 *
 * Express types are declared structurally here rather than imported. This package has no
 * dependencies, and it is not going to acquire one to describe three fields.
 */
import type { Micaforge, MiddlewareOptions, ResponseLike } from "./index.js";
import type { RequestLike } from "./types.js";

/** An Express request, as much of one as this adapter reads. */
export interface ExpressRequestLike extends RequestLike {
  originalUrl?: string;
}

/** An Express response, as much of one as this adapter reads. */
export type ExpressResponseLike = ResponseLike;

/** What `app.use()` expects. */
export type ExpressMiddleware = (
  req: ExpressRequestLike,
  res: ExpressResponseLike,
  next: (error?: unknown) => void,
) => void;

/**
 * Build the Express middleware for a client.
 *
 * ```ts
 * app.use(micaforgeExpress(micaforge, { exclude: ["/health", "/_next/**"] }));
 * ```
 *
 * Mount it **first**, before your routes and before any body parser. It reports on the
 * response's `finish` event, so it needs to be watching before a handler answers, and it
 * reads nothing from the body.
 *
 * Behind a proxy, either leave `trustProxy` on (the default) or set Express's own
 * `trust proxy` and let `req.ip` carry the answer. Both paths end at the same address.
 */
export function micaforgeExpress(
  client: Micaforge,
  options: MiddlewareOptions = {},
): ExpressMiddleware {
  const middleware = client.middleware(options);
  return (req, res, next) => {
    // `originalUrl` survives a mounted router; `req.url` inside `app.use("/docs", ...)`
    // has already had the mount path stripped, which would file every hit under the
    // wrong path. Express only sets `originalUrl`, so nothing else is affected.
    const target: ExpressRequestLike =
      typeof req.originalUrl == "string" && req.originalUrl !== req.url
        ? Object.create(req, { url: { value: req.originalUrl, enumerable: true } })
        : req;
    middleware(target, res, next);
  };
}

export default micaforgeExpress;
