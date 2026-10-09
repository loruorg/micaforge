/**
 * Google Search Console: the shapes the Search screen reads, and the words it
 * uses for them.
 *
 * Every figure here is Google's, not Micaforge's: clicks and impressions from
 * Google Search, the click-through rate (a fraction), and the average position
 * (1 is the top of the results). Micaforge's own count appears only where it
 * is labelled as such, beside a page.
 */
import type { Envelope } from "../../lib/api";

/** Where a site's connection stands. */
export interface SearchConnection {
  status: "pending" | "active" | "revoked";
  property: string | null;
  connected_by: string | null;
  connected_at: string;
  backfill_until: string | null;
  backfilled_from: string | null;
  synced_through: string | null;
  backfill_done: boolean;
  syncing: boolean;
  last_sync_started_at: string | null;
  last_sync_finished_at: string | null;
  last_error: string | null;
  last_error_at: string | null;
  rows_synced: number;
}

/** `GET /api/search-console?site=` */
export interface SearchStatus {
  configured: boolean;
  redirect_uri: string;
  can_manage: boolean;
  connection: SearchConnection | null;
}

/** One property the connected Google account can read. */
export interface SearchProperty {
  site_url: string;
  permission: string;
  kind: "domain" | "url_prefix";
}

export interface SearchTotals {
  clicks: number;
  impressions: number;
  ctr: number;
  position: number;
}

/** Each figure's change, in percent; `null` where the earlier one was zero. */
export interface SearchChange {
  clicks: number | null;
  impressions: number | null;
  ctr: number | null;
  position: number | null;
}

/** `GET /api/stats/search/overview` */
export interface SearchOverview {
  window: { start: string; end: string };
  totals: SearchTotals;
  previous_window: { start: string; end: string } | null;
  previous: SearchTotals | null;
  change: SearchChange | null;
  first_day: string | null;
  last_day: string | null;
  provisional_from: string;
}

/** One day of `GET /api/stats/search/timeseries`. */
export interface SearchPoint extends SearchTotals {
  date: string;
}

/** One ranked row: a search term, a page, a country or a device. */
export interface SearchRow extends SearchTotals {
  key: string;
  hostname?: string;
  pathname?: string;
}

/** A page, with Micaforge's own count beside Google's. */
export interface SearchPageRow extends SearchRow {
  visitors: number | null;
  pageviews: number | null;
  from_google: number | null;
}

export type SearchMetric = "clicks" | "impressions" | "ctr" | "position";

export const METRICS: { v: SearchMetric; label: string }[] = [
  { v: "clicks", label: "Clicks" },
  { v: "impressions", label: "Impressions" },
  { v: "ctr", label: "Click-through rate" },
  { v: "position", label: "Average position" },
];

/** The query envelope for a named range or a measured `start..end` one. */
export function envelopeOf(siteId: number, range: string): Envelope {
  const cut = range.indexOf("..");
  return cut > 0
    ? { site: siteId, start: range.slice(0, cut), end: range.slice(cut + 2) }
    : { site: siteId, range };
}

/** A `YYYY-MM-DD` day as local midnight, for a chart axis. */
export function dayTime(date: string): number {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, (m ?? 1) - 1, d ?? 1).getTime();
}

/** A `YYYY-MM-DD` day in words: "8 Oct 2026". */
export function dayWords(date: string | null | undefined): string {
  if (!date) return "—";
  return new Date(dayTime(date)).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
  });
}

/** An average position, to one decimal. */
export function positionText(p: number | null | undefined): string {
  if (p == null || !Number.isFinite(p) || p <= 0) return "—";
  return p.toFixed(1);
}

/** Google's device names, in the product's words. */
export function deviceName(d: string): string {
  switch (d) {
    case "desktop":
      return "Desktop";
    case "mobile":
      return "Mobile";
    case "tablet":
      return "Tablet";
    default:
      return d || "Unknown";
  }
}

/** A property, as a person reads it. */
export function propertyName(p: string): string {
  return p.startsWith("sc-domain:") ? `${p.slice("sc-domain:".length)} (domain)` : p;
}

/** What each refusal code from Google's redirect means, in one sentence. */
export const CONNECT_ERRORS: Record<string, string> = {
  state:
    "The connection could not be finished: the link was used already, it expired, or it was started by another account. Start again.",
  denied: "Access was not granted at Google. Start again and allow Micaforge to read Search Console.",
  forbidden: "Your role on this site no longer allows connecting Search Console. Ask a site admin.",
  scope:
    "Google did not grant access to Search Console data. Start again and leave the Search Console box ticked on Google's screen.",
  disabled: "Google Search Console is not configured on this server.",
  provider: "Google did not answer as expected. Try again in a minute.",
};
