/**
 * One investigation, at /app/:site/insights/:id
 *
 * The page reads top to bottom in the order a person checks a claim: what
 * moved and by how much, the separate findings that make up that movement, a
 * likely explanation, the exact queries behind all of it, and what has happened
 * to the investigation since it was opened.
 *
 * Four decisions shape it:
 *
 *   1. THE FIGURES COME BEFORE THE STORY. The findings are statistics the server
 *      measured, and they stand on their own. The explanation is a model's
 *      reading of them, so it is placed after them, marked as written by AI,
 *      and is optional: an instance with no model configured still opens and
 *      updates investigations, and this page says so in one sentence instead
 *      of showing an empty box.
 *   2. THE EVIDENCE IS THE SAME RECORD AS ASK AI'S. Every query is listed with
 *      the stats request a person could send and the SQL the server ran, in the
 *      shared QueryTrail, so an investigation and an answer are checked the
 *      same way.
 *   3. STATUS IS A PERSON'S CALL. Open, monitoring and resolved are set here by
 *      a member of the site. The server refuses a viewer, and the refusal is
 *      shown in words beside the control, not hidden by greying it out on a
 *      guess about the reader's role.
 *   4. THE TIMELINE RUNS OLDEST FIRST, so it reads as the history it is: opened,
 *      checked, explained, changed, posted.
 */
import type { ReactNode } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import type { Investigation, InvestigationDetail, InvestigationStatus } from "../../lib/api";
import { ago } from "../../lib/format";
import { Empty, ErrorState, Panel, PanelHead, Readout, Sheet } from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import { QueryTrail } from "../../components/react/QueryTrail";
import { PlainText } from "../../components/react/PlainText";
import { IconChevron } from "../../components/react/icons";
import { useSite } from "../providers";
import {
  Change,
  EventMark,
  STATUS_WORDS,
  SeverityMark,
  StatusMark,
  figure,
  fullTime,
  insightError,
  isAiMetric,
  scopeLine,
} from "./marks";
import "../../styles/app/insights.css";

type Finding = Investigation["findings"][number];

const STATUSES: InvestigationStatus[] = ["open", "monitoring", "resolved"];

const insightKey = (id: string | undefined) => ["insights", "detail", id ?? null] as const;

/* ── small parts ─────────────────────────────────────────────────────────── */

function BackLink({ to }: { to: string }) {
  return (
    <Link className="mf-btn mf-btn--quiet mf-btn--sm mf-ins-back" to={to}>
      <span className="mf-btn__icon">
        <IconChevron size={13} className="mf-ins-back__caret" />
      </span>
      <span className="mf-btn__label">All investigations</span>
    </Link>
  );
}

/** A relative time with the full date in its title, or a plain word when there is none. */
function When({ iso, none }: { iso: string | null; none: string }) {
  if (!iso) return <span className="mf-ins-facts__none">{none}</span>;
  return (
    <time className="mf-data" dateTime={iso} title={fullTime(iso)}>
      {ago(iso)}
    </time>
  );
}

/** The grey shapes of this page while it loads: a heading, then three panels. */
function DetailWaiting() {
  return (
    <div className="mf-ins-wait" aria-busy="true" aria-live="polite">
      <span className="mf-sr">Loading the investigation…</span>
      <span className="mf-wait" style={{ width: "46%", height: "22px" }} aria-hidden="true" />
      <span className="mf-wait" style={{ width: "30%" }} aria-hidden="true" />
      {[0, 1, 2].map((i) => (
        <Panel key={i}>
          <div className="mf-ins-wait__panel">
            <span className="mf-wait" style={{ width: `${28 + i * 9}%`, height: "15px" }} aria-hidden="true" />
            <span className="mf-wait" style={{ width: `${70 - i * 12}%` }} aria-hidden="true" />
            <span className="mf-wait" style={{ width: `${54 + i * 7}%` }} aria-hidden="true" />
          </div>
        </Panel>
      ))}
    </div>
  );
}

/** The explanation, or the one sentence that says why there is none. */
function Explanation({ inv }: { inv: Investigation }) {
  let body: ReactNode;
  if (inv.explanation_status === "not_configured") {
    body = (
      <p className="mf-ins-prose">
        The explanation needs AI configured on this instance. The operator sets{" "}
        <code className="mf-data">MICAFORGE_LLM_BASE_URL</code> and{" "}
        <code className="mf-data">MICAFORGE_LLM_MODEL</code> (see{" "}
        <a className="mf-ins-link" href="/docs/dashboard/ask-ai">
          Ask AI in the docs
        </a>
        ). The findings above are complete without it.
      </p>
    );
  } else if (inv.explanation_status === "failed") {
    body = (
      <p className="mf-ins-prose">
        The model did not answer when this investigation was last checked, so there is no explanation
        yet. The findings above are still current, and the next check asks the model again.
      </p>
    );
  } else if (inv.explanation && inv.explanation.trim()) {
    body = <PlainText text={inv.explanation} />;
  } else {
    body = <p className="mf-ins-prose">The model answered, but it returned no text.</p>;
  }

  const ready = inv.explanation_status === "ready" && !!inv.explanation?.trim();
  return (
    <Panel>
      <PanelHead
        title={
          <span className="mf-ins-titled">
            Likely causes
            {ready ? (
              <span className="mf-ins-ai" title="Written by an AI model">
                AI
              </span>
            ) : null}
          </span>
        }
        note={
          ready
            ? "Written by an AI model from the findings and the queries on this page. Check it against the evidence before you act on it."
            : undefined
        }
      />
      <div className="mf-ins-body">{body}</div>
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function InsightDetail() {
  const { site, id } = useParams();
  const { siteId } = useSite();
  const qc = useQueryClient();
  const listPath = `/app/${site ?? siteId ?? ""}/insights`;

  const detail = useQuery({
    queryKey: insightKey(id),
    queryFn: () =>
      api.get<InvestigationDetail>(`/api/investigations/${encodeURIComponent(id ?? "")}`).then((r) => r.data),
    enabled: !!id,
  });

  const setStatus = useMutation({
    mutationFn: (status: InvestigationStatus) =>
      api
        .patch<Investigation>(`/api/investigations/${encodeURIComponent(id ?? "")}`, { status })
        .then((r) => r.data),
    onSuccess: (updated) => {
      // The answer carries no timeline, so the events already on screen are
      // kept, and the detail is read again for the event this change wrote.
      qc.setQueryData<InvestigationDetail>(insightKey(id), (old) =>
        old ? { ...old, ...updated, events: old.events } : old,
      );
      void qc.invalidateQueries({ queryKey: insightKey(id) });
      void qc.invalidateQueries({ queryKey: ["insights", "list"] });
    },
  });

  if (detail.isPending) {
    return (
      <div className="mf-ins">
        <BackLink to={listPath} />
        <h1 className="mf-sr">Investigation</h1>
        <DetailWaiting />
      </div>
    );
  }

  if (detail.isError) {
    const missing = detail.error instanceof ApiError && detail.error.status === 404;
    return (
      <div className="mf-ins">
        <BackLink to={listPath} />
        <h1 className="mf-sr">Investigation</h1>
        <Panel>
          {missing ? (
            <Empty
              title="No investigation at this address"
              body="It may belong to another site, or the link may be from before it was removed. The list has every investigation on this site."
            />
          ) : (
            <ErrorState
              title="Could not load this investigation"
              detail={insightError(detail.error, "reading this investigation")}
              onRetry={() => void detail.refetch()}
            />
          )}
        </Panel>
      </div>
    );
  }

  const inv = detail.data;
  const events = [...(inv.events ?? [])].sort(
    (a, b) => Date.parse(a.created_at) - Date.parse(b.created_at),
  );

  const change: Channel[] = [
    {
      key: "now",
      label: "Now",
      value: figure(inv.metric, inv.current),
      delta: inv.change_pct != null && Number.isFinite(inv.change_pct) ? inv.change_pct / 100 : null,
      inverse: inv.metric === "bounce_rate",
      note: inv.change_pct == null ? "no baseline to compare with" : undefined,
    },
    {
      key: "normal",
      label: "Normal level",
      value: figure(inv.metric, inv.baseline),
      note: inv.window,
    },
  ];

  const findingColumns: Column<Finding>[] = [
    { key: "label", head: "Finding", width: "46%", render: (f) => <span>{f.label}</span> },
    {
      key: "current",
      head: "Now",
      numeric: true,
      render: (f) => <span className="mf-data">{figure(inv.metric, f.current)}</span>,
    },
    {
      key: "baseline",
      head: "Normal level",
      numeric: true,
      render: (f) => <span className="mf-data">{figure(inv.metric, f.baseline)}</span>,
    },
    {
      key: "change",
      head: "Change",
      numeric: true,
      render: (f) => <Change metric={inv.metric} changePct={f.change_pct} />,
    },
  ];

  return (
    <div className="mf-ins">
      <BackLink to={listPath} />

      <header className="mf-ins-bar mf-ins-bar--detail">
        <h1 className="mf-ins-bar__title">{inv.title}</h1>
        <div className="mf-ins-bar__marks">
          <SeverityMark value={inv.severity} />
          <StatusMark value={inv.status} />
          <span className="mf-ins-bar__scope">
            {scopeLine(inv)}
            {isAiMetric(inv.metric) ? (
              <span className="mf-ins-ai" title="This metric counts AI traffic">
                AI
              </span>
            ) : null}
          </span>
        </div>
        <div className="mf-ins-bar__controls">
          <span className="mf-ins-bar__label" id={`ins-status-${inv.id}`}>
            Status
          </span>
          <div className="mfs-bank" role="group" aria-labelledby={`ins-status-${inv.id}`}>
            {STATUSES.map((s) => (
              <button
                key={s}
                type="button"
                className="mfs-bank__key"
                aria-pressed={inv.status === s}
                disabled={setStatus.isPending}
                onClick={() => {
                  if (s !== inv.status) setStatus.mutate(s);
                }}
              >
                {STATUS_WORDS[s]}
              </button>
            ))}
          </div>
        </div>
        {setStatus.isError || setStatus.isPending ? (
          <p
            className="mf-ins-say"
            data-tone={setStatus.isError ? "warn" : undefined}
            role={setStatus.isError ? "alert" : "status"}
          >
            {setStatus.isError
              ? insightError(setStatus.error, "changing the status")
              : "Saving the status…"}
          </p>
        ) : null}
        <dl className="mf-ins-facts">
          <div>
            <dt>Opened</dt>
            <dd>
              <When iso={inv.created_at} none="unknown" />
            </dd>
          </div>
          <div>
            <dt>Updated</dt>
            <dd>
              <When iso={inv.updated_at} none="unknown" />
            </dd>
          </div>
          <div>
            <dt>Next check</dt>
            <dd>
              <When iso={inv.next_check_at} none="none scheduled" />
            </dd>
          </div>
          <div>
            <dt>Resolved</dt>
            <dd>
              <When iso={inv.resolved_at} none="not resolved" />
            </dd>
          </div>
        </dl>
      </header>

      <Panel>
        <PanelHead
          title="What changed"
          note={`${figure(inv.metric, inv.current)} against a normal level of ${figure(inv.metric, inv.baseline)}, measured as ${inv.window}.`}
        />
        <div className="mf-ins-body mf-ins-body--flush">
          <Readout channels={change} />
        </div>
      </Panel>

      <Panel>
        <PanelHead
          title="Findings"
          note="The separate measurements behind the change, each against its own normal level."
        />
        <Sheet
          rows={inv.findings ?? []}
          columns={findingColumns}
          getKey={(f, i) => `${f.label}-${i}`}
          empty={
            <Empty
              title="No separate findings"
              body="The change above is the only measurement this investigation recorded."
            />
          }
        />
      </Panel>

      <Explanation inv={inv} />

      <Panel>
        <PanelHead
          title="Evidence"
          note="The statistical queries first, then any the AI model ran. Each one lists the stats request you can send yourself and the SQL the server ran."
        />
        <div className="mf-ins-body">
          <QueryTrail
            traces={inv.evidence ?? []}
            summary="The queries behind this investigation"
            empty="This investigation ran no queries yet."
          />
        </div>
      </Panel>

      <Panel>
        <PanelHead title="Timeline" note="Everything that happened to this investigation, oldest first." />
        <div className="mf-ins-body">
          {events.length ? (
            <ol className="mf-ins-timeline">
              {events.map((ev) => (
                <li key={ev.id} className="mf-ins-event">
                  <EventMark kind={ev.kind} />
                  <p className="mf-ins-event__summary">{ev.summary}</p>
                  <time className="mf-ins-event__at mf-data" dateTime={ev.created_at} title={fullTime(ev.created_at)}>
                    {ago(ev.created_at)}
                  </time>
                </li>
              ))}
            </ol>
          ) : (
            <p className="mf-ins-prose mf-ins-prose--quiet">
              Nothing is recorded yet. The first check writes the first entry.
            </p>
          )}
        </div>
      </Panel>
    </div>
  );
}

export default InsightDetail;
