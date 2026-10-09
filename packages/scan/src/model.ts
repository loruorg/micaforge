/**
 * The scanner's vocabulary: what one call site is, what an event is once its call
 * sites are put together, and what a finding is.
 *
 * Every type here is plain data, so a report can go to JSON unchanged and be read back by
 * another tool (a CI annotation step, a tracking-plan diff) without this package.
 */

/** Where a call site came from, so a report can say how the event is sent. */
export type SourceKind =
  /** `track("name")` from `@micaforge/sdk` or one of its framework entries. */
  | "sdk"
  /** `window.micaforge.track("name")` or `micaforge("event", "name")`. */
  | "global"
  /** `micaforge.track({ name })` on a `@micaforge/sdk-server` client. */
  | "server"
  /** `useMicaforge().track("name")`: the React or Vue hook, or the Nuxt composable. */
  | "composable"
  /** `data-track="name"` on an element. */
  | "attribute"
  /** `<TrackClick event="name">`. */
  | "component"
  /** `v-mf="'name'"` in Vue, `use:trackClick` in Svelte. */
  | "directive";

/** What a property's value is, as far as the source says. */
export type PropType = "string" | "number" | "boolean" | "null" | "array" | "object" | "unknown";

/** One property at one call site. */
export interface PropInfo {
  /** The key in the props object. */
  key: string;
  /** The value's type when it is written as a literal, otherwise `unknown`. */
  type: PropType;
  /** The value itself, when it is a literal string, number or boolean. */
  literal?: string | number | boolean;
  /** The source text of a non-literal value, cut short, for the personal-data check. */
  expression?: string;
}

/** One place in the code that sends an event. */
export interface Occurrence {
  /** Path relative to the scanned root, with `/` separators. */
  file: string;
  /** 1-based line. */
  line: number;
  /** 1-based column. */
  column: number;
  /** How it is sent. */
  source: SourceKind;
  /** The event name, or `null` when it is computed at run time. */
  name: string | null;
  /** For a computed name with fixed parts: a glob of what it can be, such as `signup_*`. */
  pattern?: string;
  /** For a computed name: its source text, cut short. */
  expression?: string;
  /** The properties written at this call site. */
  props: PropInfo[];
  /** True when some properties cannot be known here: a spread, or an object passed by name. */
  propsDynamic: boolean;
}

/** One event name with every place it is sent from. */
export interface EventSummary {
  /** The name. */
  name: string;
  /** Every call site, in file and line order. */
  occurrences: Occurrence[];
  /** Every property key seen, with every type seen for it. */
  props: { key: string; types: PropType[]; everywhere: boolean }[];
  /** True when any call site passes properties the scanner cannot see. */
  propsDynamic: boolean;
}

/** How serious a finding is. `--ci` fails on errors, and on warnings with `--strict`. */
export type Severity = "error" | "warning" | "info";

/** The checks a finding can come from. */
export type CheckId =
  | "invalid-name"
  | "near-duplicate"
  | "personal-data"
  | "dynamic-name"
  | "revenue"
  | "parse-error"
  | "not-received"
  | "not-in-code";

/** One thing worth a person's attention. */
export interface Finding {
  check: CheckId;
  severity: Severity;
  /** One sentence, plain words. */
  message: string;
  /** The event it is about, when there is one. */
  event?: string;
  /** Where, when there is one place. */
  file?: string;
  line?: number;
}

/** What the comparison with a live site found. */
export interface Comparison {
  /** The site id compared against. */
  site: number;
  /** The window, as the stats API was asked for it. */
  range: string;
  /** Event names the site received in the window, with their counts. */
  received: { name: string; count: number }[];
  /** Names in the code that the site never received in the window. */
  notReceived: string[];
  /** Names the site received that no call site in the code sends. */
  notInCode: { name: string; count: number }[];
  /** True when the site has more names than one read returns, so `notInCode` is partial. */
  truncated: boolean;
}

/** A whole scan. */
export interface ScanReport {
  /** The scanned root, absolute. */
  root: string;
  /** How many files were read. */
  files: number;
  /** Events with a fixed name, by name. */
  events: EventSummary[];
  /** Call sites whose name is computed at run time. */
  dynamic: Occurrence[];
  /** Everything worth attention, errors first. */
  findings: Finding[];
  /** Present when `--site` was given. */
  comparison?: Comparison;
}
