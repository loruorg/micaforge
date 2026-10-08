/**
 * Realtime: the last five minutes.
 *
 * The same chart as the dashboard, at minute resolution and refreshed on a
 * timer. Both audiences show at once and neither is a filter of the other: the
 * count of people on the site and the count of AI crawler fetches are two live
 * series of the same site.
 *
 * The polling is deliberately visible. A dashboard that silently stops updating
 * when its tab goes to the background is lying to whoever comes back to it, so
 * the update state (live, paused by the user, paused in the background) is
 * printed above the chart, and the data catches up the moment the tab is
 * looked at again.
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
import { errorDetail, statsKey, toMs, useRecordSite, useRecordState } from "./TopSheets";
import "../../styles/app/record.css";

/** The refresh interval. The progress bar in the status line is driven by it. */
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

const pagesOpen = (n: number) => `${num(n)} ${n === 1 ? "page" : "pages"} open`;

/** Pause and resume, drawn here because the shared icon set has neither. */
function PauseMark({ paused }: { paused: boolean }) {
  return (
    <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true" focusable="false">
      {paused ? (
        <path d="M4 2.5v9l7.5-4.5z" fill="currentColor" />
      ) : (
        <path d="M4.5 2.5v9M9.5 2.5v9" stroke="currentColor" strokeWidth="1.8" fill="none" />
      )}
    </svg>
  );
}

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

  /* Coming back to the tab fetches fresh data at once rather than waiting
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
      label: "People",
      color: "var(--mf-people)",
      pen: "written",
      values: per.map((p) => p[1] ?? 0),
      format: (v) => num(v),
    },
    {
      id: "machines",
      label: "AI crawlers",
      color: "var(--mf-ai)",
      pen: "stepped",
      values: per.map((p) => p[2] ?? 0),
      format: (v) => num(v),
    },
  ];

  /** Read off the data: each series' own peak minute. */
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
      out.push({ at: readers.at, label: `People peak ${num(readers.best)}`, tone: "neutral" });
    }
    if (machines.at >= 0 && machines.best > 0) {
      out.push({ at: machines.at, label: `Crawler peak ${num(machines.best)}`, tone: "neutral" });
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
      head: "People",
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
      head: "Visitors",
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
      head: "Event or crawler",
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

  const penState = held ? "held" : hidden ? "paused" : "live";

  return (
    <div className="mfr-screen">
      <h1 className="mf-sr">Realtime{domain ? ` for ${domain}` : ""}</h1>

      {/* ── update state, and the button that pauses it ───────────────── */}
      <div className="mfr-bar">
        <div className="mfr-status">
          <span role="status">
            {penState === "live" ? (
              <LivePulse label={`Updates every ${POLL_MS / 1000} seconds`} />
            ) : penState === "held" ? (
              <span className="mfr-status__held">Paused. Updates are stopped.</span>
            ) : (
              <span className="mfr-status__held">
                Paused while this tab is in the background. Updates start again when you come back.
              </span>
            )}
          </span>
          {penState === "live" ? (
            <span
              className="mfr-advance"
              aria-hidden="true"
              style={{ "--mfr-advance-dur": `${POLL_MS}ms` } as CSSProperties}
            >
              <span className="mfr-advance__pen" key={rt.dataUpdatedAt} />
            </span>
          ) : null}
          <span>
            Updated {rt.dataUpdatedAt ? ago(rt.dataUpdatedAt) : "not yet"}
          </span>
        </div>

        <div className="mfr-bar__end">
          <Button
            size="sm"
            className="mfr-toggle"
            icon={<PauseMark paused={held} />}
            aria-pressed={held}
            onClick={() => setHeld((v) => !v)}
          >
            {held ? "Resume" : "Pause"}
          </Button>
        </div>
      </div>

      {/* ── the last five minutes ─────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Last five minutes"
          note="People and AI crawlers, per minute. Each series has its own scale, so a small series never looks flat. The counts below are the exact figures."
        />
        <div className="mfr-body">
          {rt.isError ? (
            <ErrorState
              title="Could not load realtime data"
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
                // Refreshed in place every poll: redrawing from the left each time
                // would be noise. The live marker still shows each new value.
                write={false}
                formatTime={(t) =>
                  new Date(t).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
                }
                emptyLabel={
                  waiting
                    ? "Loading…"
                    : "No visits in the last five minutes."
                }
              />

              <div className="mfr-live">
                <div className="mfr-live__ch">
                  <span className="mfr-live__label">
                    <span className="mfr-pen" data-pen="written" aria-hidden="true" />
                    People online
                  </span>
                  <span className="mfr-live__n">{waiting ? "—" : num(data?.visitors_now)}</span>
                  <span className="mfr-live__note">
                    {waiting ? "loading" : pagesOpen(data?.top_pages?.length ?? 0)}
                  </span>
                </div>
                <div className="mfr-live__ch">
                  <span className="mfr-live__label">
                    <span className="mfr-pen" data-pen="stepped" aria-hidden="true" />
                    AI crawlers now
                  </span>
                  <span className="mfr-live__n">{waiting ? "—" : num(data?.agents_now)}</span>
                  <span className="mfr-live__note">named AI crawlers</span>
                </div>
              </div>
            </>
          )}
        </div>
      </Panel>

      {/* ── what is open, and what sent it ────────────────────────────── */}
      <div className="mfr-now">
        <Panel>
          <PanelHead title="Pages people are on" />
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
                    body="A page shows here for five minutes after someone opens it."
                  />
                }
              />
            )}
          </div>
        </Panel>

        <Panel>
          <PanelHead title="Sources" />
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
                    body="AI answers show here beside search, social and direct visits."
                  />
                }
              />
            )}
          </div>
        </Panel>
      </div>

      {/* ── live event list ───────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Live events"
          note="Every event in the last five minutes, newest first. A stepped line marks an AI crawler; a smooth line marks a person."
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
                    title="No events yet"
                    body="Open the site in another tab and events show here as they happen."
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
