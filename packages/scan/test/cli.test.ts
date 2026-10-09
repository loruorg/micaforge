import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterAll, describe, expect, it, vi } from "vitest";

import { main, type Io } from "../src/cli.js";
import type { ScanReport } from "../src/model.js";

const fixtures = fileURLToPath(new URL("./fixtures", import.meta.url));
const made: string[] = [];
afterAll(() => {
  for (const dir of made) rmSync(dir, { recursive: true, force: true });
});

const KEY = "mfg_cli_test_key_never_printed";

/** Run the command in `cwd` and capture what it printed. */
async function run(argv: string[], options: Partial<Io> = {}) {
  let stdout = "";
  let stderr = "";
  const code = await main(argv, {
    stdout: (t) => {
      stdout += t;
    },
    stderr: (t) => {
      stderr += t;
    },
    env: {},
    cwd: fixtures,
    isTTY: false,
    ...options,
  });
  return { code, stdout, stderr };
}

/** A stats API stub answering `GET /api/stats/events`. */
function statsApi(status: number, body: unknown) {
  return vi.fn(async (_url: string | URL | Request, _init?: RequestInit) =>
    new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } }),
  );
}

describe("micaforge-scan", () => {
  it("prints JSON when stdout is not a terminal", async () => {
    const { code, stdout } = await run(["react-app"]);
    expect(code).toBe(0);
    const report = JSON.parse(stdout) as ScanReport;
    expect(report.events.map((e) => e.name)).toContain("signup");
  });

  it("prints a table in a terminal and Markdown on request", async () => {
    const table = await run(["vue-app"], { isTTY: true, env: { NO_COLOR: "1" } });
    expect(table.stdout).toMatch(/^7 events in 2 files/);
    expect(table.stdout).not.toContain("\u001b[");
    const markdown = await run(["svelte-app", "--markdown"]);
    expect(markdown.stdout).toContain("| `svelte_cta` | src/Signup.svelte:10 | color: string |");
  });

  it("fails a CI run on errors, and on warnings with --strict", async () => {
    expect((await run(["html-site", "--ci"])).code).toBe(1);
    expect((await run(["vue-app", "--ci"])).code).toBe(0);
    expect((await run(["react-app", "--ci"])).code).toBe(0);
    expect((await run(["react-app", "--ci", "--strict"])).code).toBe(1);
    expect((await run(["react-app", "--strict"])).code).toBe(0);
  });

  it("writes the report and the typed events to files", async () => {
    const dir = mkdtempSync(join(tmpdir(), "micaforge-cli-"));
    made.push(dir);
    const out = join(dir, "plan.md");
    const types = join(dir, "nested/events.ts");
    const { code, stdout, stderr } = await run(["react-app", "--format=markdown", "-o", out, "--types", types]);
    expect(code).toBe(0);
    expect(stdout).toBe("");
    expect(readFileSync(out, "utf8")).toMatch(/^# Analytics events/);
    expect(existsSync(types)).toBe(true);
    expect(readFileSync(types, "utf8")).toContain("export interface MicaforgeEvents");
    expect(stderr).toContain("Wrote typed events for 10 events");
  });

  it("compares with the site's events through the stats API", async () => {
    const fetchStub = statsApi(200, {
      data: [
        { name: "signup", count: 120, visitors: 100 },
        { name: "step_2", count: 40, visitors: 30 },
        { name: "old_banner_click", count: 7, visitors: 5 },
      ],
      meta: {},
    });
    const { code, stdout, stderr } = await run(
      ["react-app", "--site", "3", "--host", "https://analytics.example.com/", "--range", "7d"],
      { env: { MICAFORGE_API_KEY: KEY }, fetch: fetchStub as unknown as typeof fetch },
    );
    expect(code, stderr).toBe(0);
    const [url, init] = fetchStub.mock.calls[0] ?? [];
    expect(String(url)).toBe("https://analytics.example.com/api/stats/events?site=3&range=7d&limit=10000");
    expect((init?.headers as Record<string, string>).authorization).toBe(`Bearer ${KEY}`);

    const report = JSON.parse(stdout) as ScanReport;
    // step_2 is accounted for by the computed `step_${step}`.
    expect(report.comparison?.notInCode).toEqual([{ name: "old_banner_click", count: 7 }]);
    expect(report.comparison?.notReceived).toContain("cta_click");
    expect(report.comparison?.notReceived).not.toContain("signup");
    expect(report.findings.some((f) => f.check === "not-in-code" && f.event === "old_banner_click")).toBe(true);
    expect(stdout).not.toContain(KEY);
  });

  it("says why a comparison failed, and never prints the key", async () => {
    const fetchStub = statsApi(403, { error: { code: "forbidden", message: "this key lacks stats:read" } });
    const { code, stderr } = await run(["react-app", "--site", "3", "--host", "analytics.example.com", "--api-key", KEY], {
      fetch: fetchStub as unknown as typeof fetch,
    });
    expect(code).toBe(2);
    expect(stderr).toContain("403: this key lacks stats:read");
    expect(stderr).toContain("stats:read scope");
    expect(stderr).not.toContain(KEY);
  });

  it("refuses a comparison it cannot make", async () => {
    expect((await run(["react-app", "--site", "3"])).stderr).toContain("needs --api-key");
    expect((await run(["react-app", "--site", "abc", "--api-key", KEY, "--host", "x"])).code).toBe(2);
  });

  it("explains a usage mistake", async () => {
    expect(await run(["--format", "xml"])).toMatchObject({ code: 2, stderr: expect.stringContaining("table, json or markdown") });
    expect(await run(["--wat"])).toMatchObject({ code: 2, stderr: expect.stringContaining("unknown option --wat") });
    expect(await run(["--types"])).toMatchObject({ code: 2, stderr: expect.stringContaining("--types needs a value") });
    expect(await run(["no-such-dir"])).toMatchObject({ code: 2, stderr: expect.stringContaining("is not a directory") });
    expect((await run(["--help"])).stdout).toContain("micaforge-scan [directory] [options]");
    expect((await run(["-v"])).stdout).toBe("0.1.0\n");
  });
});
