/**
 * UPTIME, at /app/:site/uptime
 *
 * The one question: are this site's addresses answering, and have they been?
 *
 * A monitor is a URL this server requests on a schedule. The screen reads top
 * to bottom in the order a person checks after an alert:
 *
 *   1. The strip: is anything down right now, how has the site done over a
 *      day, a week and a month, and is an incident still open.
 *   2. The monitor list: which address, and how each one has done.
 *   3. One monitor in detail (the selected row): response times, failures and
 *      the last fifty checks. Selection lives in the address (`?monitor=`), so
 *      a link in an alert email or a chat message lands on the right monitor.
 *   4. Incidents: when each outage started, whether it has ended, and why.
 *
 * The site-wide uptime figures are averages across monitors, computed here,
 * because the summary endpoint reports each monitor separately. The strip says
 * "average of N monitors" under them so nobody reads a computed figure as a
 * measured one.
 *
 * Every read refreshes each minute. Monitors check every few minutes, and an
 * outage screen that shows the state from a quarter of an hour ago is wrong in
 * exactly the moment it is read.
 *
 * Nothing here sends a request to the monitored addresses until a monitor
 * exists. The empty state says so, because "add a monitor" on a privacy-bound
 * product deserves a plain statement of what it will start doing.
 */
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconPlus } from "../../components/react/icons";
import { StateShape } from "../../components/status/shapes";
import { uptimeText, when } from "../../components/status/words";
import { api } from "../../lib/api";
import { ms, num } from "../../lib/format";
import { useSite } from "../providers";
import { MonitorDetail } from "./MonitorDetail";
import { MonitorEditor } from "./MonitorEditor";
import { StateMark, Strip, StripWaiting } from "./parts";
import type { StripItem } from "./parts";
import type { Incident, Monitor, MonitorSummary, UptimeSummary } from "./types";
import { LIVE_MS, daysText, everyText, keys, problem, spanText } from "./types";

/** A monitor as the list draws it: its settings and its figures, side by side. */
interface Row {
  monitor: Monitor;
  figures: MonitorSummary | undefined;
}

/* ── the site-wide strip ─────────────────────────────────────────────────── */

const mean = (xs: (number | null | undefined)[]): { value: number | null; n: number } => {
  const got = xs.filter((x): x is number => x != null && Number.isFinite(x));
  return { value: got.length ? got.reduce((a, b) => a + b, 0) / got.length : null, n: got.length };
};

function siteStrip(rows: Row[], openIncidents: number): StripItem[] {
  const live = rows.filter((r) => r.monitor.enabled);
  const down = live.filter((r) => r.monitor.state === "down").length;
  const up = live.filter((r) => r.monitor.state === "up").length;
  const paused = rows.length - live.length;

  let now: StripItem["value"];
  if (!live.length) now = <StateMark state="unknown" enabled={false} big label="All paused" />;
  else if (down) now = <StateMark state="down" big label={`${down} down`} />;
  else if (up === live.length) now = <StateMark state="up" big label="All up" />;
  else if (up === 0) now = <StateMark state="unknown" big />;
  else now = <StateMark state="up" big label={`${up} up`} />;

  const items: StripItem[] = [
    {
      key: "now",
      label: "Now",
      value: now,
      note: `${num(up)} of ${num(live.length)} running ${live.length === 1 ? "monitor is" : "monitors are"} up${paused ? `, ${num(paused)} paused` : ""}`,
    },
  ];

  const windows: [string, string, (f: MonitorSummary) => number | null][] = [
    ["24h", "Uptime, 24 hours", (f) => f.uptime_24h],
    ["7d", "Uptime, 7 days", (f) => f.uptime_7d],
    ["30d", "Uptime, 30 days", (f) => f.uptime_30d],
  ];
  for (const [key, label, pick] of windows) {
    const m = mean(rows.map((r) => (r.figures ? pick(r.figures) : null)));
    items.push({
      key,
      label,
      value: uptimeText(m.value),
      note: m.n === 0 ? "no checks in this window" : m.n === 1 ? "one monitor" : `average of ${m.n} monitors`,
    });
  }

  const timed = rows.filter((r) => r.figures?.last_latency_ms != null);
  if (timed.length) {
    const slowest = timed.reduce((a, b) =>
      (b.figures?.last_latency_ms ?? 0) > (a.figures?.last_latency_ms ?? 0) ? b : a,
    );
    items.push({
      key: "latency",
      label: timed.length === 1 ? "Last response time" : "Slowest last response",
      value: ms(slowest.figures?.last_latency_ms),
      note: slowest.monitor.name,
    });
  }

  const certs = rows.filter((r) => r.figures?.ssl_days_left != null);
  if (certs.length) {
    const soonest = certs.reduce((a, b) =>
      (b.figures?.ssl_days_left ?? Infinity) < (a.figures?.ssl_days_left ?? Infinity) ? b : a,
    );
    items.push({
      key: "ssl",
      label: "Next certificate expiry",
      value: daysText(soonest.figures?.ssl_days_left),
      note: soonest.monitor.name,
    });
  }

  items.push({
    key: "open",
    label: "Open incidents",
    value: num(openIncidents),
    note: openIncidents ? "listed under Incidents" : "none open",
  });
  return items;
}

/* ── the tables ──────────────────────────────────────────────────────────── */

function monitorColumns(selected: string | undefined): Column<Row>[] {
  return [
    {
      key: "state",
      head: "State",
      render: (r) => <StateMark state={r.monitor.state} enabled={r.monitor.enabled} />,
    },
    {
      key: "name",
      head: "Name",
      render: (r) => (
        <span className="mf-mon-name" data-selected={r.monitor.id === selected ? "true" : undefined}>
          {r.monitor.name}
          {r.monitor.id === selected ? <span className="mf-sr"> (shown below)</span> : null}
        </span>
      ),
    },
    {
      key: "url",
      head: "Address",
      render: (r) => (
        <span className="mf-data mf-mon-url" title={r.monitor.url}>
          {r.monitor.url.replace(/^https?:\/\//, "")}
        </span>
      ),
    },
    {
      key: "interval",
      head: "Interval",
      numeric: true,
      render: (r) => (
        <span className="mf-data" title={everyText(r.monitor.interval_minutes)}>
          {r.monitor.interval_minutes} min
        </span>
      ),
    },
    {
      key: "24h",
      head: "24 hours",
      numeric: true,
      render: (r) => <span className="mf-data">{uptimeText(r.figures?.uptime_24h)}</span>,
    },
    {
      key: "7d",
      head: "7 days",
      numeric: true,
      render: (r) => <span className="mf-data">{uptimeText(r.figures?.uptime_7d)}</span>,
    },
    {
      key: "30d",
      head: "30 days",
      numeric: true,
      render: (r) => <span className="mf-data">{uptimeText(r.figures?.uptime_30d)}</span>,
    },
    {
      key: "latency",
      head: "Last response",
      numeric: true,
      render: (r) => <span className="mf-data">{ms(r.figures?.last_latency_ms)}</span>,
    },
  ];
}

const INCIDENT_COLUMNS: Column<Incident>[] = [
  {
    key: "state",
    head: "State",
    render: (i) =>
      i.resolved_at ? (
        <span className="mf-mon-state" data-state="up">
          <StateShape shape="up" size={11} />
          Resolved
        </span>
      ) : (
        <span className="mf-mon-state" data-state="down">
          <StateShape shape="down" size={11} />
          Ongoing
        </span>
      ),
  },
  { key: "monitor", head: "Monitor", render: (i) => <span>{i.monitor_name}</span> },
  { key: "started", head: "Started", render: (i) => <span className="mf-data">{when(i.started_at)}</span> },
  {
    key: "resolved",
    head: "Resolved",
    render: (i) => <span className="mf-data">{i.resolved_at ? when(i.resolved_at) : "Ongoing"}</span>,
  },
  {
    key: "duration",
    head: "Duration",
    numeric: true,
    render: (i) => <span className="mf-data">{spanText(i.duration_secs)}</span>,
  },
  {
    key: "failed",
    head: "Failed checks",
    numeric: true,
    render: (i) => <span className="mf-data">{num(i.failed_checks)}</span>,
  },
  {
    key: "cause",
    head: "Cause",
    render: (i) => (
      <span className="mf-mon-wrap" title={i.cause}>
        {i.cause}
      </span>
    ),
  },
];

/* ── the screen ──────────────────────────────────────────────────────────── */

export function UptimeScreen() {
  const { siteId } = useSite();
  const [params, setParams] = useSearchParams();
  /** "new" while adding a monitor, a monitor id while editing one. */
  const [editing, setEditing] = useState<string | null>(null);
  const ready = siteId != null;

  const monitors = useQuery({
    queryKey: keys.monitors(siteId),
    queryFn: () => api.get<Monitor[]>("/api/uptime/monitors", { site: siteId as number }).then((r) => r.data),
    enabled: ready,
    refetchInterval: LIVE_MS,
  });

  const summary = useQuery({
    queryKey: keys.summary(siteId),
    queryFn: () => api.get<UptimeSummary>("/api/uptime/summary", { site: siteId as number }).then((r) => r.data),
    enabled: ready,
    refetchInterval: LIVE_MS,
  });

  const incidents = useQuery({
    queryKey: keys.incidents(siteId),
    queryFn: () =>
      api.get<Incident[]>("/api/uptime/incidents", { site: siteId as number, limit: 50 }).then((r) => r.data),
    enabled: ready,
    refetchInterval: LIVE_MS,
  });

  const rows: Row[] = useMemo(() => {
    const figures = new Map((summary.data?.monitors ?? []).map((m) => [m.id, m]));
    return (monitors.data ?? []).map((m) => ({ monitor: m, figures: figures.get(m.id) }));
  }, [monitors.data, summary.data]);

  const wanted = params.get("monitor");
  const current = rows.find((r) => r.monitor.id === wanted) ?? rows[0];

  const select = (id: string | null) =>
    setParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (id) next.set("monitor", id);
        else next.delete("monitor");
        return next;
      },
      { replace: true },
    );

  const openFor = (id: string) => (incidents.data ?? []).filter((i) => i.monitor_id === id && !i.resolved_at).length;
  const openIncidents = summary.data?.open_incidents ?? (incidents.data ?? []).filter((i) => !i.resolved_at).length;

  const loading = monitors.isPending || summary.isPending;
  const none = monitors.isSuccess && rows.length === 0;

  const finish = (saved?: Monitor) => {
    setEditing(null);
    if (saved) select(saved.id);
  };

  const addButton = (
    <Button tone="primary" icon={<IconPlus size={14} />} onClick={() => setEditing("new")} disabled={editing === "new"}>
      Add a monitor
    </Button>
  );

  return (
    <div className="mf-mon">
      <h1 className="mf-sr">Uptime</h1>

      {editing === "new" && siteId != null ? <MonitorEditor siteId={siteId} onDone={finish} /> : null}

      <Panel>
        <PanelHead
          title="Uptime"
          note="Each monitor requests one address on a schedule, from this server, and opens an incident when it fails several checks in a row."
          actions={none ? undefined : addButton}
        />
        {monitors.isError ? (
          <ErrorState
            title="Could not load the monitors"
            detail={problem(monitors.error)}
            onRetry={() => void monitors.refetch()}
          />
        ) : none ? (
          <Empty
            title="No monitors yet"
            body="A monitor checks a URL every few minutes from this server and alerts the channels you choose when it fails. No request leaves this server until a monitor exists."
            action={editing === "new" ? undefined : addButton}
          />
        ) : (
          <>
            {loading ? (
              <StripWaiting count={6} />
            ) : summary.isError ? (
              <p className="mf-mon-inline-err" role="alert">
                The uptime figures did not load: {problem(summary.error)}{" "}
                <Button size="sm" tone="quiet" onClick={() => void summary.refetch()}>
                  Try again
                </Button>
              </p>
            ) : (
              <Strip items={siteStrip(rows, openIncidents)} />
            )}
            <div className="mf-mon-list">
              <Sheet
                rows={rows}
                columns={monitorColumns(current?.monitor.id)}
                getKey={(r) => r.monitor.id}
                loading={monitors.isPending}
                onRowClick={(r) => {
                  select(r.monitor.id);
                  if (editing && editing !== "new") setEditing(null);
                }}
              />
            </div>
            {summary.data ? (
              <p className="mf-mon-foot mf-mon-quiet">Checked from: {summary.data.region}</p>
            ) : null}
          </>
        )}
      </Panel>

      {current && siteId != null ? (
        editing === current.monitor.id ? (
          <MonitorEditor key={current.monitor.id} siteId={siteId} monitor={current.monitor} onDone={finish} />
        ) : (
          <MonitorDetail
            key={current.monitor.id}
            siteId={siteId}
            monitor={current.monitor}
            summary={current.figures}
            openIncidents={openFor(current.monitor.id)}
            onEdit={() => setEditing(current.monitor.id)}
            onDeleted={() => select(null)}
          />
        )
      ) : null}

      {none ? null : (
        <Panel>
          <PanelHead
            title="Incidents"
            note="An incident opens when a monitor fails its set number of checks in a row, and closes when the monitor passes again. Newest first."
          />
          {incidents.isError ? (
            <ErrorState
              title="Could not load the incidents"
              detail={problem(incidents.error)}
              onRetry={() => void incidents.refetch()}
            />
          ) : (
            <Sheet
              rows={incidents.data ?? []}
              columns={INCIDENT_COLUMNS}
              getKey={(i) => i.id}
              loading={incidents.isPending}
              onRowClick={(i) => select(i.monitor_id)}
              empty={
                <Empty
                  title="No incidents"
                  body="Nothing has failed enough checks in a row to open an incident. When one opens, it is listed here and sent to the monitor's channels."
                />
              }
            />
          )}
        </Panel>
      )}
    </div>
  );
}

export default UptimeScreen;
