/**
 * The switchyard: an authored Sankey.
 *
 * Nodes are ruled bands, the same band the funnel and the readout are made of.
 * Links are ribbons in the shellac ramp, and their thickness is the only thing
 * carrying their value, so the ramp is decoration on top of a reading that is
 * already complete without it. Hovering or focusing any part of a route lifts
 * the whole route, everything upstream and everything downstream, because a
 * single hop out of context answers nothing.
 *
 * Real records are messy, so the three degenerate cases are handled outright
 * rather than left to break the layout:
 *
 *   ONE NODE      the band is drawn alone and says so.
 *   A CYCLE       a link that runs back to a column at or before its source is
 *                 not drawn as a ribbon (there is no honest place to draw it);
 *                 it is counted and reported under the diagram.
 *   A HUNDRED     each column keeps its strongest bands and gathers the rest
 *                 into one labelled remainder. Nothing is silently dropped.
 *
 * There is no layout library here on purpose: the geometry is the signature and
 * it is written to this instrument's vocabulary.
 */
import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { compact, num, pct } from "../../lib/format";

export interface SankeyNode {
  id: string;
  label: string;
  depth: number;
  value: number;
  kind?: "entry" | "page" | "event" | "exit" | string;
}

export interface SankeyLink {
  source: string;
  target: string;
  value: number;
}

export interface SankeyProps {
  nodes: SankeyNode[];
  links: SankeyLink[];
  /** Bands kept per column before the rest are gathered into a remainder. */
  maxPerColumn?: number;
  /** A link key ("source→target") lifted from outside the diagram. */
  highlight?: string | null;
  onHighlight?: (linkKey: string | null) => void;
  className?: string;
}

export const linkKey = (source: string, target: string) => `${source}→${target}`;

const NODE_W = 11;
const ROW_GAP = 10;
const PAD = { top: 24, right: 10, bottom: 10, left: 10 };
const CHAR_W = 6.4; /* measured against the UI face at the label's own size */
/** A band shorter than this cannot carry a legible label, so it is floored.
    Height is the fast read; the printed figure beside it is the true one. */
const MIN_H = 17;
/** The narrowest a column can be and still carry a label beside its band. */
const MIN_COL_W = 178;

/* ── measurement ─────────────────────────────────────────────────────────── */

function useWidth<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [w, setW] = useState(880);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.width;
      if (next && Math.abs(next - w) > 1) setW(next);
    });
    ro.observe(el);
    setW(el.clientWidth || 880);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return [ref, w] as const;
}

/* ── layout ──────────────────────────────────────────────────────────────── */

interface Placed extends SankeyNode {
  col: number;
  x: number;
  y: number;
  h: number;
  /** True for a band that stands for the tail of its column. */
  remainder?: boolean;
  members?: number;
}

interface Ribbon {
  key: string;
  source: string;
  target: string;
  value: number;
  d: string;
  ramp: number;
}

function buildLayout(
  nodes: SankeyNode[],
  links: SankeyLink[],
  width: number,
  maxPerColumn: number,
) {
  /* Depths as the server gave them may be sparse; make them contiguous. */
  const depths = [...new Set(nodes.map((n) => n.depth))].sort((a, b) => a - b);
  const colOf = new Map(depths.map((d, i) => [d, i]));
  const columnCount = depths.length;

  /* Keep the strongest bands per column; gather the rest, never drop them. */
  const byCol: SankeyNode[][] = Array.from({ length: columnCount }, () => []);
  for (const n of nodes) byCol[colOf.get(n.depth) ?? 0].push(n);

  const alias = new Map<string, string>();
  const placedNodes: Placed[] = [];
  const columns: Placed[][] = [];

  byCol.forEach((col, ci) => {
    const sorted = [...col].sort((a, b) => b.value - a.value);
    const kept = sorted.slice(0, maxPerColumn);
    const tail = sorted.slice(maxPerColumn);
    const out: Placed[] = kept.map((n) => ({ ...n, col: ci, x: 0, y: 0, h: 0 }));

    if (tail.length) {
      const id = `__rest-${ci}`;
      for (const n of tail) alias.set(n.id, id);
      out.push({
        id,
        label: `${tail.length} more page${tail.length === 1 ? "" : "s"}`,
        depth: depths[ci],
        value: tail.reduce((a, b) => a + b.value, 0),
        col: ci,
        x: 0,
        y: 0,
        h: 0,
        remainder: true,
        members: tail.length,
      });
    }

    columns.push(out);
    placedNodes.push(...out);
  });

  const index = new Map(placedNodes.map((n) => [n.id, n]));
  const resolve = (id: string) => alias.get(id) ?? id;

  /* Merge links onto the surviving bands, and set aside anything that would
     have to run backwards. */
  const merged = new Map<string, { source: string; target: string; value: number }>();
  let cycles = 0;
  let cycleValue = 0;

  for (const l of links) {
    const s = index.get(resolve(l.source));
    const t = index.get(resolve(l.target));
    if (!s || !t) continue;
    if (t.col <= s.col) {
      cycles += 1;
      cycleValue += l.value;
      continue;
    }
    const key = linkKey(s.id, t.id);
    const found = merged.get(key);
    if (found) found.value += l.value;
    else merged.set(key, { source: s.id, target: t.id, value: l.value });
  }

  const flows = [...merged.values()];

  /* Vertical scale: one scale across every column, so a band's height means
     the same thing wherever it sits. */
  const colTotals = columns.map((c) => c.reduce((a, b) => a + b.value, 0));
  const maxTotal = Math.max(1, ...colTotals);
  const maxRows = Math.max(1, ...columns.map((c) => c.length));

  const target = Math.max(180, Math.min(620, maxRows * 30 + 40));
  const usable = Math.max(60, target - ROW_GAP * (maxRows - 1));
  const scale = usable / maxTotal;

  const innerW = Math.max(240, width - PAD.left - PAD.right);
  /* Every label is written to the RIGHT of its own band, including the last
     column's, so a reading never has to be hunted for on the wrong side. That
     means the last column needs room of its own: without it, its label backs
     into the space the column before it is already using and the two collide. */
  const labelRoom = Math.min(210, Math.max(92, innerW / (columnCount + 1)));
  const plotW = Math.max(NODE_W, innerW - labelRoom);
  const colStep = columnCount > 1 ? (plotW - NODE_W) / (columnCount - 1) : 0;

  const columnHeights = columns.map(
    (col) =>
      col.reduce((a, b) => a + Math.max(MIN_H, b.value * scale), 0) +
      ROW_GAP * Math.max(0, col.length - 1),
  );
  const innerH = Math.max(target, ...columnHeights);

  columns.forEach((col, ci) => {
    let y = Math.max(0, (innerH - columnHeights[ci]) / 2);
    const x = columnCount > 1 ? ci * colStep : (plotW - NODE_W) / 2;
    for (const n of col) {
      n.h = Math.max(MIN_H, n.value * scale);
      n.x = x;
      n.y = y;
      y += n.h + ROW_GAP;
    }
  });

  /* Ribbons. Outgoing stack down the source band in the target's own order, so
     ribbons cross as little as the data allows. */
  const outAcc = new Map<string, number>();
  const inAcc = new Map<string, number>();
  const rampOf = new Map<string, number>();
  columns.forEach((col) => col.forEach((n, i) => rampOf.set(n.id, (i % 5) + 1)));

  const ordered = [...flows].sort((a, b) => {
    const sa = index.get(a.source)!;
    const sb = index.get(b.source)!;
    if (sa.y !== sb.y) return sa.y - sb.y;
    return index.get(a.target)!.y - index.get(b.target)!.y;
  });

  const ribbons: Ribbon[] = ordered.map((f) => {
    const s = index.get(f.source)!;
    const t = index.get(f.target)!;
    const th = Math.max(1, f.value * scale);

    const so = outAcc.get(s.id) ?? 0;
    const ti = inAcc.get(t.id) ?? 0;
    outAcc.set(s.id, so + th);
    inAcc.set(t.id, ti + th);

    const x0 = s.x + NODE_W;
    const x1 = t.x;
    const xm = (x0 + x1) / 2;
    const sy0 = s.y + Math.min(so, Math.max(0, s.h - th));
    const ty0 = t.y + Math.min(ti, Math.max(0, t.h - th));

    return {
      key: linkKey(s.id, t.id),
      source: s.id,
      target: t.id,
      value: f.value,
      ramp: rampOf.get(s.id) ?? 3,
      d:
        `M${x0},${sy0}` +
        `C${xm},${sy0} ${xm},${ty0} ${x1},${ty0}` +
        `L${x1},${ty0 + th}` +
        `C${xm},${ty0 + th} ${xm},${sy0 + th} ${x0},${sy0 + th}Z`,
    };
  });

  return {
    columns,
    nodes: placedNodes,
    ribbons,
    index,
    /* A caller holding a raw node id, a transitions list, say, may be naming
       a band that was gathered into a remainder. Resolving it here keeps that
       control live instead of silently highlighting nothing. */
    resolve,
    innerW,
    innerH,
    columnCount,
    colWidth: columnCount > 1 ? colStep : plotW,
    labelRoom,
    cycles,
    cycleValue,
    gathered: placedNodes.filter((n) => n.remainder).length,
    floored: placedNodes.filter((n) => n.value * scale < MIN_H).length,
  };
}

/* ── the component ───────────────────────────────────────────────────────── */

export function Sankey({
  nodes,
  links,
  maxPerColumn = 10,
  highlight = null,
  onHighlight,
  className,
}: SankeyProps) {
  const [wrapRef, width] = useWidth<HTMLDivElement>();
  const [hovered, setHovered] = useState<string | null>(null);
  /* Two switchyards can sit on one page, so the paper's ruling is defined per
     instance rather than under a name they would fight over. */
  const ruleId = `mfb-band-${useId().replace(/[^a-zA-Z0-9_-]/g, "")}`;

  /* A column narrower than this cannot hold a label, so the diagram keeps its
     own minimum and scrolls sideways inside its container instead of crushing.
     The page body never scrolls sideways; this one element does. */
  const columnsWanted = useMemo(
    () => new Set(nodes.map((n) => n.depth)).size || 1,
    [nodes],
  );
  const drawWidth = Math.max(
    width,
    MIN_COL_W * columnsWanted + 110 + PAD.left + PAD.right,
  );

  const layout = useMemo(
    () => buildLayout(nodes, links, drawWidth, maxPerColumn),
    [nodes, links, drawWidth, maxPerColumn],
  );

  const active = hovered ?? highlight;

  /* The whole route through a link or a band: everything that feeds it and
     everything it feeds. */
  const route = useMemo(() => {
    if (!active) return null;

    const arrow = active.indexOf("\u2192");
    const target =
      arrow > 0
        ? linkKey(
            layout.resolve(active.slice(0, arrow)),
            layout.resolve(active.slice(arrow + 1)),
          )
        : layout.resolve(active);

    const linkSet = new Set<string>();
    const nodeSet = new Set<string>();

    const forward = (id: string) => {
      for (const r of layout.ribbons) {
        if (r.source !== id || linkSet.has(r.key)) continue;
        linkSet.add(r.key);
        nodeSet.add(r.source);
        nodeSet.add(r.target);
        forward(r.target);
      }
    };
    const backward = (id: string) => {
      for (const r of layout.ribbons) {
        if (r.target !== id || linkSet.has(r.key)) continue;
        linkSet.add(r.key);
        nodeSet.add(r.source);
        nodeSet.add(r.target);
        backward(r.source);
      }
    };

    const asLink = layout.ribbons.find((r) => r.key === target);
    if (asLink) {
      linkSet.add(asLink.key);
      nodeSet.add(asLink.source);
      nodeSet.add(asLink.target);
      forward(asLink.target);
      backward(asLink.source);
    } else if (layout.index.has(target)) {
      nodeSet.add(target);
      forward(target);
      backward(target);
    } else {
      return null;
    }

    return { linkSet, nodeSet };
  }, [active, layout]);

  const setActive = useCallback(
    (key: string | null) => {
      setHovered(key);
      onHighlight?.(key);
    },
    [onHighlight],
  );

  if (nodes.length === 0) return null;

  const total = Math.max(1, layout.columns[0]?.reduce((a, b) => a + b.value, 0) ?? 1);
  const w = Math.max(drawWidth, 320);
  const h = layout.innerH + PAD.top + PAD.bottom;
  const scrolls = drawWidth > width + 1;
  /* Room differs by column: every column but the last writes into the gap
     before the next band, while the last writes into the margin reserved for
     it. The figure rides on the same line, so a few characters are held back
     for it. */
  const charsFor = (col: number) => {
    const room =
      col === layout.columnCount - 1 ? layout.labelRoom - 12 : layout.colWidth - NODE_W - 14;
    return Math.max(5, Math.floor(Math.max(48, room) / CHAR_W) - 5);
  };

  return (
    <div ref={wrapRef} className={`mfb-sankey${className ? ` ${className}` : ""}`}>
      <div className="mfb-sankey-wrap">
        <svg
          width={w}
          height={h}
          viewBox={`0 0 ${w} ${h}`}
          /* Not role="img": the bands inside are real, focusable controls, and
             an image would hide every one of them from assistive technology
             while leaving them in the tab order. */
          role="group"
          aria-label={`Routes through ${layout.columnCount} step${
            layout.columnCount === 1 ? "" : "s"
          }, from ${num(total)} sessions. The strongest transitions are listed under the diagram.`}
          onMouseLeave={() => setActive(null)}
        >
          <defs>
            {/* A band is a strip of the instrument's own ruled paper, so the
                ruling is printed into it rather than drawn around it. It is
                laid in the sheet's coordinates, which keeps every band's lines
                on one continuous set of rules across the whole diagram. */}
            <pattern
              id={ruleId}
              width={NODE_W}
              height="4"
              patternUnits="userSpaceOnUse"
            >
              <rect width={NODE_W} height="4" fill="var(--mf-ink-2)" />
              <line
                x1="0"
                y1="3.5"
                x2={NODE_W}
                y2="3.5"
                stroke="var(--mf-ground-2)"
                strokeWidth="1"
              />
            </pattern>
          </defs>

          <g transform={`translate(${PAD.left},${PAD.top})`}>
            {/* column headings, written above the bands they name */}
            {layout.columns.map((col, ci) => (
              <text
                key={`col-${ci}`}
                x={col[0]?.x ?? 0}
                y={-10}
                textAnchor="start"
                className="mfb-sankey__col"
              >
                {ci === 0 ? "Entry" : `Step ${ci + 1}`}
              </text>
            ))}

            {/* ribbons first, so a band always sits over the flow it holds */}
            <g>
              {layout.ribbons.map((r) => {
                const dim = route != null && !route.linkSet.has(r.key);
                const lit = route != null && route.linkSet.has(r.key);
                return (
                  <path
                    key={r.key}
                    className="mfb-sankey__link"
                    d={r.d}
                    fill={`var(--mf-s${r.ramp})`}
                    opacity={dim ? 0.09 : lit ? 0.82 : 0.42}
                    onMouseEnter={() => setActive(r.key)}
                    onClick={() => setActive(active === r.key ? null : r.key)}
                  >
                    <title>
                      {`${labelOf(layout.index, r.source)} to ${labelOf(
                        layout.index,
                        r.target,
                      )}: ${num(r.value)} sessions`}
                    </title>
                  </path>
                );
              })}
            </g>

            {/* the bands */}
            <g>
              {layout.nodes.map((n) => {
                const dim = route != null && !route.nodeSet.has(n.id);
                const text = trim(n.label, charsFor(n.col));
                return (
                  <g
                    key={n.id}
                    className="mfb-sankey__node"
                    tabIndex={0}
                    role="button"
                    aria-label={`${n.label}: ${num(n.value)} sessions, ${pct(n.value / total)} of entries`}
                    /* A band off the lifted route steps back, but its label
                       stays readable: the dim is emphasis, not concealment.
                       Held at a value where the label still clears 4.5:1;
                       the ribbons carry the emphasis, so the type does not
                       have to be sacrificed for it. */
                    opacity={dim ? 0.6 : 1}
                    onMouseEnter={() => setActive(n.id)}
                    onFocus={() => setActive(n.id)}
                    onBlur={() => setActive(null)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        setActive(active === n.id ? null : n.id);
                      }
                    }}
                  >
                    {/* the ridge of displaced ground under the band */}
                    <rect
                      x={n.x}
                      y={n.y + 1}
                      width={NODE_W}
                      height={n.h}
                      fill="var(--mf-well)"
                    />
                    <rect
                      x={n.x}
                      y={n.y}
                      width={NODE_W}
                      height={n.h}
                      /* A gathered remainder is not a page, so it is not drawn
                         as one: no ruling, and a dashed edge instead. */
                      fill={n.remainder ? "var(--mf-ground-3)" : `url(#${ruleId})`}
                      stroke={n.remainder ? "var(--mf-rule-strong)" : "none"}
                      strokeDasharray={n.remainder ? "2 2" : undefined}
                    />
                    {/* One line, always: a second line collides the moment two
                        bands sit close, and a collided label is unreadable. */}
                    <text
                      x={n.x + NODE_W + 6}
                      y={n.y + n.h / 2}
                      dominantBaseline="central"
                      textAnchor="start"
                      className={`mfb-sankey__label${
                        route?.nodeSet.has(n.id) ? " mfb-sankey__label--strong" : ""
                      }`}
                    >
                      {text}
                      <tspan className="mfb-sankey__val" dx="6">
                        {compact(n.value)}
                      </tspan>
                    </text>
                    <title>{n.label}</title>
                  </g>
                );
              })}
            </g>
          </g>
        </svg>
      </div>

      {layout.columnCount === 1 ? (
        <p className="mfb-note">
          Only one step was reached in this interval, so there is nothing to branch to yet.
        </p>
      ) : null}

      {scrolls ? (
        <p className="mfb-note">
          The switchyard is wider than this screen. Drag it sideways to follow a route to
          its end; the list under it carries the same reading in full.
        </p>
      ) : null}

      {layout.cycles > 0 || layout.gathered > 0 || layout.floored > 0 ? (
        <p className="mfb-note">
          {layout.gathered > 0
            ? `Each column keeps its ${maxPerColumn} strongest pages; the rest are gathered into one band and still counted. `
            : ""}
          {layout.floored > 0
            ? `${num(layout.floored)} band${
                layout.floored === 1 ? " is" : "s are"
              } drawn at the smallest height a label fits in, so read those by their figure rather than their depth. `
            : ""}
          {layout.cycles > 0
            ? `${num(layout.cycles)} transition${
                layout.cycles === 1 ? " returned" : "s returned"
              } to a step already passed (${num(layout.cycleValue)} sessions) and ${
                layout.cycles === 1 ? "is" : "are"
              } counted but not drawn, because a ribbon running backwards would say something the record does not.`
            : ""}
        </p>
      ) : null}
    </div>
  );
}

/* ── helpers ─────────────────────────────────────────────────────────────── */

const labelOf = (index: Map<string, Placed>, id: string) => index.get(id)?.label ?? id;

/**
 * Trims to the room the column actually has.
 *
 * Two rules, in order, because a column of ten pages that all trim to the
 * same string tells a reader nothing:
 *
 *   1. Drop leading segments WHOLE. Cutting at an arbitrary character gives
 *      "…og/why-cookieless-works", which reads as damage rather than as a
 *      shortened path.
 *   2. When even the last segment alone will not fit, keep its HEAD. A slug
 *      carries what makes it different from its neighbours at the front
 *      ("article-12-…"), so keeping the tail would collapse a whole column
 *      into one repeated string.
 */
function trim(label: string, max: number): string {
  if (label.length <= max) return label;
  if (label.startsWith("/")) {
    const parts = label.split("/").filter(Boolean);
    for (let i = 1; i < parts.length; i++) {
      const tail = `…/${parts.slice(i).join("/")}`;
      if (tail.length <= max) return tail;
    }
    const leaf = parts[parts.length - 1];
    if (parts.length > 1 && leaf && max > 4) return `…/${leaf.slice(0, max - 3)}…`;
  }
  return `${label.slice(0, Math.max(1, max - 1))}…`;
}

export default Sankey;
