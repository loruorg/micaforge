#!/usr/bin/env node
/**
 * Build with esbuild, as the SDK does.
 *
 *   dist/devtools.js  the script-tag build: one IIFE that mounts itself when asked
 *                     (see src/auto.ts)
 *   dist/index.js     the ESM entry, for `import { mountDevtools } from "@micaforge/devtools"`
 *
 * The tracker helpers the overlay replays (`bucket`, `maskUrl`, ...) are bundled from
 * `@micaforge/sdk`, so the script-tag build stands alone and the ESM build carries the same
 * rules the tracker runs. Declarations come from `tsc -p tsconfig.build.json`, run next.
 *
 * There is no size budget: this file is never on a production page. Its size is printed so
 * a change that makes it heavy is at least seen.
 */
import { build } from "esbuild";
import { mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const dist = join(root, "dist");

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

const common = {
  absWorkingDir: root,
  bundle: true,
  charset: "utf8",
  legalComments: "none",
  logLevel: "warning",
  platform: "browser",
  target: ["es2020", "chrome80", "firefox78", "safari14", "edge88"],
};

await build({
  ...common,
  entryPoints: ["src/auto.ts"],
  outfile: "dist/devtools.js",
  format: "iife",
  minify: true,
  banner: { js: `/*! Micaforge DevTools ${pkg.version} MIT */` },
});

await build({
  ...common,
  entryPoints: { index: "src/index.ts" },
  outdir: "dist",
  format: "esm",
});

for (const f of ["devtools.js", "index.js"]) {
  const path = join(dist, f);
  const raw = statSync(path).size;
  const gz = gzipSync(readFileSync(path), { level: 9 }).length;
  console.log(`  ${f.padEnd(12)} ${(raw / 1024).toFixed(2).padStart(7)} KB raw   ${(gz / 1024).toFixed(2).padStart(6)} KB gzipped`);
}
