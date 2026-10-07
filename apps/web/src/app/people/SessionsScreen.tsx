/**
 * PEOPLE: sessions.
 *
 * One row per run of the pen. The column that matters is the last one: every
 * session says whether a person or a named machine wrote it, because in this
 * product a crawler is an audience, not a filtered-out nuisance.
 *
 * This file is also the People area's shared floor. The session shapes, the
 * time formatting bound to the site's own timezone, and the `Hand` mark are
 * used by the visitor and replay screens too, so they are declared and exported
 * once here rather than copied into four files.
 */
import type { ReactNode } from "react";
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import type { Envelope, Filter } from "../../lib/api";
import { channelLabel, duration, num, shortPath } from "../../lib/format";
import { agentName, titleCase } from "../display";
import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconDownload, IconSearch } from "../../components/react/icons";
import { useAppState } from "../state";
import { useSite } from "../providers";
import "../../styles/app/people.css";

/* ── shapes ──────────────────────────────────────────────────────────────
   Fields follow docs/CONTRACT.md §1.1 and §1.4. Everything past the identity
   and the start is optional on purpose: the ingest path fills what it has, and
   a screen that throws because a column was empty is a screen that lies about
   the health of the pipeline. */

export interface SessionRow {
  session_id: string;
  visitor_id?: string;
  identified_id?: string;
  started: number | string;
  ended?: number | string;
  duration_s?: number;
  pageviews?: number;
  events?: number;
  entry_path?: string;
  exit_path?: string;
  channel?: string;
  source?: string;
  referrer?: string;
  referrer_host?: string;
  utm_source?: string;
  utm_medium?: string;
  utm_campaign?: string;
  utm_term?: string;
  utm_content?: string;
  click_id?: string;
  country?: string;
  region?: string;
  city?: string;
  device_type?: string;
  browser?: string;
  browser_version?: string;
  os?: string;
  os_version?: string;
  screen_w?: number;
  screen_h?: number;
  viewport_w?: number;
  viewport_h?: number;
  language?: string;
  timezone?: string;
  bounced?: boolean;
  revenue?: number | null;
  currency?: string;
  has_replay?: boolean;
  /** the machine side, from `agent_events` */
  is_agent?: boolean | number;
  agent_id?: string;
  agent_name?: string;
  operator?: string;
  purpose?: string;
  verified?: boolean | number;
  user_agent?: string;
}

export interface SessionHit {
  timestamp: number | string;
  kind?: string;
  event_name?: string;
  pathname?: string;
  page_title?: string;
  querystring?: string;
  hash?: string;
  duration_ms?: number;
  scroll_pct?: number;
  status?: number;
  bytes?: number;
  method?: string;
  props?: Record<string, unknown> | null;
  revenue?: number | null;
}

/* ── shared helpers ─────────────────────────────────────────────────────── */

/** The API may send a DateTime64 as epoch milliseconds or as ISO-8601. */
export function toMs(v: number | string | null | undefined): number {
  if (v == null) return Number.NaN;
  if (typeof v === "number") return v < 1e12 ? v * 1000 : v;
  const parsed = Date.parse(v);
  if (Number.isFinite(parsed)) return parsed;
  const asNum = Number(v);
  return Number.isFinite(asNum) ? (asNum < 1e12 ? asNum * 1000 : asNum) : Number.NaN;
}

/** A list payload may arrive as a bare array or wrapped under a named key. */
export function asRows<T>(payload: unknown, key: string): T[] {
  if (Array.isArray(payload)) return payload as T[];
  if (payload && typeof payload === "object") {
    const v = (payload as Record<string, unknown>)[key];
    if (Array.isArray(v)) return v as T[];
    const rows = (payload as Record<string, unknown>).rows;
    if (Array.isArray(rows)) return rows as T[];
  }
  return [];
}

export function totalOf(payload: unknown): number | null {
  if (payload && typeof payload === "object" && !Array.isArray(payload)) {
    const t = (payload as Record<string, unknown>).total;
    if (typeof t === "number" && Number.isFinite(t)) return t;
  }
  return null;
}

/** Errors name the problem; the state that shows them names the recovery. */
export function apiMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === "network") return "The Micaforge server did not answer. It may be restarting.";
    return err.message;
  }
  return err instanceof Error ? err.message : "The reading failed for a reason the dashboard could not name.";
}

const fmtCache = new Map<string, Intl.DateTimeFormat>();
function fmt(key: string, opts: Intl.DateTimeFormatOptions, tz?: string): Intl.DateTimeFormat {
  const id = `${key}|${tz ?? ""}`;
  let f = fmtCache.get(id);
  if (!f) {
    try {
      f = new Intl.DateTimeFormat(undefined, tz ? { ...opts, timeZone: tz } : opts);
    } catch {
      f = new Intl.DateTimeFormat(undefined, opts);
    }
    fmtCache.set(id, f);
  }
  return f;
}

/** Clock time in the site's own timezone: the operator reads their own day. */
export const clockAt = (t: number | string | null | undefined, tz?: string): string => {
  const ms = toMs(t);
  return Number.isFinite(ms)
    ? fmt("clock", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false }, tz).format(ms)
    : "—";
};

export const stampAt = (t: number | string | null | undefined, tz?: string): string => {
  const ms = toMs(t);
  return Number.isFinite(ms)
    ? fmt("stamp", { year: "numeric", month: "short", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }, tz).format(ms)
    : "—";
};

export const dayAt = (t: number | string | null | undefined, tz?: string): string => {
  const ms = toMs(t);
  return Number.isFinite(ms) ? fmt("day", { month: "short", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }, tz).format(ms) : "—";
};

let regionNames: Intl.DisplayNames | null | undefined;
export function countryName(cc: string | undefined): string {
  const code = (cc ?? "").trim().toUpperCase();
  if (code.length !== 2) return "—";
  if (regionNames === undefined) {
    try {
      regionNames = new Intl.DisplayNames(undefined, { type: "region" });
    } catch {
      regionNames = null;
    }
  }
  try {
    return regionNames?.of(code) ?? code;
  } catch {
    return code;
  }
}

const DEVICES: Record<string, string> = {
  desktop: "Desktop",
  mobile: "Phone",
  tablet: "Tablet",
  tv: "TV",
  wearable: "Wearable",
  console: "Console",
};
export const deviceLabel = (d: string | undefined): string => (d ? (DEVICES[d] ?? d) : "—");

export const isAgentSession = (s: Pick<SessionRow, "is_agent" | "agent_id">): boolean =>
  s.is_agent === true || s.is_agent === 1 || (!!s.agent_id && s.agent_id.length > 0);

/**
 * The hand that wrote the run. The pen SHAPE carries the meaning (a written
 * line for a person, a stepped corner for a machine), so the distinction holds
 * with no colour at all, and the name is written out beside it.
 */
export function Hand({ session }: { session: Pick<SessionRow, "is_agent" | "agent_id" | "agent_name"> }) {
  const machine = isAgentSession(session);
  const name = machine ? agentName(session.agent_name, session.agent_id) : "Reader";
  return (
    <span className="mf-p-hand" data-pen={machine ? "stepped" : "written"}>
      <span className="mf-p-hand__pen" data-pen={machine ? "stepped" : "written"} aria-hidden="true" />
      <span className="mf-p-hand__name">{name}</span>
    </span>
  );
}

/**
 * Links are built from the `:site` segment actually in the URL, not from the
 * numeric site id the API wants, so a slug in the address bar survives a hop
 * into a detail screen and back.
 */
export function useBase(): string {
  const { site } = useParams();
  const { siteId } = useSite();
  return `/app/${site ?? siteId ?? ""}`;
}

/** The envelope every read in this area is built from. */
export function useEnvelope(extra?: Record<string, unknown>): Envelope {
  const { siteId, timezone } = useSite();
  const { range, audience, filters } = useAppState();
  return useMemo(
    () => ({
      // Every read that uses this envelope is gated on `siteId != null`, so the
      // cast never reaches the network with an absent site.
      site: siteId as number,
      range,
      audience,
      tz: timezone || undefined,
      filters: (filters ?? []) as Filter[],
      ...extra,
    }),
    // `extra` is spread by value; the JSON keeps the identity stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [siteId, range, audience, timezone, JSON.stringify(filters ?? []), JSON.stringify(extra ?? {})],
  );
}

/** The shared key shape, so one invalidation reaches every panel that read it. */
export function statsKey(
  endpoint: string,
  siteId: number | string | undefined,
  range: string,
  audience: string,
  filters: unknown,
  ...rest: unknown[]
) {
  return ["stats", endpoint, siteId, range, audience, JSON.stringify(filters ?? []), ...rest] as const;
}

/* ── pagination ─────────────────────────────────────────────────────────── */

const PAGE = 50;

export function Pager({
  page,
  onPage,
  count,
  total,
  noun,
}: {
  page: number;
  onPage: (p: number) => void;
  count: number;
  total: number | null;
  noun: string;
}) {
  const from = page * PAGE + 1;
  const to = page * PAGE + count;
  const more = total != null ? to < total : count === PAGE;
  return (
    <div className="mf-p-pager">
      <p className="mf-p-pager__at">
        {count === 0
          ? `No ${noun} on this page`
          : total != null
            ? `${num(from)}–${num(to)} of ${num(total)} ${noun}`
            : `${num(from)}–${num(to)} ${noun}`}
      </p>
      <Button size="sm" disabled={page === 0} onClick={() => onPage(Math.max(0, page - 1))}>
        Earlier page
      </Button>
      <Button size="sm" disabled={!more} onClick={() => onPage(page + 1)}>
        Later page
      </Button>
    </div>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

export function SessionsScreen() {
  const navigate = useNavigate();
  const base = useBase();
  const { siteId, domain, timezone } = useSite();
  const { range, audience, filters } = useAppState();
  const [page, setPage] = useState(0);
  const [find, setFind] = useState("");
  const [findApplied, setFindApplied] = useState("");

  const filterKey = JSON.stringify(filters ?? []);
  // A new interval, audience or filter set is a new record: start at its head.
  useEffect(() => {
    setPage(0);
  }, [range, audience, filterKey, findApplied]);

  const pathFilter: Filter[] = findApplied
    ? [{ dimension: "pathname", op: "contains", value: findApplied }]
    : [];
  const env = useEnvelope({ limit: PAGE, page });
  const envelope: Envelope = {
    ...env,
    filters: [...((env.filters as Filter[]) ?? []), ...pathFilter],
  };

  const q = useQuery({
    queryKey: statsKey("sessions", siteId, range, audience, envelope.filters, page),
    queryFn: () => api.get<unknown>("/api/stats/sessions", envelope),
    enabled: siteId != null,
    placeholderData: keepPreviousData,
  });

  const rows = useMemo(() => asRows<SessionRow>(q.data?.data, "sessions"), [q.data]);
  const total = totalOf(q.data?.data);
  const machines = rows.filter(isAgentSession).length;

  const columns: Column<SessionRow>[] = [
    {
      key: "started",
      head: "Started",
      width: "15ch",
      render: (r) => (
        <Link
          className="mf-data"
          to={`${base}/sessions/${encodeURIComponent(r.session_id)}`}
          onClick={(e) => e.stopPropagation()}
        >
          {dayAt(r.started, timezone)}
        </Link>
      ),
    },
    {
      key: "duration",
      head: "Duration",
      numeric: true,
      width: "10ch",
      render: (r) => <span className="mf-data">{duration(r.duration_s)}</span>,
    },
    {
      key: "pageviews",
      head: "Pages",
      numeric: true,
      width: "8ch",
      render: (r) => <span className="mf-data">{num(r.pageviews ?? 0)}</span>,
    },
    {
      key: "entry",
      head: "Entry",
      render: (r) => <span title={r.entry_path}>{shortPath(r.entry_path ?? "/", 28)}</span>,
    },
    {
      key: "exit",
      head: "Exit",
      render: (r) => <span title={r.exit_path}>{shortPath(r.exit_path ?? "/", 28)}</span>,
    },
    {
      key: "channel",
      head: "Channel",
      width: "16ch",
      render: (r) => (
        <span>
          {channelLabel(r.channel ?? "direct")}
          {r.source ? <span className="mf-p-dim"> · {titleCase(r.source)}</span> : null}
        </span>
      ),
    },
    {
      key: "country",
      head: "Country",
      width: "14ch",
      render: (r) => <span>{countryName(r.country)}</span>,
    },
    {
      key: "device",
      head: "Device",
      width: "12ch",
      render: (r) => <span>{isAgentSession(r) ? "—" : deviceLabel(r.device_type)}</span>,
    },
    {
      key: "hand",
      head: "Written by",
      width: "16ch",
      render: (r) => <Hand session={r} />,
    },
  ];

  // Never state a figure the response did not carry: without a total, the page
  // count is the only honest number on offer.
  const note = !q.isSuccess
    ? "A session opens on the first hit and closes after thirty minutes of quiet."
    : total != null
      ? `${num(total)} ${total === 1 ? "session" : "sessions"} in this interval.` +
        (machines ? ` ${num(machines)} of the ${num(rows.length)} shown were written by a named agent.` : "")
      : `Newest first.` +
        (machines ? ` ${num(machines)} of the ${num(rows.length)} shown were written by a named agent.` : "");

  return (
    <div className="mf-p-stack">
      <Panel ruled feed>
        <PanelHead
          title="Sessions"
          note={note}
          actions={
            <>
              <form
                className="mf-p-find"
                onSubmit={(e) => {
                  e.preventDefault();
                  setFindApplied(find.trim());
                }}
              >
                <IconSearch size={14} />
                <input
                  type="search"
                  value={find}
                  placeholder="Path contains…"
                  aria-label="Show only sessions that touched a path"
                  onChange={(e) => setFind(e.target.value)}
                  onBlur={() => setFindApplied(find.trim())}
                />
              </form>
              {findApplied ? (
                <Button
                  size="sm"
                  onClick={() => {
                    setFind("");
                    setFindApplied("");
                  }}
                >
                  Clear path
                </Button>
              ) : null}
              <a
                className="mf-btn mf-btn--key mf-btn--sm"
                href={api.exportUrl("/api/stats/export.csv", { ...envelope, report: "sessions" })}
              >
                <span className="mf-btn__icon">
                  <IconDownload size={14} />
                </span>
                <span className="mf-btn__label">Export CSV</span>
              </a>
            </>
          }
        />

        {q.isError ? (
          <ErrorState detail={apiMessage(q.error)} onRetry={() => q.refetch()} />
        ) : (
          <>
            <Sheet
              rows={rows}
              columns={columns}
              loading={q.isPending}
              getKey={(r, i) => r.session_id || String(i)}
              onRowClick={(r) => navigate(`${base}/sessions/${encodeURIComponent(r.session_id)}`)}
              empty={
                <Empty
                  title={findApplied ? "No session touched that path" : "No sessions in this interval"}
                  body={
                    findApplied ? (
                      <>Nothing in this interval hit a path containing “{findApplied}”. Clear the path, or widen the interval.</>
                    ) : (
                      <>
                        Sessions appear here as soon as the tracker reports its first hit
                        {domain ? ` from ${domain}` : ""}. Widen the interval, or check that the audience switch is not
                        hiding the traffic you are looking for.
                      </>
                    )
                  }
                />
              }
            />
            {rows.length || page > 0 ? (
              <Pager page={page} onPage={setPage} count={rows.length} total={total} noun="sessions" />
            ) : null}
          </>
        )}
      </Panel>
    </div>
  );
}

/** Small shared bits the sibling screens re-use for their own headers. */
export function Subject({
  id,
  when,
  actions,
}: {
  id: ReactNode;
  when?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="mf-p-subject">
      <span className="mf-p-subject__id">{id}</span>
      {when ? <span className="mf-p-subject__when">{when}</span> : null}
      {actions ? <div className="mf-p-subject__acts">{actions}</div> : null}
    </div>
  );
}

export { PAGE as PEOPLE_PAGE_SIZE };
