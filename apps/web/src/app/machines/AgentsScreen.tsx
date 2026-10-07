/**
 * Agent traffic, at /app/:site/agents
 *
 * The screen that says who the machines are, what they took, and what they have
 * never bothered to read. There is no audience switch here: this screen IS the
 * machines. The one editorial decision worth defending is the coverage panel,
 * which lists pages with NO crawl at all rather than filtering them away. The
 * absence is the finding.
 */
import { useMemo, useState } from "react";
import { useQuery, keepPreviousData } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { AgentOverview, CoverageRow, Filter } from "../../lib/api";
import { Button, Empty, Panel, PanelHead, Readout, Sheet, VerifiedMark } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { Drum } from "../../components/react/Drum";
import type { DrumChannel } from "../../components/react/Drum";
import { IconCheck, IconClose } from "../../components/react/icons";
import { ago, bytes, compact, num, pct, purposeLabel, shortPath } from "../../lib/format";
import { agentName, titleCase } from "../display";
import {
  AgentDetail,
  Bench,
  FreshnessCell,
  Reading,
  VerificationCaveat,
  Waiting,
  robotsState,
  robotsWord,
  sorted,
  useMachineEnv,
  useSort,
} from "./AgentDetail";
import type { AgentBreakdownRow, AgentIdentity, AgentSeriesResponse, MachineEnv } from "./AgentDetail";

/* ── the pivot ──────────────────────────────────────────────────────────── */

type Pivot = "agent_id" | "operator" | "purpose" | "pathname" | "status";

const PIVOTS: { v: Pivot; label: string }[] = [
  { v: "agent_id", label: "Agent" },
  { v: "operator", label: "Operator" },
  { v: "purpose", label: "Collecting for" },
  { v: "pathname", label: "Path" },
  { v: "status", label: "Status" },
];

/** The five series colours are a lightness ramp, so the bands survive greyscale. */
const RAMP = ["var(--mf-s1)", "var(--mf-s2)", "var(--mf-s3)", "var(--mf-s4)", "var(--mf-s5)"];
const MAX_TRACES = 5;

/* ── the record ─────────────────────────────────────────────────────────── */

function CrawlRecord({ bench }: { bench: MachineEnv }) {
  /* Per-channel gain is the default because five of the six bands are usually
     an order of magnitude apart, and a pen pinned flat on its baseline records
     nothing at all. Each band prints its own full scale in the readout, and one
     shared gain is one key away. */
  const [gain, setGain] = useState<"shared" | "per-channel">("per-channel");

  const q = useQuery({
    queryKey: bench.key("agents/timeseries", "operator"),
    queryFn: async () =>
      (await api.get<AgentSeriesResponse>("/api/agents/timeseries", { ...bench.env, dimension: "operator" })).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });

  const folded = useMemo(() => {
    const d = q.data;
    if (!d?.buckets?.length) return null;
    const series = d.series ?? [];
    const ranked = series
      .map((s) => ({ s, total: s.values.reduce((a, b) => a + (b ?? 0), 0) }))
      .sort((a, b) => b.total - a.total);
    const top = ranked.slice(0, MAX_TRACES);
    const rest = ranked.slice(MAX_TRACES);
    const other = rest.length
      ? d.buckets.map((_, i) => rest.reduce((a, r) => a + (r.s.values[i] ?? 0), 0))
      : null;
    const perBucket = d.buckets.map((_, i) => series.reduce((a, s) => a + (s.values[i] ?? 0), 0));
    let peak = 0;
    let low = 0;
    let surge = 0;
    let surgeBy = 0;
    for (let i = 1; i < perBucket.length; i++) {
      if (perBucket[i] > perBucket[peak]) peak = i;
      if (perBucket[i] < perBucket[low]) low = i;
      const rise = perBucket[i] - perBucket[i - 1];
      if (rise > surgeBy) {
        surgeBy = rise;
        surge = i;
      }
    }
    return { top, other, foldedCount: rest.length, perBucket, peak, low, surge, surgeBy };
  }, [q.data]);

  const channels: DrumChannel[] = useMemo(() => {
    if (!folded) return [];
    const list: DrumChannel[] = folded.top.map((r, i) => ({
      id: r.s.key,
      label: r.s.label ?? r.s.key,
      color: RAMP[i] ?? RAMP[RAMP.length - 1],
      pen: "stepped",
      values: r.s.values,
      format: (n) => num(n),
    }));
    if (folded.other) {
      list.push({
        id: "other",
        label: `Other operators (${folded.foldedCount})`,
        color: "var(--mf-ink-3)",
        pen: "stepped",
        values: folded.other,
        format: (n) => num(n),
      });
    }
    return list;
  }, [folded]);

  const foldNote =
    folded && folded.foldedCount > 0
      ? `The five heaviest operators are drawn; the remaining ${folded.foldedCount} ${folded.foldedCount === 1 ? "is" : "are"} folded into one "Other" channel.`
      : "One stepped trace per operator: a crawler fetches in discrete jumps, so its pen steps rather than curves.";

  return (
    <Panel ruled feed>
      <PanelHead
        title="Crawl record"
        note={
          gain === "shared"
            ? `${foldNote} Every channel is on one gain, so the bands compare directly and a quiet operator reads as a quiet band.`
            : `${foldNote} Each channel is on its own gain, printed beside it, so a quiet operator's shape stays legible, but the band heights no longer compare.`
        }
        actions={
          <div className="mf-pivot" role="group" aria-label="Gain">
            <button
              type="button"
              className="mf-pivot__key"
              aria-pressed={gain === "shared"}
              onClick={() => setGain("shared")}
              title="Every operator on one scale. Band heights compare directly."
            >
              One gain
            </button>
            <button
              type="button"
              className="mf-pivot__key"
              aria-pressed={gain === "per-channel"}
              onClick={() => setGain("per-channel")}
              title="Each operator scaled to its own maximum, with that maximum printed beside it."
            >
              Own gain
            </button>
          </div>
        }
      />
      <div className="mf-wedge__plot">
        <Reading
          query={q}
          waiting={<Waiting rows={5} plot />}
          isEmpty={(d) => !d?.buckets?.length || !(d.series ?? []).some((s) => s.values.some((v) => v))}
          empty={
            <Empty
              title="No machine has fetched this site in this interval"
              body="Agent traffic is written by the server-side ingest path: /api/log/edge, the log shipper or the reverse-proxy module. A browser tag alone cannot see a crawler, because a crawler does not run JavaScript."
            />
          }
        >
          {(d) => (
            <Drum
              times={d.buckets}
              channels={channels}
              mode="stacked"
              scale={gain}
              height={420}
              live
              /* Every annotation is read off the record itself, so none of them
                 can say something the data does not. */
              annotations={
                folded && folded.perBucket[folded.peak] > 0
                  ? [
                      { at: folded.peak, label: `Peak ${compact(folded.perBucket[folded.peak])} fetches` },
                      ...(folded.surgeBy > 0
                        ? [
                            {
                              at: folded.surge,
                              label: `Surge +${compact(folded.surgeBy)}`,
                              tone: "warn" as const,
                            },
                          ]
                        : []),
                      ...(folded.low !== folded.peak
                        ? [{ at: folded.low, label: `Quietest ${compact(folded.perBucket[folded.low])}` }]
                        : []),
                    ]
                  : []
              }
              emptyLabel="No crawl recorded in this interval."
            />
          )}
        </Reading>
      </div>
    </Panel>
  );
}

/* ── the totals bank ────────────────────────────────────────────────────── */

function TotalsBank({ bench }: { bench: MachineEnv }) {
  const q = useQuery({
    queryKey: bench.key("agents/overview"),
    queryFn: async () => (await api.get<AgentOverview>("/api/agents/overview", bench.env)).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });

  return (
    <Panel className="mf-panel--bank">
      <Reading
        query={q}
        waiting={<Waiting rows={2} />}
        isEmpty={(d) => !d || (d.fetches === 0 && d.unique_agents === 0)}
        empty={
          <Empty
            title="Nothing measured for the machines yet"
            body="These figures fill in as soon as the server-side ingest path reports its first fetch from a named agent."
          />
        }
      >
        {(d) => (
          <Readout
            channels={[
              {
                key: "fetches",
                label: "Fetches",
                value: num(d.fetches),
                delta: d.delta_pct == null ? null : d.delta_pct / 100,
                // No comparison, no caption: a note promising a delta that never
                // arrives is a caption for an absence.
                note: d.delta_pct == null ? undefined : "vs. previous period",
              },
              { key: "agents", label: "Agents seen", value: num(d.unique_agents) },
              {
                key: "verified",
                label: "Verified share",
                value: pct(d.verified_pct / 100),
                note: "address checked",
              },
              { key: "bytes", label: "Bytes served", value: bytes(d.bytes) },
              { key: "paths", label: "Paths reached", value: num(d.unique_paths) },
            ]}
          />
        )}
      </Reading>
    </Panel>
  );
}

/* ── the tabulation ─────────────────────────────────────────────────────── */

function AgentSheet({
  bench,
  onOpen,
}: {
  bench: MachineEnv;
  onOpen: (a: AgentIdentity) => void;
}) {
  const [pivot, setPivot] = useState<Pivot>("agent_id");

  const q = useQuery({
    queryKey: bench.key("agents/breakdown", pivot),
    queryFn: async () =>
      (await api.get<AgentBreakdownRow[]>("/api/agents/breakdown", { ...bench.env, dimension: pivot })).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });

  const filterTo = (dimension: string, value: string) =>
    bench.addFilter({ dimension, op: "is", value } as Filter);

  const columns = useMemo<Column<AgentBreakdownRow>[]>(() => {
    const fetches: Column<AgentBreakdownRow> = {
      key: "fetches",
      head: "Fetches",
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.fetches)}</span>,
    };
    const paths: Column<AgentBreakdownRow> = {
      key: "paths",
      head: "Paths",
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.unique_paths ?? 0)}</span>,
    };
    const served: Column<AgentBreakdownRow> = {
      key: "bytes",
      head: "Served",
      numeric: true,
      render: (r) => <span className="mf-data">{bytes(r.bytes ?? 0)}</span>,
    };
    const seen: Column<AgentBreakdownRow> = {
      key: "seen",
      head: "Last seen",
      render: (r) => <span className="mf-dim">{r.last_seen ? ago(r.last_seen) : "never"}</span>,
    };

    if (pivot === "agent_id") {
      return [
        {
          key: "agent",
          head: "Agent",
          width: "26%",
          render: (r) => (
            <span className="mf-wedge-stack">
              <span>{agentName(r.label, r.key)}</span>
              <span className="mf-wedge-stack__sub">
                {r.operator_label ?? (r.operator ? titleCase(r.operator) : "operator not in the catalog")}
              </span>
            </span>
          ),
          share: (r) => r.share,
        },
        {
          key: "purpose",
          head: "Collecting for",
          render: (r) => <span>{r.purpose_label ?? purposeLabel(r.purpose ?? "unknown")}</span>,
        },
        fetches,
        paths,
        {
          key: "verified",
          head: "Identity",
          render: (r) => <VerifiedMark state={r.verified ?? "unverified"} />,
        },
        {
          key: "robots",
          head: "Your robots.txt",
          render: (r) => {
            const s = robotsState(r.robots_allowed);
            return (
              <span className="mf-flag" data-tone={s === "allowed" ? "allow" : s === "disallowed" ? "deny" : "none"}>
                {s === "allowed" ? <IconCheck size={12} /> : s === "disallowed" ? <IconClose size={12} /> : null}
                {robotsWord(s)}
              </span>
            );
          },
        },
        seen,
      ];
    }

    if (pivot === "operator") {
      return [
        {
          key: "operator",
          head: "Operator",
          width: "34%",
          render: (r) => (
            <button type="button" className="mf-cellkey" onClick={() => filterTo("operator", r.key)}>
              {r.label || titleCase(r.key)}
            </button>
          ),
          share: (r) => r.share,
        },
        fetches,
        paths,
        served,
        seen,
      ];
    }

    if (pivot === "purpose") {
      return [
        {
          key: "purpose",
          head: "Collecting for",
          width: "34%",
          render: (r) => (
            <button type="button" className="mf-cellkey" onClick={() => filterTo("purpose", r.key)}>
              {purposeLabel(r.label || r.key)}
            </button>
          ),
          share: (r) => r.share,
        },
        fetches,
        paths,
        served,
        { key: "share", head: "Share", numeric: true, render: (r) => <span className="mf-data">{pct(r.share ?? 0)}</span> },
      ];
    }

    if (pivot === "pathname") {
      return [
        {
          key: "path",
          head: "Path",
          width: "42%",
          render: (r) => (
            <button
              type="button"
              className="mf-cellkey mf-path"
              title={r.label || r.key}
              onClick={() => filterTo("pathname", r.key)}
            >
              {shortPath(r.label || r.key, 48)}
            </button>
          ),
          share: (r) => r.share,
        },
        fetches,
        served,
        seen,
      ];
    }

    return [
      {
        key: "status",
        head: "Status",
        width: "20%",
        render: (r) => (
          <button type="button" className="mf-cellkey mf-data" onClick={() => filterTo("status", r.key)}>
            {r.label || r.key}
          </button>
        ),
        share: (r) => r.share,
      },
      fetches,
      served,
      { key: "share", head: "Share", numeric: true, render: (r) => <span className="mf-data">{pct(r.share ?? 0)}</span> },
    ];
    // filterTo is stable enough for this memo: it only reads bench.addFilter.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pivot, bench.addFilter]);

  return (
    <Panel>
      <PanelHead
        title="Who fetched"
        note="Tabulate the same interval by a different channel. Selecting a row on any pivot but Agent reads the whole screen through that value."
        actions={
          <div className="mf-pivot" role="group" aria-label="Tabulate by">
            {PIVOTS.map((p) => (
              <button
                key={p.v}
                type="button"
                className="mf-pivot__key"
                aria-pressed={pivot === p.v}
                onClick={() => setPivot(p.v)}
              >
                {p.label}
              </button>
            ))}
          </div>
        }
      />
      <Reading
        query={q}
        waiting={<Waiting rows={6} />}
        isEmpty={(d) => !d?.length}
        empty={
          <Empty
            title="Nothing tabulated for this pivot"
            body="No fetch in this interval carried that field. Widen the interval, or clear a filter."
          />
        }
      >
        {(d) => (
          <Sheet
            rows={d}
            columns={columns}
            getKey={(r) => r.key}
            onRowClick={
              pivot === "agent_id"
                ? (r) =>
                    onOpen({
                      agent_id: r.key,
                      label: agentName(r.label, r.key),
                      operator:
                        r.operator_label ??
                        (r.operator ? titleCase(r.operator) : "Operator not in the catalog"),
                      purpose: r.purpose ?? "unknown",
                      verified: r.verified ?? "unverified",
                      robots_allowed: r.robots_allowed,
                      fetches: r.fetches,
                      unique_paths: r.unique_paths,
                      bytes: r.bytes,
                      last_seen: r.last_seen,
                    })
                : undefined
            }
          />
        )}
      </Reading>
      <div className="mf-wedge__body">
        <VerificationCaveat />
      </div>
    </Panel>
  );
}

/* ── coverage ───────────────────────────────────────────────────────────── */

type CoverageView = "all" | "never" | "unlisted";

function CoveragePanel({ bench }: { bench: MachineEnv }) {
  const [view, setView] = useState<CoverageView>("all");
  const sort = useSort<"path" | "first" | "last" | "gap" | "fetches">("gap");

  const q = useQuery({
    queryKey: bench.key("agents/coverage"),
    queryFn: async () =>
      (await api.get<CoverageRow[]>("/api/agents/coverage", { ...bench.env, limit: 200 })).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });

  const all = q.data ?? [];
  const neverCount = all.filter((r) => r.freshness_gap_hours == null && !r.last_crawl).length;
  const unlistedCount = all.filter((r) => !r.in_llms_txt).length;

  const rows = useMemo(() => {
    const base =
      view === "never"
        ? all.filter((r) => r.freshness_gap_hours == null && !r.last_crawl)
        : view === "unlisted"
          ? all.filter((r) => !r.in_llms_txt)
          : all;
    return sorted(
      base,
      (r) =>
        sort.by === "path"
          ? r.path
          : sort.by === "first"
            ? (r.first_crawl ?? 0)
            : sort.by === "last"
              ? (r.last_crawl ?? 0)
              : sort.by === "fetches"
                ? r.fetch_count
                : // A page never crawled has the largest gap there is, not a missing one.
                  (r.freshness_gap_hours ?? Number.MAX_SAFE_INTEGER),
      sort.dir,
    );
  }, [all, view, sort.by, sort.dir]);

  const columns: Column<CoverageRow>[] = [
    {
      key: "path",
      head: sort.head("path", "Page"),
      width: "34%",
      render: (r) => (
        <span className="mf-wedge-stack">
          <span className="mf-path" title={r.path}>
            {shortPath(r.path, 44)}
          </span>
          {r.title ? <span className="mf-wedge-stack__sub">{r.title}</span> : null}
        </span>
      ),
    },
    {
      key: "first",
      head: sort.head("first", "First crawl"),
      render: (r) => <span className="mf-dim">{r.first_crawl ? ago(r.first_crawl) : "never"}</span>,
    },
    {
      key: "last",
      head: sort.head("last", "Last crawl"),
      render: (r) => <span className="mf-dim">{r.last_crawl ? ago(r.last_crawl) : "never"}</span>,
    },
    {
      key: "gap",
      head: sort.head("gap", "Freshness gap"),
      render: (r) => <FreshnessCell hours={r.last_crawl ? r.freshness_gap_hours : null} />,
    },
    {
      key: "fetches",
      head: sort.head("fetches", "Fetches"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.fetch_count)}</span>,
    },
    {
      key: "agents",
      head: "Fetched by",
      render: (r) =>
        r.agents?.length ? (
          <span title={r.agents.join(", ")}>
            {r.agents.length === 1 ? r.agents[0] : `${r.agents[0]} +${r.agents.length - 1}`}
          </span>
        ) : (
          <span className="mf-dim">nobody</span>
        ),
    },
    {
      key: "listed",
      head: "Sitemap · llms.txt",
      render: (r) => (
        <span>
          <span className={r.in_sitemap ? undefined : "mf-dim"}>{r.in_sitemap ? "in sitemap" : "not in sitemap"}</span>
          <span className="mf-dim"> · </span>
          <span className={r.in_llms_txt ? undefined : "mf-dim"}>{r.in_llms_txt ? "in llms.txt" : "not listed"}</span>
        </span>
      ),
    },
  ];

  const views: { v: CoverageView; label: string }[] = [
    { v: "all", label: `Every page (${num(all.length)})` },
    { v: "never", label: `Never crawled (${num(neverCount)})` },
    { v: "unlisted", label: `Missing from llms.txt (${num(unlistedCount)})` },
  ];

  return (
    <Panel>
      <PanelHead
        title="Crawl coverage"
        note="Every page in the content register, including the ones no machine has ever fetched. A page nothing has read is a finding, so it is listed rather than filtered away."
        actions={
          <div className="mf-pivot" role="group" aria-label="Coverage view">
            {views.map((v) => (
              <button
                key={v.v}
                type="button"
                className="mf-pivot__key"
                aria-pressed={view === v.v}
                onClick={() => setView(v.v)}
                disabled={!q.data}
              >
                {v.label}
              </button>
            ))}
          </div>
        }
      />
      <Reading
        query={q}
        waiting={<Waiting rows={6} />}
        isEmpty={(d) => !d?.length}
        empty={
          <Empty
            title="The content register is empty"
            body="Coverage joins crawl history against the page register the crawler job builds from your sitemap. Once that job has run, every page appears here whether or not a machine has read it."
          />
        }
      >
        {() =>
          rows.length === 0 ? (
            <Empty
              title="No page matches that view"
              body={
                view === "never"
                  ? "Every page in the register has been crawled at least once."
                  : "Every page in the register is listed in llms.txt."
              }
              action={<Button onClick={() => setView("all")}>Show every page</Button>}
            />
          ) : (
            <Sheet rows={rows} columns={columns} getKey={(r) => r.path} />
          )
        }
      </Reading>
    </Panel>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

export function AgentsScreen() {
  const bench = useMachineEnv("agent");
  const [open, setOpen] = useState<AgentIdentity | null>(null);

  return (
    <div className="mf-wedge">
      <Bench title="Agent traffic" bench={bench} />
      <TotalsBank bench={bench} />
      <CrawlRecord bench={bench} />
      <AgentSheet bench={bench} onOpen={setOpen} />
      <CoveragePanel bench={bench} />
      {open ? (
        <AgentDetail
          agent={open}
          bench={bench}
          onClose={() => setOpen(null)}
          onFilterToAgent={(id) => bench.addFilter({ dimension: "agent_id", op: "is", value: id })}
        />
      ) : null}
    </div>
  );
}

export default AgentsScreen;
