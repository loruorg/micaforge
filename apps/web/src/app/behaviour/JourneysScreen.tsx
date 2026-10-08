/**
 * Journeys: where a session went after it arrived.
 *
 * The flow diagram carries the shape; the transitions listed under it carry
 * the same data in words and numbers, so nothing on this screen depends on being
 * able to see the diagram or on being able to use a pointer. Choosing a
 * transition in the list lifts its whole route in the diagram, and hovering a
 * ribbon marks the matching row.
 */
import { useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api, type BreakdownRow } from "../../lib/api";
import { useAppState } from "../state";
import { useSite } from "../providers";
import { Empty, ErrorState, Panel, PanelHead, Readout } from "../../components/react/ui";
import { IconOut } from "../../components/react/icons";
import { num, pct, shortPath } from "../../lib/format";
import { Sankey, linkKey, type SankeyLink, type SankeyNode } from "./Sankey";
import { Field, Select } from "./GoalEditor";
import { errorText } from "./EventsScreen";

interface JourneysPayload {
  nodes: SankeyNode[];
  links: SankeyLink[];
  depth: number;
  sessions: number;
}

const DEPTHS = ["2", "3", "4", "5", "6"].map((v) => ({
  value: v,
  label: `${v} steps`,
}));

export function JourneysScreen() {
  const { siteId } = useSite();
  const { range, audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);

  const [depth, setDepth] = useState("4");
  const [start, setStart] = useState("");
  /* Two separate things, deliberately: a route PINNED by choosing it, and a
     route merely under the pointer. Collapsing them would make a hovered row
     report itself as pressed, which is a lie a screen reader repeats. */
  const [pinned, setPinned] = useState<string | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);
  const lit = hovered ?? pinned;

  /* The entry pages a journey can be anchored to. */
  const entries = useQuery({
    queryKey: ["stats", "breakdown", siteId, range, audience, filtersJson, "entry_path"],
    queryFn: () =>
      api.get<BreakdownRow[]>("/api/stats/breakdown", {
        site: siteId as number,
        range,
        audience,
        filters,
        dimension: "entry_path",
        limit: 25,
      }),
    enabled: siteId != null,
  });

  const journeys = useQuery({
    queryKey: ["stats", "journeys", siteId, range, audience, filtersJson, depth, start],
    queryFn: () =>
      api.get<JourneysPayload>("/api/stats/journeys", {
        site: siteId as number,
        range,
        audience,
        filters,
        depth: Number(depth),
        start: start || undefined,
      }),
    enabled: siteId != null,
    placeholderData: keepPreviousData,
  });

  const payload = journeys.data?.data;
  const nodes = payload?.nodes ?? [];
  const links = payload?.links ?? [];

  /* Two nodes can carry the same page at different depths, so a hop is only
     unambiguous with the step it runs between. */
  const nodeOf = useMemo(() => {
    const m = new Map(nodes.map((n) => [n.id, n]));
    return (id: string) => m.get(id);
  }, [nodes]);
  const label = (id: string) => nodeOf(id)?.label ?? id;
  const depths = useMemo(() => {
    const sorted = [...new Set(nodes.map((n) => n.depth))].sort((a, b) => a - b);
    return (id: string) => sorted.indexOf(nodeOf(id)?.depth ?? 0) + 1;
  }, [nodes, nodeOf]);

  /* Only hops the diagram can actually lift belong in a list whose whole job is
     to lift them. A hop that runs back to an earlier step is real, and it is
     reported under the diagram, but it is not offered as a control here. */
  const forward = useMemo(
    () => links.filter((l) => depths(l.target) > depths(l.source)),
    [links, depths],
  );
  const transitions = useMemo(
    () => [...forward].sort((a, b) => b.value - a.value).slice(0, 12),
    [forward],
  );

  const busiest = Math.max(1, transitions[0]?.value ?? 1);

  return (
    <div className="mfb-stack">
      <Panel>
        <PanelHead
          title="Journeys"
          note="Every route a session took, from where it landed to where it stopped."
          actions={
            <div className="mfb-tools">
              <Field id="mfb-journey-depth" label="Depth">
                <Select
                  id="mfb-journey-depth"
                  value={depth}
                  onChange={setDepth}
                  options={DEPTHS}
                />
              </Field>
              <Field id="mfb-journey-start" label="Starting page">
                <Select
                  id="mfb-journey-start"
                  value={start}
                  onChange={setStart}
                  disabled={entries.isPending || entries.isError}
                  options={[
                    {
                      value: "",
                      label: entries.isError ? "Entry pages unavailable" : "Any entry page",
                    },
                    ...(entries.data?.data ?? []).map((r) => ({
                      value: r.key,
                      label: shortPath(r.label || r.key, 40),
                    })),
                  ]}
                />
              </Field>
            </div>
          }
        />

        {/* An em dash means "not loaded yet"; once the API has answered, a
            genuine nothing is written as 0. Conflating the two would let a
            failed load pass for an empty one. */}
        <Readout
          channels={[
            { key: "sessions", label: "Sessions traced", value: num(payload?.sessions) },
            {
              key: "pages",
              label: "Pages",
              value: payload ? num(nodes.length) : "—",
              note: "in these journeys",
            },
            {
              key: "links",
              label: "Transitions",
              value: payload ? num(links.length) : "—",
            },
            { key: "depth", label: "Steps deep", value: num(payload?.depth) },
          ]}
        />

        {journeys.isError ? (
          <ErrorState
            title="Could not load journeys"
            detail={errorText(journeys.error)}
            onRetry={() => void journeys.refetch()}
          />
        ) : journeys.isPending ? (
          <p className="mfb-note" aria-live="polite">
            Loading journeys…
          </p>
        ) : nodes.length === 0 ? (
          <Empty
            title="No journeys in this period"
            body={
              start
                ? "No session started on that page in this period. Choose another starting page or a longer period."
                : "A route needs a session that saw at least two pages. Once one does, its path is drawn here."
            }
          />
        ) : (
          <div className="mfb-inset">
            <Sankey
              nodes={nodes}
              links={links}
              highlight={lit}
              onHighlight={setHovered}
              maxPerColumn={10}
            />
          </div>
        )}
      </Panel>

      {nodes.length > 0 && transitions.length > 0 ? (
        <Panel>
          <PanelHead
            title="Strongest transitions"
            note="The same data as single steps. Choose one to highlight its whole route in the diagram above."
          />

          <div className="mfb-routes">
            {transitions.map((t) => {
              const key = linkKey(t.source, t.target);
              return (
                <button
                  key={key}
                  type="button"
                  className="mfb-route"
                  data-active={lit === key ? "true" : undefined}
                  aria-pressed={pinned === key}
                  onClick={() => setPinned((v) => (v === key ? null : key))}
                  onMouseEnter={() => setHovered(key)}
                  onMouseLeave={() => setHovered((v) => (v === key ? null : v))}
                  onFocus={() => setHovered(key)}
                  onBlur={() => setHovered((v) => (v === key ? null : v))}
                >
                  <span
                    className="mfb-reg__ink"
                    style={{ width: `${(t.value / busiest) * 100}%` }}
                    aria-hidden="true"
                  />
                  <span className="mfb-route__step" aria-hidden="true">
                    {depths(t.source)}–{depths(t.target)}
                  </span>
                  <span className="mfb-route__hop">
                    <span title={label(t.source)}>{shortPath(label(t.source), 30)}</span>
                    <IconOut size={12} className="mfb-route__arrow" />
                    <span title={label(t.target)}>{shortPath(label(t.target), 30)}</span>
                    <span className="mf-sr">
                      {` from step ${depths(t.source)} to step ${depths(t.target)}`}
                    </span>
                  </span>
                  <span className="mfb-route__fig">{num(t.value)}</span>
                  <span className="mfb-route__fig mfb-route__fig--dim mfb-col-2">
                    {pct(t.value / Math.max(1, payload?.sessions ?? 1))}
                  </span>
                </button>
              );
            })}
          </div>

          <p className="mfb-note">
            Share is measured against the {num(payload?.sessions)} sessions traced in this
            period.
            {links.length > forward.length
              ? ` ${num(links.length - forward.length)} hop${
                  links.length - forward.length === 1 ? " that returned" : "s that returned"
                } to an earlier step ${
                  links.length - forward.length === 1 ? "is" : "are"
                } counted in the diagram but ${
                  links.length - forward.length === 1 ? "is" : "are"
                } not listed here, because there is no single route to highlight.`
              : ""}
          </p>
        </Panel>
      ) : null}
    </div>
  );
}

export default JourneysScreen;
