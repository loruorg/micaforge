/**
 * `@micaforge/nuxt`: Micaforge in a Nuxt app, configured from `nuxt.config`.
 *
 * ```ts
 * export default defineNuxtConfig({
 *   modules: ["@micaforge/nuxt"],
 *   micaforge: {
 *     siteKey: 1,
 *     host: "https://analytics.example.com",
 *     plugins: ["errors", "forms"],
 *     devtools: true,
 *   },
 * });
 * ```
 *
 * What it adds:
 *
 * - **The tracker tag**, with its `data-*` attributes (plugin opt-ins included), on every
 *   server-rendered page, built at run time from `runtimeConfig.public.micaforge` so the
 *   site and host can come from `NUXT_PUBLIC_MICAFORGE_SITE_KEY` and
 *   `NUXT_PUBLIC_MICAFORGE_HOST`.
 * - **Route pageviews from the Nuxt router.** The tag is told `data-spa="false"`, so the
 *   tracker's own history listener stays off and no navigation is counted twice.
 *   `routeTracking: false` hands navigation back to the tracker instead.
 * - **`useMicaforge()` and `useFlag()`**, auto-imported, both safe in server rendering.
 * - **`useMicaforgeServer()` and `useMicaforgeFlags()`** in Nitro, from
 *   `@micaforge/sdk-server`, when `NUXT_MICAFORGE_INGEST_KEY` / `NUXT_MICAFORGE_API_KEY`
 *   are set.
 * - **The DevTools overlay** in development, with `devtools: true`.
 */
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { addImports, addPlugin, addServerImports, createResolver, defineNuxtModule, useLogger } from "@nuxt/kit";
import type { NuxtModule } from "@nuxt/schema";
import { normalisePlugins, trackerScript, type FlagOptions, type PluginName, type PublicConfig, type TrackerOptions } from "./runtime/core.js";

export type { FlagBinding, FlagOptions, MicaforgeApi, PluginName, PublicConfig, TrackerOptions } from "./runtime/core.js";

/** The overlay's settings, when `devtools` is an object. */
export interface DevtoolsModuleOptions {
  /** the event names the site is meant to send; any other name is flagged */
  knownEvents?: string[];
  /** open the panel on load rather than showing the launcher */
  open?: boolean;
  /** also load it in a production build (for a staging deploy). Default false. */
  production?: boolean;
}

/** `micaforge` in `nuxt.config`. */
export interface ModuleOptions {
  /** The numeric site id, as in the tag's `data-site`. Public; never the ingest key. */
  siteKey?: number | string;
  /** Your Micaforge server, such as `https://analytics.example.com`. */
  host?: string;
  /** Where the tracker is loaded from. Default `<host>/mf.js`; set it when you proxy the script. */
  scriptSrc?: string;
  /** Tracker plugins to turn on: `["errors", "forms"]`, or `{ vitals: false, errors: true }`. */
  plugins?: PluginName[] | Partial<Record<PluginName, boolean>>;
  /** `false` adds nothing to the page and makes every call a no-op. Default true. */
  enabled?: boolean;
  /** Report route changes from the Nuxt router. Default true. */
  routeTracking?: boolean;
  /** The DevTools overlay: `true` in development only, or its settings. Default false. */
  devtools?: boolean | DevtoolsModuleOptions;
  /** The tracker's other `data-*` settings: exclude, mask, strip-query, sample, ... */
  tracker?: TrackerOptions;
  /** `useFlag()` settings: bootstrap values, environment, context. */
  flags?: FlagOptions;
}

/** The public runtime config the options describe. */
export function publicConfig(options: ModuleOptions): PublicConfig {
  return {
    siteKey: options.siteKey == null ? "" : String(options.siteKey).trim(),
    host: String(options.host || "").trim().replace(/\/+$/, ""),
    scriptSrc: String(options.scriptSrc || "").trim(),
    enabled: options.enabled !== false,
    routeTracking: options.routeTracking !== false,
    plugins: normalisePlugins(options.plugins),
    tracker: { ...options.tracker },
    flags: { ...options.flags },
  };
}

/**
 * The overlay's inline `<script>`, or `null`. Inline rather than a file: it has to run
 * before the deferred tracker so the first pageview is seen, and a dev-only asset route is
 * more machinery than one script element in a development page.
 */
export function devtoolsScript(code: string | undefined, devtools: ModuleOptions["devtools"], dev: boolean): Record<string, string> | null {
  if (!devtools || !code) return null;
  const settings: DevtoolsModuleOptions = devtools === true ? {} : devtools;
  if (!dev && !settings.production) return null;
  const script: Record<string, string> = { key: "micaforge-devtools", innerHTML: code, "data-devtools": "", tagPriority: "critical" };
  if (settings.knownEvents && settings.knownEvents.length) script["data-events"] = settings.knownEvents.join(",");
  if (settings.open) script["data-open"] = "";
  return script;
}

/** The overlay's built script, from the installed `@micaforge/devtools`. */
function readDevtools(): string | undefined {
  try {
    return readFileSync(createRequire(import.meta.url).resolve("@micaforge/devtools/devtools.js"), "utf8");
  } catch {
    return undefined;
  }
}

const micaforge: NuxtModule<ModuleOptions> = defineNuxtModule<ModuleOptions>({
  meta: {
    name: "@micaforge/nuxt",
    configKey: "micaforge",
    compatibility: { nuxt: ">=3.13.0" },
  },
  defaults: {
    enabled: true,
    routeTracking: true,
    devtools: false,
  },
  setup(options, nuxt) {
    const logger = useLogger("micaforge");
    const { resolve } = createResolver(import.meta.url);
    const pub = publicConfig(options);

    // Values already in runtimeConfig (set by hand in nuxt.config) win over the module's.
    const runtime = nuxt.options.runtimeConfig as Record<string, any>;
    runtime.public = runtime.public || {};
    runtime.public.micaforge = { ...pub, ...runtime.public.micaforge };
    runtime.micaforge = { ingestKey: "", apiKey: "", ...runtime.micaforge };

    if (pub.enabled && !trackerScript(runtime.public.micaforge)) {
      logger.warn("micaforge: siteKey (a site id, 1-65535) and host are needed; the tracker tag is added once both are set, at build time or through NUXT_PUBLIC_MICAFORGE_SITE_KEY and NUXT_PUBLIC_MICAFORGE_HOST.");
    }

    nuxt.options.build.transpile.push(resolve("./runtime"));
    addPlugin(resolve("./runtime/plugin"));
    addPlugin({ src: resolve("./runtime/plugin.client"), mode: "client" });
    addImports([
      { name: "useMicaforge", from: resolve("./runtime/composables") },
      { name: "useFlag", from: resolve("./runtime/composables") },
    ]);
    addServerImports([
      { name: "useMicaforgeServer", from: resolve("./runtime/server/utils") },
      { name: "useMicaforgeFlags", from: resolve("./runtime/server/utils") },
    ]);

    if (options.devtools && pub.enabled) {
      const script = devtoolsScript(readDevtools(), options.devtools, nuxt.options.dev);
      if (script) {
        const head = nuxt.options.app.head as { script?: Record<string, unknown>[] };
        head.script = [script, ...(head.script || [])];
      } else if (nuxt.options.dev) {
        logger.warn("micaforge: devtools is on but @micaforge/devtools could not be read; the overlay is not added.");
      }
    }
  },
});

export default micaforge;
