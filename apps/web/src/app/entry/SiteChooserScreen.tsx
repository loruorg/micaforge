/**
 * The site chooser: the first screen after signing in.
 *
 * It is not a list of names. Every site is a live strip of its own record: the
 * machine pen and the human pen writing right now, the two counts beside them,
 * and the Citation Gap for the last thirty days. Someone who has only ever seen
 * a pageview counter should understand the subject of this product before they
 * have clicked anything.
 *
 * Each row is three reads (the site itself, its live pen, its gap), so a slow
 * or failed sub-read degrades that one cell and never the page.
 */
import { useMemo, useState } from "react";
import { useNavigate } from "react-router";
import { useQueries, useQuery } from "@tanstack/react-query";
import type { Column } from "../../components/react/ui";
import { Button, Empty, ErrorState, Panel, PanelHead, Readout, Sheet } from "../../components/react/ui";
import { IconPlus, IconSearch } from "../../components/react/icons";
import type { GapSummary, Realtime, SiteSummary } from "../../lib/api";
import { api } from "../../lib/api";
import { num } from "../../lib/format";
import { gapRatio, ratioText } from "../display";
import { ENTRY_PATHS } from "./AuthFrame";
import "../../styles/app/entry.css";

/* ── the live two-pen strip ──────────────────────────────────────────────── */

const SPARK_W = 108;
const SPARK_H = 30;
const SPARK_MID = SPARK_H / 2;
const SPARK_AMP = SPARK_MID - 2;

/** A crawler fetches in discrete jumps, so its pen steps rather than curves. */
function steppedPath(vals: number[], max: number, dir: -1 | 1): string {
  if (vals.length < 2) return "";
  const x = (i: number) => (i / (vals.length - 1)) * SPARK_W;
  const y = (v: number) => SPARK_MID + dir * (max === 0 ? 0 : (v / max) * SPARK_AMP);
  let d = `M${x(0).toFixed(1)},${y(vals[0]).toFixed(1)}`;
  for (let i = 1; i < vals.length; i++) {
    const mid = ((x(i - 1) + x(i)) / 2).toFixed(1);
    d += `L${mid},${y(vals[i - 1]).toFixed(1)}L${mid},${y(vals[i]).toFixed(1)}L${x(i).toFixed(1)},${y(vals[i]).toFixed(1)}`;
  }
  return d;
}

/** A reader arrives continuously, so their pen is written rather than stepped. */
function writtenPath(vals: number[], max: number, dir: -1 | 1): string {
  if (vals.length < 2) return "";
  const x = (i: number) => (i / (vals.length - 1)) * SPARK_W;
  const y = (v: number) => SPARK_MID + dir * (max === 0 ? 0 : (v / max) * SPARK_AMP);
  const pts = vals.map((v, i) => [x(i), y(v)] as [number, number]);
  let d = `M${pts[0][0].toFixed(1)},${pts[0][1].toFixed(1)}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i === 0 ? 0 : i - 1];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    d += `C${(p1[0] + (p2[0] - p0[0]) / 6).toFixed(1)},${(p1[1] + (p2[1] - p0[1]) / 6).toFixed(1)} ${(p2[0] - (p3[0] - p1[0]) / 6).toFixed(1)},${(p2[1] - (p3[1] - p1[1]) / 6).toFixed(1)} ${p2[0].toFixed(1)},${p2[1].toFixed(1)}`;
  }
  return d;
}

/**
 * The machine pen writes up from the centre rule and the human pen writes down
 * from it, exactly as the full record does.
 *
 * Each pen is scaled to its own maximum: at 30 pixels tall a shared scale would
 * flatten the human trace to a straight line every time, which reads as "no
 * readers" rather than "far fewer readers". Magnitude is the Citation Gap
 * column's job; this strip carries shape and rhythm, and the label states both
 * counts for anyone who cannot see it.
 */
function MiniRecord({ humans, agents, label }: { humans: number[]; agents: number[]; label: string }) {
  const humanMax = Math.max(...humans, 0);
  const agentMax = Math.max(...agents, 0);
  // Two flat pens sitting on the zero rule read as a drawing fault rather than
  // as silence, so silence says so in words instead.
  if (humanMax === 0 && agentMax === 0) {
    return (
      <span className="mf-cell-quiet" title={label}>
        quiet
      </span>
    );
  }
  const agentLine = steppedPath(agents, agentMax, -1);
  const humanLine = writtenPath(humans, humanMax, 1);

  return (
    <svg
      className="mf-spark"
      width={SPARK_W}
      height={SPARK_H}
      viewBox={`0 0 ${SPARK_W} ${SPARK_H}`}
      role="img"
      aria-label={label}
      focusable="false"
    >
      <line
        x1={0}
        x2={SPARK_W}
        y1={SPARK_MID}
        y2={SPARK_MID}
        stroke="var(--mf-rule-major)"
        strokeWidth={1}
        shapeRendering="crispEdges"
      />
      {agentLine ? (
        <>
          <path d={agentLine} fill="none" stroke="var(--mf-well)" strokeWidth={2.4} transform="translate(0,1)" opacity={0.8} />
          <path d={agentLine} fill="none" stroke="var(--mf-trace-agent)" strokeWidth={1.4} strokeLinejoin="round" />
        </>
      ) : null}
      {humanLine ? (
        <>
          <path d={humanLine} fill="none" stroke="var(--mf-well)" strokeWidth={2.4} transform="translate(0,1)" opacity={0.8} />
          <path d={humanLine} fill="none" stroke="var(--mf-trace-human)" strokeWidth={1.4} strokeLinejoin="round" />
        </>
      ) : null}
    </svg>
  );
}

/* ── the gap, read as a ratio ────────────────────────────────────────────── */

type GapTone = "ratio" | "quiet" | "warn";

function gapReading(g: GapSummary | undefined): { text: string; title: string; tone: GapTone } {
  if (!g) return { text: "—", title: "No gap reading came back", tone: "quiet" };
  const taken = g.agent_fetches ?? 0;
  const back = g.answer_arrivals ?? 0;
  const title = `${num(taken)} agent fetches and ${num(back)} arrivals from answer engines, last 30 days`;
  if (taken === 0 && back === 0) return { text: gapRatio(taken, back), title, tone: "quiet" };
  if (back === 0) return { text: gapRatio(taken, back), title, tone: "warn" };
  const ratio = Number.isFinite(g.ratio) && g.ratio > 0 ? g.ratio : taken / back;
  return { text: ratioText(ratio), title, tone: "ratio" };
}

/* ── the screen ──────────────────────────────────────────────────────────── */

interface Row {
  site: SiteSummary;
  realtime?: Realtime;
  realtimePending: boolean;
  gap?: GapSummary;
  gapPending: boolean;
}

const RANGE = "30d";

export function SiteChooserScreen() {
  const navigate = useNavigate();
  const [term, setTerm] = useState("");

  const sitesQuery = useQuery({
    queryKey: ["sites"],
    queryFn: () => api.get<SiteSummary[]>("/api/sites").then((r) => r.data),
  });

  const sites = useMemo(() => sitesQuery.data ?? [], [sitesQuery.data]);

  const realtimeQueries = useQueries({
    queries: sites.map((s) => ({
      queryKey: ["stats", "realtime", s.site_id, "5m", "all", "[]"],
      queryFn: () =>
        api.get<Realtime>("/api/stats/realtime", { site: s.site_id, audience: "all" as const }).then((r) => r.data),
      refetchInterval: 15_000,
      staleTime: 10_000,
      retry: 1,
    })),
  });

  const gapQueries = useQueries({
    queries: sites.map((s) => ({
      queryKey: ["stats", "citation-gap/summary", s.site_id, RANGE, "all", "[]"],
      queryFn: () =>
        api
          .get<GapSummary>("/api/citation-gap/summary", { site: s.site_id, range: RANGE, audience: "all" as const })
          .then((r) => r.data),
      staleTime: 5 * 60 * 1000,
      retry: 1,
    })),
  });

  const rows: Row[] = sites.map((site, i) => ({
    site,
    realtime: realtimeQueries[i]?.data,
    realtimePending: realtimeQueries[i]?.isPending ?? true,
    gap: gapQueries[i]?.data,
    gapPending: gapQueries[i]?.isPending ?? true,
  }));

  const filtered = term.trim()
    ? rows.filter((r) => {
        const q = term.trim().toLowerCase();
        return r.site.domain.toLowerCase().includes(q) || r.site.name.toLowerCase().includes(q);
      })
    : rows;

  // The ink behind a site row is its share of the machine traffic across the
  // whole account, which is the comparison the chooser exists to make.
  const totalFetches = rows.reduce((a, r) => a + (r.gap?.agent_fetches ?? 0), 0);
  const liveHumans = rows.reduce((a, r) => a + (r.realtime?.visitors_now ?? 0), 0);
  const liveAgents = rows.reduce((a, r) => a + (r.realtime?.agents_now ?? 0), 0);
  const anyLive = rows.some((r) => r.realtime != null);
  const reporting = sites.filter((s) => s.has_data).length;
  // A list that never arrived is not a list of nothing: on a failed read the
  // strip reports that it has no reading, rather than a confident zero.
  const listed = sitesQuery.isSuccess;
  const knownZero = listed && sites.length === 0;

  const open = (row: Row) =>
    navigate(row.site.has_data ? ENTRY_PATHS.record(row.site.site_id) : ENTRY_PATHS.onboarding(row.site.site_id));

  const columns: Column<Row>[] = [
    {
      key: "site",
      head: "Site",
      share: (r) => (totalFetches > 0 ? (r.gap?.agent_fetches ?? 0) / totalFetches : undefined),
      render: (r) => (
        <span className="mf-site">
          <span className="mf-site__domain">{r.site.domain}</span>
          <span className="mf-site__name">{r.site.has_data ? r.site.name : "not reporting yet"}</span>
        </span>
      ),
    },
    {
      key: "record",
      head: "Live record",
      width: "15ch",
      render: (r) => {
        if (r.realtimePending) return <span className="mf-wait" style={{ width: "100%" }} aria-hidden="true" />;
        const per = r.realtime?.per_minute ?? [];
        if (!per.length)
          return (
            <span className="mf-cell-quiet" title="No hits in the last five minutes">
              quiet
            </span>
          );
        return (
          <MiniRecord
            humans={per.map((p) => p[1] ?? 0)}
            agents={per.map((p) => p[2] ?? 0)}
            label={`${r.site.domain}: ${num(r.realtime?.visitors_now ?? 0)} reading and ${num(
              r.realtime?.agents_now ?? 0,
            )} fetching in the last five minutes`}
          />
        );
      },
    },
    {
      key: "reading",
      head: "Reading",
      numeric: true,
      width: "11ch",
      render: (r) =>
        r.realtimePending ? (
          <span className="mf-wait" style={{ width: "60%" }} aria-hidden="true" />
        ) : (
          <span className="mf-data">{r.realtime ? num(r.realtime.visitors_now) : "—"}</span>
        ),
    },
    {
      key: "fetching",
      head: "Fetching",
      numeric: true,
      width: "11ch",
      render: (r) =>
        r.realtimePending ? (
          <span className="mf-wait" style={{ width: "60%" }} aria-hidden="true" />
        ) : (
          <span className="mf-data">{r.realtime ? num(r.realtime.agents_now) : "—"}</span>
        ),
    },
    {
      key: "gap",
      head: "Citation gap",
      numeric: true,
      width: "15ch",
      render: (r) => {
        if (r.gapPending) return <span className="mf-wait" style={{ width: "70%" }} aria-hidden="true" />;
        const g = gapReading(r.gap);
        return (
          <span
            className={g.tone === "ratio" ? "mf-data" : g.tone === "warn" ? "mf-cell-warn" : "mf-cell-quiet"}
            title={g.title}
          >
            {g.text}
          </span>
        );
      },
    },
  ];

  return (
    <div className="mf-stack">
      <div className="mf-chooser__mast">
        <div>
          <h1 className="mf-chooser__title">Your sites</h1>
          <p className="mf-chooser__sub">
            Two pens per site: what the machines fetched, what the readers read. The gap is how many
            agent fetches it took to bring one reader back from an answer engine.
          </p>
        </div>
        <div className="mf-chooser__tools">
          {sites.length > 6 ? (
            <span className="mf-search">
              <IconSearch size={14} className="mf-search__glyph" />
              <label className="mf-sr" htmlFor="mf-site-search">
                Filter sites by domain or name
              </label>
              <input
                id="mf-site-search"
                className="mf-input"
                type="search"
                placeholder="Filter sites"
                value={term}
                onChange={(e) => setTerm(e.target.value)}
              />
            </span>
          ) : null}
          <Button tone="primary" icon={<IconPlus size={14} />} onClick={() => navigate(ENTRY_PATHS.addSite)}>
            Add a site
          </Button>
        </div>
      </div>

      <Panel ruled>
        <PanelHead
          title="Writing now"
          note="The last five minutes, across every site you can read."
        />
        <Readout
          channels={[
            {
              key: "sites",
              label: "Sites",
              value: listed ? num(sites.length) : "—",
              note: listed ? `${num(reporting)} reporting` : "no reading",
            },
            {
              key: "reading",
              label: "Reading now",
              value: anyLive || knownZero ? num(liveHumans) : "—",
              note: "people in a browser",
            },
            {
              key: "fetching",
              label: "Fetching now",
              value: anyLive || knownZero ? num(liveAgents) : "—",
              note: "named AI agents",
            },
          ]}
        />
      </Panel>

      <Panel>
        <PanelHead
          title="Choose a site"
          note="Each row opens that site's record, and a site that has never reported opens its install steps instead. The ink behind a name is that site's share of the agent fetches across your whole account."
        />
        {sitesQuery.isError ? (
          <ErrorState
            title="The site list did not come back"
            detail={
              sitesQuery.error instanceof Error
                ? sitesQuery.error.message
                : "The server did not answer for your sites."
            }
            onRetry={() => sitesQuery.refetch()}
          />
        ) : (
          <Sheet
            rows={filtered}
            columns={columns}
            loading={sitesQuery.isPending}
            getKey={(r) => r.site.id}
            onRowClick={open}
            empty={
              term.trim() ? (
                <Empty
                  title="No site matches that"
                  body="Nothing in your list matches that filter. Clear it to see every site again."
                  action={<Button onClick={() => setTerm("")}>Clear the filter</Button>}
                />
              ) : (
                <Empty
                  title="No sites yet"
                  body="A site is one domain you record, and everything under it. Add the first one and Micaforge will walk you through installing the tracker and connecting your server log."
                  action={
                    <Button tone="primary" icon={<IconPlus size={14} />} onClick={() => navigate(ENTRY_PATHS.addSite)}>
                      Add the first site
                    </Button>
                  }
                />
              )
            }
          />
        )}
      </Panel>
    </div>
  );
}

export default SiteChooserScreen;
