/**
 * The tracker tag, on the server render and on the client alike.
 *
 * Built from the runtime config rather than baked into `app.head` at build time, so
 * `NUXT_PUBLIC_MICAFORGE_SITE_KEY` and `NUXT_PUBLIC_MICAFORGE_HOST` set on the server change
 * the tag without a rebuild. `key: "micaforge"` keeps it to one tag through hydration.
 */
import { defineNuxtPlugin, useHead, useRuntimeConfig } from "#imports";
import { trackerScript, type PublicConfig } from "./core.js";

export default defineNuxtPlugin(() => {
  const config = useRuntimeConfig().public.micaforge as PublicConfig | undefined;
  const script = config && trackerScript(config);
  if (script) useHead({ script: [script] });
});
