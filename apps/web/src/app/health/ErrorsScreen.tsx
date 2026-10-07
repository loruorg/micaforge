/**
 * Errors: every distinct failure the tracker saw, grouped by fingerprint.
 *
 * A fingerprint is the unit here, not an occurrence: one broken call in one
 * component can write ten thousand rows, and ten thousand rows is one problem.
 * So the sheet ranks groups, and each group carries the two numbers that decide
 * whether it matters (how often it fired, and how many readers it reached)
 * beside a trend trace that says whether it is getting worse.
 *
 * Identity is carried by type hierarchy: the class in the ink, the message in a
 * dimmer tone under it, the state as a word. Nothing here is a coloured chip.
 *
 * Reads `GET /api/stats/errors` under the standard query envelope
 * (docs/CONTRACT.md §3.1). §3.3 fixes the endpoint but not its body, so the
 * shapes the dashboard reads are declared here.
 */
import { useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router";
import { api, ApiError, type Audience, type Filter } from "../../lib/api";
import { ago, num, shortPath } from "../../lib/format";
import {
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  type Channel,
  type Column,
} from "../../components/react/ui";
import { IconChevron, IconInfo } from "../../components/react/icons";
import { ReadoutWaiting } from "./PerformanceScreen";
import { useAppState } from "../state";
import { useSite } from "../providers";

/* ── what the endpoint returns ───────────────────────────────────────────── */

export type ErrorStatus = "open" | "resolved" | "ignored";

export interface ErrorGroup {
  fingerprint: string;
  name: string;
  message: string;
  /** Occurrences in the interval. */
  count: number;
  /** Distinct visitors who hit it. */
  visitors: number;
  /** Distinct sessions it fired in. The detail carries the sessions themselves. */
  session_count?: number;
  first_seen: number | string | null;
  last_seen: number | string | null;
  /** False when the exception reached the page uncaught. */
  handled?: boolean;
  release?: string;
  /** The path it fired on most often. */
  top_path?: string;
  browsers?: string[];
  status?: ErrorStatus;
  /** Occurrences per bucket across the interval: the group's trend trace. */
  trend?: number[];
}

export interface ErrorFacet {
  value: string;
  count: number;
}

export type ErrorFacetKey = "browser" | "os" | "pathname" | "release";

export interface ErrorsResponse {
  rows: ErrorGroup[];
  totals?: { groups: number; occurrences: number; visitors?: number };
  facets?: Partial<Record<ErrorFacetKey, ErrorFacet[]>>;
}

/** A list endpoint may hand back the array itself or a wrapper around it. */
function asResponse(data: ErrorsResponse | ErrorGroup[] | null | undefined): ErrorsResponse {
  if (!data) return { rows: [] };
  if (Array.isArray(data)) return { rows: data };
  return { rows: data.rows ?? [], totals: data.totals, facets: data.facets };
}

function reason(err: unknown): string | undefined {
  if (err instanceof ApiError) return err.message;
  return err instanceof Error ? err.message : undefined;
}

const PAGE = 50;

/* ── the trend trace ─────────────────────────────────────────────────────── */

/**
 * A group's trend, drawn small. It is a scratch like every other trace in this
 * product: the ridge of displaced soot first, the bright line over it. Oxide is
 * the annotation ink a station operator reached for to mark a fault.
 */
export function ErrorTrace({ values, label }: { values?: number[] | null; label: string }) {
  const pts = (values ?? []).filter((v) => Number.isFinite(v));
  if (pts.length < 2) {
    return (
      <span className="mf-trace--none" title={`${label}: no trend for this interval`}>
        —
      </span>
    );
  }
  const w = 84;
  const h = 22;
  const peak = Math.max(...pts, 1);
  /* Inset by the stroke's own half-width at both ends: a trace drawn hard on
     the viewBox edge gets half of itself shaved off. */
  const inset = 1;
  const step = (w - inset * 2) / (pts.length - 1);
  const d = pts
    .map(
      (v, i) =>
        `${i === 0 ? "M" : "L"}${(inset + i * step).toFixed(1)},${(h - 2.5 - (v / peak) * (h - 6)).toFixed(1)}`,
    )
    .join("");
  return (
    <svg
      className="mf-trace"
      width={w}
      height={h}
      viewBox={`0 0 ${w} ${h}`}
      role="img"
      aria-label={`${label}: ${pts.length} intervals, peak ${num(peak)} in one interval`}
    >
      <line
        x1={0}
        x2={w}
        y1={h - 1.5}
        y2={h - 1.5}
        stroke="var(--mf-rule)"
        strokeWidth={1}
        shapeRendering="crispEdges"
      />
      <path
        d={d}
        fill="none"
        stroke="var(--mf-well)"
        strokeWidth={2.2}
        transform="translate(0,1)"
        opacity={0.85}
      />
      <path d={d} fill="none" stroke="var(--mf-oxide)" strokeWidth={1.3} />
    </svg>
  );
}

/* ── one facet control ───────────────────────────────────────────────────── */

function Facet({
  label,
  anyLabel,
  value,
  options,
  onChange,
  format,
  waiting,
}: {
  label: string;
  anyLabel: string;
  value: string;
  options: string[];
  onChange: (v: string) => void;
  format?: (v: string) => string;
  waiting?: boolean;
}) {
  /* A control with nothing to choose is disabled and says why, rather than
     opening onto an empty list. */
  const empty = options.length === 0 && !value;
  return (
    <span className="mf-select" data-set={value ? "true" : "false"}>
      <select
        aria-label={label}
        value={value}
        disabled={empty}
        title={
          !empty
            ? undefined
            : waiting
              ? "Reading the record…"
              : `No ${label.toLowerCase()} values came back for this interval`
        }
        onChange={(e) => onChange(e.target.value)}
      >
        <option value="">{anyLabel}</option>
        {options.map((o) => (
          <option key={o} value={o}>
            {format ? format(o) : o}
          </option>
        ))}
      </select>
      <IconChevron size={12} className="mf-select__caret" />
    </span>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function ErrorsScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const { range, audience, filters } = useAppState();
  const { site } = useParams();
  const navigate = useNavigate();

  const [browser, setBrowser] = useState("");
  const [os, setOs] = useState("");
  const [pathname, setPathname] = useState("");
  const [release, setRelease] = useState("");
  const [limit, setLimit] = useState(PAGE);

  const activeFilters: Filter[] = filters ?? [];

  /* Browser, OS and page are real dimensions from CONTRACT.md §3.2, so they
     ride in the envelope's `filters`. `release` is a column on the errors table
     and not a global dimension, so it is passed to this endpoint by name. */
  const merged: Filter[] = useMemo(() => {
    const local: Filter[] = [];
    if (browser) local.push({ dimension: "browser", op: "is", value: browser });
    if (os) local.push({ dimension: "os", op: "is", value: os });
    if (pathname) local.push({ dimension: "pathname", op: "is", value: pathname });
    return [...activeFilters, ...local];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [JSON.stringify(activeFilters), browser, os, pathname]);

  const filtersJson = JSON.stringify(merged);
  const ready = siteId != null && !siteLoading;
  const facetsSet = Boolean(browser || os || pathname || release);

  const query = useQuery({
    queryKey: ["stats", "errors", siteId, range, audience, filtersJson, release, limit],
    queryFn: () =>
      api
        .get<ErrorsResponse | ErrorGroup[]>("/api/stats/errors", {
          site: siteId as number,
          range,
          audience: audience as Audience,
          filters: merged,
          release: release || undefined,
          limit,
        })
        .then((r) => asResponse(r.data)),
    enabled: ready,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });

  const data = query.data ?? { rows: [] };
  const rows = data.rows;

  const facetOptions = (key: ErrorFacetKey, fallback: string[]): string[] => {
    const served = data.facets?.[key];
    if (served?.length) return served.map((f) => f.value).filter(Boolean);
    return Array.from(new Set(fallback.filter(Boolean))).sort();
  };

  const browserOptions = facetOptions("browser", rows.flatMap((r) => r.browsers ?? []));
  const osOptions = facetOptions("os", []);
  const pathOptions = facetOptions(
    "pathname",
    rows.map((r) => r.top_path ?? ""),
  );
  const releaseOptions = facetOptions(
    "release",
    rows.map((r) => r.release ?? ""),
  );

  const occurrences = data.totals?.occurrences ?? rows.reduce((s, r) => s + (r.count || 0), 0);
  const groups = data.totals?.groups ?? rows.length;
  const unhandled = rows.filter((r) => r.handled === false).length;
  /* Without site-wide totals the strip can only count what it has loaded, and
     it says so rather than passing a page off as the whole. */
  const derived = data.totals?.groups == null;

  const channels: Channel[] = [
    {
      key: "groups",
      label: "Distinct errors",
      value: num(groups),
      note: derived ? "loaded so far" : undefined,
    },
    { key: "occurrences", label: "Occurrences", value: num(occurrences) },
    {
      key: "unhandled",
      label: "Unhandled",
      value: num(unhandled),
      note: "reached the page uncaught",
    },
  ];

  const columns: Column<ErrorGroup>[] = [
    {
      key: "error",
      head: "Error",
      width: "36%",
      render: (row) => (
        <span className="mf-err" data-state={row.status ?? "open"}>
          <span className="mf-err__line">
            <span className="mf-err__name">{row.name || "Error"}</span>
            {row.handled === false ? (
              <span className="mf-err__flag" data-flag="unhandled">
                unhandled
              </span>
            ) : null}
            {row.status && row.status !== "open" ? (
              <span className="mf-err__flag">{row.status}</span>
            ) : null}
          </span>
          <span className="mf-err__msg" title={row.message}>
            {row.message || "No message recorded"}
          </span>
        </span>
      ),
    },
    {
      key: "trend",
      head: "Trend",
      width: "96px",
      render: (row) => <ErrorTrace values={row.trend} label={row.name || "Error"} />,
    },
    {
      key: "count",
      head: "Occurrences",
      numeric: true,
      render: (row) => <span className="mf-data">{num(row.count)}</span>,
    },
    {
      key: "visitors",
      head: "Readers",
      numeric: true,
      render: (row) => <span className="mf-data">{num(row.visitors)}</span>,
    },
    {
      key: "first",
      head: "First seen",
      numeric: true,
      render: (row) => <span className="mf-data">{ago(row.first_seen)}</span>,
    },
    {
      key: "last",
      head: "Last seen",
      numeric: true,
      render: (row) => <span className="mf-data">{ago(row.last_seen)}</span>,
    },
  ];

  const clearFacets = () => {
    setBrowser("");
    setOs("");
    setPathname("");
    setRelease("");
    setLimit(PAGE);
  };

  const openDetail = (row: ErrorGroup) =>
    navigate(`/app/${site ?? ""}/errors/${encodeURIComponent(row.fingerprint)}`);

  return (
    <div className="mf-hs">
      <h1 className="mf-sr">Errors</h1>

      {audience === "agent" ? (
        <p className="mf-hs-note">
          <IconInfo size={16} />
          <span>
            The tracker runs in browsers, so JavaScript errors are only recorded for readers. What
            the machines were served, status codes included, is on Agent traffic.
          </span>
        </p>
      ) : null}

      <Panel feed>
        <PanelHead
          title="Errors"
          note="Grouped by fingerprint: one row is one distinct failure, however many times it fired."
        />

        <div className="mf-hs-strip">
          <Facet
            label="Browser"
            anyLabel="Any browser"
            value={browser}
            options={browserOptions}
            onChange={setBrowser}
            waiting={query.isPending}
          />
          <Facet
            label="Operating system"
            anyLabel="Any system"
            value={os}
            options={osOptions}
            onChange={setOs}
            waiting={query.isPending}
          />
          <Facet
            label="Page"
            anyLabel="Any page"
            value={pathname}
            options={pathOptions}
            onChange={setPathname}
            format={(v) => shortPath(v, 32)}
            waiting={query.isPending}
          />
          <Facet
            label="Release"
            anyLabel="Any release"
            value={release}
            options={releaseOptions}
            onChange={setRelease}
            waiting={query.isPending}
          />
          {facetsSet ? (
            <Button tone="quiet" size="sm" onClick={clearFacets}>
              Clear
            </Button>
          ) : null}
        </div>

        {query.isError ? (
          <ErrorState
            title="The errors did not come back"
            detail={reason(query.error)}
            onRetry={() => void query.refetch()}
          />
        ) : (
          <>
            {query.isPending || siteLoading ? (
              <ReadoutWaiting count={3} />
            ) : (
              <Readout channels={channels} dense />
            )}
            <Sheet
              rows={rows}
              columns={columns}
              getKey={(row, i) => row.fingerprint || `row-${i}`}
              loading={query.isPending || siteLoading}
              onRowClick={openDetail}
              empty={
                facetsSet || activeFilters.length ? (
                  <Empty
                    title="No errors match this filter"
                    body="Clear a filter or widen the interval. Nothing was reported for this exact cut."
                    action={
                      facetsSet ? <Button onClick={clearFacets}>Clear these filters</Button> : undefined
                    }
                  />
                ) : (
                  <Empty
                    title="Nothing has broken in this interval"
                    body="Errors arrive on the tracker's error event. If you have never seen one here, confirm error capture is switched on in the snippet."
                  />
                )
              }
            />
            {rows.length >= limit ? (
              <div className="mf-hs-more">
                <Button
                  disabled={query.isFetching}
                  onClick={() => setLimit((n) => n + PAGE)}
                >
                  {query.isFetching ? "Reading…" : `Read ${PAGE} more`}
                </Button>
              </div>
            ) : null}
          </>
        )}
      </Panel>
    </div>
  );
}

export default ErrorsScreen;
