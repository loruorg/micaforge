/**
 * Route changes as pageviews, from the Nuxt router.
 *
 * The tag carries `data-spa="false"` whenever this runs (see `trackerScript`), so the
 * tracker's own history listener is off and each navigation is reported once, here, after
 * the router has finished it.
 */
import { defineNuxtPlugin, useRouter, useRuntimeConfig } from "#imports";
import { trackRoutes, type PublicConfig } from "./core.js";

export default defineNuxtPlugin(() => {
  const config = useRuntimeConfig().public.micaforge as PublicConfig | undefined;
  if (!config || !config.enabled || !config.routeTracking) return;
  trackRoutes(useRouter(), window, !!(config.tracker && config.tracker.hash));
});
