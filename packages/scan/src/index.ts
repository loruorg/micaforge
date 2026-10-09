/**
 * `@micaforge/scan`: find every analytics event in a codebase.
 *
 * ```ts
 * import { scan, formatReport } from "@micaforge/scan";
 *
 * const report = await scan({ root: "." });
 * process.stdout.write(formatReport(report, "markdown"));
 * ```
 *
 * A scan reads the files (see `files.ts` for which), extracts every call site that sends
 * an event (`js.ts` for scripts, `markup.ts` for templates), groups them by name, runs the
 * checks (`checks.ts`), and, when a site is given, compares the names with what the site
 * received (`compare.ts`). It never runs the code it reads and never writes a file
 * itself; `--types` and `--output` are the CLI's.
 *
 * @packageDocumentation
 */
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, join, relative, resolve, sep } from "node:path";

import { runChecks } from "./checks.js";
import { compareWithSite, type CompareOptions } from "./compare.js";
import { listFiles, type FileOptions } from "./files.js";
import {
  collectBindings,
  createContext,
  exportedStrings,
  findCalls,
  parseScript,
  type ModuleRef,
} from "./js.js";
import { extractMarkup, type MarkupKind } from "./markup.js";
import type { EventSummary, Finding, Occurrence, PropType, ScanReport } from "./model.js";

export * from "./model.js";
export { compareWithSite, patternToRegExp } from "./compare.js";
export type { CompareOptions } from "./compare.js";
export { DEFAULT_EXCLUDES, EXTENSIONS, globToRegExp, listFiles, matchesAny } from "./files.js";
export type { FileOptions } from "./files.js";
export { editDistance, nearDuplicates, normaliseName, MAX_NAME_BYTES } from "./checks.js";
export { formatReport, tally } from "./format.js";
export type { Format } from "./format.js";
export { generateTypes } from "./types-gen.js";

/** Largest file read. A bigger one is a bundle or a data file, not code anyone edits. */
const MAX_FILE_BYTES = 2 * 1024 * 1024;

/** What to scan. */
export interface ScanOptions extends FileOptions {
  /** The directory to scan. Default: the working directory. */
  root?: string;
  /** Compare with a live site. Leave out to scan offline. */
  compare?: CompareOptions;
}

/** What one file holds. */
export interface SourceScan {
  occurrences: Occurrence[];
  errors: { line: number; message: string }[];
  /** Call sites whose name is a constant exported by another module of the project. */
  refs: Map<Occurrence, ModuleRef>;
}

/** Read one file's call sites. */
export function scanSource(file: string, text: string): SourceScan {
  const ctx = createContext(file);
  const ext = extname(file).toLowerCase();
  const markup: Record<string, MarkupKind> = { ".vue": "vue", ".svelte": "svelte", ".astro": "astro", ".html": "html", ".htm": "html" };
  const kind = markup[ext];
  if (kind) extractMarkup(ctx, text, kind);
  else {
    const typescript = /^\.(?:ts|tsx|mts|cts)$/.test(ext);
    const jsx = ext !== ".ts" && ext !== ".mts" && ext !== ".cts";
    const ast = parseScript(ctx, text, typescript, jsx);
    if (ast) {
      collectBindings(ctx, ast);
      findCalls(ctx, ast, text);
    }
  }
  return { occurrences: ctx.occurrences, errors: ctx.errors, refs: ctx.refs };
}

/** The extensions an import without one may mean, in the order bundlers try them. */
const RESOLVE_EXTENSIONS = [".ts", ".tsx", ".mts", ".js", ".jsx", ".mjs", ".cjs", ".cts"];

/**
 * The file a relative import names, or `null`: `./names` may be `names.ts` or
 * `names/index.ts`, and a TypeScript file imports `./names.js` meaning `names.ts`.
 */
export function resolveModule(fromFile: string, source: string): string | null {
  const base = resolve(dirname(fromFile), source);
  const stem = base.replace(/\.(?:js|jsx|mjs|cjs)$/, "");
  const candidates = [
    base,
    ...RESOLVE_EXTENSIONS.map((ext) => stem + ext),
    ...RESOLVE_EXTENSIONS.map((ext) => join(base, `index${ext}`)),
  ];
  for (const candidate of candidates) {
    try {
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    } catch {
      continue;
    }
  }
  return null;
}

/**
 * Give a name to every call site that reads a constant from another module.
 *
 * Done after the scan and only for the modules such a call site names, so a project
 * pays for this only where it uses a constants module, and each module is parsed once.
 * A module outside the scanned root is not read.
 */
function resolveRefs(root: string, refs: Map<Occurrence, ModuleRef>): void {
  const exportsByFile = new Map<string, Map<string, string>>();
  const exportsOf = (path: string): Map<string, string> => {
    const cached = exportsByFile.get(path);
    if (cached) return cached;
    let found = new Map<string, string>();
    try {
      const ext = extname(path).toLowerCase();
      const typescript = /^\.(?:ts|tsx|mts|cts)$/.test(ext);
      const ast = parseScript(createContext(path), readFileSync(path, "utf8"), typescript, ext !== ".ts");
      if (ast) found = exportedStrings(ast);
    } catch {
      found = new Map();
    }
    exportsByFile.set(path, found);
    return found;
  };
  for (const [occurrence, ref] of refs) {
    const target = resolveModule(join(root, occurrence.file), ref.source);
    if (!target || relative(root, target).split(sep).includes("..")) continue;
    const value = exportsOf(target).get(ref.member ? `${ref.name}.${ref.member}` : ref.name);
    if (value === undefined) continue;
    occurrence.name = value;
    delete occurrence.pattern;
    delete occurrence.expression;
  }
}

/** Group call sites by name, with the property keys and types seen for each. */
export function summarise(occurrences: Occurrence[]): { events: EventSummary[]; dynamic: Occurrence[] } {
  const byName = new Map<string, Occurrence[]>();
  const dynamic: Occurrence[] = [];
  const order = (a: Occurrence, b: Occurrence) => a.file.localeCompare(b.file) || a.line - b.line || a.column - b.column;
  for (const o of [...occurrences].sort(order)) {
    if (o.name === null) dynamic.push(o);
    else byName.set(o.name, [...(byName.get(o.name) ?? []), o]);
  }
  const events: EventSummary[] = [...byName.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([name, list]) => {
      const keys = new Map<string, { types: Set<PropType>; count: number }>();
      for (const o of list) {
        for (const p of o.props) {
          const entry = keys.get(p.key) ?? { types: new Set<PropType>(), count: 0 };
          entry.types.add(p.type);
          entry.count++;
          keys.set(p.key, entry);
        }
      }
      return {
        name,
        occurrences: list,
        props: [...keys.entries()]
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([key, v]) => ({ key, types: [...v.types].sort(), everywhere: v.count >= list.length })),
        propsDynamic: list.some((o) => o.propsDynamic),
      };
    });
  return { events, dynamic };
}

/** Scan a codebase. Rejects only when the comparison with the site fails. */
export async function scan(options: ScanOptions = {}): Promise<ScanReport> {
  const root = resolve(options.root ?? ".");
  const files = listFiles(root, options);
  const occurrences: Occurrence[] = [];
  const refs = new Map<Occurrence, ModuleRef>();
  const findings: Finding[] = [];
  let read = 0;

  for (const file of files) {
    let text: string;
    try {
      const buffer = readFileSync(join(root, file));
      if (buffer.length > MAX_FILE_BYTES) continue;
      text = buffer.toString("utf8");
    } catch {
      continue;
    }
    read++;
    const result = scanSource(file, text);
    occurrences.push(...result.occurrences);
    for (const [occurrence, ref] of result.refs) refs.set(occurrence, ref);
    for (const error of result.errors) {
      findings.push({
        check: "parse-error",
        severity: "warning",
        file,
        line: error.line,
        message: `Part of this file could not be read, so events in it may be missing: ${error.message}`,
      });
    }
  }

  resolveRefs(root, refs);
  const { events, dynamic } = summarise(occurrences);
  findings.push(...runChecks(events, dynamic));

  const report: ScanReport = { root, files: read, events, dynamic, findings };
  if (options.compare) {
    const comparison = await compareWithSite(
      events.map((e) => e.name),
      dynamic.flatMap((o) => (o.pattern ? [o.pattern] : [])),
      options.compare,
    );
    report.comparison = comparison;
    for (const name of comparison.notReceived) {
      const first = events.find((e) => e.name.trim() === name)?.occurrences[0];
      findings.push({
        check: "not-received",
        severity: "warning",
        event: name,
        ...(first ? { file: first.file, line: first.line } : {}),
        message: `"${name}" is in the code but site ${comparison.site} received none in ${comparison.range}. It may not be deployed, or the call never runs.`,
      });
    }
    const opaque = dynamic.filter((o) => !o.pattern).length;
    for (const row of comparison.notInCode) {
      findings.push({
        check: "not-in-code",
        severity: opaque ? "info" : "warning",
        event: row.name,
        message: `Site ${comparison.site} received "${row.name}" ${row.count} times in ${comparison.range}, and no call in the code sends it${
          opaque ? ` (unless one of the ${opaque} computed names does)` : ""
        }.`,
      });
    }
  }

  const rank = { error: 0, warning: 1, info: 2 } as const;
  report.findings = findings.sort(
    (a, b) => rank[a.severity] - rank[b.severity] || (a.file ?? "").localeCompare(b.file ?? "") || (a.line ?? 0) - (b.line ?? 0),
  );
  return report;
}
