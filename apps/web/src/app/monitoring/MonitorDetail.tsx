/**
 * ONE MONITOR, in detail: where it stands now, how fast it has answered, and
 * the last fifty checks it ran.
 *
 * The strip at the top answers "is it working, and has it been?" in one line:
 * the state as a shape and a word, the three uptime windows, the last response
 * time, the certificate's remaining days when they are known, and how many of
 * its incidents are still open.
 *
 * The chart answers "is it getting slower?" and the table answers "what
 * exactly happened at 03:12?". Both refresh each minute, because a monitor
 * writes a new check every few minutes and a stale reading of an outage is the
 * one reading this screen must not give.
 *
 * "Test now" runs one check against the SAVED monitor and stores nothing, so
 * pressing it cannot open or close an incident. That is said beside the
 * button, because a person who presses it during an outage needs to know it
 * will not make the outage go away.
 */
import { useState } from "react";
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconPen } from "../../components/react/icons";
import { uptimeText, when } from "../../components/status/words";
import { api } from "../../lib/api";
import { ago, ms } from "../../lib/format";
import { LatencyChart } from "./LatencyChart";
import { CheckOutcome, ConfirmRow, PassMark, Say, Segments, StateMark, Strip } from "./parts";
import type { StripItem } from "./parts";
import type { Check, CheckResult, Monitor, MonitorSeries, MonitorSummary, SeriesRange } from "./types";
import { LIVE_MS, daysText, everyText, keys, localZone, problem } from "./types";

const RANGES: { v: SeriesRange; label: string }[] = [
  { v: "24h", label: "24 hours" },
  { v: "7d", label: "7 days" },
  { v: "30d", label: "30 days" },
];

const CHECK_COLUMNS: Column<Check>[] = [
  { key: "result", head: "Result", render: (c) => <PassMark ok={c.ok} /> },
  { key: "when", head: "Checked", render: (c) => <span className="mf-data">{when(c.checked_at)}</span> },
  {
    key: "status",
    head: "Status",
    numeric: true,
    render: (c) => <span className="mf-data">{c.status_code ?? "No response"}</span>,
  },
  { key: "latency", head: "Response time", numeric: true, render: (c) => <span className="mf-data">{ms(c.latency_ms)}</span> },
  {
    key: "detail",
    head: "Detail",
    render: (c) => {
      const text = c.error ?? (c.ssl_days_left != null ? `Certificate expires in ${daysText(c.ssl_days_left)}` : "");
      return text ? (
        <span className="mf-mon-wrap" title={text}>
          {text}
        </span>
      ) : (
        <span className="mf-mon-quiet">—</span>
      );
    },
  },
];

export function MonitorDetail({
  siteId,
  monitor,
  summary,
  openIncidents,
  onEdit,
  onDeleted,
}: {
  siteId: number;
  monitor: Monitor;
  summary: MonitorSummary | undefined;
  openIncidents: number;
  onEdit: () => void;
  onDeleted: () => void;
}) {
  const qc = useQueryClient();
  const [range, setRange] = useState<SeriesRange>("24h");
  const [confirming, setConfirming] = useState(false);

  const series = useQuery({
    queryKey: keys.series(monitor.id, range),
    queryFn: async () => {
      const tz = localZone();
      const zone = tz ? `&tz=${encodeURIComponent(tz)}` : "";
      const r = await api.get<MonitorSeries>(`/api/uptime/monitors/${monitor.id}/series?range=${range}${zone}`);
      return r.data;
    },
    placeholderData: keepPreviousData,
    refetchInterval: LIVE_MS,
  });

  const checks = useQuery({
    queryKey: keys.checks(monitor.id),
    queryFn: () => api.get<Check[]>(`/api/uptime/monitors/${monitor.id}/checks?limit=50`).then((r) => r.data),
    refetchInterval: LIVE_MS,
  });

  const test = useMutation({
    mutationFn: () => api.post<CheckResult>(`/api/uptime/monitors/${monitor.id}/test`).then((r) => r.data),
  });

  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: string }>(`/api/uptime/monitors/${monitor.id}`),
    onSuccess: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: keys.monitors(siteId) }),
        qc.invalidateQueries({ queryKey: keys.summary(siteId) }),
        qc.invalidateQueries({ queryKey: keys.incidents(siteId) }),
      ]);
      onDeleted();
    },
  });

  const items: StripItem[] = [
    { key: "state", label: "Now", value: <StateMark state={monitor.state} enabled={monitor.enabled} big />, note: monitor.last_checked_at ? `checked ${ago(monitor.last_checked_at)}` : "no check has run yet" },
    { key: "24h", label: "Uptime, 24 hours", value: uptimeText(summary?.uptime_24h) },
    { key: "7d", label: "Uptime, 7 days", value: uptimeText(summary?.uptime_7d) },
    { key: "30d", label: "Uptime, 30 days", value: uptimeText(summary?.uptime_30d) },
    {
      key: "latency",
      label: "Last response time",
      value: ms(summary?.last_latency_ms),
      note: summary?.avg_latency_24h_ms != null ? `${ms(summary.avg_latency_24h_ms)} average, 24 hours` : undefined,
    },
  ];
  if (summary?.ssl_days_left != null) {
    items.push({
      key: "ssl",
      label: "Certificate",
      value: daysText(summary.ssl_days_left),
      note: `fails under ${daysText(monitor.ssl_expiry_days)}`,
    });
  }
  items.push({
    key: "incidents",
    label: "Open incidents",
    value: String(openIncidents),
    note: openIncidents ? "listed under Incidents" : "none open",
  });

  const points = series.data?.points ?? [];
  const noChecks = points.every((p) => p.checks === 0);

  return (
    <Panel className="mf-mon-detail">
      <PanelHead
        title={monitor.name}
        note={
          <>
            <span className="mf-data mf-mon-url">
              {monitor.method} {monitor.url}
            </span>
            <span className="mf-mon-quiet">
              {" "}
              {everyText(monitor.interval_minutes)}. Checked from: {monitor.region}.
            </span>
          </>
        }
        actions={
          <>
            <Button size="sm" disabled={test.isPending} onClick={() => test.mutate()}>
              {test.isPending ? "Testing…" : "Test now"}
            </Button>
            <Button size="sm" icon={<IconPen size={13} />} onClick={onEdit}>
              Edit
            </Button>
            <Button size="sm" tone="danger" onClick={() => setConfirming(true)} disabled={confirming}>
              Delete
            </Button>
          </>
        }
      />

      {confirming ? (
        <ConfirmRow
          confirm="Delete monitor"
          busy={remove.isPending}
          onConfirm={() => remove.mutate()}
          onCancel={() => {
            setConfirming(false);
            remove.reset();
          }}
        >
          Delete <strong>{monitor.name}</strong>? Its checks stop at once. Status pages that show it stop showing
          it. This cannot be undone.
          {remove.isError ? (
            <>
              {" "}
              <Say tone="warn">{problem(remove.error)}</Say>
            </>
          ) : null}
        </ConfirmRow>
      ) : null}

      {test.data || test.isError ? (
        <div className="mf-mon-band">
          {test.data ? <CheckOutcome result={test.data} /> : null}
          {test.isError ? <Say tone="warn">{problem(test.error)}</Say> : null}
          <p className="mf-mon-quiet">A test stores nothing and does not open or close an incident.</p>
        </div>
      ) : null}

      <Strip items={items} dense />

      <div className="mf-mon-sub">
        <h3 className="mf-mon-sub__title">Response time</h3>
        <Segments value={range} options={RANGES} onChange={setRange} label="Chart range" />
      </div>
      <div className="mf-mon-band">
        {series.isPending ? (
          <div className="mf-mon-chart-wait" aria-busy="true" aria-live="polite">
            <span className="mf-sr">Loading the chart…</span>
            {[52, 38, 61, 44, 70, 49, 57, 41].map((h, i) => (
              <span key={i} className="mf-wait" style={{ height: `${h}%` }} />
            ))}
          </div>
        ) : series.isError ? (
          <ErrorState
            title="Could not load the response times"
            detail={problem(series.error)}
            onRetry={() => void series.refetch()}
          />
        ) : noChecks ? (
          <Empty
            title="No checks in this range yet"
            body={
              monitor.enabled
                ? `The next check runs ${Date.parse(monitor.next_check_at) > Date.now() ? ago(monitor.next_check_at) : "in a moment"}. Each check adds a point to this chart.`
                : "This monitor is paused, so it runs no checks. Edit it and turn on \"Run this monitor\" to start."
            }
          />
        ) : (
          <>
            <LatencyChart points={points} range={range} />
            <p className="mf-mon-quiet">
              Uptime over this range: <span className="mf-data">{uptimeText(series.data?.uptime)}</span>
            </p>
          </>
        )}
      </div>

      <div className="mf-mon-sub">
        <h3 className="mf-mon-sub__title">Recent checks</h3>
      </div>
      {checks.isError ? (
        <ErrorState title="Could not load the checks" detail={problem(checks.error)} onRetry={() => void checks.refetch()} />
      ) : (
        <Sheet
          rows={checks.data ?? []}
          columns={CHECK_COLUMNS}
          getKey={(c, i) => `${c.checked_at}-${i}`}
          loading={checks.isPending}
          empty={
            <Empty
              title="No checks yet"
              body="The last fifty checks are listed here, newest first, as soon as the first one runs."
            />
          }
        />
      )}
    </Panel>
  );
}

export default MonitorDetail;
