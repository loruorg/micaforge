/**
 * Sources: the acquisition report, and the shape of it is the argument.
 *
 * Channels come first, and `answer_engine` sits in that list as a peer of
 * `search`: same row, same treatment, same measures. It is never folded into
 * referrals, and it is never given a decorative promotion either: a peer is a
 * peer, and identical treatment is precisely the claim being made.
 *
 * Under the channel list is the question this product can answer and the
 * others cannot: *is that traffic worth anything?* Conversion rate and time on
 * site are drawn per channel on one shared scale each, with the site's own
 * average ruled across every track, so an operator can see in one look whether
 * an answer engine sends readers or tourists.
 */
import { useMemo, useState } from "react";
import { Empty, ErrorState, Panel, PanelHead, Readout, Sheet } from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import type { Envelope, Filter } from "../../lib/api";
import { channelLabel, compact, num, pct } from "../../lib/format";
import { againstLeader, sourceName } from "../display";
import { useAppState } from "../state";
import {
  AudienceBar,
  Caveat,
  ExportKey,
  FilterTrail,
  ReadoutWait,
  count,
  errorDetail,
  n,
  percentOf100,
  ratio,
  secs,
  useAudienceQuery,
  useEnvelope,
} from "./PagesScreen";
import type { AudienceRow } from "./PagesScreen";
import "../../styles/app/audience.css";

interface ChannelRow extends AudienceRow {
  conversions?: number | null;
  conversion_rate?: number | null;
}

const PIVOTS: { id: string; label: string }[] = [
  { id: "utm_campaign", label: "Campaign" },
  { id: "utm_source", label: "Source" },
  { id: "utm_medium", label: "Medium" },
  { id: "utm_term", label: "Term" },
  { id: "utm_content", label: "Content" },
];

/** A conversion rate, taken as reported or derived when only counts came back. */
function convRate(r: ChannelRow): number | null {
  const direct = n(r, "conversion_rate");
  if (direct != null) return direct;
  const conv = n(r, "conversions");
  const base = n(r, "sessions") ?? n(r, "visitors");
  if (conv != null && base != null && base > 0) return conv / base;
  return null;
}

/** A measured bar with its figure printed beside it, never instead of it. */
function Gauge({
  caption,
  value,
  max,
  mark,
  lead,
  format,
}: {
  caption: string;
  value: number | null;
  max: number;
  mark: number | null;
  lead: boolean;
  format: (v: number) => string;
}) {
  const width = value == null || max <= 0 ? 0 : Math.max(0, Math.min(1, value / max)) * 100;
  const markAt = mark == null || max <= 0 ? null : Math.max(0, Math.min(1, mark / max)) * 100;
  return (
    <div className="mfa-gauge">
      {/* Read out at every width, drawn only where the column head is gone. */}
      <span className="mfa-gauge__cap">{caption}</span>
      <span className="mfa-gauge__track" aria-hidden="true">
        {value != null ? (
          <span className="mfa-gauge__fill" data-lead={lead} style={{ width: `${width}%` }} />
        ) : null}
        {markAt != null ? (
          <span className="mfa-gauge__mark" style={{ left: `${markAt}%` }} />
        ) : null}
      </span>
      <span className="mfa-gauge__val" data-empty={value == null}>
        {value == null ? "—" : format(value)}
      </span>
    </div>
  );
}

export function SourcesScreen() {
  const { addFilter, audience } = useAppState();
  const [channel, setChannel] = useState<string | null>(null);
  const [pivot, setPivot] = useState<string>(PIVOTS[0].id);
  const machines = audience === "agent";

  const { env, ready } = useEnvelope();

  const channelEnv = useMemo<Envelope>(() => ({ ...env, dimension: "channel" }), [env]);
  const channelsQ = useAudienceQuery<ChannelRow>(
    "sources:channel",
    "/api/stats/sources",
    channelEnv,
    ready,
  );
  const rows = channelsQ.data?.rows ?? [];

  /* Channels are a closed set, so summing them is a complete reading rather
     than a total inferred from a top-N list. */
  const totals = useMemo(() => {
    let visitors = 0;
    let answer = 0;
    let search = 0;
    for (const r of rows) {
      const v = n(r, "visitors") ?? 0;
      visitors += v;
      if (r.key === "answer_engine") answer += v;
      if (r.key === "search") search += v;
    }
    return { visitors, answer, search };
  }, [rows]);

  const readout = useMemo<Channel[]>(
    () => [
      {
        key: "visitors",
        label: machines ? "Clients" : "Visitors",
        value: count(totals.visitors),
        note: `${rows.length} channel${rows.length === 1 ? "" : "s"}`,
      },
      { key: "search", label: "From search", value: count(totals.search) },
      {
        key: "answer",
        label: "From answer engines",
        value: count(totals.answer),
        note: "its own channel, not a referral",
      },
      {
        key: "share",
        label: "Answer-engine share",
        value: totals.visitors > 0 ? pct(totals.answer / totals.visitors, 1) : "—",
      },
    ],
    [totals, rows.length, machines],
  );

  /* ── the worth comparison ──────────────────────────────────────────────── */
  const worth = useMemo(() => {
    const measured = rows.filter((r) => (n(r, "sessions") ?? n(r, "visitors") ?? 0) > 0);
    let convNum = 0;
    let convDen = 0;
    let timeNum = 0;
    let timeDen = 0;
    let maxConv = 0;
    let maxTime = 0;
    let anyConv = false;
    for (const r of measured) {
      const base = n(r, "sessions") ?? n(r, "visitors") ?? 0;
      const c = convRate(r);
      if (c != null) {
        anyConv = true;
        convNum += c * base;
        convDen += base;
        if (c > maxConv) maxConv = c;
      }
      const t = n(r, "avg_duration_s");
      if (t != null) {
        timeNum += t * base;
        timeDen += base;
        if (t > maxTime) maxTime = t;
      }
    }
    let leadConv: string | null = null;
    let leadTime: string | null = null;
    let bestConv = -Infinity;
    let bestTime = -Infinity;
    for (const r of measured) {
      const c = convRate(r);
      if (c != null && c > bestConv) { bestConv = c; leadConv = r.key; }
      const t = n(r, "avg_duration_s");
      if (t != null && t > bestTime) { bestTime = t; leadTime = r.key; }
    }
    return {
      rows: measured,
      anyConv,
      avgConv: convDen > 0 ? convNum / convDen : null,
      avgTime: timeDen > 0 ? timeNum / timeDen : null,
      maxConv: maxConv > 0 ? maxConv : 1,
      maxTime: maxTime > 0 ? maxTime : 1,
      leadConv,
      leadTime,
    };
  }, [rows]);

  /* ── sources within the selected channel ───────────────────────────────── */
  const sourceEnv = useMemo<Envelope>(
    () => ({
      ...env,
      dimension: "source",
      filters: channel
        ? [
            ...((env.filters ?? []) as Filter[]),
            { dimension: "channel", op: "is", value: channel } as Filter,
          ]
        : (env.filters as Filter[]),
    }),
    [env, channel],
  );
  const sourcesQ = useAudienceQuery<AudienceRow>(
    `sources:source:${channel ?? "all"}`,
    "/api/stats/sources",
    sourceEnv,
    ready,
  );

  /* ── campaigns ─────────────────────────────────────────────────────────── */
  const campaignEnv = useMemo<Envelope>(() => ({ ...env, dimension: pivot }), [env, pivot]);
  const campaignsQ = useAudienceQuery<AudienceRow>(
    `sources:${pivot}`,
    "/api/stats/sources",
    campaignEnv,
    ready,
  );

  /* A named sender is a name and is cased like one; a campaign tag is a string
     the marketer typed and is left exactly as it was typed. Same columns, two
     different naming rules, so the caller states which it wants. */
  const measureCols = (
    labelHead: string,
    name: (r: AudienceRow) => string,
    rows: AudienceRow[],
  ): Column<AudienceRow>[] => [
    {
      key: "label",
      head: labelHead,
      width: "36%",
      /* Drawn against the leading row of this sheet: at a typical share of a few
         per cent, ink drawn against the whole site is the same stub on every
         row. The share column still carries the share of the whole. */
      share: againstLeader(rows, (r) => n(r, "share")),
      render: (r) => (
        <span className="mfa-name" title={r.label ?? r.key}>
          {name(r)}
        </span>
      ),
    },
    {
      key: "visitors",
      head: machines ? "Clients" : "Visitors",
      numeric: true,
      render: (r) => count(n(r, "visitors")),
    },
    {
      key: "sessions",
      head: "Sessions",
      numeric: true,
      render: (r) => count(n(r, "sessions")),
    },
    { key: "bounce", head: "Bounce", numeric: true, render: (r) => percentOf100(n(r, "bounce_rate")) },
    {
      key: "time",
      head: "Avg visit",
      numeric: true,
      render: (r) => secs(n(r, "avg_duration_s")),
    },
  ];

  const channelColumns: Column<ChannelRow>[] = [
    {
      key: "channel",
      head: "Channel",
      width: "34%",
      share: againstLeader(rows, (r) => n(r, "share")),
      render: (r) => <span className="mfa-name">{channelLabel(r.key)}</span>,
    },
    {
      key: "visitors",
      head: machines ? "Clients" : "Visitors",
      numeric: true,
      render: (r) => count(n(r, "visitors")),
    },
    { key: "sessions", head: "Sessions", numeric: true, render: (r) => count(n(r, "sessions")) },
    { key: "bounce", head: "Bounce", numeric: true, render: (r) => percentOf100(n(r, "bounce_rate")) },
    {
      key: "time",
      head: "Avg visit",
      numeric: true,
      render: (r) => secs(n(r, "avg_duration_s")),
    },
    {
      key: "share",
      head: "Share",
      numeric: true,
      render: (r) => (n(r, "share") == null ? "—" : pct(n(r, "share")!, 1)),
    },
  ];

  const sourceRows = sourcesQ.data?.rows ?? [];
  const campaignRows = campaignsQ.data?.rows ?? [];
  const sourceCols = measureCols("Source", (r) => sourceName(r), sourceRows);
  const campaignCols = measureCols(
    PIVOTS.find((p) => p.id === pivot)?.label ?? "Value",
    (r) => r.label || r.key || "(not set)",
    campaignRows,
  );

  return (
    /* Which pen these sheets are written in: the ink behind a row is the trace
       of the audience being read, never the machine pen on a human table. */
    <div className="mfa-screen" data-trace={machines ? "agent" : "human"}>
      <Panel ruled feed>
        <PanelHead
          title="Sources"
          note="Where the traffic came from, bucketed into channels. Answer engines are one of those channels, listed beside search rather than buried in referrals."
          actions={<AudienceBar extra={<ExportKey report="sources:channel" env={env} />} />}
        />

        {channelsQ.isError ? (
          <ErrorState
            title="Channels did not come back"
            detail={errorDetail(channelsQ.error)}
            onRetry={() => void channelsQ.refetch()}
          />
        ) : channelsQ.isLoading ? (
          <ReadoutWait channels={4} />
        ) : (
          <Readout channels={readout} />
        )}

        <FilterTrail />

        {machines ? (
          <Caveat>
            A crawler arrives without a referrer, so the machine trace has almost nothing to
            attribute: most of it lands in Direct. Acquisition is a question about readers;
            switch to Readers to ask it, or read Agent traffic for what the machines did.
          </Caveat>
        ) : null}

        {channelsQ.isError ? null : (
          <Sheet<ChannelRow>
            rows={rows}
            columns={channelColumns}
            loading={channelsQ.isLoading}
            getKey={(r) => r.key}
            onRowClick={(r) => setChannel((c) => (c === r.key ? null : r.key))}
            empty={
              <Empty
                title="No arrivals were recorded in this interval"
                body="A channel appears the moment the first hit is attributed to it. Widen the interval, or clear a clip."
              />
            }
          />
        )}

        {rows.length ? (
          <div className="mfa-foot">
            <span>
              Click a channel to read the senders inside it.
              {channel ? ` Reading ${channelLabel(channel)}.` : ""}
            </span>
            {channel ? (
              <button type="button" className="mfa-crumbs__step" onClick={() => setChannel(null)}>
                Read all channels
              </button>
            ) : null}
          </div>
        ) : null}
      </Panel>

      {/* ── is any of it worth anything? ─────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="What each channel is worth"
          note="Conversion rate and time on site per channel, each on its own shared scale. The upright rule on every track is the site's own average, so a channel above it is pulling its weight."
        />

        {channelsQ.isError ? (
          <ErrorState
            title="The comparison did not come back"
            detail={errorDetail(channelsQ.error)}
            onRetry={() => void channelsQ.refetch()}
          />
        ) : channelsQ.isLoading ? (
          <ReadoutWait channels={3} />
        ) : worth.rows.length === 0 ? (
          <Empty
            title="Nothing to compare yet"
            body="Two channels with recorded sessions are enough to fill this. It stays empty rather than compare one channel with itself."
          />
        ) : (
          <>
            {!worth.anyConv ? (
              <Caveat>
                No goal is defined for this site, so there is no conversion rate to compare.
                Define a goal in settings and this column fills in for every channel, answer
                engines included.
              </Caveat>
            ) : null}

            <div className="mfa-compare">
              <div className="mfa-compare__row mfa-compare__row--head">
                <span>Channel</span>
                <span className="mfa-legend">
                  Conversion rate
                  {worth.avgConv != null ? (
                    <>
                      <span className="mfa-legend__rule" aria-hidden="true" />
                      site average {pct(worth.avgConv, 1)}
                    </>
                  ) : null}
                </span>
                <span className="mfa-legend">
                  Time on site
                  {worth.avgTime != null ? (
                    <>
                      <span className="mfa-legend__rule" aria-hidden="true" />
                      site average {secs(worth.avgTime)}
                    </>
                  ) : null}
                </span>
              </div>

              {worth.rows.map((r) => (
                <div className="mfa-compare__row" key={r.key}>
                  <span className="mfa-compare__chan">
                    <span className="mfa-compare__label">{channelLabel(r.key)}</span>
                    <span className="mfa-compare__vis">
                      {num(n(r, "sessions") ?? n(r, "visitors") ?? 0)}{" "}
                      {n(r, "sessions") != null ? "sessions" : "visitors"}
                    </span>
                  </span>
                  <Gauge
                    caption="Conversion rate"
                    value={convRate(r)}
                    max={worth.maxConv}
                    mark={worth.avgConv}
                    lead={worth.leadConv === r.key}
                    format={(v) => pct(v, 1)}
                  />
                  <Gauge
                    caption="Time on site"
                    value={n(r, "avg_duration_s")}
                    max={worth.maxTime}
                    mark={worth.avgTime}
                    lead={worth.leadTime === r.key}
                    format={(v) => secs(v)}
                  />
                </div>
              ))}
            </div>
          </>
        )}
      </Panel>

      {/* ── sources ──────────────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title={channel ? `Named senders in ${channelLabel(channel)}` : "Named senders"}
          note={
            channel
              ? "The named senders inside the channel you selected above."
              : "The named senders behind every channel. Select a channel above to narrow this."
          }
          actions={<ExportKey report={`sources:source:${channel ?? "all"}`} env={sourceEnv} />}
        />
        {sourcesQ.isError ? (
          <ErrorState
            title="Sources did not come back"
            detail={errorDetail(sourcesQ.error)}
            onRetry={() => void sourcesQ.refetch()}
          />
        ) : (
          <Sheet<AudienceRow>
            rows={sourceRows}
            columns={sourceCols}
            loading={sourcesQ.isLoading}
            getKey={(r, i) => `${r.key}-${i}`}
            onRowClick={(r) =>
              addFilter({ dimension: "source", op: "is", value: r.key } as Filter)
            }
            empty={
              <Empty
                title={
                  channel
                    ? `Nothing was attributed to ${channelLabel(channel)} in this interval`
                    : "No named sources in this interval"
                }
                body="A source appears here once a referrer or a utm_source can be resolved to a name. Direct traffic carries neither, by definition."
              />
            }
          />
        )}
        {(sourcesQ.data?.rows.length ?? 0) > 0 ? (
          <div className="mfa-foot">
            <span>Click a source to clip the whole record to it.</span>
            <span className="mf-data">{compact(sourcesQ.data?.total ?? 0)} rows</span>
          </div>
        ) : null}
      </Panel>

      {/* ── campaigns ────────────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Campaigns"
          note="The UTM parameters on the links people followed. Switch the pivot to read the same traffic by a different tag."
          actions={
            <div className="mf-caliper__jaws" role="group" aria-label="Campaign pivot">
              {PIVOTS.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  className="mf-caliper__stop"
                  aria-pressed={pivot === p.id}
                  onClick={() => setPivot(p.id)}
                >
                  {p.label}
                </button>
              ))}
            </div>
          }
        />
        {campaignsQ.isError ? (
          <ErrorState
            title="Campaigns did not come back"
            detail={errorDetail(campaignsQ.error)}
            onRetry={() => void campaignsQ.refetch()}
          />
        ) : (
          <Sheet<AudienceRow>
            rows={campaignRows}
            columns={campaignCols}
            loading={campaignsQ.isLoading}
            getKey={(r, i) => `${r.key}-${i}`}
            onRowClick={(r) => addFilter({ dimension: pivot, op: "is", value: r.key } as Filter)}
            empty={
              <Empty
                title={`Nothing carried ${pivot} in this interval`}
                body={`Tag a link with ?${pivot}=… and the arrivals from it are grouped here.`}
              />
            }
          />
        )}
        {(campaignsQ.data?.rows.length ?? 0) > 0 ? (
          <div className="mfa-foot">
            <span>Click a row to clip the whole record to that tag.</span>
            <span className="mf-data">{compact(campaignsQ.data?.total ?? 0)} rows</span>
          </div>
        ) : null}
      </Panel>
    </div>
  );
}

export default SourcesScreen;
