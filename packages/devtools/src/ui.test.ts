/**
 * The overlay as a person uses it: the launcher, the hotkey, the tabs, the filter, copy
 * and clear, and the isolation from the page's CSS.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { autoload, mountDevtools, shouldLoad, SESSION_KEY } from "./index.js";
import { Store } from "./store.js";
import { mountOverlay, parseHotkey } from "./ui.js";
import type { TrackerSetup } from "./tracker.js";

const G: any = globalThis;
const flush = () => new Promise((r) => setTimeout(r, 5));

const setup = (over: Partial<TrackerSetup> = {}): TrackerSetup => ({
  source: "running",
  site: 1,
  host: "https://mf.example.com",
  debug: false,
  respectDnt: false,
  sample: NaN,
  exclude: [],
  mask: [],
  stripQuery: undefined,
  hash: false,
  spa: true,
  autoPageview: true,
  ...over,
});

const add = (store: Store, over: Record<string, unknown> = {}) =>
  store.add({
    at: Date.now(),
    sincePageStart: 1200,
    kind: "custom",
    name: "signup",
    url: "https://example.com/pricing",
    payload: { s: 1, t: "custom", n: "signup", u: "https://example.com/pricing" },
    status: "sent",
    via: "beacon",
    warnings: [],
    ...over,
  } as any);

beforeEach(() => {
  sessionStorage.clear();
  localStorage.clear();
});

afterEach(() => {
  document.querySelectorAll("micaforge-devtools").forEach((n) => n.remove());
  G.__micaforgeDevtools?.destroy();
});

describe("the overlay", () => {
  it("lives in a shadow root and leaves the page's head alone", () => {
    const before = document.head.querySelectorAll("style").length;
    const overlay = mountOverlay(window, new Store(), () => setup());
    const host = document.querySelector("micaforge-devtools");
    expect(host?.shadowRoot).toBe(overlay.root);
    expect(overlay.root.querySelector("style")?.textContent).toContain(":host { all: initial; }");
    expect(document.head.querySelectorAll("style").length).toBe(before);
    overlay.destroy();
    expect(document.querySelector("micaforge-devtools")).toBeNull();
  });

  it("opens from the launcher and from the hotkey, and closes on Escape", () => {
    const overlay = mountOverlay(window, new Store(), () => setup());
    const panel = overlay.root.querySelector(".panel") as HTMLElement;
    const launcher = overlay.root.querySelector(".launcher") as HTMLButtonElement;
    expect(panel.hidden).toBe(true);
    launcher.click();
    expect(panel.hidden).toBe(false);
    expect(launcher.getAttribute("aria-expanded")).toBe("true");
    panel.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(panel.hidden).toBe(true);
    window.dispatchEvent(new KeyboardEvent("keydown", { code: "KeyM", altKey: true, shiftKey: true }));
    expect(panel.hidden).toBe(false);
    expect(sessionStorage.getItem("mf-devtools-open")).toBe("1");
    overlay.destroy();
  });

  it("lists events newest first, filters them, and expands one to its payload", async () => {
    const store = new Store();
    const overlay = mountOverlay(window, store, () => setup(), { open: true });
    add(store, { name: "signup" });
    add(store, { name: "purchase", status: "blocked", reason: "This browser opted out.", warnings: ["revenue must be a number"] });
    await flush();
    const labels = () => Array.from(overlay.root.querySelectorAll("summary .label")).map((n) => n.firstChild?.textContent);
    expect(labels()).toEqual(["purchase", "signup"]);

    const select = overlay.root.querySelector("select") as HTMLSelectElement;
    select.value = "blocked";
    select.dispatchEvent(new Event("change"));
    expect(labels()).toEqual(["purchase"]);
    expect(overlay.root.querySelector(".note.bad")?.textContent).toBe("Not sent. This browser opted out.");

    select.value = "all";
    select.dispatchEvent(new Event("change"));
    const search = overlay.root.querySelector("input") as HTMLInputElement;
    search.value = "sign";
    search.dispatchEvent(new Event("input"));
    expect(labels()).toEqual(["signup"]);
    expect(overlay.root.querySelector("pre")?.textContent).toContain('"n": "signup"');
    overlay.destroy();
  });

  it("copies an event as JSON and clears the list", async () => {
    const writeText = vi.fn((_text: string) => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const store = new Store();
    const overlay = mountOverlay(window, store, () => setup(), { open: true });
    add(store);
    await flush();
    const copy = Array.from(overlay.root.querySelectorAll("button")).find((b) => b.textContent == "Copy JSON") as HTMLButtonElement;
    copy.click();
    expect(JSON.parse(writeText.mock.calls[0][0])).toMatchObject({ n: "signup" });
    const clear = Array.from(overlay.root.querySelectorAll("button")).find((b) => b.textContent == "Clear") as HTMLButtonElement;
    clear.click();
    await flush();
    expect(store.events).toEqual([]);
    expect(overlay.root.querySelector(".empty strong")?.textContent).toBe("No events yet");
    overlay.destroy();
  });

  it("shows text from the page as text, never as markup", async () => {
    const store = new Store();
    const overlay = mountOverlay(window, store, () => setup(), { open: true });
    add(store, { name: '<img src=x onerror="window.pwned=1">' });
    await flush();
    expect(overlay.root.querySelector("img")).toBeNull();
    expect(G.pwned).toBeUndefined();
    overlay.destroy();
  });

  it("states whether the tracker is sending, and shows flags with their rule", async () => {
    const store = new Store();
    let s = setup();
    const overlay = mountOverlay(window, store, () => s, { open: true });
    expect(overlay.root.querySelector(".status")?.textContent).toBe("Sending to site 1 at mf.example.com.");
    localStorage.setItem("mf-opt-out", "1");
    s = setup({ site: 1 });
    store.setFlags({ beta: { enabled: false, variant: null, reason: "conditions" } });
    await flush();
    expect(overlay.root.querySelector(".status")?.className).toBe("status off");
    (overlay.root.querySelector('[data-tab="flags"]') as HTMLButtonElement).click();
    expect(overlay.root.querySelector("tbody td")?.textContent).toBe("beta");
    expect(overlay.root.querySelector(".why")?.textContent).toMatch(/conditions did not match/);
    overlay.destroy();
  });
});

describe("loading", () => {
  it("reads the hotkey by physical key", () => {
    expect(parseHotkey("Alt+Shift+M")).toEqual({ alt: true, shift: true, ctrl: false, meta: false, code: "KeyM" });
    expect(parseHotkey("Ctrl+1")).toMatchObject({ ctrl: true, code: "Digit1" });
  });

  it("loads on data-devtools, on ?mf_devtools=1 for the session, and never on ?mf_devtools=0", () => {
    expect(shouldLoad(window, null)).toBe(false);
    expect(shouldLoad(window, "")).toBe(true);
    expect(shouldLoad(window, "false")).toBe(false);
    history.replaceState(null, "", "/?mf_devtools=1");
    expect(shouldLoad(window, null)).toBe(true);
    history.replaceState(null, "", "/other");
    expect(shouldLoad(window, null)).toBe(true);
    expect(sessionStorage.getItem(SESSION_KEY)).toBe("1");
    history.replaceState(null, "", "/?mf_devtools=0");
    expect(shouldLoad(window, "")).toBe(false);
    expect(sessionStorage.getItem(SESSION_KEY)).toBeNull();
    history.replaceState(null, "", "/docs/start?ref=x");
  });

  it("mounts once, and autoload only when asked", () => {
    expect(autoload()).toBeUndefined();
    const a = mountDevtools();
    expect(mountDevtools()).toBe(a);
    expect(document.querySelectorAll("micaforge-devtools")).toHaveLength(1);
    a.destroy();
    expect(document.querySelector("micaforge-devtools")).toBeNull();
  });
});
