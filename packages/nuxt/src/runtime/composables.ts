/**
 * `useMicaforge()` and `useFlag()`, auto-imported in every component.
 *
 * Both are safe during server rendering. `useMicaforge()` does nothing on the server
 * (there is no visitor there to report). `useFlag()` answers on the server from the
 * bootstrap values in `nuxt.config`, and the client starts from the same state, so the
 * hydrated page matches the server's HTML; the client's evaluated answer replaces it once
 * the app is ready.
 */
import { createFlags, type FlagRecord, type FlagsClient } from "@micaforge/sdk";
import { onNuxtReady, useRuntimeConfig, useState } from "#imports";
import type { Ref } from "vue";
import { bindFlag, connectFlag, createApi, normaliseBootstrap, type FlagBinding, type MicaforgeApi, type PublicConfig } from "./core.js";

/** One flag client per browser tab, shared by every `useFlag()`. */
let client: FlagsClient | undefined;

const config = (): PublicConfig | undefined => useRuntimeConfig().public.micaforge as PublicConfig | undefined;

const browser = (): any => (typeof window == "undefined" ? undefined : window);

/**
 * The shared client. Created by the first `useFlag()` on the client, which hands over the
 * shared state, so every later answer (a reload after `identify`) reaches every flag.
 */
function flagClient(c: PublicConfig, state: Ref<Record<string, FlagRecord>>): FlagsClient {
  if (!client) {
    client = createFlags({
      site: c.siteKey,
      host: c.host,
      bootstrap: c.flags.bootstrap,
      environment: c.flags.environment,
      context: c.flags.context,
      ttlMs: c.flags.ttlMs,
      stats: c.flags.stats,
    });
    client.onChange((all) => {
      state.value = { ...state.value, ...all };
    });
  }
  return client;
}

/**
 * The tracker's calls: `useMicaforge().track("signup", { plan: "pro" })`.
 *
 * `identify` also moves the flags to the identified id, so a flag rolled out to a
 * percentage of users stays with the user rather than with the browser tab.
 */
export function useMicaforge(): MicaforgeApi {
  return createApi(browser, (id) => {
    if (client) {
      client.setVisitor(id);
      client.load(true).catch(() => {});
    }
  });
}

/**
 * One feature flag, reactive: `const { enabled, variant } = useFlag("new-checkout")`.
 *
 * `fallback` is the value while nothing else answers (no bootstrap value, before the
 * network). The client counts the first read as an exposure, for the flag's screen.
 */
export function useFlag(key: string, fallback = false): FlagBinding {
  const c = config();
  const state = useState<Record<string, FlagRecord>>("micaforge:flags", () => normaliseBootstrap(c && c.flags && c.flags.bootstrap));
  const binding = bindFlag(state, key, fallback);
  if (browser() && c && c.enabled && c.siteKey && c.host) connectFlag(flagClient(c, state), state, key, onNuxtReady);
  return binding;
}
