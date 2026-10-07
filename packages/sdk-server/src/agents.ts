/**
 * A deliberately small mirror of the server's agent catalog.
 *
 * # Why this is not the catalog
 *
 * The authoritative catalog lives in the server, at
 * `apps/backend/crates/agents/src/catalog.rs`, and it is the only thing allowed to
 * decide what an agent *is*. It carries the operator, the purpose, the published IP
 * range documents, the reverse-DNS suffixes and the robots position for every entry,
 * and it is what sets `agent_id`, `operator`, `purpose` and `verified` on the row.
 *
 * This file exists for one job: to answer, on the caller's own machine and without a
 * network call, "is this worth sending as an agent hit?". It is a filter, not a
 * verdict. Copying the full catalog here would give a customer's process a stale
 * second opinion that could disagree with the dashboard, which is worse than having no
 * opinion at all.
 *
 * So:
 *
 * - Every token below is copied from the server catalog. None is invented.
 * - The list is partial on purpose and will fall behind. That is safe, because a hit
 *   the mirror does not recognise is still sent, as a human row, and the server
 *   reclassifies it. Nothing is lost by the mirror being out of date.
 * - The mirror never sets `agent_id`, `operator`, `purpose` or `verified` on the wire.
 *   {@link classify} returns the operator only so that a dry run can print something
 *   readable; it is not transmitted.
 *
 * `bin/micaforge-shipper.mjs` keeps its own copy of this list. That duplication is
 * deliberate: the shipper has to run straight from `npx` with no build step and no
 * import graph, so it cannot reach into `dist/`. Both copies name this file and the
 * Rust catalog as their source, and both are filters that the server overrules.
 */

/** What the local mirror was able to say about a user agent. */
export interface AgentGuess {
  /** The catalog slug, for display only. Never sent to the server. */
  slug: string;
  /** The operator, for display only. Never sent to the server. */
  operator: string;
}

/**
 * `[token, slug, operator]`, taken from the server catalog.
 *
 * The token is the product string an operator writes into its user agent. Matching is
 * a case-insensitive substring test because a real user agent wraps the token in a
 * sentence, and operators change the version suffix without notice:
 * `Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2;
 * +https://openai.com/gptbot)`.
 */
const MIRROR: ReadonlyArray<readonly [string, string, string]> = [
  // OpenAI. Three tokens, three jobs. Blocking one does not block the others, which is
  // exactly why they are reported apart rather than as one "OpenAI" row.
  ["GPTBot", "gptbot", "openai"],
  ["OAI-SearchBot", "oai-searchbot", "openai"],
  ["ChatGPT-User", "chatgpt-user", "openai"],
  // Anthropic.
  ["ClaudeBot", "claudebot", "anthropic"],
  ["Claude-User", "claude-user", "anthropic"],
  ["Claude-SearchBot", "claude-searchbot", "anthropic"],
  ["anthropic-ai", "anthropic-ai", "anthropic"],
  ["Claude-Web", "claude-web", "anthropic"],
  // Perplexity.
  ["PerplexityBot", "perplexitybot", "perplexity"],
  ["Perplexity-User", "perplexity-user", "perplexity"],
  // Google. `Google-Extended` is a robots token only and never appears in a user
  // agent, so it is absent here as it is in the server catalog's pattern list.
  ["Googlebot", "googlebot", "google"],
  ["GoogleOther", "googleother", "google"],
  ["Google-CloudVertexBot", "google-cloudvertexbot", "google"],
  ["Google-InspectionTool", "google-inspectiontool", "google"],
  // Microsoft.
  ["bingbot", "bingbot", "microsoft"],
  ["BingPreview", "bingpreview", "microsoft"],
  // Meta.
  ["meta-externalagent", "meta-externalagent", "meta"],
  ["meta-externalfetcher", "meta-externalfetcher", "meta"],
  ["FacebookBot", "facebookbot", "meta"],
  ["facebookexternalhit", "facebookexternalhit", "meta"],
  // The rest of the AI crawl surface.
  ["Applebot-Extended", "applebot-extended", "apple"],
  ["Amazonbot", "amazonbot", "amazon"],
  ["Bytespider", "bytespider", "bytedance"],
  ["CCBot", "ccbot", "common-crawl"],
  ["cohere-training-data-crawler", "cohere-training-data-crawler", "cohere"],
  ["MistralAI-User", "mistralai-user", "mistral"],
  ["DuckAssistBot", "duckassistbot", "duckduckgo"],
  ["AI2Bot", "ai2bot", "allen-institute"],
  ["Diffbot", "diffbot", "diffbot"],
  ["FirecrawlAgent", "firecrawl", "firecrawl"],
  ["ImagesiftBot", "imagesiftbot", "imagesift"],
  // Classic crawlers and link unfurlers. Not AI, but they are non-human fetches and
  // counting them as readers would be a lie about the audience.
  ["SemrushBot-OCOB", "semrushbot-ocob", "semrush"],
  ["SemrushBot", "semrushbot", "semrush"],
  ["AhrefsBot", "ahrefsbot", "ahrefs"],
  ["Baiduspider", "baiduspider", "baidu"],
  ["DuckDuckBot", "duckduckbot", "duckduckgo"],
  ["Twitterbot", "twitterbot", "x"],
  ["LinkedInBot", "linkedinbot", "linkedin"],
  ["Discordbot", "discordbot", "discord"],
  ["TelegramBot", "telegrambot", "telegram"],
];

/**
 * The mirror, lowercased and sorted longest token first.
 *
 * The sort is what makes the answer deterministic. `SemrushBot` is a substring of
 * `SemrushBot-OCOB`, so first-match-in-list-order would be wrong roughly half the time.
 * Ties break alphabetically so the result never depends on the sort being stable. This
 * is the same rule the server's `find_by_ua` uses; the two must not disagree on the
 * entries they share.
 */
const INDEX: ReadonlyArray<readonly [string, string, string]> = MIRROR.map(
  ([token, slug, operator]) => [token.toLowerCase(), slug, operator] as const,
).sort((a, b) => b[0].length - a[0].length || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));

/**
 * Name the client behind a user agent, or return `null`.
 *
 * `null` means "the mirror does not know", never "this was a person". A `null` here
 * sends the hit as a human row and lets the server apply the full catalog and its bot
 * heuristics, which is the only place either judgement is made honestly.
 */
export function classify(userAgent: string | undefined | null): AgentGuess | null {
  if (!userAgent) return null;
  const haystack = userAgent.toLowerCase();
  for (const [token, slug, operator] of INDEX) {
    if (haystack.includes(token)) return { slug, operator };
  }
  return null;
}

/** True when the mirror can name the client. Sugar over {@link classify}. */
export function isAgent(userAgent: string | undefined | null): boolean {
  return classify(userAgent) != null;
}

/** Every token the mirror carries, longest first. Exported for the tests and `--help`. */
export function mirrorTokens(): string[] {
  return INDEX.map(([token]) => token);
}
