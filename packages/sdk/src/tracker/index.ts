/**
 * The Micaforge tracker: the file served at `GET /mf.js`.
 *
 * Reads its whole configuration from the `data-*` attributes on its own script tag,
 * installs `window.micaforge`, and drains anything the page queued before it loaded.
 *
 * Nothing in here may throw into the host page.
 */
import { createTracker, G } from "../core.js";
import type { TrackerApi, TrackerConfig } from "../types.js";

/** Every attribute whose name is its config key in kebab-case. */
const ATTRS =
  "autoPageview spa outbound downloads hash respectDnt scroll engagement debug sample vitals errors frustration forms tags";

const split = (v: string | null): string[] =>
  (v || "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);

/** Turn one `<script data-site="1" …>` into a `TrackerConfig`. Null without a site. */
export function readScriptConfig(el: any, fallbackOrigin: string): TrackerConfig | null {
  const at = (k: string): string | null => el.getAttribute("data-" + k);
  const site = +(at("site") as any);
  if (!site) return null;

  const cfg: any = {
    site: site,
    // `el.src` is the resolved absolute URL, so its origin is the ingest origin.
    host: at("host") || (el.src || "").replace(/^(\w+:\/\/[^/]*).*/, "$1") || fallbackOrigin,
    track404: at("404"),
    exclude: split(at("exclude")),
    mask: split(at("mask")),
  };
  for (const k of ATTRS.split(" ")) cfg[k] = at(k.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase()));
  // `data-strip-query` alone (or "true") drops the whole query string; a list
  // drops only the parameters it names.
  const q = at("strip-query");
  if (q != null) cfg.stripQuery = /^(true)?$/i.test(q) || split(q);
  // `data-downloads` doubles as the extension list when it holds one: "pdf,zip" or ".dmg".
  if (/[,.]/.test(cfg.downloads || ""))
    cfg.downloadExtensions = split(cfg.downloads).map((e) => e.replace(/^\./, "").toLowerCase());
  return cfg;
}

/**
 * Find the script tag, start the tracker, publish the global.
 * Returns the instance, or undefined when there is nothing to do.
 */
export function boot(): TrackerApi | undefined {
  try {
    const W: any = G.window,
      D: any = W && W.document;
    if (!D || (W.micaforge && W.micaforge.mf)) return;

    const el = D.currentScript || D.querySelector("script[data-site]"),
      cfg = el && readScriptConfig(el, W.location.origin);
    if (!cfg) return;

    // Anything the page queued through the snippet before this file arrived.
    const queued = W.micaforge && W.micaforge.q;
    const api: any = createTracker(cfg);
    // `micaforge("event", "signup", { plan: "pro" })`: the queue's calling convention.
    const mf: any = function (...args: any[]) {
      try {
        const fn = api[args[0] == "event" ? "track" : args[0]];
        if (fn) fn.apply(0, args.slice(1));
      } catch {}
    };
    Object.assign(mf, api, cfg, { mf: 1 });
    W.micaforge = mf;

    if (queued) queued.forEach((a: any) => mf.apply(0, a));
    return api;
  } catch {}
  return;
}

boot();
