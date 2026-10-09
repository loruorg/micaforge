import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { scan, scanSource, type Occurrence, type ScanReport } from "../src/index.js";

const fixture = (name: string) => fileURLToPath(new URL(`./fixtures/${name}`, import.meta.url));

const names = (report: ScanReport) => report.events.map((e) => e.name);
const event = (report: ScanReport, name: string) => {
  const found = report.events.find((e) => e.name === name);
  if (!found) throw new Error(`${name} was not found; found ${names(report).join(", ")}`);
  return found;
};
const checks = (report: ScanReport, check: string) => report.findings.filter((f) => f.check === check);

describe("a React project", async () => {
  const report = await scan({ root: fixture("react-app") });

  it("finds every way the browser SDK sends an event", () => {
    expect(names(report)).toEqual([
      "checkout_started",
      "cta_click",
      "docs_open",
      "newsletter_signup",
      "plan_chosen",
      "profile_saved",
      "search_run",
      "Sign-Up",
      "signup",
      "widget_open",
    ]);
    expect(event(report, "signup").occurrences[0]).toMatchObject({
      file: "src/App.tsx",
      line: 12,
      source: "sdk",
      props: [
        { key: "plan", type: "string", literal: "pro" },
        { key: "seats", type: "number", literal: 5 },
      ],
    });
    expect(event(report, "plan_chosen").occurrences[0]?.source).toBe("sdk"); // useTrackEvent()
    expect(event(report, "Sign-Up").occurrences[0]?.source).toBe("composable"); // useMicaforge()
    expect(event(report, "search_run").occurrences).toHaveLength(1); // EVENTS.SEARCH
    expect(event(report, "docs_open").occurrences[0]).toMatchObject({ source: "component", line: 19 });
  });

  it("reads data-track in JSX and in an HTML string", () => {
    expect(event(report, "cta_click").occurrences[0]).toMatchObject({
      source: "attribute",
      props: [{ key: "placement", type: "string", literal: "hero" }],
    });
    expect(event(report, "widget_open").occurrences[0]).toMatchObject({ file: "src/legacy.js", line: 6 });
  });

  it("reads the global tracker and the queue form", () => {
    const sites = event(report, "newsletter_signup").occurrences;
    expect(sites.map((o) => [o.line, o.source])).toEqual([
      [2, "global"],
      [5, "global"],
    ]);
  });

  it("resolves a constant exported by another module of the project", () => {
    expect(event(report, "checkout_started").occurrences[0]).toMatchObject({ file: "src/App.tsx", line: 23 });
  });

  it("reports a computed name with the glob of its fixed parts", () => {
    expect(report.dynamic).toHaveLength(1);
    expect(report.dynamic[0]).toMatchObject({ name: null, pattern: "step_*", expression: "`step_${step}`" });
    expect(checks(report, "dynamic-name")).toHaveLength(1);
  });

  it("ignores a function that is only called track, and the build output", () => {
    expect(names(report)).not.toContain("not_an_event");
    expect(names(report)).not.toContain("build_output_event");
    expect(report.files).toBe(4);
  });

  it("flags the near duplicate and the personal property", () => {
    expect(checks(report, "near-duplicate").map((f) => f.message)).toEqual([
      expect.stringContaining('"Sign-Up" and "signup" differ only in case or separators'),
    ]);
    expect(checks(report, "personal-data")).toEqual([
      expect.objectContaining({ severity: "warning", file: "src/App.tsx", line: 24 }),
    ]);
  });
});

describe("a Vue and Nuxt project", async () => {
  const report = await scan({ root: fixture("vue-app") });

  it("reads the template through the names the script set up", () => {
    expect(event(report, "plan_selected").occurrences[0]).toMatchObject({
      file: "components/Pricing.vue",
      line: 10,
      source: "composable",
      props: [{ key: "plan", literal: "pro" }],
    });
    expect(event(report, "vue_global").occurrences[0]).toMatchObject({ source: "sdk", line: 14 });
  });

  it("reads v-mf in both its forms and a bound data-track", () => {
    expect(event(report, "pricing_faq_open").occurrences[0]?.source).toBe("directive");
    expect(event(report, "contact_sales").occurrences[0]?.props).toEqual([
      { key: "size", type: "string", literal: "enterprise" },
    ]);
    expect(event(report, "outbound_docs").occurrences[0]).toMatchObject({
      source: "attribute",
      props: [{ key: "target", literal: "docs" }],
    });
  });

  it("matches the Nuxt composable by name, with no import", () => {
    expect(event(report, "nuxt_cta").occurrences[0]).toMatchObject({ file: "pages/index.vue", line: 6, source: "composable" });
    expect(event(report, "nuxt_page_ready").occurrences[0]?.line).toBe(9);
  });

  it("stays quiet about template syntax that is not JavaScript", () => {
    expect(checks(report, "parse-error")).toEqual([]);
    expect(report.findings).toEqual([]);
  });
});

describe("a Svelte component", async () => {
  const report = await scan({ root: fixture("svelte-app") });

  it("reads the script, use:trackClick, the store and data-track={…}", () => {
    expect(names(report)).toEqual(["svelte_cta", "svelte_signup_done", "svelte_simple", "svelte_store", "svelte_tag"]);
    expect(event(report, "svelte_cta").occurrences[0]).toMatchObject({
      source: "directive",
      line: 10,
      props: [{ key: "color", literal: "blue" }],
    });
    expect(event(report, "svelte_store").occurrences[0]?.source).toBe("sdk");
    expect(event(report, "svelte_tag").occurrences[0]?.props).toEqual([
      { key: "variant", type: "string", literal: "b" },
    ]);
  });
});

describe("an Astro page", async () => {
  const report = await scan({ root: fixture("astro-site") });

  it("reads the frontmatter, the markup and the client script", () => {
    expect(event(report, "astro_ssr_render").occurrences[0]).toMatchObject({ source: "server", line: 5 });
    expect(event(report, "astro_cta").occurrences[0]).toMatchObject({ source: "attribute", line: 11 });
    expect(event(report, "astro_client_click").occurrences[0]).toMatchObject({ source: "sdk", line: 15 });
    expect(report.dynamic.map((o) => o.expression)).toEqual(["i.event"]);
  });
});

describe("a plain HTML page", async () => {
  const report = await scan({ root: fixture("html-site") });

  it("reads the queue, inline handlers and tags, and skips data and comments", () => {
    expect(names(report)).toEqual(["", "html_buy", "html_inline", "html_queued"]);
    expect(event(report, "html_queued").occurrences[0]).toMatchObject({ source: "global", line: 7 });
    expect(event(report, "html_inline").occurrences[0]).toMatchObject({ source: "global", line: 14 });
    expect(names(report)).not.toContain("commented_out");
  });

  it("refuses an empty data-track and a credential in a property", () => {
    const errors = report.findings.filter((f) => f.severity === "error");
    expect(errors.map((f) => f.message)).toEqual([
      expect.stringContaining('"token" holds what looks like a credential'),
      expect.stringContaining("data-track has no value"),
    ]);
  });
});

describe("a server project", async () => {
  const report = await scan({ root: fixture("server-app") });

  it("reads the object form, pageview names and event batches", () => {
    expect(event(report, "purchase").occurrences[0]).toMatchObject({ source: "server", file: "src/server.ts", line: 10 });
    expect(event(report, "checkout_view").occurrences[0]?.props).toEqual([{ key: "step", type: "number", literal: 1 }]);
    expect(event(report, "import_done").occurrences[0]?.line).toBe(13);
    // A client imported from the project's own module, and one from createInstrumentation.
    expect(event(report, "refund_issued").occurrences[0]?.source).toBe("server");
    expect(event(report, "next_server_boot").occurrences[0]?.source).toBe("server");
    // `track({ url })` is a pageview, not an event.
    expect(report.events.every((e) => e.occurrences.every((o) => o.name !== null))).toBe(true);
  });

  it("applies the server's name rules and the revenue rules", () => {
    const messages = report.findings.map((f) => `${f.severity}: ${f.message}`);
    expect(messages).toEqual(
      expect.arrayContaining([
        expect.stringMatching(/^error: The event name is empty/),
        expect.stringMatching(/^error: The name is 220 bytes/),
        expect.stringMatching(/^error: The property "email" holds an email address/),
        expect.stringMatching(/^warning: "trailing_space " has spaces/),
        expect.stringMatching(/^warning: "Checkout Started" and "checkout_started"/),
        expect.stringMatching(/^warning: "revenue" is a string/),
        expect.stringMatching(/^warning: "currency" is "euro"/),
      ]),
    );
    // Errors come first.
    expect(report.findings[0]?.severity).toBe("error");
  });
});

describe("scanSource()", () => {
  const one = (file: string, text: string): Occurrence[] => scanSource(file, text).occurrences;

  it("reads both sides of a conditional and a const string", () => {
    const found = one("a.ts", `import { track } from "@micaforge/sdk";\nconst A = "alpha";\ntrack(ok ? A : "beta");`);
    expect(found.map((o) => o.name)).toEqual(["alpha", "beta"]);
  });

  it("keeps a concatenation's fixed parts as a glob", () => {
    const [found] = one("a.js", `import { track } from "@micaforge/sdk";\ntrack("export_" + format + "_done");`);
    expect(found).toMatchObject({ name: null, pattern: "export_*_done" });
  });

  it("marks properties it cannot see", () => {
    const [found] = one("a.js", `import { track } from "@micaforge/sdk";\ntrack("x", { ...base, plan: "pro" });`);
    expect(found).toMatchObject({ propsDynamic: true, props: [{ key: "plan" }] });
    const [byName] = one("b.js", `import { track } from "@micaforge/sdk";\ntrack("y", props);`);
    expect(byName?.propsDynamic).toBe(true);
  });

  it("follows an aliased import and a namespace import", () => {
    const found = one(
      "a.ts",
      `import { track as send } from "@micaforge/sdk";\nimport * as mf from "@micaforge/sdk";\nsend("one");\nmf.track("two");\nmf.getTracker()?.track("three");`,
    );
    expect(found.map((o) => o.name)).toEqual(["one", "two", "three"]);
  });

  it("reports a file it cannot parse instead of failing the scan", () => {
    const result = scanSource("broken.js", "import { track } from '@micaforge/sdk';\ntrack('a', {");
    expect(result.errors).toHaveLength(1);
  });
});
