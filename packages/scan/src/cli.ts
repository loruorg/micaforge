/**
 * The `micaforge-scan` command.
 *
 * `main` takes its arguments, its streams, its environment and its transport as
 * parameters and returns the exit code, so the tests run the real command without a
 * child process. `bin.ts` is the three lines that hand it the real ones.
 *
 * Exit codes: `0` done; `1` with `--ci` when there are errors (or warnings, with
 * `--strict`); `2` a usage error, an unreadable directory, or a failed comparison.
 */
import { existsSync, mkdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

import { formatReport, generateTypes, scan, tally, type Format } from "./index.js";

/** The package version, printed by `--version`. */
export const VERSION = "0.1.0";

/** What the command can touch. */
export interface Io {
  stdout: (text: string) => void;
  stderr: (text: string) => void;
  env: Record<string, string | undefined>;
  cwd: string;
  /** Colour and the table format by default. */
  isTTY: boolean;
  fetch?: typeof globalThis.fetch;
}

const HELP = `micaforge-scan — find every analytics event in a codebase

Usage
  micaforge-scan [directory] [options]

It finds track() calls from @micaforge/sdk and its React, Vue, Svelte and Next entries,
window.micaforge and the micaforge("event", …) queue, useMicaforge().track (the React and
Vue hooks and the Nuxt composable), @micaforge/sdk-server's micaforge.track({ name }),
<TrackClick event>, v-mf, use:trackClick, and data-track / data-track-* attributes, in
.js .jsx .ts .tsx .mjs .cjs .vue .svelte .astro and .html files. It respects .gitignore.

Output
  --format <table|json|markdown>   Default: table in a terminal, json otherwise.
  --json, --markdown               Short forms of --format.
  --output <file>                  Write the report to a file instead of stdout.
  --types <file>                   Write a typed track() for @micaforge/sdk to <file>.

Files
  --include <glob>                 Only matching paths. Repeatable.
  --exclude <glob>                 Skip matching paths. Repeatable.
  --no-gitignore                   Read ignored files too.

CI
  --ci                             Exit 1 when there are errors.
  --strict                         With --ci, warnings fail the run too.

Compare with a site (API key scope: stats:read)
  --site <id>                      Numeric site id. Env MICAFORGE_SITE_ID.
  --api-key <key>                  Env MICAFORGE_API_KEY. Prefer the environment.
  --host <url>                     Env MICAFORGE_HOST.
  --range <range>                  24h, 7d, 30d, 90d, 12mo… Default 30d.

  -h, --help                       This text.
  -v, --version                    The version.

Examples
  micaforge-scan
  micaforge-scan src --ci --strict
  micaforge-scan --markdown --output tracking-plan.md
  micaforge-scan --types src/analytics/events.ts
  MICAFORGE_API_KEY=mfg_… micaforge-scan --site 3 --host https://analytics.example.com
`;

/** Parsed arguments. */
interface Args {
  root: string;
  format?: Format;
  output?: string;
  types?: string;
  include: string[];
  exclude: string[];
  gitignore: boolean;
  ci: boolean;
  strict: boolean;
  site?: string;
  apiKey?: string;
  host?: string;
  range: string;
  help: boolean;
  version: boolean;
}

/** A usage mistake: printed with a pointer to `--help`, exit code 2. */
class UsageError extends Error {}

/** Read the arguments. Both `--flag value` and `--flag=value` are accepted. */
function parseArgs(argv: string[]): Args {
  const args: Args = { root: ".", include: [], exclude: [], gitignore: true, ci: false, strict: false, range: "30d", help: false, version: false };
  const positional: string[] = [];
  for (let i = 0; i < argv.length; i++) {
    let arg = argv[i] as string;
    let inline: string | undefined;
    const eq = arg.indexOf("=");
    if (arg.startsWith("--") && eq !== -1) {
      inline = arg.slice(eq + 1);
      arg = arg.slice(0, eq);
    }
    const value = (): string => {
      const v = inline ?? argv[++i];
      if (v === undefined || (inline === undefined && v.startsWith("-"))) throw new UsageError(`${arg} needs a value`);
      return v;
    };
    switch (arg) {
      case "-h":
      case "--help":
        args.help = true;
        break;
      case "-v":
      case "--version":
        args.version = true;
        break;
      case "--format": {
        const f = value();
        if (f !== "table" && f !== "json" && f !== "markdown") throw new UsageError(`--format must be table, json or markdown, not ${f}`);
        args.format = f;
        break;
      }
      case "--json":
        args.format = "json";
        break;
      case "--markdown":
      case "--md":
        args.format = "markdown";
        break;
      case "--output":
      case "-o":
        args.output = value();
        break;
      case "--types":
        args.types = value();
        break;
      case "--include":
        args.include.push(value());
        break;
      case "--exclude":
        args.exclude.push(value());
        break;
      case "--no-gitignore":
        args.gitignore = false;
        break;
      case "--ci":
        args.ci = true;
        break;
      case "--strict":
        args.strict = true;
        break;
      case "--site":
        args.site = value();
        break;
      case "--api-key":
        args.apiKey = value();
        break;
      case "--host":
        args.host = value();
        break;
      case "--range":
        args.range = value();
        break;
      default:
        if (arg.startsWith("-")) throw new UsageError(`unknown option ${arg}`);
        positional.push(arg);
    }
  }
  if (positional.length > 1) throw new UsageError("give one directory to scan");
  if (positional[0]) args.root = positional[0];
  return args;
}

/** Write a file, creating its directory. */
function write(path: string, text: string): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

/** Run the command. Never throws; every failure is a message and an exit code. */
export async function main(argv: string[], io: Io): Promise<number> {
  let args: Args;
  try {
    args = parseArgs(argv);
  } catch (error) {
    io.stderr(`micaforge-scan: ${(error as Error).message}. See --help.\n`);
    return 2;
  }
  if (args.help) {
    io.stdout(HELP);
    return 0;
  }
  if (args.version) {
    io.stdout(`${VERSION}\n`);
    return 0;
  }

  const root = resolve(io.cwd, args.root);
  if (!existsSync(root) || !statSync(root).isDirectory()) {
    io.stderr(`micaforge-scan: ${args.root} is not a directory.\n`);
    return 2;
  }

  const siteRaw = args.site ?? io.env.MICAFORGE_SITE_ID;
  const apiKey = args.apiKey ?? io.env.MICAFORGE_API_KEY;
  const host = args.host ?? io.env.MICAFORGE_HOST;
  let compare;
  if (args.site !== undefined || (siteRaw && apiKey && host)) {
    const site = Number(siteRaw);
    if (!Number.isInteger(site) || site < 1 || site > 65535) {
      io.stderr(`micaforge-scan: --site must be a site id from 1 to 65535.\n`);
      return 2;
    }
    if (!apiKey || !host) {
      io.stderr(`micaforge-scan: --site needs --api-key (or MICAFORGE_API_KEY) and --host (or MICAFORGE_HOST).\n`);
      return 2;
    }
    compare = { site, apiKey, host, range: args.range, ...(io.fetch ? { fetch: io.fetch } : {}) };
  }

  let report;
  try {
    report = await scan({
      root,
      include: args.include,
      exclude: args.exclude,
      gitignore: args.gitignore,
      ...(compare ? { compare } : {}),
    });
  } catch (error) {
    io.stderr(`micaforge-scan: the comparison with the site failed: ${(error as Error).message}\n`);
    return 2;
  }

  const format = args.format ?? (io.isTTY ? "table" : "json");
  const text = formatReport(report, format, io.isTTY && !args.output && !io.env.NO_COLOR);
  if (args.output) {
    write(resolve(io.cwd, args.output), text);
    io.stderr(`Wrote the report to ${args.output}.\n`);
  } else io.stdout(text);

  if (args.types) {
    write(resolve(io.cwd, args.types), generateTypes(report.events));
    io.stderr(`Wrote typed events for ${report.events.length} event${report.events.length === 1 ? "" : "s"} to ${args.types}.\n`);
  }

  if (!args.ci) return 0;
  const counts = tally(report.findings);
  return counts.errors > 0 || (args.strict && counts.warnings > 0) ? 1 : 0;
}
