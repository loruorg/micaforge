import { describe, expect, it } from "vitest";

import { editDistance, nameFindings, nearDuplicates, normaliseName, propFindings } from "../src/checks.js";
import type { Occurrence } from "../src/model.js";

const at = (name: string | null, source: Occurrence["source"] = "sdk"): Occurrence => ({
  file: "a.ts",
  line: 3,
  column: 1,
  source,
  name,
  props: [],
  propsDynamic: false,
});

describe("names", () => {
  it("follows the server: trimmed, at most 200 bytes, never empty", () => {
    expect(nameFindings(at("signup"))).toEqual([]);
    expect(nameFindings(at("   "))[0]).toMatchObject({ severity: "error", check: "invalid-name" });
    expect(nameFindings(at("", "attribute"))[0]?.message).toContain("sends nothing");
    expect(nameFindings(at("é".repeat(101)))[0]?.message).toContain("202 bytes");
    expect(nameFindings(at("a".repeat(200)))).toEqual([]);
    expect(nameFindings(at(" signup"))[0]).toMatchObject({ severity: "warning" });
    expect(nameFindings(at("sign\nup"))[0]?.message).toContain("control character");
    expect(nameFindings(at(null))).toEqual([]);
  });
});

describe("near duplicates", () => {
  it("normalises case and separators", () => {
    expect(normaliseName("Sign-Up")).toBe("signup");
    expect(normaliseName("sign_up")).toBe("signup");
    expect(normaliseName("signUp")).toBe("signup");
    expect(normaliseName("plan.changed")).toBe("planchanged");
  });

  it("counts edits, with a transposition as one", () => {
    expect(editDistance("signup", "signup")).toBe(0);
    expect(editDistance("signup", "singup")).toBe(1);
    expect(editDistance("signup", "signups")).toBe(1);
    expect(editDistance("checkout", "checkin", 3)).toBe(3);
    expect(editDistance("a", "abcdefgh", 2)).toBe(3);
  });

  it("pairs one event spelled two ways, and leaves a numbered series alone", () => {
    const pairs = nearDuplicates([
      "signup",
      "Sign_Up",
      "checkout_started",
      "checkout_strated",
      "step_1",
      "step_2",
      "plan",
      "plans",
      "signup",
    ]).map((f) => f.message);
    expect(pairs).toEqual([
      expect.stringContaining('"Sign_Up" and "signup" differ only in case or separators'),
      expect.stringContaining('"checkout_started" and "checkout_strated" are one or two letters apart'),
    ]);
  });
});

describe("properties", () => {
  const occurrence = at("signup");

  it("flags personal keys, personal literals and personal sources", () => {
    expect(propFindings(occurrence, { key: "email", type: "unknown" })[0]).toMatchObject({ severity: "warning" });
    expect(propFindings(occurrence, { key: "lastName", type: "unknown" })[0]?.check).toBe("personal-data");
    expect(propFindings(occurrence, { key: "plan", type: "string", literal: "pro" })).toEqual([]);
    expect(propFindings(occurrence, { key: "contact", type: "string", literal: "ana@example.com" })[0]).toMatchObject({
      severity: "error",
    });
    expect(
      propFindings(occurrence, { key: "token", type: "string", literal: "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ4In0.c2ln" }),
    ).toEqual([expect.objectContaining({ severity: "error" }), expect.objectContaining({ severity: "warning" })]);
    expect(propFindings(occurrence, { key: "who", type: "unknown", expression: "user.email" })[0]?.message).toContain(
      "`user.email`",
    );
    expect(propFindings(occurrence, { key: "callback", type: "string", literal: "+44 20 7946 0958" })[0]?.message).toContain(
      "phone number",
    );
  });

  it("checks the two properties that become revenue columns", () => {
    expect(propFindings(occurrence, { key: "revenue", type: "number", literal: 49 })).toEqual([]);
    expect(propFindings(occurrence, { key: "revenue", type: "string", literal: "49" })[0]?.check).toBe("revenue");
    expect(propFindings(occurrence, { key: "currency", type: "string", literal: "EUR" })).toEqual([]);
    expect(propFindings(occurrence, { key: "currency", type: "string", literal: "euro" })[0]?.check).toBe("revenue");
  });

  it("warns when a click tag value is longer than the plugin sends", () => {
    const tag = at("cta", "attribute");
    expect(propFindings(tag, { key: "label", type: "string", literal: "x".repeat(201) })[0]?.message).toContain("first 200");
  });
});
