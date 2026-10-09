/**
 * The Growth area's model: the shapes the Links and Revenue screens read, and
 * the few pure rules both screens must apply the same way every time.
 *
 * Three of those rules carry weight and live here, once, for that reason:
 *
 *   1. THE FINAL URL. The editor previews the address a short link will send
 *      people to while the operator is still typing, so it has to build that
 *      address the way the server does: the target's own query is kept exactly
 *      as written, and each non-empty utm_* tag is appended, encoded. The
 *      server's `final_url` is still the truth once the link is saved; this is
 *      a preview of it, not a second opinion.
 *   2. MONEY. The API sends every amount as an integer in MINOR units with an
 *      ISO 4217 code. How many minor units make one major unit is a property of
 *      the currency (100 for USD, 1 for JPY, 1000 for KWD), so it is read from
 *      Intl for that currency rather than assumed to be 100. Two currencies are
 *      never added together anywhere in this area: there is no honest exchange
 *      rate in a webhook.
 *   3. THE ORGANISATION. Short links can belong to the organisation rather than
 *      one site. The site list carries `org_id` on each record (the server's
 *      site JSON writes it), but the shared `SiteSummary` type does not declare
 *      it, so it is read defensively here and the organisation view is simply
 *      not offered when it is missing.
 *
 * Shapes follow the A2 API spec (sections 3 and 4) field for field.
 */
import { ApiError } from "../../lib/api";
import type { SiteSummary } from "../../lib/api";

/* ── short links ─────────────────────────────────────────────────────────── */

export const UTM_KEYS = ["source", "medium", "campaign", "term", "content"] as const;
export type UtmKey = (typeof UTM_KEYS)[number];
export type Utm = Partial<Record<UtmKey, string>>;

export interface Link {
  id: string;
  org_id: string;
  site_id: string | null;
  folder_id: string | null;
  slug: string;
  short_url: string;
  target_url: string;
  final_url: string;
  utm: Utm;
  title: string | null;
  description: string | null;
  image_url: string | null;
  ios_url: string | null;
  android_url: string | null;
  deep_link: string | null;
  expires_at: string | null;
  expired: boolean;
  clicks_30d: number;
  created_at: string;
  updated_at: string;
}

/** What POST and PATCH take. An omitted slug asks the server to generate one. */
export interface LinkInput {
  slug?: string;
  folder_id: string | null;
  target_url: string;
  utm: Utm;
  title: string | null;
  description: string | null;
  image_url: string | null;
  ios_url: string | null;
  android_url: string | null;
  deep_link: string | null;
  expires_at: string | null;
}

export interface Folder {
  id: string;
  org_id: string;
  name: string;
  links: number;
  created_at: string;
}

export interface Ranked {
  value: string;
  clicks: number;
}

export interface LinkStats {
  range: string;
  total: number;
  humans: number;
  bots: number;
  series: { t: number; humans: number; bots: number }[];
  referrers: Ranked[];
  countries: Ranked[];
  devices: Ranked[];
  browsers: Ranked[];
  targets: Ranked[];
}

export interface LinksConfig {
  links_host: string | null;
  short_base: string;
}

export type StatsRange = "24h" | "7d" | "30d" | "90d";

export const STATS_RANGES: { v: StatsRange; label: string }[] = [
  { v: "24h", label: "24 hours" },
  { v: "7d", label: "7 days" },
  { v: "30d", label: "30 days" },
  { v: "90d", label: "90 days" },
];

/** Which list the screen is reading: one site's links, or the organisation's. */
export type LinkScope = { kind: "site"; site: number } | { kind: "org"; org: string };

/** The scope as the query parameter the API reads: `site=` or `org=`. */
export function scopeQuery(scope: LinkScope): string {
  return scope.kind === "site"
    ? `site=${encodeURIComponent(String(scope.site))}`
    : `org=${encodeURIComponent(scope.org)}`;
}

export function scopeKey(scope: LinkScope | null): string {
  if (!scope) return "none";
  return scope.kind === "site" ? `site:${scope.site}` : `org:${scope.org}`;
}

/** The organisation a site belongs to, when the site record carries it. */
export function orgOf(site: SiteSummary | null): string | null {
  const v = (site as (SiteSummary & { org_id?: unknown }) | null)?.org_id;
  return typeof v === "string" && v.length > 0 ? v : null;
}

/** Slugs are 1 to 64 letters, digits, hyphens or underscores. */
export const SLUG_RE = /^[A-Za-z0-9_-]{1,64}$/;

/** The UTM tags with blanks dropped and edges trimmed: what is saved. */
export function cleanUtm(utm: Utm): Utm {
  const out: Utm = {};
  for (const k of UTM_KEYS) {
    const v = (utm[k] ?? "").trim();
    if (v) out[k] = v;
  }
  return out;
}

/**
 * The address a short link sends people to. The target's query string is kept
 * exactly as written and each non-empty tag is appended after it; a fragment
 * stays at the very end, where a browser expects it.
 */
export function finalUrl(target: string, utm: Utm): string {
  const t = target.trim();
  const tags = cleanUtm(utm);
  const pairs = UTM_KEYS.flatMap((k) => {
    const v = tags[k];
    return v ? [`utm_${k}=${encodeURIComponent(v)}`] : [];
  });
  if (!pairs.length) return t;
  const cut = t.indexOf("#");
  const base = cut >= 0 ? t.slice(0, cut) : t;
  const hash = cut >= 0 ? t.slice(cut) : "";
  const joiner = !base.includes("?") ? "?" : base.endsWith("?") || base.endsWith("&") ? "" : "&";
  return `${base}${joiner}${pairs.join("&")}${hash}`;
}

/** True for an absolute http(s) address. */
export function isWebUrl(v: string): boolean {
  try {
    const u = new URL(v.trim());
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

export function isHttpsUrl(v: string): boolean {
  try {
    return new URL(v.trim()).protocol === "https:";
  } catch {
    return false;
  }
}

/** A short URL without its scheme: what a person reads and types. */
export function bareUrl(u: string): string {
  return u.replace(/^https?:\/\//, "");
}

/** Where clicks went, in words. */
export const TARGET_WORDS: Record<string, string> = {
  web: "Web",
  ios: "iOS",
  android: "Android",
  deep_link: "App",
  preview: "Link preview",
  expired: "Expired page",
};

/* ── dates ───────────────────────────────────────────────────────────────── */

const pad = (n: number) => String(n).padStart(2, "0");

/** An ISO timestamp as the value a `datetime-local` input holds, in local time. */
export function toLocalInput(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** A `datetime-local` value (local time) as an ISO timestamp in UTC. */
export function fromLocalInput(v: string): string | null {
  if (!v) return null;
  const d = new Date(v);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

export function localZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return "your local time";
  }
}

/** A day, as a person writes it: "8 Oct 2026". */
export function day(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  return new Date(t).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

/** A moment, to the minute: "8 Oct 2026, 14:05". */
export function moment(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "—";
  return new Date(t).toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/* ── countries ───────────────────────────────────────────────────────────── */

let regionNames: Intl.DisplayNames | null | undefined;

/** An ISO alpha-2 code as the country's name in the reader's language. */
export function countryName(code: string): string {
  const c = code.trim().toUpperCase();
  if (!c) return "Unknown";
  if (regionNames === undefined) {
    try {
      regionNames = new Intl.DisplayNames(undefined, { type: "region" });
    } catch {
      regionNames = null;
    }
  }
  try {
    return regionNames?.of(c) ?? c;
  } catch {
    return c;
  }
}

/* ── revenue ─────────────────────────────────────────────────────────────── */

export type Provider = "stripe" | "paddle";

/**
 * A provider connection. It is created before its signing secret exists,
 * because Stripe and Paddle only show the secret after they are given the
 * webhook address; until `has_secret` is true the server refuses its webhooks.
 * The secret itself is never returned.
 */
export interface Integration {
  id: string;
  site_id: string;
  provider: Provider;
  webhook_url: string;
  enabled: boolean;
  has_secret: boolean;
  last_event_at: string | null;
  created_at: string;
}

export interface CurrencyTotals {
  currency: string;
  gross: number;
  refunds: number;
  net: number;
  payments: number;
  refund_count: number;
  customers: number;
  /** Fraction of payments with a session attribution. */
  attributed: number;
}

export interface RevenueOverview {
  currencies: CurrencyTotals[];
  integrations: number;
}

export interface RevenueSeries {
  currency: string;
  points: { t: number; gross: number; refunds: number; net: number }[];
}

export interface RevenueRow {
  value: string;
  currency: string;
  net: number;
  payments: number;
  customers: number;
}

export interface RevenueCustomer {
  customer: string;
  currency: string;
  net: number;
  payments: number;
  refunds: number;
  first_at: string;
  last_at: string;
  channel: string;
}

export type Attribution = "session" | "visitor" | "email" | "none";

export interface RevenuePayment {
  at: string;
  provider: Provider;
  type: "sale" | "subscription" | "refund";
  amount: number;
  currency: string;
  customer: string;
  channel: string;
  country: string;
  attribution: Attribution;
}

export type RevenueDimension =
  | "channel"
  | "source"
  | "referrer_host"
  | "utm_source"
  | "utm_medium"
  | "utm_campaign"
  | "country"
  | "device_type"
  | "browser"
  | "entry_path"
  | "attribution";

export const DIMENSIONS: { v: RevenueDimension; label: string }[] = [
  { v: "channel", label: "Channel" },
  { v: "source", label: "Source" },
  { v: "referrer_host", label: "Referrer" },
  { v: "utm_source", label: "UTM source" },
  { v: "utm_medium", label: "UTM medium" },
  { v: "utm_campaign", label: "UTM campaign" },
  { v: "country", label: "Country" },
  { v: "device_type", label: "Device" },
  { v: "browser", label: "Browser" },
  { v: "entry_path", label: "First page" },
  { v: "attribution", label: "Attribution method" },
];

export const ATTRIBUTION_WORDS: Record<string, string> = {
  session: "Linked to a visit",
  visitor: "Linked to a visitor",
  email: "Matched by email",
  none: "Not attributed",
};

export const PAYMENT_WORDS: Record<RevenuePayment["type"], string> = {
  sale: "Sale",
  subscription: "Subscription",
  refund: "Refund",
};

export const PROVIDERS: Record<
  Provider,
  { name: string; events: string[]; docs: string; secretName: string; secretHint: string }
> = {
  stripe: {
    name: "Stripe",
    events: ["checkout.session.completed", "invoice.paid", "charge.refunded"],
    docs: "/docs/tracking/revenue-stripe",
    secretName: "Signing secret",
    secretHint:
      "After you add the endpoint, Stripe shows it on the endpoint's page under Signing secret. It starts with whsec_.",
  },
  paddle: {
    name: "Paddle",
    events: ["transaction.completed", "adjustment.created", "adjustment.updated"],
    docs: "/docs/tracking/revenue-paddle",
    secretName: "Secret key",
    secretHint:
      "After you save the notification destination, Paddle shows it on the destination under Secret key.",
  },
};

/** Minor units per major unit, as a power of ten, for one currency. */
const digitCache = new Map<string, number>();
export function minorDigits(currency: string): number {
  const hit = digitCache.get(currency);
  if (hit != null) return hit;
  let d = 2;
  try {
    d = new Intl.NumberFormat("en", { style: "currency", currency }).resolvedOptions()
      .maximumFractionDigits ?? 2;
  } catch {
    d = 2;
  }
  digitCache.set(currency, d);
  return d;
}

/** An amount in minor units, written in its own currency. */
export function money(minor: number, currency: string): string {
  if (!Number.isFinite(minor)) return "—";
  const d = minorDigits(currency);
  const major = minor / 10 ** d;
  try {
    return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(major);
  } catch {
    return `${major.toFixed(d)} ${currency}`;
  }
}

/** The same, abbreviated, for a chart axis where width is scarce. */
export function moneyCompact(minor: number, currency: string): string {
  if (!Number.isFinite(minor)) return "—";
  const major = minor / 10 ** minorDigits(currency);
  try {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency,
      notation: "compact",
      maximumFractionDigits: 1,
    }).format(major);
  } catch {
    return `${Math.round(major)} ${currency}`;
  }
}

/** A refund amount always reads as negative, whatever sign the row carried. */
export const negative = (minor: number): number => -Math.abs(minor);

/* ── errors in words ─────────────────────────────────────────────────────── */

/** The same, for the admin-only changes to a payment connection. */
export function adminProblem(e: unknown): string {
  if (e instanceof ApiError && e.status === 403)
    return "Only an admin or the owner of this organisation can change payment connections. The server refused because your role is lower. Ask an admin to do it.";
  return problem(e);
}

/** Names the problem and the way out of it. Never just "an error occurred". */
export function problem(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network")
      return "Could not reach the Micaforge server. Check that it is running, then try again.";
    if (e.status === 401) return "Your session has expired. Sign in again to continue.";
    if (e.status === 403)
      return "Your role on this organisation does not allow this. Ask an admin or the owner.";
    if (e.status === 404) return "That item no longer exists. It may have been deleted. Reload the list.";
    return e.message;
  }
  if (e instanceof Error && e.message) return e.message;
  return "Something failed and the server did not say what. Try again.";
}
