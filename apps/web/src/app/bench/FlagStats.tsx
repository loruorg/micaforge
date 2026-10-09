/**
 * EVALUATIONS AND EXPOSURES: what SDKs reported about one flag.
 *
 * Two different counts, and the difference matters. An EVALUATION is an SDK
 * working out the flag's value; an EXPOSURE is the calling code saying the
 * variant was actually put in front of someone. A variant with many
 * evaluations and few exposures is being decided and then not shown, which is
 * usually a bug in the calling code rather than in the flag.
 *
 * Each variant is drawn in its own band, top to bottom in the order the flag
 * lists them, on ONE shared scale, so heights compare across bands. The band's
 * position and the named legend under it tell the variants apart; the tone
 * only repeats the swatch the rollout band uses, so the two views agree. The
 * table under the chart carries every total, so no figure depends on hovering.
 *
 * The rollout band in the editor reads the same answer (one request, one
 * cache entry), so "observed" there and the totals here never disagree.
 */
import { useState } from "react";
import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { Drum, type DrumChannel } from "../../components/react/Drum";
import { num, pct } from "../../lib/format";
import { KeyBank, Reading } from "./QueryScreen";
import "../../styles/app/flags.css";

/* ═══ the answer ══════════════════════════════════════════════════════════ */

export type StatsRange = "24h" | "7d" | "30d" | "90d";

export const STATS_RANGES: { v: StatsRange; label: string }[] = [
  { v: "24h", label: "24 hours" },
  { v: "7d", label: "7 days" },
  { v: "30d", label: "30 days" },
  { v: "90d", label: "90 days" },
];

export interface VariantTotals {
  /** The variant key; "on" for an on/off flag that evaluated on, "off" when it evaluated off. */
  variant: string;
  evaluations: number;
  exposures: number;
}

export interface FlagStatsData {
  key: string;
  range: { start: string; end: string };
  granularity: "hour" | "day";
  /** Bucket start times, epoch milliseconds, ascending. */
  buckets: number[];
  series: { variant: string; evaluations: number[]; exposures: number[] }[];
  totals: VariantTotals[];
}

export function useFlagStats(flagId: string, range: StatsRange) {
  return useQuery({
    queryKey: ["bench", "flag-stats", flagId, range],
    enabled: !!flagId,
    queryFn: () =>
      api
        .get<FlagStatsData>(`/api/flags/${encodeURIComponent(flagId)}/stats?range=${range}`)
        .then((r) => r.data),
  });
}

/** True when SDKs have reported nothing at all for the period. */
export function nothingReported(d: FlagStatsData | undefined): boolean {
  if (!d || !d.buckets.length) return true;
  return d.totals.every((t) => !t.evaluations && !t.exposures);
}

export function variantLabel(v: string): string {
  if (v === "off") return "off (evaluated off)";
  if (v === "on") return "on";
  return v || "(no variant)";
}

/* ═══ the panel ═══════════════════════════════════════════════════════════ */

type Metric = "evaluations" | "exposures";

export function FlagStats({
  query,
  range,
  onRange,
  order,
  toneOf,
}: {
  query: UseQueryResult<FlagStatsData>;
  range: StatsRange;
  onRange: (r: StatsRange) => void;
  /** The flag's own variant keys, in its order; anything else follows, and "off" comes last. */
  order: string[];
  toneOf: (variant: string) => string;
}) {
  const [metric, setMetric] = useState<Metric>("evaluations");
  const data = query.data;

  const rank = (v: string) => {
    if (v === "off") return Number.MAX_SAFE_INTEGER;
    const i = order.indexOf(v);
    return i === -1 ? order.length : i;
  };
  const series = [...(data?.series ?? [])].sort((a, b) => rank(a.variant) - rank(b.variant));
  const totals = [...(data?.totals ?? [])].sort((a, b) => rank(a.variant) - rank(b.variant));

  const channels: DrumChannel[] = series.map((s) => ({
    id: s.variant || "none",
    label: variantLabel(s.variant),
    color: toneOf(s.variant),
    pen: "written",
    values: s[metric],
    format: num,
  }));

  const all: VariantTotals = {
    variant: "",
    evaluations: totals.reduce((s, t) => s + t.evaluations, 0),
    exposures: totals.reduce((s, t) => s + t.exposures, 0),
  };
  const rows = totals.length > 1 ? [...totals, all] : totals;

  const columns: Column<VariantTotals>[] = [
    {
      key: "variant",
      head: "Variant",
      render: (t) =>
        t === all ? (
          <span className="mf-fl-total">All variants</span>
        ) : (
          <>
            <span className="mf-fl-swatch" style={{ background: toneOf(t.variant) }} aria-hidden="true" />
            <span className="mf-data">{variantLabel(t.variant)}</span>
          </>
        ),
    },
    { key: "eval", head: "Evaluations", numeric: true, render: (t) => <span className="mf-data">{num(t.evaluations)}</span> },
    { key: "exp", head: "Exposures", numeric: true, render: (t) => <span className="mf-data">{num(t.exposures)}</span> },
    {
      key: "rate",
      head: "Exposures per evaluation",
      numeric: true,
      render: (t) => (
        <span className="mf-data">{t.evaluations > 0 ? pct(t.exposures / t.evaluations) : "—"}</span>
      ),
    },
  ];

  const hourly = data?.granularity === "hour";

  return (
    <Panel>
      <PanelHead
        title="Evaluations and exposures"
        note="An evaluation is an SDK working out this flag's value. An exposure is your code reporting that the variant was shown. SDKs send both in batches, so the newest minutes fill in shortly after they happen."
        actions={
          <KeyBank label="Period" value={range} onChange={onRange} options={STATS_RANGES} />
        }
      />
      <div className="mf-fl-stats">
        <Reading
          loading={query.isPending}
          error={query.isError ? query.error : undefined}
          onRetry={() => void query.refetch()}
          isEmpty={nothingReported(data)}
          lines={4}
          empty={
            <Empty
              title="Nothing reported yet"
              body="SDKs report evaluations and exposures in batches; nothing has arrived for this flag in this period."
            />
          }
        >
          <div className="mf-fl-stats__bar">
            <KeyBank
              label="Count shown in the chart"
              value={metric}
              onChange={setMetric}
              options={[
                { v: "evaluations", label: "Evaluations" },
                { v: "exposures", label: "Exposures" },
              ]}
            />
            <span className="mf-bn-field__hint">
              One band per variant, all on one scale. Counted per {hourly ? "hour" : "day"}.
            </span>
          </div>
          <Drum
            times={data?.buckets ?? []}
            channels={channels}
            mode="stacked"
            scale="shared"
            height={Math.max(200, 76 * channels.length + 56)}
            formatTime={
              hourly
                ? (t) => new Date(t).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
                : undefined
            }
            emptyLabel="Nothing reported in this period."
          />
          <Sheet rows={rows} columns={columns} getKey={(t) => (t === all ? "__all" : t.variant || "__none")} />
        </Reading>
      </div>
    </Panel>
  );
}

export default FlagStats;
