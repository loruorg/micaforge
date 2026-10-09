/**
 * Clicks on one short link: how many, from whom, and where they went.
 *
 * People and bots are counted apart and printed apart. A short link pasted
 * into a chat app is fetched by that app's preview bot before any person
 * clicks it, and campaign links are crawled by scanners; folding those into the
 * click count would make every link look busier than it is. So the totals say
 * "by people" and "by bots" in words, and the chart draws people as a solid
 * blue line and bots as a dashed line in a neutral ink. Bots here are not AI
 * traffic in this product's sense, so they never take the AI orange.
 *
 * The ranked lists share one shape: the value, its clicks, its share of the
 * list, and a share bar drawn against the leading row as on every other ranked
 * table. Referrers count people only, as the API reports them.
 */
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useState } from "react";

import { Empty, ErrorState, Panel, PanelHead, Readout } from "../../components/react/ui";
import type { Channel } from "../../components/react/ui";
import { api } from "../../lib/api";
import { num } from "../../lib/format";
import { titleCase } from "../display";
import { Bank, RankedList, WaitLines } from "./controls";
import { ChartWaiting, LineChart } from "./LineChart";
import { STATS_RANGES, TARGET_WORDS, countryName, localZone, problem } from "./model";
import type { Link, LinkStats as Stats, StatsRange } from "./model";

const RANGE_WORDS: Record<StatsRange, string> = {
  "24h": "in the last 24 hours",
  "7d": "in the last 7 days",
  "30d": "in the last 30 days",
  "90d": "in the last 90 days",
};

export function LinkStats({ link }: { link: Link }) {
  const [range, setRange] = useState<StatsRange>("7d");

  const stats = useQuery({
    queryKey: ["links", "stats", link.id, range],
    queryFn: () =>
      api
        .get<Stats>(
          `/api/links/${encodeURIComponent(link.id)}/stats?range=${range}&tz=${encodeURIComponent(localZone())}`,
        )
        .then((r) => r.data),
    placeholderData: keepPreviousData,
  });

  const data = stats.data;
  const channels: Channel[] = data
    ? [
        { key: "people", label: "Clicks by people", value: num(data.humans) },
        {
          key: "bots",
          label: "Clicks by bots",
          value: num(data.bots),
          note: "link previews, scanners and scripts",
        },
        { key: "all", label: "All clicks", value: num(data.total) },
      ]
    : [];

  return (
    <Panel className="mf-gr-stats">
      <PanelHead
        title={`Clicks on /${link.slug}`}
        note="People and bots are counted apart. Bots include the preview fetch a chat app makes when the link is pasted."
      />
      <div className="mf-gr-strip">
        <Bank<StatsRange>
          label="Period"
          value={range}
          options={STATS_RANGES.map((r) => ({ v: r.v, label: r.label }))}
          onChange={setRange}
        />
      </div>

      {stats.isPending ? (
        <div className="mf-gr-pad">
          <WaitLines lines={2} label="Loading clicks…" />
          <ChartWaiting />
        </div>
      ) : stats.isError ? (
        <ErrorState
          title="Could not load the clicks for this link"
          detail={problem(stats.error)}
          onRetry={() => void stats.refetch()}
        />
      ) : !data || data.total === 0 ? (
        <Empty
          title={`No clicks ${RANGE_WORDS[range]}`}
          body={`Clicks appear here when someone opens ${link.short_url.replace(/^https?:\/\//, "")}. Choose a longer period to look further back.`}
        />
      ) : (
        <div className="mf-gr-stats__body" aria-busy={stats.isFetching}>
          <Readout channels={channels} dense />
          <div className="mf-gr-pad">
            <LineChart
              label={`Clicks on /${link.slug} ${RANGE_WORDS[range]}`}
              times={data.series.map((p) => p.t)}
              integer
              format={(n) => num(n)}
              series={[
                {
                  key: "humans",
                  label: "People",
                  color: "var(--mf-people)",
                  values: data.series.map((p) => p.humans),
                },
                {
                  key: "bots",
                  label: "Bots",
                  color: "var(--mf-ink-3)",
                  dashed: true,
                  values: data.series.map((p) => p.bots),
                },
              ]}
            />
          </div>
          <div className="mf-gr-ranks">
            <RankedList
              title="Referrers"
              note="People only"
              rows={data.referrers}
              name={(v) => v || "Direct or unknown"}
              mono
              empty="No referrer was recorded."
            />
            <RankedList
              title="Countries"
              rows={data.countries}
              name={countryName}
              empty="No country was recorded."
            />
            <RankedList
              title="Devices"
              rows={data.devices}
              name={(v) => titleCase(v) || "Unknown"}
              empty="No device was recorded."
            />
            <RankedList
              title="Browsers"
              rows={data.browsers}
              name={(v) => titleCase(v) || "Unknown"}
              empty="No browser was recorded."
            />
            <RankedList
              title="Opened"
              note="Web, iOS, Android, app, preview card or expired page"
              rows={data.targets}
              name={(v) => TARGET_WORDS[v] ?? (titleCase(v) || "Unknown")}
              empty="No click was recorded."
            />
          </div>
        </div>
      )}
    </Panel>
  );
}
