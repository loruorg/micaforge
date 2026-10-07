/**
 * REPORTS: the posted record.
 *
 * A report is a copy of the record, cut on a schedule and posted to whoever
 * needs it. Its contents are fixed and stated plainly rather than assembled
 * from a wall of checkboxes: the whole point of a scheduled report is that the
 * reader gets the same shape every time and can compare it to last week's.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { IconClose, IconOut, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  asSite,
  Field,
  Foot,
  Note,
  QueryState,
  Result,
  Select,
  Spec,
  SpecRow,
  TextInput,
  errText,
} from "./SettingsLayout";

type Cadence = "daily" | "weekly" | "monthly";

interface ReportChannel {
  kind: "slack" | "webhook";
  to: string;
}

interface ReportRecord {
  id: string;
  cadence: Cadence;
  recipients: string[];
  channels: ReportChannel[];
  next_run_at: string | null;
}

const CADENCE_WORD: Record<Cadence, string> = {
  daily: "Every morning",
  weekly: "Every Monday",
  monthly: "First of the month",
};

const CONTENTS: { what: string; detail: string }[] = [
  {
    what: "Both traces, side by side",
    detail: "Readers and machine fetches over the period, against the period before it.",
  },
  {
    what: "The Citation Gap",
    detail: "What was taken, what came back, and the pages where the two moved apart.",
  },
  { what: "Top pages", detail: "Ten pages by readers, and ten by machine fetches. They are rarely the same ten." },
  { what: "Acquisition", detail: "Channels including answer engines, with the sources under each." },
  { what: "Goals and events", detail: "Conversions and revenue for every goal defined on the site." },
  { what: "Anything that tripped", detail: "Alerts that fired during the period, with the reading that fired them." },
];

export function ReportsSettings() {
  const { siteId } = useSite();
  const qc = useQueryClient();
  const key = ["settings", "reports", siteId];

  const reports = useQuery({
    queryKey: key,
    queryFn: () => api.get<ReportRecord[]>("/api/reports", { site: asSite(siteId) }).then((r) => r.data),
    enabled: siteId != null,
  });

  const [cadence, setCadence] = useState<Cadence>("weekly");
  const [recipients, setRecipients] = useState<string[]>([]);
  const [draft, setDraft] = useState("");
  const [hookKind, setHookKind] = useState<"none" | "slack" | "webhook">("none");
  const [hookTo, setHookTo] = useState("");

  const create = useMutation({
    mutationFn: () =>
      api.post<ReportRecord>("/api/reports", {
        site: siteId,
        cadence,
        recipients,
        channels: hookKind === "none" || !hookTo ? [] : [{ kind: hookKind, to: hookTo }],
      }),
    onSuccess: () => {
      setRecipients([]);
      setDraft("");
      setHookTo("");
      setHookKind("none");
      void qc.invalidateQueries({ queryKey: key });
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/reports/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: key }),
  });

  const sendNow = useMutation({
    mutationFn: (id: string) => api.post<{ sent: number }>(`/api/reports/${id}/send-now`),
  });

  const addRecipient = () => {
    const v = draft.trim();
    if (!v || recipients.includes(v)) return;
    setRecipients((r) => [...r, v]);
    setDraft("");
  };

  const columns: Column<ReportRecord>[] = [
    {
      key: "cadence",
      head: "Cadence",
      width: "22%",
      render: (r) => <span>{CADENCE_WORD[r.cadence] ?? r.cadence}</span>,
    },
    {
      key: "to",
      head: "Posted to",
      render: (r) => (
        <span>
          {r.recipients.length ? r.recipients.join(", ") : "no addresses"}
          {r.channels.length ? ` · ${r.channels.map((c) => c.kind).join(", ")}` : ""}
        </span>
      ),
    },
    {
      key: "next",
      head: "Next",
      width: "18%",
      render: (r) => <span className="mf-data">{r.next_run_at ? ago(r.next_run_at) : "not scheduled"}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (r) => (
        <span className="mf-set-row">
          <Button
            size="sm"
            tone="quiet"
            icon={<IconOut size={13} />}
            disabled={sendNow.isPending}
            onClick={() => sendNow.mutate(r.id)}
          >
            Send now
          </Button>
          <Button
            size="sm"
            tone="quiet"
            icon={<IconClose size={13} />}
            disabled={remove.isPending}
            onClick={() => remove.mutate(r.id)}
          >
            Remove
          </Button>
        </span>
      ),
    },
  ];

  return (
    <>
      <Panel>
        <PanelHead
          title="Scheduled reports"
          note="Sent from the instance's own mail settings. Each report covers the period that just closed, on this site's clock."
        />
        <QueryState query={reports}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={columns}
                getKey={(r) => r.id}
                empty={
                  <Empty
                    title="Nothing is scheduled"
                    body="A report is a copy of the record posted on a cadence. Add one below and it will arrive without anyone opening the dashboard."
                  />
                }
              />
              {sendNow.isSuccess || sendNow.isError || remove.isError ? (
                <Foot tight>
                  {sendNow.isSuccess ? (
                    <span className="mf-set-foot__say" data-tone="good">
                      Report sent.
                    </span>
                  ) : null}
                  {sendNow.isError ? (
                    <span className="mf-set-foot__say" data-tone="warn" role="alert">
                      {errText(sendNow.error)}
                    </span>
                  ) : null}
                  {remove.isError ? (
                    <span className="mf-set-foot__say" data-tone="warn" role="alert">
                      {errText(remove.error)}
                    </span>
                  ) : null}
                </Foot>
              ) : null}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead title="Add a report" />
        <Spec>
          <SpecRow name="Cadence" hint="Daily arrives at 07:00 on this site's clock." htmlFor="rep-cadence">
            <Select id="rep-cadence" value={cadence} onChange={(e) => setCadence(e.target.value as Cadence)}>
              <option value="daily">Daily</option>
              <option value="weekly">Weekly, on Monday</option>
              <option value="monthly">Monthly, on the first</option>
            </Select>
          </SpecRow>

          <SpecRow
            name="Recipients"
            hint="Anyone at all. They do not need a Micaforge account to read it."
            htmlFor="rep-to"
          >
            <div className="mf-set-row">
              <TextInput
                id="rep-to"
                type="email"
                placeholder="name@example.com"
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addRecipient();
                  }
                }}
              />
              <Button icon={<IconPlus size={13} />} onClick={addRecipient} disabled={!draft.trim()}>
                Add
              </Button>
            </div>
            {recipients.length ? (
              <div className="mf-set-people">
                {recipients.map((r) => (
                  <div className="mf-set-people__row" key={r}>
                    <span className="mf-set-people__who mf-data">{r}</span>
                    <Button
                      size="sm"
                      tone="quiet"
                      icon={<IconClose size={12} />}
                      onClick={() => setRecipients((list) => list.filter((x) => x !== r))}
                    >
                      Remove
                    </Button>
                  </div>
                ))}
              </div>
            ) : (
              <Note>No addresses yet. A report with no recipients is never sent.</Note>
            )}
          </SpecRow>

          <SpecRow name="Also post to" hint="Optional. The same report body, posted as JSON.">
            <div className="mf-set-row">
              <Select
                auto
                aria-label="Extra channel"
                value={hookKind}
                onChange={(e) => setHookKind(e.target.value as "none" | "slack" | "webhook")}
              >
                <option value="none">Nowhere else</option>
                <option value="slack">Slack</option>
                <option value="webhook">Webhook</option>
              </Select>
              {hookKind !== "none" ? (
                <Field htmlFor="rep-hook">
                  <TextInput
                    id="rep-hook"
                    data
                    spellCheck={false}
                    placeholder={
                      hookKind === "slack"
                        ? "https://hooks.slack.com/services/…"
                        : "https://example.com/hooks/micaforge"
                    }
                    value={hookTo}
                    onChange={(e) => setHookTo(e.target.value)}
                  />
                </Field>
              ) : null}
            </div>
          </SpecRow>
        </Spec>

        <Foot>
          <Button
            tone="primary"
            icon={<IconPlus size={14} />}
            disabled={!recipients.length || create.isPending}
            onClick={() => create.mutate()}
          >
            Schedule this report
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={create} done="Report scheduled." working="Scheduling…" />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead
          title="What the report contains"
          note="Fixed, so that this week's can be laid against last week's without reading a legend first."
        />
        <div className="mf-set-block">
          <div className="mf-set-ledger">
            {CONTENTS.map((c) => (
              <div className="mf-set-ledger__row" key={c.what}>
                <span className="mf-set-ledger__tick" aria-hidden="true" />
                <span>
                  <span className="mf-set-ledger__what">{c.what}</span>: {c.detail}
                </span>
              </div>
            ))}
          </div>
        </div>
      </Panel>
    </>
  );
}

export default ReportsSettings;
