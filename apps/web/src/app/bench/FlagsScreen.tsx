/**
 * FEATURE FLAGS: the index.
 *
 * The row is the useful unit here: the key your code reads, what the flag is
 * for, whether it is serving, and how far the rollout has actually been pushed,
 * drawn as the same band the editor uses so the two never disagree. Opening a
 * flag brings the editor up underneath, keeping the list in view.
 *
 * A new flag starts from a template or from nothing. A template only fills in
 * the editor; nothing is created until "Create the flag", and the template's
 * name travels with that call so the server can fill any field left unset the
 * same way. The site's target groups are managed in their own panel at the
 * foot of the screen, because they belong to the site rather than to one flag.
 */
import { useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { Button, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconPlus, IconSearch, IconSettings } from "../../components/react/icons";
import { ago, num } from "../../lib/format";
import { useSite } from "../providers";
import { Reading, TextInput, Throw, errText } from "./QueryScreen";
import {
  FlagEditor,
  RolloutBand,
  blankFlag,
  effectiveVariants,
  flagFromTemplate,
  flagsKey,
  normaliseFlag,
} from "./FlagEditor";
import type { FeatureFlag } from "./FlagEditor";
import { conditionCount, readConditions } from "./ConditionEditor";
import { TargetGroupsPanel } from "./TargetGroups";
import "../../styles/app/flags.css";

/** `GET /api/flags/templates`: a starting point, never a flag on its own. */
interface FlagTemplate {
  id: string;
  name: string;
  description: string;
  flag: Record<string, unknown>;
}

/** What a template sets, in a line: "starts off · 10% rollout · variants control, treatment". */
function templateSummary(t: FlagTemplate): string {
  const f = t.flag;
  const parts: string[] = [];
  if (typeof f.enabled === "boolean") parts.push(f.enabled ? "starts serving" : "starts off");
  if (typeof f.rollout_pct === "number") parts.push(`${f.rollout_pct}% rollout`);
  if (Array.isArray(f.variants) && f.variants.length)
    parts.push(`variants ${f.variants.map((v: { key?: unknown }) => String(v?.key ?? "?")).join(", ")}`);
  const conds = conditionCount(readConditions(f.conditions));
  if (conds) parts.push(`${conds} targeting condition${conds === 1 ? "" : "s"}`);
  return parts.join(" · ");
}

/** One line of what a flag does beyond its rollout, for its row in the index. */
function flagMeta(f: FeatureFlag): string {
  const vs = effectiveVariants(f);
  const parts = [f.variants.length ? `${f.variants.length} variants: ${vs.map((v) => v.key).join(", ")}` : "On or off"];
  const groups = f.target_groups.length;
  const conds = conditionCount(f.conditions);
  if (!groups && !conds) parts.push("no targeting");
  if (groups) parts.push(`${groups} target group${groups === 1 ? "" : "s"}`);
  if (conds) parts.push(`${conds} targeting condition${conds === 1 ? "" : "s"}`);
  if (f.dependencies.length) parts.push(`needs ${f.dependencies.map((d) => d.flag).join(", ")}`);
  const envs = Object.keys(f.environments);
  if (envs.length) parts.push(`overrides for ${envs.join(", ")}`);
  if (f.updated_at) parts.push(`changed ${ago(f.updated_at)}`);
  return parts.join(" · ");
}

function TemplateChooser({
  onPick,
  onCancel,
}: {
  onPick: (flag: Record<string, unknown>, template: string | null) => void;
  onCancel: () => void;
}) {
  const templates = useQuery({
    queryKey: ["bench", "flag-templates"],
    queryFn: () => api.get<FlagTemplate[]>("/api/flags/templates").then((r) => r.data ?? []),
    staleTime: 60 * 60_000,
  });

  return (
    <Panel className="mf-bn-editor">
      <PanelHead
        title="Start a new flag"
        note="A template fills in the editor with a common setup. Nothing is created until you save, and every field can be changed first."
        actions={
          <Button tone="quiet" onClick={onCancel}>
            Close
          </Button>
        }
      />
      <ul className="mf-fl-templates">
        <li className="mf-fl-templates__row">
          <div className="mf-fl-templates__text">
            <span className="mf-fl-templates__name">Start from nothing</span>
            <p className="mf-fl-templates__desc">An off flag with no rollout, no variants and no targeting.</p>
          </div>
          <Button tone="primary" onClick={() => onPick({}, null)}>
            Start from nothing
          </Button>
        </li>
      </ul>
      <div className="mf-bn-indexwrap">
        <Reading
          loading={templates.isPending}
          error={templates.isError ? templates.error : undefined}
          onRetry={() => void templates.refetch()}
          isEmpty={(templates.data ?? []).length === 0}
          lines={3}
          empty={
            <p className="mf-fl-say">This server offers no templates. Start from nothing instead.</p>
          }
        >
          <ul className="mf-fl-templates">
            {(templates.data ?? []).map((t) => {
              const summary = templateSummary(t);
              return (
                <li key={t.id} className="mf-fl-templates__row">
                  <div className="mf-fl-templates__text">
                    <span className="mf-fl-templates__name">{t.name}</span>
                    {t.description ? <p className="mf-fl-templates__desc">{t.description}</p> : null}
                    {summary ? <p className="mf-fl-templates__meta">{summary}</p> : null}
                  </div>
                  <Button onClick={() => onPick(t.flag ?? {}, t.id)}>Use this template</Button>
                </li>
              );
            })}
          </ul>
        </Reading>
      </div>
    </Panel>
  );
}

export function FlagsScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const qc = useQueryClient();

  const [open, setOpen] = useState<FeatureFlag | null>(null);
  const [template, setTemplate] = useState<string | null>(null);
  const [choosing, setChoosing] = useState(false);
  const [find, setFind] = useState("");

  const q = useQuery({
    queryKey: flagsKey(siteId),
    enabled: siteId != null,
    queryFn: async () => {
      const res = await api.get<Record<string, unknown>[]>("/api/flags", { site: siteId as number });
      return (res.data ?? []).map((f) => normaliseFlag(f));
    },
  });

  const toggle = useMutation({
    mutationFn: (input: { flag: FeatureFlag; enabled: boolean }) =>
      api.post<FeatureFlag>(`/api/flags/${input.flag.id}/toggle`, { enabled: input.enabled }),
    onSuccess: (_, input) => {
      void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
      void qc.invalidateQueries({ queryKey: ["bench", "flag-history", input.flag.id] });
    },
  });

  const all = q.data ?? [];
  const needle = find.trim().toLowerCase();
  const flags = useMemo(
    () =>
      needle
        ? all.filter(
            (f) =>
              f.key.toLowerCase().includes(needle) ||
              f.name.toLowerCase().includes(needle) ||
              f.description.toLowerCase().includes(needle),
          )
        : all,
    [all, needle],
  );

  const startNew = () => {
    setOpen(null);
    setTemplate(null);
    setChoosing(true);
  };

  const newFlagButton = (
    <Button tone="primary" icon={<IconPlus size={13} />} onClick={startNew}>
      New flag
    </Button>
  );

  return (
    <div className="mf-bn-page">
      <Panel>
        <PanelHead
          title="Feature flags"
          note="A flag is a key your code reads at runtime. Turn it on, roll it out to a share of visitors, or split that share into variants."
          actions={newFlagButton}
        />

        {all.length > 6 ? (
          <div className="mf-bn-find">
            <IconSearch size={14} />
            <TextInput
              value={find}
              onChange={setFind}
              placeholder="Find a flag by key, name or description"
              ariaLabel="Find a flag"
            />
            {needle ? (
              <span className="mf-bn-find__count mf-data">
                {num(flags.length)} of {num(all.length)}
              </span>
            ) : null}
          </div>
        ) : null}

        <div className="mf-bn-indexwrap">
          <Reading
            loading={siteLoading || q.isLoading}
            error={q.isError ? q.error : undefined}
            onRetry={() => void q.refetch()}
            isEmpty={flags.length === 0}
            lines={4}
            empty={
              all.length && needle ? (
                <Empty
                  title="No flags match your search"
                  body="No flag has that text in its key, its name or its description."
                  action={<Button onClick={() => setFind("")}>Show all flags</Button>}
                />
              ) : (
                <Empty
                  title="No flags yet"
                  body="A flag lets you ship code that is off, turn it on for a share of visitors, and turn it back off without a deploy. SDKs report evaluations and exposures, so you can see what each variant actually did."
                  action={newFlagButton}
                />
              )
            }
          >
            <ul className="mf-bn-flags">
              {flags.map((f) => {
                const pending = toggle.isPending && toggle.variables?.flag.id === f.id;
                return (
                  <li
                    key={f.id}
                    className={`mf-bn-flags__row${open?.id === f.id ? " is-open" : ""}`}
                  >
                    <div className="mf-bn-flags__ident">
                      <span className="mf-bn-flags__key mf-data">{f.key}</span>
                      {f.name ? <span className="mf-fl-name">{f.name}</span> : null}
                      <p className="mf-bn-flags__desc">
                        {f.description || "No description. Open it and say what it is for."}
                      </p>
                      <p className="mf-bn-flags__meta">{flagMeta(f)}</p>
                    </div>

                    <div className="mf-bn-flags__band">
                      <RolloutBand flag={f} compact />
                    </div>

                    <div className="mf-bn-flags__act">
                      <Throw
                        label={`Serving ${f.key}`}
                        on={f.enabled}
                        onChange={(v) => {
                          if (!pending) toggle.mutate({ flag: f, enabled: v });
                        }}
                      />
                      <Button
                        size="sm"
                        icon={<IconSettings size={13} />}
                        onClick={() => {
                          setChoosing(false);
                          setTemplate(null);
                          setOpen(open?.id === f.id ? null : f);
                        }}
                      >
                        {open?.id === f.id ? "Close" : "Open"}
                      </Button>
                    </div>
                  </li>
                );
              })}
            </ul>
          </Reading>

          {toggle.isError ? (
            <p className="mf-bn-field__err mf-fl-say" role="alert">
              That flag was not changed. {errText(toggle.error)}
            </p>
          ) : null}
        </div>
      </Panel>

      {choosing ? (
        <TemplateChooser
          onCancel={() => setChoosing(false)}
          onPick={(partial, id) => {
            setChoosing(false);
            setTemplate(id);
            setOpen(id ? flagFromTemplate(partial, siteId) : blankFlag(siteId));
          }}
        />
      ) : null}

      {open ? (
        <FlagEditor
          key={open.id || `new-${template ?? "blank"}`}
          flag={open}
          template={template}
          allFlags={all}
          onDone={(result) => {
            setOpen(null);
            setTemplate(null);
            if (result === "saved" || result === "deleted") {
              void qc.invalidateQueries({ queryKey: flagsKey(siteId) });
            }
          }}
        />
      ) : null}

      <TargetGroupsPanel siteId={siteId} />
    </div>
  );
}

export default FlagsScreen;
