/**
 * The slice of Nuxt's virtual modules the runtime files use, for compiling this package
 * without a Nuxt app around it. In an app, `#imports` and `#app` are Nuxt's own and these
 * declarations are never seen: this file is not emitted and not published.
 */
declare module "#imports" {
  import type { Ref } from "vue";

  export function defineNuxtPlugin<T extends Record<string, unknown> | void>(
    plugin: (nuxtApp: { provide?: unknown; hook: (name: string, fn: () => void) => void }) => T | Promise<T>,
  ): unknown;
  export function useRuntimeConfig(): { public: Record<string, any>; [key: string]: any };
  export function useState<T>(key: string, init?: () => T): Ref<T>;
  export function useRouter(): { afterEach(guard: (to: unknown, from: unknown, failure?: unknown) => unknown): () => void };
  export function useHead(input: { script?: Record<string, unknown>[] }): void;
  export function onNuxtReady(callback: () => void): void;
}
