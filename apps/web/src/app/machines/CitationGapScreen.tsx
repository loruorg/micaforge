/**
 * The Citation gap, at /app/:site/citation-gap
 *
 * The reason this product exists, in one image: AI crawler fetches drawn up
 * from the centre line and visits from AI answers drawn down from it. The
 * difference between the two sides IS the report.
 *
 * The screen deliberately ends in an instruction rather than a number. A ratio
 * nobody can act on is a statistic; a ratio with a decision attached is a
 * report.
 */
import { Fragment, useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { useQuery, keepPreviousData } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { GapRow, GapSummary, GapVerdict } from "../../lib/api";
import {
  Button,
  DemoNotice,
  Empty,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  Verdict,
} from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { Drum } from "../../components/react/Drum";
import { IconOut } from "../../components/react/icons";
import { compact, delta, num, shortPath } from "../../lib/format";
import { gapRatio } from "../display";
import { demoRecord } from "../../lib/demo";
import { Bench, Reading, Waiting, sorted, useMachineEnv, useSort } from "./AgentDetail";
import type { MachineEnv } from "./AgentDetail";

const VERDICTS: {
  v: GapVerdict;
  means: string;
  action: string;
}[] = [
  {
    v: "extracted",
    means: "Fetched often by AI crawlers, almost never cited back.",
    action:
      "Decide whether you want to be in that corpus. Either tighten robots.txt for the operators taking the most, or make the page quotable enough to earn the citation: a clear answer near the top, named and dated.",
  },
  {
    v: "compounding",
    means: "Crawled, and the crawls come back as visitors.",
    action: "This is the shape that works. Write more pages like these, and keep them current so the re-crawl stays worth it.",
  },
  {
    v: "invisible",
    means: "Cited more than it is crawled: the AI tools know it from elsewhere.",
    action:
      "The AI tools are quoting a copy, not your page. Put it in the sitemap and in llms.txt so the current version is the one they read.",
  },
  {
    v: "quiet",
    means: "Too few crawls or visits to judge.",
    action: "Nothing to do yet. It will move into one of the other three once it is crawled or found.",
  },
];

/* ── the chart ──────────────────────────────────────────────────────────── */

function GapRecord({ summary }: { summary: ReturnType<typeof useGapSummary> }) {
  const [gain, setGain] = useState<"shared" | "per-channel">("shared");

  const record = useMemo(() => {
    const trend = summary.data?.trend ?? [];
    return {
      times: trend.map((t) => t[0]),
      taken: trend.map((t) => t[1]),
      returned: trend.map((t) => t[2]),
    };
  }, [summary.data]);

  const annotations = useMemo(() => {
    const { taken, returned } = record;
    if (!taken.length) return [];
    const out: { at: number; label: string; tone?: "neutral" | "warn" | "good" }[] = [];
    /* No peak note here. The chart writes its own when the crawler series runs
       off the top of the scale, and it is the only one that knows the true
       value. These are the notes the chart cannot read for itself. */
    const first = returned.findIndex((v) => v > 0);
    if (first >= 0) out.push({ at: first, label: "First visit from AI", tone: "good" });
    let best = -1;
    for (let i = 0; i < returned.length; i++) if (returned[i] > 0 && (best < 0 || returned[i] > returned[best])) best = i;
    if (best >= 0 && best !== first) out.push({ at: best, label: `Best: ${compact(returned[best])} visits`, tone: "good" });
    return out;
  }, [record]);

  /* The example chart shown when nothing has been measured yet. It is
     labelled twice (once by the notice, once inside the chart's own legend)
     because an example figure that could be mistaken for measured traffic is
     the one thing this product may never ship. */
  const demo = useMemo(() => demoRecord(45, 20260821, Date.UTC(2026, 7, 21)), []);

  return (
    <Panel>
      <PanelHead
        title="Crawled vs. visitors sent back"
        note={
          /* The note has to follow the scale, because on one scale the distance
             between the two series IS the quantity and on separate scales it is
             not. A fixed sentence made the panel contradict the chart's own
             note directly below it. */
          gain === "shared"
            ? "Above the line: every page fetch by an AI crawler. Below the line: every visit from an AI answer. Both share one scale, so the hatched area between them is what was taken and never came back."
            : "Above the line: every page fetch by an AI crawler. Below the line: every visit from an AI answer. Each side has its own scale, so compare the shapes here and read the size of the gap from the totals below."
        }
        actions={
          <div className="mf-pivot" role="group" aria-label="Scale">
            <button
              type="button"
              className="mf-pivot__key"
              aria-pressed={gain === "shared"}
              onClick={() => setGain("shared")}
              title="Both series on one scale, with the difference between them hatched. Shows the true difference in size."
            >
              Shared scale
            </button>
            <button
              type="button"
              className="mf-pivot__key"
              aria-pressed={gain === "per-channel"}
              onClick={() => setGain("per-channel")}
              title="Each series scaled to its own maximum, with that maximum shown in the legend. Shows the shape of both."
            >
              Separate scales
            </button>
          </div>
        }
      />
      <div className="mf-wedge__plot">
        <Reading
          query={summary}
          waiting={<Waiting rows={6} plot />}
          isEmpty={(d) => !d?.trend?.length || !d.trend.some((t) => t[1] || t[2])}
          empty={
            <div className="mf-wedge-demo">
              <Empty
                title="No crawls or AI visits in this period"
                body="Crawler fetches come from the server-side ingest path. Visits from AI answers come from referrals by ChatGPT, Perplexity, Claude, Gemini and similar tools. With both reporting, this is the report you get."
              />
              <DemoNotice>
                Below is example data, not this site. Every figure in it is an example.
              </DemoNotice>
              <Drum
                demo
                times={demo.times}
                mode="mirrored"
                scale="per-channel"
                height={260}
                channels={[
                  {
                    id: "demo-taken",
                    label: "AI crawler fetches, this sample",
                    color: "var(--mf-ai)",
                    pen: "stepped",
                    values: demo.machines,
                    format: (n) => num(n),
                  },
                  {
                    id: "demo-returned",
                    label: "Visits from AI answers, this sample",
                    color: "var(--mf-people)",
                    pen: "written",
                    values: demo.answers,
                    format: (n) => num(n),
                  },
                ]}
              />
            </div>
          }
        >
          {() => (
            /* The scale is explained once, by the chart itself, directly
               under the series it governs. A second copy of the same sentence
               used to sit here and ran into the first one on a narrow screen. */
            <Drum
              times={record.times}
              mode="mirrored"
              scale={gain}
              height={380}
              live
              annotations={annotations}
              channels={[
                {
                  id: "taken",
                  label: "AI crawler fetches, this sample",
                  color: "var(--mf-ai)",
                  pen: "stepped",
                  values: record.taken,
                  format: (n) => num(n),
                },
                {
                  id: "returned",
                  label: "Visits from AI answers, this sample",
                  color: "var(--mf-people)",
                  pen: "written",
                  values: record.returned,
                  format: (n) => num(n),
                },
              ]}
            />
          )}
        </Reading>
      </div>
    </Panel>
  );
}

/* ── the totals ─────────────────────────────────────────────────────────── */

function useGapSummary(bench: MachineEnv) {
  return useQuery({
    queryKey: bench.key("citation-gap/summary"),
    queryFn: async () => (await api.get<GapSummary>("/api/citation-gap/summary", bench.env)).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });
}

function GapTotals({ summary }: { summary: ReturnType<typeof useGapSummary> }) {
  /** The trend is read off the data itself: the ratio in the second half of
      the period against the ratio in the first. A falling ratio is the good
      outcome, so the metric is marked inverse. */
  const trend = useMemo(() => {
    const t = summary.data?.trend ?? [];
    if (t.length < 4) return null;
    const mid = Math.floor(t.length / 2);
    const sum = (from: number, to: number, i: 1 | 2) => t.slice(from, to).reduce((a, r) => a + (r[i] ?? 0), 0);
    const early = { taken: sum(0, mid, 1), back: sum(0, mid, 2) };
    const late = { taken: sum(mid, t.length, 1), back: sum(mid, t.length, 2) };
    if (!early.back || !late.back) return null;
    return delta(late.taken / late.back, early.taken / early.back);
  }, [summary.data]);

  return (
    <Panel className="mf-panel--bank">
      <Reading
        query={summary}
        waiting={<Waiting rows={2} />}
        isEmpty={(d) => !d}
        empty={<Empty title="No data yet" body="The totals fill in with the first crawler fetch and the first visit from an AI answer." />}
      >
        {(d) => (
          <>
            {/* The chart legend above prints one sample per series. These are
                the totals for the whole period, and they say so, because two
                different measurements under one name is a bug. */}
            <p className="mf-wedge__caption">
              Totals for the whole period. The chart legend above shows a single sample: the one
              under the cursor, or the latest one.
            </p>
            <Readout
              channels={[
                { key: "taken", label: "AI crawler fetches", value: num(d.agent_fetches), note: "pages taken" },
                { key: "back", label: "Visits from AI answers", value: num(d.answer_arrivals), note: "people sent back" },
                {
                  key: "ratio",
                  label: "Fetches per visit",
                  value: gapRatio(d.agent_fetches, d.answer_arrivals),
                  delta: trend,
                  inverse: true,
                  note: "vs. the first half of the period",
                },
                {
                  key: "pages",
                  label: "Pages judged",
                  value: num(d.verdict_counts ? Object.values(d.verdict_counts).reduce((a, b) => a + b, 0) : 0),
                },
              ]}
            />
          </>
        )}
      </Reading>
    </Panel>
  );
}

/* ── the verdicts ────────────────────────────────────────────────────────── */

function VerdictBank({
  summary,
  selected,
  onSelect,
}: {
  summary: ReturnType<typeof useGapSummary>;
  selected: GapVerdict | null;
  onSelect: (v: GapVerdict | null) => void;
}) {
  const counts = summary.data?.verdict_counts;
  const total = counts ? Object.values(counts).reduce((a: number, b: number) => a + b, 0) : 0;

  return (
    <Panel className="mf-panel--bank">
      <PanelHead
        title="Verdicts"
        note="Every known page falls into one of four groups. Select one to show only those pages in the table below."
        actions={
          selected ? (
            <Button size="sm" tone="quiet" onClick={() => onSelect(null)}>
              Show all four
            </Button>
          ) : undefined
        }
      />
      <Reading
        query={summary}
        waiting={<Waiting rows={3} />}
        isEmpty={(d) => !d?.verdict_counts}
        empty={
          <Empty
            title="No page has enough data to judge"
            body="A page is judged once an AI crawler has fetched it or an AI answer has sent a visitor to it."
          />
        }
      >
        {(d) => (
          <div className="mf-verdicts">
            {VERDICTS.map(({ v, means }) => {
              const n = d.verdict_counts?.[v] ?? 0;
              const on = selected === v;
              return (
                <button
                  key={v}
                  type="button"
                  className="mf-verdicts__cell"
                  aria-pressed={on}
                  onClick={() => onSelect(on ? null : v)}
                >
                  <Verdict value={v} />
                  <span className="mf-verdicts__n mf-data">{num(n)}</span>
                  <span className="mf-verdicts__of">
                    {total ? `${Math.round((n / total) * 100)}% of pages · ${means}` : means}
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </Reading>
    </Panel>
  );
}

/* ── the per-page table ─────────────────────────────────────────────────── */

type SortKey = "path" | "taken" | "back" | "humans" | "ratio" | "verdict";

function GapSheet({
  bench,
  selected,
  onSelect,
}: {
  bench: MachineEnv;
  selected: GapVerdict | null;
  onSelect: (v: GapVerdict | null) => void;
}) {
  const sort = useSort<SortKey>("taken");
  const { domain } = bench;

  const q = useQuery({
    queryKey: bench.key("citation-gap"),
    queryFn: async () => (await api.get<GapRow[]>("/api/citation-gap", { ...bench.env, limit: 200 })).data,
    enabled: bench.ready,
    placeholderData: keepPreviousData,
  });

  const maxTaken = useMemo(
    () => (q.data ?? []).reduce((m, r) => (r.agent_fetches > m ? r.agent_fetches : m), 0),
    [q.data],
  );

  const rows = useMemo(() => {
    const base = (q.data ?? []).filter((r) => !selected || r.verdict === selected);
    return sorted(
      base,
      (r) =>
        sort.by === "path"
          ? r.path
          : sort.by === "taken"
            ? r.agent_fetches
            : sort.by === "back"
              ? r.answer_arrivals
              : sort.by === "humans"
                ? r.human_pageviews
                : sort.by === "verdict"
                  ? r.verdict
                  : // A page that returned nothing has the largest gap there is.
                    (r.answer_arrivals ? r.agent_fetches / r.answer_arrivals : Number.MAX_SAFE_INTEGER),
      sort.dir,
    );
  }, [q.data, selected, sort.by, sort.dir]);

  const columns: Column<GapRow>[] = [
    {
      key: "path",
      head: sort.head("path", "Page"),
      width: "34%",
      render: (r) => (
        <span className="mf-wedge-stack">
          {domain ? (
            <a
              className="mf-path mf-cellkey"
              href={`https://${domain}${r.path}`}
              target="_blank"
              rel="noreferrer noopener"
              title={`Open ${r.path} in a new tab`}
            >
              {shortPath(r.path, 40)}
              <IconOut size={11} className="mf-dim" />
            </a>
          ) : (
            <span className="mf-path" title={r.path}>
              {shortPath(r.path, 40)}
            </span>
          )}
          {r.title ? <span className="mf-wedge-stack__sub">{r.title}</span> : null}
        </span>
      ),
      /* The bar behind the cell is the page's share of the most-crawled page. */
      share: (r) => (maxTaken ? r.agent_fetches / maxTaken : undefined),
    },
    {
      key: "taken",
      head: sort.head("taken", "Crawler fetches"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.agent_fetches)}</span>,
    },
    {
      key: "back",
      head: sort.head("back", "AI visits"),
      numeric: true,
      render: (r) => (
        <span className={r.answer_arrivals ? "mf-data" : "mf-data mf-dim"}>{num(r.answer_arrivals)}</span>
      ),
    },
    {
      key: "humans",
      head: sort.head("humans", "Pageviews"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.human_pageviews)}</span>,
    },
    {
      key: "ratio",
      head: sort.head("ratio", "Fetches per visit"),
      numeric: true,
      render: (r) => <span className="mf-data">{gapRatio(r.agent_fetches, r.answer_arrivals)}</span>,
    },
    {
      key: "verdict",
      head: sort.head("verdict", "Verdict"),
      render: (r) => <Verdict value={r.verdict} />,
    },
  ];

  const chosen = selected ? VERDICTS.find((v) => v.v === selected) : null;

  return (
    <Panel>
      <PanelHead
        title="Page by page"
        note={
          selected
            ? `Showing only the pages judged ${selected}.`
            : "Every judged page, ranked. Select a column head to sort, or a verdict above to filter. The bar behind each page shows its crawls against the most-crawled page."
        }
        actions={
          selected ? (
            <Button size="sm" onClick={() => onSelect(null)}>
              Clear verdict filter
            </Button>
          ) : undefined
        }
      />
      {chosen ? (
        <div className="mf-wedge__body">
          <p className="mf-wedge__prose">
            <strong>{chosen.means}</strong> {chosen.action}
          </p>
        </div>
      ) : null}
      <Reading
        query={q}
        waiting={<Waiting rows={7} />}
        isEmpty={(d) => !d?.length}
        empty={
          <Empty
            title="No page judged in this period"
            body="A page shows here once an AI crawler has fetched it or an AI answer has sent a visitor to it. The crawler job builds the page list from your sitemap, so a page with no traffic at all still gets a row after that job runs."
          />
        }
      >
        {() =>
          rows.length === 0 ? (
            <Empty
              title={`No page is judged ${selected} in this period`}
              body="That is a result, not a gap in the data."
              action={<Button onClick={() => onSelect(null)}>Show all four verdicts</Button>}
            />
          ) : (
            <Sheet rows={rows} columns={columns} getKey={(r) => r.path} />
          )
        }
      </Reading>
    </Panel>
  );
}

/* ── what to do about it ────────────────────────────────────────────────── */

function ThePlan({ selected, site }: { selected: GapVerdict | null; site: string | undefined }) {
  return (
    <Panel>
      <PanelHead
        title="What to do about it"
        note="A ratio nobody can act on is a statistic. This is the decision attached to each verdict."
      />
      <div className="mf-wedge__body">
        <div className="mf-plan">
          <span className="mf-plan__head">Verdict</span>
          <span className="mf-plan__head">What it means</span>
          <span className="mf-plan__head">What to do</span>
          {VERDICTS.map(({ v, means, action }) => {
            const on = selected === v ? " mf-plan__row-on" : "";
            return (
              <Fragment key={v}>
                <span className={`mf-plan__cell${on}`}>
                  <Verdict value={v} />
                </span>
                <span className={`mf-plan__cell${on}`}>{means}</span>
                <span className={`mf-plan__cell${on}`}>{action}</span>
              </Fragment>
            );
          })}
        </div>
        <p className="mf-wedge__prose">
          The next question is usually whether a page is losing visitors while AI crawlers keep
          fetching it.
        </p>
        <Link className="mf-btn mf-btn--key mf-btn--md mf-linkkey" to={site ? `/app/${site}/content-decay` : "/app"}>
          <span className="mf-btn__label">Open content decay</span>
        </Link>
      </div>
    </Panel>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

export function CitationGapScreen() {
  const bench = useMachineEnv("all");
  const { site } = useParams();
  const [selected, setSelected] = useState<GapVerdict | null>(null);
  const summary = useGapSummary(bench);

  return (
    <div className="mf-wedge">
      <Bench title="Citation gap" bench={bench} />
      <GapRecord summary={summary} />
      <GapTotals summary={summary} />
      <VerdictBank summary={summary} selected={selected} onSelect={setSelected} />
      <GapSheet bench={bench} selected={selected} onSelect={setSelected} />
      <ThePlan selected={selected} site={site} />
    </div>
  );
}

export default CitationGapScreen;
