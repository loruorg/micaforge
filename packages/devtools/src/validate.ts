/**
 * Problems worth fixing before an event ships, found by reading the payload.
 *
 * Every limit here is the server's, not a style preference: the server cuts an event name
 * at 200 characters, drops a whole property set above 8 KB (and records that it did), lifts
 * `revenue` into the money columns only when it is a number, and drops a hit whose kind it
 * does not know. The personal-data checks are heuristics, and say so: Micaforge is
 * cookieless and stores no address, and an email typed into a property undoes that for
 * every row it lands on.
 */

/** Every kind the server accepts (`events.kind`, CONTRACT.md section 1.1). */
export const KINDS = [
  "pageview",
  "custom",
  "outbound",
  "file_download",
  "form",
  "error",
  "web_vitals",
  "engagement",
  "identify",
  "frustration",
];

/** The server's limits (`routes/ingest.rs`). */
export const MAX_NAME = 200;
export const MAX_PROPS_BYTES = 8 * 1024;

const EMAIL = /[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/i;
const PHONE = /(?:^|[^\d])(\+?\d[\d\s().-]{8,}\d)(?:[^\d]|$)/;
const LONG_DIGITS = /\d{13,19}/;
const ID_LIKE = /\d{6,}|[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

/** The Luhn check card numbers pass, so a long order number is not called a card. */
export function luhn(digits: string): boolean {
  let sum = 0;
  let double = false;
  for (let i = digits.length - 1; i >= 0; i--) {
    let d = digits.charCodeAt(i) - 48;
    if (double) {
      d *= 2;
      if (d > 9) d -= 9;
    }
    sum += d;
    double = !double;
  }
  return digits.length >= 13 && sum % 10 == 0;
}

/** What a value looks like, when it looks like personal data; `null` otherwise. */
export function personalData(value: string): string | null {
  if (EMAIL.test(value)) return "an email address";
  const run = value.replace(/[\s-]/g, "").match(LONG_DIGITS);
  if (run && luhn(run[0])) return "a payment card number";
  const phone = value.match(PHONE);
  if (phone && phone[1].replace(/\D/g, "").length >= 9 && /[\s()+.-]/.test(phone[1])) return "a phone number";
  return null;
}

/** Walk a property set, calling `visit` with each string value and its dotted path. */
function strings(value: unknown, path: string, visit: (path: string, s: string) => void, depth = 0): void {
  if (depth > 8) return;
  if (typeof value == "string") visit(path, value);
  else if (Array.isArray(value)) value.forEach((v, i) => strings(v, `${path}[${i}]`, visit, depth + 1));
  else if (value && typeof value == "object") {
    for (const k of Object.keys(value)) strings((value as Record<string, unknown>)[k], path ? `${path}.${k}` : k, visit, depth + 1);
  }
}

/** Byte length of a string as UTF-8: what the server measures its 8 KB against. */
function utf8Length(s: string): number {
  let n = 0;
  for (const ch of s) {
    const c = ch.codePointAt(0) as number;
    n += c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
  }
  return n;
}

/**
 * Every warning for one payload (sent, or the preview of one that was not).
 * `known` is the site's list of expected event names, when the page gave one.
 */
export function validate(payload: Record<string, unknown>, known?: string[]): string[] {
  const out: string[] = [];
  const kind = String(payload.t || "pageview");
  const name = payload.n;

  if (!KINDS.includes(kind)) out.push(`Unknown kind "${kind}": the server drops a hit whose kind it does not know.`);

  if (kind == "custom") {
    if (typeof name != "string" || !name) out.push("A custom event with no name: it cannot be told apart from any other.");
    else {
      if (name.length > MAX_NAME) out.push(`The name is ${name.length} characters; the server keeps the first ${MAX_NAME}.`);
      if (name != name.trim()) out.push("The name starts or ends with a space, so it will not match the same name without one.");
      if (/[\u0000-\u001f\u007f]/.test(name)) out.push("The name contains a control character.");
      if (ID_LIKE.test(name)) out.push("The name contains what looks like an id. Put the id in a property and keep the name fixed, or every id becomes its own event.");
      const pii = personalData(name);
      if (pii) out.push(`The name looks like it contains ${pii}.`);
      if (known && known.length && !known.includes(name)) {
        const near = known.find((k) => k.toLowerCase().replace(/[\s_-]/g, "") == name.toLowerCase().replace(/[\s_-]/g, ""));
        out.push(near ? `Unknown event name "${name}". Did you mean "${near}"?` : `Unknown event name "${name}": it is not in your list of events.`);
      }
    }
  }

  const props = payload.p;
  if (props !== undefined) {
    if (!props || typeof props != "object" || Array.isArray(props)) out.push("Properties must be an object of names to values.");
    else {
      const bytes = utf8Length(JSON.stringify(props));
      if (bytes > MAX_PROPS_BYTES) out.push(`Properties are ${bytes} bytes. The server drops the whole set above ${MAX_PROPS_BYTES} bytes.`);
      strings(props, "", (path, s) => {
        const pii = personalData(s);
        if (pii) out.push(`Property "${path}" looks like ${pii}. Micaforge stores no personal data unless you send it; this would.`);
      });
      const p = props as Record<string, unknown>;
      if (kind != "identify" && "revenue" in p) {
        if (typeof p.revenue != "number" || !isFinite(p.revenue)) out.push("revenue must be a number (major units, such as 19.99) or it is not counted as money.");
        if (!("currency" in p)) out.push("revenue without a currency is summed as if every amount were in one currency.");
      }
      if ("currency" in p && !/^[A-Za-z]{3}$/.test(String(p.currency))) out.push(`currency "${String(p.currency)}" is not a three-letter ISO 4217 code.`);
    }
  }

  const url = String(payload.u || "");
  try {
    const u = new URL(url);
    u.searchParams.forEach((value, key) => {
      const pii = personalData(value);
      if (pii) out.push(`The address carries ${pii} in its query (${key}=). Strip it with data-strip-query="${key}".`);
    });
  } catch {
    if (url) out.push("The address is not a full URL; the server needs one to place the hit.");
  }
  return out;
}
