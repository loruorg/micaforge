import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

// The overlay is DOM code, so every test runs in jsdom, on a page that is not localhost
// (the tracker refuses to report from localhost without `debug`, and most tests want it to
// report). `@micaforge/sdk` resolves to its source rather than its build, so the tests
// drive the real tracker with no build step first.
export default defineConfig({
  resolve: {
    alias: {
      "@micaforge/sdk": fileURLToPath(new URL("../sdk/src/index.ts", import.meta.url)),
    },
  },
  test: {
    environment: "jsdom",
    environmentOptions: { jsdom: { url: "https://example.com/docs/start?ref=x" } },
    include: ["src/**/*.test.ts"],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
