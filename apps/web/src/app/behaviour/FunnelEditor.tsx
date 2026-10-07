/**
 * The funnel editor.
 *
 * A funnel is an ordered rule, so order is a first-class control here: every
 * step can be moved with a key, which means it can be moved from a keyboard and
 * from a screen reader. There is no drag handle, because a drag handle is the
 * one reordering affordance that cannot be operated without a pointer.
 *
 * Steps are numbered in the register's own data face and the number is the
 * step's position, recomputed on every move: it is never a label carried along
 * with the row.
 */
import { useId, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ApiError, api } from "../../lib/api";
import { useSite } from "../providers";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconAlert, IconChevron, IconClose, IconPlus } from "../../components/react/icons";
import { Field, Select, type Goal, type MatchOp } from "./GoalEditor";

export type StepKind = "page" | "event" | "goal";

export interface FunnelStep {
  kind: StepKind;
  op: MatchOp;
  /** A path, an event name, or a goal id, depending on `kind`. */
  value: string;
  /** What to call the step in the report. Falls back to the value. */
  label?: string;
}

export interface Funnel {
  id: string;
  name: string;
  steps: FunnelStep[];
  window_hours: number;
}

const STEP_KINDS: { value: StepKind; label: string }[] = [
  { value: "page", label: "Page reached" },
  { value: "event", label: "Event fired" },
  { value: "goal", label: "Goal converted" },
];

const PATH_OPS: { value: MatchOp; label: string }[] = [
  { value: "is", label: "is exactly" },
  { value: "starts_with", label: "starts with" },
  { value: "contains", label: "contains" },
  { value: "matches", label: "matches" },
];

const WINDOWS: { value: string; label: string }[] = [
  { value: "1", label: "1 hour" },
  { value: "6", label: "6 hours" },
  { value: "24", label: "1 day" },
  { value: "72", label: "3 days" },
  { value: "168", label: "7 days" },
  { value: "720", label: "30 days" },
];

const emptyStep = (): FunnelStep => ({ kind: "page", op: "is", value: "", label: "" });

type Errors = Record<string, string>;

export function FunnelEditor({
  funnel,
  goals,
  goalsUnavailable,
  onDone,
  onCancel,
}: {
  funnel?: Funnel;
  /** Existing goals, offered as steps. Empty is a normal state, not an error. */
  goals: Goal[];
  /** True when the goals could not be read, so that step kind is not offered. */
  goalsUnavailable?: boolean;
  onDone: (saved: Funnel) => void;
  onCancel: () => void;
}) {
  const { siteId } = useSite();
  const qc = useQueryClient();
  const uid = useId().replace(/:/g, "");

  const [name, setName] = useState(funnel?.name ?? "");
  const [windowHours, setWindowHours] = useState(String(funnel?.window_hours ?? 24));
  const [steps, setSteps] = useState<FunnelStep[]>(
    funnel?.steps?.length ? funnel.steps.map((s) => ({ ...s })) : [emptyStep(), emptyStep()],
  );
  const [errors, setErrors] = useState<Errors>({});
  const [formError, setFormError] = useState<string | null>(null);

  const kindOptions = goalsUnavailable
    ? STEP_KINDS.filter((k) => k.value !== "goal")
    : STEP_KINDS;

  const patch = (i: number, next: Partial<FunnelStep>) => {
    setSteps((s) => s.map((st, k) => (k === i ? { ...st, ...next } : st)));
    setErrors((e) => (e[`step-${i}`] ? { ...e, [`step-${i}`]: "" } : e));
  };

  const move = (from: number, to: number) => {
    if (to < 0 || to >= steps.length) return;
    setSteps((s) => {
      const next = [...s];
      const [taken] = next.splice(from, 1);
      next.splice(to, 0, taken);
      return next;
    });
    setErrors({});
    // Follow the step to its new place, so the order can be changed repeatedly
    // without hunting for the key again. A step moved to either end finds that
    // key disabled, so focus goes to the one that can still move it, never to
    // a disabled control, and never nowhere.
    window.requestAnimationFrame(() => {
      const went = to > from ? "down" : "up";
      const back = went === "down" ? "up" : "down";
      const key = (d: string) =>
        document.getElementById(`${uid}-move-${to}-${d}`) as HTMLButtonElement | null;
      const first = key(went);
      (first && !first.disabled ? first : key(back))?.focus();
    });
  };

  const save = useMutation({
    mutationFn: async (body: Record<string, unknown>) => {
      const res = funnel
        ? await api.patch<Funnel>(`/api/funnels/${funnel.id}`, body)
        : await api.post<Funnel>("/api/funnels", body);
      return res.data;
    },
    onSuccess: (saved) => {
      void qc.invalidateQueries({ queryKey: ["funnels", siteId] });
      void qc.invalidateQueries({ queryKey: ["stats", "funnel", siteId] });
      onDone(saved);
    },
    onError: (err) => {
      if (err instanceof ApiError && err.field) {
        // The server names a step as steps.<index>.<field>; show it on that step.
        const m = /^steps\.(\d+)/.exec(err.field);
        setErrors((e) => ({
          ...e,
          [m ? `step-${m[1]}` : err.field!]: err.message,
        }));
        setFormError(null);
        return;
      }
      setFormError(
        err instanceof ApiError && err.code === "network"
          ? "Could not reach the Micaforge server. The funnel was not saved. Try again once it answers."
          : err instanceof Error
            ? err.message
            : "The funnel was not saved and the server did not say why.",
      );
    },
  });

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    setFormError(null);

    const found: Errors = {};
    if (!name.trim()) found.name = "Give the funnel a name.";
    if (steps.length < 2) found.steps = "A funnel needs at least two steps to have a drop between them.";

    steps.forEach((s, i) => {
      if (!s.value.trim()) {
        found[`step-${i}`] =
          s.kind === "goal" ? "Choose which goal this step is." : "State what this step matches.";
      } else if (s.kind === "page" && (s.op === "is" || s.op === "starts_with") && !s.value.trim().startsWith("/")) {
        found[`step-${i}`] = "A path starts with a slash, for example /pricing.";
      } else if (s.kind === "page" && s.op === "matches") {
        try {
          new RegExp(s.value);
        } catch {
          found[`step-${i}`] = "That pattern is not a valid regular expression.";
        }
      }
    });

    setErrors(found);
    if (Object.values(found).some(Boolean)) {
      const first = Object.keys(found).find((k) => found[k]);
      if (first === "name") document.getElementById(`${uid}-name`)?.focus();
      else if (first?.startsWith("step-"))
        document.getElementById(`${uid}-value-${first.slice(5)}`)?.focus();
      return;
    }

    save.mutate({
      site: siteId,
      name: name.trim(),
      window_hours: Number(windowHours),
      steps: steps.map((s) => ({
        kind: s.kind,
        op: s.kind === "page" ? s.op : "is",
        value: s.value.trim(),
        label: s.label?.trim() || undefined,
      })),
    });
  };

  return (
    <Panel>
      <PanelHead
        title={funnel ? "Edit funnel" : "New funnel"}
        note="Steps are counted in the order they are listed. A session must reach each one within the window to be carried to the next."
      />

      <form className="mfb-form" onSubmit={submit} noValidate>
        <div className="mfb-form__section">
          <div className="mfb-form__grid">
            <Field id={`${uid}-name`} label="Funnel name" error={errors.name}>
              <input
                id={`${uid}-name`}
                className="mfb-input"
                value={name}
                maxLength={80}
                aria-invalid={errors.name ? true : undefined}
                onChange={(e) => {
                  setName(e.target.value);
                  setErrors((x) => ({ ...x, name: "" }));
                }}
                placeholder="Docs to signup"
              />
            </Field>

            <Field
              id={`${uid}-window`}
              label="Completion window"
              hint="How long a session has to finish the whole funnel."
            >
              <Select
                id={`${uid}-window`}
                value={windowHours}
                onChange={setWindowHours}
                options={WINDOWS}
              />
            </Field>
          </div>
        </div>

        {/* See GoalEditor: a group, not a <fieldset>, so this section's own
            ruling is not drawn through its heading. */}
        <div className="mfb-form__section" role="group" aria-labelledby={`${uid}-steps`}>
          <h3 className="mfb-form__legend" id={`${uid}-steps`}>
            Steps
          </h3>

          {errors.steps ? (
            <p className="mfb-field__err mfb-form__blockerr" role="alert">
              {errors.steps}
            </p>
          ) : null}

          <div className="mfb-steps">
            {steps.map((s, i) => (
              <div className="mfb-estep" key={i}>
                <span className="mfb-estep__ord" aria-hidden="true">
                  {String(i + 1).padStart(2, "0")}
                </span>

                <div className="mfb-estep__fields">
                  <Select
                    id={`${uid}-kind-${i}`}
                    ariaLabel={`Step ${i + 1} kind`}
                    value={s.kind}
                    /* Changing what a step matches clears what it matched:
                       an event name is not a path and must not survive the
                       switch as one. */
                    onChange={(v) => patch(i, { kind: v as StepKind, value: "", op: "is" })}
                    options={kindOptions}
                  />

                  {s.kind === "page" ? (
                    <Select
                      id={`${uid}-op-${i}`}
                      ariaLabel={`Step ${i + 1} match`}
                      value={s.op}
                      onChange={(v) => patch(i, { op: v as MatchOp })}
                      options={PATH_OPS}
                    />
                  ) : null}

                  {s.kind === "goal" ? (
                    <Select
                      id={`${uid}-value-${i}`}
                      ariaLabel={`Step ${i + 1} goal`}
                      value={s.value}
                      invalid={Boolean(errors[`step-${i}`])}
                      onChange={(v) =>
                        patch(i, {
                          value: v,
                          label: goals.find((g) => g.id === v)?.name ?? "",
                        })
                      }
                      options={[
                        { value: "", label: goals.length ? "Choose a goal…" : "No goals set yet" },
                        ...goals.map((g) => ({ value: g.id, label: g.name })),
                      ]}
                    />
                  ) : (
                    <input
                      id={`${uid}-value-${i}`}
                      className="mfb-input mfb-input--data"
                      value={s.value}
                      aria-label={`Step ${i + 1} ${s.kind === "page" ? "path" : "event name"}`}
                      aria-invalid={errors[`step-${i}`] ? true : undefined}
                      placeholder={s.kind === "page" ? "/pricing" : "signup"}
                      onChange={(e) => patch(i, { value: e.target.value })}
                    />
                  )}

                  <input
                    className="mfb-input"
                    value={s.label ?? ""}
                    aria-label={`Step ${i + 1} name in the report`}
                    placeholder="Name in the report (optional)"
                    onChange={(e) => patch(i, { label: e.target.value })}
                  />

                  {errors[`step-${i}`] ? (
                    <p className="mfb-field__err mfb-estep__err" role="alert">
                      {errors[`step-${i}`]}
                    </p>
                  ) : null}
                </div>

                <span className="mfb-estep__keys">
                  <Button
                    id={`${uid}-move-${i}-up`}
                    size="sm"
                    icon={<IconChevron size={12} className="mfb-icon-flip" />}
                    disabled={i === 0}
                    aria-label={`Move step ${i + 1} earlier`}
                    onClick={() => move(i, i - 1)}
                  />
                  <Button
                    id={`${uid}-move-${i}-down`}
                    size="sm"
                    icon={<IconChevron size={12} />}
                    disabled={i === steps.length - 1}
                    aria-label={`Move step ${i + 1} later`}
                    onClick={() => move(i, i + 1)}
                  />
                  <Button
                    size="sm"
                    icon={<IconClose size={12} />}
                    disabled={steps.length <= 2}
                    aria-label={`Remove step ${i + 1}`}
                    onClick={() => {
                      setSteps((x) => x.filter((_, k) => k !== i));
                      setErrors({});
                    }}
                  />
                </span>
              </div>
            ))}
          </div>

          <div className="mfb-form__add">
            <Button
              size="sm"
              icon={<IconPlus size={12} />}
              onClick={() => setSteps((s) => [...s, emptyStep()])}
            >
              Add a step
            </Button>
            {goalsUnavailable ? (
              <p className="mfb-field__hint mfb-form__addhint">
                Goal steps are unavailable because the goals could not be read.
              </p>
            ) : null}
          </div>
        </div>

        {formError ? (
          <p className="mfb-alert" role="alert">
            <IconAlert size={14} />
            <span>{formError}</span>
          </p>
        ) : null}

        <div className="mfb-form__foot">
          <Button type="submit" tone="primary" disabled={save.isPending}>
            {save.isPending ? "Saving…" : funnel ? "Save funnel" : "Create funnel"}
          </Button>
          <Button type="button" tone="quiet" onClick={onCancel}>
            Cancel
          </Button>
          <span className="mfb-field__hint mfb-form__foot-spacer">
            {steps.length} step{steps.length === 1 ? "" : "s"}, completed within{" "}
            {WINDOWS.find((w) => w.value === windowHours)?.label ?? `${windowHours} hours`}.
          </span>
        </div>
      </form>
    </Panel>
  );
}

export default FunnelEditor;
