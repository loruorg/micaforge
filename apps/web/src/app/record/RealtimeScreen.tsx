/**
 * THE LIVE PEN: the last five minutes.
 *
 * The same instrument as the overview, wound down to minute resolution and left
 * running. Both pens write here at once and neither is a filter of the other:
 * the count of people reading and the count of machines fetching are two live
 * channels of the same station.
 *
 * The polling is deliberately visible. A dashboard that silently stops updating
 * when its tab goes to the background is lying to whoever comes back to it, so
 * the pen states (writing, held, paused) are printed next to the record, and
 * the reading catches up the moment the tab is looked at again.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { Realtime } from "../../lib/api";
import { ago, num, shortPath } from "../../lib/format";
import { agentName, titleCase } from "../display";
import { Drum } from "../../components/react/Drum";
import type { DrumAnnotation, DrumChannel } from "../../components/react/Drum";
import {
  Button,
  Empty,
  ErrorState,
  LivePulse,
  Panel,
  PanelHead,
  Sheet,
} from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconPen } from "../../components/react/icons";
import { errorDetail, statsKey, toMs, useRecordSite, useRecordState } from "./TopSheets";
import "../../styles/app/record.css";

/** The interval the pen advances on. The status track below is driven by it. */
const POLL_MS = 5000;

const KINDS: Record<string, string> = {
  pageview: "Pageview",
  custom: "Event",
  outbound: "Outbound link",
  file_download: "Download",
  form: "Form",
  error: "Error",
  web_vitals: "Web vitals",
  engagement: "Engagement",
};

const clock = (t: number) =>
  new Date(toMs(t)).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

type PageRow = { path: string; n: number };
type SourceRow = { source: string; n: number };
type FeedRow = Realtime["recent"][number];

export function RealtimeScreen() {
  const { filters } = useRecordState();
  const { siteId, domain, timezone } = useRecordSite();

  const [held, setHeld] = useState(false);
  const [hidden, setHidden] = useState(false);
  const wasHidden = useRef(false);

  useEffect(() => {
    const read = () => setHidden(document.hidden);
    read();
    document.addEventListener("visibilitychange", read);
    return () => document.removeEventListener("visibilitychange", read);
  }, []);

  const active = useMemo(() => filters ?? [], [filters]);
  const filtersJson = useMemo(() => JSON.stringify(active), [active]);
  const paused = hidden || held;

  const rt = useQuery({
    queryKey: statsKey("realtime", siteId, "5m", "all", filtersJson),
    queryFn: () =>
      api
        .get<Realtime>("/api/stats/realtime", {
          site: siteId ?? 0,
          audience: "all",
          ...(active.length ? { filters: active } : {}),
          ...(timezone ? { tz: timezone } : {}),
        })
        .then((r) => r.data),
    enabled: siteId != null,
    refetchInterval: paused ? false : POLL_MS,
    refetchIntervalInBackground: false,
    staleTime: 0,
  });

  /* Coming back to the tab takes a fresh reading at once rather than waiting
     out the rest of the interval. */
  const refetch = useRef(rt.refetch);
  refetch.current = rt.refetch;
  useEffect(() => {
    if (wasHidden.current && !hidden) void refetch.current();
    wasHidden.current = hidden;
  }, [hidden]);

  const data = rt.data;
  const waiting = siteId == null || rt.isPending;

  const per = useMemo(() => data?.per_minute ?? [], [data]);
  const times = useMemo(() => per.map((p) => toMs(p[0])), [per]);

  const channels: DrumChannel[] = [
    {
      id: "readers",
      label: "Reading",
      color: "var(--mf-trace-human)",
      pen: "written",
      values: per.map((p) => p[1] ?? 0),
      format: (v) => num(v),
    },
    {
      id: "machines",
      label: "Fetching",
      color: "var(--mf-trace-agent)",
      pen: "stepped",
      values: per.map((p) => p[2] ?? 0),
      format: (v) => num(v),
    },
  ];

  /** Read off the trace, never written into it: each pen's own peak minute. */
  const annotations = useMemo<DrumAnnotation[]>(() => {
    const out: DrumAnnotation[] = [];
    if (per.length < 3) return out;
    const peakOf = (col: 1 | 2) => {
      let best = -Infinity;
      let at = -1;
      for (let i = 0; i < per.length; i++) {
        const v = per[i][col] ?? 0;
        if (v > best) {
          best = v;
          at = i;
        }
      }
      return { at, best };
    };
    const readers = peakOf(1);
    const machines = peakOf(2);
    if (readers.at >= 0 && readers.best > 0) {
      out.push({ at: readers.at, label: `Reading peak ${num(readers.best)}`, tone: "neutral" });
    }
    if (machines.at >= 0 && machines.best > 0) {
      out.push({ at: machines.at, label: `Fetching peak ${num(machines.best)}`, tone: "neutral" });
    }
    return out;
  }, [per]);

  const pageCols: Column<PageRow>[] = [
    {
      key: "path",
      head: "Page",
      share: (r) => (data?.top_pages?.[0]?.n ? r.n / data.top_pages[0].n : undefined),
      render: (r) => (
        <span className="mf-data" title={r.path}>
          {shortPath(r.path, 34)}
        </span>
      ),
    },
    {
      key: "n",
      head: "Reading",
      numeric: true,
      width: "6rem",
      render: (r) => <span className="mf-data">{num(r.n)}</span>,
    },
  ];

  const sourceCols: Column<SourceRow>[] = [
    {
      key: "source",
      head: "Source",
      share: (r) => (data?.top_sources?.[0]?.n ? r.n / data.top_sources[0].n : undefined),
      render: (r) => (
        <span className="mfr-name" title={r.source}>
          {titleCase(r.source) || "Direct"}
        </span>
      ),
    },
    {
      key: "n",
      head: "Readers",
      numeric: true,
      width: "6rem",
      render: (r) => <span className="mf-data">{num(r.n)}</span>,
    },
  ];

  const feedCols: Column<FeedRow>[] = [
    {
      key: "t",
      head: "Time",
      width: "7rem",
      render: (r) => <span className="mf-data mfr-feed__when">{clock(r.t)}</span>,
    },
    {
      key: "what",
      head: "Channel",
      width: "12rem",
      render: (r) => (
        <>
          <span className="mfr-pen" data-pen={r.agent ? "stepped" : "written"} aria-hidden="true" />
          <span className="mfr-name">
            {r.agent ? agentName(null, r.agent) : (KINDS[r.kind] ?? r.kind)}
          </span>
        </>
      ),
    },
    {
      key: "path",
      head: "Page",
      render: (r) => (
        <span className="mf-data" title={r.path}>
          {shortPath(r.path, 40)}
        </span>
      ),
    },
    {
      key: "from",
      head: "From",
      width: "10rem",
      render: (r) =>
        r.agent ? (
          <span className="mf-data">{r.country?.trim() || "—"}</span>
        ) : (
          <span className="mfr-name">{titleCase(r.source) || "Direct"}</span>
        ),
    },
  ];

  const penState = held ? "held" : hidden ? "paused" : "writing";

  return (
    <div className="mfr-screen">
      <h1 className="mf-sr">Live pen{domain ? ` for ${domain}` : ""}</h1>

      {/* ── pen state, and the key that parks it ──────────────────────── */}
      <div className="mfr-bar">
        <div className="mfr-status">
          <span role="status">
            {penState === "writing" ? (
              <LivePulse label={`Writing every ${POLL_MS / 1000} seconds`} />
            ) : penState === "held" ? (
              <span className="mfr-status__held">
                Held. The pen is parked and nothing is being read.
              </span>
            ) : (
              <span className="mfr-status__held">
                Paused. This tab is in the background; the reading resumes when you come back.
              </span>
            )}
          </span>
          {penState === "writing" ? (
            <span
              className="mfr-advance"
              aria-hidden="true"
              style={{ "--mfr-advance-dur": `${POLL_MS}ms` } as CSSProperties}
            >
              <span className="mfr-advance__pen" key={rt.dataUpdatedAt} />
            </span>
          ) : null}
          <span>
            Last reading {rt.dataUpdatedAt ? ago(rt.dataUpdatedAt) : "not taken yet"}
          </span>
        </div>

        <div className="mfr-bar__end">
          <Button
            size="sm"
            className="mfr-toggle"
            icon={<IconPen size={14} />}
            aria-pressed={held}
            onClick={() => setHeld((v) => !v)}
          >
            {held ? "Release the pen" : "Hold the pen"}
          </Button>
        </div>
      </div>

      {/* ── the last five minutes ─────────────────────────────────────── */}
      <Panel feed>
        <PanelHead
          title="The last five minutes"
          note="Both pens at minute resolution. Each carries its own gain, so a flat pen never hides a small channel; the counts underneath are the figures."
        />
        <div className="mfr-body">
          {rt.isError ? (
            <ErrorState
              title="The live reading did not come back"
              detail={errorDetail(rt.error)}
              onRetry={() => void rt.refetch()}
            />
          ) : (
            <>
              <Drum
                times={times}
                channels={channels}
                mode="stacked"
                scale="per-channel"
                height={228}
                notchEvery={Math.max(1, Math.round(times.length / 6))}
                annotations={annotations}
                live={!paused}
                // Refreshed in place every poll: a full rewrite each time would be
                // noise. The live pen's datum tick still marks each new reading.
                write={false}
                formatTime={(t) =>
                  new Date(t).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
                }
                emptyLabel={
                  waiting
                    ? "Reading the record…"
                    : "Nothing has been recorded in the last five minutes."
                }
              />

              <div className="mfr-live">
                <div className="mfr-live__ch">
                  <span className="mfr-live__label">
                    <span className="mfr-pen" data-pen="written" aria-hidden="true" />
                    Reading now
                  </span>
                  <span className="mfr-live__n">{waiting ? "—" : num(data?.visitors_now)}</span>
                  <span className="mfr-live__note">
                    {waiting ? "taking the reading" : `${num(data?.top_pages?.length ?? 0)} pages open`}
                  </span>
                </div>
                <div className="mfr-live__ch">
                  <span className="mfr-live__label">
                    <span className="mfr-pen" data-pen="stepped" aria-hidden="true" />
                    Fetching now
                  </span>
                  <span className="mfr-live__n">{waiting ? "—" : num(data?.agents_now)}</span>
                  <span className="mfr-live__note">named machine clients</span>
                </div>
              </div>
            </>
          )}
        </div>
      </Panel>

      {/* ── what is open, and what sent it ────────────────────────────── */}
      <div className="mfr-now">
        <Panel>
          <PanelHead title="Pages being read" />
          <div className="mfr-body">
            {rt.isError ? (
              <ErrorState detail={errorDetail(rt.error)} onRetry={() => void rt.refetch()} />
            ) : (
              <Sheet<PageRow>
                rows={data?.top_pages ?? []}
                columns={pageCols}
                loading={waiting}
                getKey={(r, i) => `${r.path}-${i}`}
                empty={
                  <Empty
                    title="No page is open right now"
                    body="A page appears here for the five minutes after someone reads it."
                  />
                }
              />
            )}
          </div>
        </Panel>

        <Panel>
          <PanelHead title="Where they came from" />
          <div className="mfr-body">
            {rt.isError ? (
              <ErrorState detail={errorDetail(rt.error)} onRetry={() => void rt.refetch()} />
            ) : (
              <Sheet<SourceRow>
                rows={data?.top_sources ?? []}
                columns={sourceCols}
                loading={waiting}
                getKey={(r, i) => `${r.source}-${i}`}
                empty={
                  <Empty
                    title="No source in the last five minutes"
                    body="Answer engines are grouped here beside search, social and direct."
                  />
                }
              />
            )}
          </div>
        </Panel>
      </div>

      {/* ── the feed ──────────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="As it happens"
          note="Every hit in the last five minutes, newest first. A stepped mark is a machine; a written mark is a person."
        />
        <div className="mfr-body">
          {rt.isError ? (
            <ErrorState detail={errorDetail(rt.error)} onRetry={() => void rt.refetch()} />
          ) : (
            <div className="mfr-feed">
              <Sheet<FeedRow>
                rows={data?.recent ?? []}
                columns={feedCols}
                loading={waiting}
                getKey={(r, i) => `${r.t}-${r.path}-${i}`}
                empty={
                  <Empty
                    title="Nothing has come in yet"
                    body="Open the site in another tab and the feed writes as it happens."
                  />
                }
              />
            </div>
          )}
        </div>
      </Panel>
    </div>
  );
}

export default RealtimeScreen;
