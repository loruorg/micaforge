/**
 * Funnels: read as a descending record rather than a graphic.
 *
 * A funnel here is a run of ruled bands, one per step, each drawn against the
 * extent the step before it reached. What survived is ink; what was lost is the
 * hatched remainder of the track, and the size of that loss is written in the
 * margin on the same right-hand axis every other figure in the panel shares.
 * There is no tapering trapezoid and no bar chart: the instrument records what
 * each step held, and the debt between two steps is the interval between two
 * pens, exactly as it is everywhere else in this product.
 *
 * Every band is a real control. Choosing one reads back the sessions that got
 * that far, so the drop is never just a number you have to take on faith.
 */
import { useEffect, useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { useAppState } from "../state";
import { useSite } from "../providers";
import {
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  type Column,
} from "../../components/react/ui";
import { IconOut, IconPlus } from "../../components/react/icons";
import { ago, duration, num, pct, shortPath } from "../../lib/format";
import { FunnelEditor, type Funnel } from "./FunnelEditor";
import { Select } from "./GoalEditor";
import type { Goal } from "./GoalEditor";
import { errorText } from "./EventsScreen";

interface FunnelStepStat {
  index: number;
  label: string;
  /** The rule, written out by the server, e.g. "path starts with /docs". */
  detail: string;
  visitors: number;
  sessions: number;
  /** Fraction of the previous step that did not arrive. */
  drop_off: number;
  drop_count: number;
  avg_seconds_from_previous: number | null;
}

interface FunnelPayload {
  id: string;
  name: string;
  window_hours: number;
  entered: number;
  completed: number;
  conversion_rate: number;
  steps: FunnelStepStat[];
}

interface StepSession {
  session_id: string;
  started: number;
  reached_at: number;
  entry_path: string;
  country: string;
  device_type: string;
  duration_s: number;
  pageviews: number;
}

interface StepSessionsPayload {
  step: number;
  total: number;
  rows: StepSession[];
}

type Editing = { mode: "new" } | { mode: "edit"; funnel: Funnel } | null;

export function FunnelsScreen() {
  const { siteId } = useSite();
  const { site } = useParams();
  const { range, audience, filters } = useAppState();
  const qc = useQueryClient();
  const filtersJson = JSON.stringify(filters ?? []);

  const [selected, setSelected] = useState<string | null>(null);
  const [step, setStep] = useState<number | null>(null);
  const [editing, setEditing] = useState<Editing>(null);

  const funnels = useQuery({
    queryKey: ["funnels", siteId],
    queryFn: () => api.get<Funnel[]>("/api/funnels", { site: siteId as number }),
    enabled: siteId != null,
  });

  /* Goals are offered as funnel steps. Failing to read them is not fatal. */
  const goals = useQuery({
    queryKey: ["goals", siteId],
    queryFn: () => api.get<Goal[]>("/api/goals", { site: siteId as number }),
    enabled: siteId != null,
  });

  const list = funnels.data?.data ?? [];
  const current = list.find((f) => f.id === selected) ?? list[0] ?? null;

  // Keep the selection honest when the list changes underneath it.
  useEffect(() => {
    if (current && selected !== current.id) setSelected(current.id);
    if (!current && selected) setSelected(null);
  }, [current, selected]);

  useEffect(() => {
    setStep(null);
  }, [current?.id, range, audience, filtersJson]);

  const stats = useQuery({
    queryKey: ["stats", "funnel", siteId, range, audience, filtersJson, current?.id],
    queryFn: () =>
      api.get<FunnelPayload>(`/api/stats/funnel/${current!.id}`, {
        site: siteId as number,
        range,
        audience,
        filters,
      }),
    enabled: siteId != null && current != null,
    placeholderData: keepPreviousData,
  });

  const payload = stats.data?.data;
  const steps = payload?.steps ?? [];

  const worst = useMemo(() => {
    let out: FunnelStepStat | null = null;
    for (const s of steps) if (s.index > 0 && (!out || s.drop_off > out.drop_off)) out = s;
    return out;
  }, [steps]);

  if (editing) {
    return (
      <div className="mfb-stack">
        <FunnelEditor
          funnel={editing.mode === "edit" ? editing.funnel : undefined}
          goals={goals.data?.data ?? []}
          goalsUnavailable={goals.isError}
          onDone={(saved) => {
            setSelected(saved.id);
            setEditing(null);
            void qc.invalidateQueries({ queryKey: ["stats", "funnel", siteId] });
          }}
          onCancel={() => setEditing(null)}
        />
      </div>
    );
  }

  return (
    <div className="mfb-stack">
      <Panel ruled feed>
        <PanelHead
          title="Funnels"
          note={
            current
              ? `${current.steps.length} steps, completed within ${windowWords(current.window_hours)}.`
              : "An ordered run of steps, and what was lost between them."
          }
          actions={
            <>
              {list.length > 1 ? (
                <span className="mfb-picker">
                  <Select
                    id="mfb-funnel-pick"
                    ariaLabel="Funnel"
                    value={current?.id ?? ""}
                    onChange={setSelected}
                    options={list.map((f) => ({ value: f.id, label: f.name }))}
                  />
                </span>
              ) : null}
              {current ? (
                <Button onClick={() => setEditing({ mode: "edit", funnel: current })}>
                  Edit steps
                </Button>
              ) : null}
              <Button
                tone="primary"
                icon={<IconPlus size={13} />}
                onClick={() => setEditing({ mode: "new" })}
              >
                New funnel
              </Button>
            </>
          }
        />

        {funnels.isError ? (
          <ErrorState
            title="The funnels could not be read"
            detail={errorText(funnels.error)}
            onRetry={() => void funnels.refetch()}
          />
        ) : funnels.isPending ? (
          <p className="mfb-note" aria-live="polite">
            Reading the funnels…
          </p>
        ) : list.length === 0 ? (
          <Empty
            title="No funnels built"
            body="A funnel is an ordered run of steps (a page, an event, a goal) and the record of how many sessions made it from one to the next. Build one and the drops appear here."
            action={
              <Button tone="primary" onClick={() => setEditing({ mode: "new" })}>
                Build the first funnel
              </Button>
            }
          />
        ) : (
          <>
            <Readout
              channels={[
                { key: "entered", label: "Entered", value: num(payload?.entered) },
                { key: "completed", label: "Completed", value: num(payload?.completed) },
                {
                  key: "rate",
                  label: "Completion rate",
                  value: payload ? pct(payload.conversion_rate, 1) : "—",
                },
                {
                  key: "worst",
                  label: "Steepest drop",
                  value: worst ? pct(worst.drop_off, 1) : "—",
                  note: worst ? `into ${worst.label}` : undefined,
                },
              ]}
            />

            {stats.isError ? (
              <ErrorState
                title="That funnel did not come back"
                detail={errorText(stats.error)}
                onRetry={() => void stats.refetch()}
              />
            ) : stats.isPending ? (
              <p className="mfb-note" aria-live="polite">
                Reading {current?.name ?? "the funnel"}…
              </p>
            ) : steps.length === 0 ? (
              <Empty
                title="This funnel has no steps yet"
                body="Add at least two steps and the record between them is drawn here."
                action={
                  current ? (
                    <Button onClick={() => setEditing({ mode: "edit", funnel: current })}>
                      Add steps
                    </Button>
                  ) : undefined
                }
              />
            ) : payload && payload.entered === 0 ? (
              <Empty
                title="No session entered this funnel"
                body="Nothing matched the first step over this interval. Widen the interval, loosen the first step, or check the path it is matching."
              />
            ) : (
              <FunnelRecord
                steps={steps}
                selected={step}
                onSelect={(i) => setStep((v) => (v === i ? null : i))}
              />
            )}
          </>
        )}
      </Panel>

      {current && step != null ? (
        <StepSessions
          funnelId={current.id}
          step={step}
          stepLabel={steps.find((s) => s.index === step)?.label ?? `Step ${step + 1}`}
          siteSlug={site}
          onClose={() => setStep(null)}
        />
      ) : null}
    </div>
  );
}

/* ── the record ──────────────────────────────────────────────────────────── */

function FunnelRecord({
  steps,
  selected,
  onSelect,
}: {
  steps: FunnelStepStat[];
  selected: number | null;
  onSelect: (i: number) => void;
}) {
  const top = Math.max(1, steps[0]?.sessions ?? 1);
  const last = steps.length - 1;

  return (
    <div className="mfb-funnel">
      {steps.map((s, i) => {
        const held = s.sessions / top;
        const prevHeld = i === 0 ? 1 : (steps[i - 1].sessions ?? 0) / top;
        // The ramp is spread across however many steps there are, so the run
        // reads as one descending scale rather than five colours repeating.
        // It starts at the second step of the ramp: the first is bright enough
        // at full width to pull the eye off the figures that carry the reading.
        // The far end of the ramp sits close to the track's own tone on paper,
        // so the band is given a drawn stop at its leading edge (see
        // behaviour.css) and never has to be found by tone alone.
        const ramp = last === 0 ? 2 : 2 + Math.round((i / last) * 3);

        return (
          <div key={s.index}>
            {i > 0 ? (
              <div className="mfb-fdrop">
                <span className="mfb-fdrop__leader" aria-hidden="true" />
                <span
                  className="mfb-fdrop__note"
                  data-tone={s.drop_count > 0 ? "loss" : "none"}
                >
                  {s.drop_count > 0
                    ? `${num(s.drop_count)} left · ${pct(s.drop_off, 1)}`
                    : "none lost"}
                </span>
              </div>
            ) : null}

            <button
              type="button"
              className="mfb-fstep"
              aria-pressed={selected === s.index}
              onClick={() => onSelect(s.index)}
            >
              <span className="mfb-fstep__lead">
                <span className="mfb-fstep__name">
                  {i + 1}. {s.label}
                </span>
                <span className="mfb-fstep__rule">
                  {s.detail}
                  {s.avg_seconds_from_previous != null && i > 0
                    ? ` · ${duration(s.avg_seconds_from_previous)} after the step before`
                    : ""}
                </span>
                <span className="mfb-fstep__track">
                  <span
                    className="mfb-fstep__band"
                    data-ramp={ramp}
                    style={{ width: `${Math.max(0, Math.min(1, held)) * 100}%` }}
                    aria-hidden="true"
                  />
                  {i > 0 && prevHeld > held ? (
                    <span
                      className="mfb-fstep__lost"
                      style={{
                        left: `${held * 100}%`,
                        width: `${(prevHeld - held) * 100}%`,
                      }}
                      aria-hidden="true"
                    />
                  ) : null}
                </span>
              </span>

              <span className="mfb-fstep__figs">
                <span className="mfb-fstep__count">{num(s.sessions)}</span>
                <span className="mfb-fstep__share">
                  {pct(held, held < 0.1 ? 1 : 0)} of entrants
                </span>
                <span className="mfb-fstep__share">{num(s.visitors)} visitors</span>
              </span>
            </button>
          </div>
        );
      })}

      <p className="mfb-note">
        Choose a step to read the sessions that reached it.
      </p>
    </div>
  );
}

/* ── the sessions behind one step ────────────────────────────────────────── */

function StepSessions({
  funnelId,
  step,
  stepLabel,
  siteSlug,
  onClose,
}: {
  funnelId: string;
  step: number;
  stepLabel: string;
  siteSlug: string | undefined;
  onClose: () => void;
}) {
  const { siteId } = useSite();
  const { range, audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);

  const q = useQuery({
    queryKey: ["stats", "funnel", siteId, range, audience, filtersJson, funnelId, "step", step],
    queryFn: () =>
      api.get<StepSessionsPayload>(`/api/stats/funnel/${funnelId}`, {
        site: siteId as number,
        range,
        audience,
        filters,
        step,
        sessions: 1,
        limit: 50,
      }),
    enabled: siteId != null,
  });

  const rows = q.data?.data.rows ?? [];
  const total = q.data?.data.total;

  const columns: Column<StepSession>[] = [
    {
      key: "when",
      head: "Reached",
      render: (r) => <span className="mf-data">{ago(r.reached_at)}</span>,
    },
    {
      key: "entry",
      head: "Entered on",
      render: (r) => (
        <span className="mf-data" title={r.entry_path}>
          {shortPath(r.entry_path, 34)}
        </span>
      ),
    },
    {
      key: "where",
      head: "From",
      render: (r) => `${r.country || "—"} · ${r.device_type || "—"}`,
    },
    {
      key: "views",
      head: "Pages",
      numeric: true,
      render: (r) => <span className="mf-data">{num(r.pageviews)}</span>,
    },
    {
      key: "dur",
      head: "Length",
      numeric: true,
      render: (r) => <span className="mf-data">{duration(r.duration_s)}</span>,
    },
    {
      key: "open",
      head: <span className="mf-sr">Open the session</span>,
      width: "3rem",
      numeric: true,
      render: (r) =>
        siteSlug ? (
          <Link
            to={`/app/${siteSlug}/sessions/${r.session_id}`}
            aria-label={`Open session ${r.session_id}`}
          >
            <IconOut size={14} />
          </Link>
        ) : null,
    },
  ];

  return (
    <Panel>
      <PanelHead
        title={`Sessions that reached ${stepLabel}`}
        note={
          total != null
            ? `${num(total)} in total; the most recent ${Math.min(rows.length, 50)} are listed.`
            : undefined
        }
        actions={
          <Button tone="quiet" onClick={onClose}>
            Close
          </Button>
        }
      />

      {q.isError ? (
        <ErrorState detail={errorText(q.error)} onRetry={() => void q.refetch()} />
      ) : (
        <Sheet
          rows={rows}
          columns={columns}
          loading={q.isPending}
          getKey={(r) => r.session_id}
          empty={
            <Empty
              title="No sessions reached this step"
              body="Nothing in this interval got this far. Widen the interval or check the step's rule."
            />
          }
        />
      )}
    </Panel>
  );
}

const windowWords = (h: number): string =>
  h < 24 ? `${h} hour${h === 1 ? "" : "s"}` : `${Math.round(h / 24)} day${h < 48 ? "" : "s"}`;

export default FunnelsScreen;
