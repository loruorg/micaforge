/**
 * The server SDK clients a Nitro route uses, built from the runtime config.
 *
 * Kept apart from `utils.ts` (which reads `#imports`) so it can be tested without Nitro.
 * The secrets are private runtime config, never public: the ingest key signs agent hits
 * and server pageviews, the API key reads the flag ruleset, and neither may reach a
 * browser bundle.
 */
import { Micaforge } from "@micaforge/sdk-server";
import { createFlagsClient, type FlagsClient } from "@micaforge/sdk-server/flags";

/** `runtimeConfig.micaforge`: private, server only. */
export interface PrivateConfig {
  /** the site's ingest key, for `useMicaforgeServer()` (`NUXT_MICAFORGE_INGEST_KEY`) */
  ingestKey?: string;
  /** an API key with `manage:read`, for `useMicaforgeFlags()` (`NUXT_MICAFORGE_API_KEY`) */
  apiKey?: string;
}

/** The public half the clients need: the site and the host. */
export interface SiteConfig {
  siteKey?: string;
  host?: string;
  enabled?: boolean;
  flags?: { environment?: string };
}

/** A server client, or `undefined` when the module is off or a value is missing. */
export function serverClient(site: SiteConfig | undefined, secrets: PrivateConfig | undefined): Micaforge | undefined {
  if (!site || site.enabled === false || !site.siteKey || !site.host || !secrets || !secrets.ingestKey) return undefined;
  return new Micaforge({ host: site.host, siteId: Number(site.siteKey), ingestKey: secrets.ingestKey, flushOnExit: true });
}

/** A server flag client, or `undefined` when the module is off or a value is missing. */
export function flagsClient(site: SiteConfig | undefined, secrets: PrivateConfig | undefined): FlagsClient | undefined {
  if (!site || site.enabled === false || !site.siteKey || !site.host || !secrets || !secrets.apiKey) return undefined;
  return createFlagsClient({
    host: site.host,
    site: site.siteKey,
    apiKey: secrets.apiKey,
    environment: site.flags && site.flags.environment,
  });
}
