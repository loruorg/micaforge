import { defineConfig } from "vitest/config";

// Node environment: everything in this package is server-side by definition, and the
// HTTP transport is stubbed per test rather than mocked globally.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
