/**
 * One error, in full.
 *
 * The list answers "what is broken"; this answers "who did it reach, where, and
 * is it still happening". So the trend is drawn as a full-size chart rather
 * than as a thumbnail, the stack is printed as numbered frames instead of a
 * wall of text, and the affected pages, browsers and systems sit in three
 * columns on one shared grid so they can be read across.
 *
 * The resolve and ignore keys write the group's state back. CONTRACT.md §3.3
 * does not yet list a mutation for it: this is the one deviation in this area
 * and needs writing back to the contract. The screen is honest when the server
 * has not caught up: the state is not changed locally unless the write lands,
 * and the failure says so.
 */
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "react-router";
import { Drum, type DrumAnnotation } from "../../components/react/Drum";
import { api, ApiError, type Audience, type Bucket, type Filter } from "../../lib/api";
import { ago, num, shortPath } from "../../lib/format";
import {
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  type Channel,
  type Column,
} from "../../components/react/ui";
import {
  IconAlert,
  IconCheck,
  IconChevron,
  IconClose,
  IconCopy,
  IconReplay,
} from "../../components/react/icons";
import type { ErrorFacet, ErrorGroup, ErrorStatus } from "./ErrorsScreen";
import { useAppState } from "../state";
import { useSite } from "../providers";

/* ── what the endpoint returns ───────────────────────────────────────────── */

export interface ErrorSession {
  session_id: string;
  t: number | string | null;
  pathname?: string;
  browser?: string;
  os?: string;
  country?: string;
}

export interface ErrorDetailData extends ErrorGroup {
  stack?: string | null;
  filename?: string;
  lineno?: number;
  colno?: number;
  by_page?: ErrorFacet[];
  by_browser?: ErrorFacet[];
  by_os?: ErrorFacet[];
  series?: Bucket[];
  sessions?: ErrorSession[];
}

function asDetail(
  data: ErrorDetailData | { rows?: ErrorDetailData[] } | null | undefined,
): ErrorDetailData | null {
  if (!data) return null;
  if ("rows" in data && Array.isArray(data.rows)) return data.rows[0] ?? null;
  return "fingerprint" in data ? (data as ErrorDetailData) : null;
}

function reason(err: unknown): string | undefined {
  if (err instanceof ApiError) return err.message;
  return err instanceof Error ? err.message : undefined;
}

const STATE_WORD: Record<ErrorStatus, string> = {
  open: "Open",
  resolved: "Resolved",
  ignored: "Ignored",
};

/**
 * A stack's first line is the class and the message, which are already the
 * heading of this screen, so it is dropped rather than printed twice.
 */
function frames(stack: string | null | undefined, name: string): string[] {
  const lines = (stack ?? "")
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);
  if (lines.length > 1 && name && lines[0].startsWith(name)) return lines.slice(1);
  return lines;
}

/* ── a small ranked column ───────────────────────────────────────────────── */

function RankedColumn({
  head,
  rows,
  label,
  format,
}: {
  head: string;
  rows?: ErrorFacet[];
  label: string;
  format?: (v: string) => string;
}) {
  const list = rows ?? [];
  const total = list.reduce((s, r) => s + (r.count || 0), 0);
  const columns: Column<ErrorFacet>[] = [
    {
      key: "value",
      head: label,
      render: (row) => (
        <span title={row.value}>{format ? format(row.value) : row.value || "Not reported"}</span>
      ),
      share: (row) => (total > 0 ? row.count / total : undefined),
    },
    {
      key: "count",
      head: "Occurrences",
      numeric: true,
      render: (row) => <span className="mf-data">{num(row.count)}</span>,
    },
  ];
  return (
    <div className="mf-hsplit__col">
      <h3 className="mf-hsplit__head">{head}</h3>
      {list.length ? (
        <Sheet rows={list} columns={columns} getKey={(row, i) => `${row.value}-${i}`} />
      ) : (
        <p className="mf-hs-none">Not reported for this error.</p>
      )}
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function ErrorDetail(props: { fingerprint?: string }) {
  const params = useParams();
  const fingerprint = props.fingerprint ?? params.fingerprint ?? params.id ?? "";
  const site = params.site ?? "";

  const { siteId, isLoading: siteLoading } = useSite();
  const { range, audience, filters } = useAppState();
  const qc = useQueryClient();
  const [copied, setCopied] = useState(false);

  const activeFilters: Filter[] = filters ?? [];
  const filtersJson = JSON.stringify(activeFilters);
  const ready = siteId != null && !siteLoading && Boolean(fingerprint);

  const query = useQuery({
    queryKey: ["stats", "errors/detail", siteId, range, audience, filtersJson, fingerprint],
    queryFn: () =>
      api
        .get<ErrorDetailData | { rows?: ErrorDetailData[] }>("/api/stats/errors", {
          site: siteId as number,
          range,
          audience: audience as Audience,
          filters: activeFilters,
          fingerprint,
        })
        .then((r) => asDetail(r.data)),
    enabled: ready,
    staleTime: 30_000,
  });

  const setStatus = useMutation({
    mutationFn: (next: ErrorStatus) =>
      api.post<{ status: ErrorStatus }>(
        `/api/stats/errors/${encodeURIComponent(fingerprint)}/status`,
        { status: next },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["stats", "errors"] });
      void qc.invalidateQueries({ queryKey: ["stats", "errors/detail"] });
    },
  });

  const detail = query.data ?? null;
  const status: ErrorStatus = detail?.status ?? "open";
  const series = detail?.series ?? [];
  const stackFrames = frames(detail?.stack, detail?.name ?? "");

  /* Two facts read straight off the series, written in the chart's margin:
     the worst interval, and the last one that carried anything at all.
     Neither is invented. */
  const annotations: DrumAnnotation[] = useMemo(() => {
    if (series.length < 2) return [];
    let peak = 0;
    let peakAt = 0;
    let lastAt = -1;
    series.forEach((b, i) => {
      if (b.v > peak) {
        peak = b.v;
        peakAt = i;
      }
      if (b.v > 0) lastAt = i;
    });
    const marks: DrumAnnotation[] = [];
    if (peak > 0) marks.push({ at: peakAt, label: `Peak ${num(peak)}`, tone: "warn" });
    if (lastAt >= 0 && lastAt !== peakAt) {
      marks.push({ at: lastAt, label: "Last occurrence", tone: "neutral" });
    }
    return marks;
  }, [series]);

  /* An hourly chart labelled by date repeats the same word eight times, so
     the axis is written in whichever unit the interval actually moves in. */
  const span = series.length > 1 ? series[series.length - 1].t - series[0].t : 0;
  const hourly = span > 0 && span <= 2 * 86_400_000;
  const formatTime = (t: number) =>
    new Date(t).toLocaleString(
      undefined,
      hourly ? { hour: "2-digit", minute: "2-digit" } : { month: "short", day: "numeric" },
    );

  const channels: Channel[] = [
    { key: "count", label: "Occurrences", value: num(detail?.count) },
    { key: "visitors", label: "Visitors affected", value: num(detail?.visitors) },
    {
      key: "sessions",
      label: "Sessions",
      value: num(detail?.session_count ?? detail?.sessions?.length ?? null),
    },
    {
      key: "pages",
      label: "Pages",
      value: num(detail?.by_page?.length ?? null),
      note: "distinct paths it fired on",
    },
  ];

  const sessionColumns: Column<ErrorSession>[] = [
    {
      key: "session",
      head: "Session",
      render: (row) => (
        <Link className="mf-data mf-hs-link" to={`/app/${site}/sessions/${row.session_id}`}>
          {row.session_id ? row.session_id.slice(0, 12) : "—"}
        </Link>
      ),
    },
    {
      key: "when",
      head: "When",
      numeric: true,
      render: (row) => <span className="mf-data">{ago(row.t)}</span>,
    },
    {
      key: "path",
      head: "Page",
      render: (row) => (
        <span className="mf-data" title={row.pathname}>
          {shortPath(row.pathname ?? "", 34)}
        </span>
      ),
    },
    { key: "browser", head: "Browser", render: (row) => row.browser || "—" },
    { key: "os", head: "System", render: (row) => row.os || "—" },
    { key: "country", head: "Country", render: (row) => row.country || "—" },
  ];

  const copyStack = async () => {
    if (!detail?.stack) return;
    try {
      await navigator.clipboard.writeText(detail.stack);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      /* clipboard blocked: the stack is selectable, which is the fallback */
    }
  };

  return (
    <div className="mf-hs">
      <Link className="mf-hs-back" to={`/app/${site}/errors`}>
        <IconChevron size={14} />
        All errors
      </Link>

      {query.isError ? (
        <Panel>
          <ErrorState
            title="Could not load this error"
            detail={reason(query.error)}
            onRetry={() => void query.refetch()}
          />
        </Panel>
      ) : query.isPending || siteLoading ? (
        <Panel>
          <div className="mf-hs-wait" aria-busy="true" aria-live="polite">
            <span className="mf-sr">Loading…</span>
            {Array.from({ length: 4 }).map((_, i) => (
              <span key={i} className="mf-wait" />
            ))}
          </div>
        </Panel>
      ) : !detail ? (
        <Panel>
          <Empty
            title="No error with that fingerprint"
            body="It may have aged out of this site's retention window, or the link is older than the data behind it."
            action={<Link className="mf-btn mf-btn--key mf-btn--md" to={`/app/${site}/errors`}>Back to errors</Link>}
          />
        </Panel>
      ) : (
        <>
          {/* ── identity, state, and the four figures ───────────────────── */}
          <Panel>
            <PanelHead
              title={detail.name || "Error"}
              note={detail.message || "No message was recorded with this error."}
            />

            {/* The state and the two keys that change it, on their own strip:
                a masthead is sized to its text and will not shrink, so a row of
                controls parked inside it runs off a narrow screen. */}
            <div className="mf-hs-strip">
              <span className="mf-estate" data-state={status}>
                {status === "resolved" ? (
                  <IconCheck size={14} />
                ) : status === "ignored" ? (
                  <IconClose size={14} />
                ) : (
                  <IconAlert size={14} />
                )}
                {STATE_WORD[status]}
              </span>
              <Button
                tone={status === "resolved" ? "key" : "primary"}
                icon={status === "resolved" ? <IconReplay size={14} /> : <IconCheck size={14} />}
                disabled={setStatus.isPending}
                onClick={() => setStatus.mutate(status === "resolved" ? "open" : "resolved")}
              >
                {status === "resolved" ? "Reopen" : "Mark resolved"}
              </Button>
              <Button
                icon={status === "ignored" ? <IconReplay size={14} /> : <IconClose size={14} />}
                disabled={setStatus.isPending}
                onClick={() => setStatus.mutate(status === "ignored" ? "open" : "ignored")}
              >
                {status === "ignored" ? "Stop ignoring" : "Ignore"}
              </Button>
            </div>

            {setStatus.isError ? (
              <p className="mf-hs-alert" role="alert">
                <IconAlert size={16} />
                <span>
                  {reason(setStatus.error) ?? "The server refused that change."} The error is still{" "}
                  {STATE_WORD[status].toLowerCase()}. Try again.
                </span>
              </p>
            ) : null}

            <Readout channels={channels} dense />

            <p className="mf-hs-when">
              First seen {ago(detail.first_seen)}. Last seen {ago(detail.last_seen)}.
              {detail.release ? ` Release ${detail.release}.` : ""}
              {detail.filename
                ? ` ${shortPath(detail.filename, 44)}${
                    detail.lineno != null ? `:${detail.lineno}` : ""
                  }${detail.colno != null ? `:${detail.colno}` : ""}.`
                : ""}
            </p>
          </Panel>

          {/* ── when it fired ───────────────────────────────────────────── */}
          <Panel>
            <PanelHead
              title="When it fired"
              note="Occurrences across the selected period."
            />
            <Drum
              times={series.map((b) => b.t)}
              channels={[
                {
                  id: "occurrences",
                  label: "Occurrences",
                  color: "var(--mf-danger)",
                  values: series.map((b) => b.v),
                  pen: "written",
                  format: (n: number) => num(n),
                },
              ]}
              height={220}
              annotations={annotations}
              formatTime={formatTime}
              emptyLabel="No occurrences in this period."
            />
          </Panel>

          {/* ── the stack ───────────────────────────────────────────────── */}
          <Panel>
            <PanelHead
              title="Stack"
              note={
                stackFrames.length
                  ? "As captured in the browser, innermost frame first."
                  : undefined
              }
              actions={
                detail.stack ? (
                  <Button
                    size="sm"
                    icon={copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
                    onClick={() => void copyStack()}
                  >
                    {copied ? "Copied" : "Copy stack"}
                  </Button>
                ) : undefined
              }
            />
            {stackFrames.length ? (
              <ol className="mf-stack" role="list">
                {stackFrames.map((line, i) => (
                  <li className="mf-frame" data-top={i === 0 ? "true" : "false"} key={`${i}-${line}`}>
                    <span className="mf-frame__n">{i + 1}</span>
                    <span className="mf-frame__text">{line}</span>
                  </li>
                ))}
              </ol>
            ) : (
              <p className="mf-hs-none">
                No stack was captured with this error. Browsers withhold the stack for scripts
                served cross-origin without <code className="mf-data">crossorigin="anonymous"</code>{" "}
                and a matching CORS header.
              </p>
            )}
          </Panel>

          {/* ── where it fired ──────────────────────────────────────────── */}
          <Panel>
            <PanelHead
              title="Where it fired"
              note="Ranked by occurrences."
            />
            <div className="mf-hsplit">
              <RankedColumn
                head="Pages"
                label="Path"
                rows={detail.by_page}
                format={(v) => shortPath(v, 30)}
              />
              <RankedColumn head="Browsers" label="Browser" rows={detail.by_browser} />
              <RankedColumn head="Systems" label="System" rows={detail.by_os} />
            </div>
          </Panel>

          {/* ── the sessions it happened in ─────────────────────────────── */}
          <Panel>
            <PanelHead
              title="Sessions where it happened"
              note="Open one to see what the visitor did before it fired."
            />
            <Sheet
              rows={detail.sessions ?? []}
              columns={sessionColumns}
              getKey={(row, i) => `${row.session_id}-${i}`}
              empty={
                <Empty
                  title="No sessions returned"
                  body="Sessions appear here once the error is joined to session records. A session that has aged past this site's retention window is gone for good."
                />
              }
            />
          </Panel>
        </>
      )}
    </div>
  );
}

export default ErrorDetail;
