/**
 * THE RECORD: the site overview, and the product's home.
 *
 * The screen is built as one instrument rather than a page of widgets: the
 * patch bar sets what is being read, the drum writes it on continuous paper,
 * the readout bank under the drum reports the same interval as figures and
 * chooses which channel the pen is writing, and the bench below tabulates what
 * the trace is made of.
 *
 * The machines strip sits deliberately high on the page. An operator can read
 * the human numbers anywhere; the count of what the crawlers took, and how
 * little of it came back, is the reason this product exists, so it is not
 * buried at the bottom next to the browser table.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";

import { publicOrigin, api } from "../../lib/api";
import type { AgentOverview, ApiMeta, Filter, GapSummary, Overview, Series } from "../../lib/api";
import { compact, delta, duration, num, pct } from "../../lib/format";
import { gapRatio } from "../display";
import { demoRecord } from "../../lib/demo";
import { Drum } from "../../components/react/Drum";
import type { DrumAnnotation, DrumChannel } from "../../components/react/Drum";
import {
  AudienceSwitch,
  Button,
  CopyField,
  DemoNotice,
  ErrorState,
  Panel,
  PanelHead,
  RangePicker,
  Readout,
} from "../../components/react/ui";
import type { Channel } from "../../components/react/ui";
import { IconClose, IconOut } from "../../components/react/icons";
import {
  TopSheets,
  asFraction,
  asPercent,
  baseEnvelope,
  errorDetail,
  isMeasuredRange,
  rangeLabel,
  statsKey,
  toMs,
  useRecordSite,
  useRecordState,
} from "./TopSheets";
import type { RecordQuery } from "./TopSheets";
import "../../styles/app/record.css";

/* ── the six channels of the readout bank ────────────────────────────────── */

type MetricKey =
  | "visitors"
  | "sessions"
  | "pageviews"
  | "bounce_rate"
  | "avg_duration_s"
  | "views_per_session";

interface MetricDef {
  key: MetricKey;
  label: string;
  /** The figure in the readout. */
  fmt: (v: number | null | undefined) => string;
  /** What the drum prints for one sample of this channel. */
  write: (v: number) => string;
  /**
   * A rate crosses the wire as 0..1, and the drum's gain scale is written in
   * whole numbers, so a rate is handed to the pen as 0..100 and formatted back
   * on the way out. Without this the gain margin reads 0, 0, 1, 1.
   */
  toPen?: (v: number | null) => number | null;
  /** True where a fall is the good outcome. */
  inverse?: boolean;
}

const oneDp = (v: number | null | undefined) =>
  v == null || !Number.isFinite(v) ? "—" : v.toFixed(1);

const METRICS: MetricDef[] = [
  { key: "visitors", label: "Visitors", fmt: num, write: compact },
  { key: "sessions", label: "Sessions", fmt: num, write: compact },
  { key: "pageviews", label: "Pageviews", fmt: num, write: compact },
  {
    key: "bounce_rate",
    label: "Bounce rate",
    fmt: (v) => pct(asFraction(v)),
    write: (v) => `${Math.round(v)}%`,
    toPen: asPercent,
    inverse: true,
  },
  { key: "avg_duration_s", label: "Time on page", fmt: duration, write: duration },
  { key: "views_per_session", label: "Views per session", fmt: oneDp, write: (v) => v.toFixed(1) },
];

/* ── what the machines did ───────────────────────────────────────────────── */

function MachinesStrip({ q, base }: { q: RecordQuery; base: string }) {
  const machineQ: RecordQuery = { ...q, audience: "agent" };

  const agents = useQuery({
    queryKey: statsKey("agents/overview", q.siteId, q.range, "agent", q.filtersJson),
    queryFn: () =>
      api.get<AgentOverview>("/api/agents/overview", baseEnvelope(machineQ)).then((r) => r.data),
    enabled: q.siteId != null,
    staleTime: 30_000,
  });

  const gap = useQuery({
    queryKey: statsKey("citation-gap/summary", q.siteId, q.range, "agent", q.filtersJson),
    queryFn: () =>
      api
        .get<GapSummary>("/api/citation-gap/summary", baseEnvelope(machineQ))
        .then((r) => r.data),
    enabled: q.siteId != null,
    staleTime: 30_000,
  });

  const a = agents.data;
  const g = gap.data;
  const waiting = q.siteId == null || agents.isPending || gap.isPending;
  const failed = agents.isError || gap.isError;

  /* Read from the two counts, exactly as the Citation Gap report reads it, so
     the same site over the same interval cannot print two different ratios on
     two screens. The formatting rule itself lives in app/display.ts. */
  const gapReading = waiting || g == null ? "—" : gapRatio(g.agent_fetches, g.answer_arrivals);
  const verified = a == null ? null : asFraction(a.verified_pct);

  const channels: Channel[] = [
    {
      key: "fetches",
      label: "Agent fetches",
      value: waiting ? "—" : num(a?.fetches),
      delta: a ? asFraction(a.delta_pct) : null,
    },
    {
      key: "agents",
      label: "Distinct agents",
      value: waiting ? "—" : num(a?.unique_agents),
      note: verified == null ? undefined : `${pct(verified)} verified`,
    },
    {
      key: "arrivals",
      label: "Answer-engine arrivals",
      value: waiting ? "—" : num(g?.answer_arrivals),
      note: "readers sent by an AI answer",
    },
    {
      key: "gap",
      label: "Citation Gap",
      value: gapReading,
      note:
        waiting || g == null
          ? undefined
          : g.answer_arrivals > 0
            ? "fetches for each arrival"
            : g.agent_fetches > 0
              ? "nothing has come back yet"
              : "no crawl to measure yet",
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="What the machines did"
        note="Fetches by named AI clients over the same interval, and how many readers an answer engine sent back."
        actions={
          <>
            <Link className="mf-btn mf-btn--quiet mf-btn--sm mfr-key" to={`${base}/agents`}>
              <span className="mf-btn__label">Agent traffic</span>
              <span className="mf-btn__icon">
                <IconOut size={13} />
              </span>
            </Link>
            <Link className="mf-btn mf-btn--quiet mf-btn--sm mfr-key" to={`${base}/citation-gap`}>
              <span className="mf-btn__label">Citation gap</span>
              <span className="mf-btn__icon">
                <IconOut size={13} />
              </span>
            </Link>
          </>
        }
      />
      <div className="mfr-body mfr-body--flush">
        {failed ? (
          <ErrorState
            title="The machine reading did not come back"
            detail={errorDetail(agents.error ?? gap.error)}
            onRetry={() => {
              void agents.refetch();
              void gap.refetch();
            }}
          />
        ) : (
          <Readout channels={channels} dense />
        )}
      </div>
    </Panel>
  );
}

/* ── waiting for the first hit ───────────────────────────────────────────── */

function FirstHit({
  domain,
  siteId,
  range,
  onRecheck,
}: {
  domain?: string;
  siteId: number | null;
  range: string;
  onRecheck: () => void;
}) {
  const endMs = useMemo(() => Date.now(), []);
  const demo = useMemo(() => demoRecord(30, 20260821, endMs), [endMs]);

  const channels: DrumChannel[] = [
    {
      id: "readers",
      label: "Readers",
      color: "var(--mf-trace-human)",
      pen: "written",
      values: demo.readers,
      format: compact,
    },
    {
      id: "machines",
      label: "Machine fetches",
      color: "var(--mf-trace-agent)",
      pen: "stepped",
      values: demo.machines,
      format: compact,
    },
  ];

  return (
    <Panel feed>
      <PanelHead
        title="Waiting for the first hit"
        note={`${rangeLabel(range)}: nothing was recorded for ${domain ?? "this site"}. Add the tracker and the pen starts on the next pageview.`}
        actions={
          <Button size="sm" onClick={onRecheck}>
            Check again
          </Button>
        }
      />
      <div className="mfr-body">
        <div className="mfr-install">
          <div className="mfr-install__say">
            <p className="mfr-install__body">
              One tag, no cookie, no consent banner. Agent fetches are counted server-side, so
              crawlers are recorded whether or not they run this script.
            </p>
            <CopyField
              label="Paste this before </head>"
              value={`<script defer src="${publicOrigin()}/mf.js" data-site="${siteId ?? ""}"></script>`}
            />
          </div>
          <div className="mfr-install__demo">
            <Drum
              times={demo.times}
              channels={channels}
              scale="per-channel"
              annotations={demo.annotations}
              height={236}
              demo
            />
            <DemoNotice>
              Illustrative record. This is the shape the two pens make, not traffic to{" "}
              {domain ?? "this site"}.
            </DemoNotice>
          </div>
        </div>
      </div>
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function OverviewScreen() {
  const { range, setRange, audience, setAudience, filters, addFilter } = useRecordState();
  const { siteId, domain, timezone } = useRecordSite();
  const { site } = useParams();

  const [metricKey, setMetricKey] = useState<MetricKey>("visitors");
  const [ghost, setGhost] = useState(true);

  /** The named interval to fall back to when the caliper is released. */
  const namedRange = useRef(isMeasuredRange(range) ? "7d" : range);
  useEffect(() => {
    if (!isMeasuredRange(range)) namedRange.current = range;
  }, [range]);

  const active = useMemo(() => filters ?? [], [filters]);
  const filtersJson = useMemo(() => JSON.stringify(active), [active]);
  const q: RecordQuery = { siteId, range, audience, filters: active, filtersJson, timezone };

  const metric = METRICS.find((m) => m.key === metricKey) ?? METRICS[0];
  const base = site ? `/app/${site}` : "/app";

  const overview = useQuery({
    queryKey: statsKey("overview", siteId, range, audience, filtersJson),
    queryFn: () =>
      api.get<Overview>("/api/stats/overview", baseEnvelope(q, { compare: "previous" })),
    enabled: siteId != null,
    staleTime: 30_000,
  });

  const series = useQuery({
    queryKey: statsKey("timeseries", siteId, range, audience, filtersJson, metric.key, ghost),
    queryFn: () =>
      api
        .get<Series>(
          "/api/stats/timeseries",
          baseEnvelope(q, { metric: metric.key, compare: ghost ? "previous" : "none" }),
        )
        .then((r) => r.data),
    enabled: siteId != null,
    staleTime: 30_000,
  });

  const ov: Overview | undefined = overview.data?.data;
  const meta: ApiMeta | undefined = overview.data?.meta;
  const before = ov?.compare ?? null;

  /* ── the trace ─────────────────────────────────────────────────────────── */

  const buckets = series.data?.buckets;
  const times = useMemo(() => (buckets ?? []).map((b) => toMs(b.t)), [buckets]);
  const values = useMemo(
    () => (buckets ?? []).map((b) => (metric.toPen ? metric.toPen(b.v) : b.v)),
    [buckets, metric],
  );
  const ghostValues = useMemo(() => {
    const cmp = series.data?.compare;
    if (!ghost || !buckets || !cmp || !cmp.length) return null;
    return buckets.map((_, i) => {
      const v = cmp[i]?.v ?? null;
      return metric.toPen ? metric.toPen(v) : v;
    });
  }, [buckets, ghost, series.data, metric]);

  /* The drum's readout prints the value of ONE sample: the one under the
     cursor, or the last one written. The strip beneath the drum prints the
     total for the whole interval. Two different measurements, so neither is
     allowed to wear the bare metric name on its own. */
  const channels: DrumChannel[] = [
    {
      id: metric.key,
      label: `${metric.label}, this sample`,
      color: audience === "agent" ? "var(--mf-trace-agent)" : "var(--mf-trace-human)",
      pen: audience === "agent" ? "stepped" : "written",
      values,
      format: metric.write,
    },
  ];
  if (ghostValues) {
    channels.push({
      id: `${metric.key}-before`,
      label: "Previous period, this sample",
      color: "var(--mf-ink-3)",
      pen: audience === "agent" ? "stepped" : "written",
      values: ghostValues,
      ghost: true,
      format: metric.write,
    });
  }

  /** Annotations are read off the trace itself: never written into it. */
  const annotations = useMemo<DrumAnnotation[]>(() => {
    const out: DrumAnnotation[] = [];
    if (values.length < 3) return out;

    let peak = -Infinity;
    let peakAt = -1;
    for (let i = 0; i < values.length; i++) {
      const v = values[i];
      if (v != null && v > peak) {
        peak = v;
        peakAt = i;
      }
    }
    /* The peak is deliberately NOT written here. The drum notes it in the
       margin itself when the pen runs off the top of the paper, and it is the
       only one that knows the true value it ran off at, so writing a second
       "Peak" from this side put two identical marks on one sample. `peakAt` is
       still read, to keep the two notes below off the same bucket. */

    let rise = 0;
    let riseAt = -1;
    let fall = 0;
    let fallAt = -1;
    for (let i = 1; i < values.length; i++) {
      const a = values[i - 1];
      const b = values[i];
      if (a == null || b == null || a <= 0) continue;
      const d = (b - a) / a;
      if (d > rise) {
        rise = d;
        riseAt = i;
      }
      if (d < fall) {
        fall = d;
        fallAt = i;
      }
    }
    const up = metric.inverse ? "warn" : "good";
    const down = metric.inverse ? "good" : "warn";
    if (riseAt > 0 && rise >= 0.15 && riseAt !== peakAt) {
      out.push({ at: riseAt, label: `Steepest rise +${Math.round(rise * 100)}%`, tone: up });
    }
    if (fallAt > 0 && fall <= -0.15 && fallAt !== peakAt && fallAt !== riseAt) {
      out.push({ at: fallAt, label: `Steepest fall ${Math.round(fall * 100)}%`, tone: down });
    }
    return out;
  }, [values, metric]);

  /** The caliper measures an interval off the paper and makes it the range. */
  const onMeasure = useCallback(
    (from: number, to: number) => {
      const t0 = times[from];
      const t1 = times[to];
      if (t0 == null || t1 == null) return;
      const step =
        times.length > 1 ? (times[times.length - 1] - times[0]) / (times.length - 1) : 0;
      setRange(`${new Date(t0).toISOString()}..${new Date(t1 + step).toISOString()}`);
    },
    [times, setRange],
  );

  const onDrill = useCallback((f: Filter) => addFilter(f), [addFilter]);

  /* ── the readout bank ──────────────────────────────────────────────────── */

  const readout: Channel[] = METRICS.map((m) => ({
    key: m.key,
    label: m.label,
    value: ov ? m.fmt(ov[m.key]) : "—",
    delta: ov && before ? delta(ov[m.key], before[m.key]) : null,
    inverse: m.inverse,
    active: m.key === metric.key,
    onSelect: () => setMetricKey(m.key),
  }));

  const noRecord =
    ov != null && !ov.visitors && !ov.sessions && !ov.pageviews && active.length === 0;

  const heading = <h1 className="mf-sr">The record{domain ? ` for ${domain}` : ""}</h1>;

  if (noRecord) {
    return (
      <div className="mfr-screen">
        {heading}
        <FirstHit
          domain={domain}
          siteId={siteId}
          range={range}
          onRecheck={() => {
            void overview.refetch();
            void series.refetch();
          }}
        />
      </div>
    );
  }

  const windowNote = `${[rangeLabel(range), timezone, meta?.sampled ? "sampled" : null]
    .filter(Boolean)
    .join(" · ")}. Choose a channel below to write it on the drum.`;

  return (
    <div className="mfr-screen">
      {heading}

      {/* ── the patch bar ─────────────────────────────────────────────── */}
      <div className="mfr-bar">
        <div className="mfr-bar__audience">
          <AudienceSwitch value={audience} onChange={setAudience} />
        </div>

        <RangePicker value={range} onChange={setRange} />

        {isMeasuredRange(range) ? (
          <Button
            size="sm"
            icon={<IconClose size={12} />}
            aria-label={`Release the caliper and return to ${rangeLabel(namedRange.current)}`}
            onClick={() => setRange(namedRange.current)}
          >
            {rangeLabel(range)}
          </Button>
        ) : null}
      </div>

      {/* ── the record ────────────────────────────────────────────────── */}
      <Panel feed>
        <PanelHead
          title="The record"
          note={windowNote}
          actions={
            <Button
              size="sm"
              className="mfr-toggle"
              aria-pressed={ghost}
              onClick={() => setGhost((v) => !v)}
            >
              Previous period
            </Button>
          }
        />
        <div className="mfr-body">
          {series.isError ? (
            <ErrorState
              title="The trace did not come back"
              detail={errorDetail(series.error)}
              onRetry={() => void series.refetch()}
            />
          ) : (
            <Drum
              key={range}
              times={times}
              channels={channels}
              mode="stacked"
              scale="shared"
              height={channels.length > 1 ? 320 : 244}
              annotations={annotations}
              onMeasure={onMeasure}
              live={audience !== "agent"}
              emptyLabel={
                siteId == null || series.isPending
                  ? "Reading the record…"
                  : "No trace in this interval. Nothing was recorded."
              }
            />
          )}

          <div className="mfr-record__readout">
            {overview.isError ? (
              <ErrorState
                title="The figures did not come back"
                detail={errorDetail(overview.error)}
                onRetry={() => void overview.refetch()}
              />
            ) : (
              <>
                <p className="mfr-record__totals">
                  Totals for {rangeLabel(range)}. The figure beside each channel on the drum above
                  is a single sample: the interval under the cursor, or the last one written.
                </p>
                <Readout channels={readout} />
              </>
            )}
          </div>
        </div>
      </Panel>

      {/* ── what the machines did ─────────────────────────────────────── */}
      <MachinesStrip q={q} base={base} />

      {/* ── the bench ─────────────────────────────────────────────────── */}
      <TopSheets q={q} base={base} onDrill={onDrill} />
    </div>
  );
}

export default OverviewScreen;
