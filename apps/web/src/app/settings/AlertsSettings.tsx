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
 *
 * WHERE AN ALERT GOES is one channels document on the server, read by the same
 * lenient parser the worker delivers with: `{ email: [...], webhook, slack,
 * discord, telegram: { chat_id, bot_token } }`. The screen edits it as a list
 * of rows (any number of addresses, at most one of each of the others) and
 * writes the document back whole. Two details matter:
 *
 *   - The rows are read from `resolved_channels`, the server's own reading of
 *     the stored document, so whatever shape it was stored in, the screen shows
 *     exactly what the worker will deliver to.
 *   - A Telegram bot token is sealed on the server and never returned. The
 *     screen keeps the sealed value it was given and sends it back unchanged,
 *     so saving another field never asks for the token again; typing a new
 *     token replaces it.
 */
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck, IconClose, IconPlus } from "../../components/react/icons";
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
type ChannelKind = "email" | "webhook" | "slack" | "discord" | "telegram";

/** One row of the channel editor. Telegram uses `chat_id` and the token fields. */
interface ChannelRow {
  kind: ChannelKind;
  to: string;
  chat_id: string;
  /** A new token typed in this session; empty keeps the sealed one. */
  bot_token: string;
  /** The sealed token the server returned, sent back unchanged to keep it. */
  sealed: string | null;
}

/** The server's own reading of a stored channels document. */
interface ResolvedChannels {
  emails?: string[];
  webhook?: string | null;
  slack?: string | null;
  discord?: string | null;
  telegram?: { chat_id: string } | null;
}

interface AlertRecord {
  id: string;
  kind: AlertKind;
  config: Record<string, string | number>;
  /** The stored document, in whatever shape it was written. */
  channels: unknown;
  resolved_channels?: ResolvedChannels;
  last_fired_at: string | null;
}

/** One line of a test send's report. */
interface TestResult {
  channel: string;
  target?: string;
  delivered: boolean;
  error?: string | null;
  status?: number;
}

interface TestReport {
  results: TestResult[];
  mail_delivered?: boolean;
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

const CHANNEL_NAME: Record<ChannelKind, string> = {
  email: "Email",
  webhook: "Webhook",
  slack: "Slack",
  discord: "Discord",
  telegram: "Telegram",
};

const CHANNEL_PLACEHOLDER: Record<Exclude<ChannelKind, "telegram">, string> = {
  email: "ops@example.com",
  webhook: "https://example.com/hooks/micaforge",
  slack: "https://hooks.slack.com/services/…",
  discord: "https://discord.com/api/webhooks/…",
};

const CHANNEL_LABEL: Record<Exclude<ChannelKind, "telegram">, string> = {
  email: "Email address to send it to",
  webhook: "Endpoint to post to",
  slack: "Slack incoming webhook",
  discord: "Discord channel webhook",
};

const KIND_ORDER: ChannelKind[] = ["email", "webhook", "slack", "discord", "telegram"];

const blankRow = (kind: ChannelKind = "email"): ChannelRow => ({
  kind,
  to: "",
  chat_id: "",
  bot_token: "",
  sealed: null,
});

/** The sealed Telegram token in a stored document, if it holds one. */
function sealedTelegram(stored: unknown): { chat_id?: string; bot_token_sealed?: string } | null {
  if (!stored || typeof stored !== "object" || Array.isArray(stored)) return null;
  const t = (stored as { telegram?: unknown }).telegram;
  return t && typeof t === "object" ? (t as { chat_id?: string; bot_token_sealed?: string }) : null;
}

/** The editor's rows for a stored alert, read off the server's own reading. */
function rowsFrom(record: AlertRecord | undefined): ChannelRow[] {
  if (!record) return [blankRow()];
  const r = record.resolved_channels ?? {};
  const rows: ChannelRow[] = [];
  for (const e of r.emails ?? []) rows.push({ ...blankRow("email"), to: e });
  if (r.webhook) rows.push({ ...blankRow("webhook"), to: r.webhook });
  if (r.slack) rows.push({ ...blankRow("slack"), to: r.slack });
  if (r.discord) rows.push({ ...blankRow("discord"), to: r.discord });
  const tg = sealedTelegram(record.channels);
  if (r.telegram || tg) {
    rows.push({
      ...blankRow("telegram"),
      chat_id: r.telegram?.chat_id ?? tg?.chat_id ?? "",
      sealed: tg?.bot_token_sealed ?? null,
    });
  }
  return rows.length ? rows : [blankRow()];
}

/** The channels document the server stores, written from the editor's rows. */
function documentFrom(rows: ChannelRow[]): Record<string, unknown> {
  const doc: Record<string, unknown> = {};
  const emails = rows.filter((r) => r.kind === "email" && r.to.trim()).map((r) => r.to.trim());
  if (emails.length) doc.email = emails;
  for (const kind of ["webhook", "slack", "discord"] as const) {
    const row = rows.find((r) => r.kind === kind && r.to.trim());
    if (row) doc[kind] = row.to.trim();
  }
  const tg = rows.find((r) => r.kind === "telegram");
  if (tg && (tg.chat_id.trim() || tg.bot_token.trim() || tg.sealed)) {
    doc.telegram = tg.bot_token.trim()
      ? { bot_token: tg.bot_token.trim(), chat_id: tg.chat_id.trim() }
      : { bot_token_sealed: tg.sealed, chat_id: tg.chat_id.trim() };
  }
  return doc;
}

/** What is still missing before these rows can be saved, in words. */
function rowsProblem(rows: ChannelRow[]): string | null {
  const tg = rows.find((r) => r.kind === "telegram");
  if (tg) {
    if (!tg.chat_id.trim()) return "Telegram needs the chat id to post to.";
    if (!tg.bot_token.trim() && !tg.sealed) return "Telegram needs the bot token from @BotFather.";
  }
  if (!Object.keys(documentFrom(rows)).length) return "Add at least one place to send this alert.";
  return null;
}

const CHANNEL_WORD: Record<string, string> = {
  email: "Email",
  webhook: "Webhook",
  slack: "Slack",
  discord: "Discord",
  telegram: "Telegram",
};

function numOf(c: Record<string, string | number>, k: string, fallback: number): number {
  const v = c[k];
  return typeof v === "number" ? v : typeof v === "string" && v !== "" ? Number(v) : fallback;
}

/* ── where it goes ───────────────────────────────────────────────────────── */

function ChannelEditor({
  name,
  rows,
  onChange,
}: {
  name: string;
  rows: ChannelRow[];
  onChange: (rows: ChannelRow[]) => void;
}) {
  const setRow = (i: number, patch: Partial<ChannelRow>) =>
    onChange(rows.map((r, k) => (k === i ? { ...r, ...patch } : r)));
  /* Only email may repeat: the document holds one webhook, one Slack hook,
     one Discord hook and one Telegram chat. */
  const taken = (kind: ChannelKind, self: number) =>
    kind !== "email" && rows.some((r, k) => k !== self && r.kind === kind);
  const free = KIND_ORDER.find((k) => !taken(k, -1)) ?? "email";

  return (
    <div className="mf-cx-chans">
      {rows.map((row, i) => (
        <div className="mf-cx-chan" key={i} data-kind={row.kind}>
          <Select
            aria-label={`Channel ${i + 1} for the ${name} alert`}
            value={row.kind}
            onChange={(e) => setRow(i, { ...blankRow(e.target.value as ChannelKind) })}
          >
            {KIND_ORDER.map((k) => (
              <option key={k} value={k} disabled={taken(k, i)}>
                {CHANNEL_NAME[k]}
              </option>
            ))}
          </Select>

          {row.kind === "telegram" ? (
            <div className="mf-cx-chan__pair">
              <TextInput
                data
                aria-label="Telegram chat id"
                placeholder="Chat id, e.g. -1001234567890"
                value={row.chat_id}
                spellCheck={false}
                autoComplete="off"
                onChange={(e) => setRow(i, { chat_id: e.target.value })}
              />
              <TextInput
                data
                type="password"
                aria-label={row.sealed ? "New Telegram bot token (leave empty to keep the saved one)" : "Telegram bot token"}
                placeholder={row.sealed ? "Saved. Type a new token to replace it" : "Bot token, e.g. 123456:ABC…"}
                value={row.bot_token}
                spellCheck={false}
                autoComplete="off"
                onChange={(e) => setRow(i, { bot_token: e.target.value })}
              />
            </div>
          ) : (
            <TextInput
              data={row.kind !== "email"}
              type={row.kind === "email" ? "email" : "url"}
              aria-label={CHANNEL_LABEL[row.kind]}
              placeholder={CHANNEL_PLACEHOLDER[row.kind]}
              value={row.to}
              spellCheck={false}
              onChange={(e) => setRow(i, { to: e.target.value })}
            />
          )}

          <Button
            size="sm"
            tone="quiet"
            icon={<IconClose size={12} />}
            aria-label={`Remove channel ${i + 1} (${CHANNEL_NAME[row.kind]})`}
            disabled={rows.length < 2}
            onClick={() => onChange(rows.filter((_, k) => k !== i))}
          />
        </div>
      ))}
      <div>
        <Button size="sm" tone="quiet" icon={<IconPlus size={12} />} onClick={() => onChange([...rows, blankRow(free)])}>
          Add a channel
        </Button>
      </div>
    </div>
  );
}

/** One line per channel the test went to, each with its own outcome. */
function TestReportLines({ report }: { report: TestReport }) {
  if (!report.results.length) {
    return (
      <p className="mf-set-foot__say" data-tone="warn" role="status">
        Nothing was sent: the saved rule has no channel the server can deliver to.
      </p>
    );
  }
  return (
    <ul className="mf-cx-tests" role="status">
      {report.results.map((r, i) => (
        <li key={i} className="mf-cx-tests__row" data-ok={r.delivered ? "true" : "false"}>
          {r.delivered ? <IconCheck size={12} /> : <IconClose size={12} />}
          <span>
            {CHANNEL_WORD[r.channel] ?? r.channel}
            {r.target && r.channel === "email" ? (
              <>
                {" "}
                to <span className="mf-data">{r.target}</span>
              </>
            ) : null}
            {r.delivered ? ": sent." : `: not sent. ${r.error ?? (r.status ? `The endpoint answered ${r.status}.` : "")}`}
          </span>
        </li>
      ))}
    </ul>
  );
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
  const [rows, setRows] = useState<ChannelRow[]>(() => rowsFrom(record));
  const [tried, setTried] = useState(false);

  // The server is the source of truth; a fresh record replaces the draft.
  const stored = JSON.stringify(documentFrom(rowsFrom(record)));
  useEffect(() => {
    if (!record) return;
    setConfig(record.config ?? defaults);
    setRows(rowsFrom(record));
  }, [record?.id, JSON.stringify(record?.config), stored]);

  const invalidate = () => qc.invalidateQueries({ queryKey: key });
  const channels = documentFrom(rows);
  const problem = rowsProblem(rows);

  const arm = useMutation({
    mutationFn: () =>
      api.post<AlertRecord>(`/api/alerts?site=${encodeURIComponent(String(siteId))}`, {
        kind,
        config,
        channels,
      }),
    onSuccess: invalidate,
  });
  const disarm = useMutation({
    mutationFn: () => api.del<null>(`/api/alerts/${record?.id}`),
    onSuccess: invalidate,
  });
  const save = useMutation({
    mutationFn: () => api.patch<AlertRecord>(`/api/alerts/${record?.id}`, { kind, config, channels }),
    onSuccess: invalidate,
  });
  const test = useMutation({
    mutationFn: () => api.post<TestReport>(`/api/alerts/${record?.id}/test`).then((r) => r.data),
  });

  const dirty =
    armed &&
    (JSON.stringify(config) !== JSON.stringify(record?.config) || JSON.stringify(channels) !== stored);

  /** Arms or saves only a rule that can be delivered; otherwise says why not. */
  const commit = (action: { mutate: () => void }) => {
    setTried(true);
    if (!problem) action.mutate();
  };

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
          onChange={(v) => (v ? commit(arm) : disarm.mutate())}
        />
      </div>

      <p className="mf-set-trip__rule">{rule}</p>

      {noGoals ? (
        <Note>
          No goals are defined for this site yet. Define one on the Goals screen and this alert can watch it.
        </Note>
      ) : null}

      <ChannelEditor name={name} rows={rows} onChange={setRows} />

      <div className="mf-set-trip__acts">
        <Button
          size="sm"
          icon={<IconCheck size={13} />}
          disabled={!dirty || save.isPending}
          onClick={() => commit(save)}
        >
          Save this rule
        </Button>
        <Button
          size="sm"
          tone="quiet"
          disabled={!armed || dirty || test.isPending}
          title={dirty ? "Save first: a test goes to the saved channels." : undefined}
          onClick={() => test.mutate()}
        >
          Send a test
        </Button>
        {tried && problem ? (
          <span className="mf-set-foot__say" data-tone="warn" role="alert">
            {problem}
          </span>
        ) : null}
        <Result action={save} done="Rule saved." />
        {dirty ? <span className="mf-set-foot__say">Save first: a test goes to the saved channels.</span> : null}
        {test.isPending ? <span className="mf-set-foot__say">Sending…</span> : null}
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

      {test.isSuccess && test.data ? <TestReportLines report={test.data} /> : null}
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
                Slack hook, so any endpoint that accepts JSON will do. Discord takes a channel webhook URL
                (the channel&apos;s settings, Integrations, Webhooks). Telegram takes a bot token from
                @BotFather and the id of the chat the bot is in; the token is sealed on the server and is
                never shown again.
              </Note>
            </Foot>
          </>
        )}
      </QueryState>
    </Panel>
  );
}

export default AlertsSettings;
