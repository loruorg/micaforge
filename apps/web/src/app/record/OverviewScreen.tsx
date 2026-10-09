/**
 * The dashboard: the site overview, and the product's home.
 *
 * Top to bottom: the audience and range controls, the traffic chart, the
 * metric totals under it (selecting one charts it), and the breakdown tables
 * below.
 *
 * The chart also carries the annotations people wrote ("Launched v2"), drawn
 * as flagged rules beside the automatic notes it reads off the data. A click
 * on one sample opens the annotation form at that time, and the list under
 * the chart is where every note in the period can be read in full, changed or
 * removed (see Annotations.tsx).
 *
 * The AI traffic panel sits deliberately high on the page. People numbers are
 * available anywhere; the count of what the crawlers took, and how little of it
 * came back, is the reason this product exists, so it is not buried at the
 * bottom next to the browser table.
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
import { IconClose, IconOut, IconPlus } from "../../components/react/icons";
import {
  AnnotationForm,
  AnnotationList,
  annotationMarks,
  blankDraft,
  draftFrom,
  toLocalInput,
  useAnnotations,
} from "./Annotations";
import type { AnnotationDraft } from "./Annotations";
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
import { SamplingNote } from "../signals/SamplingNote";
import "../../styles/app/record.css";

/* ── the six metrics under the chart ─────────────────────────────────────── */

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
  /** What the chart prints for one sample of this metric. */
  write: (v: number) => string;
  /**
   * A rate crosses the wire as 0..1, and the chart's axis is written in whole
   * numbers, so a rate is handed to the chart as 0..100 and formatted back on
   * the way out. Without this the axis reads 0, 0, 1, 1.
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

/* ── AI traffic ──────────────────────────────────────────────────────────── */

function AiTrafficPanel({ q, base }: { q: RecordQuery; base: string }) {
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
      label: "Crawler fetches",
      value: waiting ? "—" : num(a?.fetches),
      delta: a ? asFraction(a.delta_pct) : null,
    },
    {
      key: "agents",
      label: "AI crawlers seen",
      value: waiting ? "—" : num(a?.unique_agents),
      note: verified == null ? undefined : `${pct(verified)} verified`,
    },
    {
      key: "arrivals",
      label: "Visits from AI answers",
      value: waiting ? "—" : num(g?.answer_arrivals),
      note: "people sent by an AI answer",
    },
    {
      key: "gap",
      label: "Citation gap",
      value: gapReading,
      note:
        waiting || g == null
          ? undefined
          : g.answer_arrivals > 0
            ? "fetches for each visit"
            : g.agent_fetches > 0
              ? "nothing has come back yet"
              : "no crawl to measure yet",
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="AI traffic"
        note="Pages fetched by named AI crawlers in the same period, and how many people an AI answer sent back."
        actions={
          <>
            <Link className="mf-btn mf-btn--quiet mf-btn--sm mfr-key" to={`${base}/agents`}>
              <span className="mf-btn__label">AI crawlers</span>
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
            title="Could not load AI traffic"
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
      label: "People",
      color: "var(--mf-people)",
      pen: "written",
      values: demo.readers,
      format: compact,
    },
    {
      id: "machines",
      label: "AI crawler fetches",
      color: "var(--mf-ai)",
      pen: "stepped",
      values: demo.machines,
      format: compact,
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="Waiting for the first visit"
        note={`${rangeLabel(range)}: no data yet for ${domain ?? "this site"}. Add the tracker and data starts with the next pageview.`}
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
              One tag, no cookie, no consent banner. AI crawler fetches are counted on the server, so
              crawlers are counted whether or not they run this script.
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
              Example data. This is the shape the two series make, not traffic to{" "}
              {domain ?? "this site"}.
            </DemoNotice>
          </div>
        </div>
      </div>
    </Panel>
  );
}

/** The comparison series' own line, dashed, so the toggle doubles as its legend. */
function CompareMark() {
  return (
    <svg width="16" height="8" viewBox="0 0 16 8" aria-hidden="true" focusable="false">
      <path d="M0 4h16" stroke="currentColor" strokeWidth="1.5" strokeDasharray="3 2.5" fill="none" />
    </svg>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function OverviewScreen() {
  const { range, setRange, audience, setAudience, filters, addFilter } = useRecordState();
  const { siteId, domain, timezone } = useRecordSite();
  const { site } = useParams();

  const [metricKey, setMetricKey] = useState<MetricKey>("visitors");
  const [ghost, setGhost] = useState(true);
  /** The annotation being written or changed; null while the form is closed. */
  const [draft, setDraft] = useState<AnnotationDraft | null>(null);

  /** The named range to go back to when a range selected on the chart is cleared. */
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

  /* ── the chart series ──────────────────────────────────────────────────── */

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

  /* The chart legend prints the value of ONE sample: the one under the
     cursor, or the latest one. The totals under the chart print the whole
     period. Two different measurements, so neither is allowed to wear the
     bare metric name on its own. */
  const channels: DrumChannel[] = [
    {
      id: metric.key,
      label: `${metric.label}, this sample`,
      color: audience === "agent" ? "var(--mf-ai)" : "var(--mf-people)",
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

  /** Annotations are read off the series itself. */
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
    /* The peak is deliberately NOT noted here. The chart notes it in the
       margin itself when a value runs off the top of the scale, and it is the
       only one that knows the true value, so writing a second "Peak" from this
       side put two identical marks on one sample. `peakAt` is still read, to
       keep the two notes below off the same bucket. */

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

  /* The notes people wrote, placed on the same samples, after the automatic
     ones. The chart draws them as a different mark (see Drum.tsx). */
  const notes = useAnnotations(siteId, range, timezone);
  const chartNotes = useMemo<DrumAnnotation[]>(
    () => [...annotations, ...annotationMarks(times, notes.data ?? [])],
    [annotations, times, notes.data],
  );

  /** A click on one sample opens the annotation form at that sample's time. */
  const onPickSample = useCallback(
    (i: number) => {
      const t = times[i];
      if (t == null) return;
      setDraft((d) => (d ? { ...d, start: toLocalInput(t) } : blankDraft(t)));
    },
    [times],
  );

  /** A range dragged on the chart becomes the date range. */
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

  /* ── the metric totals ───────────────────────────────────────────────────── */

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

  const heading = <h1 className="mf-sr">Dashboard{domain ? ` for ${domain}` : ""}</h1>;

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
    .join(" · ")}. Select a metric below to chart it.`;

  return (
    <div className="mfr-screen">
      {heading}

      {/* ── audience and range ────────────────────────────────────────── */}
      <div className="mfr-bar">
        <div className="mfr-bar__audience">
          <AudienceSwitch value={audience} onChange={setAudience} />
        </div>

        <RangePicker value={range} onChange={setRange} />

        {isMeasuredRange(range) ? (
          <Button
            size="sm"
            icon={<IconClose size={12} />}
            aria-label={`Clear the selected range and go back to ${rangeLabel(namedRange.current)}`}
            onClick={() => setRange(namedRange.current)}
          >
            {rangeLabel(range)}
          </Button>
        ) : null}
      </div>

      {/* Says so when the tracker keeps only some visitors (data-sample). */}
      <SamplingNote />

      {/* ── traffic chart ─────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Traffic"
          note={windowNote}
          actions={
            <>
              <Button
                size="sm"
                className="mfr-toggle"
                icon={<CompareMark />}
                aria-pressed={ghost}
                onClick={() => setGhost((v) => !v)}
              >
                Compare to previous period
              </Button>
              <Button
                size="sm"
                icon={<IconPlus size={12} />}
                aria-expanded={draft != null}
                disabled={siteId == null}
                onClick={() =>
                  setDraft((d) => d ?? blankDraft(times[times.length - 1] ?? Date.now()))
                }
              >
                Add annotation
              </Button>
            </>
          }
        />
        <div className="mfr-body">
          {series.isError ? (
            <ErrorState
              title="Could not load the chart"
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
              annotations={chartNotes}
              onMeasure={onMeasure}
              onPickSample={onPickSample}
              live={audience !== "agent"}
              emptyLabel={
                siteId == null || series.isPending
                  ? "Loading…"
                  : "No data for this period."
              }
            />
          )}

          {draft && siteId != null ? (
            <AnnotationForm
              key={draft.id ?? "new"}
              siteId={siteId}
              draft={draft}
              onDraft={setDraft}
              onDone={() => setDraft(null)}
            />
          ) : null}
          {siteId != null ? (
            <AnnotationList query={notes} siteId={siteId} onEdit={(a) => setDraft(draftFrom(a))} />
          ) : null}

          <div className="mfr-record__readout">
            {overview.isError ? (
              <ErrorState
                title="Could not load the totals"
                detail={errorDetail(overview.error)}
                onRetry={() => void overview.refetch()}
              />
            ) : (
              <>
                <p className="mfr-record__totals">
                  Totals for {rangeLabel(range)}. The figure in the chart legend is a single
                  sample: the one under the cursor, or the latest one.
                </p>
                <Readout channels={readout} />
              </>
            )}
          </div>
        </div>
      </Panel>

      {/* ── AI traffic ────────────────────────────────────────────────── */}
      <AiTrafficPanel q={q} base={base} />

      {/* ── breakdown tables ──────────────────────────────────────────── */}
      <TopSheets q={q} base={base} onDrill={onDrill} />
    </div>
  );
}

export default OverviewScreen;
