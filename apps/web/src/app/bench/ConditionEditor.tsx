/**
 * TARGETING CONDITIONS, for a flag and for a target group.
 *
 * A flag is evaluated against whatever context the SDK passes, not against a
 * stored event, so the attribute is free text: `plan`, `email`, `user_id`,
 * anything the calling code knows. The suggestions are the attributes the
 * browser SDK fills in by itself plus the ones people most often send; none of
 * them is required and none is checked against a list.
 *
 * The server stores conditions exactly as sent, and the one evaluator shared by
 * the server and both SDKs reads three shapes: a list (every one must match), a
 * single leaf, or a group `{ logic, filters }` that can nest. This editor
 * writes the first two and the one-level `or` group, which is everything a
 * person builds by hand. A nested group written through the API is shown as it
 * is and left alone, rather than flattened into something that means a
 * different thing.
 *
 * `matches` (a regular expression) is not offered: the evaluator answers it
 * false on every platform, so offering it would be offering a condition that
 * can never be true. A stored one is still shown, and said to be inert.
 */
import { useId } from "react";
import type { Filter } from "../../lib/api";
import { Button } from "../../components/react/ui";
import { IconClose, IconPlus } from "../../components/react/icons";
import { KeyBank, OPS, Select, TextInput } from "./QueryScreen";

/* ═══ the model ═══════════════════════════════════════════════════════════ */

export type ConditionOp = Filter["op"];

/** One condition as it is edited: the value is kept as typed until saved. */
export interface ConditionLeaf {
  dimension: string;
  op: ConditionOp;
  value: string;
}

export interface ConditionSet {
  logic: "and" | "or";
  leaves: ConditionLeaf[];
  /**
   * Conditions in a shape this editor cannot show as a list (nested groups),
   * kept verbatim so saving the flag does not rewrite them. Null otherwise.
   */
  verbatim: unknown;
}

/** The operators a person can choose. `matches` is left out on purpose. */
export const CONDITION_OPS = OPS.filter((o) => o.v !== "matches");

/** Operators that take a list of values when the value has commas in it. */
const LIST_OPS: ConditionOp[] = ["is", "is_not"];

/** Operators that compare numbers. The value must be a number to mean anything. */
const NUMBER_OPS: ConditionOp[] = ["gt", "lt", "gte", "lte"];

const isValueless = (op: ConditionOp) => op === "is_set" || op === "is_not_set";

/** Attributes worth suggesting. Free text is accepted; these only save typing. */
export const ATTRIBUTE_SUGGESTIONS: { v: string; note: string }[] = [
  { v: "country", note: "two-letter country code" },
  { v: "device_type", note: "desktop, mobile or tablet" },
  { v: "browser", note: "browser name" },
  { v: "os", note: "operating system" },
  { v: "language", note: "browser language" },
  { v: "plan", note: "a value your code passes" },
  { v: "email", note: "a value your code passes" },
  { v: "user_id", note: "a value your code passes" },
];

export const emptyConditions = (): ConditionSet => ({ logic: "and", leaves: [], verbatim: null });

const NUMBER_RE = /^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$/;

function isLeafShape(v: unknown): v is { dimension: unknown; op?: unknown; value?: unknown } {
  return !!v && typeof v === "object" && !Array.isArray(v) && "dimension" in v;
}

function valueText(v: unknown): string {
  if (v == null) return "";
  if (Array.isArray(v)) return v.map((x) => (x == null ? "" : String(x))).join(", ");
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

function toLeaf(raw: { dimension: unknown; op?: unknown; value?: unknown }): ConditionLeaf {
  return {
    dimension: String(raw.dimension ?? ""),
    op: (typeof raw.op === "string" && raw.op ? raw.op : "is") as ConditionOp,
    value: valueText(raw.value),
  };
}

/** Reads whatever the server stored into the editor's model. Never throws. */
export function readConditions(raw: unknown): ConditionSet {
  let v = raw;
  if (typeof v === "string") {
    try {
      v = JSON.parse(v);
    } catch {
      return emptyConditions();
    }
  }
  if (v == null) return emptyConditions();
  if (Array.isArray(v)) {
    if (v.every(isLeafShape)) return { logic: "and", leaves: v.map(toLeaf), verbatim: null };
    return { logic: "and", leaves: [], verbatim: v };
  }
  if (typeof v !== "object") return emptyConditions();
  if (isLeafShape(v)) return { logic: "and", leaves: [toLeaf(v)], verbatim: null };

  const group = v as { logic?: unknown; filters?: unknown };
  if (group.filters === undefined && Object.keys(group).length === 0) return emptyConditions();
  const filters = Array.isArray(group.filters) ? group.filters : null;
  if (filters && filters.every(isLeafShape)) {
    return { logic: group.logic === "or" ? "or" : "and", leaves: filters.map(toLeaf), verbatim: null };
  }
  return { logic: "and", leaves: [], verbatim: v };
}

function leafValue(l: ConditionLeaf): string | string[] | undefined {
  if (isValueless(l.op)) return undefined;
  const text = l.value.trim();
  if (LIST_OPS.includes(l.op) && text.includes(",")) {
    return text
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  }
  return text;
}

/** Writes the editor's model in the shape the server and every SDK evaluate. */
export function writeConditions(set: ConditionSet): unknown {
  if (set.verbatim != null) return set.verbatim;
  const leaves = set.leaves.map((l) => {
    const value = leafValue(l);
    return value === undefined
      ? { dimension: l.dimension.trim(), op: l.op }
      : { dimension: l.dimension.trim(), op: l.op, value };
  });
  if (!leaves.length) return [];
  // An OR of nothing matches nobody; a single condition is the same either way.
  return set.logic === "or" && leaves.length > 1 ? { logic: "or", filters: leaves } : leaves;
}

export function conditionCount(set: ConditionSet): number {
  return set.verbatim != null ? 1 : set.leaves.length;
}

const opWord = (op: ConditionOp): string =>
  CONDITION_OPS.find((o) => o.v === op)?.label ?? (op === "matches" ? "matches (never true)" : op);

/** One plain sentence: "plan is pro and country is PT". */
export function describeConditions(set: ConditionSet): string {
  if (set.verbatim != null) return "nested conditions written through the API";
  if (!set.leaves.length) return "everyone";
  return set.leaves
    .map((l) =>
      isValueless(l.op)
        ? `${l.dimension || "?"} ${opWord(l.op)}`
        : `${l.dimension || "?"} ${opWord(l.op)} ${l.value.trim() || "(empty)"}`,
    )
    .join(set.logic === "or" ? " or " : " and ");
}

/** The first problem with a set of conditions, worded as the fix. */
export function conditionProblem(set: ConditionSet): string | undefined {
  for (const l of set.leaves) {
    if (!l.dimension.trim()) return "Every condition needs an attribute name, such as plan or country.";
    if (isValueless(l.op)) continue;
    if (!l.value.trim()) return `Give "${l.dimension}" a value, or choose "is not set".`;
    if (NUMBER_OPS.includes(l.op) && !NUMBER_RE.test(l.value.trim()))
      return `"${l.dimension}" ${opWord(l.op)} needs a number.`;
  }
  return undefined;
}

/* ═══ the editor ══════════════════════════════════════════════════════════ */

const BLANK: ConditionLeaf = { dimension: "", op: "is", value: "" };

export function ConditionEditor({
  value,
  onChange,
  addLabel = "Add a condition",
  emptyNote = "No conditions. Everyone matches.",
  error,
}: {
  value: ConditionSet;
  onChange: (next: ConditionSet) => void;
  addLabel?: string;
  emptyNote?: string;
  error?: string;
}) {
  const listId = useId();
  const leaves = value.leaves;
  const set = (i: number, patch: Partial<ConditionLeaf>) =>
    onChange({ ...value, leaves: leaves.map((l, k) => (k === i ? { ...l, ...patch } : l)) });

  if (value.verbatim != null) {
    return (
      <div className="mf-fl-cond">
        <p className="mf-fl-cond__note">
          These conditions were written through the API with nested groups, which this editor cannot
          show as a list. They are kept exactly as they are when you save. Replace them to edit them
          here.
        </p>
        <pre className="mf-fl-cond__raw mf-data">{JSON.stringify(value.verbatim, null, 2)}</pre>
        <Button size="sm" onClick={() => onChange(emptyConditions())}>
          Replace with an editable list
        </Button>
      </div>
    );
  }

  return (
    <div className="mf-bn-cond">
      <datalist id={listId}>
        {ATTRIBUTE_SUGGESTIONS.map((a) => (
          <option key={a.v} value={a.v}>
            {a.note}
          </option>
        ))}
      </datalist>

      {leaves.length > 1 ? (
        <KeyBank
          label="How the conditions combine"
          value={value.logic}
          onChange={(logic) => onChange({ ...value, logic })}
          options={[
            { v: "and", label: "Match every condition" },
            { v: "or", label: "Match any condition" },
          ]}
        />
      ) : null}

      {leaves.length === 0 ? (
        <p className="mf-bn-cond__none">{emptyNote}</p>
      ) : (
        <ul className="mf-bn-cond__list">
          {leaves.map((l, i) => {
            const valueless = isValueless(l.op);
            const known = CONDITION_OPS.some((o) => o.v === l.op);
            return (
              <li key={i} className="mf-fl-cond__row">
                <span className="mf-bn-cond__join">
                  {i === 0 ? "where" : value.logic === "or" ? "or" : "and"}
                </span>

                <TextInput
                  value={l.dimension}
                  mono
                  list={listId}
                  ariaLabel={`Condition ${i + 1} attribute`}
                  placeholder="plan"
                  spellCheck={false}
                  autoCapitalize="off"
                  onChange={(v) => set(i, { dimension: v })}
                />

                <Select
                  value={l.op}
                  ariaLabel={`Condition ${i + 1} operator`}
                  onChange={(v) => set(i, { op: v as ConditionOp })}
                >
                  {known ? null : <option value={l.op}>{opWord(l.op)}</option>}
                  {CONDITION_OPS.map((o) => (
                    <option key={o.v} value={o.v}>
                      {o.label}
                    </option>
                  ))}
                </Select>

                {valueless ? (
                  <span className="mf-bn-cond__novalue">no value needed</span>
                ) : (
                  <TextInput
                    value={l.value}
                    mono
                    ariaLabel={`Condition ${i + 1} value`}
                    placeholder={LIST_OPS.includes(l.op) ? "pro, team" : NUMBER_OPS.includes(l.op) ? "10" : "value"}
                    onChange={(v) => set(i, { value: v })}
                  />
                )}

                <Button
                  size="sm"
                  tone="quiet"
                  icon={<IconClose size={13} />}
                  aria-label={`Remove condition ${i + 1}`}
                  onClick={() => onChange({ ...value, leaves: leaves.filter((_, k) => k !== i) })}
                />
              </li>
            );
          })}
        </ul>
      )}

      {leaves.some((l) => LIST_OPS.includes(l.op)) ? (
        <p className="mf-bn-field__hint">
          With "is" and "is not", separate several values with commas: "pro, team" matches either one.
          Text is compared without regard to case.
        </p>
      ) : null}

      {error ? (
        <p className="mf-bn-field__err" role="alert">
          {error}
        </p>
      ) : null}

      <Button
        size="sm"
        icon={<IconPlus size={13} />}
        onClick={() => onChange({ ...value, leaves: [...leaves, { ...BLANK }] })}
      >
        {addLabel}
      </Button>
    </div>
  );
}

export default ConditionEditor;
