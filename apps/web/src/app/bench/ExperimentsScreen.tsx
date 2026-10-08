/**
 * EXPERIMENTS: the index and the composer.
 *
 * An experiment is three commitments made before any data arrives: what you
 * expect to happen, how traffic is split, and which single goal decides it. This
 * screen asks for all three up front and refuses to pretend it can report
 * without the third: a running experiment with no goal attached says so on its
 * own row rather than showing an encouraging number.
 *
 * Opening one brings its results up underneath, which is where the arithmetic
 * lives (`ExperimentResults`).
 */
import { useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { Button, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconClose, IconPlus, IconSettings } from "../../components/react/icons";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  Field,
  KeyBank,
  NumberInput,
  Reading,
  Select,
  TextArea,
  TextInput,
  errText,
} from "./QueryScreen";
import {
  ExperimentResults,
  blankExperiment,
  controlOf,
  experimentsKey,
  goalsKey,
  normaliseExperiment,
} from "./ExperimentResults";
import type { Experiment, ExperimentStatus, Goal } from "./ExperimentResults";

const KEY_RE = /^[a-z0-9][a-z0-9._-]*$/;

const STATUSES: { v: ExperimentStatus; label: string }[] = [
  { v: "draft", label: "Draft" },
  { v: "running", label: "Running" },
  { v: "stopped", label: "Stopped" },
];

function problemsOf(e: Experiment): Record<string, string> {
  const out: Record<string, string> = {};
  if (!e.key.trim()) out.key = "An experiment needs a key. It is what your code sends with each event.";
  else if (!KEY_RE.test(e.key.trim()))
    out.key = "Use lower-case letters, numbers, dot, dash or underscore, starting with a letter or number.";
  if (e.variants.length < 2) out.variants = "An experiment needs at least two variants to compare.";
  else {
    const keys = e.variants.map((v) => v.key.trim());
    if (keys.some((k) => !k)) out.variants = "Every variant needs a key.";
    else if (new Set(keys).size !== keys.length) out.variants = "Two variants share a key. Each must be unique.";
    else if (!e.variants.some((v) => v.control)) out.variants = "Mark one variant as the baseline.";
    else if (e.variants.reduce((s, v) => s + Math.max(0, v.weight), 0) <= 0)
      out.variants = "At least one variant needs a weight above zero.";
  }
  if (e.status !== "draft" && !e.goal_id)
    out.goal = "A running experiment needs the goal it is meant to move, or it can never report.";
  return out;
}

/** A plain sentence about where the experiment is, never a coloured chip. */
function statusLine(e: Experiment): string {
  if (e.status === "running")
    return e.started_at ? `Running since ${new Date(e.started_at).toLocaleDateString()}` : "Running";
  if (e.status === "stopped")
    return e.ended_at ? `Stopped ${ago(e.ended_at)}` : "Stopped";
  return "Not started";
}

/* ═══ the composer ════════════════════════════════════════════════════════ */

function Composer({
  value,
  goals,
  siteId,
  onDone,
}: {
  value: Experiment;
  goals: Goal[];
  siteId: number | undefined;
  onDone: (r: "saved" | "deleted" | "cancelled") => void;
}) {
  const qc = useQueryClient();
  const [e, setE] = useState<Experiment>(value);
  const [show, setShow] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const set = (p: Partial<Experiment>) => setE((prev) => ({ ...prev, ...p }));
  const problems = problemsOf(e);
  const bad = Object.keys(problems).length > 0;
  const isNew = !e.id;
  const err = (k: string) => (show ? problems[k] : undefined);

  const save = useMutation({
    mutationFn: () => {
      const body = {
        site_id: siteId,
        key: e.key.trim(),
        hypothesis: e.hypothesis.trim(),
        variants: e.variants.map((v) => ({ key: v.key.trim(), weight: v.weight, control: !!v.control })),
        goal_id: e.goal_id,
        status: e.status,
        started_at: e.started_at,
        ended_at: e.ended_at,
      };
      return isNew
        ? api.post<Experiment>("/api/experiments", body)
        : api.patch<Experiment>(`/api/experiments/${e.id}`, body);
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: experimentsKey(siteId) });
      onDone("saved");
    },
  });

  const remove = useMutation({
    mutationFn: () => api.del<void>(`/api/experiments/${e.id}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: experimentsKey(siteId) });
      onDone("deleted");
    },
  });

  /** Moving to running stamps the start; stopping stamps the end. */
  const setStatus = (next: ExperimentStatus) => {
    const now = new Date().toISOString();
    if (next === "running") set({ status: next, started_at: e.started_at ?? now, ended_at: null });
    else if (next === "stopped") set({ status: next, ended_at: e.ended_at ?? now });
    else set({ status: next });
  };

  return (
    <Panel className="mf-bn-editor">
      <PanelHead
        title={isNew ? "New experiment" : `Edit ${e.key || "experiment"}`}
        note="Decide the split and the goal before any traffic arrives. Changing them while it runs makes the results unreliable."
        actions={
          <Button tone="quiet" onClick={() => onDone("cancelled")}>
            Close
          </Button>
        }
      />

      <div className="mf-bn-editor__body">
        <div className="mf-bn-build__row">
          <Field
            label="Key"
            error={err("key")}
            hint="Sent on every event as `experiment`."
            control={(id) => (
              <TextInput
                id={id}
                mono
                value={e.key}
                invalid={!!err("key")}
                placeholder="checkout-copy"
                onChange={(v) => set({ key: v })}
              />
            )}
          />
          <Field
            label="Goal being measured"
            error={err("goal")}
            hint={
              goals.length
                ? "One goal decides the experiment. Two goals means two experiments."
                : "No goals exist on this site yet."
            }
            control={(id) =>
              goals.length ? (
                <Select
                  id={id}
                  value={e.goal_id ?? ""}
                  onChange={(v) => set({ goal_id: v || null })}
                >
                  <option value="">Choose a goal…</option>
                  {goals.map((g) => (
                    <option key={g.id} value={g.id}>
                      {g.name}
                    </option>
                  ))}
                </Select>
              ) : (
                <p className="mf-bn-field__hint">
                  Define one first, then come back and attach it.
                </p>
              )
            }
          />
          <Field
            label="State"
            control={(id) => (
              <KeyBank<ExperimentStatus>
                id={id}
                label="State"
                value={e.status}
                onChange={setStatus}
                options={STATUSES}
              />
            )}
          />
        </div>

        <div className="mf-bn-build__row">
          <Field
            label="Hypothesis"
            wide
            hint="One sentence, written before the data: what you expect to change, and roughly by how much."
            control={(id) => (
              <TextArea
                id={id}
                rows={2}
                value={e.hypothesis}
                placeholder="Naming the plan on the button raises checkout completion by about two points."
                onChange={(v) => set({ hypothesis: v })}
              />
            )}
          />
        </div>

        <section className="mf-bn-editor__variants" aria-label="Variants">
          <h3 className="mf-bn-editor__h">Variants</h3>
          <p className="mf-bn-editor__hint">
            One is the baseline every other is measured against. Weights are relative.
          </p>
          <ul className="mf-bn-vars">
            {e.variants.map((v, i) => (
              <li key={i} className="mf-bn-vars__row mf-bn-vars__row--exp">
                <label className="mf-bn-vars__base">
                  <input
                    type="radio"
                    name="mf-exp-control"
                    checked={!!v.control}
                    onChange={() =>
                      set({ variants: e.variants.map((x, k) => ({ ...x, control: k === i })) })
                    }
                  />
                  <span>baseline</span>
                </label>
                <TextInput
                  value={v.key}
                  mono
                  ariaLabel={`Variant ${i + 1} key`}
                  onChange={(nv) =>
                    set({ variants: e.variants.map((x, k) => (k === i ? { ...x, key: nv } : x)) })
                  }
                />
                <NumberInput
                  value={v.weight}
                  min={0}
                  max={1000}
                  ariaLabel={`Variant ${i + 1} weight`}
                  onChange={(nv) =>
                    set({
                      variants: e.variants.map((x, k) =>
                        k === i ? { ...x, weight: Math.max(0, Math.round(nv)) } : x,
                      ),
                    })
                  }
                />
                <Button
                  size="sm"
                  tone="quiet"
                  icon={<IconClose size={13} />}
                  aria-label={`Remove variant ${v.key || i + 1}`}
                  disabled={e.variants.length <= 2}
                  onClick={() => set({ variants: e.variants.filter((_, k) => k !== i) })}
                />
              </li>
            ))}
          </ul>
          {err("variants") ? (
            <p className="mf-bn-field__err" role="alert">
              {problems.variants}
            </p>
          ) : null}
          <Button
            size="sm"
            icon={<IconPlus size={13} />}
            onClick={() =>
              set({ variants: [...e.variants, { key: `variant-${e.variants.length + 1}`, weight: 50 }] })
            }
          >
            Add a variant
          </Button>
        </section>
      </div>

      <div className="mf-bn-editor__foot">
        <Button
          tone="primary"
          disabled={save.isPending}
          onClick={() => {
            setShow(true);
            if (!bad) save.mutate();
          }}
        >
          {save.isPending ? "Saving…" : isNew ? "Create the experiment" : "Save the experiment"}
        </Button>
        <Button tone="quiet" onClick={() => onDone("cancelled")}>
          Cancel
        </Button>

        {!isNew ? (
          <span className="mf-bn-editor__danger">
            {confirmDelete ? (
              <>
                <span className="mf-bn-editor__ask">
                  Delete this experiment? The events it produced stay in your data.
                </span>
                <Button tone="danger" size="sm" disabled={remove.isPending} onClick={() => remove.mutate()}>
                  Delete
                </Button>
                <Button size="sm" tone="quiet" onClick={() => setConfirmDelete(false)}>
                  Keep
                </Button>
              </>
            ) : (
              <Button tone="danger" size="sm" onClick={() => setConfirmDelete(true)}>
                Delete this experiment
              </Button>
            )}
          </span>
        ) : null}

        {show && bad ? (
          <p className="mf-bn-field__err" role="alert">
            {Object.values(problems)[0]}
          </p>
        ) : null}
        {save.isError ? (
          <p className="mf-bn-field__err" role="alert">
            The experiment was not saved. {errText(save.error)}
          </p>
        ) : null}
        {remove.isError ? (
          <p className="mf-bn-field__err" role="alert">
            The experiment was not deleted. {errText(remove.error)}
          </p>
        ) : null}
      </div>
    </Panel>
  );
}

/* ═══ the index ═══════════════════════════════════════════════════════════ */

export function ExperimentsScreen() {
  const { site } = useParams();
  const { siteId, isLoading: siteLoading } = useSite();
  const [view, setView] = useState<{ mode: "results" | "edit"; exp: Experiment } | null>(null);

  const q = useQuery({
    queryKey: experimentsKey(siteId),
    enabled: siteId != null,
    queryFn: async () => {
      const res = await api.get<Experiment[]>("/api/experiments", { site: siteId as number });
      return (res.data ?? []).map((x) => normaliseExperiment(x as unknown as Record<string, unknown>));
    },
  });

  const goalsQ = useQuery({
    queryKey: goalsKey(siteId),
    enabled: siteId != null,
    queryFn: async () => {
      const res = await api.get<Goal[]>("/api/goals", { site: siteId as number });
      return res.data ?? [];
    },
  });

  const goals = goalsQ.data ?? [];
  const goalById = useMemo(() => new Map(goals.map((g) => [g.id, g])), [goals]);
  const rows = q.data ?? [];

  return (
    <div className="mf-bn-page">
      <Panel>
        <PanelHead
          title="Experiments"
          note="Split traffic across variants and measure one goal. Every result shows its confidence interval and sample size, and stays inconclusive until the data supports a difference."
          actions={
            <Button
              tone="primary"
              icon={<IconPlus size={13} />}
              onClick={() => setView({ mode: "edit", exp: blankExperiment(siteId) })}
            >
              New experiment
            </Button>
          }
        />

        <div className="mf-bn-indexwrap">
          <Reading
            loading={siteLoading || q.isLoading}
            error={q.isError ? q.error : undefined}
            onRetry={() => void q.refetch()}
            isEmpty={rows.length === 0}
            lines={3}
            empty={
              <Empty
                title="No experiments yet"
                body="An experiment needs three things decided in advance: what you expect to happen, how traffic splits, and the one goal that settles it. Write those down, and the results screen does the statistics."
                action={
                  <Button
                    tone="primary"
                    icon={<IconPlus size={13} />}
                    onClick={() => setView({ mode: "edit", exp: blankExperiment(siteId) })}
                  >
                    New experiment
                  </Button>
                }
              />
            }
          >
            <ul className="mf-bn-exps">
              {rows.map((e) => {
                const goal = e.goal_id ? goalById.get(e.goal_id) : undefined;
                const control = controlOf(e);
                const isOpen = view?.exp.id === e.id;
                return (
                  <li key={e.id} className={`mf-bn-exps__row${isOpen ? " is-open" : ""}`}>
                    <div className="mf-bn-exps__ident">
                      <span className="mf-bn-exps__key mf-data">{e.key}</span>
                      <p className="mf-bn-exps__hyp">
                        {e.hypothesis || "No hypothesis written down."}
                      </p>
                    </div>

                    <div className="mf-bn-exps__state">
                      <p className="mf-bn-exps__status">
                        {e.status === "running" ? (
                          <span className="mf-bn-exps__live" aria-hidden="true" />
                        ) : null}
                        {statusLine(e)}
                      </p>
                      <p className="mf-bn-exps__meta">
                        {e.variants.map((v) => v.key).join(" · ") || "no variants"}
                        {control ? `, baseline ${control.key}` : ""}
                      </p>
                      <p className="mf-bn-exps__goal">
                        {goal ? (
                          <>Measuring {goal.name}</>
                        ) : e.goal_id ? (
                          <>Goal {e.goal_id} is attached but was not found on this site.</>
                        ) : (
                          <span className="mf-bn-exps__nogoal">
                            No goal attached, so it cannot report.{" "}
                            <Link to={`/app/${site}/goals`}>Goals</Link>
                          </span>
                        )}
                      </p>
                    </div>

                    <div className="mf-bn-exps__act">
                      <Button
                        size="sm"
                        onClick={() =>
                          setView(isOpen && view?.mode === "results" ? null : { mode: "results", exp: e })
                        }
                      >
                        {isOpen && view?.mode === "results" ? "Close" : "Results"}
                      </Button>
                      <Button
                        size="sm"
                        tone="quiet"
                        icon={<IconSettings size={13} />}
                        aria-label={`Edit ${e.key}`}
                        onClick={() => setView({ mode: "edit", exp: e })}
                      />
                    </div>
                  </li>
                );
              })}
            </ul>
          </Reading>

          {goalsQ.isError ? (
            <p className="mf-bn-field__err" role="alert">
              Could not load goals, so you cannot attach a goal now. {errText(goalsQ.error)}
            </p>
          ) : null}
        </div>
      </Panel>

      {view?.mode === "edit" ? (
        <Composer
          key={view.exp.id || "new"}
          value={view.exp}
          goals={goals}
          siteId={siteId}
          onDone={() => setView(null)}
        />
      ) : null}

      {view?.mode === "results" ? (
        <ExperimentResults
          key={view.exp.id}
          experiment={view.exp}
          goal={view.exp.goal_id ? goalById.get(view.exp.goal_id) ?? null : null}
        />
      ) : null}
    </div>
  );
}

export default ExperimentsScreen;
