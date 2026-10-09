/**
 * Checkout reference tests. The claim under test: it asks the right address, returns
 * only a real reference, and never throws into a checkout.
 */
import { describe, expect, it, vi } from "vitest";
import { requestCheckoutRef } from "./checkout.js";

const answer = (status: number, body: unknown) =>
  vi.fn(async () => new Response(JSON.stringify(body), { status })) as unknown as typeof fetch;

describe("requestCheckoutRef", () => {
  it("asks the site's own reference route and returns the token", async () => {
    const f = answer(200, { data: { ref: "mfr_abc" }, meta: {} });
    const ref = await requestCheckoutRef({ site: 7, host: "https://mf.example.com/", fetch: f });
    expect(ref).toBe("mfr_abc");
    const [url, init] = (f as unknown as ReturnType<typeof vi.fn>).mock.calls[0] as [string, RequestInit];
    expect(url).toBe("https://mf.example.com/api/revenue/ref?s=7");
    expect(init.credentials).toBe("omit");
  });

  it("returns null rather than something that is not a reference", async () => {
    expect(await requestCheckoutRef({ site: 7, host: "https://h", fetch: answer(200, { data: { ref: "x" } }) })).toBeNull();
    expect(await requestCheckoutRef({ site: 7, host: "https://h", fetch: answer(404, { error: {} }) })).toBeNull();
  });

  it("never throws, and asks nothing without a site or a host", async () => {
    const broken = vi.fn(async () => {
      throw new Error("offline");
    }) as unknown as typeof fetch;
    expect(await requestCheckoutRef({ site: 7, host: "https://h", fetch: broken })).toBeNull();
    const unused = answer(200, { data: { ref: "mfr_x" } });
    expect(await requestCheckoutRef({ site: "", host: "https://h", fetch: unused })).toBeNull();
    expect(await requestCheckoutRef({ site: 7, host: "", fetch: unused })).toBeNull();
    expect((unused as unknown as ReturnType<typeof vi.fn>).mock.calls.length).toBe(0);
  });
});
