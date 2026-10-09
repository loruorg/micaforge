/**
 * Revenue, at /app/:site/revenue
 *
 * Payments arrive from Stripe or Paddle by webhook, and each one is linked,
 * where it can be, to the visit that led to it. This screen answers one
 * question: how much money came in, and from where.
 *
 * The rules this screen keeps:
 *
 *   - ONE CURRENCY AT A TIME. A site that sells in dollars and euros has two
 *     revenues, not one: there is no honest exchange rate in a webhook. Every
 *     figure, chart and table below the bar reads in the chosen currency, and
 *     the currency control prints each currency's own net beside its code, so
 *     the others are visible without ever being summed.
 *   - MONEY FROM MINOR UNITS. Amounts arrive as integers in minor units and are
 *     written by Intl in their own currency (model.ts `money`).
 *   - REFUNDS READ AS NEGATIVE. In the strip, the tables and the chart, where
 *     refunds are drawn dashed below the zero line under a solid net line.
 *   - A CUSTOMER IS AN ID. The customer column prints the identifier the
 *     provider sent, exactly, in mono, and nothing else about the person.
 *
 * With no connection yet, the screen is a setup panel instead (RevenueSetup).
 * While a connection is still waiting for its secret or its first event, the
 * connections panel moves to the top, because finishing setup is the next
 * thing to do.
 */
import { useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";

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
import { ago, channelLabel, num, pct, shortPath } from "../../lib/format";
import { againstLeader, titleCase } from "../display";
import { useSite } from "../providers";
import { rangeWords, useAppState } from "../state";
import { Bank, WaitLines } from "./controls";
import { ChartWaiting, LineChart } from "./LineChart";
import { AttributionNote, ConnectProvider, Connections } from "./RevenueSetup";
import {
  ATTRIBUTION_WORDS,
  DIMENSIONS,
  PAYMENT_WORDS,
  PROVIDERS,
  countryName,
  day,
  moment,
  money,
  moneyCompact,
  negative,
  problem,
} from "./model";
import type {
  CurrencyTotals,
  Integration,
  RevenueCustomer,
  RevenueDimension,
  RevenueOverview,
  RevenuePayment,
  RevenueRow,
  RevenueSeries,
} from "./model";

const PAGE = 10;

/** The query envelope for a named range or a measured `start..end` one. */
function envelopeOf(siteId: number, range: string): Envelope {
  const cut = range.indexOf("..");
  return cut > 0
    ? { site: siteId, start: range.slice(0, cut), end: range.slice(cut + 2) }
    : { site: siteId, range };
}

/* ── headline figures ────────────────────────────────────────────────────── */

function Headline({ totals }: { totals: CurrencyTotals }) {
  const c = totals.currency;
  const channels: Channel[] = [
    { key: "net", label: "Net revenue", value: money(totals.net, c), note: "after refunds" },
    { key: "gross", label: "Gross revenue", value: money(totals.gross, c) },
    {
      key: "refunds",
      label: "Refunds",
      value: money(negative(totals.refunds), c),
      note: `${num(totals.refund_count)} ${totals.refund_count === 1 ? "refund" : "refunds"}`,
    },
    { key: "payments", label: "Payments", value: num(totals.payments) },
    { key: "customers", label: "Customers", value: num(totals.customers) },
    {
      key: "attributed",
      label: "Linked to a visit",
      value: pct(totals.attributed),
      note: "share of payments",
    },
  ];
  return <Readout channels={channels} dense />;
}

/* ── revenue over time ───────────────────────────────────────────────────── */

function RevenueChart({ env, currency, rangeText }: { env: Envelope; currency: string; rangeText: string }) {
  const series = useQuery({
    queryKey: ["revenue", "timeseries", env, currency],
    queryFn: () =>
      api.get<RevenueSeries[]>("/api/revenue/timeseries", { ...env, currency }).then((r) => r.data),
    placeholderData: keepPreviousData,
  });
  const points = series.data?.find((s) => s.currency === currency)?.points ?? [];
  const empty = points.every((p) => !p.net && !p.refunds && !p.gross);

  return (
    <div className="mf-gr-pad">
      {series.isPending ? (
        <ChartWaiting />
      ) : series.isError ? (
        <ErrorState
          title="Could not load revenue over time"
          detail={problem(series.error)}
          onRetry={() => void series.refetch()}
        />
      ) : empty ? (
        <Empty
          title="No payments to chart"
          body={`No payment in ${currency} was recorded for ${rangeText.toLowerCase()}. Choose a longer range.`}
        />
      ) : (
        <LineChart
          label={`Revenue in ${currency}, ${rangeText.toLowerCase()}`}
          times={points.map((p) => p.t)}
          format={(n) => money(n, currency)}
          formatAxis={(n) => moneyCompact(n, currency)}
          series={[
            { key: "net", label: "Net revenue", color: "var(--mf-people)", values: points.map((p) => p.net) },
            {
              key: "refunds",
              label: "Refunds, below zero",
              color: "var(--mf-ink-3)",
              dashed: true,
              values: points.map((p) => negative(p.refunds)),
            },
          ]}
        />
      )}
    </div>
  );
}

/* ── where it came from ──────────────────────────────────────────────────── */

function valueName(dim: RevenueDimension, v: string): { text: string; mono: boolean } {
  switch (dim) {
    case "channel":
      return { text: v ? channelLabel(v) : "Unknown", mono: false };
    case "source":
      return { text: v ? titleCase(v) : "Direct or unknown", mono: false };
    case "referrer_host":
      return v ? { text: v, mono: true } : { text: "Direct or unknown", mono: false };
    case "utm_source":
    case "utm_medium":
    case "utm_campaign":
      return { text: v || "No tag", mono: false };
    case "country":
      return { text: countryName(v), mono: false };
    case "device_type":
    case "browser":
      return { text: titleCase(v) || "Unknown", mono: false };
    case "entry_path":
      return v ? { text: shortPath(v, 48), mono: true } : { text: "Unknown", mono: false };
    case "attribution":
      return { text: ATTRIBUTION_WORDS[v] ?? (v || "Unknown"), mono: false };
  }
}

function Breakdown({ env, currency }: { env: Envelope; currency: string }) {
  const [dim, setDim] = useState<RevenueDimension>("channel");
  const rows = useQuery({
    queryKey: ["revenue", "breakdown", env, currency, dim],
    queryFn: () =>
      api
        .get<RevenueRow[]>("/api/revenue/breakdown", { ...env, currency, dimension: dim })
        .then((r) => r.data.filter((row) => row.currency === currency)),
    placeholderData: keepPreviousData,
  });
  const data = rows.data ?? [];
  const label = DIMENSIONS.find((d) => d.v === dim)?.label ?? "Value";

  const columns: Column<RevenueRow>[] = [
    {
      key: "value",
      head: label,
      share: againstLeader(data, (r) => (r.net > 0 ? r.net : 0)),
      render: (r) => {
        const n = valueName(dim, r.value);
        return (
          <span className={n.mono ? "mf-data" : undefined} title={r.value || n.text}>
            {n.text}
          </span>
        );
      },
    },
    { key: "net", head: "Net revenue", numeric: true, render: (r) => <span className="mf-data">{money(r.net, currency)}</span> },
    { key: "payments", head: "Payments", numeric: true, render: (r) => <span className="mf-data">{num(r.payments)}</span> },
    { key: "customers", head: "Customers", numeric: true, render: (r) => <span className="mf-data">{num(r.customers)}</span> },
  ];

  return (
    <Panel>
      <PanelHead
        title="Where revenue came from"
        note={`Net revenue in ${currency}, split by the visit each payment is linked to. The bar is each row against the largest.`}
      />
      <div className="mf-gr-strip">
        <Bank<RevenueDimension> label="Split revenue by" value={dim} options={DIMENSIONS} onChange={setDim} />
      </div>
      {rows.isError ? (
        <ErrorState
          title="Could not load this breakdown"
          detail={problem(rows.error)}
          onRetry={() => void rows.refetch()}
        />
      ) : (
        <Sheet<RevenueRow>
          rows={data}
          columns={columns}
          loading={rows.isPending}
          getKey={(r, i) => `${r.value}-${i}`}
          empty={
            <Empty
              title="Nothing to split"
              body={`No payment in ${currency} in this range has a ${label.toLowerCase()} recorded.`}
            />
          }
        />
      )}
    </Panel>
  );
}

/* ── customers ───────────────────────────────────────────────────────────── */

function Customers({ env, currency }: { env: Envelope; currency: string }) {
  const [limit, setLimit] = useState(PAGE);
  const rows = useQuery({
    queryKey: ["revenue", "customers", env, currency, limit],
    queryFn: () =>
      api
        .get<RevenueCustomer[]>("/api/revenue/customers", { ...env, currency, limit })
        .then((r) => r.data.filter((row) => row.currency === currency)),
    placeholderData: keepPreviousData,
  });
  const data = rows.data ?? [];

  const columns: Column<RevenueCustomer>[] = [
    {
      key: "customer",
      head: "Customer",
      render: (r) => (
        <span className="mf-data" title={r.customer}>
          {r.customer}
        </span>
      ),
    },
    { key: "net", head: "Net revenue", numeric: true, render: (r) => <span className="mf-data">{money(r.net, r.currency)}</span> },
    { key: "payments", head: "Payments", numeric: true, render: (r) => <span className="mf-data">{num(r.payments)}</span> },
    {
      key: "refunds",
      head: "Refunds",
      numeric: true,
      render: (r) => <span className="mf-data">{r.refunds ? money(negative(r.refunds), r.currency) : "—"}</span>,
    },
    { key: "channel", head: "First channel", render: (r) => <span>{r.channel ? channelLabel(r.channel) : "Unknown"}</span> },
    { key: "first", head: "First payment", render: (r) => <span>{day(r.first_at)}</span> },
    {
      key: "last",
      head: "Last payment",
      render: (r) => <span title={moment(r.last_at)}>{ago(r.last_at)}</span>,
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="Top customers"
        note={`By net revenue in ${currency}. Each customer is the id the payment provider sent, shown exactly, and nothing else about the person.`}
      />
      {rows.isError ? (
        <ErrorState
          title="Could not load customers"
          detail={problem(rows.error)}
          onRetry={() => void rows.refetch()}
        />
      ) : (
        <>
          <Sheet<RevenueCustomer>
            rows={data}
            columns={columns}
            loading={rows.isPending}
            getKey={(r, i) => `${r.customer}-${i}`}
            empty={<Empty title="No customers in this range" body={`No payment in ${currency} was recorded for this range.`} />}
          />
          {data.length >= limit && limit < 50 ? (
            <div className="mf-gr-more">
              <Button disabled={rows.isFetching} onClick={() => setLimit((n) => Math.min(50, n + 20))}>
                {rows.isFetching ? "Loading…" : "Show more customers"}
              </Button>
            </div>
          ) : null}
        </>
      )}
    </Panel>
  );
}

/* ── payments ────────────────────────────────────────────────────────────── */

function Payments({ env, currency }: { env: Envelope; currency: string }) {
  const [limit, setLimit] = useState(PAGE);
  const rows = useQuery({
    queryKey: ["revenue", "payments", env, currency, limit],
    queryFn: () =>
      api
        .get<RevenuePayment[]>("/api/revenue/payments", { ...env, currency, limit })
        .then((r) => r.data.filter((row) => row.currency === currency)),
    placeholderData: keepPreviousData,
  });
  const data = rows.data ?? [];

  const columns: Column<RevenuePayment>[] = [
    { key: "at", head: "When", render: (r) => <span title={moment(r.at)}>{ago(r.at)}</span> },
    { key: "type", head: "Type", render: (r) => <span>{PAYMENT_WORDS[r.type] ?? r.type}</span> },
    {
      key: "amount",
      head: "Amount",
      numeric: true,
      render: (r) => (
        <span className="mf-data" data-tone={r.type === "refund" ? "refund" : undefined}>
          {money(r.type === "refund" ? negative(r.amount) : r.amount, r.currency)}
        </span>
      ),
    },
    {
      key: "customer",
      head: "Customer",
      render: (r) => (
        <span className="mf-data" title={r.customer}>
          {r.customer || "—"}
        </span>
      ),
    },
    { key: "channel", head: "Channel", render: (r) => <span>{r.channel ? channelLabel(r.channel) : "Unknown"}</span> },
    { key: "country", head: "Country", render: (r) => <span>{countryName(r.country)}</span> },
    {
      key: "attribution",
      head: "Linked by",
      render: (r) => <span>{ATTRIBUTION_WORDS[r.attribution] ?? r.attribution}</span>,
    },
    { key: "provider", head: "Provider", render: (r) => <span>{PROVIDERS[r.provider]?.name ?? r.provider}</span> },
  ];

  return (
    <Panel>
      <PanelHead title="Recent payments" note={`Payments and refunds in ${currency}, newest first.`} />
      {rows.isError ? (
        <ErrorState
          title="Could not load payments"
          detail={problem(rows.error)}
          onRetry={() => void rows.refetch()}
        />
      ) : (
        <>
          <Sheet<RevenuePayment>
            rows={data}
            columns={columns}
            loading={rows.isPending}
            getKey={(r, i) => `${r.at}-${r.customer}-${i}`}
            empty={<Empty title="No payments in this range" body={`No payment in ${currency} was recorded for this range.`} />}
          />
          {data.length >= limit && limit < 50 ? (
            <div className="mf-gr-more">
              <Button disabled={rows.isFetching} onClick={() => setLimit((n) => Math.min(50, n + 20))}>
                {rows.isFetching ? "Loading…" : "Show more payments"}
              </Button>
            </div>
          ) : null}
        </>
      )}
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function RevenueScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const range = useAppState((s) => s.range);
  const setRange = useAppState((s) => s.setRange);
  const [chosen, setChosen] = useState<string | null>(null);
  const [createdId, setCreatedId] = useState<string | null>(null);

  const env = useMemo(() => (siteId != null ? envelopeOf(siteId, range) : null), [siteId, range]);
  const rangeText = rangeWords(range);

  const overview = useQuery({
    queryKey: ["revenue", "overview", env],
    queryFn: () => api.get<RevenueOverview>("/api/revenue/overview", env as Envelope).then((r) => r.data),
    enabled: env != null,
    placeholderData: keepPreviousData,
  });
  const integrations = useQuery({
    queryKey: ["revenue", "integrations", siteId],
    queryFn: () =>
      api
        .get<Integration[]>(`/api/revenue/integrations?site=${encodeURIComponent(String(siteId))}`)
        .then((r) => r.data),
    enabled: siteId != null,
  });

  const currencies = useMemo(
    () => [...(overview.data?.currencies ?? [])].sort((a, b) => b.net - a.net),
    [overview.data],
  );
  const totals = currencies.find((c) => c.currency === chosen) ?? currencies[0] ?? null;
  const currency = totals?.currency ?? null;
  const items = integrations.data ?? [];
  const unfinished = items.some((i) => !i.has_secret || i.last_event_at == null);

  const heading = <h1 className="mf-sr">Revenue</h1>;

  if (siteLoading || siteId == null || env == null || overview.isPending) {
    return (
      <div className="mf-gr">
        {heading}
        <Panel>
          <div className="mf-gr-pad">
            <WaitLines lines={2} label="Loading revenue…" />
            <ChartWaiting />
          </div>
        </Panel>
      </div>
    );
  }

  if (overview.isError) {
    return (
      <div className="mf-gr">
        {heading}
        <Panel>
          <ErrorState
            title="Could not load revenue"
            detail={problem(overview.error)}
            onRetry={() => void overview.refetch()}
          />
        </Panel>
      </div>
    );
  }

  const noConnection = overview.data.integrations === 0 && items.length === 0;

  if (noConnection) {
    return (
      <div className="mf-gr">
        {heading}
        <Panel className="mf-gr-setup">
          <PanelHead
            title="Connect a payment provider"
            note="Revenue arrives from Stripe or Paddle by webhook. Micaforge links each payment to the visit that led to it, without cookies, so you can see which pages and channels earn money."
          />
          <div className="mf-gr-pad">
            {integrations.isError ? (
              <p className="mf-gr-aside">
                The list of connections did not load, so this page cannot tell whether one exists. {problem(integrations.error)}
              </p>
            ) : null}
            <ConnectProvider siteId={siteId} taken={[]} lead onCreated={setCreatedId} />
          </div>
        </Panel>
        <AttributionNote siteId={siteId} />
      </div>
    );
  }

  const connections = (
    <Connections
      siteId={siteId}
      items={items}
      loading={integrations.isPending}
      error={integrations.isError ? integrations.error : null}
      onRetry={() => void integrations.refetch()}
      focusId={createdId}
      onCreated={setCreatedId}
    />
  );

  return (
    <div className="mf-gr">
      {heading}

      <div className="mf-gr-bar">
        <RangePicker value={range} onChange={setRange} />
        {currencies.length > 1 && currency ? (
          <Bank<string>
            label="Currency"
            value={currency}
            options={currencies.map((c) => ({
              v: c.currency,
              label: (
                <>
                  {c.currency} <span className="mf-gr-bank__fig mf-data">{money(c.net, c.currency)}</span>
                </>
              ),
            }))}
            onChange={setChosen}
          />
        ) : null}
      </div>
      {currencies.length > 1 ? (
        <p className="mf-gr-aside">
          Each currency is reported on its own. Amounts in different currencies are never added together.
        </p>
      ) : null}

      {unfinished ? connections : null}

      {totals && currency ? (
        <>
          <Panel>
            <PanelHead title="Revenue" note={`${rangeText}, in ${currency}. Refunds are taken off net revenue.`} />
            <Headline totals={totals} />
            <RevenueChart env={env} currency={currency} rangeText={rangeText} />
          </Panel>
          <Breakdown env={env} currency={currency} />
          <Customers env={env} currency={currency} />
          <Payments env={env} currency={currency} />
        </>
      ) : (
        <Panel>
          <Empty
            title="No payments in this range"
            body={
              unfinished
                ? "Payments appear here once the connection above has its secret and the provider sends its first event."
                : `No payment arrived for ${rangeText.toLowerCase()}. Choose a longer range, or check below when the last event arrived.`
            }
          />
        </Panel>
      )}

      {unfinished ? null : connections}
      <AttributionNote siteId={siteId} />
    </div>
  );
}

export default RevenueScreen;
