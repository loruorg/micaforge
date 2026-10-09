/**
 * THE FLAG EDITOR.
 *
 * A flag is a switch your code reads at runtime, so this screen is built around
 * one view: THE ROLLOUT BAND. It draws the 0-100 bucket space as a track, fills
 * the enrolled part, splits that fill by the weights you set, and then draws a
 * second track directly beneath it split by what actually landed.
 *
 * That pairing is deliberate. It is the same question the rest of Micaforge asks
 * about robots.txt: here is what you declared, here is what really happened. A
 * weight that reads 50/50 and lands 63/37 is a bug you can only see if the two
 * are drawn on the same axis. "What landed" is the exposures SDKs reported,
 * read from the same answer as the evaluations panel under the editor.
 *
 * Under the basics, the rule sections run in the order the evaluator applies
 * them, so reading the editor top to bottom is reading the decision: the
 * environment override, prerequisites, target groups, the flag's own
 * conditions, then the rollout and the variant it lands on.
 *
 * This file owns the flag model; `FlagsScreen` and the panels beside the
 * editor import it.
 */
import { useEffect, useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ApiError, api, publicOrigin, type Actor } from "../../lib/api";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconClose, IconCopy, IconPlus } from "../../components/react/icons";
import { ago, num, pct } from "../../lib/format";
import { useSite } from "../providers";
import {
  Field,
  NumberInput,
  Reading,
  TextArea,
  TextInput,
  Throw,
  WaitStrip,
  errText,
} from "./QueryScreen";
import {
  ConditionEditor,
  conditionProblem,
  describeConditions,
  emptyConditions,
  readConditions,
  writeConditions,
  type ConditionSet,
} from "./ConditionEditor";
import { DependencyEditor, EnvironmentEditor, cleanEnvironments, dependencyProblem } from "./FlagRules";
import { FlagStats, useFlagStats, type StatsRange } from "./FlagStats";
import { FlagHistory, actorText } from "./FlagHistory";
import { useTargetGroups } from "./TargetGroups";
import "../../styles/app/flags.css";

/* ═══ the flag model ══════════════════════════════════════════════════════ */

export interface FlagVariant {
  key: string;
  /** Relative weight. Weights need not sum to 100; the SDK normalises them. */
  weight: number;
}

/** "Only when <flag> is on (with variant <variant>)". */
export interface FlagDependency {
  flag: string;
  enabled: boolean;
  /** A variant the prerequisite must land on, or null for any. */
  variant: string | null;
}

/** An environment override. An absent field means "as the main setting". */
export interface FlagEnvironment {
  enabled?: boolean;
  rollout_pct?: number;
}

export interface FeatureFlag {
  id: string;
  site_id: string | number;
  key: string;
  /** A readable name for the list. The key is what code reads. */
  name: string;
  description: string;
  enabled: boolean;
  /** 0-100. A visitor is enrolled when their deterministic bucket falls below it. */
  rollout_pct: number;
  variants: FlagVariant[];
  /** The flag's own targeting conditions. */
  conditions: ConditionSet;
  /** Target group ids. A visitor must match at least one when any are listed. */
  target_groups: string[];
  dependencies: FlagDependency[];
  environments: Record<string, FlagEnvironment>;
  created_at?: string;
  updated_at?: string;
  updated_by?: Actor | null;
}

export const flagsKey = (siteId: number | undefined) => ["bench", "flags", siteId];

function parseJson<T>(v: unknown, fallback: T): T {
  if (v == null) return fallback;
  if (typeof v === "string") {
    try {
      return JSON.parse(v) as T;
    } catch {
      return fallback;
    }
  }
  return v as T;
}

/** jsonb columns arrive as whatever was written; nothing here trusts the shape. */
export function normaliseFlag(raw: Record<string, unknown>): FeatureFlag {
  const variants = parseJson<unknown>(raw.variants, []);
  const groups = parseJson<unknown>(raw.target_groups, []);
  const deps = parseJson<unknown>(raw.dependencies, []);
  const envs = parseJson<unknown>(raw.environments, {});

  const environments: Record<string, FlagEnvironment> = {};
  if (envs && typeof envs === "object" && !Array.isArray(envs)) {
    for (const [name, e] of Object.entries(envs as Record<string, Record<string, unknown> | null>)) {
      const env: FlagEnvironment = {};
      if (typeof e?.enabled === "boolean") env.enabled = e.enabled;
      if (typeof e?.rollout_pct === "number" && Number.isFinite(e.rollout_pct)) env.rollout_pct = e.rollout_pct;
      environments[name] = env;
    }
  }

  return {
    id: String(raw.id ?? ""),
    site_id: (raw.site_id as string | number | undefined) ?? "",
    key: String(raw.key ?? ""),
    name: String(raw.name ?? ""),
    description: String(raw.description ?? ""),
    enabled: !!raw.enabled,
    rollout_pct: Math.max(0, Math.min(100, Number(raw.rollout_pct ?? 0) || 0)),
    variants: Array.isArray(variants)
      ? variants.map((v: { key?: unknown; weight?: unknown }) => ({
          key: String(v?.key ?? ""),
          weight: Number(v?.weight ?? 0) || 0,
        }))
      : [],
    conditions: readConditions(raw.conditions),
    target_groups: Array.isArray(groups) ? groups.map(String) : [],
    dependencies: Array.isArray(deps)
      ? deps.map((d: { flag?: unknown; enabled?: unknown; variant?: unknown }) => ({
          flag: String(d?.flag ?? ""),
          enabled: d?.enabled !== false,
          variant: typeof d?.variant === "string" && d.variant ? d.variant : null,
        }))
      : [],
    environments,
    created_at: raw.created_at as string | undefined,
    updated_at: raw.updated_at as string | undefined,
    updated_by: (raw.updated_by as Actor | null | undefined) ?? null,
  };
}

export function blankFlag(siteId: number | undefined): FeatureFlag {
  return {
    id: "",
    site_id: siteId ?? "",
    key: "",
    name: "",
    description: "",
    enabled: false,
    rollout_pct: 0,
    variants: [],
    conditions: emptyConditions(),
    target_groups: [],
    dependencies: [],
    environments: {},
  };
}

/** A new flag pre-filled from a template's partial flag. The key is still yours to choose. */
export function flagFromTemplate(partial: Record<string, unknown>, siteId: number | undefined): FeatureFlag {
  const blank = blankFlag(siteId);
  const merged = normaliseFlag({ ...blank, conditions: [], ...partial, id: "" });
  return { ...merged, site_id: blank.site_id };
}

/** A boolean flag still has one branch worth drawing: the enrolled side. */
export function effectiveVariants(f: FeatureFlag): FlagVariant[] {
  return f.variants.length ? f.variants : [{ key: "on", weight: 100 }];
}

export const totalWeight = (vs: FlagVariant[]): number =>
  vs.reduce((s, v) => s + (v.weight > 0 ? v.weight : 0), 0);

const KEY_RE = /^[a-z0-9][a-z0-9._-]*$/;

export function flagProblems(f: FeatureFlag): Record<string, string> {
  const out: Record<string, string> = {};
  if (!f.key.trim()) out.key = "A flag needs a key. It is the string your code reads.";
  else if (!KEY_RE.test(f.key.trim()))
    out.key =
      "Use lower-case letters, numbers, dot, dash or underscore, starting with a letter or number.";
  if (!(f.rollout_pct >= 0 && f.rollout_pct <= 100)) out.rollout = "The rollout is a share from 0 to 100.";
  const keys = f.variants.map((v) => v.key.trim());
  if (keys.some((k) => !k)) out.variants = "Every variant needs a key.";
  else if (new Set(keys).size !== keys.length) out.variants = "Two variants share a key. Each must be unique.";
  else if (f.variants.length && totalWeight(f.variants) <= 0)
    out.variants = "At least one variant needs a weight above zero, or nothing can be served.";
  const cond = conditionProblem(f.conditions);
  if (cond) out.conditions = cond;
  const dep = dependencyProblem(f.dependencies, f.key.trim());
  if (dep) out.dependencies = dep;
  return out;
}

/* ═══ observed exposure ═══════════════════════════════════════════════════ */

export interface Observed {
  key: string;
  exposures: number;
  share: number;
}

/** Lenient matching: a boolean flag reports `on`, older events `true` or `1`. */
function matchObserved(variantKey: string, rows: Observed[]): Observed | undefined {
  return rows.find(
    (r) =>
      r.key === variantKey ||
      (variantKey === "on" && (r.key === "true" || r.key === "1" || r.key === "on")),
  );
}

const isOffRow = (k: string) => k === "false" || k === "0" || k === "" || k === "off";

/* ═══ the rollout band ════════════════════════════════════════════════════ */

const RAMP = ["var(--mf-s1)", "var(--mf-s2)", "var(--mf-s3)", "var(--mf-s4)", "var(--mf-s5)"];

/**
 * The tone a variant is drawn in, everywhere it is drawn: the band, the
 * evaluations chart and its table. "off" is grey because it is the absence of
 * a variant, not one of them.
 */
export function variantTone(f: FeatureFlag, variant: string): string {
  if (variant === "off") return "var(--mf-ink-3)";
  const i = effectiveVariants(f).findIndex((v) => v.key === variant);
  return i === -1 ? "var(--mf-ink-2)" : RAMP[i % RAMP.length];
}

export function RolloutBand({
  flag,
  observed,
  compact: isCompact,
}: {
  flag: FeatureFlag;
  observed?: Observed[] | null;
  compact?: boolean;
}) {
  const vs = effectiveVariants(flag);
  const total = totalWeight(vs) || 1;
  const enrolled = Math.max(0, Math.min(100, flag.rollout_pct));

  const setSplit = vs.map((v, i) => ({
    key: v.key,
    frac: Math.max(0, v.weight) / total,
    tone: RAMP[i % RAMP.length],
  }));

  const live = (observed ?? []).filter((r) => !isOffRow(r.key));
  const liveTotal = live.reduce((s, r) => s + r.exposures, 0);
  const liveSplit = vs.map((v, i) => {
    const m = matchObserved(v.key, live);
    return {
      key: v.key,
      exposures: m?.exposures ?? 0,
      frac: liveTotal > 0 ? (m?.exposures ?? 0) / liveTotal : 0,
      tone: RAMP[i % RAMP.length],
    };
  });

  const stray = live.filter((r) => !vs.some((v) => matchObserved(v.key, [r])));

  const setLabel = setSplit.map((s) => `${s.key} ${pct(s.frac)}`).join(", ");
  const liveLabel =
    liveTotal > 0 ? liveSplit.map((s) => `${s.key} ${pct(s.frac)}`).join(", ") : "nothing recorded yet";

  return (
    <div className={`mf-bn-band${isCompact ? " mf-bn-band--compact" : ""}`}>
      <div className="mf-bn-band__row">
        <span className="mf-bn-band__cap">Set</span>
        <div
          className="mf-bn-band__track"
          role="img"
          aria-label={`Set: ${enrolled} per cent enrolled, split ${setLabel}`}
        >
          <div className="mf-bn-band__in" style={{ width: `${enrolled}%` }}>
            {setSplit.map((s) => (
              <span
                key={s.key}
                className="mf-bn-band__seg"
                style={{ flexGrow: Math.max(0.0001, s.frac), background: s.tone }}
              />
            ))}
          </div>
        </div>
        <span className="mf-bn-band__fig mf-data">{enrolled}%</span>
      </div>

      <div className="mf-bn-band__scale" aria-hidden="true">
        <span>0</span>
        <span>50</span>
        <span>100</span>
      </div>

      {isCompact ? null : (
        <>
          <div className="mf-bn-band__row">
            <span className="mf-bn-band__cap">Observed</span>
            <div
              className="mf-bn-band__track"
              role="img"
              aria-label={`Observed split of exposures: ${liveLabel}`}
            >
              <div className="mf-bn-band__in mf-bn-band__in--live" style={{ width: `${enrolled}%` }}>
                {liveTotal > 0 ? (
                  liveSplit.map((s) => (
                    <span
                      key={s.key}
                      className="mf-bn-band__seg"
                      style={{ flexGrow: Math.max(0.0001, s.frac), background: s.tone }}
                    />
                  ))
                ) : (
                  <span className="mf-bn-band__none">no exposures reported in this period</span>
                )}
              </div>
            </div>
            <span className="mf-bn-band__fig mf-data">{liveTotal > 0 ? num(liveTotal) : "—"}</span>
          </div>

          <p className="mf-bn-band__note">
            Both tracks are drawn across the enrolled share, so a boundary that does not line up is a
            split landing differently from the way it was set. Exposure counts are what SDKs reported
            for this flag over the period chosen under Evaluations and exposures.
          </p>

          <ul className="mf-bn-band__legend">
            {vs.map((v, i) => {
              const l = liveSplit[i];
              return (
                <li key={v.key}>
                  <span className="mf-bn-band__mark" style={{ background: RAMP[i % RAMP.length] }} aria-hidden="true" />
                  <span className="mf-bn-band__name mf-data">{v.key}</span>
                  <span className="mf-bn-band__set">set {pct(setSplit[i].frac)}</span>
                  <span className="mf-bn-band__got">
                    {liveTotal > 0 ? `observed ${pct(l.frac)}` : "no exposures yet"}
                  </span>
                  <span className="mf-bn-band__n mf-data">{num(l.exposures)}</span>
                </li>
              );
            })}
            {stray.map((r) => (
              <li key={`stray-${r.key}`} className="mf-bn-band__stray">
                <span className="mf-bn-band__mark mf-bn-band__mark--stray" aria-hidden="true" />
                <span className="mf-bn-band__name mf-data">{r.key || "(empty)"}</span>
                <span className="mf-bn-band__set">not configured on this flag</span>
                <span className="mf-bn-band__got">still being reported</span>
                <span className="mf-bn-band__n mf-data">{num(r.exposures)}</span>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

/* ═══ the SDK snippets ════════════════════════════════════════════════════ */

const IDENT_RE = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

/** The attributes this flag's conditions read, so the snippet passes the right context. */
function contextAttributes(f: FeatureFlag): { name: string; example: string }[] {
  const seen = new Map<string, string>();
  for (const l of f.conditions.leaves) {
    const name = l.dimension.trim();
    if (!name || seen.has(name)) continue;
    seen.set(name, l.op === "is" ? (l.value.split(",")[0] ?? "").trim() || "…" : "…");
  }
  if (!seen.size) seen.set("plan", "pro");
  return [...seen].map(([name, example]) => ({ name, example }));
}

function browserSnippet(f: FeatureFlag, environment: string): string {
  const key = f.key || "your-flag-key";
  const ctx = contextAttributes(f)
    .map((a) => `${IDENT_RE.test(a.name) ? a.name : JSON.stringify(a.name)}: ${JSON.stringify(a.example)}`)
    .join(", ");
  const lines = [
    'import { flags } from "@micaforge/sdk";',
    "",
    "// `visitor` is any durable id you already have; without one, bucketing is",
    "// stable for a single page load only. `environment` picks this flag's",
    "// override for that environment, and `context` is what targeting reads.",
    "const mf = flags({",
    "  visitor: userId,",
    `  environment: ${JSON.stringify(environment)},`,
    `  context: { ${ctx} },`,
    "});",
    "",
  ];
  if (f.variants.length) {
    const first = f.variants[0]?.key || "control";
    lines.push(`const variant = mf.variant(${JSON.stringify(key)}, ${JSON.stringify(first)});`);
    lines.push(`if (variant === ${JSON.stringify(first)}) {`);
  } else {
    lines.push(`if (mf.isEnabled(${JSON.stringify(key)})) {`);
  }
  lines.push("  // …", "}");
  return lines.join("\n");
}

function serverSnippet(f: FeatureFlag, environment: string, site: number | undefined): string {
  const key = f.key || "your-flag-key";
  const ctx = contextAttributes(f)
    .map((a) => (IDENT_RE.test(a.name) ? a.name : `${JSON.stringify(a.name)}: "…"`))
    .join(", ");
  return [
    'import { createFlagsClient } from "@micaforge/sdk-server";',
    "",
    "// Needs an API key with the manage:read scope. The client downloads this",
    "// site's rules once and evaluates every flag in your process.",
    "const flags = createFlagsClient({",
    `  host: ${JSON.stringify(publicOrigin() || "https://analytics.example.com")},`,
    "  apiKey: process.env.MICAFORGE_API_KEY,",
    `  site: ${site ?? 0},`,
    `  environment: ${JSON.stringify(environment)},`,
    "});",
    "await flags.ready();",
    "",
    `if (flags.isEnabled(${JSON.stringify(key)}, { visitor: userId, context: { ${ctx} } })) {`,
    "  // …",
    "}",
  ].join("\n");
}

function Snippet({ code, label }: { code: string; label: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(t);
  }, [copied]);
  return (
    <div className="mf-bn-snip">
      <div className="mf-bn-snip__head">
        <span className="mf-bn-snip__label">{label}</span>
        <Button
          size="sm"
          icon={copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
          onClick={() => {
            navigator.clipboard
              ?.writeText(code)
              .then(() => setCopied(true))
              .catch(() => {
                /* clipboard blocked: the snippet below is selectable */
              });
          }}
        >
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
      <pre className="mf-bn-snip__code mf-data">
        <code>{code}</code>
      </pre>
    </div>
  );
}

/* ═══ the editor ══════════════════════════════════════════════════════════ */

/** The server's message for a field it refused, when it named one in this section. */
function refusal(err: unknown, ...fields: string[]): string | undefined {
  if (!(err instanceof ApiError) || !err.field) return undefined;
  const f = err.field;
  return fields.some((name) => f === name || f.startsWith(`${name}.`) || f.startsWith(`${name}[`))
    ? err.message
    : undefined;
}

const SECTION_FIELDS = [
  "key",
  "name",
  "description",
  "rollout_pct",
  "variants",
  "conditions",
  "target_groups",
  "dependencies",
  "environments",
];

export function FlagEditor({
  flag: initial,
  template,
  allFlags,
  onDone,
}: {
  /** An existing flag, `blankFlag(siteId)`, or a template's pre-filled flag. */
  flag: FeatureFlag;
  /** The template the new flag started from, sent with the create call. */
  template?: string | null;
  /** Every flag on the site, for choosing prerequisites. */
  allFlags: FeatureFlag[];
  onDone: (result: "saved" | "deleted" | "cancelled") => void;
}) {
  const { siteId } = useSite();
  const qc = useQueryClient();
  const groups = useTargetGroups(siteId);

  const [f, setF] = useState<FeatureFlag>(initial);
  const [showProblems, setShowProblems] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [statsRange, setStatsRange] = useState<StatsRange>("7d");

  useEffect(() => setF(initial), [initial]);

  const set = (p: Partial<FeatureFlag>) => setF((prev) => ({ ...prev, ...p }));
  const problems = flagProblems(f);
  const hasProblems = Object.keys(problems).length > 0;
  const isNew = !f.id;

  /* What SDKs reported. A flag that does not exist yet has nothing to count. */
  const stats = useFlagStats(isNew ? "" : f.id, statsRange);
  const observed = useMemo<Observed[]>(() => {
    const totals = stats.data?.totals ?? [];
    const sum = totals.reduce((s, t) => s + (isOffRow(t.variant) ? 0 : t.exposures), 0);
    return totals.map((t) => ({
      key: t.variant,
      exposures: t.exposures,
      share: sum > 0 && !isOffRow(t.variant) ? t.exposures / sum : 0,
    }));
  }, [stats.data]);

  const groupList = groups.data ?? [];
  const groupName = (id: string) => groupList.find((g) => g.id === id)?.name ?? `a deleted group (${id.slice(0, 8)})`;
  const environment = Object.keys(f.environments)[0] ?? "production";

  const save = useMutation({
    mutationFn: () => {
      const body = {
        key: f.key.trim(),
        name: f.name.trim(),
        description: f.description.trim(),
        enabled: f.enabled,
        rollout_pct: Math.round(f.rollout_pct),
        variants: f.variants.map((v) => ({ key: v.key.trim(), weight: v.weight })),
        conditions: writeConditions(f.conditions),
        target_groups: f.target_groups,
        dependencies: f.dependencies.map((d) => ({
          flag: d.flag,
          enabled: d.enabled,
          variant: d.enabled && d.variant ? d.variant : null,
        })),
        environments: cleanEnvironments(f.environments),
        ...(isNew && template ? { template } : {}),
      };
      return isNew
        ? api.post<FeatureFlag>(`/api/flags?site=${encodeURIComponent(String(siteId ?? ""))}`, body)
        : api.patch<FeatureFlag>(`/api/flags/${f.id}`, body);
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
      void qc.invalidateQueries({ queryKey: ["bench", "flag-groups", siteId] });
      void qc.invalidateQueries({ queryKey: ["bench", "flag-history", f.id] });
      onDone("saved");
    },
  });

  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: string }>(`/api/flags/${f.id}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
      void qc.invalidateQueries({ queryKey: ["bench", "flag-groups", siteId] });
      onDone("deleted");
    },
  });

  const err = (field: string) => (showProblems ? problems[field] : undefined);
  const placed = refusal(save.error, ...SECTION_FIELDS) !== undefined;

  return (
    <>
      <Panel className="mf-bn-editor">
        <PanelHead
          title={isNew ? "New flag" : f.name || f.key || "Flag"}
          note={
            isNew
              ? "The key is what your code reads. Everything else can change afterwards without a deploy."
              : `Changes take effect for new evaluations; SDKs keep an evaluated set for about a minute.${
                  f.updated_at ? ` Last changed ${ago(f.updated_at)}${f.updated_by ? ` by ${actorText(f.updated_by)}` : ""}.` : ""
                }`
          }
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
              error={err("key") ?? refusal(save.error, "key")}
              hint="The string your code passes to isEnabled()."
              control={(id) => (
                <TextInput
                  id={id}
                  mono
                  value={f.key}
                  invalid={!!err("key")}
                  placeholder="checkout-v2"
                  spellCheck={false}
                  autoCapitalize="off"
                  onChange={(v) => set({ key: v })}
                />
              )}
            />
            <Field
              label="Name"
              error={refusal(save.error, "name")}
              hint="Optional. How the flag reads in this list."
              control={(id) => (
                <TextInput id={id} value={f.name} placeholder="New checkout" onChange={(v) => set({ name: v })} />
              )}
            />
            <Field
              label="Serving"
              hint="Off means every evaluation returns false, whatever the rollout says."
              control={(id) => (
                <Throw id={id} label="Serving" on={f.enabled} onChange={(v) => set({ enabled: v })} />
              )}
            />
            <Field
              label="Rollout"
              error={err("rollout") ?? refusal(save.error, "rollout_pct")}
              hint="The share of visitors enrolled, by deterministic bucket."
              control={(id) => (
                <NumberInput
                  id={id}
                  value={f.rollout_pct}
                  min={0}
                  max={100}
                  step={5}
                  suffix="%"
                  onChange={(v) => set({ rollout_pct: Math.max(0, Math.min(100, Math.round(v))) })}
                />
              )}
            />
          </div>

          <div className="mf-bn-build__row">
            <Field
              label="What this flag is for"
              wide
              error={refusal(save.error, "description")}
              hint="Written for whoever finds it in six months and wonders whether it can be deleted."
              control={(id) => (
                <TextArea
                  id={id}
                  value={f.description}
                  rows={2}
                  placeholder="New checkout, behind a flag until the payment migration lands."
                  onChange={(v) => set({ description: v })}
                />
              )}
            />
          </div>

          {/* ── environments ─────────────────────────────────────────────── */}
          <section className="mf-bn-editor__target" aria-label="Environments">
            <h3 className="mf-bn-editor__h">Environments</h3>
            <p className="mf-bn-editor__hint">
              SDKs pass an environment name, such as production or staging, with each evaluation. When
              this flag has an override for that name, the override's serving switch and rollout replace
              the main ones above. An SDK that sends no environment, or one this flag has no override
              for, gets the main settings.
            </p>
            <EnvironmentEditor
              value={f.environments}
              onChange={(environments) => set({ environments })}
              mainEnabled={f.enabled}
              mainRollout={Math.round(f.rollout_pct)}
              error={refusal(save.error, "environments")}
            />
          </section>

          {/* ── prerequisites ────────────────────────────────────────────── */}
          <section className="mf-bn-editor__target" aria-label="Prerequisites">
            <h3 className="mf-bn-editor__h">Prerequisites</h3>
            <p className="mf-bn-editor__hint">
              Checked before targeting. When a prerequisite does not evaluate the way it is set here, this flag is
              off for that visitor whatever its own settings say. A chain that loops back to this flag
              is refused when you save.
            </p>
            <DependencyEditor
              value={f.dependencies}
              onChange={(dependencies) => set({ dependencies })}
              flags={allFlags.map((x) => ({ key: x.key, variants: x.variants }))}
              selfKey={isNew ? f.key.trim() : initial.key}
              error={err("dependencies") ?? refusal(save.error, "dependencies")}
            />
          </section>

          {/* ── targeting ────────────────────────────────────────────────── */}
          <section className="mf-bn-editor__target" aria-label="Targeting">
            <h3 className="mf-bn-editor__h">Targeting</h3>
            <p className="mf-bn-editor__hint">
              When target groups are chosen, a visitor must be in at least one of them. The flag's own
              conditions must match as well. The rollout then applies to whoever is left; with no groups
              and no conditions, it applies to everyone.
            </p>

            <h4 className="mf-fl-sub">Target groups</h4>
            {groups.isPending ? (
              <WaitStrip lines={2} label="Loading the target groups…" />
            ) : groups.isError ? (
              <div className="mf-fl-inline">
                <p className="mf-bn-field__err" role="alert">
                  Could not load this site's target groups. {errText(groups.error)}
                </p>
                <Button size="sm" onClick={() => void groups.refetch()}>
                  Try again
                </Button>
              </div>
            ) : groupList.length === 0 && f.target_groups.length === 0 ? (
              <p className="mf-bn-cond__none">
                This site has no target groups yet. Create one in the Target groups panel on this screen,
                then choose it here.
              </p>
            ) : (
              <ul className="mf-fl-picks">
                {groupList.map((g) => {
                  const on = f.target_groups.includes(g.id);
                  return (
                    <li key={g.id} className="mf-fl-picks__row">
                      <label className="mf-bn-check">
                        <input
                          type="checkbox"
                          checked={on}
                          onChange={() =>
                            set({
                              target_groups: on
                                ? f.target_groups.filter((id) => id !== g.id)
                                : [...f.target_groups, g.id],
                            })
                          }
                        />
                        {g.name}
                      </label>
                      <span className="mf-fl-picks__rule mf-data">{describeConditions(g.conditions)}</span>
                    </li>
                  );
                })}
                {f.target_groups
                  .filter((id) => !groupList.some((g) => g.id === id))
                  .map((id) => (
                    <li key={id} className="mf-fl-picks__row">
                      <label className="mf-bn-check">
                        <input
                          type="checkbox"
                          checked
                          onChange={() => set({ target_groups: f.target_groups.filter((x) => x !== id) })}
                        />
                        {groupName(id)}
                      </label>
                      <span className="mf-fl-picks__rule">No longer exists. Clear it and save.</span>
                    </li>
                  ))}
              </ul>
            )}
            {refusal(save.error, "target_groups") ? (
              <p className="mf-bn-field__err" role="alert">
                {refusal(save.error, "target_groups")}
              </p>
            ) : null}

            <h4 className="mf-fl-sub">This flag's own conditions</h4>
            <ConditionEditor
              value={f.conditions}
              onChange={(conditions) => set({ conditions })}
              addLabel="Add a targeting condition"
              emptyNote="No conditions of its own."
              error={err("conditions") ?? refusal(save.error, "conditions")}
            />
          </section>

          {/* ── the rollout band ─────────────────────────────────────────── */}
          <section className="mf-bn-editor__band" aria-label="Rollout">
            <h3 className="mf-bn-editor__h">Rollout and what it is landing as</h3>
            {isNew ? (
              <>
                <RolloutBand flag={f} observed={null} />
                <p className="mf-bn-editor__hint">
                  Nothing is observed yet. Once the flag is saved and SDKs report exposures, the second
                  track fills in.
                </p>
              </>
            ) : (
              <Reading
                loading={stats.isPending}
                error={stats.isError ? stats.error : undefined}
                onRetry={() => void stats.refetch()}
                lines={2}
              >
                <RolloutBand flag={f} observed={observed} />
              </Reading>
            )}
          </section>

          {/* ── variants ─────────────────────────────────────────────────── */}
          <section className="mf-bn-editor__variants" aria-label="Variants">
            <h3 className="mf-bn-editor__h">Variants</h3>
            <p className="mf-bn-editor__hint">
              {f.variants.length
                ? "Weights are relative; they do not have to add up to 100. The SDK buckets deterministically, so a visitor keeps the same variant."
                : "No variants: the flag is on or off. Add two or more to split the enrolled share between them."}
            </p>
            {f.variants.length ? (
              <ul className="mf-bn-vars">
                {f.variants.map((v, i) => {
                  const share = totalWeight(f.variants) > 0 ? Math.max(0, v.weight) / totalWeight(f.variants) : 0;
                  return (
                    <li key={i} className="mf-bn-vars__row">
                      <span className="mf-bn-vars__mark" style={{ background: RAMP[i % RAMP.length] }} aria-hidden="true" />
                      <TextInput
                        value={v.key}
                        mono
                        ariaLabel={`Variant ${i + 1} key`}
                        placeholder="treatment"
                        onChange={(nv) =>
                          set({ variants: f.variants.map((x, k) => (k === i ? { ...x, key: nv } : x)) })
                        }
                      />
                      <NumberInput
                        value={v.weight}
                        min={0}
                        max={1000}
                        ariaLabel={`Variant ${i + 1} weight`}
                        onChange={(nv) =>
                          set({
                            variants: f.variants.map((x, k) =>
                              k === i ? { ...x, weight: Math.max(0, Math.round(nv)) } : x,
                            ),
                          })
                        }
                      />
                      <span className="mf-bn-vars__share mf-data">{pct(share)}</span>
                      <Button
                        size="sm"
                        tone="quiet"
                        icon={<IconClose size={13} />}
                        aria-label={`Remove variant ${v.key || i + 1}`}
                        onClick={() => set({ variants: f.variants.filter((_, k) => k !== i) })}
                      />
                    </li>
                  );
                })}
              </ul>
            ) : null}
            {err("variants") ?? refusal(save.error, "variants") ? (
              <p className="mf-bn-field__err" role="alert">
                {err("variants") ?? refusal(save.error, "variants")}
              </p>
            ) : null}
            <Button
              size="sm"
              icon={<IconPlus size={13} />}
              onClick={() =>
                set({
                  variants: [
                    ...f.variants,
                    { key: f.variants.length === 0 ? "control" : `variant-${f.variants.length + 1}`, weight: 50 },
                  ],
                })
              }
            >
              Add a variant
            </Button>
          </section>

          {/* ── reading it from the SDKs ─────────────────────────────────── */}
          <section className="mf-bn-editor__sdk" aria-label="Reading this flag">
            <h3 className="mf-bn-editor__h">Reading it from your code</h3>
            <div className="mf-fl-snips">
              <Snippet code={browserSnippet(f, environment)} label="In the browser: @micaforge/sdk" />
              <Snippet code={serverSnippet(f, environment, siteId)} label="On a Node server: @micaforge/sdk-server" />
            </div>
          </section>
        </div>

        <div className="mf-bn-editor__foot">
          <Button
            tone="primary"
            disabled={save.isPending}
            onClick={() => {
              setShowProblems(true);
              if (!hasProblems) save.mutate();
            }}
          >
            {save.isPending ? "Saving…" : isNew ? "Create the flag" : "Save the flag"}
          </Button>
          <Button tone="quiet" onClick={() => onDone("cancelled")}>
            Cancel
          </Button>

          {!isNew ? (
            <span className="mf-bn-editor__danger">
              {confirmDelete ? (
                <>
                  <span className="mf-bn-editor__ask">
                    Delete this flag? Code still reading it will get false.
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
                  Delete this flag
                </Button>
              )}
            </span>
          ) : null}

          {showProblems && hasProblems ? (
            <p className="mf-bn-field__err" role="alert">
              {Object.values(problems)[0]}
            </p>
          ) : null}
          {save.isError ? (
            <p className="mf-bn-field__err" role="alert">
              The flag was not saved.{" "}
              {placed ? "The reason is shown in the section it concerns." : errText(save.error)}
            </p>
          ) : null}
          {remove.isError ? (
            <p className="mf-bn-field__err" role="alert">
              The flag was not deleted. {errText(remove.error)}
            </p>
          ) : null}
        </div>
      </Panel>

      {isNew ? null : (
        <>
          <FlagStats
            query={stats}
            range={statsRange}
            onRange={setStatsRange}
            order={effectiveVariants(f).map((v) => v.key)}
            toneOf={(v) => variantTone(f, v)}
          />
          <FlagHistory
            flagId={f.id}
            groupName={groupName}
            onReverted={(raw) => setF(normaliseFlag(raw))}
          />
        </>
      )}
    </>
  );
}

export default FlagEditor;
