/**
 * The bench under the record, and the plumbing the two record screens share.
 *
 * The six sheets here are deliberately NOT a grid of identical cards. They are
 * panels laid on one bench at three different widths, assigned by how much room
 * the content genuinely needs: a page path gets the long sheet, a browser name
 * does not, and the event ledger runs the full width because its rows carry
 * four figures. Each sheet also reports a different pair of measures, because
 * the useful question about a country is not the useful question about a page.
 *
 * Every row is live: clicking one files that value as a filter on the whole
 * screen, which is why the filter bar above is a real control and not a badge.
 */
import { useMemo } from "react";
import { Link } from "react-router";
import { useQuery } from "@tanstack/react-query";

import { api, ApiError } from "../../lib/api";
import type { Audience, BreakdownRow, Envelope, Filter, SiteSummary } from "../../lib/api";
import { duration, num, pct, shortPath } from "../../lib/format";
import { againstLeader, sourceName } from "../display";
import { Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconOut } from "../../components/react/icons";
import { useAppState } from "../state";
import { useSite } from "../providers";
import "../../styles/app/record.css";

/* ── the two hooks the router agent owns ─────────────────────────────────────
   Declared as the shapes this area was built against, so a change of signature
   upstream shows up here as one edit rather than forty. */

export interface RecordState {
  range: string;
  setRange: (v: string) => void;
  audience: Audience;
  setAudience: (v: Audience) => void;
  filters: Filter[];
  addFilter: (f: Filter) => void;
  removeFilter: (index: number) => void;
  clearFilters: () => void;
}

export function useRecordState(): RecordState {
  return useAppState() as unknown as RecordState;
}

export interface RecordSite {
  site?: SiteSummary | null;
  siteId: number | null;
  domain?: string;
  timezone?: string;
  isLoading: boolean;
}

export function useRecordSite(): RecordSite {
  return useSite() as unknown as RecordSite;
}

/* ── envelope, keys and honest number handling ───────────────────────────── */

/**
 * The caliper writes a measured interval back into the shared range as
 * `<startISO>..<endISO>`; every other value is one of the named ranges in
 * CONTRACT §3.1. This is the one place that distinction is unpicked.
 */
export function rangeEnvelope(range: string): Pick<Envelope, "range" | "start" | "end"> {
  const cut = range.indexOf("..");
  if (cut > 0) return { start: range.slice(0, cut), end: range.slice(cut + 2) };
  return { range };
}

const RANGE_LABELS: Record<string, string> = {
  today: "Today",
  yesterday: "Yesterday",
  "24h": "Last 24 hours",
  "7d": "Last 7 days",
  "30d": "Last 30 days",
  "90d": "Last 90 days",
  "12mo": "Last 12 months",
  mtd: "Month to date",
  ytd: "Year to date",
  all: "All time",
};

const dayLabel = (iso: string) => {
  const t = Date.parse(iso);
  return Number.isFinite(t)
    ? new Date(t).toLocaleDateString(undefined, { day: "numeric", month: "short" })
    : iso;
};

/** What the operator is looking at, said in words. */
export function rangeLabel(range: string): string {
  const cut = range.indexOf("..");
  if (cut > 0) return `${dayLabel(range.slice(0, cut))} to ${dayLabel(range.slice(cut + 2))}`;
  return RANGE_LABELS[range] ?? range;
}

export const isMeasuredRange = (range: string) => range.indexOf("..") > 0;

/**
 * Shares and rates cross the wire either as a fraction or as a percentage
 * depending on the endpoint, and the contract does not settle it. Reading a
 * value above 1 as a percentage is the only interpretation that cannot turn a
 * 42% bounce rate into 4,200%.
 */
export function asFraction(v: number | null | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return Math.abs(v) > 1 ? v / 100 : v;
}

export function asPercent(v: number | null | undefined): number | null {
  const f = asFraction(v);
  return f == null ? null : f * 100;
}

/** Epoch values arrive in seconds from some stores and milliseconds from others. */
export const toMs = (t: number): number => (t < 1e12 ? t * 1000 : t);

/** The shared query key shape: ["stats", endpoint, site, range, audience, filters]. */
export function statsKey(
  endpoint: string,
  siteId: number | null,
  range: string,
  audience: Audience,
  filtersJson: string,
  ...extra: (string | number | boolean | null)[]
): (string | number | boolean | null)[] {
  return ["stats", endpoint, siteId, range, audience, filtersJson, ...extra];
}

export interface RecordQuery {
  siteId: number | null;
  range: string;
  audience: Audience;
  filters: Filter[];
  filtersJson: string;
  timezone?: string;
}

export function baseEnvelope(q: RecordQuery, extra?: Partial<Envelope>): Envelope {
  return {
    ...extra,
    site: q.siteId ?? 0,
    ...rangeEnvelope(q.range),
    audience: q.audience,
    ...(q.filters.length ? { filters: q.filters } : {}),
    ...(q.timezone ? { tz: q.timezone } : {}),
  };
}

export const errorDetail = (e: unknown): string =>
  e instanceof ApiError
    ? e.code === "network"
      ? "The Micaforge server did not answer. Check that it is running, then take the reading again."
      : e.message
    : "Something went wrong between the dashboard and the server.";

/* ── one sheet on the bench ──────────────────────────────────────────────── */

type Width = "lead" | "unit" | "full";

interface BenchSheetProps {
  title: string;
  /** Breakdown dimension, per CONTRACT §3.2. */
  dimension: string;
  width: Width;
  seeAll: string;
  q: RecordQuery;
  limit?: number;
  columns: Column<BreakdownRow>[];
  emptyTitle: string;
  emptyBody: string;
  onDrill: (row: BreakdownRow) => void;
  drillHint: string;
}

function BenchSheet({
  title,
  dimension,
  width,
  seeAll,
  q,
  limit = 8,
  columns,
  emptyTitle,
  emptyBody,
  onDrill,
  drillHint,
}: BenchSheetProps) {
  const query = useQuery({
    queryKey: statsKey(`breakdown:${dimension}`, q.siteId, q.range, q.audience, q.filtersJson, limit),
    queryFn: () =>
      api
        .get<BreakdownRow[]>("/api/stats/breakdown", baseEnvelope(q, { dimension, limit }))
        .then((r) => r.data),
    enabled: q.siteId != null,
    staleTime: 30_000,
  });

  const rows = Array.isArray(query.data) ? query.data : [];
  const loading = q.siteId == null || query.isPending;

  /* The ink behind a row is drawn against the leading row of THIS sheet, not
     against the whole site: at a typical share of eight per cent every row on
     the bench drew the same short stub. The figure in the share column is still
     the share of the whole, which is where that number belongs. */
  const inked = columns.map((c) =>
    c.share ? { ...c, share: againstLeader(rows, c.share) } : c,
  );

  return (
    <Panel className={`mfr-${width}`}>
      <PanelHead
        title={title}
        actions={
          <Link className="mf-btn mf-btn--quiet mf-btn--sm mfr-key" to={seeAll}>
            <span className="mf-btn__label">See all</span>
            <span className="mf-btn__icon">
              <IconOut size={13} />
            </span>
          </Link>
        }
      />
      <div className="mfr-body">
        {!query.isError && rows.length > 0 ? (
          <p className="mf-sr">
            {drillHint} The ink behind each row is its size against the leading row.
          </p>
        ) : null}
        {query.isError ? (
          <ErrorState
            title={`${title} did not come back`}
            detail={errorDetail(query.error)}
            onRetry={() => void query.refetch()}
          />
        ) : (
          <Sheet<BreakdownRow>
            rows={rows}
            columns={inked}
            loading={loading}
            getKey={(r, i) => `${r.key}-${i}`}
            onRowClick={onDrill}
            empty={<Empty title={emptyTitle} body={emptyBody} />}
          />
        )}
      </div>
    </Panel>
  );
}

/* ── column sets ─────────────────────────────────────────────────────────────
   Each sheet answers its own question, so no two share a column set. */

const shareOf = (r: BreakdownRow): number | undefined => asFraction(r.share) ?? undefined;

const visitorsCol: Column<BreakdownRow> = {
  key: "visitors",
  head: "Visitors",
  numeric: true,
  width: "6.5rem",
  render: (r) => <span className="mf-data">{num(r.visitors)}</span>,
};

/* ── the bench ───────────────────────────────────────────────────────────── */

export function TopSheets({
  q,
  base,
  onDrill,
}: {
  q: RecordQuery;
  base: string;
  onDrill: (f: Filter) => void;
}) {
  const sheets = useMemo(
    () => [
      {
        title: "Top pages",
        dimension: "pathname",
        width: "lead" as Width,
        seeAll: `${base}/pages`,
        limit: 8,
        emptyTitle: "No page has been read yet",
        emptyBody: "Pages appear here as soon as the tracker reports its first pageview.",
        drillHint: "Choosing a row filters this screen to that page.",
        onRow: (r: BreakdownRow) => onDrill({ dimension: "pathname", op: "is", value: r.key }),
        columns: [
          {
            key: "path",
            head: "Path",
            share: shareOf,
            render: (r: BreakdownRow) => (
              <span className="mf-data" title={r.key}>
                {shortPath(r.label || r.key, 40)}
              </span>
            ),
          },
          {
            key: "pageviews",
            head: "Views",
            numeric: true,
            width: "6rem",
            render: (r: BreakdownRow) => <span className="mf-data">{num(r.pageviews)}</span>,
          },
          visitorsCol,
          {
            key: "time",
            head: "Time on page",
            numeric: true,
            width: "8rem",
            render: (r: BreakdownRow) => (
              <span className="mf-data">{duration(r.avg_duration_s)}</span>
            ),
          },
        ] as Column<BreakdownRow>[],
      },
      {
        title: "Sources",
        dimension: "source",
        width: "unit" as Width,
        seeAll: `${base}/sources`,
        limit: 8,
        emptyTitle: "Nothing has sent a reader yet",
        emptyBody: "Referrers, campaigns and answer engines are grouped here once they arrive.",
        drillHint: "Choosing a row filters this screen to that source.",
        onRow: (r: BreakdownRow) => onDrill({ dimension: "source", op: "is", value: r.key }),
        columns: [
          {
            key: "source",
            head: "Source",
            share: shareOf,
            render: (r: BreakdownRow) => (
              /* The name is prose and is written the way the sender writes it:
                 the server's own label where there is one, the key cased for
                 reading where there is not. The key itself is untouched, and
                 it is what the row filters on. */
              <span className="mfr-name" title={r.key}>
                {sourceName(r)}
              </span>
            ),
          },
          visitorsCol,
        ] as Column<BreakdownRow>[],
      },
      {
        title: "Countries",
        dimension: "country",
        width: "unit" as Width,
        seeAll: `${base}/locations`,
        limit: 7,
        emptyTitle: "No country resolved yet",
        emptyBody: "Country comes from the bundled geolocation database, never from a cookie.",
        drillHint: "Choosing a row filters this screen to that country.",
        onRow: (r: BreakdownRow) => onDrill({ dimension: "country", op: "is", value: r.key }),
        columns: [
          {
            key: "country",
            head: "Country",
            share: shareOf,
            render: (r: BreakdownRow) => (
              <span className="mfr-name">{r.label || r.key || "Unknown"}</span>
            ),
          },
          visitorsCol,
        ] as Column<BreakdownRow>[],
      },
      {
        title: "Browsers",
        dimension: "browser",
        width: "unit" as Width,
        seeAll: `${base}/devices`,
        limit: 7,
        emptyTitle: "No browser reported yet",
        emptyBody: "The browser is read from the request header at ingest, never fingerprinted.",
        drillHint: "Choosing a row filters this screen to that browser.",
        onRow: (r: BreakdownRow) => onDrill({ dimension: "browser", op: "is", value: r.key }),
        columns: [
          {
            key: "browser",
            head: "Browser",
            share: shareOf,
            render: (r: BreakdownRow) => (
              <span className="mfr-name">{r.label || r.key || "Unknown"}</span>
            ),
          },
          visitorsCol,
        ] as Column<BreakdownRow>[],
      },
      {
        title: "Devices",
        dimension: "device_type",
        width: "unit" as Width,
        seeAll: `${base}/devices`,
        limit: 7,
        emptyTitle: "No device reported yet",
        emptyBody: "Desktop, mobile, tablet and the rest are split here once hits arrive.",
        drillHint: "Choosing a row filters this screen to that device type.",
        onRow: (r: BreakdownRow) => onDrill({ dimension: "device_type", op: "is", value: r.key }),
        columns: [
          {
            key: "device",
            head: "Device",
            share: shareOf,
            render: (r: BreakdownRow) => (
              <span className="mfr-name">{r.label || r.key || "Unknown"}</span>
            ),
          },
          {
            key: "sessions",
            head: "Sessions",
            numeric: true,
            width: "6.5rem",
            render: (r: BreakdownRow) => <span className="mf-data">{num(r.sessions)}</span>,
          },
        ] as Column<BreakdownRow>[],
      },
      {
        title: "Events",
        dimension: "event_name",
        width: "full" as Width,
        seeAll: `${base}/events`,
        limit: 6,
        emptyTitle: "No custom event has fired yet",
        emptyBody:
          "Call mf.track(\"name\", props) from the tracker, or turn on auto-capture for outbound links, downloads and form submits.",
        drillHint: "Choosing a row filters this screen to that event.",
        onRow: (r: BreakdownRow) => onDrill({ dimension: "event_name", op: "is", value: r.key }),
        columns: [
          {
            key: "event",
            head: "Event",
            share: shareOf,
            render: (r: BreakdownRow) => (
              <span className="mf-data" title={r.key}>
                {r.label || r.key}
              </span>
            ),
          },
          {
            key: "count",
            head: "Fired",
            numeric: true,
            width: "7rem",
            render: (r: BreakdownRow) => <span className="mf-data">{num(r.pageviews)}</span>,
          },
          visitorsCol,
          {
            key: "share",
            head: "Share of visitors",
            numeric: true,
            width: "10rem",
            render: (r: BreakdownRow) => <span className="mf-data">{pct(asFraction(r.share))}</span>,
          },
        ] as Column<BreakdownRow>[],
      },
    ],
    [base, onDrill],
  );

  return (
    /* Which pen this bench is written in. The ink behind a row is the trace of
       the audience the sheet is reading, so a human table is never drawn in the
       machine pen's shellac. */
    <div className="mfr-bench" data-trace={q.audience === "agent" ? "agent" : "human"}>
      {sheets.map((s) => (
        <BenchSheet
          key={s.dimension}
          q={q}
          title={s.title}
          dimension={s.dimension}
          width={s.width}
          seeAll={s.seeAll}
          limit={s.limit}
          columns={s.columns}
          emptyTitle={s.emptyTitle}
          emptyBody={s.emptyBody}
          drillHint={s.drillHint}
          onDrill={s.onRow}
        />
      ))}
    </div>
  );
}
