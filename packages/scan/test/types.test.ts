import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import ts from "typescript";
import { afterAll, describe, expect, it } from "vitest";

import { generateTypes, scan } from "../src/index.js";

const made: string[] = [];
afterAll(() => {
  for (const dir of made) rmSync(dir, { recursive: true, force: true });
});

/**
 * The SDK's public signature, as `@micaforge/sdk` publishes it: `track(name: string,
 * props?: Props)`. A stub, so the test compiles the generated file against the real
 * shape without building the SDK first.
 */
const SDK_TYPES = `
export type PropValue = string | number | boolean | null | PropValue[] | { [k: string]: PropValue };
export type Props = Record<string, PropValue | undefined>;
export declare function track(name: string, props?: Props): void;
`;

/** Type-check `files` (relative path to text) and return the diagnostics as text. */
function typecheck(files: Record<string, string>): string[] {
  const dir = mkdtempSync(join(tmpdir(), "micaforge-types-"));
  made.push(dir);
  mkdirSync(join(dir, "node_modules/@micaforge/sdk"), { recursive: true });
  writeFileSync(join(dir, "node_modules/@micaforge/sdk/package.json"), '{"name":"@micaforge/sdk","types":"index.d.ts"}');
  writeFileSync(join(dir, "node_modules/@micaforge/sdk/index.d.ts"), SDK_TYPES);
  for (const [name, text] of Object.entries(files)) writeFileSync(join(dir, name), text);
  const program = ts.createProgram(
    Object.keys(files).map((name) => join(dir, name)),
    {
      strict: true,
      noEmit: true,
      target: ts.ScriptTarget.ES2022,
      module: ts.ModuleKind.ESNext,
      moduleResolution: ts.ModuleResolutionKind.Bundler,
      skipLibCheck: true,
      // The ES library only, and no ambient @types: a third of the time, the same answer.
      lib: ["lib.es2022.d.ts"],
      types: [],
    },
  );
  return ts
    .getPreEmitDiagnostics(program)
    .map((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n"));
}

// Compiling with the TypeScript API takes seconds, not milliseconds.
describe("--types", { timeout: 30_000 }, async () => {
  const report = await scan({ root: fileURLToPath(new URL("./fixtures/react-app", import.meta.url)) });
  const generated = generateTypes(report.events);

  it("names every event and types its properties from the literals", () => {
    expect(generated).toContain('"signup": {\n    plan: string;\n    seats: number;\n  };');
    expect(generated).toContain('"newsletter_signup": {\n    source: string;\n  };');
    expect(generated).toContain('"checkout_started": Record<string, never>;');
    // A value the scanner cannot see is any property value.
    expect(generated).toContain("email: PropValue;");
    expect(generated).toContain("/** Sent from src/legacy.js:2, src/legacy.js:5. */");
  });

  it("compiles, and refuses an unknown event or a property of the wrong type", () => {
    const diagnostics = typecheck({
      "events.ts": generated,
      "usage.ts": `import { track, MICAFORGE_EVENTS } from "./events";
track("signup", { plan: "pro", seats: 5 });
track("cta_click", { placement: "hero" });
track("checkout_started");
const all: readonly string[] = MICAFORGE_EVENTS;
// @ts-expect-error an event the code never sends
track("sign_up_typo");
// @ts-expect-error a number where the code sends a string
track("signup", { plan: 5, seats: 5 });
// @ts-expect-error a required property is missing
track("signup");
export { all };
`,
    });
    expect(diagnostics).toEqual([]);
  });

  it("makes properties optional when only some call sites send them", () => {
    const text = generateTypes([
      {
        name: "opened",
        occurrences: [],
        props: [
          { key: "from", types: ["string"], everywhere: false },
          { key: "kind", types: ["number", "string"], everywhere: true },
        ],
        propsDynamic: true,
      },
    ]);
    expect(text).toContain("from?: string;");
    expect(text).toContain("kind: number | string;");
    expect(text).toContain("[key: string]: PropValue | undefined;");
    expect(typecheck({ "events.ts": text, "use.ts": 'import { track } from "./events";\ntrack("opened", { kind: 1, other: true });\n' })).toEqual([]);
  });
});
