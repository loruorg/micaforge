/**
 * Demo data for the homepage. Every figure here is illustrative, and every place
 * that shows one says "Demo data" beside it: Micaforge has no customer whose
 * numbers it could show, and it does not invent one.
 *
 * Deterministic on purpose, so the static build and the page a visitor sees are
 * the same page every time.
 */

/** A small seeded generator, so the "noise" in the demo is the same on every build. */
function rng(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

export interface ChannelSeries {
  key: "search" | "direct" | "other" | "ai";
  label: string;
  values: number[];
}

export const DAYS = 30;

/**
 * Daily visitors by channel for the hero chart. AI answers start small and grow,
 * which is the story the page is telling.
 */
export function channelSeries(): ChannelSeries[] {
  const r = rng(7);
  const search: number[] = [];
  const direct: number[] = [];
  const other: number[] = [];
  const ai: number[] = [];
  for (let d = 0; d < DAYS; d++) {
    const t = d / (DAYS - 1);
    // A working-week rhythm: quieter at the weekend.
    const week = [1, 1.04, 1.06, 1.03, 0.97, 0.72, 0.68][d % 7];
    const base = (1350 + 260 * t) * week * (0.94 + 0.12 * r());
    search.push(base * (0.46 - 0.05 * t));
    direct.push(base * (0.27 + 0.01 * r()));
    other.push(base * (0.22 - 0.02 * t));
    ai.push(base * (0.035 + 0.11 * t * t + 0.01 * r()));
  }
  return [
    { key: "search", label: "Search", values: search },
    { key: "direct", label: "Direct", values: direct },
    { key: "other", label: "Social and referral", values: other },
    { key: "ai", label: "AI answers", values: ai },
  ];
}

/** Catmull-Rom through the points, as cubic Beziers. Smooth, and it passes through every point. */
function smooth(points: [number, number][]): string {
  if (points.length < 2) return "";
  let d = `M${points[0][0].toFixed(1)},${points[0][1].toFixed(1)}`;
  for (let i = 0; i < points.length - 1; i++) {
    const p0 = points[i - 1] ?? points[i];
    const p1 = points[i];
    const p2 = points[i + 1];
    const p3 = points[i + 2] ?? p2;
    const c1x = p1[0] + (p2[0] - p0[0]) / 6;
    const c1y = p1[1] + (p2[1] - p0[1]) / 6;
    const c2x = p2[0] - (p3[0] - p1[0]) / 6;
    const c2y = p2[1] - (p3[1] - p1[1]) / 6;
    d += ` C${c1x.toFixed(1)},${c1y.toFixed(1)} ${c2x.toFixed(1)},${c2y.toFixed(1)} ${p2[0].toFixed(1)},${p2[1].toFixed(1)}`;
  }
  return d;
}

export interface StackBand {
  key: ChannelSeries["key"];
  label: string;
  /** The filled band between this series and the one below it. */
  area: string;
  /** The band's top edge, drawn as a line. */
  line: string;
}

/**
 * Stack the series bottom-up into bands on a `width` x `height` plot, with the
 * ceiling rounded up to a clean grid step. Returns the bands and the grid.
 */
export function stackBands(series: ChannelSeries[], width: number, height: number) {
  const n = series[0].values.length;
  const totals = Array.from({ length: n }, (_, i) => series.reduce((sum, s) => sum + s.values[i], 0));
  const peak = Math.max(...totals);
  const step = peak > 1500 ? 500 : 250;
  const ceiling = Math.ceil((peak * 1.08) / step) * step;
  const x = (i: number) => (i / (n - 1)) * width;
  const y = (v: number) => height - (v / ceiling) * height;

  const below = new Array<number>(n).fill(0);
  const bands: StackBand[] = series.map((s) => {
    const lower = below.slice();
    const upper = lower.map((v, i) => v + s.values[i]);
    for (let i = 0; i < n; i++) below[i] = upper[i];
    const top = upper.map((v, i): [number, number] => [x(i), y(v)]);
    const bottom = lower.map((v, i): [number, number] => [x(i), y(v)]).reverse();
    const topPath = smooth(top);
    const bottomPath = smooth(bottom).replace(/^M/, "L");
    return { key: s.key, label: s.label, area: `${topPath} ${bottomPath} Z`, line: topPath };
  });

  const grid: { y: number; label: string }[] = [];
  for (let v = 0; v <= ceiling; v += step) grid.push({ y: y(v), label: v >= 1000 ? `${v / 1000}k` : String(v) });
  return { bands, grid, totals };
}

export const STATS = [
  { label: "Visitors", value: 48213, delta: "+12%", tone: "people" },
  { label: "Pageviews", value: 131904, delta: "+9%", tone: "people" },
  { label: "From AI answers", value: 3904, delta: "+64%", tone: "ai" },
  { label: "AI crawler visits", value: 212480, delta: "+23%", tone: "ai" },
] as const;

export const SOURCES = [
  { name: "Google", visitors: 18240, ai: false },
  { name: "Direct", visitors: 9871, ai: false },
  { name: "ChatGPT", visitors: 2412, ai: true },
  { name: "GitHub", visitors: 1985, ai: false },
  { name: "Perplexity", visitors: 812, ai: true },
  { name: "Claude", visitors: 371, ai: true },
] as const;

export const CRAWLERS = [
  { name: "GPTBot", owner: "OpenAI", purpose: "Training", visits: 81200, state: "verified" },
  { name: "ClaudeBot", owner: "Anthropic", purpose: "Training", visits: 44900, state: "verified" },
  { name: "Googlebot", owner: "Google", purpose: "Search", visits: 38050, state: "verified" },
  { name: "PerplexityBot", owner: "Perplexity", purpose: "Search", visits: 19610, state: "verified" },
  { name: "ChatGPT-User", owner: "OpenAI", purpose: "Assistant", visits: 8930, state: "verified" },
  { name: "ClaudeBot", owner: "Claims Anthropic", purpose: "Unknown", visits: 2140, state: "spoofed" },
] as const;

export type LiveKind = "people" | "ai-referral" | "crawler";

export const LIVE: { kind: LiveKind; who: string; page: string; note?: string }[] = [
  { kind: "ai-referral", who: "ChatGPT", page: "/docs/install" },
  { kind: "crawler", who: "GPTBot", page: "/pricing", note: "verified" },
  { kind: "people", who: "Google", page: "/blog/rate-limits" },
  { kind: "crawler", who: "ClaudeBot", page: "/docs/api", note: "verified" },
  { kind: "ai-referral", who: "Perplexity", page: "/compare/stripe" },
  { kind: "people", who: "Direct", page: "/" },
  { kind: "crawler", who: "OAI-SearchBot", page: "/changelog", note: "verified" },
  { kind: "people", who: "Hacker News", page: "/blog/launch" },
  { kind: "ai-referral", who: "Claude", page: "/docs/webhooks" },
  { kind: "crawler", who: "PerplexityBot", page: "/docs/sdks", note: "verified" },
];

export const CHANNEL_SPLIT = [
  { key: "search", label: "Search", share: 41 },
  { key: "direct", label: "Direct", share: 24 },
  { key: "referral", label: "Referral", share: 15 },
  { key: "ai", label: "AI answers", share: 11 },
  { key: "social", label: "Social", share: 9 },
] as const;

export const GAP_PAGES = [
  { path: "/docs/api/authentication", crawled: 8421, returned: 12 },
  { path: "/blog/why-cookieless", crawled: 3110, returned: 486 },
  { path: "/docs/install", crawled: 6002, returned: 931 },
  { path: "/pricing", crawled: 2640, returned: 9 },
] as const;
