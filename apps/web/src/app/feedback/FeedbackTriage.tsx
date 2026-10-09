/**
 * FEEDBACK, as the instance administrator reads it: every report sent to this
 * instance, from every door it can come through.
 *
 * Reports arrive from four places: the dialog in the dashboard, an MCP client
 * acting for someone, the API, and the chat apps. The list says which one, and
 * WHO it was in the terms that place knows: a person's name or address, the
 * MCP client that wrote it, or the chat channel it was typed in. An answer to
 * "who sent this?" that only says "a user" is no answer when the report came
 * from an agent.
 *
 * Three decisions:
 *
 *   1. ONE PANEL, ON THE ADMIN SCREEN. Feedback is about the instance, not
 *      about one site, so it belongs with the people, storage and job queue,
 *      and only the instance administrator is shown it. The server refuses
 *      everyone else whatever this screen does; the screen just does not offer
 *      a list it would be refused.
 *   2. A REPORT IS READ AS IT WAS WRITTEN. The body is plain text with its line
 *      breaks kept, never markup: it is typed by strangers and by agents. The
 *      page address is a link only when it is an ordinary web address.
 *   3. A CHANGE IS SAVED WHEN IT IS MADE. Triage is a run of small decisions;
 *      a status picked from the list is stored at once and the result is said
 *      in one quiet line beside the control, with no save button to forget.
 */
import { useEffect, useId, useRef, useState } from "react";
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import type {
  ApiMeta,
  Feedback,
  FeedbackKind,
  FeedbackSeverity,
  FeedbackSource,
  FeedbackStatus,
  GitHubIssue as GitHubIssueResult,
} from "../../lib/api";
import {
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Sheet,
  type Column,
} from "../../components/react/ui";
import {
  IconAlert,
  IconCheck,
  IconClock,
  IconClose,
  IconFilter,
  IconInfo,
} from "../../components/react/icons";
import { ago, num } from "../../lib/format";
import { Field, Foot, Select, errText } from "../settings/SettingsLayout";
import { KIND_WORDS, SEVERITY_WORDS, SOURCE_WORDS, STATUS_WORDS } from "./words";
import "../../styles/app/feedback.css";

const LIMIT = 50;

/* ── reading one report ──────────────────────────────────────────────────── */

const STATUS_GLYPH: Record<FeedbackStatus, typeof IconInfo> = {
  new: IconInfo,
  triaged: IconFilter,
  planned: IconClock,
  done: IconCheck,
  wont_fix: IconClose,
};

/** A status as a drawn glyph and a word, so it reads with no colour at all. */
function StatusMark({ status }: { status: FeedbackStatus }) {
  const Glyph = STATUS_GLYPH[status];
  return (
    <span className="mf-fb-status" data-status={status}>
      <Glyph size={12} />
      {STATUS_WORDS[status]}
    </span>
  );
}

function SeverityMark({ severity }: { severity: FeedbackSeverity | null }) {
  if (!severity) return <span className="mf-fb-quiet">Severity not set</span>;
  return (
    <span className="mf-fb-severity" data-severity={severity}>
      {severity === "high" || severity === "critical" ? <IconAlert size={12} /> : null}
      {SEVERITY_WORDS[severity]} severity
    </span>
  );
}

/** Who sent it, in the terms of the door it came through. */
function reporterName(r: Feedback["reporter"]): string {
  if (r.name && r.email) return `${r.name} (${r.email})`;
  return r.name || r.email || (r.api_key_id ? "An API key" : r.chat ? "Someone in a chat app" : "Unknown");
}

/** The MCP client or chat channel a report came through, when it has one. */
function reporterVia(r: Feedback["reporter"]): string | null {
  if (r.client_name) return `MCP client ${r.client_name}`;
  if (r.chat) return `chat ${r.chat}`;
  return null;
}

const when = (iso: string): string => {
  const t = Date.parse(iso);
  return Number.isFinite(t)
    ? new Date(t).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })
    : iso;
};

/** Only an ordinary web address becomes a link; anything else stays text. */
const isWebAddress = (u: string): boolean => /^https?:\/\//i.test(u);

/* ── the GitHub issue a report became ────────────────────────────────────── */

/**
 * The issue, as a link, or the control that opens one in the repository
 * linked to the report's site. The server decides who may: the instance
 * administrator needs the admin role on that site too, because the issue goes
 * into that site's repository. A second press answers with the same issue.
 */
function GitHubIssue({ item, onChanged }: { item: Feedback; onChanged: (f: Feedback) => void }) {
  const qc = useQueryClient();
  const open = useMutation({
    mutationFn: () =>
      api.post<GitHubIssueResult>(`/api/github/feedback/${item.id}/issue`, {}).then((r) => r.data),
    onSuccess: (issue) => {
      onChanged({ ...item, github_issue: { url: issue.url, number: issue.number, repo: issue.repo } });
      void qc.invalidateQueries({ queryKey: ["admin", "feedback"] });
    },
  });

  if (item.github_issue) {
    const { url, number, repo } = item.github_issue;
    return isWebAddress(url) ? (
      <a className="mf-fb-link" href={url} target="_blank" rel="noreferrer noopener">
        <span className="mf-data">
          {repo ?? "issue"}#{number}
        </span>
      </a>
    ) : (
      <span className="mf-data">
        {repo ?? "issue"}#{number}
      </span>
    );
  }
  if (!item.site) {
    return <span className="mf-fb-quiet">Only a report about a site can become a GitHub issue.</span>;
  }

  const refusal = (e: unknown): string => {
    if (e instanceof ApiError && e.status === 403)
      return "Opening an issue needs the admin role on the site whose repository receives it.";
    if (e instanceof ApiError && e.status === 404 && /not configured/i.test(e.message))
      return "The GitHub App is not configured on this server.";
    if (e instanceof ApiError && e.status === 409) return e.message;
    return errText(e);
  };

  return (
    <span className="mf-fb-issue">
      <Button size="sm" disabled={open.isPending} onClick={() => open.mutate()}>
        {open.isPending ? "Opening…" : "Create GitHub issue"}
      </Button>
      <span className="mf-fb-quiet">
        In the repository linked to <span className="mf-data">{item.site.domain}</span>. The issue carries the
        text and plain facts, never the reporter&apos;s name or address.
      </span>
      {open.isError ? (
        <span className="mf-set-foot__say" data-tone="warn" role="alert">
          {refusal(open.error)}
        </span>
      ) : null}
    </span>
  );
}

/* ── one report, opened ──────────────────────────────────────────────────── */

function Detail({
  item,
  onClose,
  onChanged,
}: {
  item: Feedback;
  onClose: () => void;
  onChanged: (f: Feedback) => void;
}) {
  const qc = useQueryClient();
  const headId = useId();
  const statusId = useId();
  const severityId = useId();
  const root = useRef<HTMLElement | null>(null);

  // Opening a report moves the reader to it, so a keyboard user is not left
  // at the bottom of a fifty-row table wondering what changed.
  useEffect(() => {
    root.current?.focus();
  }, []);

  const change = useMutation({
    mutationFn: (patch: { status?: FeedbackStatus; severity?: FeedbackSeverity | null }) =>
      api.patch<Feedback>(`/api/admin/feedback/${item.id}`, patch).then((r) => r.data),
    onSuccess: (f) => {
      onChanged(f);
      void qc.invalidateQueries({ queryKey: ["admin", "feedback"] });
    },
  });

  const said = (() => {
    const v = change.variables;
    if (!v) return null;
    if (v.status) return `Status set to ${STATUS_WORDS[v.status]}.`;
    return v.severity ? `Severity set to ${SEVERITY_WORDS[v.severity]}.` : "Severity cleared.";
  })();

  const via = reporterVia(item.reporter);

  return (
    <section className="mf-fb-detail" aria-labelledby={headId} tabIndex={-1} ref={root}>
      <div className="mf-fb-detail__head">
        <div className="mf-fb-detail__head-text">
          <h3 className="mf-fb-detail__title" id={headId}>
            {item.title}
          </h3>
          <p className="mf-fb-detail__marks">
            <span>{KIND_WORDS[item.kind]}</span>
            <StatusMark status={item.status} />
            <SeverityMark severity={item.severity} />
          </p>
        </div>
        <Button size="sm" tone="quiet" icon={<IconClose size={12} />} onClick={onClose}>
          Close
        </Button>
      </div>

      <div className="mf-fb-detail__body">{item.body}</div>

      <dl className="mf-fb-facts">
        <div className="mf-fb-facts__row">
          <dt>Sent by</dt>
          <dd>{reporterName(item.reporter)}</dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Sent from</dt>
          <dd>
            {SOURCE_WORDS[item.source]}
            {via ? <span className="mf-fb-quiet">, {via}</span> : null}
            {item.reporter.api_key_id ? (
              <span className="mf-fb-quiet">
                , API key <span className="mf-data">{item.reporter.api_key_id}</span>
              </span>
            ) : null}
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Organisation</dt>
          <dd>{item.org ? item.org.name : <span className="mf-fb-quiet">none</span>}</dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Site</dt>
          <dd>
            {item.site ? (
              <span className="mf-data">{item.site.domain}</span>
            ) : (
              <span className="mf-fb-quiet">none</span>
            )}
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Page</dt>
          <dd className="mf-data mf-fb-wrap">
            {item.page_url ? (
              isWebAddress(item.page_url) ? (
                <a
                  className="mf-fb-link"
                  href={item.page_url}
                  target="_blank"
                  rel="noreferrer noopener"
                >
                  {item.page_url}
                </a>
              ) : (
                item.page_url
              )
            ) : (
              <span className="mf-fb-quiet">not sent</span>
            )}
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>App version</dt>
          <dd className="mf-data">
            {item.app_version ?? <span className="mf-fb-quiet">not sent</span>}
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Browser</dt>
          <dd className="mf-data mf-fb-wrap">
            {item.user_agent ?? <span className="mf-fb-quiet">not recorded</span>}
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Received</dt>
          <dd>
            {when(item.created_at)} <span className="mf-fb-quiet">({ago(item.created_at)})</span>
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>Last changed</dt>
          <dd>
            {when(item.updated_at)} <span className="mf-fb-quiet">({ago(item.updated_at)})</span>
          </dd>
        </div>
        <div className="mf-fb-facts__row">
          <dt>GitHub issue</dt>
          <dd>
            <GitHubIssue item={item} onChanged={onChanged} />
          </dd>
        </div>
      </dl>

      <div className="mf-fb-detail__acts">
        <Field label="Status" htmlFor={statusId}>
          <Select
            id={statusId}
            auto
            value={item.status}
            disabled={change.isPending}
            onChange={(e) => change.mutate({ status: e.target.value as FeedbackStatus })}
          >
            {(Object.keys(STATUS_WORDS) as FeedbackStatus[]).map((s) => (
              <option key={s} value={s}>
                {STATUS_WORDS[s]}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Severity" htmlFor={severityId}>
          <Select
            id={severityId}
            auto
            value={item.severity ?? ""}
            disabled={change.isPending}
            onChange={(e) =>
              change.mutate({ severity: (e.target.value || null) as FeedbackSeverity | null })
            }
          >
            <option value="">Not set</option>
            {(Object.keys(SEVERITY_WORDS) as FeedbackSeverity[]).map((s) => (
              <option key={s} value={s}>
                {SEVERITY_WORDS[s]}
              </option>
            ))}
          </Select>
        </Field>
        <span className="mf-fb-detail__say" aria-live="polite">
          {change.isPending ? (
            <span className="mf-set-foot__say">Saving…</span>
          ) : change.isError ? (
            <span className="mf-set-foot__say" data-tone="warn" role="alert">
              {change.error instanceof ApiError && change.error.status === 403
                ? "Only the instance administrator can change feedback."
                : errText(change.error)}
            </span>
          ) : change.isSuccess && said ? (
            <span className="mf-set-foot__say" data-tone="good">
              {said}
            </span>
          ) : null}
        </span>
      </div>
    </section>
  );
}

/* ── the panel ───────────────────────────────────────────────────────────── */

function FilterSelect<T extends string>({
  label,
  value,
  words,
  onChange,
}: {
  label: string;
  value: T | "";
  words: Record<T, string>;
  onChange: (v: T | "") => void;
}) {
  const id = useId();
  return (
    <Field label={label} htmlFor={id}>
      <Select id={id} value={value} onChange={(e) => onChange(e.target.value as T | "")}>
        <option value="">All</option>
        {(Object.keys(words) as T[]).map((k) => (
          <option key={k} value={k}>
            {words[k]}
          </option>
        ))}
      </Select>
    </Field>
  );
}

export function FeedbackTriage() {
  const [status, setStatus] = useState<FeedbackStatus | "">("");
  const [kind, setKind] = useState<FeedbackKind | "">("");
  const [source, setSource] = useState<FeedbackSource | "">("");
  const [page, setPage] = useState(1);
  const [chosen, setChosen] = useState<Feedback | null>(null);

  const list = useQuery({
    queryKey: ["admin", "feedback", status, kind, source, page],
    queryFn: async () => {
      const q = new URLSearchParams({ limit: String(LIMIT), page: String(page) });
      if (status) q.set("status", status);
      if (kind) q.set("kind", kind);
      if (source) q.set("source", source);
      const r = await api.get<Feedback[]>(`/api/admin/feedback?${q.toString()}`);
      return { rows: r.data, total: (r.meta as ApiMeta & { total?: number }).total ?? null };
    },
    placeholderData: keepPreviousData,
  });

  const filtered = status !== "" || kind !== "" || source !== "";
  const forbidden = list.error instanceof ApiError && list.error.status === 403;

  /** A new filter starts the list again from its first page. */
  const refilter = <T,>(set: (v: T) => void) => (v: T) => {
    set(v);
    setPage(1);
  };

  const columns: Column<Feedback>[] = [
    { key: "kind", head: "Kind", width: "12%", render: (f) => <span>{KIND_WORDS[f.kind]}</span> },
    { key: "title", head: "Title", render: (f) => <span title={f.title}>{f.title}</span> },
    {
      key: "who",
      head: "Sent by",
      render: (f) => {
        const via = reporterVia(f.reporter);
        return (
          <span>
            {reporterName(f.reporter)}
            {via ? <span className="mf-fb-quiet"> · {via}</span> : null}
          </span>
        );
      },
    },
    { key: "source", head: "From", width: "10%", render: (f) => <span>{SOURCE_WORDS[f.source]}</span> },
    {
      key: "site",
      head: "Site",
      width: "14%",
      render: (f) =>
        f.site ? <span className="mf-data">{f.site.domain}</span> : <span className="mf-fb-quiet">—</span>,
    },
    {
      key: "at",
      head: "Received",
      numeric: true,
      width: "12%",
      render: (f) => (
        <span className="mf-data" title={when(f.created_at)}>
          {ago(f.created_at)}
        </span>
      ),
    },
    { key: "status", head: "Status", width: "11%", render: (f) => <StatusMark status={f.status} /> },
  ];

  const body = (() => {
    if (forbidden)
      return (
        <Empty
          title="This list is for the instance administrator"
          body="Feedback sent to this instance is read by whoever runs it. Your account can send feedback, but not read everyone's."
        />
      );
    if (list.isPending) return <Sheet rows={[]} columns={columns} getKey={(f) => f.id} loading />;
    if (list.isError)
      return (
        <ErrorState
          title="Could not load the feedback"
          detail={errText(list.error)}
          onRetry={() => void list.refetch()}
        />
      );

    const { rows, total } = list.data;
    if (!rows.length) {
      return filtered ? (
        <Empty
          title="No feedback matches these filters"
          body="Nothing sent to this instance has this status, kind and source. Clear the filters to see every report."
          action={
            <Button
              onClick={() => {
                setStatus("");
                setKind("");
                setSource("");
                setPage(1);
              }}
            >
              Clear the filters
            </Button>
          }
        />
      ) : (
        <Empty
          title="No feedback yet"
          body="Reports sent from the dashboard, MCP clients, the API and chat apps arrive here."
        />
      );
    }

    const from = (page - 1) * LIMIT + 1;
    const to = from + rows.length - 1;
    const atEnd = total != null ? to >= total : rows.length < LIMIT;
    return (
      <>
        <Sheet rows={rows} columns={columns} getKey={(f) => f.id} onRowClick={(f) => setChosen(f)} />
        <Foot tight>
          <span className="mf-set-foot__say" aria-live="polite">
            {num(from)}–{num(to)}
            {total != null ? ` of ${num(total)}` : ""}
            {list.isPlaceholderData ? " · loading…" : ""}
          </span>
          <span className="mf-set-foot__spacer" />
          <Button size="sm" disabled={page === 1 || list.isFetching} onClick={() => setPage((p) => p - 1)}>
            Previous
          </Button>
          <Button size="sm" disabled={atEnd || list.isFetching} onClick={() => setPage((p) => p + 1)}>
            Next
          </Button>
        </Foot>
      </>
    );
  })();

  return (
    <Panel>
      <PanelHead
        title="Feedback"
        note="Reports sent to this instance from the dashboard, MCP clients, the API and chat apps, newest first. Open one to read it and set its status."
      />
      {forbidden ? null : (
        <div className="mf-fb-filters" role="group" aria-label="Filter the feedback">
          <FilterSelect label="Status" value={status} words={STATUS_WORDS} onChange={refilter(setStatus)} />
          <FilterSelect label="Kind" value={kind} words={KIND_WORDS} onChange={refilter(setKind)} />
          <FilterSelect label="From" value={source} words={SOURCE_WORDS} onChange={refilter(setSource)} />
        </div>
      )}
      {body}
      {chosen && !forbidden ? (
        <Detail key={chosen.id} item={chosen} onClose={() => setChosen(null)} onChanged={setChosen} />
      ) : null}
    </Panel>
  );
}

export default FeedbackTriage;
