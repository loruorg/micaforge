/**
 * Which files a scan reads.
 *
 * The answer has to match what the developer thinks of as "the code": what git tracks
 * or would track, and not the build output, the dependencies or the generated bundles
 * next to it. A scan that counted `dist/app.min.js` would report every event twice and
 * a wall of names a minifier made.
 *
 * Inside a git work tree the list comes from `git ls-files --cached --others
 * --exclude-standard`, which is git's own reading of every `.gitignore`, the global
 * excludes file and `.git/info/exclude`: the one implementation that cannot disagree
 * with git. Outside one (an unpacked archive, a CI checkout without `.git`), the tree
 * is walked and each directory's `.gitignore` is read with the common rules: comments,
 * `!` negation, a leading `/` to anchor, a trailing `/` for directories, `*`, `?` and
 * `**`. Either way a short list of build and dependency directories is always skipped.
 */
import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

/** The files the scanner knows how to read. */
export const EXTENSIONS = [
  ".js",
  ".jsx",
  ".mjs",
  ".cjs",
  ".ts",
  ".tsx",
  ".mts",
  ".cts",
  ".vue",
  ".svelte",
  ".astro",
  ".html",
  ".htm",
] as const;

/**
 * Always skipped, gitignored or not: dependencies, build output, and files a tool wrote.
 * `--include` cannot bring these back, because nothing in them is code anyone edits.
 */
export const DEFAULT_EXCLUDES = [
  "**/node_modules/**",
  "**/.git/**",
  "**/dist/**",
  "**/build/**",
  "**/.next/**",
  "**/.nuxt/**",
  "**/.output/**",
  "**/.svelte-kit/**",
  "**/.astro/**",
  "**/.vercel/**",
  "**/.turbo/**",
  "**/coverage/**",
  "**/*.min.js",
  "**/*.d.ts",
];

/** How to choose the files. */
export interface FileOptions {
  /** Only paths matching one of these. Default: every supported file. */
  include?: string[];
  /** Never paths matching one of these, on top of {@link DEFAULT_EXCLUDES}. */
  exclude?: string[];
  /** Honour `.gitignore`. Default `true`. */
  gitignore?: boolean;
  /**
   * Ask git for the list when the root is in a work tree. Default `true`. Turned off by
   * the tests that exercise the built-in `.gitignore` reader.
   */
  git?: boolean;
}

/**
 * A glob as a regular expression over a `/`-separated relative path.
 *
 * `**` crosses directories, `*` and `?` do not, `{a,b}` is a choice and `[...]` a class.
 */
export function globToRegExp(glob: string): RegExp {
  let out = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i] as string;
    if (c === "*") {
      if (glob[i + 1] === "*") {
        const slashAfter = glob[i + 2] === "/";
        out += slashAfter ? "(?:.*/)?" : ".*";
        i += slashAfter ? 2 : 1;
      } else out += "[^/]*";
    } else if (c === "?") out += "[^/]";
    else if (c === "{") {
      const close = glob.indexOf("}", i);
      if (close === -1) out += "\\{";
      else {
        const options = glob.slice(i + 1, close).split(",").map((part) => globToRegExp(part).source.slice(1, -1));
        out += `(?:${options.join("|")})`;
        i = close;
      }
    } else if (c === "[") {
      const close = glob.indexOf("]", i);
      if (close === -1) out += "\\[";
      else {
        out += `[${glob.slice(i + 1, close).replace(/^!/, "^").replace(/\\/g, "\\\\")}]`;
        i = close;
      }
    } else out += c.replace(/[.+^${}()|\\]/g, "\\$&");
  }
  return new RegExp(`^${out}$`);
}

/**
 * Does `path` match one of `globs`?
 *
 * A glob without a `/` matches a file or directory name at any depth (`*.test.ts`,
 * `legacy`), as in `.gitignore`. A glob that names a directory matches everything in it,
 * so `--include src` and `--exclude src/legacy` mean what they say.
 */
export function matchesAny(path: string, globs: string[]): boolean {
  return globs.some((glob) => {
    const clean = glob.replace(/^\.\//, "").replace(/\/+$/, "");
    if (!clean) return false;
    const re = globToRegExp(clean.includes("/") ? clean : `**/${clean}`);
    if (re.test(path)) return true;
    // A directory glob: any parent of the path may match.
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++) {
      if (re.test(parts.slice(0, i).join("/"))) return true;
    }
    return false;
  });
}

/** One rule from a `.gitignore`. */
export interface IgnoreRule {
  negate: boolean;
  dirOnly: boolean;
  re: RegExp;
  /** The directory the `.gitignore` sits in, relative to the root; "" for the root. */
  base: string;
}

/** Read a `.gitignore` into rules that apply below `base`. */
export function parseGitignore(text: string, base: string): IgnoreRule[] {
  const rules: IgnoreRule[] = [];
  for (const rawLine of text.split(/\r?\n/)) {
    let line = rawLine.replace(/(?<!\\)\s+$/, "");
    if (!line || line.startsWith("#")) continue;
    const negate = line.startsWith("!");
    if (negate) line = line.slice(1);
    line = line.replace(/^\\([#!])/, "$1");
    const dirOnly = line.endsWith("/");
    if (dirOnly) line = line.slice(0, -1);
    if (!line) continue;
    const anchored = line.includes("/");
    line = line.replace(/^\//, "");
    rules.push({ negate, dirOnly, base, re: globToRegExp(anchored ? line : `**/${line}`) });
  }
  return rules;
}

/** Git's rule: the last rule that matches decides. */
function isIgnored(path: string, isDir: boolean, rules: IgnoreRule[]): boolean {
  let ignored = false;
  for (const rule of rules) {
    if (rule.dirOnly && !isDir) continue;
    if (rule.base && !path.startsWith(`${rule.base}/`)) continue;
    const local = rule.base ? path.slice(rule.base.length + 1) : path;
    if (rule.re.test(local)) ignored = !rule.negate;
  }
  return ignored;
}

/** The candidate files, relative to `root`, from git when it can answer. */
function fromGit(root: string): string[] | null {
  try {
    const out = execFileSync("git", ["ls-files", "--cached", "--others", "--exclude-standard", "-z"], {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
      stdio: ["ignore", "pipe", "ignore"],
    });
    // `--cached` also lists a tracked file deleted from the work tree.
    return out.split("\0").filter((path) => path && existsSync(join(root, path)));
  } catch {
    return null;
  }
}

/** Walk the tree, reading each directory's `.gitignore` when asked to. */
function walk(root: string, gitignore: boolean): string[] {
  const out: string[] = [];
  const visit = (dir: string, inherited: IgnoreRule[]) => {
    const rel = relative(root, dir).split(sep).join("/");
    let rules = inherited;
    const ignoreFile = join(dir, ".gitignore");
    if (gitignore && existsSync(ignoreFile)) {
      rules = [...inherited, ...parseGitignore(readFileSync(ignoreFile, "utf8"), rel)];
    }
    let entries: string[];
    try {
      entries = readdirSync(dir).sort();
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = join(dir, entry);
      const path = rel ? `${rel}/${entry}` : entry;
      let stat;
      try {
        stat = statSync(full);
      } catch {
        continue;
      }
      if (stat.isDirectory()) {
        if (entry === ".git" || entry === "node_modules") continue;
        // Git cannot re-include a file inside an ignored directory, so neither does this.
        if (gitignore && isIgnored(path, true, rules)) continue;
        visit(full, rules);
      } else if (stat.isFile()) {
        if (gitignore && isIgnored(path, false, rules)) continue;
        out.push(path);
      }
    }
  };
  visit(root, []);
  return out;
}

/** Every file a scan of `root` reads, relative and `/`-separated, sorted. */
export function listFiles(root: string, options: FileOptions = {}): string[] {
  const gitignore = options.gitignore !== false;
  const candidates = (gitignore && options.git !== false ? fromGit(root) : null) ?? walk(root, gitignore);
  const include = options.include?.length ? options.include : null;
  const exclude = [...DEFAULT_EXCLUDES, ...(options.exclude ?? [])];
  return candidates
    .map((path) => path.split(sep).join("/"))
    .filter((path) => EXTENSIONS.some((ext) => path.toLowerCase().endsWith(ext)))
    .filter((path) => !matchesAny(path, exclude))
    .filter((path) => !include || matchesAny(path, include))
    .sort();
}
