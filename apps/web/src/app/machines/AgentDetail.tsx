/**
 * The detail panel for one AI crawler, and the parts the four AI traffic
 * screens share.
 *
 * WHY THE SHARED PARTS LIVE HERE. This area ships a fixed file list, so the
 * pieces every AI traffic screen needs (the envelope, the query key, the
 * control bar, the async states, the sort head) live in the leaf of the import
 * graph rather than in a seventh file. AgentDetail is that leaf: it imports no
 * sibling, so every other screen can import from it with no cycle.
 */
import type { ReactNode } from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { UseQueryResult } from "@tanstack/react-query";

import { useAppState } from "../state";
import { useSite } from "../providers";
import { publicOrigin, ApiError, api } from "../../lib/api";
import type { Audience, Envelope, Filter } from "../../lib/api";
import {
  Button,
  Empty,
  ErrorState,
  RangePicker,
  Readout,
  Sheet,
  VerifiedMark,
} from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { Drum } from "../../components/react/Drum";
import { IconChevron, IconClose, IconInfo, IconOut } from "../../components/react/icons";
import { ago, bytes, gapHours, num, purposeLabel, shortPath } from "../../lib/format";
import { agentName } from "../display";

/* ══ the envelope ═══════════════════════════════════════════════════════════ */

/**
 * The shared query key, exactly as the contract sets it:
 *   ["stats", endpoint, siteId, range, audience, filtersJson]
 * Anything a single screen varies (a pivot dimension, a page) is appended after
 * that prefix so a site- or range-level invalidation still catches it.
 */
export function statsKey(
  endpoint: string,
  siteId: number | string | undefined,
  range: string,
  audience: Audience,
  filters: Filter[],
  ...extra: unknown[]
): unknown[] {
  return ["stats", endpoint, siteId ?? null, range, audience, JSON.stringify(filters ?? []), ...extra];
}

export interface MachineEnv {
  env: Envelope;
  key: (endpoint: string, ...extra: unknown[]) => unknown[];
  ready: boolean;
  siteId: number | string | undefined;
  site: string;
  domain: string | undefined;
  range: string;
  filters: Filter[];
  addFilter: (f: Filter) => void;
  /**
   * The single point where this area touches the store's filter removal. If the
   * store ever changes that signature, it changes here and nowhere else.
   */
  dropFilter: (index: number) => void;
  clearFilters: () => void;
}

/**
 * The AI traffic screens do not carry an audience switch (they are only about
 * AI crawlers), so the audience is stated per endpoint instead of read from the
 * store, and the user's own audience choice on the other screens is left alone.
 */
export function useMachineEnv(audience: Audience = "agent"): MachineEnv {
  const state = useAppState();
  const { siteId, domain, timezone, isLoading } = useSite();
  const { range, filters, addFilter, removeFilter, clearFilters } = state;

  const env = useMemo<Envelope>(
    () => ({
      site: siteId ?? "",
      range,
      audience,
      filters,
      ...(timezone ? { tz: timezone } : {}),
    }),
    [siteId, range, audience, filters, timezone],
  );

  const key = useCallback(
    (endpoint: string, ...extra: unknown[]) =>
      statsKey(endpoint, siteId, range, audience, filters, ...extra),
    [siteId, range, audience, filters],
  );

  return {
    env,
    key,
    ready: !isLoading && siteId != null && String(siteId) !== "",
    siteId,
    site: String(siteId ?? ""),
    domain,
    range,
    filters,
    addFilter,
    dropFilter: (index: number) => removeFilter(index),
    clearFilters,
  };
}

/* ══ async states ═══════════════════════════════════════════════════════════ */

/** Names the problem, then the way out of it. Never just "something failed". */
export function errorText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network") {
      return `The dashboard could not reach the Micaforge server at ${publicOrigin()}. Check that it is running and reachable, then try again.`;
    }
    if (e.status === 403 || e.status === 401) {
      return `${e.message} Sign in again, or ask an owner for access to this site.`;
    }
    if (e.status >= 500) {
      return `${e.message} The server logged this one; retrying is safe.`;
    }
    return e.message;
  }
  return e instanceof Error ? e.message : "The server sent data the dashboard does not understand.";
}

/** A skeleton of grey bars. A drawn loading state, never invisible content. */
export function Waiting({ rows = 4, plot = false }: { rows?: number; plot?: boolean }) {
  return (
    <div
      className={`mf-wedge-wait${plot ? " mf-wedge-wait--plot" : ""}`}
      aria-busy="true"
      aria-live="polite"
    >
      <span className="mf-sr">Loading…</span>
      {Array.from({ length: rows }).map((_, i) => (
        <span key={i} className="mf-wait" style={{ width: `${34 + ((i * 23) % 56)}%` }} aria-hidden="true" />
      ))}
    </div>
  );
}

/**
 * Every panel in this area passes through here, so loading, empty and error are
 * never the case somebody forgot.
 */
export function Reading<T>({
  query,
  waiting,
  isEmpty,
  empty,
  children,
}: {
  query: UseQueryResult<T, unknown>;
  waiting?: ReactNode;
  isEmpty?: (data: T) => boolean;
  empty?: ReactNode;
  children: (data: T) => ReactNode;
}) {
  if (query.isPending) return <>{waiting ?? <Waiting />}</>;
  if (query.isError) {
    return (
      <ErrorState
        title="Could not load this data"
        detail={errorText(query.error)}
        onRetry={() => void query.refetch()}
      />
    );
  }
  const data = query.data as T;
  if (isEmpty?.(data)) return <>{empty ?? <Empty />}</>;
  return <>{children(data)}</>;
}

/* ══ the control bar ════════════════════════════════════════════════════════ */

/** An active filter, with the button that removes it. */
function FilterMark({ filter, index, onDrop }: { filter: Filter; index: number; onDrop: (i: number) => void }) {
  const value = Array.isArray(filter.value) ? filter.value.join(", ") : String(filter.value);
  const op =
    filter.op === "is"
      ? ""
      : filter.op === "is_not"
        ? "not "
        : `${filter.op.replace(/_/g, " ")} `;
  return (
    <span className="mf-mark">
      <span className="mf-mark__dim">
        {filter.dimension.replace(/_/g, " ")} {op}
      </span>
      <span className="mf-mark__val" title={value}>
        {value}
      </span>
      <button
        type="button"
        className="mf-mark__off"
        onClick={() => onDrop(index)}
        aria-label={`Remove the ${filter.dimension} filter`}
      >
        <IconClose size={12} />
      </button>
    </span>
  );
}

/**
 * The control bar: the screen title on the left and the range on the right, on
 * one line with one rule under both. Deliberately not a title stacked over a
 * subtitle stacked over a toolbar.
 */
export function Bench({
  title,
  controls,
  bench,
}: {
  title: string;
  controls?: ReactNode;
  bench: MachineEnv;
}) {
  const { range, filters, dropFilter, clearFilters } = bench;
  const state = useAppState();
  return (
    <header className="mf-wedge-bar">
      <h1 className="mf-wedge-bar__title">{title}</h1>
      <div className="mf-wedge-bar__controls">
        {controls}
        <RangePicker value={range} onChange={state.setRange} />
      </div>
      {filters.length > 0 && (
        <div className="mf-wedge-bar__marks">
          <span className="mf-wedge-bar__marks-label">Filtered to</span>
          {filters.map((f, i) => (
            <FilterMark key={`${f.dimension}-${f.op}-${i}`} filter={f} index={i} onDrop={dropFilter} />
          ))}
          <Button size="sm" tone="quiet" onClick={clearFilters}>
            Clear all filters
          </Button>
        </div>
      )}
    </header>
  );
}

/* ══ sorting ════════════════════════════════════════════════════════════════ */

export function SortHead({
  label,
  active,
  dir,
  onClick,
}: {
  label: ReactNode;
  active: boolean;
  dir: "asc" | "desc";
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="mf-sort"
      data-active={active ? "true" : "false"}
      onClick={onClick}
      title={
        active
          ? `Sorted by this column, ${dir === "asc" ? "smallest first" : "largest first"}. Click to reverse.`
          : "Sort by this column"
      }
    >
      {label}
      <span className="mf-sort__dir" data-dir={dir} aria-hidden="true">
        <IconChevron size={12} />
      </span>
    </button>
  );
}

export function useSort<K extends string>(initial: K, initialDir: "asc" | "desc" = "desc") {
  const [by, setBy] = useState<K>(initial);
  const [dir, setDir] = useState<"asc" | "desc">(initialDir);
  const toggle = useCallback(
    (k: K) => {
      if (k === by) {
        setDir((d) => (d === "asc" ? "desc" : "asc"));
        return;
      }
      setBy(k);
      setDir("desc");
    },
    [by],
  );
  const head = useCallback(
    (k: K, label: ReactNode) => (
      <SortHead label={label} active={by === k} dir={dir} onClick={() => toggle(k)} />
    ),
    [by, dir, toggle],
  );
  return { by, dir, head };
}

/** Stable sort on a derived number or string. */
export function sorted<T>(rows: T[], pick: (row: T) => number | string, dir: "asc" | "desc"): T[] {
  const sign = dir === "asc" ? 1 : -1;
  return rows
    .map((row, i) => ({ row, i, k: pick(row) }))
    .sort((a, b) => {
      if (typeof a.k === "string" || typeof b.k === "string") {
        const r = String(a.k).localeCompare(String(b.k));
        return r !== 0 ? r * sign : a.i - b.i;
      }
      const av = Number.isFinite(a.k as number) ? (a.k as number) : sign > 0 ? Infinity : -Infinity;
      const bv = Number.isFinite(b.k as number) ? (b.k as number) : sign > 0 ? Infinity : -Infinity;
      return av === bv ? a.i - b.i : (av - bv) * sign;
    })
    .map((e) => e.row);
}

/* ══ shared vocabulary ══════════════════════════════════════════════════════ */

export type VerifiedState = "verified" | "unverified" | "failed";
export type RobotsState = "allowed" | "disallowed" | "unlisted";

/** `robots_allowed` on the event row is -1 unknown, 0 disallowed, 1 allowed. */
export function robotsState(v: number | null | undefined): RobotsState {
  return v === 1 ? "allowed" : v === 0 ? "disallowed" : "unlisted";
}

export function robotsWord(s: RobotsState): string {
  return s === "allowed" ? "Allowed" : s === "disallowed" ? "Disallowed" : "No rule";
}

/**
 * The caveat that keeps this product honest, written wherever a reader could
 * otherwise mistake a claimed identity for a proven one.
 */
export function VerificationCaveat({ children }: { children?: ReactNode }) {
  return (
    <p className="mf-caveat">
      <IconInfo size={14} />
      <span>
        {children ?? (
          <>
            A user-agent string is a claim, not proof. Micaforge marks a crawler{" "}
            <b>verified</b> only when its IP address matched the ranges its operator
            publishes. Anything else shows as <b>unverified</b> and is never counted as a
            violation.
          </>
        )}
      </span>
    </p>
  );
}

/* ══ the crawler detail panel ═══════════════════════════════════════════════ */

export interface AgentIdentity {
  agent_id: string;
  label: string;
  operator: string;
  purpose: string;
  verified: VerifiedState;
  robots_allowed?: number | null;
  fetches?: number;
  unique_paths?: number;
  bytes?: number;
  last_seen?: number | null;
}

interface CatalogEntry {
  slug: string;
  display_name: string;
  operator: string;
  purpose: string;
  docs_url: string | null;
  respects_robots: boolean;
}

export interface AgentSeriesResponse {
  buckets: number[];
  series: { key: string; label?: string; values: number[] }[];
}

export interface AgentBreakdownRow {
  key: string;
  label: string;
  operator?: string;
  /** The operator's name as it should be shown, e.g. "OpenAI" for `openai`. */
  operator_label?: string;
  purpose?: string;
  /** That purpose in a reader's words, e.g. "Training corpus". */
  purpose_label?: string;
  docs_url?: string | null;
  respects_robots?: boolean | null;
  verified?: VerifiedState;
  robots_allowed?: number | null;
  fetches: number;
  unique_paths?: number;
  bytes?: number;
  last_seen?: number | null;
  status?: number;
  conditional?: number;
  share?: number;
}

/**
 * The detail panel. It answers three questions in order: who claims to be
 * this, whether that claim was checked, and what it actually did.
 */
export function AgentDetail({
  agent,
  bench,
  onClose,
  onFilterToAgent,
}: {
  agent: AgentIdentity;
  bench: MachineEnv;
  onClose: () => void;
  onFilterToAgent?: (agentId: string) => void;
}) {
  const closeRef = useRef<HTMLButtonElement | null>(null);
  const stripRef = useRef<HTMLElement | null>(null);
  const returnTo = useRef<Element | null>(null);

  /* A dialog that says aria-modal has to behave like one: it takes focus when it
     opens, keeps the tab ring inside itself, closes on Escape, and hands focus
     back to whatever opened it. */
  useEffect(() => {
    returnTo.current = document.activeElement;
    closeRef.current?.focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
        return;
      }
      if (e.key !== "Tab") return;
      const root = stripRef.current;
      if (!root) return;
      const focusable = Array.from(
        root.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => el.offsetParent !== null || el === document.activeElement);
      if (!focusable.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      const inside = active instanceof Node && root.contains(active);
      if (e.shiftKey && (!inside || active === first)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (!inside || active === last)) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      if (returnTo.current instanceof HTMLElement) returnTo.current.focus();
    };
  }, [onClose]);

  const scoped = useMemo<Envelope>(
    () => ({
      ...bench.env,
      filters: [
        ...(bench.env.filters ?? []),
        { dimension: "agent_id", op: "is", value: agent.agent_id } as Filter,
      ],
    }),
    [bench.env, agent.agent_id],
  );
  const scopedFilters = scoped.filters ?? [];

  const trace = useQuery({
    queryKey: statsKey("agents/timeseries", bench.siteId, bench.range, "agent", scopedFilters, "one"),
    queryFn: async () => (await api.get<AgentSeriesResponse>("/api/agents/timeseries", scoped)).data,
    enabled: bench.ready,
  });

  const paths = useQuery({
    queryKey: statsKey("agents/breakdown", bench.siteId, bench.range, "agent", scopedFilters, "pathname", 12),
    queryFn: async () =>
      (
        await api.get<AgentBreakdownRow[]>("/api/agents/breakdown", {
          ...scoped,
          dimension: "pathname",
          limit: 12,
        })
      ).data,
    enabled: bench.ready,
  });

  const statuses = useQuery({
    queryKey: statsKey("agents/breakdown", bench.siteId, bench.range, "agent", scopedFilters, "status"),
    queryFn: async () =>
      (await api.get<AgentBreakdownRow[]>("/api/agents/breakdown", { ...scoped, dimension: "status" })).data,
    enabled: bench.ready,
  });

  const catalog = useQuery({
    queryKey: ["catalog", "agents"],
    queryFn: async () => (await api.get<CatalogEntry[]>("/api/agents/catalog")).data,
    staleTime: 60 * 60 * 1000,
  });

  const entry = catalog.data?.find((c) => c.slug === agent.agent_id);
  const robots = robotsState(agent.robots_allowed);

  const pathColumns: Column<AgentBreakdownRow>[] = [
    {
      key: "path",
      head: "Path",
      render: (r) => (
        <span className="mf-path" title={r.label || r.key}>
          {shortPath(r.label || r.key, 34)}
        </span>
      ),
      share: (r) => r.share,
    },
    { key: "fetches", head: "Fetches", numeric: true, render: (r) => <span className="mf-data">{num(r.fetches)}</span> },
  ];

  const statusColumns: Column<AgentBreakdownRow>[] = [
    {
      key: "status",
      head: "Status",
      render: (r) => <span className="mf-data">{r.label || r.key}</span>,
      share: (r) => r.share,
    },
    { key: "fetches", head: "Fetches", numeric: true, render: (r) => <span className="mf-data">{num(r.fetches)}</span> },
  ];

  return (
    <>
      <button type="button" className="mf-strip__scrim" aria-label="Close crawler details" onClick={onClose} />
      <aside
        ref={stripRef}
        className="mf-strip"
        role="dialog"
        aria-modal="true"
        aria-labelledby="mf-strip-name"
      >
        <header className="mf-strip__head">
          <div className="mf-strip__id">
            <h2 className="mf-strip__name" id="mf-strip-name">
              {agentName(agent.label, agent.agent_id)}
            </h2>
            <p className="mf-strip__op">
              {agent.operator} · {purposeLabel(agent.purpose)}
            </p>
          </div>
          <Button
            ref={closeRef}
            className="mf-strip__close"
            size="sm"
            icon={<IconClose size={14} />}
            onClick={onClose}
          >
            Close
          </Button>
        </header>

        <section className="mf-strip__section">
          <h3 className="mf-strip__h">Identity</h3>
          <dl className="mf-kv">
            <dt>Catalog ID</dt>
            <dd className="mf-data">{agent.agent_id}</dd>
            <dt>Verification</dt>
            <dd>
              <VerifiedMark state={agent.verified} />{" "}
              {agent.verified === "verified"
                ? "Verified against the operator's published IP ranges"
                : agent.verified === "failed"
                  ? "Claimed this name from an IP address the operator does not publish"
                  : "No published IP ranges to check against"}
            </dd>
            <dt>Your robots.txt</dt>
            <dd>{robotsWord(robots)}</dd>
            <dt>Last seen</dt>
            <dd>{agent.last_seen ? ago(agent.last_seen) : "never"}</dd>
            {entry ? (
              <>
                <dt>Declared</dt>
                <dd>
                  {entry.respects_robots
                    ? "The operator states this crawler obeys robots.txt"
                    : "The operator makes no robots.txt commitment for this crawler"}
                </dd>
              </>
            ) : null}
            {entry?.docs_url ? (
              <>
                <dt>Operator docs</dt>
                <dd>
                  <a className="mf-ext" href={entry.docs_url} target="_blank" rel="noreferrer noopener">
                    {entry.docs_url.replace(/^https?:\/\//, "")}
                    <IconOut size={12} />
                  </a>
                </dd>
              </>
            ) : null}
          </dl>
          {agent.verified !== "verified" ? <VerificationCaveat /> : null}
        </section>

        <section className="mf-strip__section">
          <h3 className="mf-strip__h">What it did</h3>
          <Readout
            dense
            channels={[
              { key: "f", label: "Fetches", value: num(agent.fetches ?? 0) },
              { key: "p", label: "Paths reached", value: num(agent.unique_paths ?? 0) },
              { key: "b", label: "Bytes served", value: bytes(agent.bytes ?? 0) },
            ]}
          />
          <Reading
            query={trace}
            waiting={<Waiting rows={3} plot />}
            isEmpty={(d) => !d?.buckets?.length || !d?.series?.length}
            empty={
              <Empty
                title="Nothing to chart"
                body="No series came back for this crawler in this period. The totals above still count every fetch."
              />
            }
          >
            {(d) => (
              <Drum
                times={d.buckets}
                height={150}
                live
                channels={[
                  {
                    id: agent.agent_id,
                    label: agent.label,
                    color: "var(--mf-ai)",
                    pen: "stepped",
                    values: d.series[0]?.values ?? [],
                    format: (n) => num(n),
                  },
                ]}
              />
            )}
          </Reading>
        </section>

        <section className="mf-strip__section">
          <h3 className="mf-strip__h">Paths it fetched most</h3>
          <div className="mf-strip__sheet">
            <Reading
              query={paths}
              waiting={<Waiting rows={5} />}
              isEmpty={(d) => !d?.length}
              empty={<Empty title="No paths" body="This crawler fetched nothing in this period." />}
            >
              {(d) => <Sheet rows={d} columns={pathColumns} getKey={(r) => r.key} />}
            </Reading>
          </div>
        </section>

        <section className="mf-strip__section">
          <h3 className="mf-strip__h">How the server answered</h3>
          <div className="mf-strip__sheet">
            <Reading
              query={statuses}
              waiting={<Waiting rows={3} />}
              isEmpty={(d) => !d?.length}
              empty={
                <Empty
                  title="No status codes"
                  body="Status codes come from the server-side ingest path. The browser script alone cannot see them."
                />
              }
            >
              {(d) => <Sheet rows={d} columns={statusColumns} getKey={(r) => r.key} />}
            </Reading>
          </div>
        </section>

        {onFilterToAgent ? (
          <section className="mf-strip__section">
            <Button
              tone="primary"
              onClick={() => {
                onFilterToAgent(agent.agent_id);
                onClose();
              }}
            >
              Filter the screen to {agentName(agent.label, agent.agent_id)}
            </Button>
          </section>
        ) : null}
      </aside>
    </>
  );
}

/* ══ small shared parts ═════════════════════════════════════════════════════ */

/** A coverage figure that says "never crawled" out loud rather than an em dash. */
export function FreshnessCell({ hours }: { hours: number | null | undefined }) {
  const never = hours == null || !Number.isFinite(hours);
  return <span className={never ? "mf-dim" : "mf-data"}>{gapHours(hours)}</span>;
}
