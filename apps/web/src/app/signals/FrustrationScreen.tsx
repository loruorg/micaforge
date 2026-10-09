/**
 * Frustration: where people clicked and nothing happened, clicked again and
 * again, or gave up on a form.
 *
 * Three questions, one panel each, in the order someone fixing a page asks
 * them: which pages, which elements on them, which forms. Every count is the
 * tracker's frustration plugin reporting what it saw (`data-frustration` on the
 * script tag); nothing here is estimated. A page's pageviews sit beside its
 * signals so ten rage clicks in a hundred views reads differently from ten in a
 * hundred thousand.
 *
 * An element is named by the short CSS selector the tracker built for it, in
 * the mono face, because it is code you search your source for. A form names
 * the last field people touched before they left, never anything they typed.
 *
 * Reads `GET /api/stats/frustration` under the standard query envelope.
 */
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api, ApiError } from "../../lib/api";
import { num, shortPath } from "../../lib/format";
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
import { ReadoutWaiting } from "../health/PerformanceScreen";
import { useAppState } from "../state";
import { useSite } from "../providers";
import { SamplingNote } from "./SamplingNote";
import "../../styles/app/signals.css";

/* ── what the endpoint returns ───────────────────────────────────────────── */

export interface FrustratedPage {
  pathname: string;
  rage_clicks: number;
  dead_clicks: number;
  abandoned_forms: number;
  visitors: number;
  pageviews: number;
}

export interface FrustratedElement {
  pathname: string;
  signal: "rage_click" | "dead_click";
  selector: string;
  count: number;
  visitors: number;
  clicks: number;
}

export interface AbandonedForm {
  pathname: string;
  form: string;
  abandons: number;
  visitors: number;
  last_fields: { field: string; count: number }[];
}

export interface FrustrationReport {
  rage_clicks: number;
  dead_clicks: number;
  abandoned_forms: number;
  visitors: number;
  pages: FrustratedPage[];
  elements: FrustratedElement[];
  forms: AbandonedForm[];
}

const EMPTY: FrustrationReport = {
  rage_clicks: 0,
  dead_clicks: 0,
  abandoned_forms: 0,
  visitors: 0,
  pages: [],
  elements: [],
  forms: [],
};

function reason(err: unknown): string | undefined {
  if (err instanceof ApiError) return err.message;
  return err instanceof Error ? err.message : undefined;
}

/** Signals per hundred pageviews, or a dash when the page had none in range. */
export function perHundred(signals: number, pageviews: number): string {
  if (!pageviews) return "—";
  const v = (signals / pageviews) * 100;
  return v >= 10 ? v.toFixed(0) : v.toFixed(1);
}

/**
 * The signal, as a word and a drawn mark: a rage click is three strokes, a
 * dead click one hollow ring. The word carries it without the mark, and the
 * mark without colour.
 */
function SignalLabel({ signal }: { signal: FrustratedElement["signal"] }) {
  const rage = signal === "rage_click";
  return (
    <span className="mf-fr-signal" data-signal={rage ? "rage" : "dead"}>
      <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true">
        {rage ? (
          <path d="M3 4v6M7 2.5v9M11 4v6" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
        ) : (
          <circle cx="7" cy="7" r="4" fill="none" stroke="currentColor" strokeWidth="1.4" />
        )}
      </svg>
      {rage ? "Rage click" : "Dead click"}
    </span>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function FrustrationScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const { range, audience, filters, toEnvelope } = useAppState();
  const ready = siteId != null && !siteLoading;
  const filtersJson = JSON.stringify(filters ?? []);

  const query = useQuery({
    queryKey: ["stats", "frustration", siteId, range, audience, filtersJson],
    queryFn: () =>
      api
        .get<FrustrationReport>("/api/stats/frustration", toEnvelope(siteId as number))
        .then((r) => r.data ?? EMPTY),
    enabled: ready,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });

  const data = query.data ?? EMPTY;
  const waiting = query.isPending || siteLoading;
  const total = data.rage_clicks + data.dead_clicks + data.abandoned_forms;
  const peak = Math.max(1, ...data.pages.map((p) => p.rage_clicks + p.dead_clicks + p.abandoned_forms));

  const channels: Channel[] = [
    { key: "rage", label: "Rage clicks", value: num(data.rage_clicks), note: "bursts of 3+ clicks in one spot" },
    { key: "dead", label: "Dead clicks", value: num(data.dead_clicks), note: "clicks nothing answered" },
    { key: "forms", label: "Abandoned forms", value: num(data.abandoned_forms), note: "started, left unsent" },
    { key: "visitors", label: "Visitors affected", value: num(data.visitors) },
  ];

  const pageColumns: Column<FrustratedPage>[] = [
    {
      key: "page",
      head: "Page",
      width: "34%",
      render: (row) => (
        <span className="mf-data" title={row.pathname}>
          {shortPath(row.pathname, 48)}
        </span>
      ),
      share: (row) => (row.rage_clicks + row.dead_clicks + row.abandoned_forms) / peak,
    },
    { key: "rage", head: "Rage clicks", numeric: true, render: (row) => <span className="mf-data">{num(row.rage_clicks)}</span> },
    { key: "dead", head: "Dead clicks", numeric: true, render: (row) => <span className="mf-data">{num(row.dead_clicks)}</span> },
    { key: "forms", head: "Abandoned forms", numeric: true, render: (row) => <span className="mf-data">{num(row.abandoned_forms)}</span> },
    { key: "visitors", head: "Visitors", numeric: true, render: (row) => <span className="mf-data">{num(row.visitors)}</span> },
    {
      key: "rate",
      head: "Per 100 views",
      numeric: true,
      render: (row) => (
        <span className="mf-data" title={`${num(row.pageviews)} pageviews in this period`}>
          {perHundred(row.rage_clicks + row.dead_clicks + row.abandoned_forms, row.pageviews)}
        </span>
      ),
    },
  ];

  const elementColumns: Column<FrustratedElement>[] = [
    {
      key: "element",
      head: "Element",
      width: "38%",
      render: (row) => (
        <code className="mf-fr-selector" title={row.selector}>
          {row.selector || "Not named"}
        </code>
      ),
    },
    { key: "signal", head: "Signal", render: (row) => <SignalLabel signal={row.signal} /> },
    {
      key: "page",
      head: "Page",
      render: (row) => (
        <span className="mf-data" title={row.pathname}>
          {shortPath(row.pathname, 30)}
        </span>
      ),
    },
    {
      key: "count",
      head: "Times",
      numeric: true,
      render: (row) => (
        <span
          className="mf-data"
          title={row.signal === "rage_click" ? `${num(row.clicks)} clicks in these bursts` : undefined}
        >
          {num(row.count)}
        </span>
      ),
    },
    { key: "visitors", head: "Visitors", numeric: true, render: (row) => <span className="mf-data">{num(row.visitors)}</span> },
  ];

  const formColumns: Column<AbandonedForm>[] = [
    {
      key: "form",
      head: "Form",
      width: "26%",
      render: (row) => (
        <code className="mf-fr-selector" title={row.form}>
          {row.form || "Not named"}
        </code>
      ),
    },
    {
      key: "page",
      head: "Page",
      render: (row) => (
        <span className="mf-data" title={row.pathname}>
          {shortPath(row.pathname, 30)}
        </span>
      ),
    },
    {
      key: "last",
      head: "Last field touched",
      width: "34%",
      render: (row) => (
        <span className="mf-fr-fields">
          {row.last_fields.map((f, i) => (
            <span key={`${f.field}-${i}`} className="mf-fr-field">
              <code>{f.field || "unnamed"}</code>
              <span className="mf-data">{num(f.count)}</span>
            </span>
          ))}
        </span>
      ),
    },
    { key: "abandons", head: "Abandoned", numeric: true, render: (row) => <span className="mf-data">{num(row.abandons)}</span> },
    { key: "visitors", head: "Visitors", numeric: true, render: (row) => <span className="mf-data">{num(row.visitors)}</span> },
  ];

  const noneYet = (
    <Empty
      title="No frustration signals in this period"
      body="Rage clicks, dead clicks and abandoned forms are recorded once the script tag carries data-frustration. With it on, this stays empty only while nobody struggles."
    />
  );

  return (
    <div className="mf-fr">
      <h1 className="mf-sr">Frustration</h1>
      <SamplingNote />

      {audience === "agent" ? (
        <p className="mf-fr-note">
          <IconInfo size={16} />
          <span>
            Frustration signals come from the tracker in a browser, so they are recorded for
            people only. Switch the audience to people to read them.
          </span>
        </p>
      ) : null}

      <Panel>
        <PanelHead
          title="Frustration"
          note="Rage clicks, clicks nothing answered, and forms people started and left. Never a typed value."
        />
        {query.isError ? (
          <ErrorState
            title="Could not load frustration signals"
            detail={reason(query.error)}
            onRetry={() => void query.refetch()}
          />
        ) : waiting ? (
          <ReadoutWaiting count={4} />
        ) : (
          <Readout channels={channels} dense />
        )}
      </Panel>

      {query.isError ? null : (
        <>
          <Panel>
            <PanelHead title="Pages" note="The pages with the most signals. Per 100 views puts busy and quiet pages on one scale." />
            <Sheet
              rows={data.pages}
              columns={pageColumns}
              getKey={(row) => row.pathname}
              loading={waiting}
              empty={total === 0 ? noneYet : <Empty title="No pages with signals" />}
            />
          </Panel>

          <Panel>
            <PanelHead
              title="Elements"
              note="What people clicked. The name is the selector the tracker built from the element's tag, id and classes; search your code for it."
            />
            <Sheet
              rows={data.elements}
              columns={elementColumns}
              getKey={(row, i) => `${row.pathname}-${row.signal}-${row.selector}-${i}`}
              loading={waiting}
              empty={
                <Empty
                  title="No rage or dead clicks"
                  body="Nobody clicked in anger or clicked something that did nothing in this period."
                />
              }
            />
          </Panel>

          <Panel>
            <PanelHead
              title="Forms"
              note="Forms someone typed into and left without sending, with the field they touched last."
            />
            <Sheet
              rows={data.forms}
              columns={formColumns}
              getKey={(row, i) => `${row.pathname}-${row.form}-${i}`}
              loading={waiting}
              empty={
                <Empty
                  title="No abandoned forms"
                  body="Every form someone started in this period was sent, or nobody started one."
                />
              }
            />
          </Panel>
        </>
      )}
    </div>
  );
}

export default FrustrationScreen;
