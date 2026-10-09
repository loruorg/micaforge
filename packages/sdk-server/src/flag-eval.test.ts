/**
 * The shared flag vectors, asserted against the Node copy of the evaluator, and the
 * check that the copy has not drifted from the browser's.
 *
 * The vector file lives in the browser package (`packages/sdk/test/flag-vectors.json`)
 * and is computed by the Rust evaluator the server runs. A case passing here, in the
 * browser package and in the Rust crate is one visitor getting one answer everywhere.
 */
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { blake3, utf8 } from "./blake3.js";
import { evaluateFlag, flagBucket, type FlagDecision, type FlagRuleset } from "./flag-eval.js";

const read = (relative: string): string => readFileSync(new URL(relative, import.meta.url), "utf8");

interface Vectors {
  blake3: { text?: string; len?: number; hex: string }[];
  buckets: { salt: string; subject: string; bucket: number }[];
  rulesets: Record<string, FlagRuleset>;
  cases: {
    name: string;
    ruleset: string | FlagRuleset;
    key: string;
    subject: string;
    context: Record<string, unknown>;
    environment: string | null;
    expect: FlagDecision;
  }[];
}

const v = JSON.parse(read("../../sdk/test/flag-vectors.json")) as Vectors;

describe("the evaluator copy", () => {
  it("is byte-identical to the browser package's", () => {
    for (const file of ["flag-eval.ts", "blake3.ts"]) {
      expect(
        read(`./${file}`) === read(`../../sdk/src/${file}`),
        `packages/sdk-server/src/${file} differs from packages/sdk/src/${file}; copy the browser file across`,
      ).toBe(true);
    }
  });
});

describe("the shared vectors", () => {
  it("hash every blake3 vector", () => {
    for (const c of v.blake3) {
      const input =
        c.text !== undefined ? utf8(c.text) : Uint8Array.from({ length: c.len ?? 0 }, (_, i) => i % 251);
      expect(Buffer.from(blake3(input)).toString("hex")).toBe(c.hex);
    }
  });

  it("bucket every vector subject", () => {
    for (const c of v.buckets) expect(flagBucket(c.salt, c.subject)).toBe(c.bucket);
  });

  it("decide every case exactly as the server does", () => {
    expect(v.cases.length).toBeGreaterThan(100);
    for (const c of v.cases) {
      const ruleset = typeof c.ruleset == "string" ? v.rulesets[c.ruleset] : c.ruleset;
      const decided = evaluateFlag(ruleset, c.key, {
        subject: c.subject,
        context: c.context,
        environment: c.environment,
      });
      expect(decided, c.name).toEqual(c.expect);
    }
  });
});
