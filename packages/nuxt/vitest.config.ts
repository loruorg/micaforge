import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

const src = (path: string) => fileURLToPath(new URL(path, import.meta.url));

// Unit tests, with no Nuxt app: the module runs against a hand-built Nuxt object through
// the real `@nuxt/kit`, and the runtime logic runs against the real tracker and flag client
// from the SDKs' sources (so no build step is needed first). The two files that only glue
// that logic to `#imports` are covered by the type check, not here.
export default defineConfig({
  resolve: {
    alias: [
      { find: /^@micaforge\/sdk$/, replacement: src("../sdk/src/index.ts") },
      { find: /^@micaforge\/sdk-server$/, replacement: src("../sdk-server/src/index.ts") },
      { find: /^@micaforge\/sdk-server\/flags$/, replacement: src("../sdk-server/src/flags.ts") },
    ],
  },
  test: {
    environment: "jsdom",
    environmentOptions: { jsdom: { url: "https://example.com/" } },
    include: ["src/**/*.test.ts"],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
