/**
 * THE FLAG EDITOR.
 *
 * A flag is a switch your code reads at runtime, so this screen is built around
 * one instrument: THE ROLLOUT BAND. It draws the 0-100 bucket space as a ruled
 * track, fills the enrolled part, splits that fill by the weights you set, and
 * then draws a second track directly beneath it split by what actually landed.
 *
 * That pairing is deliberate. It is the same question the rest of Micaforge asks
 * about robots.txt: here is what you declared, here is what really happened. A
 * weight that reads 50/50 and lands 63/37 is a bug you can only see if the two
 * are drawn on the same axis.
 *
 * This file owns the flag model; `FlagsScreen` imports it.
 */
import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import type { BreakdownRow, Filter } from "../../lib/api";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconClose, IconCopy, IconPlus } from "../../components/react/icons";
import { num, pct } from "../../lib/format";
import { useAppState } from "../state";
import { useSite } from "../providers";
import {
  Field,
  FilterBuilder,
  NumberInput,
  Reading,
  TextArea,
  TextInput,
  Throw,
  errText,
  statsKey,
} from "./QueryScreen";

/* ═══ the flag model ══════════════════════════════════════════════════════ */

export interface FlagVariant {
  key: string;
  /** Relative weight. Weights need not sum to 100; the SDK normalises them. */
  weight: number;
}

export interface FeatureFlag {
  id: string;
  site_id: number;
  key: string;
  description: string;
  enabled: boolean;
  /** 0-100. A visitor is enrolled when their deterministic bucket falls below it. */
  rollout_pct: number;
  variants: FlagVariant[];
  /** Targeting, in the same condition shape every filter in the product uses. */
  conditions: Filter[];
  created_at?: string;
  updated_at?: string;
}

export const flagsKey = (siteId: number | undefined) => ["bench", "flags", siteId];

/** jsonb columns arrive as whatever was written; nothing here trusts the shape. */
export function normaliseFlag(raw: Partial<FeatureFlag> & Record<string, unknown>): FeatureFlag {
  const parse = <T,>(v: unknown, fallback: T): T => {
    if (v == null) return fallback;
    if (typeof v === "string") {
      try {
        return JSON.parse(v) as T;
      } catch {
        return fallback;
      }
    }
    return v as T;
  };
  const variants = parse<FlagVariant[]>(raw.variants, []);
  const conditions = parse<Filter[]>(raw.conditions, []);
  return {
    id: String(raw.id ?? ""),
    site_id: Number(raw.site_id ?? 0),
    key: String(raw.key ?? ""),
    description: String(raw.description ?? ""),
    enabled: !!raw.enabled,
    rollout_pct: Math.max(0, Math.min(100, Number(raw.rollout_pct ?? 0))),
    variants: Array.isArray(variants)
      ? variants.map((v) => ({ key: String(v?.key ?? ""), weight: Number(v?.weight ?? 0) || 0 }))
      : [],
    conditions: Array.isArray(conditions) ? conditions : [],
    created_at: raw.created_at as string | undefined,
    updated_at: raw.updated_at as string | undefined,
  };
}

export function blankFlag(siteId: number | undefined): FeatureFlag {
  return {
    id: "",
    site_id: siteId ?? 0,
    key: "",
    description: "",
    enabled: false,
    rollout_pct: 0,
    variants: [],
    conditions: [],
  };
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
  return out;
}

/* ═══ observed exposure ═══════════════════════════════════════════════════ */

export interface Observed {
  key: string;
  exposures: number;
  share: number;
}

/** Lenient matching: a boolean flag records `true`, a multivariate one its key. */
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
                  <span className="mf-bn-band__none">no exposures recorded in this interval</span>
                )}
              </div>
            </div>
            <span className="mf-bn-band__fig mf-data">{liveTotal > 0 ? num(liveTotal) : "—"}</span>
          </div>

          <p className="mf-bn-band__note">
            Both tracks are drawn across the enrolled share, so a boundary that does not line up is a
            split landing differently from the way it was set. Exposure counts are the flag values
            recorded on real events in the current interval.
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
                <span className="mf-bn-band__got">still being recorded</span>
                <span className="mf-bn-band__n mf-data">{num(r.exposures)}</span>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

/* ═══ the SDK snippet ═════════════════════════════════════════════════════ */

function snippetFor(f: FeatureFlag): string {
  const key = f.key || "your-flag-key";
  const lines = [
    'import { flags } from "@micaforge/sdk";',
    "",
    "// `visitor` is any durable id you already have. Without one, bucketing is",
    "// stable for a single page load only.",
    "const mf = flags({",
    "  visitor: userId,",
    "  fallback: {",
  ];
  const fb: string[] = [`    "${key}": { rolloutPct: ${Math.round(f.rollout_pct)}`];
  if (f.variants.length) {
    fb.push(
      `, variants: [${f.variants
        .map((v) => `{ key: "${v.key}", weight: ${v.weight} }`)
        .join(", ")}]`,
    );
  }
  fb.push(" },");
  lines.push(fb.join(""));
  lines.push("  },");
  lines.push("});");
  lines.push("");
  if (f.variants.length) {
    lines.push(`const variant = mf.variant("${key}", "${f.variants[0]?.key ?? "control"}");`);
    lines.push(`if (variant === "${f.variants[0]?.key ?? "control"}") {`);
    lines.push("  // …");
    lines.push("}");
  } else {
    lines.push(`if (mf.isEnabled("${key}")) {`);
    lines.push("  // …");
    lines.push("}");
  }
  return lines.join("\n");
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

export function FlagEditor({
  flag: initial,
  onDone,
}: {
  /** An existing flag, or `blankFlag(siteId)` for a new one. */
  flag: FeatureFlag;
  onDone: (result: "saved" | "deleted" | "cancelled") => void;
}) {
  const { siteId } = useSite();
  const { range, filters } = useAppState();
  const qc = useQueryClient();

  const [f, setF] = useState<FeatureFlag>(initial);
  const [showProblems, setShowProblems] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  useEffect(() => setF(initial), [initial]);

  const set = (p: Partial<FeatureFlag>) => setF((prev) => ({ ...prev, ...p }));
  const problems = flagProblems(f);
  const hasProblems = Object.keys(problems).length > 0;
  const isNew = !f.id;

  /* Exposures. A flag with no key yet has nothing to count. */
  const exposureDim = f.key.trim() ? `flag.${f.key.trim()}` : "";
  const exposures = useQuery({
    queryKey: statsKey("/api/stats/breakdown", siteId, range, "all", filters, { dimension: exposureDim }),
    enabled: siteId != null && !!exposureDim && !isNew,
    queryFn: () =>
      api.get<BreakdownRow[]>("/api/stats/breakdown", {
        site: siteId ?? 0,
        range,
        audience: "all",
        filters,
        dimension: exposureDim,
        limit: 20,
      }),
  });

  const observed = useMemo<Observed[]>(
    () =>
      (exposures.data?.data ?? []).map((r) => ({
        key: r.key,
        exposures: r.visitors || r.sessions || r.pageviews || 0,
        share: r.share ?? 0,
      })),
    [exposures.data],
  );

  const save = useMutation({
    mutationFn: () => {
      const body = {
        site_id: siteId,
        key: f.key.trim(),
        description: f.description.trim(),
        enabled: f.enabled,
        rollout_pct: Math.round(f.rollout_pct),
        variants: f.variants.map((v) => ({ key: v.key.trim(), weight: v.weight })),
        conditions: f.conditions,
      };
      return isNew
        ? api.post<FeatureFlag>("/api/flags", body)
        : api.patch<FeatureFlag>(`/api/flags/${f.id}`, body);
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
      onDone("saved");
    },
  });

  const remove = useMutation({
    mutationFn: () => api.del<void>(`/api/flags/${f.id}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
      onDone("deleted");
    },
  });

  const err = (field: string) => (showProblems ? problems[field] : undefined);

  return (
    <Panel className="mf-bn-editor">
      <PanelHead
        title={isNew ? "New flag" : f.key || "Flag"}
        note={
          isNew
            ? "The key is what your code reads. Everything else can change afterwards without a deploy."
            : "Changes take effect for new evaluations. The SDK caches an evaluated set for a minute by default."
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
            error={err("key")}
            hint="The string your code passes to isEnabled()."
            control={(id) => (
              <TextInput
                id={id}
                mono
                value={f.key}
                invalid={!!err("key")}
                placeholder="checkout-v2"
                onChange={(v) => set({ key: v })}
              />
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
            error={err("rollout")}
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

        {/* ── the instrument ───────────────────────────────────────────── */}
        <section className="mf-bn-editor__band" aria-label="Rollout">
          <h3 className="mf-bn-editor__h">Rollout and what it is landing as</h3>
          {isNew ? (
            <>
              <RolloutBand flag={f} observed={null} />
              <p className="mf-bn-editor__hint">
                Nothing is observed yet. Once the flag is saved and your code reads it, the second
                track fills in from real events.
              </p>
            </>
          ) : (
            <Reading
              loading={exposures.isLoading}
              error={exposures.isError ? exposures.error : undefined}
              onRetry={() => void exposures.refetch()}
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
          {err("variants") ? (
            <p className="mf-bn-field__err" role="alert">
              {problems.variants}
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

        {/* ── targeting ────────────────────────────────────────────────── */}
        <section className="mf-bn-editor__target" aria-label="Targeting">
          <h3 className="mf-bn-editor__h">Targeting</h3>
          <p className="mf-bn-editor__hint">
            A visitor must match every condition before the rollout is even considered. With no
            conditions, the rollout applies to everyone.
          </p>
          <FilterBuilder
            value={f.conditions}
            onChange={(conditions) => set({ conditions })}
            addLabel="Add a targeting condition"
            emptyNote="No targeting. Every visitor is a candidate for the rollout."
          />
        </section>

        {/* ── reading it from the SDK ──────────────────────────────────── */}
        <section className="mf-bn-editor__sdk" aria-label="Reading this flag">
          <h3 className="mf-bn-editor__h">Reading it from the SDK</h3>
          <Snippet code={snippetFor(f)} label="JavaScript / TypeScript" />
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
            The flag was not saved. {errText(save.error)}
          </p>
        ) : null}
        {remove.isError ? (
          <p className="mf-bn-field__err" role="alert">
            The flag was not deleted. {errText(remove.error)}
          </p>
        ) : null}
      </div>
    </Panel>
  );
}

export default FlagEditor;
