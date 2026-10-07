/**
 * THE SHEET EDITOR: a paste-up board for readings.
 *
 * A sheet is one length of continuous chart paper (the pin feed runs down its
 * left edge) onto which the operator pastes readings and arranges them by hand.
 * The arrangement is real drag-to-place on a twelve-column grid, written with
 * pointer events and CSS grid and no drag-and-drop library.
 *
 * Three decisions worth keeping:
 *
 *  1. THE GAUGE. Cell geometry is measured from a one-cell element inside the
 *     grid rather than assumed from a constant, so the maths cannot drift from
 *     the stylesheet and no pixel size is hard-coded in this file.
 *  2. NO SILENT COMPACTION. A card stays exactly where it is dropped; only cards
 *     it lands on move, and they move down. Closing a gap is an explicit action
 *     ("Tidy"), because an arrangement that rearranges itself is not an
 *     arrangement.
 *  3. ARRANGING IS NOT MOUSE-ONLY. The grip is a real button: arrow keys move
 *     the card, Shift with arrow keys resizes it, and every change is announced.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent, KeyboardEvent as ReactKeyboardEvent } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import type { Audience, BreakdownRow, Envelope, Filter, Granularity, Overview, Series } from "../../lib/api";
import { Button, Empty, ErrorState, Panel, Readout, Sheet } from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import { Drum } from "../../components/react/Drum";
import { IconClose, IconPlus, IconSettings } from "../../components/react/icons";
import { ago, pct } from "../../lib/format";
import { useAppState } from "../state";
import { useSite } from "../providers";
import {
  BREAKDOWN_METRICS,
  DIMENSIONS,
  Field,
  FilterBuilder,
  KeyBank,
  METRICS,
  NumberInput,
  Reading,
  Select,
  TextInput,
  dimensionLabel,
  errText,
  metricDef,
  statsKey,
} from "./QueryScreen";
import type { MetricKey } from "./QueryScreen";
import {
  VISUALS,
  cardTitle,
  dashboardsKey,
  emptyLayout,
  newCard,
  normaliseLayout,
} from "./DashboardsScreen";
import type { CardVis, DashCard, DashLayout, Dashboard } from "./DashboardsScreen";

/* ═══ layout maths ════════════════════════════════════════════════════════ */

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

const MIN_W = 2;
/** Two rows is the floor: one row cannot hold a figure without cutting it. */
const MIN_H = 2;

const hits = (a: Rect, b: Rect) =>
  a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;

/**
 * Push whatever the moved card landed on downwards, and nothing else. The moved
 * card itself never yields: it stays where it was dropped.
 */
function resolve(cards: DashCard[], pinnedId: string): DashCard[] {
  const next = cards.map((c) => ({ ...c }));
  const order = [...next].sort((a, b) =>
    a.id === pinnedId ? -1 : b.id === pinnedId ? 1 : a.y - b.y || a.x - b.x,
  );
  const placed: DashCard[] = [];
  for (const c of order) {
    let guard = 0;
    while (placed.some((p) => hits(p, c)) && guard++ < 500) c.y += 1;
    placed.push(c);
  }
  return next;
}

/** Explicit gap-closing. Only ever run when the operator asks for it. */
function tidy(cards: DashCard[]): DashCard[] {
  const next = cards.map((c) => ({ ...c }));
  const order = [...next].sort((a, b) => a.y - b.y || a.x - b.x);
  const done: DashCard[] = [];
  for (const c of order) {
    while (c.y > 0 && !done.some((p) => hits(p, { ...c, y: c.y - 1 }))) c.y -= 1;
    done.push(c);
  }
  return next;
}

const rowsOf = (cards: DashCard[]) => Math.max(1, ...cards.map((c) => c.y + c.h));

const byPlace = (cards: DashCard[]) => [...cards].sort((a, b) => a.y - b.y || a.x - b.x);

/* ═══ small hooks ═════════════════════════════════════════════════════════ */

/** Measures a box so a chart can fill the card it was pasted into. */
function useBoxHeight<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [h, setH] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const next = entries[0]?.contentRect.height;
      if (next && Math.abs(next - h) > 2) setH(next);
    });
    ro.observe(el);
    setH(el.clientHeight);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return [ref, h] as const;
}

function useNarrow(query = "(max-width: 900px)") {
  const [narrow, setNarrow] = useState(false);
  useEffect(() => {
    const mq = window.matchMedia(query);
    const on = () => setNarrow(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, [query]);
  return narrow;
}

/* ═══ one reading ═════════════════════════════════════════════════════════ */

const RAMP = ["var(--mf-s1)", "var(--mf-s2)", "var(--mf-s3)", "var(--mf-s4)", "var(--mf-s5)"];

function endpointFor(vis: CardVis): string {
  if (vis === "record") return "/api/stats/timeseries";
  if (vis === "figure") return "/api/stats/overview";
  return "/api/stats/breakdown";
}

/**
 * The height the drum's plot may occupy inside a card: the body box, less the
 * readout strip the drum always writes beneath it (margin included). Measured
 * from the real elements so no pixel figure is guessed here or in the sheet.
 */
function useDrumHeight(host: { current: HTMLElement | null }, bodyH: number, on: boolean) {
  const [foot, setFoot] = useState(0);
  useEffect(() => {
    if (!on) return;
    const el = host.current?.querySelector(".mf-drum__readout") as HTMLElement | null;
    if (!el) return;
    const read = () => {
      const box = el.getBoundingClientRect().height;
      const mt = parseFloat(window.getComputedStyle(el).marginTop) || 0;
      setFoot((prev) => (Math.abs(prev - (box + mt)) > 1 ? box + mt : prev));
    };
    const ro = new ResizeObserver(read);
    ro.observe(el);
    read();
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [on, bodyH]);
  return Math.max(0, bodyH - foot);
}

function CardReading({ card }: { card: DashCard }) {
  const { range, audience, filters } = useAppState();
  const { siteId } = useSite();
  const [bodyRef, bodyH] = useBoxHeight<HTMLDivElement>();
  const drumH = useDrumHeight(bodyRef, bodyH, card.vis === "record");

  const aud: Audience = card.audience === "inherit" ? audience : card.audience;
  const merged: Filter[] = useMemo(
    () => [...(filters ?? []), ...card.filters],
    [filters, card.filters],
  );
  const endpoint = endpointFor(card.vis);

  const env = useMemo<Envelope>(() => {
    const e: Envelope = { site: siteId ?? 0, range, audience: aud, filters: merged };
    if (card.vis === "record") {
      e.metric = card.metric;
      if (card.granularity) e.granularity = card.granularity;
    }
    if (card.vis === "sheet" || card.vis === "distribution") {
      e.dimension = card.dimension;
      e.limit = card.limit;
    }
    return e;
  }, [siteId, range, aud, merged, card.vis, card.metric, card.granularity, card.dimension, card.limit]);

  const q = useQuery({
    queryKey: statsKey(endpoint, siteId, range, aud, merged, {
      vis: card.vis,
      metric: card.metric,
      dimension: card.dimension,
      limit: card.limit,
      granularity: card.granularity,
    }),
    enabled: siteId != null,
    queryFn: () => api.get<Overview | Series | BreakdownRow[]>(endpoint, env),
  });

  const data = q.data?.data;
  const def = metricDef(card.metric);

  const isEmpty = (() => {
    if (data == null) return true;
    if (card.vis === "record") return ((data as Series).buckets ?? []).length === 0;
    if (card.vis === "figure") {
      const o = data as Overview;
      return !o || (o.visitors === 0 && o.sessions === 0 && o.pageviews === 0);
    }
    return (data as BreakdownRow[]).length === 0;
  })();

  return (
    <div className="mf-bn-card__body" ref={bodyRef}>
      <Reading
        loading={q.isLoading}
        error={q.isError ? q.error : undefined}
        onRetry={() => void q.refetch()}
        isEmpty={isEmpty}
        lines={3}
        empty={
          <p className="mf-bn-card__none">
            Nothing in this interval. Widen the interval, or loosen this card&rsquo;s conditions.
          </p>
        }
      >
        {card.vis === "figure" ? (
          <Readout
            dense
            channels={[
              {
                key: card.metric,
                label: def.label,
                value: def.fmt(Number((data as unknown as Record<string, number>)?.[card.metric] ?? 0)),
                note: aud === "agent" ? "machines" : aud === "all" ? "readers and machines" : "readers",
              } as Channel,
            ]}
          />
        ) : null}

        {card.vis === "record" && data ? (
          <Drum
            times={(data as Series).buckets.map((b) => b.t)}
            height={drumH > 0 ? drumH : undefined}
            channels={[
              {
                id: card.id,
                label: def.label,
                color: aud === "agent" ? "var(--mf-trace-agent)" : "var(--mf-trace-human)",
                pen: aud === "agent" ? "stepped" : "written",
                values: (data as Series).buckets.map((b) => b.v),
                format: def.fmt,
              },
            ]}
          />
        ) : null}

        {card.vis === "sheet" && data ? (
          <Sheet
            rows={data as BreakdownRow[]}
            getKey={(r, i) => `${r.key}-${i}`}
            columns={
              [
                {
                  key: "label",
                  head: dimensionLabel(card.dimension),
                  render: (r: BreakdownRow) => <span title={r.label || r.key}>{r.label || r.key || "(not set)"}</span>,
                  share: (r: BreakdownRow) => r.share,
                },
                {
                  key: card.metric,
                  head: def.label,
                  numeric: true,
                  render: (r: BreakdownRow) => (
                    <span className="mf-data">
                      {def.fmt(Number((r as unknown as Record<string, number>)[card.metric] ?? 0))}
                    </span>
                  ),
                },
              ] as Column<BreakdownRow>[]
            }
          />
        ) : null}

        {card.vis === "distribution" && data ? (
          <Distribution rows={data as BreakdownRow[]} metric={card.metric} />
        ) : null}
      </Reading>
    </div>
  );
}

/**
 * A proportion band. One length of the record divided by share, with every
 * segment named in the list under it, so the picture never depends on being
 * able to tell two tones apart.
 */
function Distribution({ rows, metric }: { rows: BreakdownRow[]; metric: MetricKey }) {
  const def = metricDef(metric);
  const top = rows.slice(0, 5);
  const totalShare = rows.reduce((s, r) => s + (r.share ?? 0), 0);
  const shown = top.reduce((s, r) => s + (r.share ?? 0), 0);
  const rest = Math.max(0, totalShare - shown);
  const label = top.map((r) => `${r.label || r.key}: ${pct(r.share)}`).join(", ");

  return (
    <div className="mf-bn-dist">
      <div className="mf-bn-dist__band" role="img" aria-label={`Shares: ${label}`}>
        {top.map((r, i) => (
          <span
            key={`${r.key}-${i}`}
            className="mf-bn-dist__seg"
            style={{ flexGrow: Math.max(0.001, r.share ?? 0), background: RAMP[i % RAMP.length] }}
          />
        ))}
        {rest > 0.001 ? (
          <span className="mf-bn-dist__seg mf-bn-dist__seg--rest" style={{ flexGrow: rest }} />
        ) : null}
      </div>
      <ul className="mf-bn-dist__list">
        {top.map((r, i) => (
          <li key={`${r.key}-${i}`}>
            <span className="mf-bn-dist__mark" style={{ background: RAMP[i % RAMP.length] }} aria-hidden="true" />
            <span className="mf-bn-dist__name" title={r.label || r.key}>
              {r.label || r.key || "(not set)"}
            </span>
            <span className="mf-bn-dist__pct mf-data">{pct(r.share)}</span>
            <span className="mf-bn-dist__val mf-data">
              {def.fmt(Number((r as unknown as Record<string, number>)[metric] ?? 0))}
            </span>
          </li>
        ))}
        {rest > 0.001 ? (
          <li>
            <span className="mf-bn-dist__mark mf-bn-dist__mark--rest" aria-hidden="true" />
            <span className="mf-bn-dist__name">Everything else</span>
            <span className="mf-bn-dist__pct mf-data">{pct(rest)}</span>
            <span className="mf-bn-dist__val mf-data">—</span>
          </li>
        ) : null}
      </ul>
    </div>
  );
}

/* ═══ the card composer ═══════════════════════════════════════════════════ */

function CardComposer({
  draft,
  onChange,
  onCommit,
  onCancel,
  mode,
}: {
  draft: DashCard;
  onChange: (c: DashCard) => void;
  onCommit: () => void;
  onCancel: () => void;
  mode: "add" | "edit";
}) {
  const set = (p: Partial<DashCard>) => onChange({ ...draft, ...p });
  const ranked = draft.vis === "sheet" || draft.vis === "distribution";
  const visNote = VISUALS.find((v) => v.v === draft.vis)?.note ?? "";

  return (
    <section className="mf-bn-composer" aria-label={mode === "add" ? "New reading" : "Edit reading"}>
      <h3 className="mf-bn-composer__title">{mode === "add" ? "Pin a reading" : "Edit this reading"}</h3>

      <div className="mf-bn-build__row">
        <Field
          label="Drawn as"
          hint={visNote}
          control={(id) => (
            <KeyBank<CardVis>
              id={id}
              label="Drawn as"
              value={draft.vis}
              onChange={(v) =>
                set({
                  vis: v,
                  // A record and a sheet want different room; give the card a
                  // sensible shape for what it is about to hold.
                  w: v === "figure" ? Math.min(draft.w, 3) : Math.max(draft.w, 6),
                  h: v === "figure" ? Math.max(MIN_H, Math.min(draft.h, 2)) : Math.max(draft.h, 4),
                })
              }
              options={VISUALS.map((v) => ({ v: v.v, label: v.label }))}
            />
          )}
        />
        <Field
          label="Metric"
          control={(id) => (
            <Select id={id} value={draft.metric} onChange={(v) => set({ metric: v as MetricKey })}>
              {(ranked ? BREAKDOWN_METRICS : METRICS).map((m) => (
                <option key={m.v} value={m.v}>
                  {m.label}
                </option>
              ))}
            </Select>
          )}
        />
        <Field
          label="Audience"
          control={(id) => (
            <Select
              id={id}
              value={draft.audience}
              onChange={(v) => set({ audience: v as DashCard["audience"] })}
            >
              <option value="inherit">Follows the screen</option>
              <option value="human">Readers</option>
              <option value="agent">Machines</option>
              <option value="all">Both</option>
            </Select>
          )}
        />
      </div>

      <div className="mf-bn-build__row">
        {ranked ? (
          <>
            <Field
              label="Dimension"
              control={(id) => (
                <Select
                  id={id}
                  value={
                    draft.dimension.startsWith("props.") || draft.dimension.startsWith("flag.")
                      ? "__custom"
                      : draft.dimension
                  }
                  onChange={(v) => set({ dimension: v === "__custom" ? "props." : v })}
                >
                  {DIMENSIONS.map((g) => (
                    <optgroup key={g.group} label={g.group}>
                      {g.items.map((it) => (
                        <option key={it.v} value={it.v}>
                          {it.label}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                  <optgroup label="Custom">
                    <option value="__custom">Property or flag…</option>
                  </optgroup>
                </Select>
              )}
            />
            {draft.dimension.startsWith("props.") || draft.dimension.startsWith("flag.") ? (
              <Field
                label="Key"
                hint="props.plan, or flag.checkout-v2"
                control={(id) => (
                  <TextInput id={id} mono value={draft.dimension} onChange={(v) => set({ dimension: v })} />
                )}
              />
            ) : null}
            <Field
              label="Rows"
              control={(id) => (
                <NumberInput
                  id={id}
                  value={draft.limit}
                  min={1}
                  max={100}
                  onChange={(v) => set({ limit: Math.max(1, Math.min(100, Math.round(v))) })}
                />
              )}
            />
          </>
        ) : null}

        {draft.vis === "record" ? (
          <Field
            label="Time grain"
            hint="Automatic lets the server fit the grain to the interval."
            control={(id) => (
              <Select
                id={id}
                value={draft.granularity}
                onChange={(v) => set({ granularity: v as Granularity | "" })}
              >
                <option value="">Automatic</option>
                <option value="hour">Hour</option>
                <option value="day">Day</option>
                <option value="week">Week</option>
                <option value="month">Month</option>
              </Select>
            )}
          />
        ) : null}

        <Field
          label="Title"
          hint={`Without one, this card is called: ${cardTitle({ ...draft, title: "" })}`}
          control={(id) => (
            <TextInput id={id} value={draft.title} onChange={(v) => set({ title: v })} placeholder={cardTitle({ ...draft, title: "" })} />
          )}
        />
      </div>

      <div className="mf-bn-build__row mf-bn-build__row--cond">
        <Field
          label="Conditions for this card"
          wide
          control={() => (
            <FilterBuilder
              value={draft.filters}
              onChange={(filters) => set({ filters })}
              emptyNote="No conditions of its own. This card reads whatever the screen is set to."
            />
          )}
        />
      </div>

      <div className="mf-bn-build__act">
        <Button tone="primary" onClick={onCommit}>
          {mode === "add" ? "Pin it to the sheet" : "Save this reading"}
        </Button>
        <Button tone="quiet" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </section>
  );
}

/* ═══ the editor ══════════════════════════════════════════════════════════ */

interface Geo {
  cellW: number;
  rowH: number;
  colGap: number;
  rowGap: number;
}

interface DragState {
  id: string;
  mode: "move" | "size";
  startX: number;
  startY: number;
  origin: Rect;
  target: Rect;
  dx: number;
  dy: number;
  geo: Geo;
}

export function DashboardEditor({ id: idProp }: { id?: string } = {}) {
  const params = useParams();
  const id = idProp ?? params.id ?? "";
  const site = params.site;
  const { siteId, isLoading: siteLoading } = useSite();
  const qc = useQueryClient();
  const narrow = useNarrow();

  const gridRef = useRef<HTMLDivElement | null>(null);
  const gaugeRef = useRef<HTMLDivElement | null>(null);

  const q = useQuery({
    queryKey: ["bench", "dashboard", id],
    enabled: !!id,
    queryFn: async () => {
      const res = await api.get<Dashboard>(`/api/dashboards/${id}`);
      return { ...res.data, layout: normaliseLayout(res.data?.layout) };
    },
  });

  const [name, setName] = useState("");
  const [layout, setLayout] = useState<DashLayout>(() => emptyLayout());
  const [saved, setSaved] = useState<string>("");
  const [composing, setComposing] = useState<{ mode: "add" | "edit"; draft: DashCard } | null>(null);
  const [drag, setDragState] = useState<DragState | null>(null);
  const dragRef = useRef<DragState | null>(null);
  const setDrag = useCallback((d: DragState | null) => {
    dragRef.current = d;
    setDragState(d);
  }, []);
  const [say, setSay] = useState("");

  // Load into local state once the record arrives.
  useEffect(() => {
    if (!q.data) return;
    setName(q.data.name ?? "");
    setLayout(q.data.layout);
    setSaved(JSON.stringify({ name: q.data.name ?? "", layout: q.data.layout }));
  }, [q.data]);

  const current = JSON.stringify({ name, layout });
  const dirty = saved !== "" && current !== saved;

  useEffect(() => {
    if (!dirty) return;
    const guard = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", guard);
    return () => window.removeEventListener("beforeunload", guard);
  }, [dirty]);

  const save = useMutation({
    mutationFn: () => api.patch<Dashboard>(`/api/dashboards/${id}`, { name, layout }),
    onSuccess: () => {
      setSaved(JSON.stringify({ name, layout }));
      void qc.invalidateQueries({ queryKey: dashboardsKey(siteId) });
      void qc.invalidateQueries({ queryKey: ["bench", "dashboard", id] });
    },
  });

  const setCards = useCallback(
    (cards: DashCard[]) => setLayout((l) => ({ ...l, cards })),
    [],
  );

  /* ── geometry, measured rather than assumed ──────────────────────────── */
  const geometry = useCallback((): Geo | null => {
    const grid = gridRef.current;
    const gauge = gaugeRef.current;
    if (!grid || !gauge) return null;
    const g = gauge.getBoundingClientRect();
    if (!(g.width > 0) || !(g.height > 0)) return null;
    const cs = window.getComputedStyle(grid);
    return {
      cellW: g.width,
      rowH: g.height,
      colGap: parseFloat(cs.columnGap) || 0,
      rowGap: parseFloat(cs.rowGap) || 0,
    };
  }, []);

  const beginDrag = (e: ReactPointerEvent, card: DashCard, mode: "move" | "size") => {
    if (narrow) return;
    const geo = geometry();
    if (!geo) return;
    e.preventDefault();
    // preventDefault stops the drag selecting text, but it also stops the
    // handle taking focus, so give it focus back: arrow keys must keep working
    // on the card the pointer just moved.
    (e.currentTarget as HTMLElement).focus?.();
    (e.currentTarget as Element).setPointerCapture?.(e.pointerId);
    const origin: Rect = { x: card.x, y: card.y, w: card.w, h: card.h };
    setDrag({ id: card.id, mode, startX: e.clientX, startY: e.clientY, origin, target: origin, dx: 0, dy: 0, geo });
  };

  const moveDrag = (e: ReactPointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    const dx = e.clientX - d.startX;
    const dy = e.clientY - d.startY;
    const stepX = d.geo.cellW + d.geo.colGap;
    const stepY = d.geo.rowH + d.geo.rowGap;
    const cx = Math.round(dx / stepX);
    const cy = Math.round(dy / stepY);
    const target: Rect =
      d.mode === "move"
        ? {
            x: Math.max(0, Math.min(layout.cols - d.origin.w, d.origin.x + cx)),
            y: Math.max(0, d.origin.y + cy),
            w: d.origin.w,
            h: d.origin.h,
          }
        : {
            x: d.origin.x,
            y: d.origin.y,
            w: Math.max(MIN_W, Math.min(layout.cols - d.origin.x, d.origin.w + cx)),
            h: Math.max(MIN_H, d.origin.h + cy),
          };
    setDrag({ ...d, dx, dy, target });
  };

  const endDrag = () => {
    const d = dragRef.current;
    if (!d) return;
    setDrag(null);
    const moved = layout.cards.map((c) => (c.id === d.id ? { ...c, ...d.target } : c));
    setCards(resolve(moved, d.id));
    const c = layout.cards.find((x) => x.id === d.id);
    if (c) {
      setSay(
        `${cardTitle(c)} at column ${d.target.x + 1}, row ${d.target.y + 1}, ${d.target.w} by ${d.target.h}.`,
      );
    }
  };

  const nudge = (e: ReactKeyboardEvent, card: DashCard, forceResize = false) => {
    const keys = ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"];
    if (!keys.includes(e.key)) return;
    e.preventDefault();
    const resize = forceResize || e.shiftKey;
    const dx = e.key === "ArrowLeft" ? -1 : e.key === "ArrowRight" ? 1 : 0;
    const dy = e.key === "ArrowUp" ? -1 : e.key === "ArrowDown" ? 1 : 0;
    const next: Rect = resize
      ? {
          x: card.x,
          y: card.y,
          w: Math.max(MIN_W, Math.min(layout.cols - card.x, card.w + dx)),
          h: Math.max(MIN_H, card.h + dy),
        }
      : {
          x: Math.max(0, Math.min(layout.cols - card.w, card.x + dx)),
          y: Math.max(0, card.y + dy),
          w: card.w,
          h: card.h,
        };
    setCards(resolve(layout.cards.map((c) => (c.id === card.id ? { ...c, ...next } : c)), card.id));
    setSay(
      resize
        ? `${cardTitle(card)} resized to ${next.w} by ${next.h}.`
        : `${cardTitle(card)} at column ${next.x + 1}, row ${next.y + 1}.`,
    );
  };

  /* ── composing ───────────────────────────────────────────────────────── */
  const startAdd = () => {
    const y = layout.cards.length ? rowsOf(layout.cards) : 0;
    setComposing({ mode: "add", draft: newCard({ x: 0, y, w: 6, h: 4, vis: "record" }) });
  };

  const commitCard = () => {
    if (!composing) return;
    const d = composing.draft;
    if (composing.mode === "add") {
      setCards(resolve([...layout.cards, d], d.id));
    } else {
      setCards(layout.cards.map((c) => (c.id === d.id ? d : c)));
    }
    setComposing(null);
  };

  const ordered = byPlace(layout.cards);

  if (q.isError) {
    return (
      <div className="mf-bn-page">
        <Panel>
          <ErrorState
            title="That sheet did not open"
            detail={errText(q.error)}
            onRetry={() => void q.refetch()}
          />
          <p className="mf-bn-back">
            <Link to={`/app/${site}/dashboards`}>All sheets</Link>
          </p>
        </Panel>
      </div>
    );
  }

  return (
    <div className="mf-bn-page">
      <p className="mf-bn-back">
        <Link to={`/app/${site}/dashboards`}>All sheets</Link>
      </p>

      <Panel feed className="mf-bn-sheet">
        <div className="mf-bn-sheet__head">
          <div className="mf-bn-sheet__ident">
            <label className="mf-sr" htmlFor="mf-sheet-name">
              Sheet name
            </label>
            <input
              id="mf-sheet-name"
              className="mf-bn-sheet__name"
              value={name}
              placeholder="Untitled sheet"
              onChange={(e) => setName(e.target.value)}
            />
            <p className="mf-bn-sheet__state">
              <span className="mf-data">{layout.cards.length}</span>{" "}
              {layout.cards.length === 1 ? "reading" : "readings"}
              {dirty ? (
                <> · unsaved changes</>
              ) : q.data?.updated_at ? (
                <> · saved {ago(q.data.updated_at)}</>
              ) : null}
            </p>
          </div>

          <div className="mf-bn-sheet__act">
            <Button icon={<IconPlus size={13} />} onClick={startAdd} disabled={!!composing}>
              Add a reading
            </Button>
            <Button
              onClick={() => setCards(tidy(layout.cards))}
              disabled={layout.cards.length < 2}
              title="Close the vertical gaps between cards"
            >
              Tidy
            </Button>
            <Button
              tone="quiet"
              disabled={!dirty}
              onClick={() => {
                if (!q.data) return;
                setName(q.data.name ?? "");
                setLayout(q.data.layout);
              }}
            >
              Revert
            </Button>
            <Button tone="primary" disabled={!dirty || save.isPending} onClick={() => save.mutate()}>
              {save.isPending ? "Saving…" : "Save the sheet"}
            </Button>
          </div>
        </div>

        {save.isError ? (
          <p className="mf-bn-field__err" role="alert">
            The sheet was not saved. {errText(save.error)}
          </p>
        ) : null}

        {composing ? (
          <CardComposer
            mode={composing.mode}
            draft={composing.draft}
            onChange={(draft) => setComposing({ ...composing, draft })}
            onCommit={commitCard}
            onCancel={() => setComposing(null)}
          />
        ) : null}

        <p className="mf-sr" aria-live="polite">
          {say}
        </p>

        <Reading
          loading={siteLoading || q.isLoading}
          error={undefined}
          isEmpty={layout.cards.length === 0 && !composing}
          lines={4}
          empty={
            <Empty
              title="Nothing pinned to this sheet yet"
              body="Add a reading: choose a metric, a dimension, how it should be drawn and its own conditions. Then drag the cards into the arrangement you want."
              action={
                <Button tone="primary" icon={<IconPlus size={13} />} onClick={startAdd}>
                  Add a reading
                </Button>
              }
            />
          }
        >
          <div
            ref={gridRef}
            className={`mf-bn-grid${drag ? " is-arranging" : ""}`}
            onPointerMove={drag ? moveDrag : undefined}
            onPointerUp={drag ? endDrag : undefined}
            onPointerCancel={drag ? endDrag : undefined}
          >
            {/* The gauge: one cell, measured so no pixel size is written in JS. */}
            <div ref={gaugeRef} className="mf-bn-gauge" aria-hidden="true" />

            {drag && drag.mode === "move" ? (
              <div
                className="mf-bn-slot"
                aria-hidden="true"
                style={{
                  gridColumn: `${drag.target.x + 1} / span ${drag.target.w}`,
                  gridRow: `${drag.target.y + 1} / span ${drag.target.h}`,
                }}
              >
                <span /> <span /> <span /> <span />
              </div>
            ) : null}

            {ordered.map((card) => {
              const live = drag?.id === card.id ? drag : null;
              const rect: Rect = live && live.mode === "size" ? live.target : card;
              return (
                <article
                  key={card.id}
                  className={`mf-bn-card${live ? ` is-dragging is-${live.mode}` : ""}`}
                  style={{
                    gridColumn: `${rect.x + 1} / span ${rect.w}`,
                    gridRow: `${rect.y + 1} / span ${rect.h}`,
                    transform:
                      live && live.mode === "move" ? `translate(${live.dx}px, ${live.dy}px)` : undefined,
                  }}
                >
                  <header className="mf-bn-card__head">
                    <button
                      type="button"
                      className="mf-bn-card__grip"
                      disabled={narrow}
                      aria-label={
                        narrow
                          ? `${cardTitle(card)}. Arranging needs a wider screen.`
                          : `Move ${cardTitle(card)}. Column ${card.x + 1}, row ${card.y + 1}. Arrow keys move it, Shift with arrow keys resizes it.`
                      }
                      onPointerDown={(e) => beginDrag(e, card, "move")}
                      onKeyDown={(e) => nudge(e, card)}
                    >
                      <span className="mf-bn-card__perf" aria-hidden="true" />
                    </button>

                    <h3 className="mf-bn-card__title">{cardTitle(card)}</h3>

                    <div className="mf-bn-card__keys">
                      <Button
                        size="sm"
                        tone="quiet"
                        icon={<IconSettings size={13} />}
                        aria-label={`Edit ${cardTitle(card)}`}
                        onClick={() => setComposing({ mode: "edit", draft: { ...card } })}
                      />
                      <Button
                        size="sm"
                        tone="quiet"
                        icon={<IconClose size={13} />}
                        aria-label={`Remove ${cardTitle(card)}`}
                        onClick={() => setCards(layout.cards.filter((c) => c.id !== card.id))}
                      />
                    </div>
                  </header>

                  <CardReading card={card} />

                  <button
                    type="button"
                    className="mf-bn-card__scale"
                    disabled={narrow}
                    aria-label={`Resize ${cardTitle(card)}. ${card.w} columns by ${card.h} rows.`}
                    onPointerDown={(e) => beginDrag(e, card, "size")}
                    onKeyDown={(e) => nudge(e, card, true)}
                  >
                    <span aria-hidden="true" />
                  </button>
                </article>
              );
            })}
          </div>
        </Reading>

        {narrow && layout.cards.length > 1 ? (
          <p className="mf-bn-sheet__narrow">
            Stacked in reading order. Arranging needs a screen at least 900 pixels wide.
          </p>
        ) : null}
      </Panel>
    </div>
  );
}

export default DashboardEditor;
