/**
 * Error capture: `window.onerror` and `unhandledrejection`, sent as `error`
 * events that the server files in the `errors` table and groups by fingerprint.
 *
 * Turned on with `data-errors` on the script tag. Three things keep it from
 * becoming the noisiest thing on a page:
 *
 * - **One report per distinct error per page view.** A render loop that throws
 *   the same `TypeError` sixty times a second is one error on this page, and the
 *   first report says everything the next fifty-nine would. The page view is
 *   the URL path: a client-side navigation starts a new one.
 * - **At most {@link MAX_PER_PAGE} reports per page view**, whatever they are, so
 *   a page that has gone badly wrong costs the site a bounded number of hits.
 * - **Noise the site cannot act on is dropped**: an error raised inside a
 *   browser extension, the cross-origin "Script error." that carries no
 *   message, file or stack, and the ResizeObserver loop notice browsers raise
 *   for layouts that are working as intended.
 *
 * What is sent is the error's type, a message cut to {@link MAX_MESSAGE}
 * characters, a stack cut to {@link MAX_FRAMES} lines and {@link MAX_STACK}
 * characters, the file, line and column. Query strings are removed from every
 * URL in the file name and the stack, and anything shaped like an email
 * address is replaced, because an error message is free text and can carry
 * whatever the failing code had in hand. The page itself is the event's own
 * URL, so `data-mask` and `data-strip-query` apply to it as they do to a
 * pageview. `data-release` on the script tag, when present, is sent with every
 * report so a regression can be pinned to a deploy.
 */
import { cut, guard, plugin } from "./plugin.js";

/** Reports per page view, at most. */
export const MAX_PER_PAGE = 10;
/** Characters of message kept. */
export const MAX_MESSAGE = 500;
/** Lines of stack kept. */
export const MAX_FRAMES = 30;
/** Characters of stack kept. */
export const MAX_STACK = 4000;

/** An error, in the shape the `errors` table stores it. */
export interface Report {
  name: string;
  message: string;
  stack: string;
  filename: string;
  lineno: number;
  colno: number;
}

const EXTENSION = /(chrome|moz|safari(-web)?|edge|brave|opera)-extension:\/\//i;
const NOISE = /^ResizeObserver loop (completed with undelivered notifications|limit exceeded)/i;

/** Remove query strings and email-shaped text from free text. */
export const scrub = (s: string): string =>
  s.replace(/\?[^\s)'"]*/g, "").replace(/[^\s@()<>'"]+@[^\s@()<>'"]+\.[a-z]{2,}/gi, "[email]");

/**
 * The report for one `error` event or one rejection reason, or `null` for one
 * that is not worth sending. Pure, so the rules above are testable.
 */
export function toReport(
  src: { message?: unknown; filename?: unknown; lineno?: unknown; colno?: unknown; error?: any },
  rejection?: boolean,
): Report | null {
  const err = src.error;
  let message = src.message,
    name = err && err.name;
  if (rejection) {
    if (err instanceof Error || (err && typeof err.message == "string")) message = err.message;
    else {
      try {
        message = typeof err == "string" ? err : JSON.stringify(err);
      } catch {
        message = "" + err;
      }
    }
    name = (err instanceof Error && err.name) || "UnhandledRejection";
  }
  const stack = cut(err && typeof err.stack == "string" ? err.stack : "", 20000),
    filename = cut(src.filename, 2048);
  message = "" + (message ?? "");
  // The browser withholds everything about an error in a cross-origin script;
  // with no message, file or stack there is nothing a site could fix.
  if (!filename && !stack && /^Script error\.?$/.test(message as string)) return null;
  if (EXTENSION.test(filename) || EXTENSION.test(stack) || NOISE.test(message as string)) return null;
  return {
    name: cut(name || "Error", 100),
    message: cut(scrub(message as string), MAX_MESSAGE),
    stack: cut(scrub(stack.split("\n").slice(0, MAX_FRAMES).join("\n")), MAX_STACK),
    filename: cut(scrub(filename), 512),
    lineno: +(src.lineno as any) || 0,
    colno: +(src.colno as any) || 0,
  };
}

plugin("errors", (t, w) => {
  const D = w.document,
    el = D.querySelector("script[data-site]"),
    release = cut(el && el.getAttribute("data-release"), 64);
  let page = "",
    seen: Record<string, 1> = {},
    count = 0;

  const send = (r: Report | null) => {
    if (!r) return;
    const here = w.location.pathname;
    if (here != page) {
      page = here;
      seen = {};
      count = 0;
    }
    const key = r.name + r.message + r.filename + r.lineno + r.colno;
    if (seen[key] || count >= MAX_PER_PAGE) return;
    seen[key] = 1;
    count++;
    const props: Record<string, string | number | boolean> = { ...r, handled: false };
    if (release) props.release = release;
    t.x("error", { name: r.name, props: props, raw: true });
  };

  w.addEventListener(
    "error",
    guard((e: any) => send(toReport(e))),
  );
  w.addEventListener(
    "unhandledrejection",
    guard((e: any) => send(toReport({ error: e.reason }, true))),
  );
});
