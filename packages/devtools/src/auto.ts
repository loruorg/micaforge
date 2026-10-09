/**
 * The script-tag entry: `dist/devtools.js`.
 *
 * ```html
 * <script src="/devtools.js" data-devtools data-events="signup,purchase"></script>
 * <script defer data-site="1" src="https://analytics.example.com/mf.js"></script>
 * ```
 *
 * Put it before the tracker so the first pageview is seen too. With `data-devtools` it
 * mounts on every page; without it, only when the address carries `?mf_devtools=1` (and for
 * the rest of that tab's session), so the tag can stay on a staging site and stay out of
 * the way. Attributes:
 *
 * - `data-events`: the event names the site is meant to send, comma-separated;
 * - `data-open`: open the panel at once;
 * - `data-hotkey`: the toggle, default `Alt+Shift+M`.
 *
 * Never throws into the page.
 */
import { mountDevtools, shouldLoad } from "./index.js";

try {
  const G: any = globalThis;
  const doc = G.document;
  const el = doc && (doc.currentScript || doc.querySelector("script[data-devtools]"));
  const at = (k: string): string | null => (el ? el.getAttribute("data-" + k) : null);
  if (G.window && shouldLoad(G.window, at("devtools"))) {
    const events = at("events");
    mountDevtools({
      knownEvents: events ? events.split(",").map((s) => s.trim()).filter(Boolean) : undefined,
      open: at("open") != null && !/^(false|0|off|no)$/i.test(at("open") as string),
      hotkey: at("hotkey") || undefined,
    });
  }
} catch {}
