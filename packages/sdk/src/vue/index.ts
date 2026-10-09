/**
 * Vue 3 plugin.
 *
 * Imports nothing from `vue`: the app object is typed structurally, so this entry
 * carries no framework weight and no version coupling.
 *
 * ```ts
 * import { createApp } from "vue";
 * import { micaforge } from "@micaforge/sdk/vue";
 *
 * createApp(App).use(micaforge, { site: 1, host: "https://micaforge.example.com" });
 * ```
 *
 * Inside a component: `inject("micaforge")`, `this.$micaforge`, or `useMicaforge()`.
 */
import { getTracker, init, type InitConfig } from "../index.js";
import type { Props, TrackerApi } from "../types.js";

/** The slice of a Vue app this plugin touches. */
export interface VueAppLike {
  config: { globalProperties: Record<string, unknown> };
  provide(key: string, value: unknown): void;
  directive(name: string, def: unknown): unknown;
}

/** The injection key, also the `app.provide` key. */
export const MICAFORGE_KEY = "micaforge";

/**
 * `v-mf` tracks a click on any element:
 * `<button v-mf="'cta_click'">` or `<button v-mf="{ event: 'cta_click', props: { plan } }">`.
 */
export const vMicaforge = {
  mounted(el: any, binding: { value: string | { event: string; props?: Props } }) {
    try {
      const v = binding.value;
      const name = typeof v == "string" ? v : v && v.event;
      if (!name) return;
      el.__mf = () => {
        try {
          getTracker()?.track(name, typeof v == "string" ? undefined : v.props);
        } catch {}
      };
      el.addEventListener("click", el.__mf);
    } catch {}
  },
  unmounted(el: any) {
    try {
      if (el.__mf) el.removeEventListener("click", el.__mf);
    } catch {}
  },
};

/** The plugin. `app.use(micaforge, config)`. */
export const micaforge = {
  install(app: VueAppLike, options: InitConfig): void {
    let tracker: TrackerApi | undefined;
    try {
      tracker = init(options);
    } catch {}
    try {
      app.config.globalProperties.$micaforge = tracker;
      app.provide(MICAFORGE_KEY, tracker);
      app.directive("mf", vMicaforge);
    } catch {}
  },
};

/** The tracker, without needing `inject`. */
export const useMicaforge = (): TrackerApi | undefined => getTracker();

export default micaforge;
export { init, getTracker, track, pageview, identify, setTraits, setProps, optIn, optOut, flush } from "../index.js";
export type { InitConfig } from "../index.js";
export type { Props, TrackerApi } from "../types.js";
