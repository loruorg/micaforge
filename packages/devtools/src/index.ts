/**
 * `@micaforge/devtools`: a development overlay for the Micaforge tracker.
 *
 * It shows, live, every event the tracker sends (name, properties, address, timing, and
 * how it left), every event the tracker decided not to send and the rule that stopped it
 * (opted out, outside the sample, localhost, an excluded path, Do Not Track), addresses
 * rewritten by masking, the flags the server answered and the rule that decided each one,
 * who the tracker thinks the visitor is, and warnings about names, property sizes and
 * personal data before they reach the dashboard.
 *
 * It costs the tracker nothing. The core gains no byte and no hook: the overlay watches the
 * transport the tracker already uses and replays the tracker's own rules (`capture.ts`
 * explains how). It is meant for development and staging, and is only ever loaded when
 * someone asks for it:
 *
 * - a script tag carrying `data-devtools` (`dist/devtools.js`);
 * - `?mf_devtools=1` on a page whose devtools script has no `data-devtools`, which also
 *   keeps it on for the rest of the browser tab's session (`?mf_devtools=0` turns it off);
 * - `mountDevtools()` from your own code, or the Nuxt module's `devtools` option.
 *
 * ```ts
 * import { mountDevtools } from "@micaforge/devtools";
 * if (import.meta.env.DEV) mountDevtools({ knownEvents: ["signup", "purchase"] });
 * ```
 */
import { installCapture } from "./capture.js";
import { Store } from "./store.js";
import { mountOverlay } from "./ui.js";
import type { DevtoolsHandle, DevtoolsOptions } from "./types.js";

export type { DevtoolsEvent, DevtoolsFlag, DevtoolsHandle, DevtoolsIdentity, DevtoolsOptions, EventStatus } from "./types.js";
export { validate, personalData, KINDS } from "./validate.js";
export { gateReason, readSetup } from "./tracker.js";
export type { TrackerSetup } from "./tracker.js";

/** The session key `?mf_devtools=1` sets, so the overlay follows the developer across pages. */
export const SESSION_KEY = "mf-devtools";

const G: any = globalThis;

/**
 * Mount the overlay and start recording. Calling it again returns the overlay already
 * mounted. Does nothing (and returns an inert handle) where there is no DOM, such as
 * during server rendering.
 */
export function mountDevtools(options: DevtoolsOptions = {}): DevtoolsHandle {
  const win: any = G.window;
  if (G.__micaforgeDevtools) return G.__micaforgeDevtools;
  if (!win || !win.document) return inert();
  const store = new Store(options.maxEvents || 500);
  const capture = installCapture(win, store, options.knownEvents, options.tracker);
  const overlay = mountOverlay(win, store, capture.setup, options);
  const handle: DevtoolsHandle = {
    open: overlay.open,
    close: overlay.close,
    toggle: overlay.toggle,
    clear: () => store.clear(),
    destroy: () => {
      overlay.destroy();
      capture.uninstall();
      if (G.__micaforgeDevtools === handle) delete G.__micaforgeDevtools;
    },
    events: () => store.events.slice(),
    flags: () => Array.from(store.flags.values()),
    identity: () => ({ ...store.identity }),
  };
  G.__micaforgeDevtools = handle;
  return handle;
}

/**
 * Should the overlay load on this page? `true` when `attribute` (the value of a
 * `data-devtools` attribute, `null` when absent) is present and not a "no", or when the
 * address asks with `?mf_devtools=1`, or an earlier page of this tab's session did.
 * `?mf_devtools=0` says no and forgets the session's yes.
 */
export function shouldLoad(win: any, attribute: string | null = null): boolean {
  let param: string | null = null;
  try {
    param = new URLSearchParams(String(win.location.search || "")).get("mf_devtools");
  } catch {}
  let session = false;
  try {
    if (param == "1") win.sessionStorage.setItem(SESSION_KEY, "1");
    if (param == "0") win.sessionStorage.removeItem(SESSION_KEY);
    session = win.sessionStorage.getItem(SESSION_KEY) == "1";
  } catch {}
  if (param == "0") return false;
  const attr = attribute != null && !/^(false|0|off|no)$/i.test(attribute);
  return attr || param == "1" || session;
}

/** Mount the overlay when {@link shouldLoad} says so. For a bundle that ships to staging. */
export function autoload(options: DevtoolsOptions = {}): DevtoolsHandle | undefined {
  const win: any = G.window;
  if (!win || !shouldLoad(win)) return undefined;
  return mountDevtools(options);
}

function inert(): DevtoolsHandle {
  const nothing = () => {};
  return {
    open: nothing,
    close: nothing,
    toggle: nothing,
    clear: nothing,
    destroy: nothing,
    events: () => [],
    flags: () => [],
    identity: () => ({ id: "", traits: {}, at: 0 }),
  };
}
