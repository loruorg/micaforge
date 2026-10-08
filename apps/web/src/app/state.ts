/**
 * The station's shared settings: what the whole dashboard is currently reading.
 *
 * Four things travel with the operator from screen to screen: the interval the
 * caliper is set to, which audience the pens are patched to, what the reading is
 * being compared against, and the conditions clipped to the record. Every screen
 * reads them from here, so filing a filter on the pages sheet is still filed
 * when the operator walks over to sources.
 *
 * Two decisions worth knowing about:
 *
 *  1. THE INTERVAL IS ONE STRING. A named range ("30d") and a measured one
 *     ("2026-07-01..2026-08-01") both live in `range`, because a screen asking
 *     "what am I reading?" wants one answer, not two fields to reconcile. The
 *     `..` form is the same one `record/TopSheets.rangeEnvelope` already unpicks,
 *     and `customRange` is a derived view of it rather than a rival source of
 *     truth. Setting either keeps the other honest.
 *  2. SETTINGS ARE PER SITE. An operator watching a documentation site on a
 *     90-day window and a marketing site on 24 hours is doing something normal,
 *     so the interval, the audience and the comparison are remembered against
 *     the site they were set on. Conditions are NOT remembered: a path filter
 *     from one site is meaningless on another, and restoring one silently would
 *     be a reading nobody asked for.
 */
import { create } from "zustand";
import type { Audience, Compare, Envelope, Filter } from "../lib/api";

/* ── what gets written down ──────────────────────────────────────────────── */

const STORE_PREFIX = "mf-station:";

interface Remembered {
  range: string;
  audience: Audience;
  compare: Compare;
}

const DEFAULTS: Remembered = { range: "30d", audience: "human", compare: "none" };

const AUDIENCES: Audience[] = ["human", "agent", "all"];
const COMPARES: Compare[] = ["none", "previous", "year"];

/** A range is either one of the contract's names or a measured `start..end`. */
const NAMED_RANGES = new Set([
  "today",
  "yesterday",
  "24h",
  "7d",
  "30d",
  "90d",
  "12mo",
  "mtd",
  "ytd",
  "all",
]);

const isMeasured = (range: string): boolean => range.indexOf("..") > 0;

const isRange = (v: unknown): v is string =>
  typeof v === "string" && (NAMED_RANGES.has(v) || isMeasured(v));

/**
 * Reads a site's remembered settings. Every failure mode here (no storage, a
 * private window, a half-written value, a value from an older build) resolves
 * to the defaults rather than throwing, because a dashboard that will not open
 * because of a stale string is worse than one that opens on the wrong interval.
 */
function readRemembered(siteKey: string | null): Remembered {
  if (!siteKey) return DEFAULTS;
  try {
    const raw = localStorage.getItem(STORE_PREFIX + siteKey);
    if (!raw) return DEFAULTS;
    const parsed = JSON.parse(raw) as Partial<Remembered>;
    return {
      range: isRange(parsed.range) ? parsed.range : DEFAULTS.range,
      audience: AUDIENCES.includes(parsed.audience as Audience)
        ? (parsed.audience as Audience)
        : DEFAULTS.audience,
      compare: COMPARES.includes(parsed.compare as Compare)
        ? (parsed.compare as Compare)
        : DEFAULTS.compare,
    };
  } catch {
    return DEFAULTS;
  }
}

function writeRemembered(siteKey: string | null, value: Remembered): void {
  if (!siteKey) return;
  try {
    localStorage.setItem(STORE_PREFIX + siteKey, JSON.stringify(value));
  } catch {
    /* Storage full, or a private window: the choice holds for this session and
       simply is not remembered for the next one. Never a reason to fail. */
  }
}

/**
 * The segments that sit where a site identifier would, but are not one. The
 * router owns these paths; `siteKeyFromPath` and the site provider both read
 * this one list so the two can never disagree about whether `/app/new` is a
 * site called "new".
 */
export const RESERVED_APP_SEGMENTS: readonly string[] = [
  "new",
  "login",
  "invitation",
  "settings",
  "admin",
  // Addresses from before sign-in was by code. The router sends each one on to
  // sign-in or to the invitation screen, and none of them is a site.
  "signup",
  "forgot-password",
  "reset-password",
  "verify-email",
  "invite",
];

/**
 * The site identifier in a path, or `null` if that path is not a site's.
 *
 * Used at module load as well as on every navigation. Reading it up front is
 * what stops the first paint firing every query on the default interval and
 * then firing them all again a tick later once the provider has bound the site:
 * two round trips to land where the operator already was.
 */
export function siteKeyFromPath(pathname: string): string | null {
  const parts = pathname.split("/").filter(Boolean);
  if (parts[0] !== "app" || parts.length < 2) return null;
  const candidate = parts[1] as string;
  return RESERVED_APP_SEGMENTS.includes(candidate) ? null : candidate;
}

const siteKeyFromLocation = (): string | null =>
  typeof window === "undefined" ? null : siteKeyFromPath(window.location.pathname);

/* ── the store ───────────────────────────────────────────────────────────── */

export interface AppState extends Remembered {
  /** Which site these settings belong to; `null` outside a site's routes. */
  siteKey: string | null;

  setRange: (range: string) => void;

  /** The measured interval, when the caliper is set to one rather than a name. */
  customRange: { start: string; end: string } | null;
  setCustomRange: (range: { start: string; end: string } | null) => void;

  setAudience: (audience: Audience) => void;
  setCompare: (compare: Compare) => void;

  filters: Filter[];
  addFilter: (filter: Filter) => void;
  removeFilter: (index: number) => void;
  clearFilters: () => void;

  /**
   * Points the settings at a site and restores what that site was last read on.
   * Idempotent: re-binding the site already bound changes nothing, so it is safe
   * to call from an effect on every render.
   */
  bindSite: (siteKey: string | null) => void;

  /** The standard query envelope (CONTRACT §3.1) for the current settings. */
  toEnvelope: (siteId: number) => Envelope;
}

const splitMeasured = (range: string): { start: string; end: string } | null => {
  const cut = range.indexOf("..");
  if (cut <= 0) return null;
  const start = range.slice(0, cut);
  const end = range.slice(cut + 2);
  return start && end ? { start, end } : null;
};

const initialSiteKey = siteKeyFromLocation();
const initial = readRemembered(initialSiteKey);

export const useAppState = create<AppState>((set, get) => ({
  siteKey: initialSiteKey,
  ...initial,
  customRange: splitMeasured(initial.range),
  filters: [],

  setRange: (range) =>
    set((s) => {
      if (range === s.range) return s;
      writeRemembered(s.siteKey, { range, audience: s.audience, compare: s.compare });
      return { ...s, range, customRange: splitMeasured(range) };
    }),

  setCustomRange: (custom) =>
    set((s) => {
      // Clearing a measured interval hands the caliper back to a named stop
      // rather than leaving it pointing at nothing.
      const range = custom ? `${custom.start}..${custom.end}` : DEFAULTS.range;
      if (range === s.range) return s;
      writeRemembered(s.siteKey, { range, audience: s.audience, compare: s.compare });
      return { ...s, range, customRange: custom };
    }),

  setAudience: (audience) =>
    set((s) => {
      if (audience === s.audience) return s;
      writeRemembered(s.siteKey, { range: s.range, audience, compare: s.compare });
      return { ...s, audience };
    }),

  setCompare: (compare) =>
    set((s) => {
      if (compare === s.compare) return s;
      writeRemembered(s.siteKey, { range: s.range, audience: s.audience, compare });
      return { ...s, compare };
    }),

  addFilter: (filter) =>
    set((s) => {
      // Clicking the same row twice files one clip, not two.
      const already = s.filters.some(
        (f) =>
          f.dimension === filter.dimension &&
          f.op === filter.op &&
          JSON.stringify(f.value) === JSON.stringify(filter.value),
      );
      return already ? s : { ...s, filters: [...s.filters, filter] };
    }),

  removeFilter: (index) =>
    set((s) =>
      index < 0 || index >= s.filters.length
        ? s
        : { ...s, filters: s.filters.filter((_, i) => i !== index) },
    ),

  clearFilters: () => set((s) => (s.filters.length ? { ...s, filters: [] } : s)),

  bindSite: (siteKey) =>
    set((s) => {
      if (siteKey === s.siteKey) return s;
      const restored = readRemembered(siteKey);
      return {
        ...s,
        siteKey,
        ...restored,
        customRange: splitMeasured(restored.range),
        // A path or an agent filed on one site says nothing about another.
        filters: [],
      };
    }),

  toEnvelope: (siteId) => {
    const s = get();
    const measured = splitMeasured(s.range);
    return {
      site: siteId,
      ...(measured ? { start: measured.start, end: measured.end } : { range: s.range }),
      audience: s.audience,
      ...(s.compare === "none" ? {} : { compare: s.compare }),
      ...(s.filters.length ? { filters: s.filters } : {}),
    };
  },
}));

/* ── reading the settings out loud ───────────────────────────────────────── */

const RANGE_WORDS: Record<string, string> = {
  today: "Today",
  yesterday: "Yesterday",
  "24h": "Last 24 hours",
  "7d": "Last 7 days",
  "30d": "Last 30 days",
  "90d": "Last 90 days",
  "12mo": "Last 12 months",
  mtd: "Month to date",
  ytd: "Year to date",
  all: "All time",
};

const dayWords = (iso: string): string => {
  const t = Date.parse(iso);
  return Number.isFinite(t)
    ? new Date(t).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" })
    : iso;
};

/** What the caliper is set to, in words. */
export function rangeWords(range: string): string {
  const measured = splitMeasured(range);
  if (measured) return `${dayWords(measured.start)} to ${dayWords(measured.end)}`;
  return RANGE_WORDS[range] ?? range;
}

export const COMPARE_WORDS: Record<Compare, string> = {
  none: "Nothing",
  previous: "The interval before",
  year: "The same interval last year",
};
