import { defineConfig } from "vitest/config";

// No jsdom by default: the tracker's own tests stub the browser globals by hand, which
// keeps them honest about exactly which globals the core touches. The plugin tests
// (`src/plugins/*.test.ts`) opt into jsdom with a file comment, because a plugin's
// whole job is reading the DOM.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
