/**
 * ALERTS.
 *
 * An alert is not a form with a "threshold" field; it is a rule the server
 * enforces, so each one is WRITTEN as that rule with its figures set into the
 * sentence. Reading the page reads the operating instructions, which is both
 * terser than a labelled grid and harder to misconfigure.
 *
 * An alert that exists is on. Switching it off deletes the rule
 * rather than storing a disabled copy of it, because `alerts` has no enabled
 * column and a hidden second state is a lie waiting to happen.
 */
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  asSite,
  Foot,
  NumberInput,
  Note,
  QueryState,
  Result,
  Select,
  TextInput,
  Toggle,
  errText,
} from "./SettingsLayout";

type AlertKind = "spike" | "drop" | "goal" | "error" | "agent_surge" | "citation_gap";
type ChannelKind = "email" | "webhook" | "slack";

interface AlertChannel {
  kind: ChannelKind;
  to: string;
}

interface AlertRecord {
  id: string;
  kind: AlertKind;
  config: Record<string, string | number>;
  channels: AlertChannel[];
  last_fired_at: string | null;
}

interface Goal {
  id: string;
  name: string;
}

const KINDS: { kind: AlertKind; name: string; config: Record<string, string | number> }[] = [
  { kind: "spike", name: "Visitor spike", config: { multiple: 3 } },
  { kind: "drop", name: "Visitor drop", config: { percent: 50 } },
  { kind: "goal", name: "Goal falls short", config: { goal_id: "", minimum: 1 } },
  { kind: "error", name: "Error surge", config: { count: 25 } },
  { kind: "agent_surge", name: "AI crawler surge", config: { multiple: 4 } },
  { kind: "citation_gap", name: "Citation gap", config: { fetches: 200, days: 14 } },
];

const CHANNEL_PLACEHOLDER: Record<ChannelKind, string> = {
  email: "ops@example.com",
  webhook: "https://example.com/hooks/micaforge",
  slack: "https://hooks.slack.com/services/…",
};

const CHANNEL_LABEL: Record<ChannelKind, string> = {
  email: "Where to send it",
  webhook: "Endpoint to post to",
  slack: "Slack incoming webhook",
};

function numOf(c: Record<string, string | number>, k: string, fallback: number): number {
  const v = c[k];
  return typeof v === "number" ? v : typeof v === "string" && v !== "" ? Number(v) : fallback;
}

/* ── one trip level ──────────────────────────────────────────────────────── */

function Trip({
  kind,
  name,
  defaults,
  record,
  siteId,
  goals,
}: {
  kind: AlertKind;
  name: string;
  defaults: Record<string, string | number>;
  record: AlertRecord | undefined;
  siteId: number | string;
  goals: Goal[];
}) {
  const qc = useQueryClient();
  const key = ["settings", "alerts", siteId];
  const armed = !!record;

  const [config, setConfig] = useState<Record<string, string | number>>(record?.config ?? defaults);
  const [channel, setChannel] = useState<AlertChannel>(
    record?.channels?.[0] ?? { kind: "email", to: "" },
  );

  // The server is the source of truth; a fresh record replaces the draft.
  useEffect(() => {
    if (!record) return;
    setConfig(record.config ?? defaults);
    if (record.channels?.[0]) setChannel(record.channels[0]);
  }, [record?.id, JSON.stringify(record?.config), JSON.stringify(record?.channels)]);

  const invalidate = () => qc.invalidateQueries({ queryKey: key });

  const arm = useMutation({
    mutationFn: () =>
      api.post<AlertRecord>("/api/alerts", { site: siteId, kind, config, channels: [channel] }),
    onSuccess: invalidate,
  });
  const disarm = useMutation({
    mutationFn: () => api.del<null>(`/api/alerts/${record?.id}`),
    onSuccess: invalidate,
  });
  const save = useMutation({
    mutationFn: () => api.patch<AlertRecord>(`/api/alerts/${record?.id}`, { config, channels: [channel] }),
    onSuccess: invalidate,
  });
  const test = useMutation({
    mutationFn: () => api.post<{ delivered: boolean }>(`/api/alerts/${record?.id}/test`),
  });

  const dirty =
    armed &&
    (JSON.stringify(config) !== JSON.stringify(record?.config) ||
      JSON.stringify([channel]) !== JSON.stringify(record?.channels));

  const set = (k: string, v: string | number) => setConfig((c) => ({ ...c, [k]: v }));
  const noGoals = kind === "goal" && goals.length === 0;

  const rule = (() => {
    switch (kind) {
      case "spike":
        return (
          <>
            Alert when visitors in an hour exceed
            <NumberInput
              aria-label="Multiple of the same hour last week"
              min={1.5}
              step={0.5}
              value={numOf(config, "multiple", 3)}
              onChange={(e) => set("multiple", Number(e.target.value))}
            />
            × the same hour last week.
          </>
        );
      case "drop":
        return (
          <>
            Alert when visitors in a day fall below
            <NumberInput
              aria-label="Percentage of the seven-day median"
              min={5}
              max={95}
              step={5}
              value={numOf(config, "percent", 50)}
              onChange={(e) => set("percent", Number(e.target.value))}
            />
            % of the last seven days&apos; median.
          </>
        );
      case "goal":
        return (
          <>
            Alert when{" "}
            <Select
              auto
              aria-label="Goal to watch"
              value={String(config.goal_id ?? "")}
              disabled={noGoals}
              onChange={(e) => set("goal_id", e.target.value)}
            >
              <option value="">choose a goal</option>
              {goals.map((g) => (
                <option key={g.id} value={g.id}>
                  {g.name}
                </option>
              ))}
            </Select>{" "}
            converts fewer than
            <NumberInput
              aria-label="Minimum conversions in a day"
              min={1}
              step={1}
              value={numOf(config, "minimum", 1)}
              onChange={(e) => set("minimum", Number(e.target.value))}
            />
            times in a day.
          </>
        );
      case "error":
        return (
          <>
            Alert when errors in an hour exceed
            <NumberInput
              aria-label="Errors in an hour"
              min={1}
              step={1}
              value={numOf(config, "count", 25)}
              onChange={(e) => set("count", Number(e.target.value))}
            />
            , counted by fingerprint.
          </>
        );
      case "agent_surge":
        return (
          <>
            Alert when crawler fetches in an hour exceed
            <NumberInput
              aria-label="Multiple of the seven-day average"
              min={1.5}
              step={0.5}
              value={numOf(config, "multiple", 4)}
              onChange={(e) => set("multiple", Number(e.target.value))}
            />
            × the seven-day average for this site.
          </>
        );
      case "citation_gap":
        return (
          <>
            Alert when one page passes
            <NumberInput
              wide
              aria-label="Crawler fetches"
              min={10}
              step={10}
              value={numOf(config, "fetches", 200)}
              onChange={(e) => set("fetches", Number(e.target.value))}
            />
            crawler fetches with no visit from an AI answer for
            <NumberInput
              aria-label="Days without a visit from an AI answer"
              min={1}
              step={1}
              value={numOf(config, "days", 14)}
              onChange={(e) => set("days", Number(e.target.value))}
            />
            days.
          </>
        );
    }
  })();

  const busy = arm.isPending || disarm.isPending;

  return (
    <div className="mf-set-trip" data-on={armed ? "true" : "false"}>
      <div className="mf-set-trip__head">
        <h3 className="mf-set-trip__name">{name}</h3>
        <Toggle
          label={`${name} alert`}
          checked={armed}
          disabled={busy || noGoals}
          onChange={(v) => (v ? arm.mutate() : disarm.mutate())}
        />
      </div>

      <p className="mf-set-trip__rule">{rule}</p>

      {noGoals ? (
        <Note>
          No goals are defined for this site yet. Define one on the Goals screen and this alert can watch it.
        </Note>
      ) : null}

      <div className="mf-set-trip__where">
        <Select
          aria-label={`Channel for the ${name} alert`}
          value={channel.kind}
          onChange={(e) => setChannel({ ...channel, kind: e.target.value as ChannelKind })}
        >
          <option value="email">Email</option>
          <option value="webhook">Webhook</option>
          <option value="slack">Slack</option>
        </Select>
        <TextInput
          data={channel.kind !== "email"}
          aria-label={CHANNEL_LABEL[channel.kind]}
          placeholder={CHANNEL_PLACEHOLDER[channel.kind]}
          value={channel.to}
          spellCheck={false}
          onChange={(e) => setChannel({ ...channel, to: e.target.value })}
        />
      </div>

      <div className="mf-set-trip__acts">
        <Button
          size="sm"
          icon={<IconCheck size={13} />}
          disabled={!dirty || save.isPending}
          onClick={() => save.mutate()}
        >
          Save this rule
        </Button>
        <Button
          size="sm"
          tone="quiet"
          disabled={!armed || !channel.to || test.isPending}
          onClick={() => test.mutate()}
        >
          Send a test
        </Button>
        <Result action={save} done="Rule saved." />
        {test.isPending ? <span className="mf-set-foot__say">Sending…</span> : null}
        {test.isSuccess ? (
          <span className="mf-set-foot__say" data-tone="good">
            Test sent to {channel.to}.
          </span>
        ) : null}
        {test.isError ? (
          <span className="mf-set-foot__say" data-tone="warn" role="alert">
            {errText(test.error)}
          </span>
        ) : null}
        {arm.isError ? (
          <span className="mf-set-foot__say" data-tone="warn" role="alert">
            {errText(arm.error)}
          </span>
        ) : null}
        {disarm.isError ? (
          <span className="mf-set-foot__say" data-tone="warn" role="alert">
            {errText(disarm.error)}
          </span>
        ) : null}
        <span className="mf-set-trip__fired">
          {record?.last_fired_at ? `Last fired ${ago(record.last_fired_at)}` : "Never fired"}
        </span>
      </div>
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function AlertsSettings() {
  const { siteId } = useSite();

  const alerts = useQuery({
    queryKey: ["settings", "alerts", siteId],
    queryFn: () => api.get<AlertRecord[]>("/api/alerts", { site: asSite(siteId) }).then((r) => r.data),
    enabled: siteId != null,
  });

  const goals = useQuery({
    queryKey: ["settings", "goals", siteId],
    queryFn: () => api.get<Goal[]>("/api/goals", { site: asSite(siteId) }).then((r) => r.data),
    enabled: siteId != null,
  });

  return (
    <Panel>
      <PanelHead
        title="Alerts"
        note="Six alerts. Each one fires once when the value crosses its limit and stays quiet until the value comes back, so one long incident is one message rather than a hundred."
      />
      <QueryState query={alerts}>
        {(rows) => (
          <>
            {KINDS.map((k) => (
              <Trip
                key={k.kind}
                kind={k.kind}
                name={k.name}
                defaults={k.config}
                record={rows.find((r) => r.kind === k.kind)}
                siteId={asSite(siteId)}
                goals={goals.data ?? []}
              />
            ))}
            <Foot>
              <Note>
                Email is sent by the instance&apos;s own mail settings. A webhook receives the same body as a
                Slack hook, so any endpoint that accepts JSON will do.
              </Note>
            </Foot>
          </>
        )}
      </QueryState>
    </Panel>
  );
}

export default AlertsSettings;
