/**
 * The overlay itself: a launcher and a panel, inside a closed-off Shadow DOM.
 *
 * # Isolation, both ways
 *
 * The overlay lands on somebody's site, styled by somebody's CSS. A shadow root keeps the
 * page's selectors from reaching in and the overlay's from reaching out, and `all:
 * initial` on the host stops the one thing a shadow root lets through, inherited
 * properties (a site's `font-size: 20px` on `html` would otherwise size the panel). Every
 * colour is a custom property declared on the host, copied from the product's tokens, so
 * the panel reads as Micaforge whatever page it sits on.
 *
 * Nothing is built from markup strings: every value from the page (event names, property
 * values, URLs) goes in through `textContent`, so a property holding `<img onerror>` is
 * shown, not run.
 */
import { Store } from "./store.js";
import { gateReason, serverWarnings, type TrackerSetup } from "./tracker.js";
import type { DevtoolsEvent, DevtoolsOptions } from "./types.js";

type Tab = "events" | "flags" | "identity" | "warnings";
type Filter = "all" | "sent" | "blocked" | "warnings";

const OPEN_KEY = "mf-devtools-open";

/** `Alt+Shift+M` into the modifiers and the physical key code. */
export function parseHotkey(text: string): { alt: boolean; shift: boolean; ctrl: boolean; meta: boolean; code: string } {
  const parts = text.split("+").map((p) => p.trim().toLowerCase());
  const key = parts[parts.length - 1] || "m";
  const code = /^[a-z]$/.test(key) ? "Key" + key.toUpperCase() : /^[0-9]$/.test(key) ? "Digit" + key : key.charAt(0).toUpperCase() + key.slice(1);
  return {
    alt: parts.includes("alt") || parts.includes("option"),
    shift: parts.includes("shift"),
    ctrl: parts.includes("ctrl") || parts.includes("control"),
    meta: parts.includes("meta") || parts.includes("cmd"),
    code,
  };
}

const STYLE = `
:host { all: initial; }
.root {
  --ink-strong: #09090b; --ink: #18181b; --ink-2: #52525b; --ink-3: #71717a;
  --ground: #fafafa; --panel: #ffffff; --raised: #f4f4f5; --well: #f6f6f7;
  --rule: #e8e8eb; --rule-strong: #d4d4d8;
  --accent: #f25a1d; --accent-ink: #c2410c; --accent-wash: rgba(242, 90, 29, 0.1);
  --blue-ink: #1d4fd7; --blue-wash: rgba(47, 107, 255, 0.1);
  --danger: #d92d20; --danger-wash: rgba(217, 45, 32, 0.08);
  --success: #067647; --success-wash: rgba(6, 118, 71, 0.08);
  --shadow: 0 2px 4px rgba(16, 24, 40, 0.04), 0 12px 32px rgba(16, 24, 40, 0.14);
  font: 12.5px/1.45 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  color: var(--ink); -webkit-font-smoothing: antialiased;
  /* A page rule such as "* { letter-spacing: 2px !important }" matches the host element
     and beats ":host { all: initial }", and inherited properties then flow in. Every
     inherited text property is set again here, inside the shadow, where no page rule
     can reach. */
  letter-spacing: normal; word-spacing: normal; text-transform: none; text-indent: 0;
  text-align: left; text-shadow: none; font-style: normal; font-variant: normal;
  white-space: normal; direction: ltr; visibility: visible; cursor: auto; line-height: 1.45;
  position: fixed; right: 16px; bottom: 16px; z-index: 2147483647;
  display: flex; flex-direction: column; align-items: flex-end; gap: 8px;
}
* { box-sizing: border-box; }
[hidden] { display: none !important; }
button { font: inherit; color: inherit; cursor: pointer; }
button:focus-visible, input:focus-visible, select:focus-visible, summary:focus-visible {
  outline: 2px solid var(--blue-ink); outline-offset: 1px;
}
.launcher {
  display: inline-flex; align-items: center; gap: 6px; height: 30px; padding: 0 10px 0 8px;
  border: 1px solid var(--rule-strong); border-radius: 15px; background: var(--panel);
  box-shadow: var(--shadow); font-weight: 600; font-variant-numeric: tabular-nums;
}
.mark { width: 10px; height: 10px; border-radius: 3px; background: var(--accent); flex: none; }
.dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent-ink); }
.panel {
  width: min(460px, calc(100vw - 32px)); height: min(560px, calc(100vh - 80px));
  display: flex; flex-direction: column; background: var(--panel);
  border: 1px solid var(--rule-strong); border-radius: 10px; box-shadow: var(--shadow); overflow: hidden;
}
@media (prefers-reduced-motion: no-preference) {
  .panel { animation: rise 140ms ease-out; }
  @keyframes rise { from { opacity: 0; transform: translateY(6px); } }
}
header { padding: 10px 12px 8px; border-bottom: 1px solid var(--rule); display: grid; gap: 6px; }
.top { display: flex; align-items: center; gap: 8px; }
.title { font-weight: 650; color: var(--ink-strong); display: flex; align-items: center; gap: 7px; flex: 1; }
.ghost { border: 1px solid transparent; background: none; border-radius: 6px; padding: 3px 8px; color: var(--ink-2); }
.ghost:hover { background: var(--raised); color: var(--ink); }
.status { border-radius: 6px; padding: 5px 8px; font-size: 12px; }
.status.ok { background: var(--success-wash); color: var(--success); }
.status.off { background: var(--danger-wash); color: var(--danger); }
.status.none { background: var(--raised); color: var(--ink-2); }
[role=tablist] { display: flex; gap: 2px; padding: 0 8px; border-bottom: 1px solid var(--rule); }
[role=tab] {
  border: 0; background: none; padding: 8px 8px 7px; color: var(--ink-2);
  border-bottom: 2px solid transparent; font-variant-numeric: tabular-nums;
}
[role=tab][aria-selected=true] { color: var(--ink-strong); border-bottom-color: var(--accent); font-weight: 600; }
.toolbar { display: flex; gap: 6px; padding: 8px 12px; border-bottom: 1px solid var(--rule); }
input, select {
  font: inherit; color: var(--ink); background: var(--panel); border: 1px solid var(--rule-strong);
  border-radius: 6px; padding: 4px 8px; min-width: 0;
}
input { flex: 1; }
.body { flex: 1; overflow: auto; overscroll-behavior: contain; }
.empty { padding: 28px 20px; color: var(--ink-2); text-align: center; }
.empty strong { display: block; color: var(--ink); margin-bottom: 4px; }
details { border-bottom: 1px solid var(--rule); }
summary {
  list-style: none; display: grid; grid-template-columns: 52px auto 1fr auto; align-items: center;
  gap: 8px; padding: 7px 12px; cursor: pointer;
}
summary::-webkit-details-marker { display: none; }
summary:hover { background: var(--raised); }
.time { color: var(--ink-3); font: 11px/1 ui-monospace, SFMono-Regular, Menlo, monospace; font-variant-numeric: tabular-nums; }
.chip { font-size: 11px; font-weight: 600; padding: 1px 6px; border-radius: 4px; background: var(--raised); color: var(--ink-2); white-space: nowrap; }
.chip.sent { background: var(--success-wash); color: var(--success); }
.chip.blocked, .chip.failed { background: var(--danger-wash); color: var(--danger); }
.chip.warn { background: var(--accent-wash); color: var(--accent-ink); }
.chip.kind { background: var(--blue-wash); color: var(--blue-ink); }
.label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--ink); }
.label small { color: var(--ink-3); margin-left: 6px; }
.flags { display: flex; gap: 4px; }
.detail { padding: 2px 12px 12px; display: grid; gap: 8px; }
.note { padding: 6px 8px; border-radius: 6px; background: var(--well); color: var(--ink-2); }
.note.bad { background: var(--danger-wash); color: var(--danger); }
.note.warn { background: var(--accent-wash); color: var(--accent-ink); }
ul { margin: 0; padding-left: 18px; }
pre {
  margin: 0; padding: 8px 10px; background: var(--well); border: 1px solid var(--rule); border-radius: 6px;
  font: 11.5px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--ink); white-space: pre-wrap; word-break: break-all;
  max-height: 220px; overflow: auto;
}
.row { display: flex; gap: 6px; justify-content: flex-end; }
.button { border: 1px solid var(--rule-strong); background: var(--panel); border-radius: 6px; padding: 3px 10px; }
.button:hover { background: var(--raised); }
table { width: 100%; border-collapse: collapse; }
th, td { text-align: left; padding: 7px 12px; border-bottom: 1px solid var(--rule); vertical-align: top; }
th { font-size: 11px; font-weight: 600; color: var(--ink-3); text-transform: uppercase; letter-spacing: 0.04em; }
td.num { font-variant-numeric: tabular-nums; color: var(--ink-2); white-space: nowrap; }
.mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; }
.why { color: var(--ink-3); font-size: 11.5px; }
dl { margin: 0; padding: 10px 12px; display: grid; grid-template-columns: max-content 1fr; gap: 6px 14px; }
dt { color: var(--ink-3); }
dd { margin: 0; overflow-wrap: anywhere; }
.section { padding: 10px 12px 0; font-weight: 600; color: var(--ink-strong); }
footer { padding: 6px 12px; border-top: 1px solid var(--rule); color: var(--ink-3); font-size: 11px; }
`;

type Child = Node | string | null | undefined | false;

/** Build an element. Children that are strings become text nodes, never markup. */
function h(doc: Document, tag: string, attrs: Record<string, string | boolean | undefined> = {}, ...children: Child[]): HTMLElement {
  const el = doc.createElement(tag);
  for (const k of Object.keys(attrs)) {
    const v = attrs[k];
    if (v === false || v === undefined) continue;
    if (k == "text") el.textContent = String(v);
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children) if (c) el.append(typeof c == "string" ? doc.createTextNode(c) : c);
  return el;
}

const seconds = (ms: number): string => (ms < 1000 ? `+${ms}ms` : `+${(ms / 1000).toFixed(1)}s`);

const pathOf = (url: string): string => {
  try {
    const u = new URL(url);
    return u.pathname + u.search;
  } catch {
    return url;
  }
};

export interface Overlay {
  open(): void;
  close(): void;
  toggle(): void;
  destroy(): void;
  /** the shadow root, for tests */
  root: ShadowRoot;
}

/**
 * Draw the overlay and keep it current. `setup` is read again on every render, because
 * the tracker may start after the overlay does.
 */
export function mountOverlay(win: any, store: Store, setup: () => TrackerSetup, options: DevtoolsOptions = {}): Overlay {
  const doc: Document = win.document;
  const host = doc.createElement("micaforge-devtools");
  host.setAttribute("data-micaforge-devtools", "");
  const root = host.attachShadow({ mode: "open" });
  const hotkey = parseHotkey(options.hotkey || "Alt+Shift+M");
  const hotkeyText = options.hotkey || "Alt+Shift+M";

  let tab: Tab = "events";
  let filter: Filter = "all";
  let query = "";
  const expanded = new Set<number>();
  let scheduled = false;

  const style = doc.createElement("style");
  style.textContent = STYLE;
  const shell = h(doc, "div", { class: "root" });
  root.append(style, shell);

  const count = h(doc, "span", { class: "count", text: "0" });
  const dot = h(doc, "span", { class: "dot", hidden: true, title: "Some events have warnings" });
  const launcher = h(doc, "button", { class: "launcher", type: "button", "aria-expanded": "false", title: `Micaforge DevTools (${hotkeyText})` },
    h(doc, "span", { class: "mark" }), count, dot);

  const status = h(doc, "div", { class: "status none" });
  const closeBtn = h(doc, "button", { class: "ghost", type: "button", "aria-label": "Close", text: "Close" });
  const clearBtn = h(doc, "button", { class: "ghost", type: "button", text: "Clear" });
  const tabs: Record<Tab, HTMLElement> = {
    events: h(doc, "button", { role: "tab", type: "button", "data-tab": "events" }),
    flags: h(doc, "button", { role: "tab", type: "button", "data-tab": "flags" }),
    identity: h(doc, "button", { role: "tab", type: "button", "data-tab": "identity" }),
    warnings: h(doc, "button", { role: "tab", type: "button", "data-tab": "warnings" }),
  };
  const search = h(doc, "input", { type: "search", placeholder: "Filter by name, kind or path", "aria-label": "Filter events" }) as HTMLInputElement;
  const select = h(doc, "select", { "aria-label": "Show" },
    h(doc, "option", { value: "all", text: "All" }),
    h(doc, "option", { value: "sent", text: "Sent" }),
    h(doc, "option", { value: "blocked", text: "Not sent" }),
    h(doc, "option", { value: "warnings", text: "With warnings" })) as HTMLSelectElement;
  const toolbar = h(doc, "div", { class: "toolbar" }, search, select);
  const body = h(doc, "div", { class: "body", role: "tabpanel" });
  const panel = h(doc, "section", { class: "panel", role: "dialog", "aria-label": "Micaforge DevTools", hidden: true },
    h(doc, "header", {},
      h(doc, "div", { class: "top" }, h(doc, "div", { class: "title" }, h(doc, "span", { class: "mark" }), "Micaforge DevTools"), clearBtn, closeBtn),
      status),
    h(doc, "nav", { role: "tablist", "aria-label": "Sections" }, tabs.events, tabs.flags, tabs.identity, tabs.warnings),
    toolbar,
    body,
    h(doc, "footer", { text: `${hotkeyText} opens and closes this panel. It runs only on this page and sends nothing anywhere.` }));
  shell.append(panel, launcher);

  /* ---- rendering ---- */

  const statusOf = () => {
    const s = setup();
    if (s.source == "none") return { cls: "none", text: "No tracker found on this page yet (no script[data-site], no window.micaforge)." };
    const reason = gateReason(s, win);
    if (reason) return { cls: "off", text: "Not sending. " + reason };
    return { cls: "ok", text: `Sending to site ${s.site}` + (s.host ? ` at ${s.host.replace(/^https?:\/\//, "")}` : "") + "." };
  };

  const visible = (e: DevtoolsEvent): boolean => {
    if (filter == "sent" && e.status != "sent") return false;
    if (filter == "blocked" && e.status == "sent") return false;
    if (filter == "warnings" && !e.warnings.length) return false;
    if (!query) return true;
    const q = query.toLowerCase();
    return [e.kind, e.name || "", e.url, e.reason || ""].some((s) => s.toLowerCase().includes(q));
  };

  const copyButton = (text: string): HTMLElement => {
    const btn = h(doc, "button", { class: "button", type: "button", text: "Copy JSON" });
    btn.addEventListener("click", () => {
      const done = () => {
        btn.textContent = "Copied";
        win.setTimeout(() => (btn.textContent = "Copy JSON"), 1200);
      };
      const fallback = () => {
        const area = doc.createElement("textarea");
        area.value = text;
        shell.append(area);
        area.select();
        try {
          doc.execCommand("copy");
          done();
        } catch {}
        area.remove();
      };
      try {
        const clip = win.navigator && win.navigator.clipboard;
        if (clip && clip.writeText) clip.writeText(text).then(done, fallback);
        else fallback();
      } catch {
        fallback();
      }
    });
    return btn;
  };

  const eventRow = (e: DevtoolsEvent): HTMLElement => {
    const label = e.name || (e.kind == "identify" && e.payload.id ? `id ${String(e.payload.id)}` : "") || pathOf(e.url) || "(no address)";
    const statusChip = e.status == "sent" ? "sent" : e.status == "failed" ? "failed" : "not sent";
    const details = h(doc, "details", { "data-id": String(e.id), open: expanded.has(e.id) },
      h(doc, "summary", {},
        h(doc, "span", { class: "time", text: seconds(e.sincePageStart), title: new Date(e.at).toLocaleTimeString() }),
        h(doc, "span", { class: "chip kind", text: e.kind }),
        h(doc, "span", { class: "label", title: e.url }, label, label != pathOf(e.url) ? h(doc, "small", { text: pathOf(e.url) }) : null),
        h(doc, "span", { class: "flags" },
          e.warnings.length ? h(doc, "span", { class: "chip warn", text: String(e.warnings.length), title: `${e.warnings.length} warning(s)` }) : null,
          e.rewritten ? h(doc, "span", { class: "chip", text: "masked", title: e.rewritten.why }) : null,
          h(doc, "span", { class: "chip " + e.status, text: statusChip }))),
      h(doc, "div", { class: "detail" },
        e.reason ? h(doc, "div", { class: "note bad", text: (e.status == "blocked" ? "Not sent. " : "") + e.reason }) : null,
        e.rewritten ? h(doc, "div", { class: "note", text: `Sent as ${e.rewritten.to} rather than ${e.rewritten.from}: ${e.rewritten.why}.` }) : null,
        e.warnings.length ? h(doc, "div", { class: "note warn" }, h(doc, "ul", {}, ...e.warnings.map((w) => h(doc, "li", { text: w })))) : null,
        h(doc, "pre", { text: JSON.stringify(e.payload, null, 2) }),
        h(doc, "div", { class: "row" },
          e.status == "blocked" ? h(doc, "span", { class: "why", text: "A preview of what the tracker would have sent." }) : (e.via ? h(doc, "span", { class: "why", text: `Sent by ${e.via}.` }) : null),
          copyButton(JSON.stringify(e.payload, null, 2)))));
    details.addEventListener("toggle", () => {
      if ((details as HTMLDetailsElement).open) expanded.add(e.id);
      else expanded.delete(e.id);
    });
    return details;
  };

  const empty = (title: string, text: string) => h(doc, "div", { class: "empty" }, h(doc, "strong", { text: title }), text);

  const renderEvents = () => {
    const list = store.events.filter(visible).slice().reverse();
    if (!store.events.length) return [empty("No events yet", "Events appear here the moment the tracker sends them, or decides not to.")];
    if (!list.length) return [empty("Nothing matches", "No recorded event matches this filter.")];
    return list.map(eventRow);
  };

  const renderFlags = () => {
    const flags = Array.from(store.flags.values()).sort((a, b) => a.key.localeCompare(b.key));
    if (!flags.length) {
      return [empty("No flags seen yet", "Flags appear when the page asks the server for them (GET /api/flags/evaluate, the flag client's load()), or when the SDK reports their counts.")];
    }
    const rows = flags.map((f) =>
      h(doc, "tr", {},
        h(doc, "td", { class: "mono", text: f.key }),
        h(doc, "td", {}, h(doc, "span", { class: "chip " + (f.enabled ? "sent" : "blocked"), text: f.enabled ? f.variant || "on" : "off" })),
        h(doc, "td", {}, f.reason ? h(doc, "div", { class: "mono", text: f.reason }) : null, h(doc, "div", { class: "why", text: f.explanation })),
        h(doc, "td", { class: "num", text: `${f.evaluations} / ${f.exposures}`, title: "evaluations / exposures reported by the SDK" })));
    return [h(doc, "table", {},
      h(doc, "thead", {}, h(doc, "tr", {}, h(doc, "th", { text: "Flag" }), h(doc, "th", { text: "Value" }), h(doc, "th", { text: "Rule" }), h(doc, "th", { text: "Eval / exp" }))),
      h(doc, "tbody", {}, ...rows))];
  };

  const renderIdentity = () => {
    const s = setup();
    const id = store.identity;
    let optedOut = false;
    try {
      optedOut = win.localStorage.getItem("mf-opt-out") == "1";
    } catch {}
    const list = (v: string[]) => (v.length ? v.join(", ") : "none");
    return [
      h(doc, "div", { class: "section", text: "Who" }),
      h(doc, "dl", {},
        h(doc, "dt", { text: "Identified id" }), h(doc, "dd", { class: "mono", text: id.id || "Anonymous: no identify() yet" }),
        h(doc, "dt", { text: "Opted out" }), h(doc, "dd", { text: optedOut ? "Yes (localStorage mf-opt-out)" : "No" })),
      Object.keys(id.traits).length ? h(doc, "div", { class: "detail" }, h(doc, "pre", { text: JSON.stringify(id.traits, null, 2) })) : null,
      h(doc, "div", { class: "section", text: "Tracker" }),
      h(doc, "dl", {},
        h(doc, "dt", { text: "Found" }), h(doc, "dd", { text: s.source == "running" ? "Running" : s.source == "script" ? "Script tag (not started yet)" : "No" }),
        h(doc, "dt", { text: "Site" }), h(doc, "dd", { class: "mono", text: String(s.site || "none") }),
        h(doc, "dt", { text: "Host" }), h(doc, "dd", { class: "mono", text: s.host || "the page's own origin" }),
        h(doc, "dt", { text: "Debug" }), h(doc, "dd", { text: s.debug ? "On" : "Off" }),
        h(doc, "dt", { text: "Sample" }), h(doc, "dd", { text: s.sample < 100 ? `${s.sample}%` : "Everyone" }),
        h(doc, "dt", { text: "Masked paths" }), h(doc, "dd", { class: "mono", text: list(s.mask) }),
        h(doc, "dt", { text: "Excluded paths" }), h(doc, "dd", { class: "mono", text: list(s.exclude) }),
        h(doc, "dt", { text: "Strip query" }), h(doc, "dd", { class: "mono", text: s.stripQuery === true ? "all" : Array.isArray(s.stripQuery) ? list(s.stripQuery) : "none" }),
        h(doc, "dt", { text: "Respect DNT" }), h(doc, "dd", { text: s.respectDnt ? "On" : "Off" }),
        h(doc, "dt", { text: "Client-side routes" }), h(doc, "dd", { text: s.spa ? "Followed by the tracker" : "Not followed (data-spa off)" })),
    ];
  };

  const renderWarnings = () => {
    const server = serverWarnings(win);
    const withWarnings = store.events.filter((e) => e.warnings.length).slice().reverse();
    if (!server.length && !withWarnings.length) return [empty("No warnings", "Names, properties and addresses of every recorded event look right.")];
    const out: HTMLElement[] = [];
    if (server.length) {
      out.push(h(doc, "div", { class: "section", text: "This browser" }));
      out.push(h(doc, "div", { class: "detail" }, ...server.map((w) => h(doc, "div", { class: "note warn", text: w }))));
    }
    if (withWarnings.length) {
      out.push(h(doc, "div", { class: "section", text: "Events" }));
      out.push(...withWarnings.map(eventRow));
    }
    return out;
  };

  const render = () => {
    scheduled = false;
    const st = statusOf();
    status.className = "status " + st.cls;
    status.textContent = st.text;
    const warned = store.events.filter((e) => e.warnings.length).length + serverWarnings(win).length;
    count.textContent = String(store.events.length);
    (dot as HTMLElement).hidden = !warned;
    tabs.events.textContent = `Events ${store.events.length}`;
    tabs.flags.textContent = `Flags ${store.flags.size}`;
    tabs.identity.textContent = "Identity";
    tabs.warnings.textContent = `Warnings ${warned}`;
    for (const k of Object.keys(tabs) as Tab[]) tabs[k].setAttribute("aria-selected", String(k == tab));
    (toolbar as HTMLElement).hidden = tab != "events";
    if ((panel as HTMLElement).hidden) return;
    const content = tab == "events" ? renderEvents() : tab == "flags" ? renderFlags() : tab == "identity" ? renderIdentity() : renderWarnings();
    body.replaceChildren(...content.filter((c): c is HTMLElement => !!c));
  };

  const schedule = () => {
    if (scheduled) return;
    scheduled = true;
    win.setTimeout(render, 0);
  };

  /* ---- behaviour ---- */

  const setOpen = (open: boolean) => {
    (panel as HTMLElement).hidden = !open;
    launcher.setAttribute("aria-expanded", String(open));
    try {
      win.sessionStorage.setItem(OPEN_KEY, open ? "1" : "0");
    } catch {}
    render();
    if (open) tabs[tab].focus({ preventScroll: true });
  };
  const isOpen = () => !(panel as HTMLElement).hidden;

  launcher.addEventListener("click", () => setOpen(!isOpen()));
  closeBtn.addEventListener("click", () => {
    setOpen(false);
    launcher.focus();
  });
  clearBtn.addEventListener("click", () => {
    expanded.clear();
    store.clear();
  });
  for (const k of Object.keys(tabs) as Tab[]) {
    tabs[k].addEventListener("click", () => {
      tab = k;
      render();
    });
  }
  search.addEventListener("input", () => {
    query = search.value.trim();
    render();
  });
  select.addEventListener("change", () => {
    filter = select.value as Filter;
    render();
  });
  panel.addEventListener("keydown", (ev: Event) => {
    if ((ev as KeyboardEvent).key == "Escape") {
      setOpen(false);
      launcher.focus();
    }
  });
  const onKey = (ev: KeyboardEvent) => {
    if (ev.code == hotkey.code && ev.altKey == hotkey.alt && ev.shiftKey == hotkey.shift && ev.ctrlKey == hotkey.ctrl && ev.metaKey == hotkey.meta) {
      ev.preventDefault();
      setOpen(!isOpen());
    }
  };
  win.addEventListener("keydown", onKey, true);
  const unsubscribe = store.subscribe(schedule);

  const attach = () => (doc.body || doc.documentElement).append(host);
  if (doc.body) attach();
  else doc.addEventListener("DOMContentLoaded", attach, { once: true });

  let startOpen = !!options.open;
  try {
    startOpen = startOpen || win.sessionStorage.getItem(OPEN_KEY) == "1";
  } catch {}
  setOpen(startOpen);

  return {
    root,
    open: () => setOpen(true),
    close: () => setOpen(false),
    toggle: () => setOpen(!isOpen()),
    destroy: () => {
      unsubscribe();
      win.removeEventListener("keydown", onKey, true);
      host.remove();
    },
  };
}

