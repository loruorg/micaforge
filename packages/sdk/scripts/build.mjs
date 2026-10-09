#!/usr/bin/env node
/**
 * Build with esbuild and nothing else.
 *
 *   dist/mf.js      the tracker, IIFE, minified. This is what GET /mf.js serves
 *   dist/mf.min.js  the alias GET /mf.min.js serves
 *   dist/p/*.js     the plugins the tracker loads on demand, served at
 *                   GET /api/track/p/<name>.js
 *   dist/index.js   the programmatic SDK (ESM)
 *   dist/vue.js  dist/svelte.js
 *   dist/react.js  dist/next.js   built apart so they can carry "use client"
 *
 * Declarations come from `tsc -p tsconfig.build.json`, which the build script runs next.
 */
import { build } from "esbuild";
import { copyFileSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const define = { __MF_VERSION__: JSON.stringify(pkg.version) };
const dist = join(root, "dist");

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

const common = {
  absWorkingDir: root,
  bundle: true,
  charset: "utf8",
  define,
  legalComments: "none",
  logLevel: "warning",
  platform: "browser",
  target: ["es2020", "chrome80", "firefox78", "safari14", "edge88"],
};

// 1. The tracker.
const tracker = await build({
  ...common,
  entryPoints: ["src/tracker/iife.ts"],
  outfile: "dist/mf.js",
  format: "iife",
  minify: true,
  write: false,
  banner: { js: `/*! Micaforge ${pkg.version} MIT */` },
});
// The bundle is one IIFE that never relies on sloppy-mode semantics, so the directive
// esbuild adds for it is 13 bytes of pure cost against a hard budget.
writeFileSync(join(dist, "mf.js"), tracker.outputFiles[0].text.replace('"use strict";', ""));
copyFileSync(join(dist, "mf.js"), join(dist, "mf.min.js"));

// 1b. The plugins. One IIFE each, because the tracker appends one <script> per
//     feature the site turned on and each must run on its own. Their names are
//     the closed set the server serves; `size.mjs` holds each to a budget.
const PLUGINS = ["vitals", "errors", "frustration", "forms", "track404", "tags"];
mkdirSync(join(dist, "p"), { recursive: true });
for (const name of PLUGINS) {
  const out = await build({
    ...common,
    entryPoints: [`src/plugins/${name}.ts`],
    format: "iife",
    minify: true,
    write: false,
    banner: { js: `/*! Micaforge ${pkg.version} ${name} MIT */` },
  });
  writeFileSync(join(dist, "p", `${name}.js`), out.outputFiles[0].text.replace('"use strict";', ""));
}

// 2. The framework-free SDK.
await build({
  ...common,
  entryPoints: {
    index: "src/index.ts",
    vue: "src/vue/index.ts",
    svelte: "src/svelte/index.ts",
  },
  outdir: "dist",
  format: "esm",
  splitting: true,
  chunkNames: "chunks/[name]-[hash]",
});

// 3. React and Next. Both entry files carry their own "use client" directive, which
//    esbuild keeps at the top of the bundle, so the published modules are client
//    modules in the Next App Router.
await build({
  ...common,
  entryPoints: { react: "src/react/index.tsx", next: "src/next/index.ts" },
  outdir: "dist",
  format: "esm",
  splitting: true,
  chunkNames: "chunks/[name]-[hash]",
  external: ["react", "react-dom", "next", "next/navigation"],
});

const kb = (n) => (n / 1024).toFixed(2) + " KB";
for (const f of ["mf.js", "index.js", "react.js", "next.js", "vue.js", "svelte.js"]) {
  const raw = statSync(join(dist, f)).size;
  const gz = gzipSync(readFileSync(join(dist, f)), { level: 9 }).length;
  console.log(`  ${f.padEnd(12)} ${kb(raw).padStart(9)} raw   ${kb(gz).padStart(9)} gzipped`);
}
console.log("  (the ESM entries share their code through dist/chunks/)");
for (const name of PLUGINS) {
  const f = join(dist, "p", `${name}.js`);
  const gz = gzipSync(readFileSync(f), { level: 9 }).length;
  console.log(`  p/${(name + ".js").padEnd(16)} ${kb(statSync(f).size).padStart(9)} raw   ${kb(gz).padStart(9)} gzipped`);
}
