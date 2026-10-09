/**
 * Search, at /app/:site/search
 *
 * What people searched on Google before they arrived, from Google Search
 * Console: one question per panel, answered from Google's own figures.
 *
 * The rules this screen keeps:
 *
 *   - GOOGLE'S NUMBERS, SAID AS GOOGLE'S. Clicks, impressions, click-through
 *     rate and average position are Search Console's figures. Micaforge's own
 *     count appears in one place, beside each page, under its own heading.
 *   - EACH FIGURE FROM WHERE GOOGLE COUNTS IT. The totals are the property's
 *     own, so they match the Search Console screen; the search-term table adds
 *     up to less, because Google leaves out rare terms, and its note says so.
 *   - WHOLE DAYS. Google reports days in Pacific time and revises the last
 *     three; the chart's note says which days are still provisional.
 *   - PEOPLE ARE BLUE. Search traffic is people, so the one series is the
 *     people blue, drawn solid.
 *
 * Two tabs: the report, and Settings (connect, choose a property, sync,
 * disconnect). Until a property is chosen, the report tab says what to do
 * next instead of drawing empty panels.
 */
import { useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";

import {
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
import { api } from "../../lib/api";
import type { Envelope } from "../../lib/api";
import { compact, num, pct, shortPath } from "../../lib/format";
import { againstLeader } from "../display";
import { useSite } from "../providers";
import { rangeWords, useAppState } from "../state";
import { Bank, Say, TextInput, WaitLines, useDebounced } from "../growth/controls";
import { ChartWaiting, LineChart } from "../growth/LineChart";
import { countryName, problem } from "../growth/model";
import { ConnectButton, NotConfigured, SearchSettings, SyncNow } from "./SearchSettings";
import {
  CONNECT_ERRORS,
  METRICS,
  dayTime,
  dayWords,
  deviceName,
  envelopeOf,
  positionText,
  propertyName,
} from "./model";
import type {
  SearchMetric,
  SearchOverview,
  SearchPageRow,
  SearchPoint,
  SearchRow,
  SearchStatus,
} from "./model";

type Tab = "report" | "settings";

const fig = (n: number) => <span className="mf-data">{num(n)}</span>;

/** The four columns every ranked table shares. */
function figureColumns<T extends SearchRow>(): Column<T>[] {
  return [
    { key: "clicks", head: "Clicks", numeric: true, render: (r) => fig(r.clicks) },
    { key: "impressions", head: "Impressions", numeric: true, render: (r) => fig(r.impressions) },
    { key: "ctr", head: "CTR", numeric: true, render: (r) => <span className="mf-data">{pct(r.ctr, 1)}</span> },
    {
      key: "position",
      head: "Position",
      numeric: true,
      render: (r) => <span className="mf-data">{positionText(r.position)}</span>,
    },
  ];
}

/* ── headline and chart ──────────────────────────────────────────────────── */

function Headline({ env, rangeText }: { env: Envelope; rangeText: string }) {
  const [metric, setMetric] = useState<SearchMetric>("clicks");
  const overview = useQuery({
    queryKey: ["search", "overview", env],
    queryFn: () =>
      api.get<SearchOverview>("/api/stats/search/overview", { ...env, compare: "previous" }).then((r) => r.data),
    placeholderData: keepPreviousData,
  });
  const series = useQuery({
    queryKey: ["search", "timeseries", env],
    queryFn: () => api.get<SearchPoint[]>("/api/stats/search/timeseries", env).then((r) => r.data),
    placeholderData: keepPreviousData,
  });

  if (overview.isPending) {
    return (
      <Panel>
        <div className="mf-gr-pad">
          <WaitLines lines={2} label="Loading Google Search figures…" />
          <ChartWaiting />
        </div>
      </Panel>
    );
  }
  if (overview.isError) {
    return (
      <Panel>
        <ErrorState
          title="Could not load Google Search figures"
          detail={problem(overview.error)}
          onRetry={() => void overview.refetch()}
        />
      </Panel>
    );
  }

  const o = overview.data;
  const change = (v: number | null | undefined) => (v == null ? null : v / 100);
  const channels: Channel[] = [
    {
      key: "clicks",
      label: "Clicks",
      value: num(o.totals.clicks),
      delta: change(o.change?.clicks),
      active: metric === "clicks",
      onSelect: () => setMetric("clicks"),
    },
    {
      key: "impressions",
      label: "Impressions",
      value: compact(o.totals.impressions),
      delta: change(o.change?.impressions),
      active: metric === "impressions",
      onSelect: () => setMetric("impressions"),
    },
    {
      key: "ctr",
      label: "Click-through rate",
      value: pct(o.totals.ctr, 1),
      delta: change(o.change?.ctr),
      active: metric === "ctr",
      onSelect: () => setMetric("ctr"),
    },
    {
      key: "position",
      label: "Average position",
      value: positionText(o.totals.position),
      delta: change(o.change?.position),
      inverse: true,
      note: "1 is the top",
      active: metric === "position",
      onSelect: () => setMetric("position"),
    },
  ];

  const points = series.data ?? [];
  const label = METRICS.find((m) => m.v === metric)?.label ?? "Clicks";
  const format = (n: number) =>
    metric === "ctr" ? pct(n, 1) : metric === "position" ? positionText(n) : num(n);
  const formatAxis = (n: number) =>
    metric === "ctr" ? pct(n) : metric === "position" ? n.toFixed(0) : compact(n);
  const empty = o.first_day == null;

  return (
    <Panel>
      <PanelHead
        title="Google Search"
        note={`${rangeText}, against the period before. Google's own totals for the property, the same figures Search Console shows.`}
      />
      <Readout channels={channels} dense />
      <div className="mf-gr-pad">
        {empty ? (
          <Empty
            title="No Google Search data for this range"
            body="Google has reported nothing for these days yet. A new connection fills the last 30 days first, then older months; Google publishes each day a day or two late."
          />
        ) : series.isError ? (
          <ErrorState
            title="Could not load the daily figures"
            detail={problem(series.error)}
            onRetry={() => void series.refetch()}
          />
        ) : series.isPending ? (
          <ChartWaiting />
        ) : (
          <>
            <LineChart
              label={`${label} per day from Google Search, ${rangeText.toLowerCase()}`}
              times={points.map((p) => dayTime(p.date))}
              format={format}
              formatAxis={formatAxis}
              integer={metric === "clicks" || metric === "impressions"}
              series={[
                {
                  key: metric,
                  label,
                  color: "var(--mf-people)",
                  values: points.map((p) => p[metric]),
                },
              ]}
            />
            <p className="mf-gr-aside">
              Days from {dayWords(o.provisional_from)} on are provisional: Google may still revise them, and Micaforge
              reads them again each day.{metric === "position" ? " A lower position is better." : ""}
            </p>
          </>
        )}
      </div>
    </Panel>
  );
}

/* ── search terms ────────────────────────────────────────────────────────── */

function Queries({ env }: { env: Envelope }) {
  const [text, setText] = useState("");
  const [sort, setSort] = useState<"clicks" | "impressions">("clicks");
  const q = useDebounced(text.trim(), 300);
  const rows = useQuery({
    queryKey: ["search", "queries", env, q, sort],
    queryFn: () =>
      api
        .get<SearchRow[]>("/api/stats/search/queries", { ...env, q: q || undefined, sort, limit: 50 })
        .then((r) => r.data),
    placeholderData: keepPreviousData,
  });
  const data = rows.data ?? [];
  const columns: Column<SearchRow>[] = [
    {
      key: "query",
      head: "Search term",
      share: againstLeader(data, (r) => (sort === "clicks" ? r.clicks : r.impressions)),
      render: (r) => <span title={r.key}>{r.key}</span>,
    },
    ...figureColumns<SearchRow>(),
  ];
  return (
    <Panel>
      <PanelHead
        title="What people searched"
        note="The search terms Google showed the site for. Google leaves out rare terms it keeps private, so these add up to less than the totals above. Sort by impressions to find terms the site shows up for but rarely gets the click."
      />
      <div className="mf-gr-strip">
        <label className="mf-sr" htmlFor="mf-sc-q">
          Find a search term
        </label>
        <TextInput
          id="mf-sc-q"
          className="mf-sc-find"
          placeholder="Find a search term"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <Bank<"clicks" | "impressions">
          label="Rank search terms by"
          value={sort}
          options={[
            { v: "clicks", label: "Most clicks" },
            { v: "impressions", label: "Most impressions" },
          ]}
          onChange={setSort}
        />
      </div>
      {rows.isError ? (
        <ErrorState title="Could not load search terms" detail={problem(rows.error)} onRetry={() => void rows.refetch()} />
      ) : (
        <Sheet<SearchRow>
          rows={data}
          columns={columns}
          loading={rows.isPending}
          getKey={(r, i) => `${r.key}-${i}`}
          empty={
            <Empty
              title={q ? "No search term matches" : "No search terms in this range"}
              body={q ? "No search term in this range contains that text." : "Google reported no search term for these days. Choose a longer range."}
            />
          }
        />
      )}
    </Panel>
  );
}

/* ── pages ───────────────────────────────────────────────────────────────── */

function Pages({ env }: { env: Envelope }) {
  const rows = useQuery({
    queryKey: ["search", "pages", env],
    queryFn: () => api.get<SearchPageRow[]>("/api/stats/search/pages", { ...env, limit: 25 }).then((r) => r.data),
    placeholderData: keepPreviousData,
  });
  const data = rows.data ?? [];
  const ours = (n: number | null) => <span className="mf-data">{n == null ? "—" : num(n)}</span>;
  const columns: Column<SearchPageRow>[] = [
    {
      key: "page",
      head: "Page",
      share: againstLeader(data, (r) => r.clicks),
      render: (r) => (
        <span className="mf-data" title={r.key}>
          {shortPath(r.pathname || r.key, 48)}
        </span>
      ),
    },
    ...figureColumns<SearchPageRow>(),
    { key: "visitors", head: "Visitors", numeric: true, render: (r) => ours(r.visitors) },
    { key: "from_google", head: "From Google", numeric: true, render: (r) => ours(r.from_google) },
  ];
  return (
    <Panel>
      <PanelHead
        title="Pages that showed up"
        note="Google's clicks and impressions per page, and beside them Micaforge's own count for the same page over the same days: visitors, and visitors who came from Google Search. A dash means Micaforge counted no pageview there."
      />
      {rows.isError ? (
        <ErrorState title="Could not load pages" detail={problem(rows.error)} onRetry={() => void rows.refetch()} />
      ) : (
        <Sheet<SearchPageRow>
          rows={data}
          columns={columns}
          loading={rows.isPending}
          getKey={(r, i) => `${r.key}-${i}`}
          empty={<Empty title="No pages in this range" body="Google reported no page of the site for these days." />}
        />
      )}
    </Panel>
  );
}

/* ── countries and devices ───────────────────────────────────────────────── */

function Split({ env, which }: { env: Envelope; which: "countries" | "devices" }) {
  const rows = useQuery({
    queryKey: ["search", which, env],
    queryFn: () => api.get<SearchRow[]>(`/api/stats/search/${which}`, { ...env, limit: 15 }).then((r) => r.data),
    placeholderData: keepPreviousData,
  });
  const data = rows.data ?? [];
  const columns: Column<SearchRow>[] = [
    {
      key: "key",
      head: which === "countries" ? "Country" : "Device",
      share: againstLeader(data, (r) => r.clicks),
      render: (r) => <span>{which === "countries" ? countryName(r.key) : deviceName(r.key)}</span>,
    },
    { key: "clicks", head: "Clicks", numeric: true, render: (r) => fig(r.clicks) },
    { key: "impressions", head: "Impressions", numeric: true, render: (r) => fig(r.impressions) },
    { key: "ctr", head: "CTR", numeric: true, render: (r) => <span className="mf-data">{pct(r.ctr, 1)}</span> },
  ];
  const title = which === "countries" ? "Where searchers were" : "What searchers used";
  return (
    <Panel>
      <PanelHead title={title} />
      {rows.isError ? (
        <ErrorState title={`Could not load ${which}`} detail={problem(rows.error)} onRetry={() => void rows.refetch()} />
      ) : (
        <Sheet<SearchRow>
          rows={data}
          columns={columns}
          loading={rows.isPending}
          getKey={(r, i) => `${r.key}-${i}`}
          empty={<Empty title="Nothing in this range" body="Google reported no search for these days." />}
        />
      )}
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

/** The line under the bar: which property, how fresh, and "Sync now". */
function Freshness({ status, siteId }: { status: SearchStatus; siteId: number }) {
  const c = status.connection;
  if (!c || !c.property) return null;
  return (
    <div className="mf-sc-fresh">
      <p className="mf-gr-aside">
        From Search Console property <span className="mf-data">{propertyName(c.property)}</span>.{" "}
        {c.syncing
          ? "A sync is running."
          : c.synced_through
            ? `Data up to ${dayWords(c.synced_through)}.`
            : "The first sync has not finished yet."}
        {c.backfill_done || !c.backfilled_from ? "" : ` Older months are still being read, back to ${dayWords(c.backfill_until)}.`}
      </p>
      {c.status === "active" ? <SyncNow siteId={siteId} /> : null}
    </div>
  );
}

export function SearchScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const range = useAppState((s) => s.range);
  const setRange = useAppState((s) => s.setRange);
  const [params, setParams] = useSearchParams();
  const tab: Tab = params.get("tab") === "settings" ? "settings" : "report";
  const connected = params.get("gsc") === "connected";
  const failed = params.get("gsc_error");

  const env = useMemo(() => (siteId != null ? envelopeOf(siteId, range) : null), [siteId, range]);
  const rangeText = rangeWords(range);

  const status = useQuery({
    queryKey: ["search-console", "status", siteId],
    queryFn: () =>
      api.get<SearchStatus>(`/api/search-console?site=${encodeURIComponent(String(siteId))}`).then((r) => r.data),
    enabled: siteId != null,
    // While a sync runs, look again now and then so the page sees it finish.
    refetchInterval: (q) => (q.state.data?.connection?.syncing ? 15_000 : false),
  });

  const setTab = (next: Tab) => {
    const p = new URLSearchParams(params);
    if (next === "settings") p.set("tab", "settings");
    else p.delete("tab");
    p.delete("gsc");
    p.delete("gsc_error");
    setParams(p, { replace: true });
  };

  const heading = <h1 className="mf-sr">Search</h1>;

  if (siteLoading || siteId == null || env == null || status.isPending) {
    return (
      <div className="mf-gr">
        {heading}
        <Panel>
          <div className="mf-gr-pad">
            <WaitLines lines={3} label="Loading Search…" />
          </div>
        </Panel>
      </div>
    );
  }

  if (status.isError) {
    return (
      <div className="mf-gr">
        {heading}
        <Panel>
          <ErrorState
            title="Could not load Search"
            detail={problem(status.error)}
            onRetry={() => void status.refetch()}
          />
        </Panel>
      </div>
    );
  }

  const s = status.data;
  const c = s.connection;
  const ready = c?.property != null;

  const notice = connected ? (
    <Say tone="good">Google Search Console is connected. Choose the property this site reads below.</Say>
  ) : failed ? (
    <Say tone="warn">{CONNECT_ERRORS[failed] ?? CONNECT_ERRORS.provider}</Say>
  ) : null;

  const tabs = (
    <Bank<Tab>
      label="Search view"
      value={tab}
      options={[
        { v: "report", label: "Report" },
        { v: "settings", label: "Settings" },
      ]}
      onChange={setTab}
    />
  );

  if (tab === "settings") {
    return (
      <div className="mf-gr">
        {heading}
        <div className="mf-gr-bar">{tabs}</div>
        {notice}
        <SearchSettings siteId={siteId} status={s} />
      </div>
    );
  }

  if (!ready) {
    return (
      <div className="mf-gr">
        {heading}
        <div className="mf-gr-bar">{tabs}</div>
        {notice}
        {!s.configured && !c ? (
          <NotConfigured redirectUri={s.redirect_uri} />
        ) : (
          <Panel>
            <Empty
              title={c ? "Choose the property this site reads" : "Google Search Console is not connected"}
              body={
                c
                  ? "Google Search Console is connected. A site admin chooses which property this site reads, then the first sync starts."
                  : "Connect Search Console to see what people searched on Google before they arrived: search terms, pages, countries and devices, with clicks, impressions and position."
              }
              action={
                s.can_manage ? (
                  c ? (
                    <Button tone="primary" onClick={() => setTab("settings")}>
                      Choose a property
                    </Button>
                  ) : s.configured ? (
                    <ConnectButton siteId={siteId} />
                  ) : null
                ) : undefined
              }
            />
          </Panel>
        )}
      </div>
    );
  }

  return (
    <div className="mf-gr">
      {heading}
      <div className="mf-gr-bar">
        {tabs}
        <RangePicker value={range} onChange={setRange} />
      </div>
      {notice}
      {c?.status === "revoked" ? (
        <Say tone="warn">
          Google no longer accepts this site’s connection, so the figures below stop at the last sync.{" "}
          {s.can_manage ? "Connect again under Settings." : "Ask a site admin to connect again."}
        </Say>
      ) : null}
      <Freshness status={s} siteId={siteId} />
      <Headline env={env} rangeText={rangeText} />
      <Queries env={env} />
      <Pages env={env} />
      <div className="mf-sc-pair">
        <Split env={env} which="countries" />
        <Split env={env} which="devices" />
      </div>
    </div>
  );
}

export default SearchScreen;
