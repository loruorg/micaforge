/**
 * The naming and figure rules the dashboard screens share.
 *
 * Two things kept drifting between screens, so both are settled here and
 * nowhere else:
 *
 *   1. THE RATIO. The Citation Gap is the metric this product is named for, and
 *      it was printed three different ways: rounded to a whole number on the
 *      record, to one decimal on the gap report, and rounded again on the site
 *      chooser. A gap of 2.6 fetches per arrival is not 3, and rounding it into
 *      an integer destroys the reading. One rule now: one decimal below a
 *      hundred, whole numbers above it, where a decimal place would be noise.
 *
 *   2. THE NAMES. A catalog slug is a key, not a name. The server sends the
 *      display label where it has one (`label`, `operator_label`,
 *      `purpose_label`); where it does not, the key is title-cased for display
 *      and left untouched everywhere else, so filtering, sorting and every URL
 *      still carry the slug the server knows.
 */
import { num } from "../lib/format";

/* ── the ratio ───────────────────────────────────────────────────────────── */

/**
 * Re-exported from the shared lib, where the marketing pages can reach it too.
 * The rule itself is documented there.
 */
export { ratioText, gapRatio } from "../lib/format";

/* ── the names ───────────────────────────────────────────────────────────── */

/**
 * Names that are not what a title-caser would produce. Only real, checkable
 * product names go in here: nothing is invented, and anything absent falls
 * through to the plain title-case below.
 */
const NAMES: Record<string, string> = {
  /* answer engines and search */
  chatgpt: "ChatGPT",
  "chatgpt-user": "ChatGPT-User",
  openai: "OpenAI",
  "oai-searchbot": "OAI-SearchBot",
  gptbot: "GPTBot",
  perplexity: "Perplexity",
  perplexitybot: "PerplexityBot",
  "perplexity-user": "Perplexity-User",
  claude: "Claude",
  claudebot: "ClaudeBot",
  "claude-web": "Claude-Web",
  "claude-user": "Claude-User",
  anthropic: "Anthropic",
  gemini: "Gemini",
  "google-extended": "Google-Extended",
  googlebot: "Googlebot",
  google: "Google",
  bing: "Bing",
  bingbot: "Bingbot",
  copilot: "Copilot",
  duckduckgo: "DuckDuckGo",
  duckassistbot: "DuckAssistBot",
  yandex: "Yandex",
  baidu: "Baidu",
  ecosia: "Ecosia",
  brave: "Brave",
  applebot: "Applebot",
  "applebot-extended": "Applebot-Extended",
  apple: "Apple",
  meta: "Meta",
  "meta-externalagent": "Meta-ExternalAgent",
  facebookbot: "FacebookBot",
  bytedance: "ByteDance",
  bytespider: "Bytespider",
  amazonbot: "Amazonbot",
  ccbot: "CCBot",
  "common-crawl": "Common Crawl",
  cohere: "Cohere",
  "cohere-ai": "Cohere-AI",
  mistral: "Mistral",
  "mistralai-user": "MistralAI-User",
  youbot: "YouBot",
  diffbot: "Diffbot",
  petalbot: "PetalBot",
  semrushbot: "SemrushBot",
  ahrefsbot: "AhrefsBot",
  /* the places readers come from */
  "hacker-news": "Hacker News",
  hackernews: "Hacker News",
  github: "GitHub",
  gitlab: "GitLab",
  youtube: "YouTube",
  linkedin: "LinkedIn",
  reddit: "Reddit",
  x: "X",
  twitter: "X",
  mastodon: "Mastodon",
  bluesky: "Bluesky",
  stackoverflow: "Stack Overflow",
  producthunt: "Product Hunt",
  "dev-to": "DEV",
  substack: "Substack",
  medium: "Medium",
  slack: "Slack",
  discord: "Discord",
  telegram: "Telegram",
  whatsapp: "WhatsApp",
  npm: "npm",
  rss: "RSS",
  email: "Email",
  direct: "Direct",
};

/** True where the string is an address rather than a name, and must not be cased. */
const isAddress = (s: string) => s.includes(".") || s.includes("/") || s.includes("@");

/**
 * A slug as a name: `hacker-news` reads "Hacker News", `perplexity` reads
 * "Perplexity". Hostnames and paths are left exactly as they arrived, because
 * `news.ycombinator.com` is already written correctly.
 */
export function titleCase(key: string | null | undefined): string {
  const k = (key ?? "").trim();
  if (!k) return "";
  const known = NAMES[k.toLowerCase()];
  if (known) return known;
  if (isAddress(k)) return k;
  return k
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map((w) => NAMES[w.toLowerCase()] ?? w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/**
 * How a source row is named on screen. The server's own label wins; a bare key
 * is cased for display only, and the key itself is what gets filtered on.
 */
export function sourceName(row: { key?: string | null; label?: string | null }): string {
  const label = (row.label ?? "").trim();
  if (label) return label;
  const key = (row.key ?? "").trim();
  return key ? titleCase(key) : "Direct";
}

/**
 * How a named client is written: the catalog's display name where the server
 * has one, the cased slug where it does not.
 */
export function agentName(
  label: string | null | undefined,
  key: string | null | undefined,
): string {
  const l = (label ?? "").trim();
  if (l) return l;
  const k = (key ?? "").trim();
  return k ? titleCase(k) : "Unnamed client";
}

/* ── the ink behind a row ────────────────────────────────────────────────── */

/**
 * Scales a sheet's shares so the leading row's ink runs the full width of the
 * cell and every row below it is read against that leader.
 *
 * A share of the whole is the honest number and it stays in the figures column;
 * as a length it is useless, because a top page with nine per cent and the one
 * under it with seven draw two stubs a reader cannot tell apart. Ranked against
 * the leader the same two rows are plainly different lengths, which is what the
 * ink is for.
 */
export function againstLeader<T>(
  rows: readonly T[],
  share: (row: T) => number | null | undefined,
): (row: T) => number | undefined {
  let top = 0;
  for (const row of rows) {
    const v = share(row);
    if (v != null && Number.isFinite(v) && v > top) top = v;
  }
  return (row: T) => {
    if (top <= 0) return undefined;
    const v = share(row);
    if (v == null || !Number.isFinite(v) || v <= 0) return undefined;
    return Math.min(1, v / top);
  };
}
