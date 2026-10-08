/**
 * PEOPLE: the replay player.
 *
 * Micaforge stores replays in the rrweb event format (docs/CONTRACT.md §1.5)
 * but does not ship rrweb: the player below reconstructs the recording itself,
 * with no dependency at all. It implements exactly the subset Micaforge
 * records:
 *
 *     type 2  full snapshot            type 4  meta / viewport
 *     type 3  incremental:
 *              0 DOM mutation           1 mouse move
 *              2 mouse interaction      3 scroll
 *              4 viewport resize        5 input (redaction honoured)
 *
 * Anything else in the stored array is skipped and COUNTED, then named in
 * the status line under the transport. A player that silently drops what it
 * cannot draw is a player that lies about the session.
 *
 * THE SANDBOX. The reconstruction is built inside an iframe carrying
 * `sandbox="allow-same-origin"` and, deliberately, NOT `allow-scripts`. Without
 * `allow-scripts` nothing inside the frame can execute, which is what makes
 * granting same-origin access safe: the parent can walk the frame's document to
 * apply mutations, and the recorded page can still do nothing at all. Script,
 * noscript, iframe, object and embed nodes are never reconstructed even so, and
 * inline handlers and `javascript:` URLs are stripped on the way in.
 */
import type { CSSProperties } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import { bytes as fmtBytes, duration, num } from "../../lib/format";
import { Button, Empty, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { IconChevron, IconSessions } from "../../components/react/icons";
import { useSite } from "../providers";
import { apiMessage, asRows, countryName, deviceLabel, stampAt, useBase } from "./SessionsScreen";
import { ErasureDialog, IconErase } from "./VisitorDetail";
import type { ErasureLine } from "./VisitorDetail";
import type { ReplayRow } from "./ReplayScreen";
import "../../styles/app/people.css";

/* ── the recorded format ────────────────────────────────────────────────── */

export interface ReplayEvent {
  type: number;
  timestamp: number;
  /** The rrweb payload is a union discriminated by `type` and `data.source`;
      each branch is narrowed where it is read. */
  data?: any;
}

interface SNode {
  type: number;
  id: number;
  tagName?: string;
  attributes?: Record<string, unknown>;
  childNodes?: SNode[];
  textContent?: string;
  isStyle?: boolean;
  isSVG?: boolean;
  name?: string;
  publicId?: string;
  systemId?: string;
  needBlock?: boolean;
}

const EVT = { FULL_SNAPSHOT: 2, INCREMENTAL: 3, META: 4 } as const;
const SRC = {
  MUTATION: 0,
  MOUSE_MOVE: 1,
  MOUSE_INTERACTION: 2,
  SCROLL: 3,
  VIEWPORT_RESIZE: 4,
  INPUT: 5,
} as const;

/** The names used when something is skipped, so the status line reads plainly. */
const SOURCE_NAMES: Record<number, string> = {
  6: "touch moves",
  7: "media playback",
  8: "stylesheet rules",
  9: "canvas drawing",
  10: "web fonts",
  11: "console logs",
  12: "drag",
  13: "style edits",
  14: "text selection",
  15: "adopted stylesheets",
};

const SVG_NS = "http://www.w3.org/2000/svg";
const XLINK_NS = "http://www.w3.org/1999/xlink";
/** Never reconstructed. `noscript` is on the list because scripting is off in
    the frame, so its contents would render here having been hidden there. */
const INERT = new Set(["script", "noscript", "iframe", "frame", "frameset", "object", "embed", "applet"]);
const URLISH = new Set(["href", "src", "action", "formaction", "xlink:href", "poster", "data"]);

/** The two annotations the player draws onto the replayed page. */
const MARK_CSS = `
[data-mf-redacted]{outline:1px dashed var(--mf-replay-mark,currentColor);outline-offset:1px}
[data-mf-blocked]{outline:1px dashed var(--mf-replay-mark,currentColor);opacity:.4}
`;

/* ── the reconstruction ─────────────────────────────────────────────────── */

class Reconstruction {
  readonly nodes = new Map<number, Node>();
  readonly skipped = new Map<string, number>();
  /** Bumped whenever `skipped` changes, so the chrome can sync without polling. */
  version = 0;

  constructor(private doc: Document) {}

  note(kind: string) {
    this.skipped.set(kind, (this.skipped.get(kind) ?? 0) + 1);
    this.version += 1;
  }

  reset() {
    this.nodes.clear();
    this.skipped.clear();
    this.version += 1;
    const doc = this.doc;
    while (doc.firstChild) doc.removeChild(doc.firstChild);
  }

  /** Rebuilds the whole document from a full snapshot. */
  snapshot(root: SNode) {
    this.reset();
    // The document node itself is addressable: scroll events name it by id.
    if (root?.type === 0 && typeof root.id === "number") this.nodes.set(root.id, this.doc);
    const children = root?.type === 0 ? (root.childNodes ?? []) : [root];
    for (const child of children) {
      const node = this.build(child);
      if (!node) continue;
      try {
        this.doc.appendChild(node);
      } catch {
        /* a second doctype or a second root element: the first one stands */
      }
    }
    this.injectMarks();
  }

  private injectMarks() {
    const root = this.doc.documentElement;
    if (!root) return;
    const style = this.doc.createElement("style");
    style.setAttribute("data-mf-player", "");
    style.textContent = MARK_CSS;
    (this.doc.head ?? root).appendChild(style);
    // Carry the dashboard's own annotation ink into the frame rather than
    // inventing a colour for it here.
    try {
      const ink = getComputedStyle(document.documentElement).getPropertyValue("--mf-danger").trim();
      if (ink) root.style.setProperty("--mf-replay-mark", ink);
    } catch {
      /* the frame keeps currentColor, which is always legible on its own page */
    }
  }

  build(sn: SNode | undefined | null): Node | null {
    if (!sn || typeof sn.type !== "number") return null;
    switch (sn.type) {
      case 0:
        // A nested document belongs to an iframe, and iframes are not rebuilt.
        this.note("nested documents");
        return null;
      case 1:
        try {
          const dt = this.doc.implementation.createDocumentType(
            sn.name || "html",
            sn.publicId || "",
            sn.systemId || "",
          );
          this.nodes.set(sn.id, dt);
          return dt;
        } catch {
          return null;
        }
      case 2:
        return this.buildElement(sn);
      case 3: {
        const t = this.doc.createTextNode(sn.textContent ?? "");
        this.nodes.set(sn.id, t);
        return t;
      }
      case 4:
      case 5: {
        const c = this.doc.createComment(sn.textContent ?? "");
        this.nodes.set(sn.id, c);
        return c;
      }
      default:
        this.note(`node type ${sn.type}`);
        return null;
    }
  }

  private buildElement(sn: SNode): Node | null {
    const tag = (sn.tagName || "div").toLowerCase();
    const attrs = (sn.attributes ?? {}) as Record<string, unknown>;

    if (INERT.has(tag)) {
      this.note(`${tag} elements`);
      // A template renders nothing and takes no children into the page, so the
      // id stays addressable for later mutations without anything being shown.
      const inert = this.doc.createElement("template");
      this.nodes.set(sn.id, inert);
      return inert;
    }

    // A stylesheet link whose text was captured becomes the text itself, so the
    // replay does not go out to the network to look presentable.
    const inlineCss = typeof attrs._cssText === "string" ? (attrs._cssText as string) : null;
    let el: Element;
    try {
      if (tag === "link" && inlineCss) el = this.doc.createElement("style");
      else if (sn.isSVG) el = this.doc.createElementNS(SVG_NS, tag);
      else el = this.doc.createElement(tag);
    } catch {
      this.note("unnameable elements");
      return null;
    }

    for (const [key, value] of Object.entries(attrs)) this.setAttr(el, key, value);
    if (sn.needBlock) el.setAttribute("data-mf-blocked", "true");

    for (const child of sn.childNodes ?? []) {
      const node = this.build(child);
      if (!node) continue;
      try {
        el.appendChild(node);
      } catch {
        /* a child the element refuses is dropped rather than breaking the run */
      }
    }

    this.nodes.set(sn.id, el);
    return el;
  }

  setAttr(el: Element, key: string, value: unknown) {
    const k = key.toLowerCase();
    if (value == null || value === false) {
      try {
        el.removeAttribute(key);
      } catch {
        /* ignore */
      }
      return;
    }
    if (k === "_csstext") {
      el.textContent = String(value);
      return;
    }
    if (k.startsWith("on")) {
      this.note("inline event handlers");
      return;
    }
    if (k.startsWith("rr_")) {
      this.rrAttr(el, k, value);
      return;
    }

    let v: string;
    if (k === "style" && typeof value === "object") v = cssTextFrom(value as Record<string, unknown>);
    else v = value === true ? "" : String(value);

    if (URLISH.has(k) && /^\s*javascript:/i.test(v)) {
      this.note("javascript: urls");
      return;
    }
    if (k === "autoplay") return;

    try {
      if (k.startsWith("xlink:")) el.setAttributeNS(XLINK_NS, key, v);
      else el.setAttribute(key, v);
    } catch {
      /* an attribute name this DOM refuses is simply not set */
    }
  }

  /** rrweb's own out-of-band attributes: sizes for blocked boxes, scroll state. */
  private rrAttr(el: Element, k: string, value: unknown) {
    const px = (n: unknown) => (typeof n === "number" ? `${n}px` : String(n));
    const style = (el as HTMLElement).style;
    if (k === "rr_width" && style) style.width = px(value);
    else if (k === "rr_height" && style) style.height = px(value);
    else if (k === "rr_scrolltop") (el as HTMLElement).scrollTop = Number(value) || 0;
    else if (k === "rr_scrollleft") (el as HTMLElement).scrollLeft = Number(value) || 0;
    else this.note("recorder hints");
  }

  /* ── incremental application ─────────────────────────────────────────── */

  mutate(d: any) {
    for (const r of d?.removes ?? []) {
      const n = this.nodes.get(r.id);
      if (n?.parentNode) {
        try {
          n.parentNode.removeChild(n);
        } catch {
          /* already gone */
        }
      }
      this.nodes.delete(r.id);
    }

    // An add can name a parent that arrives later in the same batch, so the
    // queue is worked until it stops shrinking.
    let queue: any[] = [...(d?.adds ?? [])];
    let guard = queue.length + 4;
    while (queue.length && guard-- > 0) {
      const again: any[] = [];
      for (const a of queue) {
        const parent = this.nodes.get(a.parentId);
        if (!parent) {
          again.push(a);
          continue;
        }
        const node = this.build(a.node);
        if (!node) continue;
        const ref = a.nextId != null ? this.nodes.get(a.nextId) : null;
        try {
          if (ref && ref.parentNode === parent) parent.insertBefore(node, ref);
          else parent.appendChild(node);
        } catch {
          /* the parent refused the child; the rest of the batch still applies */
        }
      }
      if (again.length === queue.length) {
        for (let i = 0; i < again.length; i++) this.note("orphaned additions");
        break;
      }
      queue = again;
    }

    for (const t of d?.texts ?? []) {
      const n = this.nodes.get(t.id);
      if (n) n.textContent = t.value ?? "";
    }
    for (const a of d?.attributes ?? []) {
      const n = this.nodes.get(a.id);
      if (!n || n.nodeType !== 1) continue;
      for (const [k, v] of Object.entries(a.attributes ?? {})) this.setAttr(n as Element, k, v);
    }
  }

  /** The offset the page was already scrolled to when recording began. */
  scrollWindow(x: number, y: number) {
    try {
      this.doc.defaultView?.scrollTo(x, y);
    } catch {
      /* ignore */
    }
    if (this.doc.documentElement) {
      this.doc.documentElement.scrollTop = y;
      this.doc.documentElement.scrollLeft = x;
    }
  }

  scroll(d: any) {
    const n = this.nodes.get(d?.id);
    if (!n) return;
    const x = Number(d.x) || 0;
    const y = Number(d.y) || 0;
    if (n.nodeType === 9 || n === this.doc.documentElement || n === this.doc.body) {
      try {
        this.doc.defaultView?.scrollTo(x, y);
      } catch {
        /* ignore */
      }
      if (this.doc.documentElement) {
        this.doc.documentElement.scrollTop = y;
        this.doc.documentElement.scrollLeft = x;
      }
      return;
    }
    const el = n as HTMLElement;
    if (typeof el.scrollTop === "number") {
      el.scrollTop = y;
      el.scrollLeft = x;
    }
  }

  input(d: any) {
    const n = this.nodes.get(d?.id);
    // `instanceof` cannot be used here: the node lives in the frame's realm and
    // is not an instance of this realm's HTMLElement.
    if (!n || n.nodeType !== 1) return;
    const el = n as HTMLInputElement;
    const redacted = d.masked === true || d.isMasked === true;
    if (typeof d.isChecked === "boolean" && "checked" in el) el.checked = d.isChecked;
    if (typeof d.text === "string") {
      const text = redacted ? "•".repeat(Math.min(d.text.length || 8, 40)) : d.text;
      try {
        el.value = text;
      } catch {
        /* not a field that carries a value */
      }
      if (el.tagName === "TEXTAREA") el.textContent = text;
      if (redacted) el.setAttribute("data-mf-redacted", "true");
    }
  }
}

function cssTextFrom(style: Record<string, unknown>): string {
  return Object.entries(style)
    .map(([k, v]) => `${k.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`)}:${String(v)}`)
    .join(";");
}

/* ── the timeline ───────────────────────────────────────────────────────── */

interface MovePoint {
  t: number;
  x: number;
  y: number;
}
interface StillRange {
  from: number;
  to: number;
}

const STILL_MS = 3000;
const BINS = 72;

interface Timeline {
  events: ReplayEvent[];
  t0: number;
  total: number;
  moves: MovePoint[];
  still: StillRange[];
  bins: number[];
  binStill: boolean[];
  viewport: { w: number; h: number };
}

function buildTimeline(raw: ReplayEvent[]): Timeline {
  const events = [...raw]
    .filter((e) => e && typeof e.type === "number" && Number.isFinite(e.timestamp))
    .sort((a, b) => a.timestamp - b.timestamp);
  const t0 = events[0]?.timestamp ?? 0;
  const total = Math.max(0, (events[events.length - 1]?.timestamp ?? t0) - t0);

  const moves: MovePoint[] = [];
  const still: StillRange[] = [];
  const bins = new Array<number>(BINS).fill(0);
  const binStill = new Array<boolean>(BINS).fill(false);
  let viewport = { w: 1280, h: 720 };
  let seenViewport = false;

  let prev = t0;
  for (const e of events) {
    const at = e.timestamp - t0;
    if (total > 0) bins[Math.min(BINS - 1, Math.floor((at / total) * BINS))] += 1;

    if (e.timestamp - prev > STILL_MS) {
      still.push({ from: prev - t0 + 1000, to: at });
    }
    prev = e.timestamp;

    if (!seenViewport && e.type === EVT.META && e.data?.width && e.data?.height) {
      viewport = { w: Number(e.data.width), h: Number(e.data.height) };
      seenViewport = true;
    }
    if (e.type === EVT.INCREMENTAL && e.data?.source === SRC.MOUSE_MOVE) {
      for (const p of e.data.positions ?? []) {
        moves.push({ t: e.timestamp - t0 + (Number(p.timeOffset) || 0), x: Number(p.x) || 0, y: Number(p.y) || 0 });
      }
    }
  }
  moves.sort((a, b) => a.t - b.t);

  if (total > 0) {
    for (const s of still) {
      const a = Math.max(0, Math.floor((s.from / total) * BINS));
      const b = Math.min(BINS - 1, Math.floor((s.to / total) * BINS));
      for (let i = a; i <= b; i++) binStill[i] = true;
    }
  }

  return { events, t0, total, moves, still, bins, binStill, viewport };
}

function findMove(moves: MovePoint[], ms: number): number {
  let lo = 0;
  let hi = moves.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (moves[mid].t <= ms) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return found;
}

function clock(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const m = Math.floor(s / 60);
  const rem = s % 60;
  return `${m}:${String(rem).padStart(2, "0")}`;
}

/* ── the stage ──────────────────────────────────────────────────────────── */

const SPEEDS = [1, 2, 4, 8];

export function ReplayStage({ events, label }: { events: ReplayEvent[]; label?: string }) {
  const timeline = useMemo(() => buildTimeline(events), [events]);

  const stageRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const cursorRef = useRef<HTMLDivElement | null>(null);
  const clickRef = useRef<HTMLDivElement | null>(null);
  const clickTimerRef = useRef(0);
  const rangeRef = useRef<HTMLInputElement | null>(null);
  const clockRef = useRef<HTMLSpanElement | null>(null);
  const playedRef = useRef<HTMLDivElement | null>(null);

  const reconRef = useRef<Reconstruction | null>(null);
  const atRef = useRef(0); // playhead, ms from the first event
  const appliedRef = useRef(0); // how many events have been applied
  const rafRef = useRef(0);
  const lastFrameRef = useRef(0);
  const scaleRef = useRef(1);
  const speedRef = useRef(1);
  const skipRef = useRef(true);
  const syncedRef = useRef(-1);
  const syncAtRef = useRef(0);

  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  const [skipStill, setSkipStill] = useState(true);
  const [vp, setVp] = useState(timeline.viewport);
  const [scale, setScale] = useState(1);
  const [skipped, setSkipped] = useState<[string, number][]>([]);
  const [fault, setFault] = useState<string | null>(null);

  speedRef.current = speed;
  skipRef.current = skipStill;
  scaleRef.current = scale;

  /* -- chrome written straight to the DOM: the transport updates every frame
        and React does not need to re-render for a moving playhead. ---------- */
  const paint = useCallback(() => {
    const at = atRef.current;
    if (clockRef.current) clockRef.current.textContent = `${clock(at)} / ${clock(timeline.total)}`;
    const pctOf = timeline.total > 0 ? (at / timeline.total) * 100 : 0;
    if (rangeRef.current && document.activeElement !== rangeRef.current) {
      rangeRef.current.value = String(Math.round(pctOf * 100));
      rangeRef.current.setAttribute("aria-valuetext", `${clock(at)} of ${clock(timeline.total)}`);
    }
    if (playedRef.current) playedRef.current.style.width = `${pctOf}%`;

    const cursor = cursorRef.current;
    if (cursor) {
      const i = findMove(timeline.moves, at);
      if (i < 0) cursor.dataset.seen = "false";
      else {
        const a = timeline.moves[i];
        const b = timeline.moves[i + 1];
        let x = a.x;
        let y = a.y;
        if (b && b.t > a.t && at > a.t) {
          const k = Math.min(1, (at - a.t) / (b.t - a.t));
          x = a.x + (b.x - a.x) * k;
          y = a.y + (b.y - a.y) * k;
        }
        cursor.dataset.seen = "true";
        cursor.style.transform = `translate(${x * scaleRef.current}px, ${y * scaleRef.current}px)`;
      }
    }
  }, [timeline]);

  /** One registration mark, moved and restarted. Under reduced motion it simply
      appears and clears, so a click is still legible without any animation. */
  const strike = useCallback((x: number, y: number) => {
    const mark = clickRef.current;
    if (!mark) return;
    mark.style.transform = `translate(${x * scaleRef.current}px, ${y * scaleRef.current}px)`;
    mark.classList.remove("is-on");
    void mark.offsetWidth; // forces the animation to start again from the top
    mark.classList.add("is-on");
    window.clearTimeout(clickTimerRef.current);
    clickTimerRef.current = window.setTimeout(() => mark.classList.remove("is-on"), 700);
  }, []);

  const syncSkipped = useCallback(() => {
    const recon = reconRef.current;
    if (!recon) return;
    syncedRef.current = recon.version;
    setSkipped([...recon.skipped.entries()]);
  }, []);

  /** Applies one event. `quiet` is set while catching up after a seek, so a
      scrub does not fire a shower of click marks for moments already past. */
  const apply = useCallback(
    (e: ReplayEvent, quiet: boolean) => {
      const recon = reconRef.current;
      if (!recon) return;
      try {
        if (e.type === EVT.FULL_SNAPSHOT) {
          recon.snapshot(e.data?.node as SNode);
          const off = e.data?.initialOffset;
          if (off) recon.scrollWindow(Number(off.left) || 0, Number(off.top) || 0);
          return;
        }
        if (e.type === EVT.META) {
          if (e.data?.width && e.data?.height) setVp({ w: Number(e.data.width), h: Number(e.data.height) });
          return;
        }
        if (e.type !== EVT.INCREMENTAL) return;

        switch (e.data?.source) {
          case SRC.MUTATION:
            recon.mutate(e.data);
            return;
          case SRC.MOUSE_MOVE:
            return; // drawn from the flattened track, not applied one by one
          case SRC.MOUSE_INTERACTION: {
            const kind = e.data.type;
            if (kind === 2 || kind === 3 || kind === 4 || kind === 7) {
              if (!quiet) strike(Number(e.data.x) || 0, Number(e.data.y) || 0);
            } else if (kind === 5 || kind === 6) {
              recon.note("focus changes");
            }
            return;
          }
          case SRC.SCROLL:
            recon.scroll(e.data);
            return;
          case SRC.VIEWPORT_RESIZE:
            if (e.data.width && e.data.height) setVp({ w: Number(e.data.width), h: Number(e.data.height) });
            return;
          case SRC.INPUT:
            recon.input(e.data);
            return;
          default:
            recon.note(SOURCE_NAMES[e.data?.source as number] ?? `source ${e.data?.source}`);
        }
      } catch (err) {
        setFault(
          err instanceof Error
            ? `The recording could not be rebuilt past this point: ${err.message}`
            : "The recording could not be rebuilt past this point.",
        );
      }
    },
    [strike],
  );

  /** Seeking back rewinds to the last full snapshot and replays forward: DOM
      mutations are cumulative, so there is no other honest way to go back. */
  const rebuildTo = useCallback(
    (ms: number) => {
      const recon = reconRef.current;
      if (!recon) return;
      const { events: list, t0 } = timeline;
      let snap = -1;
      let meta = -1;
      for (let i = 0; i < list.length; i++) {
        if (list[i].timestamp - t0 > ms) break;
        if (list[i].type === EVT.FULL_SNAPSHOT) snap = i;
        if (list[i].type === EVT.META) meta = i;
      }
      // A recorder writes its first snapshot a few milliseconds into the file,
      // so at the very start there is no earlier frame to draw. The first
      // snapshot is then the earliest true state of the page and stands in for
      // it, rather than leaving the stage blank.
      if (snap < 0) snap = list.findIndex((e) => e.type === EVT.FULL_SNAPSHOT);
      if (meta < 0) meta = list.findIndex((e) => e.type === EVT.META);

      recon.reset();
      if (meta >= 0) apply(list[meta], true);
      if (snap >= 0) apply(list[snap], true);

      let i = snap >= 0 ? snap + 1 : 0;
      for (; i < list.length && list[i].timestamp - t0 <= ms; i++) apply(list[i], true);
      appliedRef.current = i;
      atRef.current = ms;
      paint();
    },
    [apply, paint, timeline],
  );

  const advanceTo = useCallback(
    (ms: number, quiet = false) => {
      if (ms < atRef.current) {
        rebuildTo(ms);
        return;
      }
      const { events: list, t0 } = timeline;
      let i = appliedRef.current;
      while (i < list.length && list[i].timestamp - t0 <= ms) {
        apply(list[i], quiet);
        i++;
      }
      appliedRef.current = i;
      atRef.current = ms;
      paint();
    },
    [apply, paint, rebuildTo, timeline],
  );

  /** Seeking backwards rebuilds the document from the last snapshot, so a seek
      is applied straight away rather than deferred to a frame that a throttled
      or backgrounded tab might never run. */
  const seekTo = useCallback(
    (fraction: number) => {
      advanceTo(Math.max(0, Math.min(timeline.total, fraction * timeline.total)), true);
      syncSkipped();
    },
    [advanceTo, syncSkipped, timeline.total],
  );

  /* -- attach to the frame ---------------------------------------------- */
  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    const attach = () => {
      const doc = frame.contentDocument;
      if (!doc) {
        setFault("This browser did not give the player a document to draw the recording into.");
        return;
      }
      reconRef.current = new Reconstruction(doc);
      setFault(null);
      rebuildTo(0);
      setSkipped([...reconRef.current.skipped.entries()]);
    };
    attach();
    frame.addEventListener("load", attach);
    return () => frame.removeEventListener("load", attach);
  }, [rebuildTo]);

  /* -- fit the recorded viewport into the stage -------------------------- */
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const measure = () => {
      const box = stage.getBoundingClientRect();
      if (!box.width || !vp.w || !vp.h) return;
      setScale(Math.min(1, box.width / vp.w, box.height / vp.h));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(stage);
    return () => ro.disconnect();
  }, [vp.w, vp.h]);

  useEffect(() => {
    paint();
  }, [paint, scale]);

  /* -- transport --------------------------------------------------------- */
  const stillAt = useCallback(
    (ms: number) => timeline.still.find((s) => ms >= s.from && ms < s.to),
    [timeline],
  );

  const stop = useCallback(() => {
    if (rafRef.current) cancelAnimationFrame(rafRef.current);
    rafRef.current = 0;
  }, []);

  useEffect(() => {
    if (!playing) {
      stop();
      return;
    }
    // The first tick only sets the origin: the frame clock and performance.now()
    // are not guaranteed to share a base, and a bogus first delta would jump
    // the playhead.
    lastFrameRef.current = -1;
    const step = (now: number) => {
      const dt = lastFrameRef.current < 0 ? 0 : Math.max(0, now - lastFrameRef.current);
      lastFrameRef.current = now;
      let next = atRef.current + dt * speedRef.current;
      if (skipRef.current) {
        const gap = stillAt(next);
        if (gap) next = gap.to;
      }
      if (next >= timeline.total) {
        advanceTo(timeline.total);
        setPlaying(false);
        syncSkipped();
        return;
      }
      advanceTo(next);

      const recon = reconRef.current;
      if (recon && recon.version !== syncedRef.current && now - syncAtRef.current > 400) {
        syncAtRef.current = now;
        syncSkipped();
      }
      rafRef.current = requestAnimationFrame(step);
    };
    rafRef.current = requestAnimationFrame(step);
    return stop;
  }, [advanceTo, playing, stillAt, stop, syncSkipped, timeline.total]);

  // The recorded viewport can change between recordings and mid-session.
  useEffect(() => {
    setVp(timeline.viewport);
  }, [timeline]);

  useEffect(
    () => () => {
      stop();
      window.clearTimeout(clickTimerRef.current);
    },
    [stop],
  );

  const empty = timeline.events.length === 0;
  const binPeak = Math.max(1, ...timeline.bins);

  return (
    <div className="mf-p-player">
      <div className="mf-p-stage" ref={stageRef}>
        {empty ? (
          <p className="mf-p-stage__note">This recording holds no events to replay.</p>
        ) : null}
        <div
          className="mf-p-stage__sheet"
          style={{ width: vp.w * scale, height: vp.h * scale } as CSSProperties}
        >
          {/* allow-same-origin WITHOUT allow-scripts: the parent may walk this
              document to apply mutations, and nothing inside it can run. */}
          <iframe
            ref={frameRef}
            className="mf-p-stage__frame"
            title={label ? `Replay of ${label}` : "Session replay"}
            sandbox="allow-same-origin"
            width={vp.w}
            height={vp.h}
            style={{ width: vp.w, height: vp.h, transform: `scale(${scale})` } as CSSProperties}
          />
          <div className="mf-p-stage__veil" aria-hidden="true">
            <div className="mf-p-click" ref={clickRef}>
              <svg viewBox="0 0 22 22" width="22" height="22" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="square" aria-hidden="true">
                <path d="M11 1.5v5.2M11 15.3v5.2M1.5 11h5.2M15.3 11h5.2" />
                <circle cx="11" cy="11" r="4.2" />
              </svg>
            </div>
            <div className="mf-p-cursor" ref={cursorRef} data-seen="false">
              <svg viewBox="0 0 18 18" width="18" height="18" fill="none" aria-hidden="true">
                <path
                  d="M2.5 1.6 13 8.4 8.3 9.6 6.6 14.4z"
                  fill="var(--mf-ground)"
                  stroke="var(--mf-ground)"
                  strokeWidth={3.2}
                  strokeLinejoin="miter"
                />
                <path
                  d="M2.5 1.6 13 8.4 8.3 9.6 6.6 14.4z"
                  fill="var(--mf-ink-strong)"
                  stroke="var(--mf-ink-strong)"
                  strokeWidth={1.2}
                  strokeLinejoin="miter"
                />
              </svg>
            </div>
          </div>
        </div>
      </div>

      <div className="mf-p-transport">
        <Button
          onClick={() => {
            if (!playing && atRef.current >= timeline.total) advanceTo(0, true);
            setPlaying((p) => !p);
            syncSkipped();
          }}
          disabled={empty}
          icon={playing ? <IconPause size={14} /> : <IconPlay size={14} />}
        >
          {playing ? "Pause" : "Play"}
        </Button>

        <span className="mf-p-transport__clock mf-data" ref={clockRef}>
          {clock(0)} / {clock(timeline.total)}
        </span>

        <div className="mf-p-scrub">
          <div className="mf-p-scrub__bins" aria-hidden="true">
            {timeline.bins.map((n, i) => (
              <span
                key={i}
                className="mf-p-scrub__bin"
                data-still={timeline.binStill[i] ? "true" : "false"}
                style={{ height: `${Math.max(8, (n / binPeak) * 100)}%` }}
              />
            ))}
          </div>
          <div className="mf-p-scrub__played" ref={playedRef} aria-hidden="true" />
          <input
            ref={rangeRef}
            type="range"
            min={0}
            max={10000}
            step={1}
            defaultValue={0}
            disabled={empty}
            aria-label="Position in the recording"
            onChange={(e) => seekTo(Number(e.target.value) / 10000)}
          />
        </div>

        <div className="mf-caliper__jaws" role="group" aria-label="Playback speed">
          {SPEEDS.map((s) => (
            <button
              key={s}
              type="button"
              className="mf-caliper__stop mf-data"
              aria-pressed={speed === s}
              onClick={() => setSpeed(s)}
            >
              {s}×
            </button>
          ))}
        </div>

        <Button
          size="sm"
          tone={skipStill ? "key" : "quiet"}
          aria-pressed={skipStill}
          icon={<IconSkipStill size={14} />}
          onClick={() => setSkipStill((v) => !v)}
        >
          Skip still paper
        </Button>
      </div>

      <p className="mf-p-status">
        <span>
          <span className="mf-p-status__num">{num(timeline.events.length)}</span> recorded events
        </span>
        <span>
          Recorded at <span className="mf-p-status__num">{vp.w} × {vp.h}</span>, shown at{" "}
          <span className="mf-p-status__num">{Math.round(scale * 100)}%</span>
        </span>
        <span>Scripts are never replayed.</span>
        {skipped.length ? (
          <span className="mf-p-status__skip">
            Not replayed:{" "}
            {skipped
              .map(([kind, n]) => `${kind} (${num(n)})`)
              .join(", ")}
          </span>
        ) : null}
        {fault ? <span className="mf-p-status__skip">{fault}</span> : null}
      </p>
    </div>
  );
}

/* ── transport marks, drawn in the icon set's grammar ───────────────────── */

function IconPlay({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true" focusable="false">
      <path d="M4.5 2.5 13 8l-8.5 5.5z" />
      <rect x={1.3} y={7.3} width={1.4} height={1.4} fill="currentColor" stroke="none" />
    </svg>
  );
}

function IconPause({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true" focusable="false">
      <path d="M5.5 3v10M10.5 3v10" />
      <rect x={1.3} y={7.3} width={1.4} height={1.4} fill="currentColor" stroke="none" />
    </svg>
  );
}

/** The still stretch of paper, jumped over. */
function IconSkipStill({ size = 16 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="square" strokeLinejoin="miter" aria-hidden="true" focusable="false">
      <path d="M1.5 8h3.5" />
      <path d="M6.2 8h3.6" strokeDasharray="1.6 1.8" opacity={0.6} />
      <path d="M10.8 4.6 14.2 8l-3.4 3.4" />
      <rect x={0.8} y={7.3} width={1.4} height={1.4} fill="currentColor" stroke="none" />
    </svg>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

interface ReplayDetailPayload {
  session?: ReplayRow;
  events?: ReplayEvent[];
}

export function ReplayPlayer() {
  const { id } = useParams();
  const base = useBase();
  const qc = useQueryClient();
  const { siteId, timezone } = useSite();
  const [confirming, setConfirming] = useState(false);

  const sessionId = id ?? "";

  const q = useQuery({
    queryKey: ["stats", "replay", siteId, sessionId],
    queryFn: () =>
      api.get<ReplayDetailPayload>(`/api/replay/${encodeURIComponent(sessionId)}`, { site: siteId as number }),
    enabled: siteId != null && !!sessionId,
  });

  const erase = useMutation({
    mutationFn: () => api.del<unknown>(`/api/replay/${encodeURIComponent(sessionId)}`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["stats", "replay"] });
      setConfirming(false);
    },
  });

  const payload = q.data?.data;
  const recording = payload?.session;
  const events = useMemo(() => asRows<ReplayEvent>(payload, "events"), [payload]);
  const missing = q.isError && q.error instanceof ApiError && q.error.status === 404;
  const erased = erase.isSuccess;

  const lines: ErasureLine[] = recording
    ? [
        { label: "Recorded", value: stampAt(recording.started, timezone) },
        { label: "Length", value: duration(recording.duration_s) },
        ...(recording.bytes != null ? [{ label: "On disk", value: fmtBytes(recording.bytes) }] : []),
        { label: "Recorded events", value: num(events.length) },
      ]
    : [];

  return (
    <div className="mf-p-stack">
      <Link className="mf-p-back" to={`${base}/replay`}>
        <IconChevron size={14} />
        All recordings
      </Link>

      {missing || erased ? (
        <Panel>
          <Empty
            title={erased ? "The recording is gone" : "That recording is not on the record"}
            body={
              erased
                ? "It was erased from this site's replay store. The session itself is still on the record."
                : "It was never recorded, or the replay retention window has passed and it has already been deleted."
            }
            action={
              <Link className="mf-btn mf-btn--key mf-btn--md" to={`${base}/replay`}>
                <span className="mf-btn__label">Back to recordings</span>
              </Link>
            }
          />
        </Panel>
      ) : q.isError ? (
        <Panel>
          <ErrorState detail={apiMessage(q.error)} onRetry={() => q.refetch()} />
        </Panel>
      ) : (
        <>
          <Panel>
            <PanelHead
              title={recording ? stampAt(recording.started, timezone) : "Recording"}
              note={
                q.isPending
                  ? "Reading the recording…"
                  : "Replayed in your browser from the stored events. Nothing is sent anywhere to play it."
              }
              actions={
                <>
                  <Link
                    className="mf-btn mf-btn--key mf-btn--sm"
                    to={`${base}/sessions/${encodeURIComponent(sessionId)}`}
                  >
                    <span className="mf-btn__icon">
                      <IconSessions size={14} />
                    </span>
                    <span className="mf-btn__label">The session record</span>
                  </Link>
                  <Button
                    tone="danger"
                    size="sm"
                    icon={<IconErase size={14} />}
                    onClick={() => setConfirming(true)}
                  >
                    Erase
                  </Button>
                </>
              }
            />
            {q.isPending ? (
              <div className="mf-p-stage" aria-busy="true">
                <p className="mf-p-stage__note">Reading the recording…</p>
              </div>
            ) : events.length === 0 ? (
              <Empty
                title="This recording is empty"
                body="The session was sampled for replay but no events were stored before it ended."
              />
            ) : (
              <ReplayStage events={events} label={recording ? stampAt(recording.started, timezone) : undefined} />
            )}
          </Panel>

          {recording ? (
            <Panel>
              <PanelHead title="The recording" />
              <dl className="mf-p-facts mf-p-facts--pairs">
                <dt>Length</dt>
                <dd className="mf-data">{duration(recording.duration_s)}</dd>
                <dt>Pages</dt>
                <dd className="mf-data">{num(recording.pageviews ?? 0)}</dd>
                <dt>Device</dt>
                <dd>{deviceLabel(recording.device_type)}</dd>
                <dt>Browser</dt>
                <dd>{recording.browser ?? "—"}</dd>
                <dt>System</dt>
                <dd>{recording.os ?? "—"}</dd>
                <dt>Country</dt>
                <dd>{countryName(recording.country)}</dd>
                <dt>On disk</dt>
                <dd className="mf-data">{recording.bytes != null ? fmtBytes(recording.bytes) : "—"}</dd>
                <dt>Deleted</dt>
                <dd>{recording.expires_at ? stampAt(recording.expires_at, timezone) : "when retention runs out"}</dd>
              </dl>
            </Panel>
          ) : null}

          <ErasureDialog
            open={confirming}
            title="Erase this recording?"
            lead={<>The recording below is deleted from this site&rsquo;s replay store. This cannot be undone.</>}
            lines={lines}
            note="The session itself stays on the record. Only the replay is removed."
            confirmLabel="Erase the recording"
            busy={erase.isPending}
            error={erase.isError ? apiMessage(erase.error) : null}
            onDismiss={() => {
              if (!erase.isPending) setConfirming(false);
            }}
            onConfirm={() => erase.mutate()}
          />
        </>
      )}
    </div>
  );
}

export default ReplayPlayer;
