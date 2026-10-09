import { describe, expect, it } from "vitest";
import { luhn, personalData, validate } from "./validate.js";

const page = "https://example.com/pricing";

describe("validate", () => {
  it("passes a clean event", () => {
    expect(validate({ t: "custom", n: "signup", u: page, p: { plan: "pro", seats: 5 } })).toEqual([]);
    expect(validate({ t: "pageview", u: page })).toEqual([]);
  });

  it("flags names the server would cut, split or cannot tell apart", () => {
    expect(validate({ t: "custom", u: page })).toEqual([expect.stringMatching(/no name/)]);
    expect(validate({ t: "custom", n: "x".repeat(201), u: page })[0]).toMatch(/keeps the first 200/);
    expect(validate({ t: "custom", n: " signup", u: page })[0]).toMatch(/space/);
    expect(validate({ t: "custom", n: "order_1234567", u: page })[0]).toMatch(/looks like an id/);
  });

  it("flags an unknown name against the site's list, with a near match", () => {
    const known = ["signup", "purchase"];
    expect(validate({ t: "custom", n: "sign_up", u: page }, known)).toEqual([expect.stringMatching(/Did you mean "signup"/)]);
    expect(validate({ t: "custom", n: "checkout", u: page }, known)[0]).toMatch(/not in your list/);
    expect(validate({ t: "custom", n: "purchase", u: page }, known)).toEqual([]);
  });

  it("flags properties over the server's 8 KB", () => {
    expect(validate({ t: "custom", n: "x", u: page, p: { blob: "é".repeat(5000) } })[0]).toMatch(/10011 bytes/);
  });

  it("flags personal data in properties and in the address", () => {
    const w = validate({ t: "custom", n: "x", u: page + "?email=a%40b.co", p: { user: { email: "ana@example.com" }, card: "4242 4242 4242 4242" } });
    expect(w.some((s) => /"user.email" looks like an email address/.test(s))).toBe(true);
    expect(w.some((s) => /"card" looks like a payment card number/.test(s))).toBe(true);
    expect(w.some((s) => /query \(email=\)/.test(s))).toBe(true);
  });

  it("flags revenue the server cannot count", () => {
    expect(validate({ t: "custom", n: "purchase", u: page, p: { revenue: "19.99", currency: "EUR" } })[0]).toMatch(/must be a number/);
    expect(validate({ t: "custom", n: "purchase", u: page, p: { revenue: 19.99 } })[0]).toMatch(/without a currency/);
    expect(validate({ t: "custom", n: "purchase", u: page, p: { revenue: 19.99, currency: "euro" } })[0]).toMatch(/ISO 4217/);
    expect(validate({ t: "identify", u: page, p: { revenue: "big" } })).toEqual([]);
  });

  it("flags a kind the server drops", () => {
    expect(validate({ t: "click", u: page })[0]).toMatch(/Unknown kind/);
  });
});

describe("personal data", () => {
  it("tells a card number from an order number and finds phones and emails", () => {
    expect(luhn("4242424242424242")).toBe(true);
    expect(luhn("4242424242424241")).toBe(false);
    expect(personalData("order 1234567890123")).toBeNull();
    expect(personalData("+44 20 7946 0958")).toBe("a phone number");
    expect(personalData("2026-10-09")).toBeNull();
    expect(personalData("hello@micaforge.test")).toBe("an email address");
  });
});
