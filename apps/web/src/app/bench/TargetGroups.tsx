/**
 * TARGET GROUPS: named audiences that several flags share.
 *
 * "Beta testers" or "customers on the pro plan" is usually one decision used by
 * five flags. Written into each flag, the five copies drift the first time the
 * definition changes; written once here and attached, every flag follows the
 * same edit. A flag that lists groups serves only people who match at least
 * one of them, and its own conditions still apply on top.
 *
 * Deleting a group is allowed while flags use it. The server detaches it from
 * them and says which ones, and the screen repeats that list, because a flag
 * that silently stops filtering is a flag now serving people it did not before.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, ApiError } from "../../lib/api";
import { Button, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconPlus, IconSettings } from "../../components/react/icons";
import { ago } from "../../lib/format";
import { Field, Reading, TextArea, TextInput, errText } from "./QueryScreen";
import {
  ConditionEditor,
  conditionProblem,
  describeConditions,
  emptyConditions,
  readConditions,
  writeConditions,
  type ConditionSet,
} from "./ConditionEditor";
import "../../styles/app/flags.css";

/* ═══ the model ═══════════════════════════════════════════════════════════ */

export interface TargetGroup {
  id: string;
  site_id: string;
  name: string;
  description: string;
  conditions: ConditionSet;
  /** Keys of the flags that list this group. */
  flags: string[];
  created_at?: string;
  updated_at?: string;
}

export const groupsKey = (siteId: number | undefined) => ["bench", "flag-groups", siteId];

function normaliseGroup(raw: Record<string, unknown>): TargetGroup {
  return {
    id: String(raw.id ?? ""),
    site_id: String(raw.site_id ?? ""),
    name: String(raw.name ?? ""),
    description: String(raw.description ?? ""),
    conditions: readConditions(raw.conditions),
    flags: Array.isArray(raw.flags) ? raw.flags.map(String) : [],
    created_at: raw.created_at as string | undefined,
    updated_at: raw.updated_at as string | undefined,
  };
}

/** The site's target groups, shared by this panel and the flag editor. */
export function useTargetGroups(siteId: number | undefined) {
  return useQuery({
    queryKey: groupsKey(siteId),
    enabled: siteId != null,
    queryFn: async () => {
      const res = await api.get<Record<string, unknown>[]>("/api/flags/groups", { site: siteId as number });
      return (res.data ?? []).map(normaliseGroup);
    },
  });
}

/* ═══ the panel ═══════════════════════════════════════════════════════════ */

interface Draft {
  id: string;
  name: string;
  description: string;
  conditions: ConditionSet;
}

const blankDraft = (): Draft => ({ id: "", name: "", description: "", conditions: emptyConditions() });

const flagList = (keys: string[]) => keys.map((k) => `"${k}"`).join(", ");

export function TargetGroupsPanel({ siteId }: { siteId: number | undefined }) {
  const qc = useQueryClient();
  const groups = useTargetGroups(siteId);

  const [draft, setDraft] = useState<Draft | null>(null);
  const [showProblems, setShowProblems] = useState(false);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [detached, setDetached] = useState<{ name: string; from: string[] } | null>(null);

  const refresh = () => {
    void qc.invalidateQueries({ queryKey: groupsKey(siteId) });
    // A group's name and membership show on every flag that lists it.
    void qc.invalidateQueries({ queryKey: ["bench", "flags", siteId] });
  };

  const save = useMutation({
    mutationFn: (d: Draft) => {
      const body = {
        name: d.name.trim(),
        description: d.description.trim(),
        conditions: writeConditions(d.conditions),
      };
      return d.id
        ? api.patch<unknown>(`/api/flags/groups/${d.id}`, body)
        : api.post<unknown>(`/api/flags/groups?site=${encodeURIComponent(String(siteId ?? ""))}`, body);
    },
    onSuccess: () => {
      setDraft(null);
      setShowProblems(false);
      refresh();
    },
  });

  const remove = useMutation({
    mutationFn: (g: TargetGroup) =>
      api
        .del<{ deleted: string; detached_from?: string[] }>(`/api/flags/groups/${g.id}`)
        .then((r) => ({ name: g.name, from: r.data?.detached_from ?? [] })),
    onSuccess: (result) => {
      setConfirming(null);
      setDetached(result);
      refresh();
    },
  });

  const open = (d: Draft) => {
    save.reset();
    setShowProblems(false);
    setDetached(null);
    setDraft(d);
  };

  const nameProblem = draft && !draft.name.trim() ? "A group needs a name, such as Beta testers." : undefined;
  const condProblem = draft ? conditionProblem(draft.conditions) : undefined;
  const serverField = (name: string) =>
    save.error instanceof ApiError && save.error.field?.startsWith(name) ? save.error.message : undefined;

  const list = groups.data ?? [];

  return (
    <Panel>
      <PanelHead
        title="Target groups"
        note="A target group is a named audience that several flags can share. A flag that lists groups serves only people who match at least one of them; its own conditions must match as well."
        actions={
          <Button icon={<IconPlus size={13} />} onClick={() => open(blankDraft())}>
            New group
          </Button>
        }
      />

      <div className="mf-bn-indexwrap">
        <Reading
          loading={siteId == null || groups.isLoading}
          error={groups.isError ? groups.error : undefined}
          onRetry={() => void groups.refetch()}
          isEmpty={list.length === 0}
          lines={3}
          empty={
            <Empty
              title="No target groups yet"
              body="Make one for an audience you target often, such as beta testers or customers on the pro plan. Change its conditions once and every flag that uses it follows."
              action={
                <Button icon={<IconPlus size={13} />} onClick={() => open(blankDraft())}>
                  New group
                </Button>
              }
            />
          }
        >
          <ul className="mf-fl-groups">
            {list.map((g) => (
              <li key={g.id} className={`mf-fl-groups__row${draft?.id === g.id ? " is-open" : ""}`}>
                <div className="mf-fl-groups__ident">
                  <span className="mf-fl-groups__name">{g.name}</span>
                  {g.description ? <p className="mf-fl-groups__desc">{g.description}</p> : null}
                  <p className="mf-fl-groups__rule">
                    Matches <span className="mf-data">{describeConditions(g.conditions)}</span>
                  </p>
                  <p className="mf-fl-groups__meta">
                    {g.flags.length
                      ? `Used by ${g.flags.length === 1 ? "one flag" : `${g.flags.length} flags`}: ${g.flags.join(", ")}`
                      : "Not used by any flag yet"}
                    {g.updated_at ? ` · changed ${ago(g.updated_at)}` : ""}
                  </p>
                </div>
                <div className="mf-fl-groups__act">
                  {confirming === g.id ? (
                    <>
                      <span className="mf-bn-editor__ask">
                        {g.flags.length
                          ? `Delete it and detach it from ${flagList(g.flags)}? Those flags then stop filtering by it.`
                          : "Delete this group?"}
                      </span>
                      <Button tone="danger" size="sm" disabled={remove.isPending} onClick={() => remove.mutate(g)}>
                        Delete
                      </Button>
                      <Button tone="quiet" size="sm" onClick={() => setConfirming(null)}>
                        Keep
                      </Button>
                    </>
                  ) : (
                    <>
                      <Button
                        size="sm"
                        icon={<IconSettings size={13} />}
                        onClick={() =>
                          draft?.id === g.id
                            ? setDraft(null)
                            : open({ id: g.id, name: g.name, description: g.description, conditions: g.conditions })
                        }
                      >
                        {draft?.id === g.id ? "Close" : "Edit"}
                      </Button>
                      <Button tone="quiet" size="sm" onClick={() => setConfirming(g.id)}>
                        Delete
                      </Button>
                    </>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </Reading>

        {remove.isError ? (
          <p className="mf-fl-say" data-tone="warn" role="alert">
            The group was not deleted. {errText(remove.error)}
          </p>
        ) : null}
        {detached ? (
          <p className="mf-fl-say" role="status">
            Deleted {detached.name}.
            {detached.from.length
              ? ` It was detached from ${flagList(detached.from)}, which no longer filter by it.`
              : " No flag was using it."}
          </p>
        ) : null}
      </div>

      {draft ? (
        <section className="mf-fl-group-edit" aria-label={draft.id ? `Edit ${draft.name}` : "New target group"}>
          <h3 className="mf-bn-editor__h">{draft.id ? `Edit ${draft.name || "group"}` : "New target group"}</h3>
          <div className="mf-bn-build__row">
            <Field
              label="Name"
              error={(showProblems ? nameProblem : undefined) ?? serverField("name")}
              control={(id) => (
                <TextInput
                  id={id}
                  value={draft.name}
                  placeholder="Beta testers"
                  invalid={showProblems && !!nameProblem}
                  onChange={(v) => setDraft({ ...draft, name: v })}
                />
              )}
            />
          </div>
          <div className="mf-bn-build__row">
            <Field
              label="What it is for"
              wide
              hint="Optional. Shown under the group's name here and in the flag editor."
              control={(id) => (
                <TextArea
                  id={id}
                  rows={2}
                  value={draft.description}
                  placeholder="People who asked for early access on the waitlist form."
                  onChange={(v) => setDraft({ ...draft, description: v })}
                />
              )}
            />
          </div>
          <div>
            <h4 className="mf-fl-sub">Who is in it</h4>
            <p className="mf-bn-editor__hint">
              Attributes come from the context your code passes to the SDK. A group with no conditions
              matches everyone.
            </p>
            <ConditionEditor
              value={draft.conditions}
              onChange={(conditions) => setDraft({ ...draft, conditions })}
              emptyNote="No conditions. This group matches everyone."
              error={(showProblems ? condProblem : undefined) ?? serverField("conditions")}
            />
          </div>
          <div className="mf-fl-group-edit__foot">
            <Button
              tone="primary"
              disabled={save.isPending}
              onClick={() => {
                setShowProblems(true);
                if (!nameProblem && !condProblem) save.mutate(draft);
              }}
            >
              {save.isPending ? "Saving…" : draft.id ? "Save the group" : "Create the group"}
            </Button>
            <Button tone="quiet" onClick={() => setDraft(null)}>
              Cancel
            </Button>
            {save.isError && !serverField("name") && !serverField("conditions") ? (
              <p className="mf-bn-field__err" role="alert">
                The group was not saved. {errText(save.error)}
              </p>
            ) : null}
          </div>
        </section>
      ) : null}
    </Panel>
  );
}

export default TargetGroupsPanel;
