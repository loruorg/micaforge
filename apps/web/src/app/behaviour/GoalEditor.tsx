/**
 * The goal editor.
 *
 * A goal is a rule the record is measured against, so the form is built as a
 * rule: pick what counts, then state the condition. The five kinds are real
 * keys on one bank rather than a dropdown, because which kind you pick changes
 * the rest of the form and that consequence should be visible before you commit
 * to it.
 *
 * Validation runs before the request goes out, and the server's own field
 * errors are shown against the field they name rather than as one banner at the
 * top. A field that the server rejects clears its error as soon as it is
 * edited, so the form never argues with a value that has already been fixed.
 *
 * The form primitives at the foot of this file are the area's only ones; the
 * funnel editor and the journey controls import them from here rather than
 * keeping a second, drifting copy.
 */
import type { ReactNode } from "react";
import { useId, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ApiError, api } from "../../lib/api";
import { useSite } from "../providers";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconAlert, IconChevron, IconClose, IconPlus } from "../../components/react/icons";

/* ── the shape of a goal ─────────────────────────────────────────────────── */

export type GoalKind = "path" | "event" | "outbound" | "file" | "revenue";

export type MatchOp = "is" | "is_not" | "contains" | "starts_with" | "ends_with" | "matches";

export interface PropCondition {
  key: string;
  op: MatchOp;
  value: string;
}

export interface GoalConditions {
  op?: MatchOp;
  value?: string;
  event_name?: string;
  props?: PropCondition[];
  currency?: string;
  min_value?: number | null;
}

export interface Goal {
  id: string;
  name: string;
  kind: GoalKind;
  conditions: GoalConditions;
  value_cents: number | null;
}

const KINDS: { kind: GoalKind; name: string; what: string }[] = [
  { kind: "path", name: "Page reached", what: "a pageview whose path matches" },
  { kind: "event", name: "Event fired", what: "a named custom event" },
  { kind: "outbound", name: "Link followed", what: "a click leaving the site" },
  { kind: "file", name: "File downloaded", what: "a download of a matching file" },
  { kind: "revenue", name: "Revenue booked", what: "an event carrying an amount" },
];

const MATCH_OPS: { op: MatchOp; label: string }[] = [
  { op: "is", label: "is exactly" },
  { op: "starts_with", label: "starts with" },
  { op: "ends_with", label: "ends with" },
  { op: "contains", label: "contains" },
  { op: "is_not", label: "is not" },
  { op: "matches", label: "matches the pattern" },
];

type Errors = Record<string, string>;

/* ── the editor ──────────────────────────────────────────────────────────── */

export function GoalEditor({
  goal,
  onDone,
  onCancel,
}: {
  /** Omitted for a new goal. */
  goal?: Goal;
  onDone: (saved: Goal) => void;
  onCancel: () => void;
}) {
  const { siteId } = useSite();
  const qc = useQueryClient();
  const uid = useId().replace(/:/g, "");

  const [name, setName] = useState(goal?.name ?? "");
  const [kind, setKind] = useState<GoalKind>(goal?.kind ?? "path");
  const [op, setOp] = useState<MatchOp>(goal?.conditions.op ?? "is");
  const [value, setValue] = useState(goal?.conditions.value ?? "");
  const [eventName, setEventName] = useState(goal?.conditions.event_name ?? "");
  const [props, setProps] = useState<PropCondition[]>(goal?.conditions.props ?? []);
  const [currency, setCurrency] = useState(goal?.conditions.currency ?? "USD");
  const [minValue, setMinValue] = useState(
    goal?.conditions.min_value == null ? "" : String(goal.conditions.min_value),
  );
  const [worth, setWorth] = useState(
    goal?.value_cents == null ? "" : (goal.value_cents / 100).toFixed(2),
  );

  const [errors, setErrors] = useState<Errors>({});
  const [formError, setFormError] = useState<string | null>(null);

  const clearError = (field: string) =>
    setErrors((e) => (e[field] ? { ...e, [field]: "" } : e));

  const save = useMutation({
    mutationFn: async (body: Record<string, unknown>) => {
      const res = goal
        ? await api.patch<Goal>(`/api/goals/${goal.id}`, body)
        : await api.post<Goal>("/api/goals", body);
      return res.data;
    },
    onSuccess: (saved) => {
      void qc.invalidateQueries({ queryKey: ["goals", siteId] });
      void qc.invalidateQueries({ queryKey: ["stats", "goals", siteId] });
      onDone(saved);
    },
    onError: (err) => {
      if (err instanceof ApiError && err.field) {
        const field = err.field.replace(/^conditions\./, "");
        setErrors((e) => ({ ...e, [field]: err.message }));
        setFormError(null);
        return;
      }
      setFormError(
        err instanceof ApiError && err.code === "network"
          ? "Could not reach the Micaforge server. The goal was not saved. Try again once it answers."
          : err instanceof Error
            ? err.message
            : "The goal was not saved and the server did not say why.",
      );
    },
  });

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    setFormError(null);

    const found = validate({ name, kind, op, value, eventName, props, currency, minValue, worth });
    setErrors(found);
    if (Object.values(found).some(Boolean)) {
      const first = Object.keys(found).find((k) => found[k]);
      if (first) document.getElementById(`${uid}-${first}`)?.focus();
      return;
    }

    const conditions: GoalConditions =
      kind === "event"
        ? { event_name: eventName.trim(), props: props.filter((p) => p.key.trim()) }
        : kind === "revenue"
          ? {
              event_name: eventName.trim(),
              currency: currency.trim().toUpperCase(),
              min_value: minValue.trim() === "" ? null : Number(minValue),
            }
          : { op, value: value.trim() };

    save.mutate({
      site: siteId,
      name: name.trim(),
      kind,
      conditions,
      value_cents: worth.trim() === "" ? null : Math.round(Number(worth) * 100),
    });
  };

  const valueLabel =
    kind === "path"
      ? "Path"
      : kind === "outbound"
        ? "Destination"
        : kind === "file"
          ? "File"
          : "Value";

  const valueHint =
    kind === "path"
      ? "The path only, starting with a slash. Query strings are ignored."
      : kind === "outbound"
        ? "A host or a full URL, for example docs.example.com."
        : "A file path or an extension, for example .pdf.";

  return (
    <Panel>
      <PanelHead
        title={goal ? "Edit goal" : "New goal"}
        note={
          goal
            ? "Changes apply to how the record is read from now on. Past conversions are recounted, not rewritten."
            : "Name what counts as a conversion, then state the condition that recognises it."
        }
      />

      <form className="mfb-form" onSubmit={submit} noValidate>
        <div className="mfb-form__section">
          <Field
            id={`${uid}-name`}
            label="Goal name"
            hint="What you will call this in reports."
            error={errors.name}
          >
            <input
              id={`${uid}-name`}
              className="mfb-input"
              value={name}
              maxLength={80}
              aria-invalid={errors.name ? true : undefined}
              onChange={(e) => {
                setName(e.target.value);
                clearError("name");
              }}
              placeholder="Signed up"
            />
          </Field>
        </div>

        {/* A group rather than a <fieldset>: a fieldset seats its legend on
            its own top border, which is this section's ruling, and the rule
            would then run straight through the heading. */}
        <div className="mfb-form__section" role="group" aria-labelledby={`${uid}-counts`}>
          <h3 className="mfb-form__legend" id={`${uid}-counts`}>
            What counts
          </h3>
          <div className="mfb-kinds">
            {KINDS.map((k) => (
              <button
                key={k.kind}
                type="button"
                className="mfb-kind"
                aria-pressed={kind === k.kind}
                onClick={() => {
                  setKind(k.kind);
                  setErrors({});
                }}
              >
                <span className="mfb-kind__name">{k.name}</span>
                <span className="mfb-kind__what">{k.what}</span>
              </button>
            ))}
          </div>
        </div>

        <div className="mfb-form__section" role="group" aria-labelledby={`${uid}-condition`}>
          <h3 className="mfb-form__legend" id={`${uid}-condition`}>
            The condition
          </h3>

          {kind === "event" || kind === "revenue" ? (
            <div className="mfb-form__grid">
              <Field
                id={`${uid}-eventName`}
                label="Event name"
                hint="Exactly as it is passed to track()."
                error={errors.eventName}
              >
                <input
                  id={`${uid}-eventName`}
                  className="mfb-input mfb-input--data"
                  value={eventName}
                  aria-invalid={errors.eventName ? true : undefined}
                  onChange={(e) => {
                    setEventName(e.target.value);
                    clearError("eventName");
                  }}
                  placeholder={kind === "revenue" ? "purchase" : "signup"}
                />
              </Field>

              {kind === "revenue" ? (
                <>
                  <Field
                    id={`${uid}-currency`}
                    label="Currency"
                    hint="Three-letter code."
                    error={errors.currency}
                  >
                    <input
                      id={`${uid}-currency`}
                      className="mfb-input mfb-input--data"
                      value={currency}
                      maxLength={3}
                      aria-invalid={errors.currency ? true : undefined}
                      onChange={(e) => {
                        setCurrency(e.target.value.toUpperCase());
                        clearError("currency");
                      }}
                    />
                  </Field>
                  <Field
                    id={`${uid}-minValue`}
                    label="Minimum amount"
                    hint="Optional. Events below this do not count."
                    error={errors.minValue}
                  >
                    <input
                      id={`${uid}-minValue`}
                      className="mfb-input mfb-input--data"
                      inputMode="decimal"
                      value={minValue}
                      aria-invalid={errors.minValue ? true : undefined}
                      onChange={(e) => {
                        setMinValue(e.target.value);
                        clearError("minValue");
                      }}
                      placeholder="0.00"
                    />
                  </Field>
                </>
              ) : null}
            </div>
          ) : (
            <div className="mfb-form__grid">
              <Field id={`${uid}-op`} label="Match" error={errors.op}>
                <Select
                  id={`${uid}-op`}
                  value={op}
                  onChange={(v) => setOp(v as MatchOp)}
                  options={MATCH_OPS.map((m) => ({ value: m.op, label: m.label }))}
                />
              </Field>

              <Field
                id={`${uid}-value`}
                label={valueLabel}
                hint={valueHint}
                error={errors.value}
              >
                <input
                  id={`${uid}-value`}
                  className="mfb-input mfb-input--data"
                  value={value}
                  aria-invalid={errors.value ? true : undefined}
                  onChange={(e) => {
                    setValue(e.target.value);
                    clearError("value");
                  }}
                  placeholder={
                    kind === "path" ? "/thank-you" : kind === "file" ? ".pdf" : "github.com"
                  }
                />
              </Field>
            </div>
          )}

          {kind === "event" ? (
            <PropConditions
              uid={uid}
              props={props}
              onChange={(next) => {
                setProps(next);
                clearError("props");
              }}
              error={errors.props}
            />
          ) : null}
        </div>

        <div className="mfb-form__section">
          <Field
            id={`${uid}-worth`}
            label="Worth per conversion"
            hint="Optional. Used to price conversions where the event carries no amount of its own."
            error={errors.worth}
          >
            <input
              id={`${uid}-worth`}
              className="mfb-input mfb-input--data"
              inputMode="decimal"
              value={worth}
              aria-invalid={errors.worth ? true : undefined}
              onChange={(e) => {
                setWorth(e.target.value);
                clearError("worth");
              }}
              placeholder="0.00"
            />
          </Field>
        </div>

        {formError ? (
          <p className="mfb-alert" role="alert">
            <IconAlert size={14} />
            <span>{formError}</span>
          </p>
        ) : null}

        <div className="mfb-form__foot">
          <Button type="submit" tone="primary" disabled={save.isPending}>
            {save.isPending ? "Saving…" : goal ? "Save goal" : "Create goal"}
          </Button>
          <Button type="button" tone="quiet" onClick={onCancel}>
            Cancel
          </Button>
          <span className="mfb-field__hint mfb-form__foot-spacer">
            {kind === "revenue"
              ? "Revenue goals read the amount from the event itself."
              : "Conversions are counted per session."}
          </span>
        </div>
      </form>
    </Panel>
  );
}

/* ── property conditions ─────────────────────────────────────────────────── */

function PropConditions({
  uid,
  props,
  onChange,
  error,
}: {
  uid: string;
  props: PropCondition[];
  onChange: (next: PropCondition[]) => void;
  error?: string;
}) {
  const set = (i: number, patch: Partial<PropCondition>) =>
    onChange(props.map((p, k) => (k === i ? { ...p, ...patch } : p)));

  return (
    <div className="mfb-form__sub">
      <p className="mfb-field__label" id={`${uid}-props-label`}>
        Property conditions
      </p>
      <p className="mfb-field__hint mfb-form__subhint">
        Optional. All of them must hold for the event to count.
      </p>

      {error ? (
        <p className="mfb-field__err mfb-form__subhint">
          {error}
        </p>
      ) : null}

      {props.length > 0 ? (
        <div className="mfb-steps mfb-steps--spaced">
          {props.map((p, i) => (
            <div className="mfb-estep" key={i}>
              <span className="mfb-estep__ord" aria-hidden="true">
                {i + 1}
              </span>
              <div className="mfb-estep__fields">
                <input
                  className="mfb-input mfb-input--data"
                  value={p.key}
                  aria-label={`Property ${i + 1} key`}
                  placeholder="plan"
                  onChange={(e) => set(i, { key: e.target.value })}
                />
                <Select
                  id={`${uid}-prop-op-${i}`}
                  ariaLabel={`Property ${i + 1} match`}
                  value={p.op}
                  onChange={(v) => set(i, { op: v as MatchOp })}
                  options={MATCH_OPS.map((m) => ({ value: m.op, label: m.label }))}
                />
                <input
                  className="mfb-input mfb-input--data"
                  value={p.value}
                  aria-label={`Property ${i + 1} value`}
                  placeholder="pro"
                  onChange={(e) => set(i, { value: e.target.value })}
                />
              </div>
              <span className="mfb-estep__keys">
                <Button
                  size="sm"
                  icon={<IconClose size={12} />}
                  onClick={() => onChange(props.filter((_, k) => k !== i))}
                  aria-label={`Remove property condition ${i + 1}`}
                />
              </span>
            </div>
          ))}
        </div>
      ) : null}

      <Button
        size="sm"
        icon={<IconPlus size={12} />}
        onClick={() => onChange([...props, { key: "", op: "is", value: "" }])}
      >
        Add a property condition
      </Button>
    </div>
  );
}

/* ── validation ──────────────────────────────────────────────────────────── */

function validate(v: {
  name: string;
  kind: GoalKind;
  op: MatchOp;
  value: string;
  eventName: string;
  props: PropCondition[];
  currency: string;
  minValue: string;
  worth: string;
}): Errors {
  const e: Errors = {};

  if (!v.name.trim()) e.name = "Give the goal a name. It is how you will find it in reports.";
  else if (v.name.trim().length > 80) e.name = "Keep the name under 80 characters.";

  if (v.kind === "event" || v.kind === "revenue") {
    if (!v.eventName.trim()) e.eventName = "Name the event this goal counts.";
    else if (/\s/.test(v.eventName.trim()))
      e.eventName = "Event names carry no spaces. Use the name exactly as track() sends it.";
  } else {
    if (!v.value.trim()) e.value = "State what the goal has to match.";
    else if (v.kind === "path" && (v.op === "is" || v.op === "starts_with") && !v.value.trim().startsWith("/"))
      e.value = "A path starts with a slash, for example /thank-you.";
    else if (v.op === "matches") {
      try {
        new RegExp(v.value);
      } catch {
        e.value = "That pattern is not a valid regular expression.";
      }
    }
  }

  if (v.kind === "event") {
    const broken = v.props.findIndex((p) => !p.key.trim() && p.value.trim());
    if (broken >= 0) e.props = `Condition ${broken + 1} has a value but no property key.`;
  }

  if (v.kind === "revenue") {
    if (!/^[A-Za-z]{3}$/.test(v.currency.trim()))
      e.currency = "Use a three-letter currency code, for example USD.";
    if (v.minValue.trim() !== "") {
      const n = Number(v.minValue);
      if (!Number.isFinite(n)) e.minValue = "The minimum has to be a number.";
      else if (n < 0) e.minValue = "The minimum cannot be negative.";
    }
  }

  if (v.worth.trim() !== "") {
    const n = Number(v.worth);
    if (!Number.isFinite(n)) e.worth = "Worth has to be a number, or left empty.";
    else if (n < 0) e.worth = "Worth cannot be negative.";
  }

  return e;
}

/* ── the area's form primitives ──────────────────────────────────────────── */

export function Field({
  id,
  label,
  hint,
  error,
  children,
}: {
  id: string;
  label: string;
  hint?: ReactNode;
  error?: string;
  children: ReactNode;
}) {
  return (
    <div className="mfb-field">
      <label className="mfb-field__label" htmlFor={id}>
        {label}
      </label>
      {children}
      {error ? (
        <p className="mfb-field__err" role="alert">
          {error}
        </p>
      ) : hint ? (
        <p className="mfb-field__hint">{hint}</p>
      ) : null}
    </div>
  );
}

/**
 * The caret is drawn from the icon set rather than left to the platform, so the
 * control belongs to the bench in both recording media.
 */
export function Select({
  id,
  value,
  onChange,
  options,
  ariaLabel,
  disabled,
  invalid,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
  ariaLabel?: string;
  disabled?: boolean;
  invalid?: boolean;
}) {
  return (
    <span className="mfb-selectwrap">
      <select
        id={id}
        className="mfb-select"
        value={value}
        disabled={disabled}
        aria-label={ariaLabel}
        aria-invalid={invalid ? true : undefined}
        onChange={(e) => onChange(e.target.value)}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <IconChevron size={14} className="mfb-selectwrap__caret" />
    </span>
  );
}

export { KINDS as GOAL_KINDS, MATCH_OPS };
export default GoalEditor;
