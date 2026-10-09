/**
 * A FLAG'S HISTORY: every change, who made it, and the way back.
 *
 * Each change is shown field by field in words ("Rollout 10% to 50%"), not as
 * two blobs of JSON, because the question asked of this list is nearly always
 * "what did that change do", asked during an incident.
 *
 * "Revert this change" puts back the flag as it stood just BEFORE that change,
 * and the server records the revert as a change of its own, so a revert can be
 * reverted. The flag's creation has no earlier state to return to, so its
 * button is shown disabled with the reason beside it rather than hidden.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type Actor } from "../../lib/api";
import { Button, Empty, Panel, PanelHead } from "../../components/react/ui";
import { IconReplay } from "../../components/react/icons";
import { ago } from "../../lib/format";
import { Reading, errText } from "./QueryScreen";
import { describeConditions, readConditions } from "./ConditionEditor";
import "../../styles/app/flags.css";

export interface FlagChange {
  id: string;
  flag_id: string;
  action: "created" | "updated" | "toggled" | "reverted";
  actor: Actor;
  created_at: string;
  diff: { field: string; before: unknown; after: unknown }[];
  revertable: boolean;
}

/** The first changes shown; the rest are one click away. */
const FIRST = 12;

const FIELD_NAME: Record<string, string> = {
  key: "Key",
  name: "Name",
  description: "Description",
  enabled: "Serving",
  rollout_pct: "Rollout",
  variants: "Variants",
  conditions: "Conditions",
  target_groups: "Target groups",
  dependencies: "Prerequisites",
  environments: "Environments",
};

export function actorText(a: Actor | null | undefined): string {
  if (!a) return "Someone";
  if (a.kind === "api_key") return `API key ${a.label}`;
  if (a.kind === "system") return a.label || "Micaforge";
  return a.label || "A member";
}

/** A stored value, in the words the editor uses for it. */
function fieldValue(field: string, v: unknown, groupName: (id: string) => string): string {
  if (v === null || v === undefined) return "not set";
  switch (field) {
    case "enabled":
      return v ? "On" : "Off";
    case "rollout_pct":
      return `${String(v)}%`;
    case "variants":
      return Array.isArray(v) && v.length
        ? v.map((x: { key?: unknown; weight?: unknown }) => `${String(x?.key ?? "?")} ${String(x?.weight ?? 1)}`).join(", ")
        : "none (on or off)";
    case "conditions":
      return describeConditions(readConditions(v));
    case "target_groups":
      return Array.isArray(v) && v.length ? v.map((id) => groupName(String(id))).join(", ") : "none";
    case "dependencies":
      return Array.isArray(v) && v.length
        ? v
            .map((d: { flag?: unknown; enabled?: unknown; variant?: unknown }) =>
              `${String(d?.flag ?? "?")} ${d?.enabled === false ? "off" : "on"}${d?.variant ? ` (${String(d.variant)})` : ""}`,
            )
            .join(", ")
        : "none";
    case "environments": {
      const entries = v && typeof v === "object" ? Object.entries(v as Record<string, { enabled?: boolean; rollout_pct?: number }>) : [];
      return entries.length
        ? entries
            .map(([name, e]) => {
              const parts = [
                e?.enabled === undefined ? null : e.enabled ? "on" : "off",
                e?.rollout_pct === undefined ? null : `${e.rollout_pct}%`,
              ].filter(Boolean);
              return `${name}: ${parts.length ? parts.join(", ") : "as main"}`;
            })
            .join("; ")
        : "none";
    }
    default:
      if (typeof v === "string") return v === "" ? "empty" : v;
      if (typeof v === "number" || typeof v === "boolean") return String(v);
      return JSON.stringify(v);
  }
}

function headline(c: FlagChange): string {
  if (c.action === "created") return "Created the flag";
  if (c.action === "reverted") return "Reverted an earlier change";
  if (c.action === "toggled") {
    const after = c.diff.find((d) => d.field === "enabled")?.after;
    return after === true ? "Turned it on" : after === false ? "Turned it off" : "Switched serving";
  }
  return c.diff.length === 1
    ? `Changed ${(FIELD_NAME[c.diff[0].field] ?? c.diff[0].field).toLowerCase()}`
    : `Changed ${c.diff.length} settings`;
}

export function FlagHistory({
  flagId,
  groupName,
  onReverted,
}: {
  flagId: string;
  /** Names a target group id, for changes to the groups a flag lists. */
  groupName: (id: string) => string;
  /** The flag as it stands after a revert, so the editor shows it at once. */
  onReverted: (flag: Record<string, unknown>) => void;
}) {
  const qc = useQueryClient();
  const [showAll, setShowAll] = useState(false);

  const history = useQuery({
    queryKey: ["bench", "flag-history", flagId],
    enabled: !!flagId,
    queryFn: () =>
      api.get<FlagChange[]>(`/api/flags/${encodeURIComponent(flagId)}/history`).then((r) => r.data ?? []),
  });

  const revert = useMutation({
    mutationFn: (changeId: string) =>
      api
        .post<Record<string, unknown>>(`/api/flags/${encodeURIComponent(flagId)}/revert`, { change_id: changeId })
        .then((r) => r.data),
    onSuccess: (flag) => {
      void qc.invalidateQueries({ queryKey: ["bench", "flag-history", flagId] });
      void qc.invalidateQueries({ queryKey: ["bench", "flags"] });
      onReverted(flag);
    },
  });

  const changes = history.data ?? [];
  const shown = showAll ? changes : changes.slice(0, FIRST);

  return (
    <Panel>
      <PanelHead
        title="History"
        note="Every change to this flag, newest first. Reverting puts the flag back as it was just before that change, and replaces any unsaved edits in the editor above."
      />
      <div className="mf-bn-indexwrap">
        <Reading
          loading={history.isPending}
          error={history.isError ? history.error : undefined}
          onRetry={() => void history.refetch()}
          isEmpty={changes.length === 0}
          lines={3}
          empty={
            <Empty
              title="No changes recorded"
              body="Changes made from here, from the API or by an MCP client are listed here as they happen."
            />
          }
        >
          <ol className="mf-fl-history">
            {shown.map((c) => (
              <li key={c.id} className="mf-fl-history__item">
                <div className="mf-fl-history__head">
                  <span className="mf-fl-history__what">{headline(c)}</span>
                  <span className="mf-fl-history__who">
                    {actorText(c.actor)} ·{" "}
                    <time dateTime={c.created_at} title={new Date(c.created_at).toLocaleString()}>
                      {ago(c.created_at)}
                    </time>
                  </span>
                  <span className="mf-fl-history__act">
                    {c.revertable ? null : (
                      <span className="mf-bn-field__hint">Nothing earlier to go back to</span>
                    )}
                    <Button
                      size="sm"
                      icon={<IconReplay size={13} />}
                      disabled={!c.revertable || revert.isPending}
                      onClick={() => revert.mutate(c.id)}
                    >
                      {revert.isPending && revert.variables === c.id ? "Reverting…" : "Revert this change"}
                    </Button>
                  </span>
                </div>
                {c.diff.length ? (
                  <dl className="mf-fl-diff">
                    {c.diff.map((d) => (
                      <div key={d.field} className="mf-fl-diff__row">
                        <dt className="mf-fl-diff__field">{FIELD_NAME[d.field] ?? d.field}</dt>
                        <dd className="mf-fl-diff__change">
                          <span className="mf-fl-diff__from mf-data">
                            <span className="mf-sr">before: </span>
                            {fieldValue(d.field, d.before, groupName)}
                          </span>
                          <span className="mf-fl-diff__arrow" aria-hidden="true">
                            →
                          </span>
                          <span className="mf-fl-diff__to mf-data">
                            <span className="mf-sr">after: </span>
                            {fieldValue(d.field, d.after, groupName)}
                          </span>
                        </dd>
                      </div>
                    ))}
                  </dl>
                ) : null}
                {revert.isError && revert.variables === c.id ? (
                  <p className="mf-bn-field__err" role="alert">
                    That change was not reverted. {errText(revert.error)}
                  </p>
                ) : null}
              </li>
            ))}
          </ol>
          {changes.length > FIRST ? (
            <div className="mf-fl-more">
              <Button size="sm" tone="quiet" onClick={() => setShowAll((v) => !v)}>
                {showAll ? `Show the newest ${FIRST}` : `Show all ${changes.length} changes`}
              </Button>
            </div>
          ) : null}
        </Reading>
        {revert.isSuccess ? (
          <p className="mf-fl-say" role="status">
            Reverted. The editor above now shows the flag as it is on the server.
          </p>
        ) : null}
      </div>
    </Panel>
  );
}

export default FlagHistory;
