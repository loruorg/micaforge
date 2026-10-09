import { defineConfig } from "vitest/config";

// Node only: the scanner reads files and never runs the code it reads. Each test scans
// a small fixture project under `test/fixtures`, so what is asserted is what a real
// codebase of that shape would report.
export default defineConfig({
  test: {
    environment: "node",
    include: ["test/**/*.test.ts"],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
