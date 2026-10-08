/**
 * The Citation Gap, at /app/:site/citation-gap
 *
 * The reason this product exists, in one image: the machine pen writing up from
 * the centre rule and the answer-engine pen writing down from it. The asymmetry
 * about that rule IS the report.
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
    means: "Fetched hard by the machines, almost never cited back.",
    action:
      "Decide whether you want to be in that corpus. Either tighten robots.txt for the operators taking the most, or make the page quotable enough to earn the citation: a clear answer near the top, named and dated.",
  },
  {
    v: "compounding",
    means: "Crawled, and the crawl comes back as readers.",
    action: "This is the shape that works. Write more pages like these, and keep them current so the re-crawl stays worth it.",
  },
  {
    v: "invisible",
    means: "Cited more than it is crawled: the engines know it from elsewhere.",
    action:
      "The answer engines are quoting a copy, not your page. Put it in the sitemap and in llms.txt so the current version is the one they read.",
  },
  {
    v: "quiet",
    means: "Too little of either pen to call.",
    action: "Nothing to do yet. It will move into one of the other three once it is crawled or found.",
  },
];

/* ── the record ─────────────────────────────────────────────────────────── */

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
    /* No peak note here. The drum writes its own when the machine pen runs off
       the top of the paper, and it is the only one that knows the true value it
       ran off at. These are the notes the drum cannot read for itself. */
    const first = returned.findIndex((v) => v > 0);
    if (first >= 0) out.push({ at: first, label: "First arrival back", tone: "good" });
    let best = -1;
    for (let i = 0; i < returned.length; i++) if (returned[i] > 0 && (best < 0 || returned[i] > returned[best])) best = i;
    if (best >= 0 && best !== first) out.push({ at: best, label: `Best ${compact(returned[best])} returned`, tone: "good" });
    return out;
  }, [record]);

  /* The illustrative record shown when nothing has been measured yet. It is
     labelled twice (once by the notice, once inside the drum's own readout)
     because a demonstration figure that could be mistaken for measured traffic
     is the one thing this product may never ship. */
  const demo = useMemo(() => demoRecord(45, 20260821, Date.UTC(2026, 7, 21)), []);

  return (
    <Panel ruled feed>
      <PanelHead
        title="The Citation Gap"
        note={
          /* The note has to follow the gain, because on one gain the distance
             between the pens IS the quantity and on separate gains it is not.
             A fixed sentence made the panel contradict the drum's own note
             directly below it. */
          gain === "shared"
            ? "The machine pen writes up from the centre rule: every fetch an AI agent made. The answer pen writes down: every reader who arrived from an answer engine. Both are on one scale, so the distance between them is what was taken and never came back."
            : "The machine pen writes up from the centre rule: every fetch an AI agent made. The answer pen writes down: every reader who arrived from an answer engine. Each is on its own scale, so compare the shapes here and read the size of the gap from the figures below."
        }
        actions={
          <div className="mf-pivot" role="group" aria-label="Gain">
            <button
              type="button"
              className="mf-pivot__key"
              aria-pressed={gain === "shared"}
              onClick={() => setGain("shared")}
              title="Both pens on one scale, with the debt between them hatched. Shows the true difference in magnitude."
            >
              One gain
            </button>
            <button
              type="button"
              className="mf-pivot__key"
              aria-pressed={gain === "per-channel"}
              onClick={() => setGain("per-channel")}
              title="Each pen scaled to its own maximum, with its full-scale value printed. Shows the shape of both."
            >
              Own gain
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
                title="Neither pen has written in this interval"
                body="Machine fetches come from the server-side ingest path; answer-engine arrivals come from referrals by ChatGPT, Perplexity, Claude, Gemini and their peers. With both reporting, this is the report you get."
              />
              <DemoNotice>
                Below is a demonstration record, not this site. Every figure in it is illustrative.
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
                    label: "Taken by machines, this sample",
                    color: "var(--mf-ai)",
                    pen: "stepped",
                    values: demo.machines,
                    format: (n) => num(n),
                  },
                  {
                    id: "demo-returned",
                    label: "Returned as readers, this sample",
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
            /* The gain is explained once, by the drum itself, directly under
               the pens it governs. A second reading of the same sentence used
               to sit here and ran into the first one on a narrow screen. */
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
                  label: "Taken by machines, this sample",
                  color: "var(--mf-ai)",
                  pen: "stepped",
                  values: record.taken,
                  format: (n) => num(n),
                },
                {
                  id: "returned",
                  label: "Returned as readers, this sample",
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
  /** The trend is read off the record itself: the ratio in the second half of
      the interval against the ratio in the first. A falling ratio is the good
      outcome, so the channel is marked inverse. */
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
        empty={<Empty title="Nothing measured yet" body="The totals fill in with the first fetch and the first answer-engine referral." />}
      >
        {(d) => (
          <>
            {/* The drum above prints one sample per channel. These are the
                totals for the whole interval, and they say so, because two
                different measurements under one name is a bug. */}
            <p className="mf-wedge__caption">
              Totals for the whole interval. The readout on the drum above reports a single sample:
              the one under the cursor, or the last one written.
            </p>
            <Readout
              channels={[
                { key: "taken", label: "Taken by machines", value: num(d.agent_fetches), note: "agent fetches" },
                { key: "back", label: "Returned as readers", value: num(d.answer_arrivals), note: "answer-engine arrivals" },
                {
                  key: "ratio",
                  label: "Fetches per arrival",
                  value: gapRatio(d.agent_fetches, d.answer_arrivals),
                  delta: trend,
                  inverse: true,
                  note: "against the first half of the interval",
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

/* ── the verdict bank ───────────────────────────────────────────────────── */

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
    <Panel>
      <PanelHead
        title="The judgement"
        note="Every page in the register falls into one of four states. Select one to read only those pages in the sheet below."
        actions={
          selected ? (
            <Button size="sm" tone="quiet" onClick={() => onSelect(null)}>
              Read all four again
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
            title="No page has enough signal to judge"
            body="A page is judged once either pen has written on it: a machine fetched it, or a reader arrived at it from an answer engine."
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

/* ── the per-page sheet ─────────────────────────────────────────────────── */

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
      /* The ink behind the cell is the page's share of the heaviest crawl. */
      share: (r) => (maxTaken ? r.agent_fetches / maxTaken : undefined),
    },
    {
      key: "taken",
      head: sort.head("taken", "Taken"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.agent_fetches)}</span>,
    },
    {
      key: "back",
      head: sort.head("back", "Returned"),
      numeric: true,
      render: (r) => (
        <span className={r.answer_arrivals ? "mf-data" : "mf-data mf-dim"}>{num(r.answer_arrivals)}</span>
      ),
    },
    {
      key: "humans",
      head: sort.head("humans", "Read by people"),
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.human_pageviews)}</span>,
    },
    {
      key: "ratio",
      head: sort.head("ratio", "Fetches per arrival"),
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
            ? `Reading only the pages judged ${selected}.`
            : "Every judged page, ranked. Any column sorts, and selecting a verdict above reads only those pages. The ink behind each page is its share of the heaviest crawl."
        }
        actions={
          selected ? (
            <Button size="sm" onClick={() => onSelect(null)}>
              Clear the verdict filter
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
            title="No page has been judged in this interval"
            body="A page appears here once a machine has fetched it or a reader has arrived at it from an answer engine. The page register is built by the crawler job from your sitemap, so a page with no traffic at all still gets a row once that job has run."
          />
        }
      >
        {() =>
          rows.length === 0 ? (
            <Empty
              title={`No page is judged ${selected} in this interval`}
              body="That is a result, not a gap in the data."
              action={<Button onClick={() => onSelect(null)}>Read all four verdicts</Button>}
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
          The next question is usually whether a page is losing its readers while the machines keep
          reading it.
        </p>
        <Link className="mf-btn mf-btn--key mf-btn--md" to={site ? `/app/${site}/decay` : "/app"}>
          <span className="mf-btn__label">Open the decay report</span>
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
      <Bench title="Citation Gap" bench={bench} />
      <GapRecord summary={summary} />
      <GapTotals summary={summary} />
      <VerdictBank summary={summary} selected={selected} onSelect={setSelected} />
      <GapSheet bench={bench} selected={selected} onSelect={setSelected} />
      <ThePlan selected={selected} site={site} />
    </div>
  );
}

export default CitationGapScreen;
