/**
 * Server helpers, auto-imported in Nitro routes and server middleware.
 *
 * ```ts
 * // server/middleware/agents.ts
 * export default defineEventHandler((event) => {
 *   useMicaforgeServer()?.pageview(event.node.req);
 * });
 * ```
 *
 * One client of each kind per server process, built on first use from the runtime
 * config, or `undefined` when the module is off or its key is not set (so a route reads
 * `useMicaforgeServer()?.track(...)` and works unchanged without one).
 */
import type { Micaforge } from "@micaforge/sdk-server";
import type { FlagsClient } from "@micaforge/sdk-server/flags";
import { useRuntimeConfig } from "#imports";
import { flagsClient, serverClient, type PrivateConfig, type SiteConfig } from "./clients.js";

let server: Micaforge | undefined;
let flags: FlagsClient | undefined;

const read = () => {
  const config = useRuntimeConfig();
  return { site: config.public.micaforge as SiteConfig | undefined, secrets: config.micaforge as PrivateConfig | undefined };
};

/**
 * The server SDK client: `pageview(req)` for server-rendered pageviews and the AI agent
 * fetches a browser tag never sees, `track(event)` for server-side events. Needs the
 * site's ingest key in `NUXT_MICAFORGE_INGEST_KEY`.
 */
export function useMicaforgeServer(): Micaforge | undefined {
  if (!server) {
    const { site, secrets } = read();
    server = serverClient(site, secrets);
  }
  return server;
}

/**
 * Feature flags decided on the server from a cached ruleset, with no network call per
 * request. Needs an API key with `manage:read` in `NUXT_MICAFORGE_API_KEY`.
 */
export function useMicaforgeFlags(): FlagsClient | undefined {
  if (!flags) {
    const { site, secrets } = read();
    flags = flagsClient(site, secrets);
  }
  return flags;
}
