/**
 * Two rules a flag can carry besides its rollout: PREREQUISITES and
 * ENVIRONMENTS.
 *
 * A prerequisite is another flag on the same site that must evaluate a given
 * way first: "only when new-checkout is on, and its variant is fast". It is
 * checked before targeting and before the rollout, so a flag whose prerequisite
 * fails is off whatever its own settings say. The server refuses a chain that
 * loops back on itself and names the loop; that message is shown under this
 * section, where the loop can be broken.
 *
 * An environment override replaces the flag's serving switch, its rollout, or
 * both, when an SDK says which environment it runs in. It is the whole of what
 * "on in staging, off in production" needs, and nothing more: there is no
 * separate copy of the flag per environment to drift out of step.
 */
import { useState } from "react";
import { Button } from "../../components/react/ui";
import { IconClose, IconPlus } from "../../components/react/icons";
import { KeyBank, NumberInput, Select, TextInput } from "./QueryScreen";
import type { FlagDependency, FlagEnvironment, FlagVariant } from "./FlagEditor";

/* ═══ prerequisites ═══════════════════════════════════════════════════════ */

/** The other flags on the site, as far as a prerequisite needs to know them. */
export interface FlagSummary {
  key: string;
  variants: FlagVariant[];
}

export function DependencyEditor({
  value,
  onChange,
  flags,
  selfKey,
  error,
}: {
  value: FlagDependency[];
  onChange: (next: FlagDependency[]) => void;
  /** Every flag on the site; this flag is left out of the choice. */
  flags: FlagSummary[];
  selfKey: string;
  /** The server's refusal, usually the loop it found. */
  error?: string;
}) {
  const others = flags.filter((f) => f.key !== selfKey);
  const set = (i: number, patch: Partial<FlagDependency>) =>
    onChange(value.map((d, k) => (k === i ? { ...d, ...patch } : d)));
  const unused = others.find((f) => !value.some((d) => d.flag === f.key));

  return (
    <div className="mf-fl-deps">
      {value.length === 0 ? (
        <p className="mf-bn-cond__none">No prerequisites. This flag does not depend on any other flag.</p>
      ) : (
        <ul className="mf-fl-deps__list">
          {value.map((d, i) => {
            const target = others.find((f) => f.key === d.flag);
            const variants = target?.variants ?? [];
            return (
              <li key={i} className="mf-fl-deps__row">
                <span className="mf-fl-deps__word">{i === 0 ? "Only when" : "and when"}</span>
                <Select
                  value={d.flag}
                  ariaLabel={`Prerequisite ${i + 1} flag`}
                  onChange={(v) => set(i, { flag: v, variant: null })}
                >
                  {target ? null : (
                    <option value={d.flag}>{d.flag ? `${d.flag} (not on this site)` : "Choose a flag"}</option>
                  )}
                  {others.map((f) => (
                    <option key={f.key} value={f.key}>
                      {f.key}
                    </option>
                  ))}
                </Select>
                <span className="mf-fl-deps__word">is</span>
                <KeyBank
                  label={`Prerequisite ${i + 1} state`}
                  value={d.enabled ? "on" : "off"}
                  onChange={(v) => set(i, { enabled: v === "on", variant: v === "on" ? d.variant : null })}
                  options={[
                    { v: "on", label: "on" },
                    { v: "off", label: "off" },
                  ]}
                />
                {d.enabled && variants.length ? (
                  <>
                    <span className="mf-fl-deps__word">with variant</span>
                    <Select
                      value={d.variant ?? ""}
                      ariaLabel={`Prerequisite ${i + 1} variant`}
                      onChange={(v) => set(i, { variant: v || null })}
                    >
                      <option value="">any variant</option>
                      {d.variant && !variants.some((x) => x.key === d.variant) ? (
                        <option value={d.variant}>{d.variant} (no longer a variant)</option>
                      ) : null}
                      {variants.map((x) => (
                        <option key={x.key} value={x.key}>
                          {x.key}
                        </option>
                      ))}
                    </Select>
                  </>
                ) : null}
                <Button
                  size="sm"
                  tone="quiet"
                  icon={<IconClose size={13} />}
                  aria-label={`Remove prerequisite ${i + 1}`}
                  onClick={() => onChange(value.filter((_, k) => k !== i))}
                />
              </li>
            );
          })}
        </ul>
      )}

      {error ? (
        <p className="mf-bn-field__err" role="alert">
          {error}
        </p>
      ) : null}

      <div className="mf-fl-inline">
        <Button
          size="sm"
          icon={<IconPlus size={13} />}
          disabled={!unused}
          onClick={() => unused && onChange([...value, { flag: unused.key, enabled: true, variant: null }])}
        >
          Add a prerequisite
        </Button>
        {others.length === 0 ? (
          <span className="mf-bn-field__hint">This is the only flag on the site, so there is nothing to depend on yet.</span>
        ) : null}
      </div>
    </div>
  );
}

/** Problems a prerequisite list has before it is sent. The server checks loops. */
export function dependencyProblem(deps: FlagDependency[], selfKey: string): string | undefined {
  if (deps.some((d) => !d.flag)) return "Choose a flag for every prerequisite.";
  if (deps.some((d) => d.flag === selfKey)) return "A flag cannot depend on itself.";
  if (new Set(deps.map((d) => d.flag)).size !== deps.length)
    return "The same flag is listed twice. Keep one row per prerequisite.";
  return undefined;
}

/* ═══ environments ════════════════════════════════════════════════════════ */

export const ENVIRONMENT_PRESETS = ["production", "staging", "development"];

/** The names the server accepts: 1 to 32 of lower-case letters, digits, dash and underscore. */
export const ENVIRONMENT_RE = /^[a-z0-9_-]{1,32}$/;

const CUSTOM = "__custom";

export function EnvironmentEditor({
  value,
  onChange,
  mainEnabled,
  mainRollout,
  error,
}: {
  value: Record<string, FlagEnvironment>;
  onChange: (next: Record<string, FlagEnvironment>) => void;
  /** The flag's own serving switch and rollout, which an absent override falls back to. */
  mainEnabled: boolean;
  mainRollout: number;
  error?: string;
}) {
  const names = Object.keys(value);
  const free = ENVIRONMENT_PRESETS.filter((p) => !names.includes(p));
  const [choice, setChoice] = useState<string>(free[0] ?? CUSTOM);
  const [custom, setCustom] = useState("");

  // A preset already taken (added here, or by a revert) falls back to a typed name.
  const typing = choice === CUSTOM || !free.includes(choice);
  const picked = typing ? custom.trim() : choice;
  const nameProblem = !typing || !picked
    ? undefined
    : !ENVIRONMENT_RE.test(picked)
      ? "Use 1 to 32 lower-case letters, digits, dashes or underscores."
      : names.includes(picked)
        ? "This flag already has an override for that environment."
        : undefined;

  const put = (name: string, env: FlagEnvironment) => onChange({ ...value, [name]: env });

  const add = () => {
    if (!picked || nameProblem || names.includes(picked)) return;
    onChange({ ...value, [picked]: {} });
    setCustom("");
    const rest = free.filter((p) => p !== picked);
    setChoice(rest[0] ?? CUSTOM);
  };

  return (
    <div className="mf-fl-envs">
      {names.length === 0 ? (
        <p className="mf-bn-cond__none">
          No overrides. Every environment gets the main settings: {mainEnabled ? "serving" : "not serving"},{" "}
          {mainRollout}% rollout.
        </p>
      ) : (
        <ul className="mf-fl-envs__list">
          {names.map((name) => {
            const env = value[name] ?? {};
            const serving = env.enabled === undefined ? "main" : env.enabled ? "on" : "off";
            const ownRollout = env.rollout_pct !== undefined;
            return (
              <li key={name} className="mf-fl-envs__row">
                <span className="mf-fl-envs__name mf-data">{name}</span>
                <div className="mf-fl-envs__cell">
                  <span className="mf-bn-field__label">Serving</span>
                  <KeyBank
                    label={`Serving in ${name}`}
                    value={serving}
                    onChange={(v) =>
                      put(name, { ...env, enabled: v === "main" ? undefined : v === "on" })
                    }
                    options={[
                      { v: "main", label: `As main (${mainEnabled ? "on" : "off"})` },
                      { v: "on", label: "On" },
                      { v: "off", label: "Off" },
                    ]}
                  />
                </div>
                <div className="mf-fl-envs__cell">
                  <span className="mf-bn-field__label">Rollout</span>
                  <div className="mf-fl-inline">
                    <KeyBank
                      label={`Rollout in ${name}`}
                      value={ownRollout ? "own" : "main"}
                      onChange={(v) =>
                        put(name, { ...env, rollout_pct: v === "own" ? mainRollout : undefined })
                      }
                      options={[
                        { v: "main", label: `As main (${mainRollout}%)` },
                        { v: "own", label: "Its own" },
                      ]}
                    />
                    {ownRollout ? (
                      <NumberInput
                        value={env.rollout_pct ?? 0}
                        min={0}
                        max={100}
                        step={5}
                        suffix="%"
                        ariaLabel={`Rollout in ${name}`}
                        onChange={(v) =>
                          put(name, { ...env, rollout_pct: Math.max(0, Math.min(100, Math.round(v))) })
                        }
                      />
                    ) : null}
                  </div>
                </div>
                <Button
                  size="sm"
                  tone="quiet"
                  icon={<IconClose size={13} />}
                  aria-label={`Remove the ${name} override`}
                  onClick={() => {
                    const next = { ...value };
                    delete next[name];
                    onChange(next);
                  }}
                />
              </li>
            );
          })}
        </ul>
      )}

      {error ? (
        <p className="mf-bn-field__err" role="alert">
          {error}
        </p>
      ) : null}

      <div className="mf-fl-inline">
        <Select
          value={typing ? CUSTOM : choice}
          ariaLabel="Environment to add"
          onChange={setChoice}
        >
          {free.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
          <option value={CUSTOM}>Another name…</option>
        </Select>
        {typing ? (
          <TextInput
            value={custom}
            mono
            invalid={!!nameProblem}
            ariaLabel="Environment name"
            placeholder="preview"
            spellCheck={false}
            autoCapitalize="off"
            maxLength={32}
            onChange={(v) => setCustom(v.toLowerCase())}
          />
        ) : null}
        <Button size="sm" icon={<IconPlus size={13} />} disabled={!picked || !!nameProblem} onClick={add}>
          Add an override
        </Button>
      </div>
      {nameProblem ? (
        <p className="mf-bn-field__err" role="alert">
          {nameProblem}
        </p>
      ) : null}
    </div>
  );
}

/**
 * Keeps only the fields an override actually sets. An absent field means "as
 * the main setting", and the evaluator reads it that way; a stray `null` would
 * be read as a value.
 */
export function cleanEnvironments(envs: Record<string, FlagEnvironment>): Record<string, FlagEnvironment> {
  const out: Record<string, FlagEnvironment> = {};
  for (const [name, env] of Object.entries(envs)) {
    const kept: FlagEnvironment = {};
    if (env.enabled !== undefined) kept.enabled = env.enabled;
    if (env.rollout_pct !== undefined) kept.rollout_pct = Math.round(env.rollout_pct);
    out[name] = kept;
  }
  return out;
}
