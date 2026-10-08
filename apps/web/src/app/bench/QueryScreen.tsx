/**
 * THE QUERY SCREEN.
 *
 * You choose a shape, a metric, a dimension and a set of conditions, and the
 * request that will actually be sent is shown beside the builder before
 * anything runs. Queries only read; nothing here writes to the site.
 *
 * This file also holds the SHARED QUERY PARTS: the field controls, the
 * condition builder, the loading states and the query-key helper, because the
 * query language lives here and the rest of the workspace (dashboards, flags,
 * experiments) borrows the same parts rather than growing a parallel set.
 *
 * Two things are deliberate and should not be "tidied":
 *
 *  1. THE BUILDER DOES NOT AUTO-RUN. A query runs when you tell it to, so the
 *     screen says plainly when the builder has changed since the last run.
 *     Cmd/Ctrl+Enter runs it from anywhere on the screen.
 *  2. THE REQUEST IS SHOWN, NOT DESCRIBED. The URL is built by the same
 *     serialiser the client uses, so it can never drift from what is sent.
 */
import { useCallback, useEffect, useId, useMemo, useState } from "react";
import type { InputHTMLAttributes, ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, ApiError } from "../../lib/api";
import type {
  Audience,
  BreakdownRow,
  Compare,
  Envelope,
  Filter,
  Granularity,
  Overview,
  Series,
} from "../../lib/api";
import {
  AudienceSwitch,
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  RangePicker,
  Readout,
  Sheet,
} from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import { Drum } from "../../components/react/Drum";
import { IconChevron, IconClose, IconDownload, IconPlus } from "../../components/react/icons";
import { duration, num, pct } from "../../lib/format";
import { useAppState } from "../state";
import { useSite } from "../providers";

/* ═══ the query vocabulary ════════════════════════════════════════════════ */

export type MetricKey =
  | "visitors"
  | "sessions"
  | "pageviews"
  | "bounce_rate"
  | "avg_duration_s"
  | "views_per_session";

export interface MetricDef {
  v: MetricKey;
  label: string;
  fmt: (n: number) => string;
  /** A fall is the good outcome (bounce rate). */
  inverse?: boolean;
}

export const METRICS: MetricDef[] = [
  { v: "visitors", label: "Visitors", fmt: num },
  { v: "sessions", label: "Sessions", fmt: num },
  { v: "pageviews", label: "Pageviews", fmt: num },
  { v: "bounce_rate", label: "Bounce rate", fmt: (n) => pct(n / 100), inverse: true },
  { v: "avg_duration_s", label: "Average session", fmt: duration },
  { v: "views_per_session", label: "Views per session", fmt: (n) => n.toFixed(2) },
];

/** `/api/stats/breakdown` rows do not carry views-per-session. */
export const BREAKDOWN_METRICS = METRICS.filter((m) => m.v !== "views_per_session");

export const metricDef = (k: MetricKey): MetricDef =>
  METRICS.find((m) => m.v === k) ?? METRICS[0];

/**
 * The dimension list is CONTRACT.md section 3.2, verbatim and grouped. Nothing is
 * added to it here; a dimension the contract does not list does not exist.
 */
export const DIMENSIONS: { group: string; items: { v: string; label: string }[] }[] = [
  {
    group: "Page",
    items: [
      { v: "pathname", label: "Path" },
      { v: "entry_path", label: "Entry path" },
      { v: "exit_path", label: "Exit path" },
      { v: "hostname", label: "Hostname" },
      { v: "page_title", label: "Page title" },
      { v: "querystring", label: "Query string" },
      { v: "hash", label: "Hash" },
    ],
  },
  {
    group: "Acquisition",
    items: [
      { v: "channel", label: "Channel" },
      { v: "source", label: "Source" },
      { v: "referrer", label: "Referrer" },
      { v: "referrer_host", label: "Referrer host" },
    ],
  },
  {
    group: "Campaign",
    items: [
      { v: "utm_source", label: "UTM source" },
      { v: "utm_medium", label: "UTM medium" },
      { v: "utm_campaign", label: "UTM campaign" },
      { v: "utm_term", label: "UTM term" },
      { v: "utm_content", label: "UTM content" },
    ],
  },
  {
    group: "Technology",
    items: [
      { v: "browser", label: "Browser" },
      { v: "browser_version", label: "Browser version" },
      { v: "os", label: "Operating system" },
      { v: "os_version", label: "OS version" },
      { v: "device_type", label: "Device" },
      { v: "screen_class", label: "Screen class" },
      { v: "language", label: "Language" },
      { v: "timezone", label: "Time zone" },
    ],
  },
  {
    group: "Place",
    items: [
      { v: "country", label: "Country" },
      { v: "region", label: "Region" },
      { v: "city", label: "City" },
      { v: "asn_org", label: "Network" },
      { v: "is_datacenter", label: "Datacentre address" },
    ],
  },
  {
    group: "Behaviour",
    items: [
      { v: "event_name", label: "Event name" },
      { v: "goal", label: "Goal" },
      { v: "experiment", label: "Experiment" },
      { v: "variant", label: "Variant" },
      { v: "visitor_id", label: "Visitor" },
      { v: "identified_id", label: "Identified id" },
      { v: "revenue", label: "Revenue" },
    ],
  },
  {
    group: "AI crawlers",
    items: [
      { v: "agent_id", label: "Crawler" },
      { v: "operator", label: "Operator" },
      { v: "purpose", label: "Declared purpose" },
      { v: "verified", label: "Verified" },
      { v: "robots_allowed", label: "Allowed by robots.txt" },
      { v: "status", label: "Status code" },
      { v: "content_type", label: "Content type" },
    ],
  },
];

/** Flat lookup for labelling a dimension anywhere in the workspace. */
export function dimensionLabel(v: string): string {
  if (v.startsWith("props.")) return `Property: ${v.slice(6)}`;
  if (v.startsWith("flag.")) return `Flag: ${v.slice(5)}`;
  for (const g of DIMENSIONS) for (const i of g.items) if (i.v === v) return i.label;
  return v;
}

export const OPS: { v: Filter["op"]; label: string; valueless?: boolean }[] = [
  { v: "is", label: "is" },
  { v: "is_not", label: "is not" },
  { v: "contains", label: "contains" },
  { v: "not_contains", label: "does not contain" },
  { v: "starts_with", label: "starts with" },
  { v: "ends_with", label: "ends with" },
  { v: "matches", label: "matches (regex)" },
  { v: "gt", label: "is greater than" },
  { v: "lt", label: "is less than" },
  { v: "gte", label: "is at least" },
  { v: "lte", label: "is at most" },
  { v: "is_set", label: "is set", valueless: true },
  { v: "is_not_set", label: "is not set", valueless: true },
];

export const opIsValueless = (op: Filter["op"]): boolean =>
  OPS.find((o) => o.v === op)?.valueless === true;

/* ═══ query keys ══════════════════════════════════════════════════════════ */

/**
 * The shared key shape: ["stats", endpoint, siteId, range, audience, filtersJson].
 * `extra` carries whatever else changes the answer (a dimension, a grain, a limit)
 * so two different reads can never share a cache entry.
 */
export function statsKey(
  endpoint: string,
  siteId: number | undefined,
  range: string,
  audience: Audience,
  filters: Filter[] | undefined,
  extra?: unknown,
): (string | number | undefined)[] {
  return [
    "stats",
    endpoint,
    siteId,
    range,
    audience,
    JSON.stringify(filters ?? []),
    extra === undefined ? "" : JSON.stringify(extra),
  ];
}

/* ═══ loading states ══════════════════════════════════════════════════════ */

/** An error the user can act on: what failed, and what to do about it. */
export function errText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network")
      return "Could not reach the Micaforge server. Check that it is running and that PUBLIC_API_URL points at it.";
    if (e.status === 401 || e.status === 403)
      return "This account cannot read that. Ask an organisation owner for access to the site.";
    if (e.status === 404) return "That item no longer exists. It may have been deleted.";
    return e.message;
  }
  if (e instanceof Error) return e.message;
  return "The server returned something this screen could not read.";
}

/** Grey bars stand in for rows while they load. Never a spinner. */
export function WaitStrip({ lines = 4, label = "Loading…" }: { lines?: number; label?: string }) {
  return (
    <div className="mf-bn-wait" aria-busy="true" aria-live="polite">
      <span className="mf-sr">{label}</span>
      {Array.from({ length: lines }).map((_, i) => (
        <span key={i} className="mf-wait" style={{ width: `${34 + ((i * 23) % 52)}%` }} />
      ))}
    </div>
  );
}

/**
 * Every panel body in the workspace goes through this, so no screen can ship
 * with only its happy path wired.
 */
export function Reading({
  loading,
  error,
  isEmpty,
  empty,
  onRetry,
  lines,
  children,
}: {
  loading?: boolean;
  error?: unknown;
  isEmpty?: boolean;
  empty?: ReactNode;
  onRetry?: () => void;
  lines?: number;
  children: ReactNode;
}) {
  if (loading) return <WaitStrip lines={lines} />;
  if (error) return <ErrorState detail={errText(error)} onRetry={onRetry} />;
  if (isEmpty) return <>{empty ?? <Empty />}</>;
  return <>{children}</>;
}

/* ═══ field controls ══════════════════════════════════════════════════════ */

export function Field({
  label,
  hint,
  error,
  wide,
  control,
}: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string;
  wide?: boolean;
  control: (id: string) => ReactNode;
}) {
  const id = useId();
  return (
    <div className={`mf-bn-field${wide ? " mf-bn-field--wide" : ""}`}>
      <label className="mf-bn-field__label" htmlFor={id}>
        {label}
      </label>
      {control(id)}
      {error ? (
        <p className="mf-bn-field__err" role="alert">
          {error}
        </p>
      ) : hint ? (
        <p className="mf-bn-field__hint">{hint}</p>
      ) : null}
    </div>
  );
}

export function Select({
  id,
  value,
  onChange,
  children,
  ariaLabel,
  disabled,
}: {
  id?: string;
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  ariaLabel?: string;
  disabled?: boolean;
}) {
  return (
    <span className="mf-bn-select">
      <select
        id={id}
        className="mf-bn-select__el"
        value={value}
        aria-label={ariaLabel}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      >
        {children}
      </select>
      <IconChevron size={13} className="mf-bn-select__caret" />
    </span>
  );
}

export function TextInput({
  id,
  value,
  onChange,
  placeholder,
  ariaLabel,
  mono,
  invalid,
  ...rest
}: {
  id?: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  ariaLabel?: string;
  mono?: boolean;
  invalid?: boolean;
} & Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "onChange" | "id">) {
  return (
    <input
      id={id}
      type="text"
      className={`mf-bn-input${mono ? " mf-data" : ""}`}
      value={value}
      placeholder={placeholder}
      aria-label={ariaLabel}
      aria-invalid={invalid || undefined}
      onChange={(e) => onChange(e.target.value)}
      {...rest}
    />
  );
}

export function NumberInput({
  id,
  value,
  onChange,
  min,
  max,
  step,
  ariaLabel,
  suffix,
}: {
  id?: string;
  value: number;
  onChange: (v: number) => void;
  min?: number;
  max?: number;
  step?: number;
  ariaLabel?: string;
  suffix?: string;
}) {
  return (
    <span className="mf-bn-num">
      <input
        id={id}
        type="number"
        className="mf-bn-input mf-data"
        value={Number.isFinite(value) ? value : 0}
        min={min}
        max={max}
        step={step}
        aria-label={ariaLabel}
        onChange={(e) => {
          const n = Number(e.target.value);
          onChange(Number.isFinite(n) ? n : 0);
        }}
      />
      {suffix ? <span className="mf-bn-num__suffix">{suffix}</span> : null}
    </span>
  );
}

export function TextArea({
  id,
  value,
  onChange,
  placeholder,
  rows = 3,
}: {
  id?: string;
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  rows?: number;
}) {
  return (
    <textarea
      id={id}
      className="mf-bn-input mf-bn-input--area"
      value={value}
      rows={rows}
      placeholder={placeholder}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

/**
 * A segmented control: one group of keys, with the selected key raised, the
 * same look as the audience switch.
 */
export function KeyBank<T extends string>({
  label,
  value,
  onChange,
  options,
  id,
}: {
  label: string;
  value: T;
  onChange: (v: T) => void;
  options: { v: T; label: string }[];
  id?: string;
}) {
  return (
    <div className="mf-bn-bank" role="group" aria-label={label} id={id}>
      {options.map((o) => (
        <button
          key={o.v}
          type="button"
          className="mf-bn-bank__key"
          aria-pressed={value === o.v}
          onClick={() => onChange(o.v)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** An on/off control: two keys side by side, the selected one filled. */
export function Throw({
  label,
  on,
  onChange,
  onLabel = "On",
  offLabel = "Off",
  id,
}: {
  label: string;
  on: boolean;
  onChange: (v: boolean) => void;
  onLabel?: string;
  offLabel?: string;
  id?: string;
}) {
  return (
    <div className="mf-bn-throw" role="group" aria-label={label} id={id}>
      <button
        type="button"
        className="mf-bn-throw__pos"
        aria-pressed={!on}
        onClick={() => onChange(false)}
      >
        {offLabel}
      </button>
      <button
        type="button"
        className="mf-bn-throw__pos"
        aria-pressed={on}
        onClick={() => onChange(true)}
      >
        {onLabel}
      </button>
    </div>
  );
}

/* ═══ the condition builder ═══════════════════════════════════════════════ */

const BLANK: Filter = { dimension: "pathname", op: "is", value: "" };

export function FilterBuilder({
  value,
  onChange,
  addLabel = "Add condition",
  emptyNote = "No conditions. The query covers everything in the selected period.",
}: {
  value: Filter[];
  onChange: (next: Filter[]) => void;
  addLabel?: string;
  emptyNote?: string;
}) {
  const set = (i: number, patch: Partial<Filter>) =>
    onChange(value.map((f, k) => (k === i ? { ...f, ...patch } : f)));

  return (
    <div className="mf-bn-cond">
      {value.length === 0 ? (
        <p className="mf-bn-cond__none">{emptyNote}</p>
      ) : (
        <ul className="mf-bn-cond__list">
          {value.map((f, i) => {
            const valueless = opIsValueless(f.op);
            const custom = f.dimension.startsWith("props.") || f.dimension.startsWith("flag.");
            return (
              <li key={i} className="mf-bn-cond__row">
                <span className="mf-bn-cond__join">{i === 0 ? "where" : "and"}</span>

                <Select
                  value={custom ? "__custom" : f.dimension}
                  ariaLabel={`Condition ${i + 1} dimension`}
                  onChange={(v) =>
                    set(i, { dimension: v === "__custom" ? "props." : v })
                  }
                >
                  {DIMENSIONS.map((g) => (
                    <optgroup key={g.group} label={g.group}>
                      {g.items.map((it) => (
                        <option key={it.v} value={it.v}>
                          {it.label}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                  <optgroup label="Custom">
                    <option value="__custom">Property or flag…</option>
                  </optgroup>
                </Select>

                {custom ? (
                  <TextInput
                    value={f.dimension}
                    mono
                    ariaLabel={`Condition ${i + 1} custom dimension key`}
                    placeholder="props.plan"
                    onChange={(v) => set(i, { dimension: v })}
                  />
                ) : null}

                <Select
                  value={f.op}
                  ariaLabel={`Condition ${i + 1} operator`}
                  onChange={(v) => set(i, { op: v as Filter["op"] })}
                >
                  {OPS.map((o) => (
                    <option key={o.v} value={o.v}>
                      {o.label}
                    </option>
                  ))}
                </Select>

                {valueless ? (
                  <span className="mf-bn-cond__novalue">no value needed</span>
                ) : (
                  <TextInput
                    value={String(f.value ?? "")}
                    mono
                    ariaLabel={`Condition ${i + 1} value`}
                    placeholder="value"
                    onChange={(v) => set(i, { value: v })}
                  />
                )}

                <Button
                  size="sm"
                  tone="quiet"
                  icon={<IconClose size={13} />}
                  aria-label={`Remove condition ${i + 1}`}
                  onClick={() => onChange(value.filter((_, k) => k !== i))}
                />
              </li>
            );
          })}
        </ul>
      )}

      <Button
        size="sm"
        icon={<IconPlus size={13} />}
        onClick={() => onChange([...value, { ...BLANK }])}
      >
        {addLabel}
      </Button>
    </div>
  );
}

/* ═══ CSV ═════════════════════════════════════════════════════════════════ */

export function toCsv(headers: string[], rows: (string | number | null | undefined)[][]): string {
  const cell = (v: string | number | null | undefined) => {
    const s = v == null ? "" : String(v);
    return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  return [headers.map(cell).join(","), ...rows.map((r) => r.map(cell).join(","))].join("\r\n");
}

/**
 * The export holds exactly the rows on screen, built in the browser from the
 * response already in hand, so the file can never disagree with the table above it.
 */
export function downloadCsv(filename: string, csv: string): void {
  const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 4000);
}

/* ═══ the query screen ════════════════════════════════════════════════════ */

type Shape = "totals" | "series" | "breakdown";

const SHAPES: { v: Shape; label: string; endpoint: string; note: string }[] = [
  {
    v: "totals",
    label: "Totals",
    endpoint: "/api/stats/overview",
    note: "One set of figures for the whole period.",
  },
  {
    v: "series",
    label: "Series",
    endpoint: "/api/stats/timeseries",
    note: "One metric over time, drawn as a chart.",
  },
  {
    v: "breakdown",
    label: "Breakdown",
    endpoint: "/api/stats/breakdown",
    note: "Ranked rows for one dimension, with every metric on each row.",
  },
];

const GRAINS: { v: Granularity; label: string }[] = [
  { v: "minute", label: "Minute" },
  { v: "hour", label: "Hour" },
  { v: "day", label: "Day" },
  { v: "week", label: "Week" },
  { v: "month", label: "Month" },
];

interface BuilderState {
  shape: Shape;
  metric: MetricKey;
  dimension: string;
  grain: Granularity | "";
  limit: number;
  compare: Compare;
  columns: MetricKey[];
  sortBy: MetricKey;
  sortDir: "desc" | "asc";
  filters: Filter[];
}

const DEFAULT_BUILDER: BuilderState = {
  shape: "breakdown",
  metric: "visitors",
  dimension: "pathname",
  grain: "",
  limit: 25,
  compare: "none",
  columns: ["visitors", "pageviews", "bounce_rate"],
  sortBy: "visitors",
  sortDir: "desc",
  filters: [],
};

interface SavedQuery extends BuilderState {
  id: string;
  name: string;
  savedAt: number;
}

const savedKey = (siteId: number | undefined) => `mf-bench-queries-${siteId ?? "none"}`;

function readSaved(siteId: number | undefined): SavedQuery[] {
  try {
    const raw = localStorage.getItem(savedKey(siteId));
    const parsed = raw ? (JSON.parse(raw) as SavedQuery[]) : [];
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function writeSaved(siteId: number | undefined, list: SavedQuery[]): void {
  try {
    localStorage.setItem(savedKey(siteId), JSON.stringify(list));
  } catch {
    /* private mode or blocked storage: the list simply does not persist */
  }
}

interface RanRequest {
  shape: Shape;
  endpoint: string;
  env: Envelope;
  builder: BuilderState;
}

export function QueryScreen() {
  const { range, setRange, audience, setAudience, filters: screenFilters, clearFilters, addFilter } =
    useAppState();
  const { siteId, isLoading: siteLoading } = useSite();

  const [b, setB] = useState<BuilderState>(() => ({
    ...DEFAULT_BUILDER,
    filters: screenFilters?.length ? screenFilters : [],
  }));
  const patch = useCallback((p: Partial<BuilderState>) => setB((prev) => ({ ...prev, ...p })), []);

  const shapeDef = SHAPES.find((s) => s.v === b.shape) ?? SHAPES[0];

  /** The exact envelope this builder would send, right now. */
  const built = useMemo<RanRequest>(() => {
    const env: Envelope = {
      site: siteId ?? 0,
      range,
      audience,
      filters: b.filters.filter((f) => opIsValueless(f.op) || String(f.value ?? "") !== ""),
    };
    if (b.compare !== "none") env.compare = b.compare;
    if (b.shape === "series") {
      env.metric = b.metric;
      if (b.grain) env.granularity = b.grain;
    }
    if (b.shape === "breakdown") {
      env.dimension = b.dimension;
      env.limit = b.limit;
    }
    return { shape: b.shape, endpoint: shapeDef.endpoint, env, builder: b };
  }, [b, range, audience, siteId, shapeDef.endpoint]);

  const [ran, setRan] = useState<RanRequest | null>(null);

  // One run on arrival, so the screen is never empty. After that it runs only
  // when asked.
  useEffect(() => {
    if (siteId != null && !ran) setRan(built);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [siteId]);

  const dirty =
    ran != null && JSON.stringify({ ...ran.env, site: 0 }) !== JSON.stringify({ ...built.env, site: 0 });

  const run = useCallback(() => setRan(built), [built]);

  // Keyboard first: Cmd+Enter (or Ctrl+Enter) runs from anywhere on the screen.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        run();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [run]);

  const [mod, setMod] = useState("Ctrl");
  useEffect(() => {
    const ua = `${navigator.platform ?? ""} ${navigator.userAgent ?? ""}`;
    if (/Mac|iPhone|iPad|iPod/.test(ua)) setMod("Cmd");
  }, []);

  const q = useQuery({
    queryKey: statsKey(ran?.endpoint ?? "", siteId, range, audience, ran?.env.filters as Filter[], {
      shape: ran?.shape,
      dimension: ran?.env.dimension,
      metric: ran?.env.metric,
      granularity: ran?.env.granularity,
      limit: ran?.env.limit,
      compare: ran?.env.compare,
    }),
    enabled: siteId != null && ran != null,
    queryFn: () => api.get<Overview | Series | BreakdownRow[]>(ran!.endpoint, ran!.env),
  });

  const requestUrl = siteId != null ? api.exportUrl(built.endpoint, built.env) : "";

  /* ── saved queries ─────────────────────────────────────────────────────── */
  const [saved, setSaved] = useState<SavedQuery[]>([]);
  const [saveName, setSaveName] = useState("");
  useEffect(() => setSaved(readSaved(siteId)), [siteId]);

  const commitSaved = (next: SavedQuery[]) => {
    setSaved(next);
    writeSaved(siteId, next);
  };

  const saveCurrent = () => {
    const name = saveName.trim();
    if (!name) return;
    const entry: SavedQuery = { ...b, id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`, name, savedAt: Date.now() };
    commitSaved([entry, ...saved.filter((s) => s.name !== name)].slice(0, 40));
    setSaveName("");
  };

  return (
    <div className="mf-bn-page">
      <Panel className="mf-bn-console">
        <PanelHead
          title="Query"
          note="Build a query with the same parameters every screen uses. Queries only read data; nothing here changes the site."
          actions={
            <>
              <RangePicker value={range} onChange={setRange} />
              <AudienceSwitch value={audience} onChange={setAudience} />
            </>
          }
        />

        <div className="mf-bn-console__grid">
          {/* ── the builder ──────────────────────────────────────────────── */}
          <section className="mf-bn-build" aria-label="Request builder">
            <div className="mf-bn-build__row">
              <Field
                label="Shape"
                hint={shapeDef.note}
                control={(id) => (
                  <KeyBank<Shape>
                    id={id}
                    label="Shape"
                    value={b.shape}
                    onChange={(v) => patch({ shape: v })}
                    options={SHAPES.map((s) => ({ v: s.v, label: s.label }))}
                  />
                )}
              />
            </div>

            <div className="mf-bn-build__row">
              {b.shape === "series" ? (
                <>
                  <Field
                    label="Metric"
                    control={(id) => (
                      <Select id={id} value={b.metric} onChange={(v) => patch({ metric: v as MetricKey })}>
                        {METRICS.map((m) => (
                          <option key={m.v} value={m.v}>
                            {m.label}
                          </option>
                        ))}
                      </Select>
                    )}
                  />
                  <Field
                    label="Time grain"
                    hint="On Automatic, the server picks a grain that fits the period."
                    control={(id) => (
                      <Select id={id} value={b.grain} onChange={(v) => patch({ grain: v as Granularity | "" })}>
                        <option value="">Automatic</option>
                        {GRAINS.map((g) => (
                          <option key={g.v} value={g.v}>
                            {g.label}
                          </option>
                        ))}
                      </Select>
                    )}
                  />
                </>
              ) : null}

              {b.shape === "breakdown" ? (
                <>
                  <Field
                    label="Dimension"
                    control={(id) => (
                      <Select
                        id={id}
                        value={
                          b.dimension.startsWith("props.") || b.dimension.startsWith("flag.")
                            ? "__custom"
                            : b.dimension
                        }
                        onChange={(v) => patch({ dimension: v === "__custom" ? "props." : v })}
                      >
                        {DIMENSIONS.map((g) => (
                          <optgroup key={g.group} label={g.group}>
                            {g.items.map((it) => (
                              <option key={it.v} value={it.v}>
                                {it.label}
                              </option>
                            ))}
                          </optgroup>
                        ))}
                        <optgroup label="Custom">
                          <option value="__custom">Property or flag…</option>
                        </optgroup>
                      </Select>
                    )}
                  />
                  {b.dimension.startsWith("props.") || b.dimension.startsWith("flag.") ? (
                    <Field
                      label="Key"
                      hint="props.plan, or flag.checkout-v2"
                      control={(id) => (
                        <TextInput id={id} mono value={b.dimension} onChange={(v) => patch({ dimension: v })} />
                      )}
                    />
                  ) : null}
                  <Field
                    label="Row limit"
                    control={(id) => (
                      <NumberInput id={id} value={b.limit} min={1} max={1000} step={5} onChange={(v) => patch({ limit: Math.max(1, Math.min(1000, Math.round(v))) })} />
                    )}
                  />
                </>
              ) : null}

              {b.shape !== "breakdown" ? (
                <Field
                  label="Compare against"
                  control={(id) => (
                    <Select id={id} value={b.compare} onChange={(v) => patch({ compare: v as Compare })}>
                      <option value="none">Nothing</option>
                      <option value="previous">The previous period</option>
                      <option value="year">The same period last year</option>
                    </Select>
                  )}
                />
              ) : null}
            </div>

            {b.shape === "breakdown" ? (
              <div className="mf-bn-build__row">
                <Field
                  label="Columns"
                  hint="Display only. The breakdown returns every metric on every row."
                  wide
                  control={(id) => (
                    <div className="mf-bn-checks" id={id} role="group" aria-label="Columns">
                      {BREAKDOWN_METRICS.map((m) => {
                        const on = b.columns.includes(m.v);
                        return (
                          <label key={m.v} className="mf-bn-check">
                            <input
                              type="checkbox"
                              checked={on}
                              onChange={() =>
                                patch({
                                  columns: on
                                    ? b.columns.filter((c) => c !== m.v)
                                    : [...b.columns, m.v],
                                })
                              }
                            />
                            <span>{m.label}</span>
                          </label>
                        );
                      })}
                    </div>
                  )}
                />
                <Field
                  label="Order rows by"
                  hint="Applied to the rows the server returned."
                  control={(id) => (
                    <div className="mf-bn-pair">
                      <Select id={id} value={b.sortBy} onChange={(v) => patch({ sortBy: v as MetricKey })}>
                        {BREAKDOWN_METRICS.map((m) => (
                          <option key={m.v} value={m.v}>
                            {m.label}
                          </option>
                        ))}
                      </Select>
                      <KeyBank<"desc" | "asc">
                        label="Order direction"
                        value={b.sortDir}
                        onChange={(v) => patch({ sortDir: v })}
                        options={[
                          { v: "desc", label: "Highest first" },
                          { v: "asc", label: "Lowest first" },
                        ]}
                      />
                    </div>
                  )}
                />
              </div>
            ) : null}

            <div className="mf-bn-build__row mf-bn-build__row--cond">
              <Field
                label="Conditions"
                wide
                control={() => (
                  <FilterBuilder value={b.filters} onChange={(filters) => patch({ filters })} />
                )}
              />
            </div>

            <div className="mf-bn-build__act">
              <Button tone="primary" onClick={run}>
                Run query
              </Button>
              <span className="mf-bn-kbd">
                <kbd className="mf-data">{mod}</kbd>
                <span aria-hidden="true">+</span>
                <kbd className="mf-data">Enter</kbd>
              </span>
              {b.filters.length > 0 ? (
                <Button
                  tone="quiet"
                  size="sm"
                  onClick={() => {
                    clearFilters();
                    for (const f of b.filters) addFilter(f);
                  }}
                >
                  Apply these conditions to every screen
                </Button>
              ) : null}
              {dirty ? (
                <span className="mf-bn-dirty" role="status">
                  The query has changed since the last run.
                </span>
              ) : null}
            </div>
          </section>

          {/* ── the request ──────────────────────────────────────────────── */}
          <section className="mf-bn-tape" aria-label="Generated request">
            <h3 className="mf-bn-tape__title">The request</h3>
            {siteId == null ? (
              <p className="mf-bn-tape__wait">Waiting for the site.</p>
            ) : (
              <>
                <p className="mf-bn-tape__line mf-data">
                  <span className="mf-bn-tape__verb">GET</span> {built.endpoint}
                </p>
                <pre className="mf-bn-tape__body mf-data">{JSON.stringify(built.env, null, 2)}</pre>
                <div className="mf-bn-tape__url">
                  <code className="mf-data">{requestUrl}</code>
                  <Button
                    size="sm"
                    onClick={() => {
                      navigator.clipboard?.writeText(requestUrl).catch(() => {
                        /* clipboard blocked: the URL above is selectable */
                      });
                    }}
                  >
                    Copy URL
                  </Button>
                </div>
                <dl className="mf-bn-meta">
                  <div>
                    <dt>Rows</dt>
                    <dd className="mf-data">{q.data?.meta?.rows != null ? num(q.data.meta.rows) : "—"}</dd>
                  </div>
                  <div>
                    <dt>Took</dt>
                    <dd className="mf-data">
                      {q.data?.meta?.took_ms != null ? `${num(q.data.meta.took_ms)} ms` : "—"}
                    </dd>
                  </div>
                  <div>
                    <dt>Sampled</dt>
                    <dd className="mf-data">{q.data?.meta ? (q.data.meta.sampled ? "yes" : "no") : "—"}</dd>
                  </div>
                </dl>
              </>
            )}
          </section>
        </div>

        {/* ── saved queries ──────────────────────────────────────────────── */}
        <section className="mf-bn-saved" aria-label="Saved queries">
          <h3 className="mf-bn-saved__title">
            Saved queries <span className="mf-bn-saved__where">kept in this browser, not on the server</span>
          </h3>
          <div className="mf-bn-saved__save">
            <TextInput
              value={saveName}
              onChange={setSaveName}
              placeholder="Name this query"
              ariaLabel="Name for the saved query"
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  saveCurrent();
                }
              }}
            />
            <Button size="sm" disabled={!saveName.trim()} onClick={saveCurrent}>
              Save
            </Button>
          </div>
          {saved.length === 0 ? (
            <p className="mf-bn-saved__none">
              Nothing saved yet. Name a query above and it comes back next time you open this browser.
            </p>
          ) : (
            <ul className="mf-bn-saved__list">
              {saved.map((s) => (
                <li key={s.id} className="mf-bn-saved__row">
                  <button
                    type="button"
                    className="mf-bn-saved__load"
                    onClick={() => {
                      const { id, name, savedAt, ...rest } = s;
                      void id;
                      void name;
                      void savedAt;
                      setB(rest);
                    }}
                  >
                    {s.name}
                  </button>
                  <span className="mf-bn-saved__desc">
                    {SHAPES.find((x) => x.v === s.shape)?.label}
                    {s.shape === "breakdown" ? ` · ${dimensionLabel(s.dimension)}` : ""}
                    {s.shape === "series" ? ` · ${metricDef(s.metric).label}` : ""}
                    {s.filters.length ? ` · ${s.filters.length} condition${s.filters.length === 1 ? "" : "s"}` : ""}
                  </span>
                  <Button
                    size="sm"
                    tone="quiet"
                    icon={<IconClose size={13} />}
                    aria-label={`Delete saved query ${s.name}`}
                    onClick={() => commitSaved(saved.filter((x) => x.id !== s.id))}
                  />
                </li>
              ))}
            </ul>
          )}
        </section>
      </Panel>

      {/* ── the result ───────────────────────────────────────────────────── */}
      <Panel>
        <PanelHead
          title="Result"
          note={
            ran
              ? `${SHAPES.find((s) => s.v === ran.shape)?.label} · ${ran.env.audience === "agent" ? "AI crawlers" : ran.env.audience === "all" ? "people and AI crawlers" : "people"} · ${range}`
              : "No query run yet."
          }
          actions={
            <ExportKey ran={ran} data={q.data?.data} builder={b} />
          }
        />
        <div className="mf-bn-result">
          <Reading
            loading={siteLoading || q.isLoading}
            error={q.isError ? q.error : undefined}
            onRetry={() => void q.refetch()}
            isEmpty={!ran || isResultEmpty(ran.shape, q.data?.data)}
            lines={6}
            empty={
              <Empty
                title="No results for this query"
                body="The period, the audience or a condition may exclude everything. Choose a longer period or remove a condition, then run it again."
                action={<Button onClick={run}>Run query</Button>}
              />
            }
          >
            {ran ? <ResultView ran={ran} data={q.data?.data} builder={b} /> : null}
          </Reading>
        </div>
      </Panel>
    </div>
  );
}

function isResultEmpty(shape: Shape, data: unknown): boolean {
  if (data == null) return true;
  if (shape === "breakdown") return (data as BreakdownRow[]).length === 0;
  if (shape === "series") return ((data as Series).buckets ?? []).length === 0;
  const o = data as Overview;
  return !o || (o.visitors === 0 && o.sessions === 0 && o.pageviews === 0);
}

function sortRows(rows: BreakdownRow[], by: MetricKey, dir: "desc" | "asc"): BreakdownRow[] {
  const key = by as keyof BreakdownRow;
  return [...rows].sort((a, b) => {
    const av = Number(a[key] ?? 0);
    const bv = Number(b[key] ?? 0);
    return dir === "desc" ? bv - av : av - bv;
  });
}

function ResultView({ ran, data, builder }: { ran: RanRequest; data: unknown; builder: BuilderState }) {
  if (ran.shape === "totals") {
    const o = data as Overview;
    const prev = o.compare ?? null;
    const channels: Channel[] = METRICS.map((m) => {
      const now = Number((o as unknown as Record<string, number>)[m.v] ?? 0);
      const before = prev ? Number((prev as unknown as Record<string, number>)[m.v] ?? 0) : null;
      return {
        key: m.v,
        label: m.label,
        value: m.fmt(now),
        delta: before == null || before === 0 ? null : (now - before) / before,
        inverse: m.inverse,
      };
    });
    return <Readout channels={channels} />;
  }

  if (ran.shape === "series") {
    const s = data as Series;
    const def = metricDef(ran.builder.metric);
    const aud = ran.env.audience as Audience | undefined;
    const times = s.buckets.map((x) => x.t);
    return (
      <Drum
        times={times}
        height={280}
        channels={[
          {
            id: "read",
            label: def.label,
            color: aud === "agent" ? "var(--mf-ai)" : "var(--mf-people)",
            pen: aud === "agent" ? "stepped" : "written",
            values: s.buckets.map((x) => x.v),
            format: def.fmt,
          },
          ...(s.compare && s.compare.length
            ? [
                {
                  id: "before",
                  label: ran.env.compare === "year" ? "Last year" : "Previous period",
                  color: "var(--mf-ink-3)",
                  pen: (aud === "agent" ? "stepped" : "written") as "stepped" | "written",
                  ghost: true,
                  values: s.compare.map((x) => x.v),
                  format: def.fmt,
                },
              ]
            : []),
        ]}
      />
    );
  }

  const rows = sortRows(data as BreakdownRow[], builder.sortBy, builder.sortDir);
  const cols: Column<BreakdownRow>[] = [
    {
      key: "label",
      head: dimensionLabel(String(ran.env.dimension ?? "")),
      render: (r) => <span title={r.label || r.key}>{r.label || r.key || "(not set)"}</span>,
      share: (r) => r.share,
    },
    ...builder.columns.map<Column<BreakdownRow>>((c) => {
      const def = metricDef(c);
      return {
        key: c,
        head: def.label,
        numeric: true,
        render: (r) => (
          <span className="mf-data">{def.fmt(Number((r as unknown as Record<string, number>)[c] ?? 0))}</span>
        ),
      };
    }),
    {
      key: "share",
      head: "Share",
      numeric: true,
      render: (r) => <span className="mf-data">{pct(r.share)}</span>,
    },
  ];

  return <Sheet rows={rows} columns={cols} getKey={(r, i) => `${r.key}-${i}`} />;
}

function ExportKey({
  ran,
  data,
  builder,
}: {
  ran: RanRequest | null;
  data: unknown;
  builder: BuilderState;
}) {
  const disabled = !ran || data == null || isResultEmpty(ran.shape, data);
  return (
    <Button
      size="sm"
      icon={<IconDownload size={13} />}
      disabled={disabled}
      onClick={() => {
        if (!ran || data == null) return;
        const stamp = new Date().toISOString().slice(0, 10);
        if (ran.shape === "totals") {
          const o = data as Overview;
          downloadCsv(
            `micaforge-totals-${stamp}.csv`,
            toCsv(
              METRICS.map((m) => m.label),
              [METRICS.map((m) => Number((o as unknown as Record<string, number>)[m.v] ?? 0))],
            ),
          );
          return;
        }
        if (ran.shape === "series") {
          const s = data as Series;
          downloadCsv(
            `micaforge-series-${ran.builder.metric}-${stamp}.csv`,
            toCsv(
              ["time", metricDef(ran.builder.metric).label],
              s.buckets.map((x) => [new Date(x.t).toISOString(), x.v]),
            ),
          );
          return;
        }
        const rows = sortRows(data as BreakdownRow[], builder.sortBy, builder.sortDir);
        downloadCsv(
          `micaforge-${String(ran.env.dimension ?? "breakdown")}-${stamp}.csv`,
          toCsv(
            [dimensionLabel(String(ran.env.dimension ?? "")), ...builder.columns.map((c) => metricDef(c).label), "share"],
            rows.map((r) => [
              r.label || r.key,
              ...builder.columns.map((c) => Number((r as unknown as Record<string, number>)[c] ?? 0)),
              r.share,
            ]),
          ),
        );
      }}
    >
      Download CSV
    </Button>
  );
}

export default QueryScreen;
