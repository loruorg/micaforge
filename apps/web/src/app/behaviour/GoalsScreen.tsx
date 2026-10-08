/**
 * Goals: what the record is measured against.
 *
 * The list is built from the goals that exist, not from the goals that happened
 * to convert, so a goal that has never fired still appears with a zero against
 * it. That is the honest reading: a goal recording nothing is exactly the fact
 * an operator needs to see.
 *
 * Each row carries its own trend written as a trace, drawn the way every other
 * line in this product is drawn: a ridge of displaced soot under a bright
 * scratch. Opening a row states the rule in plain words and offers the two
 * actions that change it.
 */
import { useMemo, useState } from "react";
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, api } from "../../lib/api";
import { useAppState } from "../state";
import { useSite } from "../providers";
import { Button, Empty, ErrorState, Panel, PanelHead, Readout } from "../../components/react/ui";
import { IconChevron, IconPlus } from "../../components/react/icons";
import { num, pct } from "../../lib/format";
import { GoalEditor, type Goal, type GoalKind, type MatchOp } from "./GoalEditor";
import { errorText } from "./EventsScreen";

interface GoalStat {
  id: string;
  conversions: number;
  visitors: number;
  /** Conversions over sessions, as a fraction. */
  rate: number;
  revenue: number | null;
  currency: string | null;
  /** One value per bucket in the current interval. */
  trend: number[];
}

interface GoalsPayload {
  goals: GoalStat[];
  conversions: number;
  conversion_rate: number;
  revenue: number | null;
  currency: string | null;
  sessions: number;
}

type Editing = { mode: "new" } | { mode: "edit"; goal: Goal } | null;

export function GoalsScreen() {
  const { siteId } = useSite();
  const { range, audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);
  const [editing, setEditing] = useState<Editing>(null);
  const [open, setOpen] = useState<string | null>(null);

  /* The goals themselves. This query is the spine of the screen. */
  const goals = useQuery({
    queryKey: ["goals", siteId],
    queryFn: () => api.get<Goal[]>("/api/goals", { site: siteId as number }),
    enabled: siteId != null,
  });

  /* What they did over the caliper's interval. */
  const stats = useQuery({
    queryKey: ["stats", "goals", siteId, range, audience, filtersJson],
    queryFn: () =>
      api.get<GoalsPayload>("/api/stats/goals", {
        site: siteId as number,
        range,
        audience,
        filters,
      }),
    enabled: siteId != null,
    placeholderData: keepPreviousData,
  });

  const totals = stats.data?.data;
  const byId = useMemo(() => {
    const m = new Map<string, GoalStat>();
    for (const g of stats.data?.data.goals ?? []) m.set(g.id, g);
    return m;
  }, [stats.data]);

  const rows = useMemo(() => {
    const defs = goals.data?.data ?? [];
    return [...defs].sort(
      (a, b) => (byId.get(b.id)?.conversions ?? 0) - (byId.get(a.id)?.conversions ?? 0),
    );
  }, [goals.data, byId]);

  if (editing) {
    return (
      <div className="mfb-stack">
        <GoalEditor
          goal={editing.mode === "edit" ? editing.goal : undefined}
          onDone={() => setEditing(null)}
          onCancel={() => setEditing(null)}
        />
      </div>
    );
  }

  return (
    <div className="mfb-stack">
      <Panel ruled feed>
        <PanelHead
          title="Goals"
          note="Conversions counted per session over the interval you have set."
          actions={
            <Button
              tone="primary"
              icon={<IconPlus size={13} />}
              onClick={() => setEditing({ mode: "new" })}
            >
              New goal
            </Button>
          }
        />

        <Readout
          channels={[
            { key: "conv", label: "Conversions", value: num(totals?.conversions) },
            {
              key: "rate",
              label: "Conversion rate",
              value: totals ? pct(totals.conversion_rate, 1) : "—",
              note: totals ? `of ${num(totals.sessions)} sessions` : undefined,
            },
            {
              key: "rev",
              label: "Revenue",
              value: money(totals?.revenue, totals?.currency),
            },
            {
              key: "count",
              label: "Goals recording",
              /* Once the list has come back, none is 0. An em dash here means
                 the list has not been read, and only that. */
              value: goals.isSuccess ? num(rows.length) : "—",
            },
          ]}
        />

        {goals.isError ? (
          <ErrorState
            title="The goals could not be read"
            detail={errorText(goals.error)}
            onRetry={() => void goals.refetch()}
          />
        ) : goals.isPending ? (
          <p className="mfb-note" aria-live="polite">
            Reading the goals…
          </p>
        ) : rows.length === 0 ? (
          <Empty
            title="No goals set"
            body="A goal names what counts as a conversion: a page reached, an event fired, a file taken, an amount booked. Set one and this list fills the moment it first fires."
            action={
              <Button tone="primary" onClick={() => setEditing({ mode: "new" })}>
                Set the first goal
              </Button>
            }
          />
        ) : (
          <>
            {stats.isError ? (
              <p className="mfb-note" role="alert">
                The goals are listed, but their figures did not come back:{" "}
                {errorText(stats.error)}{" "}
                <button type="button" className="mf-drum__clear" onClick={() => void stats.refetch()}>
                  Read the figures again
                </button>
              </p>
            ) : null}

            <div className="mfb-reg mfb-reg--goals">
              <div className="mfb-reg__head">
                <span aria-hidden="true" />
                <span className="mfb-reg__h">Goal</span>
                <span className="mfb-reg__h--num mfb-col-2">Trend</span>
                <span className="mfb-reg__h--num">Conversions</span>
                <span className="mfb-reg__h--num mfb-col-3">Rate</span>
                <span className="mfb-reg__h--num mfb-col-2">Revenue</span>
              </div>

              {rows.map((g) => (
                <GoalEntry
                  key={g.id}
                  goal={g}
                  stat={byId.get(g.id)}
                  pending={stats.isPending}
                  open={open === g.id}
                  onToggle={() => setOpen((v) => (v === g.id ? null : g.id))}
                  onEdit={() => setEditing({ mode: "edit", goal: g })}
                />
              ))}
            </div>
          </>
        )}
      </Panel>
    </div>
  );
}

/* ── one goal ────────────────────────────────────────────────────────────── */

function GoalEntry({
  goal,
  stat,
  pending,
  open,
  onToggle,
  onEdit,
}: {
  goal: Goal;
  stat: GoalStat | undefined;
  pending: boolean;
  open: boolean;
  onToggle: () => void;
  onEdit: () => void;
}) {
  const bodyId = `mfb-goal-${goal.id}`;
  return (
    <>
      <button
        type="button"
        className="mfb-reg__row"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={onToggle}
      >
        <IconChevron size={14} className="mfb-reg__caret" />
        <span className="mfb-reg__name">
          <span className="mfb-reg__title">{goal.name}</span>
          <span className="mfb-reg__sub">{describe(goal)}</span>
        </span>
        <span className="mfb-reg__fig mfb-col-2">
          <TrendTrace values={stat?.trend ?? []} label={`Trend for ${goal.name}`} />
        </span>
        <span className="mfb-reg__fig">{pending ? "…" : num(stat?.conversions ?? 0)}</span>
        <span className="mfb-reg__fig mfb-reg__fig--dim mfb-col-3">
          {pending ? "…" : stat ? pct(stat.rate, 1) : "0%"}
        </span>
        <span className="mfb-reg__fig mfb-reg__fig--dim mfb-col-2">
          {pending ? "…" : money(stat?.revenue, stat?.currency)}
        </span>
      </button>

      {open ? (
        <div className="mfb-reg__body" id={bodyId}>
          <p className="mfb-field__label mfb-caption">
            The rule
          </p>
          <p className="mfb-prop__key mfb-rule-text">
            {describe(goal, true)}
          </p>
          <GoalActions goal={goal} onEdit={onEdit} />
        </div>
      ) : null}
    </>
  );
}

/** Edit, and a delete that asks in place rather than through a browser prompt. */
function GoalActions({ goal, onEdit }: { goal: Goal; onEdit: () => void }) {
  const { siteId } = useSite();
  const qc = useQueryClient();
  const [confirming, setConfirming] = useState(false);

  const remove = useMutation({
    mutationFn: () => api.del<void>(`/api/goals/${goal.id}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["goals", siteId] });
      void qc.invalidateQueries({ queryKey: ["stats", "goals", siteId] });
    },
  });

  return (
    <div className="mfb-tools" role="group" aria-label={`Actions for ${goal.name}`}>
      <Button onClick={onEdit}>Edit this goal</Button>

      {confirming ? (
        <>
          <Button
            tone="danger"
            disabled={remove.isPending}
            onClick={() => remove.mutate()}
          >
            {remove.isPending ? "Deleting…" : "Delete permanently"}
          </Button>
          <Button tone="quiet" onClick={() => setConfirming(false)}>
            Keep it
          </Button>
          <span className="mfb-field__hint">
            The goal stops being counted. Events already recorded are untouched.
          </span>
        </>
      ) : (
        <Button tone="danger" onClick={() => setConfirming(true)}>
          Delete
        </Button>
      )}

      {remove.isError ? (
        <p className="mfb-field__err" role="alert">
          {remove.error instanceof ApiError
            ? remove.error.message
            : "The goal was not deleted."}
        </p>
      ) : null}
    </div>
  );
}

/* ── the trend trace ─────────────────────────────────────────────────────── */

/**
 * The same two-pass scratch the drum uses, at register scale: a ridge of
 * displaced ground under a bright line. Drawn from the values that are already
 * in the DOM, so it is never blank waiting for anything.
 */
function TrendTrace({ values, label }: { values: number[]; label: string }) {
  if (values.length < 2) {
    return (
      <span className="mfb-field__hint" aria-label={`${label}: too short to draw`}>
        —
      </span>
    );
  }

  const max = Math.max(...values, 1);
  const step = 100 / (values.length - 1);
  const d = values
    .map((v, i) => `${i === 0 ? "M" : "L"}${(i * step).toFixed(2)},${(22 - (v / max) * 18).toFixed(2)}`)
    .join("");

  return (
    <svg
      className="mfb-trace"
      viewBox="0 0 100 26"
      preserveAspectRatio="none"
      role="img"
      aria-label={`${label}: ${values.length} intervals, peak ${num(max)}`}
    >
      <path
        className="mfb-trace__ridge"
        d={d}
        fill="none"
        strokeWidth={1.6}
        vectorEffect="non-scaling-stroke"
        transform="translate(0,1.2)"
      />
      <path
        d={d}
        fill="none"
        stroke="var(--mf-people)"
        strokeWidth={1.4}
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}

/* ── words for a rule ────────────────────────────────────────────────────── */

const OP_WORDS: Record<MatchOp, string> = {
  is: "is",
  is_not: "is not",
  contains: "contains",
  starts_with: "starts with",
  ends_with: "ends with",
  matches: "matches",
};

const KIND_WORDS: Record<GoalKind, string> = {
  path: "Page",
  event: "Event",
  outbound: "Outbound link",
  file: "Download",
  revenue: "Revenue event",
};

/** The condition, written out. `full` adds the property conditions. */
function describe(goal: Goal, full = false): string {
  const c = goal.conditions ?? {};
  const kind = KIND_WORDS[goal.kind] ?? goal.kind;

  if (goal.kind === "event") {
    const base = `${kind} named ${c.event_name || "(unset)"}`;
    if (!full || !c.props?.length) return base;
    return `${base}, where ${c.props
      .map((p) => `${p.key} ${OP_WORDS[p.op] ?? p.op} ${p.value}`)
      .join(" and ")}`;
  }

  if (goal.kind === "revenue") {
    const base = `${kind} ${c.event_name || "(unset)"} in ${c.currency || "—"}`;
    return c.min_value != null ? `${base}, at least ${c.min_value}` : base;
  }

  const base = `${kind} ${OP_WORDS[c.op ?? "is"] ?? c.op} ${c.value || "(unset)"}`;
  if (!full) return base;
  return goal.value_cents != null
    ? `${base}. Each conversion is worth ${(goal.value_cents / 100).toFixed(2)}.`
    : base;
}

/** A money figure, or an em dash where there is genuinely no amount. */
function money(v: number | null | undefined, currency: string | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  if (!currency) return num(v);
  try {
    return new Intl.NumberFormat(undefined, {
      style: "currency",
      currency,
      maximumFractionDigits: v >= 1000 ? 0 : 2,
    }).format(v);
  } catch {
    return `${num(v)} ${currency}`;
  }
}

export default GoalsScreen;
