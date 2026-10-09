/**
 * Feature-flag evaluation: the shared rule, in TypeScript.
 *
 * # Why there is exactly one rule
 *
 * A flag is decided in three places. The server decides it for the browser
 * (`GET /api/flags/evaluate`), the Node SDK decides it locally from a cached ruleset so a
 * request handler never waits on the network, and the browser SDK decides it locally
 * from a ruleset or a fallback rule while the network is in flight. If any two of those
 * disagree for the same visitor, a person sees a feature appear and vanish between a
 * server render and a client render, and an experiment's exposure counts stop meaning
 * anything. So the rule is written down once, in the Rust server
 * (`apps/backend/crates/core/src/flags.rs`), and this file is its TypeScript copy, held
 * to it by the shared vectors in `packages/sdk/test/flag-vectors.json` that the Rust,
 * browser and Node test suites all assert, case by case.
 *
 * # The decision, in order
 *
 * 1. An unknown key is off (`unknown`).
 * 2. An environment override replaces `enabled` and `rollout_pct` when the caller names
 *    an environment the flag configures.
 * 3. A flag that is not enabled is off (`disabled`), whatever else it says.
 * 4. Every prerequisite flag must evaluate to the state it names (`dependency`). A cycle,
 *    or a chain deeper than eight flags, is off rather than a stack overflow.
 * 5. With target groups attached, the visitor must match at least one (`target_group`).
 * 6. The flag's own conditions must match (`conditions`).
 * 7. The rollout: a subject whose bucket falls outside the percentage is off (`rollout`),
 *    and with no subject at all a partial rollout cannot hold anybody in or out, so it is
 *    off too (`no_visitor`).
 * 8. Otherwise on (`match`), with a variant picked by weight.
 *
 * # Lenient reading
 *
 * A ruleset arrives from a network, a cache or a page, so it is read leniently: anything
 * malformed evaluates off instead of throwing, which is the safe direction for a flag.
 * The reading rules are the server's, one for one: `enabled` must be the boolean `true`;
 * a missing or `null` rollout is 100, a number is rounded to a whole percent, anything
 * else is 0; a variant weight that is missing or `null` is 1, a whole non-negative number
 * is itself, anything else is 0; a prerequisite is wanted on unless its `enabled` is
 * exactly `false`, and one without a usable `flag` never holds; a malformed condition,
 * group or leaf is false; an empty group at the top of a condition document matches
 * everybody, while an empty `or` group inside one is false.
 *
 * # Where JavaScript's defaults differ from Rust's, and what this file does instead
 *
 * - **Trimming.** Rust's `str::trim` strips the Unicode `White_Space` set;
 *   `String.prototype.trim` also strips U+FEFF and keeps U+0085. {@link trim} uses Rust's set.
 * - **Duplicate keys after normalising.** The server reads a JSON object into a map
 *   sorted by key, then trims and lower-cases each key, so when `Plan` and `plan` collide
 *   the later one in sorted order wins. JavaScript iterates objects in insertion order, so
 *   keys are sorted by code point first ({@link sortedKeys}).
 * - **Numbers as text.** Rust prints a float without an exponent; `String(n)` switches to
 *   one at 1e21 and below 1e-6, so {@link canonicalNumber} expands it.
 * - **Inherited properties.** `serde_json` sees only the fields a document has. A plain
 *   property read in JavaScript also finds `Object.prototype`, so a context attribute or
 *   a target-group id named `constructor` would match something that is not there.
 *   Fields are read as own properties only, and lookups by caller-chosen name go through
 *   a `Map`.
 *
 * Nothing in this file can throw for any JSON input: every read checks its type first,
 * and the only recursion (nested condition groups, prerequisite chains) is bounded.
 *
 * The browser SDK and the Node SDK each hold a byte-identical copy of this file, so
 * neither package depends on the other. A test in the Node package fails when the two
 * copies differ.
 */
import { blake3, utf8 } from "./blake3.js";

/* ------------------------------------------------------------------ types */

/** Why a flag evaluated the way it did. The spelling is the contract with the server and the stats table. */
export type FlagReason =
  | "unknown"
  | "disabled"
  | "dependency"
  | "target_group"
  | "conditions"
  | "no_visitor"
  | "rollout"
  | "match";

/** One flag, decided for one subject. */
export interface FlagDecision {
  /** whether the subject gets the flag */
  enabled: boolean;
  /** the variant they get; `null` for an on/off flag and for every off decision */
  variant: string | null;
  /** why */
  reason: FlagReason;
}

/**
 * The operators a flag condition may use. There is no `matches`: a regular expression
 * would bring three dialects (Rust, browser, Node) to one rule, so it never matches.
 */
export type FlagOperator =
  | "is"
  | "is_not"
  | "contains"
  | "not_contains"
  | "starts_with"
  | "ends_with"
  | "is_set"
  | "is_not_set"
  | "gt"
  | "lt"
  | "gte"
  | "lte";

/** What a condition compares against. A list matches when any member does. */
export type FlagConditionValue = string | number | boolean | null | (string | number | boolean)[];

/** One test of one context attribute. Attribute names compare trimmed and lower-cased. */
export interface FlagCondition {
  dimension: string;
  op: FlagOperator;
  value?: FlagConditionValue;
}

/** A group of conditions, `and` (the default) or `or`, which may nest. */
export interface FlagConditionGroup {
  logic?: "and" | "or";
  filters?: (FlagCondition | FlagConditionGroup)[];
}

/** A condition document: a list (an `and`), one condition, a group, or nothing at all. */
export type FlagConditions =
  | FlagCondition
  | FlagConditionGroup
  | (FlagCondition | FlagConditionGroup)[]
  | null;

/** One variant of a multivariate flag. Weights are relative; a missing weight is 1. */
export interface FlagVariant {
  key: string;
  weight?: number | null;
}

/** A prerequisite: another flag that must evaluate on (or off), and optionally to one variant. */
export interface FlagDependency {
  flag: string;
  /** default `true` */
  enabled?: boolean;
  variant?: string | null;
}

/** What one environment changes about a flag. */
export interface FlagEnvironment {
  enabled?: boolean;
  rollout_pct?: number;
}

/** One flag as the ruleset carries it. Field names are the server's. */
export interface FlagRule {
  key: string;
  /** default `false`; only the boolean `true` turns a flag on */
  enabled?: boolean;
  /** 0-100, default 100 */
  rollout_pct?: number | null;
  variants?: FlagVariant[];
  conditions?: FlagConditions;
  /** ids into `FlagRuleset.target_groups`; the subject must match at least one */
  target_groups?: string[];
  dependencies?: FlagDependency[];
  /** overrides keyed by environment name, compared trimmed and lower-cased */
  environments?: Record<string, FlagEnvironment>;
}

/** Every flag on a site and the target groups they reference: `GET /api/flags/ruleset`'s `ruleset`. */
export interface FlagRuleset {
  flags: FlagRule[];
  target_groups?: Record<string, { conditions?: FlagConditions }>;
}

/** Who is asking. */
export interface FlagInput {
  /** the bucketing subject: the identified id when there is one, else the visitor id; `""` for none */
  subject: string;
  /** attributes the conditions test; strings, numbers and booleans count, anything else reads as unset */
  context?: Record<string, unknown>;
  /** the environment the caller runs in, such as `production`; empty or absent means none */
  environment?: string | null;
}

/** A ruleset read once, to be evaluated many times. */
export interface CompiledRuleset {
  /** every flag key, in ruleset order (the first of two flags with one key wins) */
  readonly keys: readonly string[];
  /** decide one flag */
  evaluate(key: string, input: FlagInput): FlagDecision;
  /** decide every flag */
  evaluateAll(input: FlagInput): Record<string, FlagDecision>;
}

/* ------------------------------------------------------------- constants */

/** Buckets a subject can fall into: a rollout percentage is compared at a hundredth of a percent. */
const BUCKETS = 10000;

/** 2^32 mod 10 000: the high word's contribution when a 64-bit value is reduced modulo {@link BUCKETS}. */
const HIGH_WORD = 7296;

/**
 * The deepest chain of prerequisites one evaluation follows. Far past any honest use; it
 * stops a cycle that slipped past validation from recursing without end.
 */
const MAX_DEPENDENCY_DEPTH = 8;

/**
 * The deepest condition nesting read; anything deeper is malformed (false).
 *
 * `serde_json` refuses documents nested more than 128 containers deep, and every group
 * level costs two (the group object and its `filters` list), so the server can never
 * hold a condition tree deeper than 64 groups. `JSON.parse` has no such limit, and a
 * hand-built object can even be cyclic, so this bound keeps the recursion finite without
 * changing the answer for any document the server could have read.
 */
const MAX_NESTING = 100;

/** The largest whole number a weight may be: beyond it a double no longer holds every integer. */
const MAX_WEIGHT = 9007199254740991;

const OPERATORS: readonly string[] = [
  "is",
  "is_not",
  "contains",
  "not_contains",
  "starts_with",
  "ends_with",
  "is_set",
  "is_not_set",
  "gt",
  "lt",
  "gte",
  "lte",
];

/* --------------------------------------------------------------- reading */

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v == "object" && v !== null && !Array.isArray(v);

const own = Object.prototype.hasOwnProperty;

/** A field as `serde_json`'s `Value::get` reads it: own properties only, nothing on a non-object. */
const field = (v: unknown, name: string): unknown => (isObject(v) && own.call(v, name) ? v[name] : undefined);

/** Rust's `char::is_whitespace`: the Unicode `White_Space` property. */
const isSpace = (c: number): boolean =>
  (c >= 9 && c <= 13) ||
  c == 32 ||
  c == 0x85 ||
  c == 0xa0 ||
  c == 0x1680 ||
  (c >= 0x2000 && c <= 0x200a) ||
  c == 0x2028 ||
  c == 0x2029 ||
  c == 0x202f ||
  c == 0x205f ||
  c == 0x3000;

/** Rust's `str::trim`. A linear scan, so a long run of inner spaces costs nothing extra. */
function trim(s: string): string {
  let a = 0;
  let b = s.length;
  while (a < b && isSpace(s.charCodeAt(a))) a++;
  while (b > a && isSpace(s.charCodeAt(b - 1))) b--;
  return s.slice(a, b);
}

/** Trimmed and lower-cased: how attribute names, context keys and environment names compare. */
const norm = (s: string): string => trim(s).toLowerCase();

/**
 * A UTF-16 code unit mapped so that code units compare in code point order: a surrogate
 * (half of a character above U+FFFF) must sort after U+E000-U+FFFF, not before.
 */
const unitRank = (c: number): number => (c >= 0xe000 ? c - 0x800 : c >= 0xd800 ? c + 0x2000 : c);

/** Code point order, which is also UTF-8 byte order: the order Rust's `String` keys sort in. */
function byCodePoint(a: string, b: string): number {
  for (let i = 0; i < a.length && i < b.length; i++) {
    const x = a.charCodeAt(i);
    const y = b.charCodeAt(i);
    if (x != y) return unitRank(x) - unitRank(y);
  }
  return a.length - b.length;
}

/** An object's keys in the order the server iterates them. */
const sortedKeys = (o: Record<string, unknown>): string[] => Object.keys(o).sort(byCodePoint);

const isScalar = (v: unknown): boolean =>
  typeof v == "string" || typeof v == "number" || typeof v == "boolean";

/**
 * A number the way a person types it, and the way the server prints it: whole numbers
 * below 1e15 as integer digits (`5`, not `5.0`; `-0` as `0`), anything else as the
 * shortest decimal that reads back to the same double, written out without an exponent.
 */
export function canonicalNumber(n: number): string {
  if (Number.isInteger(n) && Math.abs(n) < 1e15) return String(n);
  if (n != n) return "NaN";
  if (n == Infinity) return "inf";
  if (n == -Infinity) return "-inf";
  const s = String(n);
  const e = s.indexOf("e");
  if (e < 0) return s;
  const sign = s.charAt(0) == "-" ? "-" : "";
  const mantissa = s.slice(sign.length, e);
  const dot = mantissa.indexOf(".");
  const digits = mantissa.replace(".", "");
  const point = (dot < 0 ? mantissa.length : dot) + Number(s.slice(e + 1));
  if (point <= 0) return sign + "0." + "0".repeat(-point) + digits;
  if (point >= digits.length) return sign + digits + "0".repeat(point - digits.length);
  return sign + digits.slice(0, point) + "." + digits.slice(point);
}

/**
 * The canonical string of a context or condition value: a string as it is, a boolean as
 * `true`/`false`, a number through {@link canonicalNumber}, and `""` for anything else.
 */
function canonicalValue(v: unknown): string {
  if (typeof v == "string") return v;
  if (typeof v == "boolean") return v ? "true" : "false";
  if (typeof v == "number") return canonicalNumber(v);
  return "";
}

const NUMBER = /^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$/;

/**
 * A decimal number in the one grammar all three runtimes read identically,
 * `[+-]?(digits[.digits*] | .digits)([eE][+-]?digits)?` after trimming, or `null`.
 *
 * Checked against the grammar rather than handed to `Number`, which also reads `0x10`,
 * `Infinity` and the empty string, none of which the server accepts. A value too large
 * for a double is not a number either.
 */
export function parseNumber(raw: string): number | null {
  const s = trim(raw);
  if (!NUMBER.test(s)) return null;
  const n = Number(s);
  return isFinite(n) ? n : null;
}

/**
 * A rollout rounded to a whole percent, saturating just outside 0-100 (it is clamped to
 * 0-100 at decision time). The server rounds half away from zero and `Math.round` rounds
 * half up; they differ only on negative halves, which the clamp turns into 0 either way.
 */
const roundRollout = (v: number): number => (v != v ? 0 : Math.min(101, Math.max(-1, Math.round(v))));

/** A flag's rollout: missing or `null` is everyone, a number is rounded, anything else is nobody. */
const rolloutOf = (v: unknown): number =>
  v === undefined || v === null ? 100 : typeof v == "number" ? roundRollout(v) : 0;

/** A variant weight: missing or `null` is 1, a whole non-negative number is itself, else 0 (never picked). */
const weightOf = (v: unknown): number => {
  if (v === undefined || v === null) return 1;
  return typeof v == "number" && Number.isInteger(v) && v >= 0 && v <= MAX_WEIGHT ? v : 0;
};

/* ------------------------------------------------------------ the shapes */

interface Leaf {
  k: "leaf";
  attr: string;
  op: string;
  /** canonical and lower-cased */
  wanted: string[];
  /** the value as a number, for the inequalities */
  number: number | null;
}

interface Group {
  k: "group";
  any: boolean;
  nodes: Node[];
}

type Node = Leaf | Group | { k: "invalid" };

const INVALID: Node = { k: "invalid" };

interface Variant {
  key: string;
  weight: number;
}

interface Dependency {
  flag: string;
  enabled: boolean;
  variant: string | null;
}

interface Override {
  enabled: boolean | undefined;
  rollout: number | undefined;
}

interface Rule {
  enabled: boolean;
  rollout: number;
  variants: Variant[];
  conditions: Node;
  groups: string[];
  /** `null` for an entry with no usable flag key: it can never hold */
  deps: (Dependency | null)[];
  envs: Map<string, Override>;
}

/** The input as every implementation normalises it: context keys trimmed and lower-cased. */
interface Input {
  subject: string;
  context: Map<string, string>;
  environment: string;
}

/* ------------------------------------------------------------ conditions */

/**
 * The top of a condition document: missing or `null` is the empty group, a list is an
 * `and` of its nodes, an object with `dimension` is one leaf, any other object is a group,
 * and anything else is malformed.
 */
function parseTop(raw: unknown): Node {
  if (raw === undefined || raw === null) return { k: "group", any: false, nodes: [] };
  if (Array.isArray(raw)) return { k: "group", any: false, nodes: raw.map((n) => parseNode(n, 1)) };
  if (!isObject(raw)) return INVALID;
  return own.call(raw, "dimension") ? { k: "group", any: false, nodes: [parseNode(raw, 1)] } : parseNode(raw, 0);
}

/** One node: a leaf when it has `dimension`, a group otherwise. */
function parseNode(raw: unknown, depth: number): Node {
  if (!isObject(raw) || depth > MAX_NESTING) return INVALID;
  if (own.call(raw, "dimension")) return parseLeaf(raw);
  const logic = field(raw, "logic");
  let any = false;
  if (logic === "or") any = true;
  else if (logic !== undefined && logic !== null && logic !== "and") return INVALID;
  const filters = field(raw, "filters");
  if (filters === undefined || filters === null) return { k: "group", any: any, nodes: [] };
  if (!Array.isArray(filters)) return INVALID;
  return { k: "group", any: any, nodes: filters.map((n) => parseNode(n, depth + 1)) };
}

function parseLeaf(raw: Record<string, unknown>): Node {
  const dimension = field(raw, "dimension");
  const attr = typeof dimension == "string" ? norm(dimension) : "";
  const op = field(raw, "op");
  if (!attr || typeof op != "string" || OPERATORS.indexOf(op) < 0) return INVALID;
  const value = field(raw, "value");
  const wanted =
    value === undefined || value === null
      ? [""]
      : Array.isArray(value)
        ? value.filter(isScalar).map(canonicalValue)
        : [canonicalValue(value)];
  const number =
    typeof value == "number"
      ? isFinite(value)
        ? value
        : null
      : typeof value == "string"
        ? parseNumber(value)
        : null;
  return { k: "leaf", attr: attr, op: op, wanted: wanted.map((w) => w.toLowerCase()), number: number };
}

/**
 * One test. An attribute the caller did not send is the empty string, which is what
 * makes `is_not_set` work and `is` fail closed. Text compares in full Unicode lower case
 * on both sides.
 */
function matchLeaf(leaf: Leaf, context: Map<string, string>): boolean {
  const actual = context.get(leaf.attr) ?? "";
  const lower = actual.toLowerCase();
  const wanted = leaf.wanted;
  switch (leaf.op) {
    case "is":
      return wanted.some((w) => w == lower);
    case "is_not":
      return !wanted.some((w) => w == lower);
    case "contains":
      return wanted.some((w) => lower.includes(w));
    case "not_contains":
      return !wanted.some((w) => lower.includes(w));
    case "starts_with":
      return wanted.some((w) => lower.startsWith(w));
    case "ends_with":
      return wanted.some((w) => lower.endsWith(w));
    case "is_set":
      return actual != "";
    case "is_not_set":
      return actual == "";
  }
  const left = parseNumber(actual);
  const right = leaf.number;
  if (left === null || right === null) return false;
  if (leaf.op == "gt") return left > right;
  if (leaf.op == "lt") return left < right;
  if (leaf.op == "gte") return left >= right;
  return left <= right;
}

function matches(node: Node, context: Map<string, string>): boolean {
  if (node.k == "leaf") return matchLeaf(node, context);
  if (node.k != "group") return false;
  return node.any ? node.nodes.some((n) => matches(n, context)) : node.nodes.every((n) => matches(n, context));
}

/** The top-level rule: a group with nothing in it constrains nothing, whatever its logic says. */
const matchesTop = (node: Node, context: Map<string, string>): boolean =>
  (node.k == "group" && node.nodes.length == 0) || matches(node, context);

/* ----------------------------------------------------------------- rules */

function readDependency(raw: unknown): Dependency | null {
  const flag = field(raw, "flag");
  if (typeof flag != "string" || !flag) return null;
  const variant = field(raw, "variant");
  return {
    flag: flag,
    enabled: field(raw, "enabled") !== false,
    variant: typeof variant == "string" && variant ? variant : null,
  };
}

function readRule(raw: unknown): Rule {
  const variants: Variant[] = [];
  const rawVariants = field(raw, "variants");
  if (Array.isArray(rawVariants)) {
    for (const v of rawVariants) {
      const key = field(v, "key");
      if (typeof key == "string" && key) variants.push({ key: key, weight: weightOf(field(v, "weight")) });
    }
  }

  const groups: string[] = [];
  const rawGroups = field(raw, "target_groups");
  if (Array.isArray(rawGroups)) for (const id of rawGroups) if (typeof id == "string") groups.push(id);

  const deps: (Dependency | null)[] = [];
  const rawDeps = field(raw, "dependencies");
  if (Array.isArray(rawDeps)) for (const d of rawDeps) deps.push(readDependency(d));

  const envs = new Map<string, Override>();
  const rawEnvs = field(raw, "environments");
  if (isObject(rawEnvs)) {
    for (const name of sortedKeys(rawEnvs)) {
      const enabled = field(rawEnvs[name], "enabled");
      const rollout = field(rawEnvs[name], "rollout_pct");
      envs.set(norm(name), {
        enabled: typeof enabled == "boolean" ? enabled : undefined,
        rollout: typeof rollout == "number" ? roundRollout(rollout) : undefined,
      });
    }
  }

  return {
    enabled: field(raw, "enabled") === true,
    rollout: rolloutOf(field(raw, "rollout_pct")),
    variants: variants,
    conditions: parseTop(field(raw, "conditions")),
    groups: groups,
    deps: deps,
    envs: envs,
  };
}

function readInput(input: FlagInput): Input {
  const i: Partial<FlagInput> = isObject(input) ? input : {};
  const context = new Map<string, string>();
  const raw = i.context;
  if (isObject(raw)) for (const key of sortedKeys(raw)) context.set(norm(key), canonicalValue(raw[key]));
  return {
    subject: typeof i.subject == "string" ? i.subject : "",
    context: context,
    environment: typeof i.environment == "string" ? norm(i.environment) : "",
  };
}

/* --------------------------------------------------------------- deciding */

/**
 * Which of 10 000 buckets a subject falls into for one salt.
 *
 * The bytes hashed are `UTF-8(salt) ‖ 1f 00 00 1f ‖ UTF-8(subject) ‖ 1f`, the server's
 * cookieless visitor hash with site id zero and an empty user agent. The first eight
 * digest bytes, read big-endian, are reduced modulo 10 000. A double cannot hold a 64-bit
 * integer exactly, so the value is split into two 32-bit words and reduced as
 * `(hi mod 10000) * (2^32 mod 10000) + lo`, which never exceeds 2^33 and so is exact.
 *
 * Rollouts salt with the flag key and variant picks with `"<key>:variant"`, so falling
 * inside a narrow rollout does not bias which variant somebody lands in.
 */
export function flagBucket(salt: string, subject: string): number {
  const head = utf8(String(salt));
  const tail = utf8(String(subject));
  const bytes = new Uint8Array(head.length + tail.length + 5);
  bytes.set(head);
  bytes.set([0x1f, 0, 0, 0x1f], head.length);
  bytes.set(tail, head.length + 4);
  bytes[bytes.length - 1] = 0x1f;
  const d = blake3(bytes);
  const hi = ((d[0] << 24) | (d[1] << 16) | (d[2] << 8) | d[3]) >>> 0;
  const lo = ((d[4] << 24) | (d[5] << 16) | (d[6] << 8) | d[7]) >>> 0;
  return ((hi % BUCKETS) * HIGH_WORD + lo) % BUCKETS;
}

/**
 * Choose a variant by weight, or `null` for an on/off flag.
 *
 * `point = floor(bucket * total / 10000)`, computed as
 * `bucket * q + floor(bucket * r / 10000)` with `total = q * 10000 + r` so no product
 * passes 2^53: exact for any total a double holds exactly.
 */
function pickVariant(variants: Variant[], key: string, subject: string): string | null {
  if (!variants.length) return null;
  let total = 0;
  for (const v of variants) total += v.weight;
  if (total == 0) return variants[0].key;
  const bucket = flagBucket(key + ":variant", subject);
  const r = total % BUCKETS;
  const point = bucket * ((total - r) / BUCKETS) + Math.floor((bucket * r) / BUCKETS);
  let seen = 0;
  for (const v of variants) {
    seen += v.weight;
    if (point < seen) return v.key;
  }
  return variants[variants.length - 1].key;
}

const off = (reason: FlagReason): FlagDecision => ({ enabled: false, variant: null, reason: reason });

interface Compiled {
  flags: Map<string, Rule>;
  groups: Map<string, Node>;
}

function decide(c: Compiled, key: string, input: Input, stack: string[]): FlagDecision {
  const rule = c.flags.get(key);
  if (!rule) return off("unknown");

  let enabled = rule.enabled;
  let rollout = rule.rollout;
  const over = input.environment ? rule.envs.get(input.environment) : undefined;
  if (over) {
    if (over.enabled !== undefined) enabled = over.enabled;
    if (over.rollout !== undefined) rollout = over.rollout;
  }

  if (!enabled) return off("disabled");

  stack.push(key);
  const held = rule.deps.every((dep) => holds(c, dep, input, stack));
  stack.pop();
  if (!held) return off("dependency");

  if (rule.groups.length) {
    const matched = rule.groups.some((id) => {
      const group = c.groups.get(id);
      return group !== undefined && matchesTop(group, input.context);
    });
    if (!matched) return off("target_group");
  }

  if (!matchesTop(rule.conditions, input.context)) return off("conditions");

  const pct = Math.min(100, Math.max(0, rollout));
  if (pct < 100) {
    if (!input.subject) return off("no_visitor");
    if (flagBucket(key, input.subject) >= pct * (BUCKETS / 100)) return off("rollout");
  }

  return { enabled: true, variant: pickVariant(rule.variants, key, input.subject), reason: "match" };
}

/** Does one prerequisite hold? A malformed one, a cycle, or a chain past the depth limit never does. */
function holds(c: Compiled, dep: Dependency | null, input: Input, stack: string[]): boolean {
  if (!dep || stack.indexOf(dep.flag) > -1 || stack.length >= MAX_DEPENDENCY_DEPTH) return false;
  const decided = decide(c, dep.flag, input, stack);
  if (decided.enabled !== dep.enabled) return false;
  return dep.variant === null || decided.variant === dep.variant;
}

/* ------------------------------------------------------------- public API */

/**
 * Read a ruleset once, to evaluate it many times.
 *
 * Never throws. A flag with no usable key is skipped, and the first of two flags with one
 * key wins (the server's keys are unique per site, so that only happens to a hand-written
 * ruleset). Hold the result while the ruleset is current: evaluating it reads nothing
 * from the original object again, so it does not see later changes to that object.
 */
export function compileRuleset(ruleset: FlagRuleset): CompiledRuleset {
  const flags = new Map<string, Rule>();
  const list = field(ruleset, "flags");
  if (Array.isArray(list)) {
    for (const raw of list) {
      const key = field(raw, "key");
      if (typeof key == "string" && key && !flags.has(key)) flags.set(key, readRule(raw));
    }
  }

  const groups = new Map<string, Node>();
  const rawGroups = field(ruleset, "target_groups");
  if (isObject(rawGroups)) {
    for (const id of Object.keys(rawGroups)) groups.set(id, parseTop(field(rawGroups[id], "conditions")));
  }

  const c: Compiled = { flags: flags, groups: groups };
  const order = Array.from(flags.keys());
  return {
    keys: order.slice(),
    evaluate: (key, input) => decide(c, key, readInput(input), []),
    evaluateAll: (input) => {
      const normalised = readInput(input);
      const out: Record<string, FlagDecision> = {};
      // defineProperty rather than assignment, so a flag keyed `__proto__` is a property
      // like any other instead of replacing the result's prototype.
      for (const key of order) {
        const value = decide(c, key, normalised, []);
        Object.defineProperty(out, key, { value: value, enumerable: true, writable: true, configurable: true });
      }
      return out;
    },
  };
}

/** Decide one flag. Reads the whole ruleset on every call; hold a {@link compileRuleset} result instead. */
export function evaluateFlag(ruleset: FlagRuleset, key: string, input: FlagInput): FlagDecision {
  return compileRuleset(ruleset).evaluate(key, input);
}

/** Decide every flag in a ruleset, keyed by flag key. */
export function evaluateFlags(ruleset: FlagRuleset, input: FlagInput): Record<string, FlagDecision> {
  return compileRuleset(ruleset).evaluateAll(input);
}
