/**
 * The checkout reference: how a payment in Stripe or Paddle is tied to the visit that
 * led to it, without a cookie.
 *
 * Micaforge identifies a visit on the server, from a daily-rotating hash, so the page
 * itself never holds a visitor or session id. When a visitor is about to pay, the page
 * asks the server for a reference to the visit it is part of right now
 * (`GET /api/revenue/ref?s=<site>`). The answer is an opaque `mfr_…` token, sealed so
 * that only that Micaforge server can read it. The page passes it to the checkout
 * (Stripe `client_reference_id` or `metadata.mf_ref`, Paddle `customData.mf_ref`), and
 * the payment webhook brings it back.
 *
 * Nothing is stored in the browser and nothing is sent unless this is called.
 */

/** Where to ask. */
export interface CheckoutRefConfig {
  /** Numeric site id. */
  site: number | string;
  /** The Micaforge origin, e.g. `https://micaforge.example.com`. */
  host: string;
  /** For tests and unusual runtimes. Defaults to the global `fetch`. */
  fetch?: typeof fetch;
}

/**
 * Ask the server for this visit's checkout reference.
 *
 * Resolves to `null`, never throws, when there is nothing to ask (no site, no host,
 * no `fetch`) or the request fails: a checkout must never break because analytics
 * could not answer. The payment is then attributed by email, or not at all.
 */
export async function requestCheckoutRef(config: CheckoutRefConfig): Promise<string | null> {
  const site = String(config.site || "").trim();
  const host = (config.host || "").replace(/\/+$/, "");
  const doFetch = config.fetch || (typeof fetch === "function" ? fetch : undefined);
  if (!site || !host || !doFetch) return null;
  try {
    const res = await doFetch(`${host}/api/revenue/ref?s=${encodeURIComponent(site)}`, {
      credentials: "omit",
      cache: "no-store",
    });
    if (!res.ok) return null;
    const body = (await res.json()) as { data?: { ref?: unknown } };
    const ref = body && body.data ? body.data.ref : undefined;
    return typeof ref === "string" && ref.indexOf("mfr_") === 0 ? ref : null;
  } catch {
    return null;
  }
}
