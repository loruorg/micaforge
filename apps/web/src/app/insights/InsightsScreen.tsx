/**
 * Insights, at /app/:site/insights
 *
 * The list of investigations Micaforge has opened on this site. An
 * investigation is opened by the server, not by a person: on a schedule it
 * compares each metric with its own normal range (this week against last
 * week, this hour against the same hour of the last four weeks) and opens one
 * when a metric moves outside it. It then keeps checking, and closes nothing on
 * its own initiative that a person has not seen.
 *
 * Three decisions shape the screen:
 *
 *   1. IT IS NOT A REPORT. There is no date range and no audience switch here,
 *      and the top bar hides its own. An investigation carries its own window
 *      ("7d vs previous 7d"), and a range control above it would suggest the
 *      list can be re-read over another period, which it cannot.
 *   2. THE CHANGE IS SAID THREE WAYS. A caret, a sign and a tone, with the tone
 *      following the metric: a bounce rate going up is bad news, visitors
 *      going up is good news. The two figures it was computed from sit beside
 *      it, so a "+400%" on a baseline of two visitors reads as what it is.
 *   3. SCAN NOW IS A REQUEST, NOT A RESULT. The scan runs as a job. The button
 *      says it was queued, and the list then checks back for a short while on
 *      its own, rather than pretending the answer is already here.
 */
import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router";
import { useMutation, useQuery } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { Investigation, InvestigationStatus } from "../../lib/api";
import { ago } from "../../lib/format";
import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconReplay } from "../../components/react/icons";
import { useSite } from "../providers";
import {
  Change,
  SeverityMark,
  StatusMark,
  figure,
  fullTime,
  insightError,
  metricWords,
} from "./marks";
import "../../styles/app/insights.css";

type Filter = InvestigationStatus | "all";

const FILTERS: { value: Filter; label: string }[] = [
  { value: "open", label: "Open" },
  { value: "monitoring", label: "Monitoring" },
  { value: "resolved", label: "Resolved" },
  { value: "all", label: "All" },
];

/** What an empty list means under each filter, in words. */
const NONE_WITH: Record<InvestigationStatus, string> = {
  open: "No open investigation",
  monitoring: "No investigation is being monitored",
  resolved: "No resolved investigation",
};

/** How long the list keeps checking back after a scan, and how often. */
const WATCH_MS = 2 * 60_000;
const WATCH_EVERY_MS = 15_000;

/** Every list key starts ["insights", "list"], so a status change on the detail page refreshes them all. */
const insightsKey = (siteId: number | undefined, status: Filter) =>
  ["insights", "list", siteId ?? null, status] as const;

export function InsightsScreen() {
  const { siteId } = useSite();
  const { site } = useParams();
  const navigate = useNavigate();
  const [status, setStatus] = useState<Filter>("all");
  const [watchUntil, setWatchUntil] = useState<number | null>(null);

  // The watch ends by itself; this only clears the flag so the note under
  // the button stops saying the list is checking.
  useEffect(() => {
    if (watchUntil == null) return;
    const t = window.setTimeout(() => setWatchUntil(null), Math.max(0, watchUntil - Date.now()));
    return () => window.clearTimeout(t);
  }, [watchUntil]);

  const list = useQuery({
    queryKey: insightsKey(siteId, status),
    queryFn: () =>
      api
        .get<Investigation[]>("/api/investigations", { site: siteId as number, status })
        .then((r) => r.data),
    enabled: siteId != null,
    refetchInterval: () => (watchUntil != null && Date.now() < watchUntil ? WATCH_EVERY_MS : false),
  });

  const scan = useMutation({
    mutationFn: () =>
      api.post<{ queued: boolean; job_id: string }>(
        `/api/investigations/scan?site=${encodeURIComponent(String(siteId))}`,
      ),
    onSuccess: () => setWatchUntil(Date.now() + WATCH_MS),
  });

  const open = (inv: Investigation) => navigate(`/app/${site ?? siteId}/insights/${inv.id}`);

  const columns: Column<Investigation>[] = [
    {
      key: "title",
      head: "Investigation",
      width: "40%",
      render: (inv) => (
        <div className="mf-ins-cell">
          <span className="mf-ins-cell__title">{inv.title}</span>
          <span className="mf-ins-cell__scope">
            {metricWords(inv.metric)}
            {inv.dimension ? (
              <>
                {" · "}
                {inv.dimension}: <span className="mf-data">{inv.dimension_value ?? "(none)"}</span>
              </>
            ) : null}
            {" · "}
            {inv.window}
          </span>
        </div>
      ),
    },
    { key: "severity", head: "Severity", render: (inv) => <SeverityMark value={inv.severity} /> },
    { key: "status", head: "Status", render: (inv) => <StatusMark value={inv.status} /> },
    {
      key: "change",
      head: "Change",
      numeric: true,
      render: (inv) => (
        <div className="mf-ins-cell mf-ins-cell--num">
          <Change metric={inv.metric} changePct={inv.change_pct} direction={inv.direction} />
          <span className="mf-ins-cell__scope mf-data">
            {figure(inv.metric, inv.current)} vs {figure(inv.metric, inv.baseline)}
          </span>
        </div>
      ),
    },
    {
      key: "updated",
      head: "Updated",
      numeric: true,
      render: (inv) => (
        <time className="mf-data" dateTime={inv.updated_at} title={fullTime(inv.updated_at)}>
          {ago(inv.updated_at)}
        </time>
      ),
    },
  ];

  const filterWord = FILTERS.find((f) => f.value === status)?.label.toLowerCase() ?? status;

  return (
    <div className="mf-ins">
      <header className="mf-ins-bar">
        <h1 className="mf-ins-bar__title">Insights</h1>
        <div className="mf-ins-bar__controls">
          <div className="mfs-bank" role="group" aria-label="Show investigations that are">
            {FILTERS.map((f) => (
              <button
                key={f.value}
                type="button"
                className="mfs-bank__key"
                aria-pressed={status === f.value}
                onClick={() => setStatus(f.value)}
              >
                {f.label}
              </button>
            ))}
          </div>
          <Button
            icon={<IconReplay size={14} />}
            disabled={siteId == null || scan.isPending}
            onClick={() => scan.mutate()}
          >
            {scan.isPending ? "Queueing…" : "Scan now"}
          </Button>
        </div>
        {scan.isError || scan.isSuccess ? (
          <p
            className="mf-ins-say"
            data-tone={scan.isError ? "warn" : "good"}
            role={scan.isError ? "alert" : "status"}
          >
            {scan.isError
              ? insightError(scan.error, "a scan")
              : watchUntil != null
                ? "Scan queued. New investigations appear here in a minute or two; this list checks again every few seconds until then."
                : "Scan queued. If nothing new appeared, the scan found no metric outside its normal range, or it has not finished yet."}
          </p>
        ) : null}
      </header>

      <Panel>
        <PanelHead
          title="Investigations"
          note="Each one is a metric that moved outside its normal range, with the figures it was measured from. Select one to see the findings, the explanation and the queries behind it."
        />
        {list.isError ? (
          <ErrorState
            title="Could not load the investigations"
            detail={insightError(list.error, "reading investigations")}
            onRetry={() => void list.refetch()}
          />
        ) : (
          <Sheet
            rows={list.data ?? []}
            columns={columns}
            getKey={(inv) => inv.id}
            loading={siteId == null || list.isPending}
            onRowClick={open}
            empty={
              status === "all" ? (
                <Empty
                  title="No investigation yet"
                  body="Micaforge checks this site on a schedule and opens an investigation when a metric moves outside its normal range. Scan now checks it at once."
                />
              ) : (
                <Empty
                  title={NONE_WITH[status]}
                  body={`No investigation on this site has the status ${filterWord}. Choose All to see every one.`}
                />
              )
            }
          />
        )}
      </Panel>
    </div>
  );
}

export default InsightsScreen;
