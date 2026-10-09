/**
 * The shared flag vectors, asserted against the browser copy of the evaluator.
 *
 * `test/flag-vectors.json` is computed by the Rust evaluator the server runs
 * (`micaforge_core::flags`) and asserted by the Rust, browser and Node test suites alike,
 * so a case that passes here is a visitor who gets the same answer from all three.
 */
import { describe, expect, it } from "vitest";
import vectors from "../test/flag-vectors.json";
import { blake3, utf8 } from "./blake3.js";
import {
  canonicalNumber,
  compileRuleset,
  evaluateFlag,
  evaluateFlags,
  flagBucket,
  parseNumber,
  type FlagDecision,
  type FlagRuleset,
} from "./flag-eval.js";

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

const v = vectors as unknown as Vectors;

const hex = (bytes: Uint8Array): string => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

const counted = (len: number): Uint8Array => {
  const out = new Uint8Array(len);
  for (let i = 0; i < len; i++) out[i] = i % 251;
  return out;
};

describe("blake3", () => {
  it("matches every digest in the vector file, the tree included", () => {
    expect(v.blake3.length).toBeGreaterThan(10);
    for (const c of v.blake3) {
      const input = c.text !== undefined ? utf8(c.text) : counted(c.len ?? 0);
      expect(hex(blake3(input)), `len ${input.length}`).toBe(c.hex);
    }
  });

  it("encodes text the way TextEncoder does", () => {
    for (const s of ["", "abc", "ünïcødé-訪問者", "😀 visitor", "a\u0000b"]) {
      expect(Array.from(utf8(s))).toEqual(Array.from(new TextEncoder().encode(s)));
    }
    // A lone surrogate cannot be UTF-8 and becomes U+FFFD, as TextEncoder does.
    expect(Array.from(utf8("\ud800x"))).toEqual(Array.from(new TextEncoder().encode("\ud800x")));
  });
});

describe("bucketing", () => {
  it("puts every vector subject in the server's bucket", () => {
    for (const c of v.buckets) expect(flagBucket(c.salt, c.subject), `${c.salt} / ${c.subject.slice(0, 20)}`).toBe(c.bucket);
  });

  it("stays inside 0-9999", () => {
    for (let i = 0; i < 500; i++) {
      const b = flagBucket("k", "visitor-" + i);
      expect(b).toBeGreaterThanOrEqual(0);
      expect(b).toBeLessThan(10000);
    }
  });
});

describe("evaluation", () => {
  it("decides every vector case exactly as the server does", () => {
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

  it("decides a whole ruleset at once, the same way as one flag at a time", () => {
    const ruleset = v.rulesets.basic;
    const input = { subject: "visitor-3", context: { plan: "pro", country: "PT" } };
    const all = evaluateFlags(ruleset, input);
    const compiled = compileRuleset(ruleset);
    expect(Object.keys(all)).toEqual(compiled.keys);
    for (const key of compiled.keys) expect(all[key]).toEqual(evaluateFlag(ruleset, key, input));
  });

  it("never throws, whatever it is handed", () => {
    const junk: unknown[] = [
      null,
      undefined,
      42,
      "rules",
      [],
      { flags: "no" },
      { flags: [null, 1, "x", { key: 5 }, { key: "a", enabled: true, conditions: { logic: "or", filters: [[]] } }] },
      { flags: [{ key: "a", enabled: true, variants: "x", dependencies: [null, 1], environments: [1] }] },
      { flags: [{ key: "constructor", enabled: true }], target_groups: { __proto__: { conditions: [] } } },
    ];
    for (const ruleset of junk) {
      for (const key of ["a", "constructor", "__proto__", ""]) {
        expect(() => evaluateFlag(ruleset as FlagRuleset, key, { subject: "s" })).not.toThrow();
        expect(() =>
          evaluateFlag(ruleset as FlagRuleset, key, { subject: 5 as unknown as string, context: "x" as never }),
        ).not.toThrow();
      }
    }
    // A cyclic condition object a hand-written page might build.
    const loop: Record<string, unknown> = { logic: "and" };
    loop.filters = [loop];
    expect(evaluateFlag({ flags: [{ key: "x", enabled: true, conditions: loop as never }] }, "x", { subject: "s" }).reason).toBe(
      "conditions",
    );
  });

  it("reads an inherited property as absent", () => {
    const ruleset: FlagRuleset = {
      flags: [{ key: "x", enabled: true, conditions: [{ dimension: "constructor", op: "is_set" }] }],
    };
    expect(evaluateFlag(ruleset, "x", { subject: "s" }).reason).toBe("conditions");
    expect(evaluateFlag(ruleset, "toString", { subject: "s" }).reason).toBe("unknown");
  });
});

describe("numbers", () => {
  it("print and parse the way the server does", () => {
    expect(canonicalNumber(5)).toBe("5");
    expect(canonicalNumber(-0)).toBe("0");
    expect(canonicalNumber(2.5)).toBe("2.5");
    expect(canonicalNumber(1e21)).toBe("1000000000000000000000");
    expect(canonicalNumber(1e-7)).toBe("0.0000001");
    expect(parseNumber(" 2.5 ")).toBe(2.5);
    expect(parseNumber("+3")).toBe(3);
    expect(parseNumber(".5")).toBe(0.5);
    expect(parseNumber("5.")).toBe(5);
    for (const refused of ["", "inf", "NaN", "Infinity", "1_000", "0x10", "1e", ".", "--1", "1e999"]) {
      expect(parseNumber(refused), refused).toBeNull();
    }
  });
});
