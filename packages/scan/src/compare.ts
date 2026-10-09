/**
 * The code against the site: which events the code sends that the site never received,
 * and which the site receives that no code sends any more.
 *
 * It reads `GET /api/stats/events`, the same endpoint the Events screen reads, with an
 * API key holding `stats:read`. No new endpoint was needed: that one already lists every
 * custom event name in a window with its count, up to 10,000 names.
 *
 * Computed names take part through their patterns: a call that sends `` `step_${n}` ``
 * accounts for every received `step_…`, so a series is not reported as missing from the
 * code. A computed name with no fixed part matches nothing, and the report says how many
 * of those there are, because each one could be sending any of the "not in code" names.
 *
 * The key travels only in the `Authorization` header and appears in no message this
 * module builds.
 */
import type { Comparison } from "./model.js";

/** Where to compare, and with what. */
export interface CompareOptions {
  /** The Micaforge origin, such as `https://analytics.example.com`. */
  host: string;
  /** The numeric site id. */
  site: number;
  /** An API key (`mfg_…`) with the `stats:read` scope. */
  apiKey: string;
  /** A named window the stats API accepts. Default `30d`. */
  range?: string;
  /** The transport. Default: the global `fetch`. */
  fetch?: typeof globalThis.fetch;
  /** Milliseconds before the read is abandoned. Default 15 000. */
  timeoutMs?: number;
}

/** Most names one read of the events endpoint returns. */
const READ_LIMIT = 10_000;

/** A name pattern (`step_*`) as a regular expression. */
export function patternToRegExp(pattern: string): RegExp {
  return new RegExp(`^${pattern.split("*").map((part) => part.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join(".*")}$`);
}

/** Read the site's event names and set them against the code's. */
export async function compareWithSite(
  names: string[],
  patterns: string[],
  options: CompareOptions,
): Promise<Comparison> {
  const range = options.range ?? "30d";
  const host = options.host.trim().replace(/\/+$/, "");
  const url = new URL(`${/^https?:\/\//i.test(host) ? host : `https://${host}`}/api/stats/events`);
  url.searchParams.set("site", String(options.site));
  url.searchParams.set("range", range);
  url.searchParams.set("limit", String(READ_LIMIT));

  const doFetch = options.fetch ?? globalThis.fetch;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? 15_000);
  let response: Response;
  try {
    response = await doFetch(url, {
      headers: { authorization: `Bearer ${options.apiKey}`, accept: "application/json" },
      signal: controller.signal,
    });
  } catch (error) {
    throw new Error(
      `could not reach ${url.origin}: ${controller.signal.aborted ? "no answer in time" : (error as Error).message}`,
    );
  } finally {
    clearTimeout(timer);
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = null;
  }
  if (!response.ok) {
    const message = (body as { error?: { message?: unknown } } | null)?.error?.message;
    const hint =
      response.status === 401
        ? " The API key was not accepted."
        : response.status === 403
          ? " The API key needs the stats:read scope and access to this site."
          : response.status === 404
            ? " The site id is not one this key can read."
            : "";
    throw new Error(
      `the stats API answered ${response.status}${typeof message == "string" ? `: ${message}` : ""}.${hint}`,
    );
  }

  const rows = (body as { data?: unknown } | null)?.data;
  if (!Array.isArray(rows)) throw new Error("the stats API answered with something that is not an event list");
  const received = rows
    .filter((r): r is { name: string; count?: number } => !!r && typeof (r as { name?: unknown }).name == "string")
    .map((r) => ({ name: r.name, count: Number(r.count) || 0 }));

  const receivedNames = new Set(received.map((r) => r.name));
  const regexps = patterns.map(patternToRegExp);
  const inCode = new Set(names.map((n) => n.trim()));

  return {
    site: options.site,
    range,
    received,
    notReceived: [...inCode].filter((n) => n && !receivedNames.has(n)).sort(),
    notInCode: received.filter((r) => !inCode.has(r.name) && !regexps.some((re) => re.test(r.name))),
    truncated: received.length >= READ_LIMIT,
  };
}
