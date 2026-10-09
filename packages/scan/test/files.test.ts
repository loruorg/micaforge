import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { afterAll, describe, expect, it } from "vitest";

import { globToRegExp, listFiles, matchesAny, parseGitignore } from "../src/files.js";
import { scan } from "../src/index.js";

/** A throwaway project, written at test time so ignored files really exist on disk. */
function project(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "micaforge-scan-"));
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, path)), { recursive: true });
    writeFileSync(join(root, path), text);
  }
  made.push(root);
  return root;
}
const made: string[] = [];
afterAll(() => {
  for (const root of made) rmSync(root, { recursive: true, force: true });
});

const TRACK = (name: string) => `import { track } from "@micaforge/sdk";\ntrack("${name}");\n`;

describe("globs", () => {
  it("treats ** as any depth and * as one segment", () => {
    expect(globToRegExp("src/**/*.ts").test("src/a/b/c.ts")).toBe(true);
    expect(globToRegExp("src/**/*.ts").test("src/c.ts")).toBe(true);
    expect(globToRegExp("src/*.ts").test("src/a/c.ts")).toBe(false);
    expect(globToRegExp("*.{vue,svelte}").test("a.svelte")).toBe(true);
    expect(globToRegExp("file?.js").test("file1.js")).toBe(true);
  });

  it("matches names at any depth and directories by their contents", () => {
    expect(matchesAny("src/a/b.test.ts", ["*.test.ts"])).toBe(true);
    expect(matchesAny("src/legacy/old.js", ["src/legacy"])).toBe(true);
    expect(matchesAny("src/legacyish/old.js", ["src/legacy"])).toBe(false);
    expect(matchesAny("app/page.tsx", ["./app/"])).toBe(true);
  });
});

describe(".gitignore", () => {
  it("reads comments, negation, anchoring and directory rules", () => {
    const rules = parseGitignore("# comment\n*.log\n/build\nsecret/\n!keep.log\n", "");
    expect(rules).toHaveLength(4);
    expect(rules.map((r) => [r.negate, r.dirOnly])).toEqual([
      [false, false],
      [false, false],
      [false, true],
      [true, false],
    ]);
  });

  it("is honoured by the built-in reader, nested files included", () => {
    const root = project({
      ".gitignore": "generated/\n*.gen.ts\n",
      "src/app.ts": TRACK("kept"),
      "src/types.gen.ts": TRACK("ignored_by_glob"),
      "generated/bundle.js": TRACK("ignored_directory"),
      "pkg/.gitignore": "local.js\n",
      "pkg/local.js": TRACK("ignored_nested"),
      "pkg/index.js": TRACK("kept_nested"),
      "node_modules/x/index.js": TRACK("dependency"),
      "build/out.js": TRACK("build_output"),
    });
    expect(listFiles(root, { git: false })).toEqual(["pkg/index.js", "src/app.ts"]);
    expect(listFiles(root, { git: false, gitignore: false })).toEqual([
      "generated/bundle.js",
      "pkg/index.js",
      "pkg/local.js",
      "src/app.ts",
      "src/types.gen.ts",
    ]);
  });

  it("asks git when the root is a work tree, and falls back when it is not", () => {
    // A temporary directory is not a work tree, so this is the fallback, with the same answer.
    const root = project({ ".gitignore": "skip.js\n", "a.js": TRACK("a"), "skip.js": TRACK("b") });
    expect(listFiles(root)).toEqual(["a.js"]);
  });

  it("narrows with --include and --exclude", async () => {
    const root = project({
      "src/a.ts": TRACK("in_src"),
      "src/a.test.ts": TRACK("in_test"),
      "scripts/b.js": TRACK("in_scripts"),
    });
    expect(listFiles(root, { git: false, include: ["src"] })).toEqual(["src/a.test.ts", "src/a.ts"]);
    expect(listFiles(root, { git: false, exclude: ["*.test.ts"] })).toEqual(["scripts/b.js", "src/a.ts"]);
    const report = await scan({ root, git: false, include: ["src"], exclude: ["*.test.ts"] });
    expect(report.events.map((e) => e.name)).toEqual(["in_src"]);
  });
});
