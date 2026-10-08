/**
 * Performance: how the site actually loaded, for the people who loaded it.
 *
 * The screen answers one question and then lets you cut it: is this site fast
 * enough by the published Core Web Vitals bounds, and where is it not. So the
 * five metrics sit on one readout strip, the distribution behind each figure
 * is shown underneath it, and the breakdown reports the
 * ONE metric you selected across whichever dimension you asked for, rather
 * than a wall of twenty-five percentile columns nobody reads.
 *
 * Every figure here comes from `GET /api/stats/performance` under the standard
 * query envelope (docs/CONTRACT.md §3.1). The response payload shapes are
 * declared below because §3.3 fixes the endpoint and its parameters but not its
 * body; these are the shapes the dashboard reads.
 */
import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, ApiError, type Audience, type Filter } from "../../lib/api";
import { delta, num, shortPath } from "../../lib/format";
import {
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  type Channel,
  type Column,
} from "../../components/react/ui";
import { IconInfo } from "../../components/react/icons";
import {
  RATING_WORD,
  VITALS,
  VITAL_BY_KEY,
  VitalsDistribution,
  rateVital,
  type VitalHistogram,
  type VitalKey,
  type VitalStats,
} from "./VitalsDistribution";
import { useAppState } from "../state";
import { useSite } from "../providers";

/* ── what the endpoint returns ───────────────────────────────────────────── */

export interface PerformanceSummary {
  /** Page loads that reported at least one vital in the interval. */
  samples: number;
  metrics: VitalStats;
  /** Bins per metric. Absent when the server was asked for percentiles only. */
  histogram?: VitalHistogram;
  compare?: { metrics: VitalStats } | null;
}

export interface PerformanceRow {
  key: string;
  label?: string;
  samples: number;
  metrics: VitalStats;
}

export interface PerformanceBreakdown {
  dimension: string;
  rows: PerformanceRow[];
}

/**
 * The dimensions this screen cuts by.
 *
 * `connection` is not one of the fixed dimensions in CONTRACT.md §3.2, because
 * it is not a column on `events`: the tracker reports it as a property when
 * the browser exposes `navigator.connection`. So it is asked for the way the
 * contract asks for any property: `props.<key>`. Chromium exposes the Network
 * Information API; Safari and Firefox do not, which is why that breakdown is
 * often thinner than the others and says so when it is empty.
 */
const DIMENSIONS = [
  { v: "pathname", label: "Page" },
  { v: "country", label: "Country" },
  { v: "device_type", label: "Device" },
  { v: "browser", label: "Browser" },
  { v: "props.connection", label: "Connection" },
] as const;

type PerfDimension = (typeof DIMENSIONS)[number]["v"];

/** Effective connection types, written the way a person writes them. */
const CONNECTION_LABELS: Record<string, string> = {
  "4g": "4G or better",
  "3g": "3G",
  "2g": "2G",
  "slow-2g": "Slow 2G",
};

function rowLabel(row: PerformanceRow, dimension: PerfDimension): string {
  const raw = row.label ?? row.key;
  if (!raw) return "Not reported";
  if (dimension === "pathname") return shortPath(raw);
  if (dimension === "props.connection") return CONNECTION_LABELS[raw] ?? raw;
  return raw;
}

function reason(err: unknown): string | undefined {
  if (err instanceof ApiError) return err.message;
  return err instanceof Error ? err.message : undefined;
}

/** A readout that has not been read yet. Same strip, same rhythm, no figures. */
export function ReadoutWaiting({ count }: { count: number }) {
  return (
    <div className="mf-readout mf-readout-wait" aria-busy="true" aria-live="polite">
      <span className="mf-sr">Loading…</span>
      {Array.from({ length: count }).map((_, i) => (
        <div key={i} className="mf-readout__chan">
          <span className="mf-wait mf-wait--label" />
          <span className="mf-wait mf-wait--value" />
          <span className="mf-wait mf-wait--foot" />
        </div>
      ))}
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function PerformanceScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const { range, audience, filters } = useAppState();

  const [metric, setMetric] = useState<VitalKey>("lcp");
  const [dimension, setDimension] = useState<PerfDimension>("pathname");

  const activeFilters: Filter[] = filters ?? [];
  const filtersJson = JSON.stringify(activeFilters);
  const ready = siteId != null && !siteLoading;

  const envelope = {
    site: siteId as number,
    range,
    audience: audience as Audience,
    filters: activeFilters,
  };

  const summary = useQuery({
    queryKey: ["stats", "performance", siteId, range, audience, filtersJson],
    queryFn: () =>
      api
        .get<PerformanceSummary>("/api/stats/performance", { ...envelope, compare: "previous" })
        .then((r) => r.data),
    enabled: ready,
    staleTime: 60_000,
  });

  const breakdown = useQuery({
    queryKey: ["stats", "performance/breakdown", siteId, range, audience, filtersJson, dimension],
    queryFn: () =>
      api
        .get<PerformanceBreakdown>("/api/stats/performance", { ...envelope, dimension })
        .then((r) => r.data),
    enabled: ready,
    staleTime: 60_000,
  });

  const spec = VITAL_BY_KEY[metric];
  const dimLabel = DIMENSIONS.find((d) => d.v === dimension)?.label ?? "Page";
  const rows = breakdown.data?.rows ?? [];
  const rowTotal = rows.reduce((sum, r) => sum + (r.samples || 0), 0);

  const channels: Channel[] = VITALS.map((s) => {
    const stat = summary.data?.metrics?.[s.key];
    const before = summary.data?.compare?.metrics?.[s.key]?.p75 ?? null;
    const rating = rateVital(s.key, stat?.p75 ?? null);
    return {
      key: s.key,
      label: s.abbr,
      value: stat?.p75 == null ? "—" : s.format(stat.p75),
      // Every vital reads better lower, so a fall is the good outcome.
      delta: stat?.p75 == null ? null : delta(stat.p75, before),
      inverse: true,
      note: RATING_WORD[rating],
      active: metric === s.key,
      onSelect: () => setMetric(s.key),
    };
  });

  const columns: Column<PerformanceRow>[] = [
    {
      key: "label",
      head: dimLabel,
      width: "32%",
      render: (row) => (
        <span title={row.label || row.key || undefined}>{rowLabel(row, dimension)}</span>
      ),
      share: (row) => (rowTotal > 0 ? row.samples / rowTotal : undefined),
    },
    {
      key: "samples",
      head: "Loads",
      numeric: true,
      render: (row) => <span className="mf-data">{num(row.samples)}</span>,
    },
    ...(["p50", "p75", "p95"] as const).map((q) => ({
      key: q,
      head: q,
      numeric: true,
      render: (row: PerformanceRow) => {
        const v = row.metrics?.[metric]?.[q];
        return <span className="mf-data">{v == null ? "—" : spec.format(v)}</span>;
      },
    })),
    {
      key: "rating",
      head: "Rating",
      render: (row) => {
        const rating = rateVital(metric, row.metrics?.[metric]?.p75 ?? null);
        return (
          <span className="mf-rating" data-rating={rating}>
            {RATING_WORD[rating]}
          </span>
        );
      },
    },
  ];

  const noVitals = summary.isSuccess && (summary.data?.samples ?? 0) === 0;

  return (
    <div className="mf-hs">
      <h1 className="mf-sr">Performance</h1>

      {audience === "agent" ? (
        <p className="mf-hs-note">
          <IconInfo size={16} />
          <span>
            AI crawlers do not run the tracker, so no vitals are recorded for them. Set the
            audience to People to see vitals. What crawlers fetched is on the AI crawlers screen.
          </span>
        </p>
      ) : null}

      {/* ── the five metrics, on one strip ────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Web vitals"
          note={
            summary.data
              ? `Rated at the 75th percentile, the way Chrome rates them. ${num(
                  summary.data.samples,
                )} page loads reported vitals in this period.`
              : "Rated at the 75th percentile, the way Chrome rates them."
          }
        />
        {summary.isError ? (
          <ErrorState
            title="Could not load web vitals"
            detail={reason(summary.error)}
            onRetry={() => void summary.refetch()}
          />
        ) : summary.isPending || siteLoading ? (
          <ReadoutWaiting count={5} />
        ) : noVitals ? (
          <Empty
            title="No vitals in this period"
            body="Vitals arrive on the tracker's web_vitals event. Make sure the snippet on this site is current and has vitals turned on, then check again after the next few page loads."
          />
        ) : (
          <Readout channels={channels} />
        )}
      </Panel>

      {/* ── what each figure is made of ───────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Distribution"
          note="Every page load, grouped into bins. The dashed lines are the published Core Web Vitals thresholds. The solid line is this site's 75th percentile."
        />
        {summary.isError ? (
          <ErrorState
            title="Could not load the distribution"
            detail={reason(summary.error)}
            onRetry={() => void summary.refetch()}
          />
        ) : summary.isPending || siteLoading ? (
          <div className="mf-hs-wait" aria-busy="true">
            <span className="mf-sr">Loading…</span>
            {Array.from({ length: 5 }).map((_, i) => (
              <span key={i} className="mf-wait" />
            ))}
          </div>
        ) : noVitals ? (
          <Empty
            title="No distribution yet"
            body="The distribution uses the same page loads as the figures above. It fills in as soon as vitals arrive."
          />
        ) : (
          <VitalsDistribution
            metrics={summary.data?.metrics}
            histogram={summary.data?.histogram}
            selected={metric}
            onSelect={setMetric}
          />
        )}
      </Panel>

      {/* ── where it is slow ──────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title={`${spec.name} by ${dimLabel.toLowerCase()}`}
          note={`${spec.abbr} at the 50th, 75th and 95th percentile. The rating is read off the 75th.`}
        />
        <div className="mf-hs-strip">
          <span className="mf-hs-strip__label" id="mf-perf-dim">
            Break down by
          </span>
          <div className="mf-hs-bank" role="group" aria-labelledby="mf-perf-dim">
            {DIMENSIONS.map((d) => (
              <button
                key={d.v}
                type="button"
                className="mf-hs-bank__key"
                aria-pressed={dimension === d.v}
                onClick={() => setDimension(d.v)}
              >
                {d.label}
              </button>
            ))}
          </div>
        </div>
        {breakdown.isError ? (
          <ErrorState
            title={`Could not load the ${dimLabel.toLowerCase()} breakdown`}
            detail={reason(breakdown.error)}
            onRetry={() => void breakdown.refetch()}
          />
        ) : (
          <Sheet
            rows={rows}
            columns={columns}
            getKey={(row, i) => `${row.key}-${i}`}
            loading={breakdown.isPending || siteLoading}
            empty={
              dimension === "props.connection" ? (
                <Empty
                  title="No connection reported"
                  body="The tracker sends connection as a property, and only when the browser exposes navigator.connection. Chromium does; Safari and Firefox do not. The other breakdowns are not affected."
                />
              ) : (
                <Empty
                  title={`No ${dimLabel.toLowerCase()} reported vitals`}
                  body="Choose a longer period or clear a filter. No page loads in this selection reported a vital."
                />
              )
            }
          />
        )}
      </Panel>
    </div>
  );
}

export default PerformanceScreen;
