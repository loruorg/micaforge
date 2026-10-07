/**
 * The clip rack: the control that decides which part of the record is being read.
 *
 * A recording station narrows a reading by clipping the roll, and it writes on
 * the clip what it holds. So the conditions here are written out as sentences,
 * "Path starts with /docs", "Agent is gptbot", each one removable on its own.
 * They are deliberately NOT a row of tinted capsules: a capsule tells you a
 * value exists, a sentence tells you what it is doing to your numbers, and rank
 * is carried by type rather than by a coloured box.
 *
 * Three parts, in the order an operator uses them:
 *   1. the conditions already clipped on, written out;
 *   2. the builder that adds one, whose value field is fed by what the record
 *      has actually observed for that dimension rather than by guesswork;
 *   3. the saved segments, which are just a set of clips somebody named.
 */
import type { ReactNode } from "react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import * as RPopover from "@radix-ui/react-popover";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import type { Audience, BreakdownRow, Filter } from "../../lib/api";
import { compact } from "../../lib/format";
import { useAppState } from "../../app/state";
import { useSite } from "../../app/providers";
import { Button } from "./ui";
import { IconChevron, IconClose, IconFilter, IconPlus } from "./icons";

/* ══ the vocabulary ═══════════════════════════════════════════════════════ */

/**
 * CONTRACT §3.2, grouped for reading and otherwise verbatim. A dimension this
 * list does not name does not exist, so nothing is added here that the contract
 * has not agreed to first.
 */
export const FILTER_DIMENSIONS: { group: string; items: { v: string; label: string }[] }[] = [
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
    group: "Machines",
    items: [
      { v: "agent_id", label: "Agent" },
      { v: "operator", label: "Operator" },
      { v: "purpose", label: "Declared purpose" },
      { v: "verified", label: "Verified" },
      { v: "robots_allowed", label: "Allowed by robots.txt" },
      { v: "status", label: "Status code" },
      { v: "content_type", label: "Content type" },
    ],
  },
  {
    group: "Your own keys",
    items: [
      { v: "props.", label: "Event property" },
      { v: "flag.", label: "Feature flag" },
    ],
  },
];

/** The two dimensions that are a prefix plus a key the operator names. */
const KEYED = new Set(["props.", "flag."]);

export const FILTER_OPS: { v: Filter["op"]; label: string; valueless?: boolean }[] = [
  { v: "is", label: "is" },
  { v: "is_not", label: "is not" },
  { v: "contains", label: "contains" },
  { v: "not_contains", label: "does not contain" },
  { v: "starts_with", label: "starts with" },
  { v: "ends_with", label: "ends with" },
  { v: "matches", label: "matches the pattern" },
  { v: "gt", label: "is more than" },
  { v: "lt", label: "is less than" },
  { v: "gte", label: "is at least" },
  { v: "lte", label: "is at most" },
  { v: "is_set", label: "is set", valueless: true },
  { v: "is_not_set", label: "is not set", valueless: true },
];

export const opTakesNoValue = (op: Filter["op"]): boolean =>
  FILTER_OPS.find((o) => o.v === op)?.valueless === true;

/** Dimensions whose values are measured rather than written. */
const NUMERIC = new Set(["revenue", "status", "verified", "robots_allowed", "is_datacenter"]);

export function filterDimensionLabel(v: string): string {
  if (v.startsWith("props.") && v.length > 6) return `Property “${v.slice(6)}”`;
  if (v.startsWith("flag.") && v.length > 5) return `Flag “${v.slice(5)}”`;
  for (const g of FILTER_DIMENSIONS) for (const i of g.items) if (i.v === v) return i.label;
  return v;
}

export const filterOpLabel = (op: Filter["op"]): string =>
  FILTER_OPS.find((o) => o.v === op)?.label ?? op;

/** One condition, said the way a person would say it. */
export function filterValueText(f: Filter): string {
  if (opTakesNoValue(f.op)) return "";
  if (Array.isArray(f.value)) {
    if (f.value.length <= 2) return f.value.join(" or ");
    return `${f.value.slice(0, 2).join(", ")} and ${f.value.length - 2} more`;
  }
  return String(f.value);
}

/* ══ saved segments ═══════════════════════════════════════════════════════ */

export interface Segment {
  id: string;
  name: string;
  filters: Filter[];
  shared?: boolean;
}

const segmentsKey = (siteId: number | undefined) => ["segments", siteId ?? null];

/** Names the problem, then the way out of it. Never just "that failed". */
function problem(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network")
      return "Could not reach the Micaforge server. Check that it is running, then try again.";
    if (e.status === 401 || e.status === 403)
      return "Your role on this site does not allow saved segments.";
    return e.message;
  }
  return e instanceof Error ? e.message : "The server did not say what went wrong.";
}

/* ══ the value field ══════════════════════════════════════════════════════ */

/**
 * A value field that offers what the record has actually observed.
 *
 * It is a real combobox, not an input with a decorative list beside it: the
 * suggestions are reachable from the keyboard, the active one is announced, and
 * typing a value the record has never seen is still allowed, because filtering
 * for something that has not happened yet is a legitimate thing to ask.
 */
function ValueField({
  dimension,
  value,
  onChange,
  onCommit,
  siteId,
  range,
  audience,
  disabled,
}: {
  dimension: string;
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  siteId: number | undefined;
  range: string;
  audience: Audience;
  disabled?: boolean;
}) {
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [cursor, setCursor] = useState(-1);
  const boxRef = useRef<HTMLDivElement | null>(null);

  // A keyed dimension has nothing to suggest until its key is written, and the
  // free-text dimensions below have far too many distinct values to rank.
  const askable =
    siteId != null &&
    !disabled &&
    !KEYED.has(dimension) &&
    !dimension.startsWith("props.") &&
    !dimension.startsWith("flag.") &&
    !["visitor_id", "identified_id", "revenue", "querystring", "hash"].includes(dimension);

  const observed = useQuery({
    queryKey: ["stats", "breakdown", siteId ?? null, range, audience, "[]", dimension, "values"],
    enabled: askable && open,
    staleTime: 60_000,
    queryFn: async () => {
      const res = await api.get<BreakdownRow[] | { rows: BreakdownRow[] }>("/api/stats/breakdown", {
        site: siteId as number,
        range,
        audience,
        dimension,
        limit: 60,
      });
      return Array.isArray(res.data) ? res.data : (res.data?.rows ?? []);
    },
  });

  const matches = useMemo(() => {
    const rows = observed.data ?? [];
    const needle = value.trim().toLowerCase();
    const hit = needle
      ? rows.filter(
          (r) =>
            String(r.key).toLowerCase().includes(needle) ||
            String(r.label ?? "").toLowerCase().includes(needle),
        )
      : rows;
    return hit.slice(0, 8);
  }, [observed.data, value]);

  // Close when the pointer goes elsewhere; Escape is handled on the input.
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);

  useEffect(() => setCursor(-1), [value]);

  const take = (v: string) => {
    onChange(v);
    setOpen(false);
    setCursor(-1);
  };

  const showList = open && askable && (matches.length > 0 || observed.isPending);

  return (
    <div className="mfs-combo" ref={boxRef}>
      <input
        className="mfs-input mfs-input--value mf-data"
        type="text"
        inputMode={NUMERIC.has(dimension) ? "decimal" : "text"}
        value={value}
        disabled={disabled}
        spellCheck={false}
        autoComplete="off"
        role="combobox"
        aria-expanded={showList}
        aria-controls={showList ? listId : undefined}
        aria-autocomplete="list"
        aria-activedescendant={cursor >= 0 ? `${listId}-${cursor}` : undefined}
        aria-label="Value"
        placeholder={NUMERIC.has(dimension) ? "a number" : "a value"}
        onChange={(e) => {
          onChange(e.target.value);
          if (askable) setOpen(true);
        }}
        onFocus={() => askable && setOpen(true)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && showList) {
            e.preventDefault();
            setCursor((c) => (c + 1) % Math.max(1, matches.length));
          } else if (e.key === "ArrowUp" && showList) {
            e.preventDefault();
            setCursor((c) => (c <= 0 ? matches.length - 1 : c - 1));
          } else if (e.key === "Enter") {
            e.preventDefault();
            const pick = matches[cursor];
            if (showList && cursor >= 0 && pick) take(String(pick.key));
            else onCommit();
          } else if (e.key === "Escape" && open) {
            e.preventDefault();
            e.stopPropagation();
            setOpen(false);
          }
        }}
      />

      {showList ? (
        <ul className="mfs-combo__list" id={listId} role="listbox" aria-label="Observed values">
          {observed.isPending ? (
            <li className="mfs-combo__waiting" role="presentation">
              <span className="mf-sr">Reading which values the record holds…</span>
              <span className="mf-wait" style={{ width: "62%" }} />
              <span className="mf-wait" style={{ width: "44%" }} />
            </li>
          ) : (
            matches.map((r, i) => (
              <li key={String(r.key)} role="none">
                <button
                  type="button"
                  id={`${listId}-${i}`}
                  role="option"
                  aria-selected={i === cursor}
                  className={`mfs-combo__opt${i === cursor ? " is-cursor" : ""}`}
                  onMouseEnter={() => setCursor(i)}
                  // `pointerdown` fires before the input's blur, so the pick is
                  // taken instead of being lost to the field closing first.
                  onPointerDown={(e) => {
                    e.preventDefault();
                    take(String(r.key));
                  }}
                >
                  <span className="mfs-combo__opt-key mf-data">{r.label || String(r.key)}</span>
                  <span className="mfs-combo__opt-n mf-data">{compact(r.visitors)}</span>
                </button>
              </li>
            ))
          )}
        </ul>
      ) : null}
    </div>
  );
}

/* ══ the builder ══════════════════════════════════════════════════════════ */

function Builder({
  onAdd,
  siteId,
  range,
  audience,
}: {
  onAdd: (f: Filter) => void;
  siteId: number | undefined;
  range: string;
  audience: Audience;
}) {
  const [dimension, setDimension] = useState("pathname");
  const [key, setKey] = useState("");
  const [op, setOp] = useState<Filter["op"]>("is");
  const [value, setValue] = useState("");
  const dimId = useId();
  const opId = useId();
  const keyId = useId();

  const keyed = KEYED.has(dimension);
  const resolved = keyed ? `${dimension}${key.trim()}` : dimension;
  const valueless = opTakesNoValue(op);
  const ready =
    (!keyed || key.trim().length > 0) && (valueless || value.trim().length > 0);

  const commit = () => {
    if (!ready) return;
    const raw = value.trim();
    const numeric = NUMERIC.has(resolved) || ["gt", "lt", "gte", "lte"].includes(op);
    const parsed = numeric && raw !== "" && Number.isFinite(Number(raw)) ? Number(raw) : raw;
    onAdd({ dimension: resolved, op, value: valueless ? "" : parsed });
    setValue("");
  };

  return (
    <form
      className="mfs-build"
      onSubmit={(e) => {
        e.preventDefault();
        commit();
      }}
    >
      <div className="mfs-build__row">
        <label className="mfs-build__lab" htmlFor={dimId}>
          Read only where
        </label>
        <span className="mfs-selwrap">
          <select
            id={dimId}
            className="mfs-select"
            value={dimension}
            onChange={(e) => {
              setDimension(e.target.value);
              setKey("");
              setValue("");
            }}
          >
            {FILTER_DIMENSIONS.map((g) => (
              <optgroup key={g.group} label={g.group}>
                {g.items.map((i) => (
                  <option key={i.v} value={i.v}>
                    {i.label}
                  </option>
                ))}
              </optgroup>
            ))}
          </select>
          <IconChevron size={13} className="mfs-selwrap__caret" />
        </span>
      </div>

      {keyed ? (
        <div className="mfs-build__row">
          <label className="mfs-build__lab" htmlFor={keyId}>
            named
          </label>
          <input
            id={keyId}
            className="mfs-input mf-data"
            value={key}
            spellCheck={false}
            autoComplete="off"
            placeholder={dimension === "props." ? "plan" : "new-nav"}
            onChange={(e) => setKey(e.target.value)}
          />
        </div>
      ) : null}

      <div className="mfs-build__row">
        <label className="mfs-build__lab" htmlFor={opId}>
          and it
        </label>
        <span className="mfs-selwrap">
          <select
            id={opId}
            className="mfs-select mfs-select--op"
            value={op}
            onChange={(e) => setOp(e.target.value as Filter["op"])}
          >
            {FILTER_OPS.map((o) => (
              <option key={o.v} value={o.v}>
                {o.label}
              </option>
            ))}
          </select>
          <IconChevron size={13} className="mfs-selwrap__caret" />
        </span>
      </div>

      {/* The action sits at the END of the sentence being built, beside the
          value rather than under it. That is the way the phrase reads, and it
          is also the only arrangement the observed-values list cannot cover:
          the list drops from the field, so a button below it would be sitting
          under a panel the operator is trying to click through. */}
      <div className="mfs-build__row mfs-build__row--act">
        <span className="mfs-build__lab" aria-hidden="true" />
        <div className="mfs-build__act">
          {valueless ? null : (
            <ValueField
              dimension={resolved}
              value={value}
              onChange={setValue}
              onCommit={commit}
              siteId={siteId}
              range={range}
              audience={audience}
            />
          )}
          <Button type="submit" tone="primary" size="sm" icon={<IconPlus size={13} />} disabled={!ready}>
            Clip this on
          </Button>
        </div>
      </div>
    </form>
  );
}

/* ══ saved segments ═══════════════════════════════════════════════════════ */

function Segments({
  siteId,
  filters,
  onLoad,
}: {
  siteId: number | undefined;
  filters: Filter[];
  onLoad: (f: Filter[]) => void;
}) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const nameId = useId();

  const list = useQuery({
    queryKey: segmentsKey(siteId),
    enabled: siteId != null,
    staleTime: 60_000,
    queryFn: async () => {
      const res = await api.get<Segment[] | { rows: Segment[] }>("/api/segments", {
        site: siteId as number,
      });
      const rows = Array.isArray(res.data) ? res.data : (res.data?.rows ?? []);
      return rows.map((s) => ({ ...s, filters: Array.isArray(s.filters) ? s.filters : [] }));
    },
  });

  const save = useMutation({
    mutationFn: () =>
      api.post<Segment>("/api/segments", {
        site_id: siteId,
        name: name.trim(),
        filters,
        shared: false,
      }),
    onSuccess: () => {
      setName("");
      void qc.invalidateQueries({ queryKey: segmentsKey(siteId) });
    },
  });

  const drop = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/segments/${encodeURIComponent(id)}`),
    onSuccess: () => void qc.invalidateQueries({ queryKey: segmentsKey(siteId) }),
  });

  const rows = list.data ?? [];

  return (
    <div className="mfs-seg">
      <h4 className="mfs-rack__head">Saved segments</h4>

      {list.isPending ? (
        <div className="mfs-seg__waiting" aria-busy="true">
          <span className="mf-sr">Reading the saved segments…</span>
          <span className="mf-wait" style={{ width: "58%" }} />
          <span className="mf-wait" style={{ width: "41%" }} />
        </div>
      ) : list.isError ? (
        <p className="mfs-say" data-tone="warn" role="alert">
          {problem(list.error)}
        </p>
      ) : rows.length === 0 ? (
        <p className="mfs-say">
          Nothing saved yet. Clip some conditions on, then name them below to keep them.
        </p>
      ) : (
        <ul className="mfs-seg__list">
          {rows.map((s) => (
            <li key={s.id} className="mfs-seg__row">
              <button
                type="button"
                className="mfs-seg__load"
                onClick={() => onLoad(s.filters)}
              >
                {s.name}
                <span className="mfs-seg__n mf-data">
                  {s.filters.length} {s.filters.length === 1 ? "condition" : "conditions"}
                </span>
              </button>
              <button
                type="button"
                className="mfs-drop"
                aria-label={`Delete the segment ${s.name}`}
                disabled={drop.isPending}
                onClick={() => drop.mutate(s.id)}
              >
                <IconClose size={12} />
              </button>
            </li>
          ))}
        </ul>
      )}

      <form
        className="mfs-seg__save"
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim() && filters.length) save.mutate();
        }}
      >
        <label className="mf-sr" htmlFor={nameId}>
          Name for these conditions
        </label>
        <input
          id={nameId}
          className="mfs-input"
          value={name}
          spellCheck={false}
          autoComplete="off"
          placeholder={filters.length ? "Name these conditions" : "Clip a condition on first"}
          disabled={!filters.length}
          onChange={(e) => setName(e.target.value)}
        />
        <Button
          type="submit"
          size="sm"
          disabled={!filters.length || !name.trim() || save.isPending}
        >
          {save.isPending ? "Saving…" : "Save"}
        </Button>
      </form>
      {save.isError ? (
        <p className="mfs-say" data-tone="warn" role="alert">
          {problem(save.error)}
        </p>
      ) : null}
      {drop.isError ? (
        <p className="mfs-say" data-tone="warn" role="alert">
          {problem(drop.error)}
        </p>
      ) : null}
    </div>
  );
}

/* ══ the written conditions ═══════════════════════════════════════════════ */

/**
 * What is clipped on, written out. Type carries the rank: the dimension is the
 * subject, the operator is quiet because it is grammar, and the value is set in
 * the data face because it is the thing that came off the record.
 */
export function ConditionList({
  filters,
  onRemove,
  onClear,
}: {
  filters: Filter[];
  onRemove: (index: number) => void;
  onClear: () => void;
}) {
  if (!filters.length) {
    return (
      <p className="mfs-say">
        Nothing is clipped on. The whole record is being read.
      </p>
    );
  }
  return (
    <>
      <ul className="mfs-conds">
        {filters.map((f, i) => (
          <li className="mfs-cond" key={`${f.dimension}-${f.op}-${String(f.value)}-${i}`}>
            <span className="mfs-cond__text">
              <span className="mfs-cond__dim">{filterDimensionLabel(f.dimension)}</span>{" "}
              <span className="mfs-cond__op">{filterOpLabel(f.op)}</span>
              {opTakesNoValue(f.op) ? null : (
                <>
                  {" "}
                  <span className="mfs-cond__val mf-data">{filterValueText(f)}</span>
                </>
              )}
            </span>
            <button
              type="button"
              className="mfs-drop"
              onClick={() => onRemove(i)}
              aria-label={`Unclip ${filterDimensionLabel(f.dimension)} ${filterOpLabel(f.op)} ${filterValueText(f)}`}
            >
              <IconClose size={12} />
            </button>
          </li>
        ))}
      </ul>
      <div className="mfs-rack__foot">
        {/* A real key, not a quiet text control: at the foot of a list, an
            unbordered word reads as a caption rather than as something that
            does anything. */}
        <Button size="sm" onClick={onClear}>
          Unclip everything
        </Button>
      </div>
    </>
  );
}

/* ══ the control ══════════════════════════════════════════════════════════ */

export interface FilterBarProps {
  /** Conditions to show. Defaults to the station's shared settings. */
  filters?: Filter[];
  onAdd?: (f: Filter) => void;
  onRemove?: (index: number) => void;
  onClear?: () => void;
  onReplace?: (f: Filter[]) => void;
  /** Overrides the site the value suggestions are read from. */
  siteId?: number;
  /**
   * `key` is the compact control for the station's top rail: one key that opens
   * the rack. `bar` writes the rack out in place, for a screen that owns a
   * whole row for it.
   */
  variant?: "key" | "bar";
  /** Saved segments are a per-site feature; a screen without one hides them. */
  segments?: boolean;
  label?: ReactNode;
}

export function FilterBar({
  filters: given,
  onAdd,
  onRemove,
  onClear,
  onReplace,
  siteId: siteIdProp,
  variant = "key",
  segments = true,
  label = "Conditions",
}: FilterBarProps) {
  const state = useAppState();
  const site = useSite();
  const [open, setOpen] = useState(false);

  const filters = given ?? state.filters;
  const siteId = siteIdProp ?? site.siteId;

  const add = onAdd ?? state.addFilter;
  const remove = onRemove ?? state.removeFilter;
  const clear = onClear ?? state.clearFilters;
  const replace =
    onReplace ??
    ((next: Filter[]) => {
      state.clearFilters();
      for (const f of next) state.addFilter(f);
    });

  const rack = (
    <div className="mfs-rack">
      <section className="mfs-rack__part">
        <h4 className="mfs-rack__head">Clipped on</h4>
        <ConditionList filters={filters} onRemove={remove} onClear={clear} />
      </section>

      <section className="mfs-rack__part">
        <h4 className="mfs-rack__head">Add a condition</h4>
        <Builder onAdd={add} siteId={siteId} range={state.range} audience={state.audience} />
      </section>

      {segments && siteId != null ? (
        <section className="mfs-rack__part">
          <Segments siteId={siteId} filters={filters} onLoad={replace} />
        </section>
      ) : null}
    </div>
  );

  if (variant === "bar") {
    return <div className="mfs-rack-bar">{rack}</div>;
  }

  return (
    <RPopover.Root open={open} onOpenChange={setOpen}>
      <RPopover.Trigger asChild>
        <button
          type="button"
          className="mfs-key"
          aria-label={
            filters.length
              ? `${label}: ${filters.length} clipped on`
              : `${label}: none clipped on`
          }
        >
          <IconFilter size={14} />
          <span className="mfs-key__label">{label}</span>
          {filters.length ? <span className="mfs-key__n mf-data">{filters.length}</span> : null}
        </button>
      </RPopover.Trigger>
      <RPopover.Portal>
        <RPopover.Content
          className="mfs-slip-panel"
          sideOffset={8}
          align="end"
          collisionPadding={12}
        >
          {rack}
        </RPopover.Content>
      </RPopover.Portal>
    </RPopover.Root>
  );
}

export default FilterBar;
