/**
 * Reading a request without depending on the framework that produced it.
 *
 * Express, Fastify, Hono, Next and a bare `node:http` server all hand you something
 * request-shaped, and no two of them are the same type. Rather than import five sets of
 * types (and then carry five peer dependencies), everything here works structurally
 * against {@link RequestLike}: a `headers` bag that is either a plain object or a
 * `Headers`, a `url` that is either a path or an absolute URL, and a socket that may or
 * may not exist.
 *
 * The rule for every function in this file is that a malformed input returns a sensible
 * empty value. Nothing here throws.
 */
import type { HeadersLike, RequestLike } from "./types.js";

/** Read one header, case-insensitively, from either header shape. */
export function header(headers: HeadersLike | undefined, name: string): string {
  if (!headers) return "";
  const key = name.toLowerCase();
  try {
    // A `Headers` (Fetch, Workers, Deno, Bun). Its own `get` is case-insensitive.
    if (typeof (headers as { get?: unknown }).get == "function") {
      return (headers as { get(n: string): string | null }).get(key) ?? "";
    }
    const bag = headers as Record<string, string | string[] | undefined>;
    // Node lowercases incoming header names, so the direct hit covers almost every
    // call. The scan is for the hand-built object and for Fastify's raw bag.
    let value = bag[key];
    if (value === undefined) {
      for (const k of Object.keys(bag)) {
        if (k.toLowerCase() === key) {
          value = bag[k];
          break;
        }
      }
    }
    if (value === undefined) return "";
    // A repeated header arrives as an array. Joining with a comma is what the HTTP
    // spec says a repeated field means, and it is what `x-forwarded-for` expects.
    return Array.isArray(value) ? value.join(",") : String(value);
  } catch {
    return "";
  }
}

/**
 * The end client's IP address, or `""`.
 *
 * `x-forwarded-for` is a list appended to by each hop, so the first entry is the
 * original client and every later one is a proxy. Cloudflare, Fastly and most managed
 * proxies also set a single-value header of their own, which is checked first because
 * it cannot be extended by a hop and is therefore harder to spoof.
 */
export function clientIp(req: RequestLike, trustProxy = true): string {
  if (trustProxy) {
    const single =
      header(req.headers, "cf-connecting-ip") ||
      header(req.headers, "true-client-ip") ||
      header(req.headers, "x-real-ip");
    if (single) return normaliseIp(single.trim());

    const forwarded = header(req.headers, "x-forwarded-for");
    if (forwarded) {
      const first = forwarded.split(",")[0];
      if (first && first.trim()) return normaliseIp(first.trim());
    }
  }
  if (typeof req.ip == "string" && req.ip) return normaliseIp(req.ip);
  const socket = req.socket ?? req.connection;
  const remote = socket?.remoteAddress;
  return remote ? normaliseIp(remote) : "";
}

/**
 * Unwrap an IPv4-mapped IPv6 address.
 *
 * A dual-stack Node socket reports `::ffff:203.0.113.9` for a plain IPv4 client. The
 * geo lookup on the server wants `203.0.113.9`, and leaving the prefix on would put
 * every such visitor in the "no country" bucket.
 */
function normaliseIp(ip: string): string {
  const lower = ip.toLowerCase();
  return lower.startsWith("::ffff:") && lower.includes(".") ? lower.slice(7) : ip;
}

/** The `User-Agent` header, or `""`. */
export function userAgent(req: RequestLike): string {
  return header(req.headers, "user-agent");
}

/**
 * True when the request was a re-crawl check rather than a fresh read.
 *
 * `conditional` on `agent_events`. A crawler that keeps sending `If-None-Match` is
 * asking "did this change?", and counting those the same as a full fetch would
 * overstate how much of the site is actually being taken.
 */
export function isConditional(req: RequestLike): boolean {
  return !!(header(req.headers, "if-none-match") || header(req.headers, "if-modified-since"));
}

/** The parts of a URL that the schema has columns for. `hash` is never set here. */
export interface UrlParts {
  hostname: string;
  pathname: string;
  querystring: string;
  hash: string;
}

const EMPTY_PARTS: UrlParts = { hostname: "", pathname: "/", querystring: "", hash: "" };

/**
 * Split a URL into the four columns that hold it.
 *
 * Accepts an absolute URL or a bare path; a bare path is resolved against a throwaway
 * base so that `new URL` can do the parsing, and the throwaway host is then discarded
 * rather than reported. A URL that will not parse yields `/` rather than an exception:
 * a broken link in a log line is not a reason to lose the hit.
 */
export function splitUrl(input: string | undefined, fallbackHost = ""): UrlParts {
  if (!input) return { ...EMPTY_PARTS, hostname: fallbackHost };
  try {
    const absolute = /^[a-z][a-z0-9+.-]*:\/\//i.test(input);
    const url = new URL(input, absolute ? undefined : "http://placeholder.invalid");
    const host = absolute || !fallbackHost ? url.hostname : fallbackHost;
    return {
      hostname: (absolute ? url.hostname : host) || fallbackHost,
      pathname: url.pathname || "/",
      querystring: url.search.replace(/^\?/, ""),
      hash: url.hash.replace(/^#/, ""),
    };
  } catch {
    // Not a URL at all. Keep whatever looks like a path so the row is still useful.
    const [path = "/", query = ""] = input.split("?", 2);
    return { hostname: fallbackHost, pathname: path || "/", querystring: query, hash: "" };
  }
}

/**
 * The host the client asked for.
 *
 * `x-forwarded-host` first, because behind a proxy the `host` header is the proxy's own
 * view. HTTP/2 sends `:authority` instead of `host`, and Node exposes it under that
 * name. The port is stripped: `hostname` in the schema is a host, not an authority.
 */
export function requestHost(req: RequestLike, trustProxy = true): string {
  const raw =
    (trustProxy ? header(req.headers, "x-forwarded-host") : "") ||
    header(req.headers, "host") ||
    header(req.headers, ":authority") ||
    (typeof req.hostname == "string" ? req.hostname : "");
  if (!raw) return "";
  const first = raw.split(",")[0]?.trim() ?? "";
  // Strip a port, but leave a bracketed IPv6 literal intact.
  if (first.startsWith("[")) return first.replace(/^\[|\](:\d+)?$/g, "");
  const colon = first.lastIndexOf(":");
  return colon > 0 ? first.slice(0, colon) : first;
}

/** Everything the SDK reads off a request, in one pass. */
export interface ReadRequest {
  method: string;
  url: UrlParts;
  userAgent: string;
  ip: string;
  referrer: string;
  language: string;
  conditional: boolean;
}

/**
 * Read a request once, into the fields both event kinds are built from.
 *
 * `referer` is the header's historical misspelling and is the one on the wire; the
 * schema column is spelled correctly. Both spellings are read because some clients
 * send the corrected one.
 */
export function readRequest(req: RequestLike, trustProxy = true): ReadRequest {
  const host = requestHost(req, trustProxy);
  const raw = typeof req.url == "string" ? req.url : typeof req.path == "string" ? req.path : "/";
  return {
    method: (typeof req.method == "string" ? req.method : "GET").toUpperCase(),
    url: splitUrl(raw, host),
    userAgent: userAgent(req),
    ip: clientIp(req, trustProxy),
    referrer: header(req.headers, "referer") || header(req.headers, "referrer"),
    language: (header(req.headers, "accept-language").split(",")[0] ?? "").trim(),
    conditional: isConditional(req),
  };
}
