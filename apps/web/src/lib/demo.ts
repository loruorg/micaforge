/**
 * Demonstration records.
 *
 * Every figure this module produces is illustrative and is labelled as such
 * wherever it is shown. It exists because a page about traffic shapes has to
 * show a traffic shape, and Micaforge has no customers whose data it could
 * borrow. It is deterministic (a seeded PRNG) so a rebuild never silently
 * changes what a reader saw yesterday.
 *
 * The shapes are argued, not random: machine fetches arrive in bursts around a
 * publish and then settle into a re-crawl rhythm, while human reading follows a
 * weekly cycle with a weekend trough. That difference is the point.
 */

function mulberry32(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export interface DemoRecord {
  times: number[];
  machines: number[];
  readers: number[];
  answers: number[];
  annotations: { at: number; label: string; tone?: "neutral" | "warn" | "good" }[];
}

/**
 * `endsToday` is passed in rather than read from the clock so a static build and
 * the hydrated client agree on the axis and React does not warn about it.
 */
export function demoRecord(days = 60, seed = 20260821, endMs?: number): DemoRecord {
  const rnd = mulberry32(seed);
  const end = endMs ?? Date.UTC(2026, 7, 21);
  const day = 86_400_000;

  const times: number[] = [];
  const machines: number[] = [];
  const readers: number[] = [];
  const answers: number[] = [];

  // Two publishes in the window; each one pulls the crawlers in hard.
  const publishes = [Math.floor(days * 0.22), Math.floor(days * 0.63)];

  for (let i = 0; i < days; i++) {
    const t = end - (days - 1 - i) * day;
    times.push(t);

    const dow = new Date(t).getUTCDay();
    const weekend = dow === 0 || dow === 6;

    // Readers: a weekly rhythm with a weekend trough and slow organic growth.
    const base = 780 + i * 6;
    const week = weekend ? 0.52 : 1 + 0.12 * Math.sin((i / 7) * Math.PI * 2);
    const readerN = Math.max(60, base * week * (0.88 + rnd() * 0.24));
    readers.push(Math.round(readerN));

    // Machines: a re-crawl floor, plus a sharp burst for a few days after a
    // publish, plus a persistent upward drift as more operators appear.
    let m = 1400 + i * 34;
    for (const p of publishes) {
      const d = i - p;
      if (d >= 0 && d < 9) m += 5200 * Math.exp(-d / 2.6);
    }
    m *= 0.9 + rnd() * 0.2;
    machines.push(Math.round(m));

    // Answer-engine arrivals: small, growing, and stubbornly out of proportion
    // to how much was taken. That disproportion is the product's whole subject.
    answers.push(Math.round(Math.max(0, 6 + i * 0.55 + (rnd() - 0.4) * 9)));
  }

  return {
    times,
    machines,
    readers,
    answers,
    annotations: [
      { at: publishes[0], label: "Guide published", tone: "neutral" },
      { at: publishes[0] + 3, label: "GPTBot: 4,102 fetches", tone: "warn" },
      { at: publishes[1] + 2, label: "ClaudeBot re-crawl", tone: "warn" },
      { at: days - 9, label: "First answer arrivals", tone: "good" },
    ],
  };
}

/**
 * The same demonstration record, `steps` days later.
 *
 * The drum on the landing page runs: every few seconds a day is written at the
 * right edge and the oldest falls off the left. The new days follow the same
 * argued shapes as `demoRecord` (a re-crawl floor that keeps rising, a burst of
 * fetching after each publish, a weekly reading rhythm, answer arrivals that
 * grow far slower than the fetching), and each one comes from its own seed, so
 * day N is the same day however the reader got there.
 */
export function demoAdvance(days: number, steps: number, seed = 20260821): DemoRecord {
  const base = demoRecord(days, seed);
  if (steps <= 0) return base;

  const day = 86_400_000;
  const times = base.times.slice();
  const machines = base.machines.slice();
  const readers = base.readers.slice();
  const answers = base.answers.slice();
  const publishedAt: number[] = [];

  for (let k = 1; k <= steps; k++) {
    const i = days - 1 + k;
    const rnd = mulberry32(seed ^ Math.imul(k, 2654435761));
    const t = base.times[base.times.length - 1] + k * day;
    const dow = new Date(t).getUTCDay();
    const weekend = dow === 0 || dow === 6;

    const week = weekend ? 0.52 : 1 + 0.12 * Math.sin((i / 7) * Math.PI * 2);
    readers.push(Math.round(Math.max(60, (780 + i * 6) * week * (0.88 + rnd() * 0.24))));

    // A publish every three weeks of drum, and the crawlers answer it.
    if (k % 21 === 7) publishedAt.push(k);
    let m = 1400 + i * 34;
    for (const p of publishedAt) {
      const d = k - p;
      if (d >= 0 && d < 9) m += 5200 * Math.exp(-d / 2.6);
    }
    machines.push(Math.round(m * (0.9 + rnd() * 0.2)));
    answers.push(Math.round(Math.max(0, 6 + i * 0.55 + (rnd() - 0.4) * 9)));
    times.push(t);
  }

  const cut = (xs: number[]) => xs.slice(steps);
  const annotations = [
    ...base.annotations,
    ...publishedAt.flatMap((p) => [
      { at: days - 1 + p, label: "Guide published", tone: "neutral" as const },
      { at: days - 1 + p + 2, label: "Re-crawl burst", tone: "warn" as const },
    ]),
  ]
    .map((a) => ({ ...a, at: a.at - steps }))
    .filter((a) => a.at >= 0 && a.at < days);

  return {
    times: cut(times),
    machines: cut(machines),
    readers: cut(readers),
    answers: cut(answers),
    annotations,
  };
}

export interface DemoGapPage {
  path: string;
  title: string;
  agent_fetches: number;
  answer_arrivals: number;
  human_pageviews: number;
  ratio: number;
  verdict: "extracted" | "compounding" | "invisible" | "quiet";
  spark: number[];
}

/** A page-level Citation Gap table, showing all four verdicts honestly. */
export function demoGapPages(): DemoGapPage[] {
  const rows: Omit<DemoGapPage, "ratio" | "spark">[] = [
    {
      path: "/docs/api/authentication",
      title: "Authentication",
      agent_fetches: 8421,
      answer_arrivals: 12,
      human_pageviews: 940,
      verdict: "extracted",
    },
    {
      path: "/blog/why-cookieless-works",
      title: "Why cookieless works",
      agent_fetches: 3110,
      answer_arrivals: 486,
      human_pageviews: 2205,
      verdict: "compounding",
    },
    {
      path: "/docs/self-hosting/install",
      title: "Install",
      agent_fetches: 6002,
      answer_arrivals: 31,
      human_pageviews: 1512,
      verdict: "extracted",
    },
    {
      path: "/changelog",
      title: "Changelog",
      agent_fetches: 210,
      answer_arrivals: 174,
      human_pageviews: 690,
      verdict: "invisible",
    },
    {
      path: "/docs/tracking/spa-routing",
      title: "SPA routing",
      agent_fetches: 1890,
      answer_arrivals: 240,
      human_pageviews: 830,
      verdict: "compounding",
    },
    {
      path: "/legal/dpa",
      title: "Data processing addendum",
      agent_fetches: 44,
      answer_arrivals: 0,
      human_pageviews: 18,
      verdict: "quiet",
    },
  ];
  return rows.map((r, i) => {
    const rnd = mulberry32(9000 + i);
    return {
      ...r,
      ratio: r.answer_arrivals === 0 ? Infinity : r.agent_fetches / r.answer_arrivals,
      spark: Array.from({ length: 14 }, () => 0.3 + rnd() * 0.7),
    };
  });
}

export interface DemoAgent {
  slug: string;
  /** The user-agent string that arrived. It is a claim, not an identity. */
  name: string;
  /**
   * The company the traffic is charged to, and null wherever it cannot be
   * charged to anyone. A user agent is an assertion: it is only an operator
   * once the source address has matched that operator's published ranges, so
   * an unverified or failed row carries no company name at all. Reporting a
   * spoofed ClaudeBot as Anthropic is the exact mistake this product exists to
   * refuse, and demonstration data does not get to make it either.
   */
  operator: string | null;
  purpose: string;
  fetches: number;
  paths: number;
  verified: "verified" | "unverified" | "failed";
  /** `path` = allowed at the root with a Disallow on one branch. */
  robots: "allowed" | "disallowed" | "unlisted" | "path";
  /**
   * Fetches that landed against a Disallow rule. Null wherever the identity did
   * not verify: those fetches are still counted, they simply cannot be charged
   * to an operator, so no number is shown on the row.
   */
  violations: number | null;
}

/**
 * The observed side of the policy report, against the robots.txt shown beside
 * it on the landing page: GPTBot and ClaudeBot allowed, GPTBot's /docs/api/
 * branch disallowed, and Google-Extended, CCBot and Bytespider disallowed
 * outright.
 *
 * Read the rows as a demonstration of the attribution rule. Only the first row
 * carries a violation count, because it is the only row that is both verified
 * and covered by a Disallow. The three disallowed rows below it fetched anyway,
 * and none of them verified, so none of them names a company and none of them
 * is charged.
 */
export function demoAgents(): DemoAgent[] {
  return [
    { slug: "gptbot", name: "GPTBot", operator: "OpenAI", purpose: "training", fetches: 8421, paths: 604, verified: "verified", robots: "path", violations: 118 },
    { slug: "claudebot", name: "ClaudeBot", operator: "Anthropic", purpose: "training", fetches: 6120, paths: 588, verified: "verified", robots: "allowed", violations: 0 },
    { slug: "perplexitybot", name: "PerplexityBot", operator: "Perplexity", purpose: "search", fetches: 3044, paths: 431, verified: "verified", robots: "unlisted", violations: 0 },
    { slug: "google-extended", name: "Google-Extended", operator: null, purpose: "training", fetches: 2870, paths: 512, verified: "unverified", robots: "disallowed", violations: null },
    { slug: "ccbot", name: "CCBot", operator: null, purpose: "training", fetches: 1902, paths: 470, verified: "unverified", robots: "disallowed", violations: null },
    { slug: "bytespider", name: "Bytespider", operator: null, purpose: "training", fetches: 1466, paths: 302, verified: "failed", robots: "disallowed", violations: null },
    { slug: "oai-searchbot", name: "OAI-SearchBot", operator: "OpenAI", purpose: "search", fetches: 1120, paths: 288, verified: "verified", robots: "unlisted", violations: 0 },
    { slug: "meta-externalagent", name: "meta-externalagent", operator: null, purpose: "training", fetches: 806, paths: 194, verified: "unverified", robots: "unlisted", violations: null },
  ];
}
