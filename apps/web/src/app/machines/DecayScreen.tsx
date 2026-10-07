/**
 * Content decay, at /app/:site/decay
 *
 * Pages whose readers are draining away while the machines keep reading them.
 * Reached from the Citation Gap, because it is the question that report raises
 * and cannot answer on its own.
 *
 * Every row carries the same mirrored figure as the big record, drawn at the
 * scale of a row: the machine pen stepping up from the rule, the reader pen
 * written down from it. Each pen is scaled to its own maximum, because at 34 pixels
 * tall a shared scale would pin one of them flat and record nothing.
 */
import { useMemo } from "react";
import { useNavigate, useParams } from "react-router";
import { useQuery, keepPreviousData } from "@tanstack/react-query";

import { api } from "../../lib/api";
import { Empty, Panel, PanelHead, Readout, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { ago, delta, num, pct, shortPath } from "../../lib/format";
import { Bench, Reading, Waiting, sorted, useMachineEnv, useSort } from "./AgentDetail";
import type { MachineEnv } from "./AgentDetail";

interface DecayRow {
  path: string;
  title: string;
  human_now: number;
  human_before: number;
  agent_now: number;
  agent_before: number;
  answer_arrivals?: number;
  last_modified?: number | null;
  /** Optional per-bucket series; a two-point trace is drawn without them. */
  human_trace?: number[];
  agent_trace?: number[];
}

/* ── the row trace ──────────────────────────────────────────────────────── */

/** A crawler fetches in discrete jumps, so its pen steps. */
function steppedPath(pts: [number, number][]): string {
  if (!pts.length) return "";
  let d = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 1; i < pts.length; i++) {
    const mid = (pts[i - 1][0] + pts[i][0]) / 2;
    d += `L${mid},${pts[i - 1][1]}L${mid},${pts[i][1]}L${pts[i][0]},${pts[i][1]}`;
  }
  return d;
}

/** A reader arrives continuously, so their pen is written. */
function writtenPath(pts: [number, number][]): string {
  if (!pts.length) return "";
  if (pts.length === 1) return `M${pts[0][0]},${pts[0][1]}`;
  let d = `M${pts[0][0]},${pts[0][1]}`;
  for (let i = 1; i < pts.length; i++) {
    const [px, py] = pts[i - 1];
    const [cx, cy] = pts[i];
    d += `Q${px},${py} ${(px + cx) / 2},${(py + cy) / 2}`;
  }
  const last = pts[pts.length - 1];
  d += `L${last[0]},${last[1]}`;
  return d;
}

const TRACE_W = 128;
const TRACE_H = 40;

function RowTrace({
  machines,
  readers,
  machineFull,
  readerFull,
  label,
}: {
  machines: number[];
  readers: number[];
  /** Full scale for each pen, taken across every row so the column compares. */
  machineFull: number;
  readerFull: number;
  label: string;
}) {
  const mid = TRACE_H / 2;
  const span = mid - 2;

  const path = (values: number[], full: number, dir: -1 | 1, stepped: boolean) => {
    if (!values.length) return "";
    const max = full > 0 ? full : 0;
    if (max <= 0) return `M0,${mid}L${TRACE_W},${mid}`;
    const pts: [number, number][] = values.map((v, i) => [
      values.length <= 1 ? TRACE_W / 2 : (i / (values.length - 1)) * TRACE_W,
      mid + dir * (v / max) * span,
    ]);
    return stepped ? steppedPath(pts) : writtenPath(pts);
  };

  const up = path(machines, machineFull, -1, true);
  const down = path(readers, readerFull, 1, false);

  return (
    <svg
      className="mf-rowtrace"
      width={TRACE_W}
      height={TRACE_H}
      viewBox={`0 0 ${TRACE_W} ${TRACE_H}`}
      role="img"
      aria-label={label}
    >
      {/* The centre rule is the zero both pens are measured from. It has to be
          legible or the mirror means nothing, so it is drawn at full strength
          with a fiducial tick at each end. */}
      <line
        x1={0}
        x2={TRACE_W}
        y1={mid}
        y2={mid}
        stroke="var(--mf-rule-strong)"
        strokeWidth={1}
        shapeRendering="crispEdges"
      />
      <line x1={0} x2={0} y1={mid - 3} y2={mid + 3} stroke="var(--mf-rule-strong)" strokeWidth={1} shapeRendering="crispEdges" />
      <line x1={TRACE_W} x2={TRACE_W} y1={mid - 3} y2={mid + 3} stroke="var(--mf-rule-strong)" strokeWidth={1} shapeRendering="crispEdges" />
      {/* the ridge of displaced soot, the same one-sided shadow as the drum */}
      <path d={up} fill="none" stroke="var(--mf-well)" strokeWidth={2.2} transform="translate(0,1)" opacity={0.85} />
      <path d={down} fill="none" stroke="var(--mf-well)" strokeWidth={2.2} transform="translate(0,1)" opacity={0.85} />
      <path d={up} fill="none" stroke="var(--mf-trace-agent)" strokeWidth={1.4} strokeLinecap="butt" />
      <path d={down} fill="none" stroke="var(--mf-trace-human)" strokeWidth={1.4} strokeLinecap="butt" />
    </svg>
  );
}

/* ── the change figure ──────────────────────────────────────────────────── */

function Change({ now, before, tone }: { now: number; before: number; tone: "reader" | "machine" }) {
  const d = delta(now, before);
  if (d == null) {
    return <span className="mf-dim">no earlier reading</span>;
  }
  const sign = d > 0 ? "+" : d < 0 ? "-" : "";
  const value = `${sign}${Math.abs(d * 100) < 0.5 ? "0" : Math.round(Math.abs(d) * 100)}%`;
  return (
    <span
      className="mf-change mf-data"
      data-tone={tone === "reader" ? (d < 0 ? "warn" : d > 0 ? "good" : "flat") : "flat"}
    >
      {value}
    </span>
  );
}

/* ── the sheet ──────────────────────────────────────────────────────────── */

function useDecay(bench: MachineEnv) {
  return useQuery({
    queryKey: bench.key("content/decay"),
    queryFn: async () => (await api.get<DecayRow[]>("/api/content/decay", { ...bench.env, limit: 100 })).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });
}

function DecaySheet({ bench, q }: { bench: MachineEnv; q: ReturnType<typeof useDecay> }) {
  const sort = useSort<"path" | "readers" | "lost" | "machines" | "shift">("lost");
  const navigate = useNavigate();
  const { site } = useParams();

  /* One full-scale value per pen for the whole sheet, so a heavy page draws a
     tall trace and a quiet one draws a short trace. Scaling each row to itself
     would make every row look equally busy, which is the opposite of the
     finding this screen exists to show. */
  const scale = useMemo(() => {
    let machine = 0;
    let reader = 0;
    for (const r of q.data ?? []) {
      for (const v of r.agent_trace?.length ? r.agent_trace : [r.agent_before, r.agent_now]) {
        if (v > machine) machine = v;
      }
      for (const v of r.human_trace?.length ? r.human_trace : [r.human_before, r.human_now]) {
        if (v > reader) reader = v;
      }
    }
    return { machine, reader };
  }, [q.data]);

  const rows = useMemo(() => {
    const base = q.data ?? [];
    return sorted(
      base,
      (r) =>
        sort.by === "path"
          ? r.path
          : sort.by === "readers"
            ? r.human_now
            : sort.by === "machines"
              ? r.agent_now
              : sort.by === "shift"
                ? (delta(r.agent_now, r.agent_before) ?? 0)
                : // readers lost, as a count rather than a rate
                  Math.max(0, r.human_before - r.human_now),
      sort.dir,
    );
  }, [q.data, sort.by, sort.dir]);

  const columns: Column<DecayRow>[] = [
    {
      key: "path",
      head: sort.head("path", "Page"),
      width: "30%",
      render: (r) => (
        <span className="mf-wedge-stack">
          <span className="mf-path" title={r.path}>
            {shortPath(r.path, 38)}
          </span>
          <span className="mf-wedge-stack__sub">
            {r.title || "untitled"}
            {r.last_modified ? ` · changed ${ago(r.last_modified)}` : ""}
          </span>
        </span>
      ),
    },
    {
      key: "trace",
      head: "Both pens",
      render: (r) => (
        <RowTrace
          machines={r.agent_trace?.length ? r.agent_trace : [r.agent_before, r.agent_now]}
          readers={r.human_trace?.length ? r.human_trace : [r.human_before, r.human_now]}
          machineFull={scale.machine}
          readerFull={scale.reader}
          label={`${r.path}: machine fetches ${num(r.agent_before)} to ${num(r.agent_now)}, readers ${num(
            r.human_before,
          )} to ${num(r.human_now)}`}
        />
      ),
    },
    {
      key: "readers",
      head: sort.head("readers", "Readers now"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.human_now)}</span>,
    },
    {
      key: "lost",
      head: sort.head("lost", "Reader change"),
      numeric: true,
      render: (r) => <Change now={r.human_now} before={r.human_before} tone="reader" />,
    },
    {
      key: "machines",
      head: sort.head("machines", "Fetches now"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.agent_now)}</span>,
    },
    {
      key: "shift",
      head: sort.head("shift", "Fetch change"),
      numeric: true,
      render: (r) => <Change now={r.agent_now} before={r.agent_before} tone="machine" />,
    },
    {
      key: "back",
      head: "Returned",
      numeric: true,
      render: (r) =>
        r.answer_arrivals == null ? (
          <span className="mf-dim">not measured</span>
        ) : (
          <span className={r.answer_arrivals ? "mf-data" : "mf-data mf-dim"}>{num(r.answer_arrivals)}</span>
        ),
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="Losing readers, still being read"
        note="A page can keep every machine reader it ever had and quietly lose every human one. These are the pages doing that, ranked by the readers each lost. Both pens share one scale down the column, so the traces compare row to row; selecting a row reads the whole Citation Gap through that page."
      />
      <div className="mf-wedge__body">
        <p className="mf-rowtrace__legend">
          <span>
            <span className="mf-rowtrace__pen" data-pen="stepped" aria-hidden="true" />
            Machine fetches, written up from the rule
          </span>
          <span>
            <span className="mf-rowtrace__pen" aria-hidden="true" />
            Readers, written down from it
          </span>
        </p>
      </div>
      <Reading
        query={q}
        waiting={<Waiting rows={7} />}
        isEmpty={(d) => !d?.length}
        empty={
          <Empty
            title="No page is losing its readers"
            body="Decay compares each page against the previous period of the same length. Nothing in the register lost human traffic while the machines held or increased theirs. That is a result, not a missing reading."
          />
        }
      >
        {() => (
          <Sheet
            rows={rows}
            columns={columns}
            getKey={(r) => r.path}
            onRowClick={(r) => {
              bench.addFilter({ dimension: "pathname", op: "is", value: r.path });
              navigate(site ? `/app/${site}/citation-gap` : "/app");
            }}
          />
        )}
      </Reading>
    </Panel>
  );
}

/* ── the totals ─────────────────────────────────────────────────────────── */

function DecayTotals({ q }: { q: ReturnType<typeof useDecay> }) {
  const totals = useMemo(() => {
    const rows = q.data ?? [];
    const lost = rows.reduce((a, r) => a + Math.max(0, r.human_before - r.human_now), 0);
    const before = rows.reduce((a, r) => a + r.human_before, 0);
    const fetches = rows.reduce((a, r) => a + r.agent_now, 0);
    const fetchesBefore = rows.reduce((a, r) => a + r.agent_before, 0);
    const returned = rows.reduce((a, r) => a + (r.answer_arrivals ?? 0), 0);
    return { pages: rows.length, lost, before, fetches, fetchesBefore, returned };
  }, [q.data]);

  return (
    <Panel className="mf-panel--bank">
      <Reading
        query={q}
        waiting={<Waiting rows={2} />}
        isEmpty={(d) => !d?.length}
        empty={
          <Empty
            title="Nothing to total yet"
            body="These figures are the sum of the pages listed below, so they fill in as soon as one page shows decay."
          />
        }
      >
        {() => (
          <Readout
            channels={[
              { key: "pages", label: "Pages decaying", value: num(totals.pages) },
              {
                key: "lost",
                label: "Readers lost",
                value: num(totals.lost),
                note: totals.before ? `${pct(totals.lost / totals.before)} of their readers` : undefined,
              },
              {
                key: "fetches",
                label: "Fetches on those pages",
                value: num(totals.fetches),
                /* Marked inverse deliberately: on this screen a rise in machine
                   fetches is the shape being reported, not a win. */
                delta: delta(totals.fetches, totals.fetchesBefore),
                inverse: true,
                note: "vs. previous period",
              },
              {
                key: "returned",
                label: "Returned as readers",
                value: num(totals.returned),
                note: "answer-engine arrivals",
              },
            ]}
          />
        )}
      </Reading>
    </Panel>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

export function DecayScreen() {
  const bench = useMachineEnv("all");
  const decay = useDecay(bench);

  return (
    <div className="mf-wedge">
      <Bench title="Content decay" bench={bench} />
      <DecayTotals q={decay} />
      <DecaySheet bench={bench} q={decay} />
    </div>
  );
}

export default DecayScreen;
