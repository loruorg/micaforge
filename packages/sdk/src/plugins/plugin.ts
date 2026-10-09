/**
 * The one thing every plugin bundle shares: finding the tracker that loaded it.
 *
 * A plugin is a separate file (`dist/p/<name>.js`, served at
 * `/api/track/p/<name>.js`) that the core appends to the page only when the site
 * asks for it, so the core stays under its 3 KB budget and a site pays only for
 * what it turns on. The core leaves itself on `globalThis.__mf` just before it
 * loads one, and a plugin reports through the core's internal `x` method, so no
 * plugin carries a second copy of the transport, the opt-out check, the
 * sampling decision or the URL masking: everything a plugin sends passes the
 * same gates a pageview does.
 *
 * The rules a plugin keeps are the core's rules. It never throws into the host
 * page (every listener goes through `guard`), it never reads a field value, and
 * it installs once even if its file is loaded twice.
 */
import type { TrackerApi } from "../types.js";

/** Wrap a function so it can never throw into the host page. */
export const guard =
  <A extends unknown[]>(fn: (...a: A) => unknown) =>
  (...a: A): void => {
    try {
      fn(...a);
    } catch {}
  };

/**
 * Run `install` once, against the tracker that loaded this file and the page's
 * `window`. Does nothing on a server, without a tracker, or the second time.
 */
export function plugin(name: string, install: (t: TrackerApi, w: any) => void): void {
  try {
    const g: any = globalThis,
      t = g.__mf,
      w = g.window,
      seen = "mf_" + name;
    if (!t || !w || t[seen]) return;
    t[seen] = 1;
    install(t, w);
  } catch {}
}

/** Cut a string to `max` characters. Free text from a page can be any size. */
export const cut = (s: unknown, max: number): string => ("" + (s ?? "")).slice(0, max);
