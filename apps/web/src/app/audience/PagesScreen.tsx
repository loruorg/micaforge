/**
 * Pages: what was read, where reading started, and where it stopped.
 *
 * The loop this screen is built around is *filtering in place*. Clicking a row
 * does not navigate anywhere; it adds that path as a filter and every panel on
 * every screen reloads through it. Navigating away and back is the
 * slow loop, and an analytics tool lives or dies on the fast one.
 *
 * The area's shared helpers also live in this file, co-located with the screen
 * that owns the filter loop, and imported by the other three audience screens.
 */
import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import type { UseQueryResult } from "@tanstack/react-query";

import {
  AudienceSwitch,
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  RangePicker,
  Readout,
  Sheet,
} from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import { IconClose, IconDownload, IconInfo } from "../../components/react/icons";
import { api, ApiError } from "../../lib/api";
import type { Audience, Envelope, Filter, Overview } from "../../lib/api";
import { compact, delta, duration, num, pct } from "../../lib/format";
import { useAppState } from "../state";
import { useSite } from "../providers";
import "../../styles/app/audience.css";

/* ══ the area's shared helpers ═════════════════════════════════════════════ */

/**
 * A row as any of the audience reports returns it. Everything past `key` is
 * optional on purpose: a machine fetch has a path but no bounce rate, and a
 * screen that assumed otherwise would print a confident zero where the honest
 * answer is a dash.
 */
export interface AudienceRow {
  key: string;
  label?: string;
  visitors?: number | null;
  sessions?: number | null;
  pageviews?: number | null;
  bounce_rate?: number | null;
  avg_duration_s?: number | null;
  share?: number | null;
  [extra: string]: unknown;
}

export interface PageRow extends AudienceRow {
  entries?: number | null;
  exits?: number | null;
  exit_rate?: number | null;
  /** Average furthest scroll, 0–100, matching the `scroll_pct` column. */
  avg_scroll_pct?: number | null;
}

/** Reads a numeric field off a tolerant row without inventing a value. */
export function n(row: AudienceRow, key: string): number | null {
  const v = row[key];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
}

/** A count, or an honest dash. */
export const count = (v: number | null | undefined): string =>
  v == null ? "—" : num(v);

/** A rate the API reports as a fraction of one. */
export const ratio = (v: number | null | undefined): string =>
  v == null ? "—" : pct(v);

/** A rate the API reports in whole percent, as `scroll_pct` is stored. */
export const percentOf100 = (v: number | null | undefined): string =>
  v == null ? "—" : pct(v / 100);

export const secs = (v: number | null | undefined): string =>
  v == null ? "—" : duration(v);

/** Turns whatever came back into a sentence an operator can act on. */
export function errorDetail(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === "network")
      return "The dashboard could not reach the Micaforge server. Check that the backend is running, then try again.";
    return err.message;
  }
  return "Something went wrong while loading this data. Try again.";
}

/** The standard query envelope for an audience read. */
export function useEnvelope(extra?: Record<string, unknown>): {
  env: Envelope;
  siteId: number | string | undefined;
  audience: Audience;
  range: string;
  filtersJson: string;
  ready: boolean;
} {
  const { range, audience, filters } = useAppState();
  const { siteId, timezone } = useSite();
  const filtersJson = JSON.stringify(filters ?? []);
  const env = useMemo<Envelope>(
    () => ({
      site: siteId as number,
      range,
      audience,
      filters: (filters ?? []) as Filter[],
      ...(timezone ? { tz: timezone } : {}),
      ...(extra ?? {}),
    }),
    // `extra` is spread by value; stringifying keeps the identity stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [siteId, range, audience, filtersJson, timezone, JSON.stringify(extra ?? {})],
  );
  return {
    env,
    siteId,
    audience: audience as Audience,
    range,
    filtersJson,
    ready: siteId != null && String(siteId) !== "",
  };
}

/**
 * Every audience read goes through here so the cache key keeps one shape:
 * ["stats", endpoint, siteId, range, audience, filtersJson]. Variants of an
 * endpoint (a tab, a pivot, a drill level) ride in the endpoint slot.
 */
export function useAudienceQuery<T>(
  endpoint: string,
  path: string,
  env: Envelope,
  ready: boolean,
): UseQueryResult<{ rows: T[]; total: number | undefined }> {
  const { site, range, audience, filters } = env;
  return useQuery({
    queryKey: ["stats", endpoint, site, range, audience, JSON.stringify(filters ?? [])],
    enabled: ready,
    staleTime: 30_000,
    queryFn: async () => {
      const res = await api.get<T[] | { rows: T[] }>(path, env);
      const rows = Array.isArray(res.data) ? res.data : (res.data?.rows ?? []);
      return { rows, total: res.meta?.rows };
    },
  });
}

/** Range and audience, the two controls every audience screen carries. */
export function AudienceBar({ extra }: { extra?: ReactNode }) {
  const { range, setRange, audience, setAudience } = useAppState();
  return (
    <div className="mfa-bar">
      <RangePicker value={range} onChange={setRange} />
      <AudienceSwitch value={audience} onChange={setAudience} />
      {extra}
    </div>
  );
}

/**
 * A CSV of exactly what is on screen, envelope and all.
 *
 * This one exports THIS table, at the dimension it is grouped by, which is why
 * it sits in the panel head rather than in the top bar. It uses the same words
 * as every other table export, because one control under two names reads as
 * two controls.
 */
export function ExportKey({ report, env }: { report: string; env: Envelope }) {
  return (
    <a
      className="mf-btn mf-btn--key mf-btn--sm"
      href={api.exportUrl("/api/stats/export.csv", { ...env, report })}
      download
    >
      <span className="mf-btn__icon">
        <IconDownload size={14} />
      </span>
      <span className="mf-btn__label">Export CSV</span>
    </a>
  );
}

const DIMENSION_NAMES: Record<string, string> = {
  pathname: "Path",
  entry_path: "Entry path",
  exit_path: "Exit path",
  page_title: "Page title",
  hostname: "Host",
  channel: "Channel",
  source: "Source",
  referrer_host: "Referrer",
  utm_source: "utm_source",
  utm_medium: "utm_medium",
  utm_campaign: "utm_campaign",
  utm_term: "utm_term",
  utm_content: "utm_content",
  country: "Country",
  region: "Region",
  city: "City",
  browser: "Browser",
  browser_version: "Browser version",
  os: "OS",
  os_version: "OS version",
  device_type: "Device",
  screen_class: "Screen",
  language: "Language",
};
export const dimensionName = (d: string): string => DIMENSION_NAMES[d] ?? d;

const OP_WORDS: Record<string, string> = {
  is: "is",
  is_not: "is not",
  contains: "contains",
  not_contains: "excludes",
  starts_with: "starts with",
  ends_with: "ends with",
  matches: "matches",
  is_set: "is set",
  is_not_set: "is not set",
  gt: ">",
  lt: "<",
  gte: "≥",
  lte: "≤",
};

/**
 * The filters that apply now. Each one names its dimension and removes
 * itself; nothing here is decorative.
 */
export function FilterTrail() {
  const { filters, removeFilter, clearFilters } = useAppState();
  const list = (filters ?? []) as Filter[];
  if (!list.length) return null;
  // The store exposes the remover by position; the trail is the only place
  // that needs it, so the coercion is contained here.
  const removeAt = removeFilter as unknown as (index: number) => void;

  return (
    <div className="mfa-trail">
      <span className="mf-stamp">Filtered by</span>
      {list.map((f, i) => (
        <span className="mfa-clip" key={`${f.dimension}-${f.op}-${String(f.value)}-${i}`}>
          <span className="mfa-clip__dim">{dimensionName(f.dimension)}</span>
          <span className="mfa-clip__op">{OP_WORDS[f.op] ?? f.op}</span>
          <span className="mfa-clip__val">
            {Array.isArray(f.value) ? f.value.join(", ") : String(f.value)}
          </span>
          <button
            type="button"
            className="mfa-clip__drop"
            onClick={() => removeAt(i)}
            aria-label={`Remove the ${dimensionName(f.dimension)} filter`}
          >
            <IconClose size={12} />
          </button>
        </span>
      ))}
      <Button tone="quiet" size="sm" onClick={() => clearFilters()}>
        Clear all filters
      </Button>
    </div>
  );
}

/** Something the screen knows about its own data and says out loud. */
export function Caveat({ children }: { children: ReactNode }) {
  return (
    <div className="mfa-caveat">
      <IconInfo size={15} />
      <p>{children}</p>
    </div>
  );
}

/** The metric strip while the totals load. */
export function ReadoutWait({ channels }: { channels: number }) {
  return (
    <div className="mf-readout" aria-busy="true" aria-live="polite">
      <span className="mf-sr">Loading…</span>
      {Array.from({ length: channels }).map((_, i) => (
        <div className="mf-readout__chan" key={i}>
          <span className="mf-wait" style={{ width: "52%" }} />
          <span className="mf-wait" style={{ width: `${44 + ((i * 13) % 30)}%`, height: "26px" }} />
          <span className="mf-wait" style={{ width: "34%" }} />
        </div>
      ))}
    </div>
  );
}

/* ══ the screen ═══════════════════════════════════════════════════════════ */

type TabId = "pathname" | "entry_path" | "exit_path" | "page_title";

const TABS: { id: TabId; label: string; note: string }[] = [
  {
    id: "pathname",
    label: "Pages",
    note: "Every page viewed in this period, and how far down people scrolled.",
  },
  {
    id: "entry_path",
    label: "Entry pages",
    note: "Where visits started. These pages make the first impression of the site.",
  },
  {
    id: "exit_path",
    label: "Exit pages",
    note: "Where visits ended. A high exit rate is only a problem on a page meant to lead somewhere.",
  },
  {
    id: "page_title",
    label: "Page titles",
    note: "The same traffic grouped by title rather than path, which is useful where one template serves many paths.",
  },
];

export function PagesScreen() {
  const [tab, setTab] = useState<TabId>("pathname");
  const { addFilter, audience } = useAppState();
  const { env, ready } = useEnvelope({ dimension: tab });
  const active = TABS.find((t) => t.id === tab)!;
  const machines = audience === "agent";

  const rowsQ = useAudienceQuery<PageRow>(`pages:${tab}`, "/api/stats/pages", env, ready);

  const overviewEnv = useMemo<Envelope>(
    () => ({ ...env, dimension: undefined, compare: "previous" as const }),
    [env],
  );
  const totals = useQuery({
    queryKey: [
      "stats",
      "overview:pages",
      env.site,
      env.range,
      env.audience,
      JSON.stringify(env.filters ?? []),
    ],
    enabled: ready,
    staleTime: 30_000,
    queryFn: async () => (await api.get<Overview>("/api/stats/overview", overviewEnv)).data,
  });

  const channels = useMemo<Channel[]>(() => {
    const o = totals.data;
    const before = o?.compare ?? null;
    return [
      {
        key: "pageviews",
        label: "Pageviews",
        value: count(o?.pageviews),
        delta: o ? delta(o.pageviews, before?.pageviews) : null,
      },
      {
        key: "visitors",
        label: machines ? "Crawlers" : "Visitors",
        value: count(o?.visitors),
        delta: o ? delta(o.visitors, before?.visitors) : null,
      },
      {
        key: "vps",
        label: "Views per visit",
        value: o?.views_per_session == null ? "—" : o.views_per_session.toFixed(2),
        delta: o ? delta(o.views_per_session, before?.views_per_session) : null,
      },
      {
        key: "bounce",
        label: "Bounce rate",
        value: percentOf100(o?.bounce_rate),
        delta: o ? delta(o.bounce_rate, before?.bounce_rate) : null,
        inverse: true,
        note: machines ? "people only" : undefined,
      },
      {
        key: "visit",
        label: "Average visit",
        value: secs(o?.avg_duration_s),
        delta: o ? delta(o.avg_duration_s, before?.avg_duration_s) : null,
      },
    ];
  }, [totals.data, machines]);

  const rows = rowsQ.data?.rows ?? [];
  const share = (r: PageRow) => n(r, "share") ?? undefined;
  const label = (r: PageRow) => r.label ?? r.key;

  const columns = useMemo<Column<PageRow>[]>(() => {
    const pathCell: Column<PageRow> = {
      key: "path",
      head: tab === "page_title" ? "Title" : "Path",
      width: "34%",
      share,
      render: (r) =>
        tab === "page_title" ? (
          // A title is prose, so it is set in the UI face.
          <span className="mfa-name" title={label(r)}>
            {label(r) || "Untitled"}
          </span>
        ) : (
          // A path is genuine data, so it stays in the data face, and the whole
          // path is on the element even where the cell has to clip it.
          <span title={r.key}>{r.key || "/"}</span>
        ),
    };
    const pageviews: Column<PageRow> = {
      key: "pageviews",
      head: machines ? "Fetches" : "Pageviews",
      numeric: true,
      render: (r) => count(n(r, "pageviews")),
    };
    const visitors: Column<PageRow> = {
      key: "visitors",
      head: machines ? "Crawlers" : "Visitors",
      numeric: true,
      render: (r) => count(n(r, "visitors")),
    };
    const entries: Column<PageRow> = {
      key: "entries",
      head: "Entries",
      numeric: true,
      render: (r) => count(n(r, "entries")),
    };
    const exits: Column<PageRow> = {
      key: "exits",
      head: "Exits",
      numeric: true,
      render: (r) => count(n(r, "exits")),
    };
    const bounce: Column<PageRow> = {
      key: "bounce",
      head: "Bounce",
      numeric: true,
      render: (r) => percentOf100(n(r, "bounce_rate")),
    };
    const time: Column<PageRow> = {
      key: "time",
      head: "Avg time",
      numeric: true,
      render: (r) => secs(n(r, "avg_duration_s")),
    };
    const scroll: Column<PageRow> = {
      key: "scroll",
      head: "Avg scroll",
      numeric: true,
      render: (r) => percentOf100(n(r, "avg_scroll_pct")),
    };
    const exitRate: Column<PageRow> = {
      key: "exit_rate",
      head: "Exit rate",
      numeric: true,
      render: (r) => ratio(n(r, "exit_rate")),
    };

    if (tab === "entry_path")
      return [pathCell, entries, visitors, bounce, time, scroll];
    if (tab === "exit_path") return [pathCell, exits, exitRate, pageviews, time, scroll];
    if (tab === "page_title") return [pathCell, pageviews, visitors, time, scroll];
    return [pathCell, pageviews, visitors, entries, exits, bounce, time, scroll];
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab, machines]);

  const clip = (r: PageRow) =>
    addFilter({ dimension: tab, op: "is", value: r.key } as Filter);

  return (
    /* The share bar behind a row takes the colour of the audience on screen:
       blue for people, orange for AI crawlers. */
    <div className="mfa-screen" data-trace={machines ? "agent" : "human"}>
      <Panel>
        <PanelHead
          title="Pages"
          note={active.note}
          actions={<AudienceBar extra={<ExportKey report={`pages:${tab}`} env={env} />} />}
        />

        {totals.isError ? (
          <ErrorState
            title="Could not load the totals"
            detail={errorDetail(totals.error)}
            onRetry={() => void totals.refetch()}
          />
        ) : totals.isLoading ? (
          <ReadoutWait channels={5} />
        ) : (
          <Readout channels={channels} />
        )}

        <div className="mfa-tabs" role="group" aria-label="Page report">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              className="mfa-tabs__key"
              aria-pressed={tab === t.id}
              onClick={() => setTab(t.id)}
            >
              {t.label}
            </button>
          ))}
        </div>

        <FilterTrail />

        {machines ? (
          <Caveat>
            You are looking at AI crawler traffic. A crawler fetches a URL and leaves, so it
            has no session, bounce, scroll depth or time on page. Those columns stay empty
            rather than guess. Switch to People for visitor figures.
          </Caveat>
        ) : null}

        {rowsQ.isError ? (
          <ErrorState
            title={`Could not load ${active.label.toLowerCase()}`}
            detail={errorDetail(rowsQ.error)}
            onRetry={() => void rowsQ.refetch()}
          />
        ) : (
          <Sheet<PageRow>
            rows={rows}
            columns={columns}
            loading={rowsQ.isLoading}
            getKey={(r, i) => `${r.key}-${i}`}
            onRowClick={clip}
            empty={
              <Empty
                title={
                  tab === "page_title"
                    ? "No page titles in this period"
                    : "No pageviews in this period"
                }
                body={
                  <>
                    A page appears here as soon as the tracker reports its first hit
                    {(env.filters as Filter[] | undefined)?.length
                      ? ". The filters above may exclude them; try clearing one."
                      : ". Choose a longer period, or check that the tracker is installed."}
                  </>
                }
              />
            }
          />
        )}

        {rows.length ? (
          <div className="mfa-foot">
            <span>
              Click a row to filter by that {tab === "page_title" ? "title" : "path"}.
              Every screen then uses the filter.
            </span>
            <span className="mf-data">
              {compact(rowsQ.data?.total ?? rows.length)} rows
            </span>
          </div>
        ) : null}
      </Panel>
    </div>
  );
}

export default PagesScreen;
