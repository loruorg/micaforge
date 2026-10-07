/**
 * Number, duration and path formatting.
 *
 * One rule runs through all of it: never round a figure into a lie. A count of
 * 1,049 shows as 1,049 in a table and 1.0k only where the column is too narrow
 * to hold it, and a percentage never gains precision it does not have.
 */

const nf = new Intl.NumberFormat();

/** Full precision, grouped. The default for a table cell. */
export const num = (n: number | null | undefined): string =>
  n == null || !Number.isFinite(n) ? "—" : nf.format(Math.round(n));

/** Abbreviated. For axis labels and readouts where width is genuinely scarce. */
export function compact(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const a = Math.abs(n);
  if (a >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(a >= 10_000_000_000 ? 0 : 1)}B`;
  if (a >= 1_000_000) return `${(n / 1_000_000).toFixed(a >= 10_000_000 ? 0 : 1)}M`;
  if (a >= 10_000) return `${Math.round(n / 1000)}k`;
  if (a >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return nf.format(Math.round(n));
}

/** A fraction (0..1) as a percentage. */
export function pct(x: number | null | undefined, digits?: number): string {
  if (x == null || !Number.isFinite(x)) return "—";
  const v = x * 100;
  const d = digits ?? (Math.abs(v) < 1 && v !== 0 ? 1 : 0);
  return `${v.toFixed(d)}%`;
}

/** Seconds as an instrument-legible duration. */
export function duration(s: number | null | undefined): string {
  if (s == null || !Number.isFinite(s) || s < 0) return "—";
  const t = Math.round(s);
  if (t < 60) return `${t}s`;
  const m = Math.floor(t / 60);
  const rem = t % 60;
  if (m < 60) return rem ? `${m}m ${rem}s` : `${m}m`;
  const h = Math.floor(m / 60);
  return `${h}h ${m % 60}m`;
}

export function bytes(b: number | null | undefined): string {
  if (b == null || !Number.isFinite(b)) return "—";
  const units = ["B", "kB", "MB", "GB", "TB"];
  let i = 0;
  let v = b;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(i === 0 ? 0 : v >= 100 ? 0 : 1)} ${units[i]}`;
}

/** Milliseconds, for web vitals. */
export function ms(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  return v >= 1000 ? `${(v / 1000).toFixed(2)}s` : `${Math.round(v)}ms`;
}

/** "4 hours ago", "just now", "in 2 days". */
export function ago(t: number | string | null | undefined): string {
  if (t == null) return "never";
  const then = typeof t === "number" ? t : Date.parse(t);
  if (!Number.isFinite(then)) return "never";
  const diff = Date.now() - then;
  const abs = Math.abs(diff);
  const past = diff >= 0;
  const step = (n: number, unit: string) =>
    `${past ? "" : "in "}${n} ${unit}${n === 1 ? "" : "s"}${past ? " ago" : ""}`;

  if (abs < 45_000) return past ? "just now" : "in a moment";
  if (abs < 3_600_000) return step(Math.round(abs / 60_000), "minute");
  if (abs < 86_400_000) return step(Math.round(abs / 3_600_000), "hour");
  if (abs < 2_592_000_000) return step(Math.round(abs / 86_400_000), "day");
  if (abs < 31_536_000_000) return step(Math.round(abs / 2_592_000_000), "month");
  return step(Math.round(abs / 31_536_000_000), "year");
}

/**
 * A crawl freshness gap. Reported in the largest honest unit, because "312
 * hours" tells an operator less than "13 days".
 */
export function gapHours(h: number | null | undefined): string {
  if (h == null || !Number.isFinite(h)) return "never crawled";
  if (h < 1) return "under an hour";
  if (h < 48) return `${Math.round(h)} hours`;
  const d = h / 24;
  if (d < 60) return `${Math.round(d)} days`;
  return `${Math.round(d / 30)} months`;
}

/** Trims a path for display without hiding which page it is. */
export function shortPath(p: string, max = 46): string {
  if (!p) return "/";
  if (p.length <= max) return p;
  const parts = p.split("/").filter(Boolean);
  if (parts.length <= 2) return `${p.slice(0, max - 1)}…`;
  return `/${parts[0]}/…/${parts[parts.length - 1]}`;
}

export function hostOf(u: string): string {
  try {
    return new URL(u).hostname.replace(/^www\./, "");
  } catch {
    return u;
  }
}

/** Signed change between two periods, as a fraction. Null when undefined. */
export function delta(now: number, before: number | null | undefined): number | null {
  if (before == null || !Number.isFinite(before)) return null;
  if (before === 0) return now === 0 ? 0 : null;
  return (now - before) / before;
}

const CHANNEL_LABELS: Record<string, string> = {
  direct: "Direct",
  search: "Search",
  answer_engine: "Answer engines",
  social: "Social",
  referral: "Referral",
  email: "Email",
  paid: "Paid",
  internal: "Internal",
};
export const channelLabel = (c: string): string => CHANNEL_LABELS[c] ?? c;

const PURPOSE_LABELS: Record<string, string> = {
  training: "Training corpus",
  rag: "Answering a question now",
  search: "Search index",
  preview: "Link preview",
  eval: "Evaluation",
  unknown: "Undeclared",
};
export const purposeLabel = (p: string): string => PURPOSE_LABELS[p] ?? p;

/* ── the gap ratio ───────────────────────────────────────────────────────────
   This lives in the shared lib, not in the dashboard bundle, because the
   marketing pages print the same figure. Two implementations of one rule is
   how the landing page came to print `129:1` while the app printed `2.6 : 1`
   for the same relation. */

/**
 * A gap ratio as figures: `2.6 : 1`, `129 : 1`.
 *
 * The colon is spaced because this figure is set large in a readout, and a
 * crushed `129:1` reads as one token rather than a relation. Both sides of the
 * product print it this way.
 */
export function ratioText(r: number | null | undefined): string {
  if (r == null || !Number.isFinite(r) || r < 0) return "—";
  // Precision follows the magnitude. One decimal is right in the middle of the
  // range and wrong at both ends: it prints a crushed whole number above 100,
  // and below 0.05 it prints "0.0 : 1", which states that nothing was taken
  // when the truth is that far more came back than was ever fetched. That case
  // is real: it is what an Invisible page looks like.
  if (r >= 100) return `${num(Math.round(r))} : 1`;
  if (r >= 1) return `${trim(r.toFixed(1))} : 1`;
  if (r >= 0.1) return `${trim(r.toFixed(2))} : 1`;
  // Under a tenth the relation reads better the other way up, because that is
  // how a person would say it: "twenty readers for every fetch".
  return `1 : ${ratioText(1 / r).replace(" : 1", "")}`;
}

/** Drops trailing zeros so "0.50" reads as "0.5" and "1.0" as "1". */
function trim(v: string): string {
  return v.replace(/(\.\d*?)0+$/, "$1").replace(/\.$/, "");
}

/**
 * The same figure from the two counts it is made of, with the two honest
 * states a ratio cannot express: nothing recorded at all, and a crawl that
 * never came back.
 */
export function gapRatio(
  taken: number | null | undefined,
  returned: number | null | undefined,
): string {
  const t = taken ?? 0;
  const r = returned ?? 0;
  if (!t && !r) return "no record";
  if (!r) return "none returned";
  return ratioText(t / r);
}
