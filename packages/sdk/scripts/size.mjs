#!/usr/bin/env node
/**
 * The tracker's size budget, enforced.
 *
 * PRODUCT.md and CONTRACT.md section 4 both state it: the core tracker is under 3 KB
 * gzipped. This script is the thing that makes that a fact rather than an intention,
 * and it exits non-zero when the budget is broken.
 *
 * The plugins the core loads on demand (`dist/p/*.js`) have budgets of their own.
 * A plugin is fetched only by the sites that turn it on, so its budget is not the
 * core's, but it is still a hard number: an optional feature that quietly grows
 * is a cost every site that turned it on pays on every page.
 */
import { readFileSync, statSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const BUDGET = 3072; // bytes, gzipped

/** Per-plugin budgets, bytes gzipped. */
const PLUGIN_BUDGETS = {
  vitals: 768,
  errors: 1024,
  frustration: 2048,
  forms: 384,
  track404: 384,
  tags: 512,
};
const root = dirname(dirname(fileURLToPath(import.meta.url)));

const measure = (file) => {
  const path = join(root, "dist", file);
  const raw = statSync(path).size;
  const gzipped = gzipSync(readFileSync(path), { level: 9 }).length;
  return { file, raw, gzipped };
};

let failed = false;
for (const file of ["mf.js", "mf.min.js"]) {
  let m;
  try {
    m = measure(file);
  } catch {
    console.error(`dist/${file} is missing. Run \`npm run build\` first.`);
    process.exit(1);
  }
  const pct = Math.round((m.gzipped / BUDGET) * 100);
  const over = m.gzipped > BUDGET;
  failed = failed || over;
  console.log(
    `${over ? "FAIL" : "ok  "}  dist/${m.file.padEnd(10)} ${String(m.raw).padStart(6)} B raw   ` +
      `${String(m.gzipped).padStart(5)} B gzipped   ${String(pct).padStart(3)}% of ${BUDGET} B ` +
      `(${over ? "+" + (m.gzipped - BUDGET) : BUDGET - m.gzipped + " to spare"})`,
  );
}

if (failed) {
  console.error("\nThe tracker is over its 3 KB gzipped budget. Cut it back before shipping.");
  process.exit(1);
}

let pluginFailed = false;
for (const [name, budget] of Object.entries(PLUGIN_BUDGETS)) {
  let m;
  try {
    m = measure(`p/${name}.js`);
  } catch {
    console.error(`dist/p/${name}.js is missing. Run \`npm run build\` first.`);
    process.exit(1);
  }
  const over = m.gzipped > budget;
  pluginFailed = pluginFailed || over;
  console.log(
    `${over ? "FAIL" : "ok  "}  dist/p/${(name + ".js").padEnd(15)} ${String(m.gzipped).padStart(5)} B gzipped   ` +
      `budget ${budget} B (${over ? "+" + (m.gzipped - budget) : budget - m.gzipped + " to spare"})`,
  );
}
if (pluginFailed) {
  console.error("\nA plugin is over its budget. Cut it back before shipping.");
  process.exit(1);
}
