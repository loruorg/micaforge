/**
 * AI agent fetches, reported to Micaforge from the edge.
 *
 * ## Why this exists
 *
 * Crawlers do not run JavaScript, so the browser tracker never sees one, and a
 * site served by Cloudflare Pages keeps no access log to ship. The Pages
 * middleware is the only place a crawler's fetch is visible at all, so it hands
 * each one here, and this posts it to Micaforge's edge intake as one signed row.
 * Micaforge then names the operator, verifies the source address against the
 * ranges the operator publishes, and checks the fetch against robots.txt and
 * llms.txt.
 *
 * ## What is and is not sent
 *
 * Only requests whose user agent names a known agent. People are counted by the
 * browser tracker; sending them here as well would count every visit twice. The
 * token list mirrors Micaforge's own catalog, and a miss only means "not sent":
 * the server, not this list, decides what a fetch was.
 *
 * ## Failure policy
 *
 * This runs after the response has been chosen, inside `waitUntil`, and every
 * failure is swallowed. Reporting a crawl must never slow or break a page.
 */

/** Micaforge's edge intake. */
const ENDPOINT = "https://api.micaforge.com/api/log/edge";

/** Names this reporter to Micaforge in its access log. */
const USER_AGENT = "micaforge-pages-edge/1";

/**
 * Agent product tokens, copied from Micaforge's catalog. Matched as
 * case-insensitive substrings, because a real user agent wraps the token in a
 * longer string.
 */
const AGENT_TOKENS = [
  "GPTBot",
  "OAI-SearchBot",
  "ChatGPT-User",
  "ClaudeBot",
  "Claude-User",
  "Claude-SearchBot",
  "anthropic-ai",
  "Claude-Web",
  "PerplexityBot",
  "Perplexity-User",
  "Googlebot",
  "GoogleOther",
  "Google-CloudVertexBot",
  "Google-InspectionTool",
  "bingbot",
  "BingPreview",
  "meta-externalagent",
  "meta-externalfetcher",
  "FacebookBot",
  "facebookexternalhit",
  "Applebot",
  "Amazonbot",
  "Bytespider",
  "CCBot",
  "cohere-training-data-crawler",
  "MistralAI-User",
  "DuckAssistBot",
  "AI2Bot",
  "Diffbot",
  "FirecrawlAgent",
  "ImagesiftBot",
  "SemrushBot",
  "AhrefsBot",
  "Baiduspider",
  "DuckDuckBot",
  "YandexBot",
  "Twitterbot",
  "LinkedInBot",
  "Discordbot",
  "TelegramBot",
  "Slackbot",
].map((token) => token.toLowerCase());

/** The configuration the middleware reads from the Pages environment. */
export interface MicaforgeEnv {
  /** The numeric site id Micaforge shows on the site's setup screen. */
  MICAFORGE_SITE_ID?: string;
  /** The site's ingest key. A secret: set it in the Pages dashboard, never in git. */
  MICAFORGE_INGEST_KEY?: string;
}

/** True when the user agent names a known AI agent or crawler. */
export function isAgent(userAgent: string | null): boolean {
  if (!userAgent) return false;
  const ua = userAgent.toLowerCase();
  return AGENT_TOKENS.some((token) => ua.includes(token));
}

/** HMAC-SHA256 as lowercase hex, with Web Crypto (no Node in a Worker). */
async function hmacHex(key: string, message: string): Promise<string> {
  const encoder = new TextEncoder();
  const cryptoKey = await crypto.subtle.importKey(
    "raw",
    encoder.encode(key),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign(
    "HMAC",
    cryptoKey,
    encoder.encode(message),
  );
  return [...new Uint8Array(signature)]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/**
 * Report one agent fetch. Resolves either way; never rejects.
 *
 * `startedAt` is `Date.now()` when the request arrived, so the duration covers
 * the middleware's own work as well as the asset fetch.
 */
export async function reportAgentFetch(
  env: MicaforgeEnv | undefined,
  request: Request,
  response: Response,
  startedAt: number,
): Promise<void> {
  try {
    const siteId = Number(env?.MICAFORGE_SITE_ID);
    const ingestKey = env?.MICAFORGE_INGEST_KEY;
    if (!ingestKey || !Number.isInteger(siteId) || siteId <= 0) return;

    const userAgent = request.headers.get("user-agent");
    if (!isAgent(userAgent)) return;

    // Pages streams HTML without a Content-Length, so the size of what the
    // crawler took is counted off a copy of the body. The copy is taken here,
    // before the first await, while the body is still unread; reading it
    // happens in the background and never delays the crawler's own response.
    const declared = response.headers.get("content-length");
    const copy =
      declared == null && request.method !== "HEAD" ? response.clone() : null;
    const length =
      declared != null
        ? Number(declared)
        : copy
          ? (await copy.arrayBuffer()).byteLength
          : NaN;

    const url = new URL(request.url);
    const row = {
      timestamp: new Date().toISOString(),
      user_agent: userAgent ?? "",
      method: request.method,
      hostname: url.hostname,
      pathname: url.pathname,
      querystring: url.search.length > 1 ? url.search.slice(1) : undefined,
      status: response.status,
      bytes: Number.isFinite(length) && length >= 0 ? length : undefined,
      duration_ms: Math.max(0, Date.now() - startedAt),
      content_type:
        response.headers.get("content-type")?.split(";")[0]?.trim() ||
        undefined,
      conditional:
        request.headers.has("if-none-match") ||
        request.headers.has("if-modified-since")
          ? 1
          : 0,
      // The address the operator's published ranges are checked against.
      // Micaforge keeps only a salted, rotating hash of it.
      ip: request.headers.get("cf-connecting-ip") ?? undefined,
    };

    // The signature covers `${t}.${body}` over these exact bytes, so the body is
    // serialised once and sent as it was signed.
    const body = JSON.stringify({ site_id: siteId, agent_events: [row] });
    const t = Math.floor(Date.now() / 1000);
    const v1 = await hmacHex(ingestKey, `${t}.${body}`);

    await fetch(ENDPOINT, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "user-agent": USER_AGENT,
        "x-micaforge-signature": `t=${t},v1=${v1}`,
      },
      body,
    });
  } catch {
    // Reporting a crawl is never worth an error on the page that served it.
  }
}
