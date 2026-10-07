/**
 * Retention: how many of the people first seen in one interval came back.
 *
 * Two choices, both real: the cohort unit (weekly or monthly), which is asked of
 * the server, and the reading (share or count), which is computed here from the
 * counts already in hand: so switching it costs nothing and the two readings
 * can never disagree with each other.
 */
import { useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { useAppState } from "../state";
import { useSite } from "../providers";
import { Empty, ErrorState, Panel, PanelHead, Readout } from "../../components/react/ui";
import { num, pct } from "../../lib/format";
import { CohortGrid, type Cohort, type CohortMode, type CohortUnit } from "./CohortGrid";
import { errorText } from "./EventsScreen";

interface RetentionPayload {
  unit: CohortUnit;
  periods: number;
  cohorts: Cohort[];
  average?: (number | null)[];
}

export function RetentionScreen() {
  const { siteId, timezone } = useSite();
  const { range, audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);

  const [unit, setUnit] = useState<CohortUnit>("week");
  const [mode, setMode] = useState<CohortMode>("share");

  const q = useQuery({
    queryKey: ["stats", "retention", siteId, range, audience, filtersJson, unit],
    queryFn: () =>
      api.get<RetentionPayload>("/api/stats/retention", {
        site: siteId as number,
        range,
        audience,
        filters,
        granularity: unit,
      }),
    enabled: siteId != null,
    placeholderData: keepPreviousData,
  });

  const payload = q.data?.data;
  const cohorts = payload?.cohorts ?? [];
  const unitWord = unit === "week" ? "week" : "month";

  /* Averages, computed here when the server did not send them, so the footer
     row is never missing and never invented. */
  const average = useMemo(() => {
    if (payload?.average?.length) return payload.average;
    if (!cohorts.length) return undefined;
    const periods = payload?.periods ?? 0;
    return Array.from({ length: periods }, (_, i) => {
      let people = 0;
      let back = 0;
      for (const c of cohorts) {
        const v = c.values[i];
        if (v == null) continue;
        people += c.size;
        back += v;
      }
      return people > 0 ? back / people : null;
    });
  }, [payload, cohorts]);

  const people = cohorts.reduce((a, c) => a + c.size, 0);
  const firstReturn = average?.[1] ?? null;
  const lastIndex = (average?.length ?? 0) - 1;
  const lastReturn = lastIndex > 1 ? (average?.[lastIndex] ?? null) : null;

  return (
    <div className="mfb-stack">
      <Panel ruled feed>
        <PanelHead
          title="Retention"
          note={`Visitors grouped by the ${unitWord} they were first seen${
            timezone ? `, in ${timezone}` : ""
          }.`}
          actions={
            <div className="mfb-tools">
              <div className="mfb-bank" role="group" aria-label="Cohort unit">
                <button
                  type="button"
                  className="mfb-bank__key"
                  aria-pressed={unit === "week"}
                  onClick={() => setUnit("week")}
                >
                  Weekly
                </button>
                <button
                  type="button"
                  className="mfb-bank__key"
                  aria-pressed={unit === "month"}
                  onClick={() => setUnit("month")}
                >
                  Monthly
                </button>
              </div>

              <div className="mfb-bank" role="group" aria-label="Reading">
                <button
                  type="button"
                  className="mfb-bank__key"
                  aria-pressed={mode === "share"}
                  onClick={() => setMode("share")}
                >
                  Share
                </button>
                <button
                  type="button"
                  className="mfb-bank__key"
                  aria-pressed={mode === "count"}
                  onClick={() => setMode("count")}
                >
                  People
                </button>
              </div>
            </div>
          }
        />

        <Readout
          channels={[
            {
              key: "cohorts",
              label: `${unit === "week" ? "Weekly" : "Monthly"} cohorts`,
              /* An em dash is "not read yet"; a measured nothing is 0. */
              value: payload ? num(cohorts.length) : "—",
            },
            {
              key: "people",
              label: "People measured",
              value: payload ? num(people) : "—",
            },
            {
              key: "first",
              label: `Held at ${unitWord} 1`,
              value: firstReturn == null ? "—" : pct(firstReturn, 1),
              note: "across cohorts",
            },
            {
              key: "last",
              label: lastIndex > 1 ? `Held at ${unitWord} ${lastIndex}` : "Longest reading",
              value: lastReturn == null ? "—" : pct(lastReturn, 1),
              note: lastIndex > 1 ? "across cohorts" : undefined,
            },
          ]}
        />

        {q.isError ? (
          <ErrorState
            title="The cohorts did not come back"
            detail={errorText(q.error)}
            onRetry={() => void q.refetch()}
          />
        ) : q.isPending ? (
          <p className="mfb-note" aria-live="polite">
            Reading the cohorts…
          </p>
        ) : cohorts.length === 0 ? (
          <Empty
            title="Not enough record to form a cohort"
            body={`A cohort needs at least one full ${unitWord} of traffic before it can be measured against the ${unitWord} after it. Widen the interval, or come back once this site has been recording longer.`}
          />
        ) : (
          <CohortGrid
            cohorts={cohorts}
            periods={payload?.periods ?? 0}
            unit={unit}
            mode={mode}
            average={average}
          />
        )}
      </Panel>
    </div>
  );
}

export default RetentionScreen;
