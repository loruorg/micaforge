/**
 * The checks a scan runs over what it found.
 *
 * Each one encodes a rule the Micaforge server applies, or a mistake that costs a team
 * its data quietly. They are written against the ingest code, not against taste:
 *
 * - **Names.** The server trims a name and cuts it to 200 bytes (`clamp_opt(&p.n, 200)`
 *   in `routes/ingest.rs`). An empty name is stored with no name, and the events report
 *   lists only named events (`event_name != ''`), so such an event exists and is never
 *   seen. A name over 200 bytes is stored cut, under a name nobody wrote. The click-tag
 *   plugin sends nothing at all for an empty `data-track`.
 * - **Near duplicates.** `signup`, `Signup` and `sign_up` are three rows in every report
 *   and one thing in the product. So are two names one typo apart.
 * - **Personal data.** Properties are stored exactly as sent, and the point of a
 *   cookie-free product is that there is no personal data in it. A key named `email`, a
 *   literal that is an email address or a token, or a value read from `user.email` is
 *   worth stopping for.
 * - **Revenue.** A `revenue` property becomes the revenue column only when it is a
 *   number, and `currency` is an ISO 4217 code.
 */
import type { EventSummary, Finding, Occurrence, PropInfo } from "./model.js";

/** The longest name the server keeps whole, in UTF-8 bytes. */
export const MAX_NAME_BYTES = 200;

/** The longest `data-track-*` value the click-tag plugin sends, in characters. */
export const MAX_TAG_VALUE = 200;

const utf8 = (s: string) => new TextEncoder().encode(s).length;

/** Source text in backticks, unless it holds one (a template literal reads as itself). */
const asCode = (s: string) => (s.includes("`") ? s : `\`${s}\``);

/** Problems with one name at one call site. */
export function nameFindings(occurrence: Occurrence): Finding[] {
  const name = occurrence.name;
  if (name === null) return [];
  const at = { file: occurrence.file, line: occurrence.line };
  const out: Finding[] = [];
  if (!name.trim()) {
    out.push({
      check: "invalid-name",
      severity: "error",
      event: name,
      ...at,
      message:
        occurrence.source === "attribute"
          ? "data-track has no value, so a click on this element sends nothing."
          : "The event name is empty. The server stores it without a name and no report lists it.",
    });
    return out;
  }
  if (utf8(name.trim()) > MAX_NAME_BYTES) {
    out.push({
      check: "invalid-name",
      severity: "error",
      event: name,
      ...at,
      message: `The name is ${utf8(name.trim())} bytes. The server keeps ${MAX_NAME_BYTES} and cuts the rest, so it is stored under a different name.`,
    });
  }
  if (name !== name.trim()) {
    out.push({
      check: "invalid-name",
      severity: "warning",
      event: name,
      ...at,
      message: `"${name}" has spaces at the start or end. The server removes them, so it is stored as "${name.trim()}".`,
    });
  }
  if (/[\u0000-\u001f\u007f]/.test(name)) {
    out.push({
      check: "invalid-name",
      severity: "warning",
      event: name,
      ...at,
      message: "The name holds a control character such as a newline or a tab.",
    });
  }
  return out;
}

/** The name with case and separators removed: `Sign-Up`, `sign_up` and `signUp` are `signup`. */
export function normaliseName(name: string): string {
  return name.toLowerCase().replace(/[\s_\-.:/]+/g, "");
}

/** Damerau-Levenshtein distance with adjacent transposition, stopping early past `limit`. */
export function editDistance(a: string, b: string, limit = 3): number {
  if (Math.abs(a.length - b.length) > limit) return limit + 1;
  const rows: number[][] = [];
  for (let i = 0; i <= a.length; i++) rows.push([i]);
  for (let j = 1; j <= b.length; j++) (rows[0] as number[])[j] = j;
  for (let i = 1; i <= a.length; i++) {
    let best = Infinity;
    for (let j = 1; j <= b.length; j++) {
      const cost = a[i - 1] === b[j - 1] ? 0 : 1;
      const row = rows[i] as number[];
      const prev = rows[i - 1] as number[];
      let v = Math.min((prev[j] as number) + 1, (row[j - 1] as number) + 1, (prev[j - 1] as number) + cost);
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1]) {
        v = Math.min(v, ((rows[i - 2] as number[])[j - 2] as number) + 1);
      }
      row[j] = v;
      best = Math.min(best, v);
    }
    if (best > limit) return limit + 1;
  }
  return (rows[a.length] as number[])[b.length] as number;
}

/** True when two names differ only in their digits: `step_1` and `step_2` are a series. */
function differsOnlyInDigits(a: string, b: string): boolean {
  return a.length === b.length && a.replace(/\d/g, "#") === b.replace(/\d/g, "#");
}

/** Pairs of names that are probably one event spelled two ways. */
export function nearDuplicates(names: string[]): Finding[] {
  const out: Finding[] = [];
  const unique = [...new Set(names.filter((n) => n.trim()))].sort();
  const seen = new Set<string>();
  for (let i = 0; i < unique.length; i++) {
    for (let j = i + 1; j < unique.length; j++) {
      const a = unique[i] as string;
      const b = unique[j] as string;
      const key = `${a}\u0000${b}`;
      if (seen.has(key)) continue;
      const na = normaliseName(a);
      const nb = normaliseName(b);
      let reason: string | null = null;
      if (na === nb) reason = "differ only in case or separators";
      else if (!differsOnlyInDigits(na, nb)) {
        const shorter = Math.min(na.length, nb.length);
        const limit = shorter >= 12 ? 2 : shorter >= 5 ? 1 : 0;
        if (limit > 0 && editDistance(na, nb, limit) <= limit) reason = "are one or two letters apart";
      }
      if (reason) {
        seen.add(key);
        out.push({
          check: "near-duplicate",
          severity: "warning",
          event: a,
          message: `"${a}" and "${b}" ${reason}. Every report shows them as two events; pick one name.`,
        });
      }
    }
  }
  return out;
}

/** Keys that name personal data or a secret. */
const PERSONAL_KEY =
  /^(?:e[-_]?mail(?:[-_]?address)?|mail|phone(?:[-_]?number)?|tel(?:ephone)?|mobile|(?:first|last|full|middle|given|family|user)[-_]?name|name|username|surname|address|street|postcode|post[-_]?code|zip(?:[-_]?code)?|ssn|social[-_]?security(?:[-_]?number)?|passport(?:[-_]?number)?|dob|birth[-_]?date|date[-_]?of[-_]?birth|birthday|ip|ip[-_]?address|iban|card[-_]?number|credit[-_]?card|cc[-_]?number|password|passwd|secret|token|access[-_]?token|refresh[-_]?token|id[-_]?token|api[-_]?key|apikey|auth(?:orization)?|bearer|session[-_]?id|cookie|jwt)$/i;

/** A literal that is an email address. */
const EMAIL = /^[^\s@]+@[^\s@]+\.[a-z]{2,}$/i;
/** A literal that looks like a phone number: nine or more digits with the usual separators. */
const PHONE = /^\+?[\d\s().-]{9,}$/;
/** A literal that looks like a credential. */
const CREDENTIAL = /^(?:eyJ[\w-]{10,}\.[\w-]{10,}\.[\w-]+|(?:sk|pk|rk)_(?:live|test)_\w{10,}|gh[pousr]_\w{20,}|xox[abpr]-[\w-]{10,}|AKIA[0-9A-Z]{16}|mf[gio]_\w{16,})$/;
/** An expression that reads personal data, such as `user.email`. */
const PERSONAL_SOURCE = /\b(?:e[-_]?mail|phone|password|token|secret|ssn|first[-_]?name|last[-_]?name|full[-_]?name)\b/i;

/** Problems with one property at one call site. */
export function propFindings(occurrence: Occurrence, prop: PropInfo): Finding[] {
  const at = { file: occurrence.file, line: occurrence.line, ...(occurrence.name !== null ? { event: occurrence.name } : {}) };
  const out: Finding[] = [];
  const literal = typeof prop.literal == "string" ? prop.literal.trim() : null;

  if (literal !== null && (EMAIL.test(literal) || CREDENTIAL.test(literal))) {
    out.push({
      check: "personal-data",
      severity: "error",
      ...at,
      message: `The property "${prop.key}" holds ${EMAIL.test(literal) ? "an email address" : "what looks like a credential"} written into the code. Properties are stored exactly as sent.`,
    });
  } else if (literal !== null && PHONE.test(literal) && (literal.match(/\d/g)?.length ?? 0) >= 9) {
    out.push({
      check: "personal-data",
      severity: "warning",
      ...at,
      message: `The property "${prop.key}" holds what looks like a phone number.`,
    });
  }
  if (PERSONAL_KEY.test(prop.key)) {
    out.push({
      check: "personal-data",
      severity: "warning",
      ...at,
      message: `The property key "${prop.key}" names personal data or a secret. Properties are stored exactly as sent; send a category or an id you own instead.`,
    });
  } else if (prop.expression && PERSONAL_SOURCE.test(prop.expression)) {
    out.push({
      check: "personal-data",
      severity: "warning",
      ...at,
      message: `The property "${prop.key}" is read from ${asCode(prop.expression)}, which looks like personal data.`,
    });
  }
  if (prop.key === "revenue" && (prop.type === "string" || prop.type === "boolean")) {
    out.push({
      check: "revenue",
      severity: "warning",
      ...at,
      message: `"revenue" is a ${prop.type}. Only a number becomes revenue; this one stays a plain property.`,
    });
  }
  if (prop.key === "currency" && literal !== null && !/^[A-Za-z]{3}$/.test(literal)) {
    out.push({
      check: "revenue",
      severity: "warning",
      ...at,
      message: `"currency" is "${literal}". It should be a three-letter ISO 4217 code such as EUR or USD.`,
    });
  }
  if (occurrence.source === "attribute" && literal !== null && literal.length > MAX_TAG_VALUE) {
    out.push({
      check: "invalid-name",
      severity: "warning",
      ...at,
      message: `data-track-${prop.key} is ${literal.length} characters; the click-tag plugin sends the first ${MAX_TAG_VALUE}.`,
    });
  }
  return out;
}

/** Every finding about the events of one scan, before the comparison. */
export function runChecks(events: EventSummary[], dynamic: Occurrence[]): Finding[] {
  const findings: Finding[] = [];
  const occurrences = [...events.flatMap((e) => e.occurrences), ...dynamic];
  for (const occurrence of occurrences) {
    findings.push(...nameFindings(occurrence));
    for (const prop of occurrence.props) findings.push(...propFindings(occurrence, prop));
  }
  findings.push(...nearDuplicates(events.map((e) => e.name)));
  for (const occurrence of dynamic) {
    const source = asCode(occurrence.expression ?? "");
    findings.push({
      check: "dynamic-name",
      severity: "info",
      file: occurrence.file,
      line: occurrence.line,
      message: occurrence.pattern
        ? `The name is computed at run time from ${source}; it can only be checked as the pattern ${occurrence.pattern}.`
        : `The name is computed at run time from ${source}, so it cannot be checked here.`,
    });
  }
  return findings;
}
