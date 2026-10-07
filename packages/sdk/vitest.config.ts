import { defineConfig } from "vitest/config";

// No jsdom: the tracker's own tests stub the browser globals by hand, which keeps them
// honest about exactly which globals the tracker touches.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
