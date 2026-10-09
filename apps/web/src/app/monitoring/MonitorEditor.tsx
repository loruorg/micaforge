/**
 * THE MONITOR EDITOR: add a monitor, or change one.
 *
 * A monitor is a promise that this server will send a request on a schedule,
 * so the form is ordered the way that promise is made: what to request, what
 * counts as a pass, and who hears about a failure. Each part is a short titled
 * group rather than one long column of boxes.
 *
 * Units follow the person, not the wire. The timeout is typed in seconds and
 * sent in milliseconds; the interval is minutes on both sides. The channels
 * are three plain boxes (email addresses, a webhook, a Slack webhook) and go
 * out as one `{ emails, webhook, slack }` document with the empty ones left
 * out, or `null` when all three are empty, which the API reads as "record
 * incidents, send nothing".
 *
 * "Test now" sends the form as it stands to `POST /api/uptime/test`, which runs
 * one check and stores nothing. So a monitor can be proved before it exists,
 * and a change can be proved before it is saved.
 *
 * Errors appear under the field they belong to. The form checks the obvious
 * limits itself (so a typo is caught before a round trip), and when the server
 * refuses for a reason of its own, its sentence goes under the field its
 * `error.field` names, or in the action row when it names none.
 */
import { useId, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconCheck } from "../../components/react/icons";
import { api } from "../../lib/api";
import { CheckOutcome, Check, Field, Input, Say, Select, TextArea } from "./parts";
import type { CheckResult, Method, Monitor, MonitorInput } from "./types";
import { fieldOf, isHttpUrl, keys, problem } from "./types";

/* ── the draft, in the units a person types ──────────────────────────────── */

interface Draft {
  name: string;
  url: string;
  method: Method;
  interval: string;
  timeout: string;
  statusMin: string;
  statusMax: string;
  keyword: string;
  jsonPath: string;
  jsonExpected: string;
  sslCheck: boolean;
  sslDays: string;
  threshold: string;
  enabled: boolean;
  emails: string;
  webhook: string;
  slack: string;
}

/**
 * The starting values of a new monitor. The API gives every field but name and
 * url a default; these are the same kind of choice, written down so the form
 * shows what will be sent rather than leaving boxes empty.
 */
const BLANK: Draft = {
  name: "",
  url: "https://",
  method: "GET",
  interval: "5",
  timeout: "10",
  statusMin: "200",
  statusMax: "299",
  keyword: "",
  jsonPath: "",
  jsonExpected: "",
  sslCheck: true,
  sslDays: "14",
  threshold: "2",
  enabled: true,
  emails: "",
  webhook: "",
  slack: "",
};

/** The channels document is stored as written; nothing here trusts its shape. */
function readChannels(raw: unknown): { emails: string[]; webhook: string; slack: string } {
  const doc = typeof raw === "object" && raw !== null && !Array.isArray(raw) ? (raw as Record<string, unknown>) : {};
  const emails = Array.isArray(doc.emails) ? doc.emails.filter((e): e is string => typeof e === "string") : [];
  return {
    emails,
    webhook: typeof doc.webhook === "string" ? doc.webhook : "",
    slack: typeof doc.slack === "string" ? doc.slack : "",
  };
}

function fromMonitor(m: Monitor): Draft {
  const ch = readChannels(m.channels);
  return {
    name: m.name,
    url: m.url,
    method: m.method,
    interval: String(m.interval_minutes),
    timeout: String(m.timeout_ms / 1000),
    statusMin: String(m.status_min),
    statusMax: String(m.status_max),
    keyword: m.keyword ?? "",
    jsonPath: m.json_path ?? "",
    jsonExpected: m.json_expected ?? "",
    sslCheck: m.ssl_check,
    sslDays: String(m.ssl_expiry_days),
    threshold: String(m.failure_threshold),
    enabled: m.enabled,
    emails: ch.emails.join(", "),
    webhook: ch.webhook,
    slack: ch.slack,
  };
}

const splitEmails = (v: string): string[] => v.split(/[\s,;]+/).filter(Boolean);

function toInput(d: Draft): MonitorInput {
  const emails = splitEmails(d.emails);
  const channels: { emails?: string[]; webhook?: string; slack?: string } = {};
  if (emails.length) channels.emails = emails;
  if (d.webhook.trim()) channels.webhook = d.webhook.trim();
  if (d.slack.trim()) channels.slack = d.slack.trim();
  const path = d.jsonPath.trim();
  return {
    name: d.name.trim(),
    url: d.url.trim(),
    method: d.method,
    interval_minutes: Number(d.interval),
    timeout_ms: Math.round(Number(d.timeout) * 1000),
    status_min: Number(d.statusMin),
    status_max: Number(d.statusMax),
    keyword: d.keyword ? d.keyword : null,
    json_path: path || null,
    json_expected: path && d.jsonExpected !== "" ? d.jsonExpected : null,
    ssl_check: d.sslCheck,
    ssl_expiry_days: Number(d.sslDays),
    failure_threshold: Number(d.threshold),
    channels: Object.keys(channels).length ? channels : null,
    enabled: d.enabled,
  };
}

const whole = (v: string, lo: number, hi: number): boolean => {
  const n = Number(v);
  return v.trim() !== "" && Number.isInteger(n) && n >= lo && n <= hi;
};

/** The limits from the API spec, keyed by the wire name of the field. */
function check(d: Draft): Record<string, string> {
  const out: Record<string, string> = {};
  if (!d.name.trim()) out.name = "Give the monitor a name, for example \"Home page\" or \"API health\".";
  if (!isHttpUrl(d.url.trim())) out.url = "Enter a full address that starts with http:// or https://.";
  if (!whole(d.interval, 1, 60)) out.interval_minutes = "Use a whole number of minutes from 1 to 60.";
  const t = Number(d.timeout);
  if (d.timeout.trim() === "" || !(t >= 1 && t <= 30)) out.timeout_ms = "Use a number of seconds from 1 to 30.";
  if (!whole(d.statusMin, 100, 599)) out.status_min = "Use a status code from 100 to 599.";
  if (!whole(d.statusMax, 100, 599)) out.status_max = "Use a status code from 100 to 599.";
  else if (!out.status_min && Number(d.statusMin) > Number(d.statusMax))
    out.status_max = "The highest status must be the same as the lowest or above it.";
  if (d.jsonExpected !== "" && !d.jsonPath.trim())
    out.json_path = "Enter the path whose value must match, or clear the expected value.";
  if (d.sslCheck && !whole(d.sslDays, 1, 365)) out.ssl_expiry_days = "Use a whole number of days from 1 to 365.";
  if (!whole(d.threshold, 1, 10)) out.failure_threshold = "Use a whole number from 1 to 10.";
  const bad = splitEmails(d.emails).find((e) => !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(e));
  if (bad) out["channels.emails"] = `"${bad}" is not an email address.`;
  if (d.webhook.trim() && !isHttpUrl(d.webhook.trim(), true))
    out["channels.webhook"] = "Enter a full https:// address.";
  if (d.slack.trim() && !isHttpUrl(d.slack.trim(), true))
    out["channels.slack"] = "Enter the https:// address Slack gave you for the incoming webhook.";
  return out;
}

/** Fields the form can point at. A server error naming any other goes in the action row. */
const FIELDS = new Set([
  "name",
  "url",
  "method",
  "interval_minutes",
  "timeout_ms",
  "status_min",
  "status_max",
  "keyword",
  "json_path",
  "json_expected",
  "ssl_check",
  "ssl_expiry_days",
  "failure_threshold",
  "channels",
  "channels.emails",
  "channels.webhook",
  "channels.slack",
  "enabled",
]);

/* ── the editor ──────────────────────────────────────────────────────────── */

export function MonitorEditor({
  siteId,
  monitor,
  onDone,
}: {
  siteId: number;
  /** The monitor being changed; absent when adding one. */
  monitor?: Monitor;
  /** Called with the saved monitor, or with nothing when the person cancels. */
  onDone: (saved?: Monitor) => void;
}) {
  const uid = useId();
  const qc = useQueryClient();
  const [draft, setDraft] = useState<Draft>(() => (monitor ? fromMonitor(monitor) : BLANK));
  const [tried, setTried] = useState(false);
  const [server, setServer] = useState<{ field?: string; message: string } | null>(null);

  const set = (patch: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setServer(null);
  };

  const local = tried ? check(draft) : {};
  const errFor = (k: string): string | undefined =>
    local[k] ?? (server?.field === k || (k === "channels" && server?.field?.startsWith("channels")) ? server?.message : undefined);
  const bind = (k: string) => ({
    id: `${uid}-${k}`,
    "aria-invalid": errFor(k) ? true : undefined,
    "aria-describedby": errFor(k) ? `${uid}-${k}-err` : undefined,
  });

  const fail = (e: unknown) => {
    const f = fieldOf(e);
    setServer({ field: f && FIELDS.has(f) ? f : undefined, message: problem(e) });
  };

  const save = useMutation({
    mutationFn: (input: MonitorInput) =>
      monitor
        ? api.patch<Monitor>(`/api/uptime/monitors/${monitor.id}`, input).then((r) => r.data)
        : api.post<Monitor>(`/api/uptime/monitors?site=${siteId}`, input).then((r) => r.data),
    onSuccess: async (saved) => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: keys.monitors(siteId) }),
        qc.invalidateQueries({ queryKey: keys.summary(siteId) }),
      ]);
      onDone(saved);
    },
    onError: fail,
  });

  const test = useMutation({
    mutationFn: (input: MonitorInput) =>
      api.post<CheckResult>(`/api/uptime/test?site=${siteId}`, input).then((r) => r.data),
    onError: fail,
  });

  const run = (what: "save" | "test") => {
    setTried(true);
    setServer(null);
    if (Object.keys(check(draft)).length) return;
    const input = toInput(draft);
    if (what === "save") save.mutate(input);
    else test.mutate(input);
  };

  const unplaced = server && !server.field ? server.message : null;
  const busy = save.isPending;

  return (
    <Panel className="mf-mon-editor">
      <PanelHead
        title={monitor ? `Edit ${monitor.name}` : "Add a monitor"}
        note={
          monitor
            ? `Checks run from this server, region ${monitor.region}. Changes apply from the next check.`
            : "This server sends the request on the schedule you set. Nothing is sent until you save."
        }
      />

      <form
        className="mf-mon-form"
        noValidate
        onSubmit={(e) => {
          e.preventDefault();
          run("save");
        }}
      >
        <fieldset className="mf-mon-form__part">
          <legend className="mf-mon-form__title">What to check</legend>
          <div className="mf-mon-grid">
            <Field id={`${uid}-name`} label="Name" error={errFor("name")}>
              <Input {...bind("name")} value={draft.name} maxLength={120} onChange={(e) => set({ name: e.target.value })} />
            </Field>
            <Field id={`${uid}-url`} label="Address" error={errFor("url")} wide>
              <Input
                {...bind("url")}
                data
                type="url"
                inputMode="url"
                spellCheck={false}
                autoComplete="off"
                value={draft.url}
                onChange={(e) => set({ url: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-method`} label="Method" error={errFor("method")}>
              <Select {...bind("method")} value={draft.method} onChange={(e) => set({ method: e.target.value as Method })}>
                <option value="GET">GET</option>
                <option value="HEAD">HEAD</option>
                <option value="POST">POST</option>
              </Select>
            </Field>
            <Field id={`${uid}-interval_minutes`} label="Check every (minutes)" hint="1 to 60." error={errFor("interval_minutes")}>
              <Input
                {...bind("interval_minutes")}
                data
                type="number"
                inputMode="numeric"
                min={1}
                max={60}
                step={1}
                value={draft.interval}
                onChange={(e) => set({ interval: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-timeout_ms`} label="Timeout (seconds)" hint="1 to 30. A slower answer fails." error={errFor("timeout_ms")}>
              <Input
                {...bind("timeout_ms")}
                data
                type="number"
                inputMode="decimal"
                min={1}
                max={30}
                step={0.5}
                value={draft.timeout}
                onChange={(e) => set({ timeout: e.target.value })}
              />
            </Field>
          </div>
        </fieldset>

        <fieldset className="mf-mon-form__part">
          <legend className="mf-mon-form__title">What counts as up</legend>
          <div className="mf-mon-grid">
            <Field id={`${uid}-status_min`} label="Lowest expected status" error={errFor("status_min")}>
              <Input
                {...bind("status_min")}
                data
                type="number"
                inputMode="numeric"
                min={100}
                max={599}
                value={draft.statusMin}
                onChange={(e) => set({ statusMin: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-status_max`} label="Highest expected status" error={errFor("status_max")}>
              <Input
                {...bind("status_max")}
                data
                type="number"
                inputMode="numeric"
                min={100}
                max={599}
                value={draft.statusMax}
                onChange={(e) => set({ statusMax: e.target.value })}
              />
            </Field>
            <Field
              id={`${uid}-keyword`}
              label="Text the page must contain"
              hint="Optional. Case-sensitive."
              error={errFor("keyword")}
              wide
            >
              <Input {...bind("keyword")} value={draft.keyword} onChange={(e) => set({ keyword: e.target.value })} />
            </Field>
            <Field
              id={`${uid}-json_path`}
              label="JSON path"
              hint={'Optional, for a JSON answer. For example "status" or "data.items[0].ok".'}
              error={errFor("json_path")}
            >
              <Input
                {...bind("json_path")}
                data
                spellCheck={false}
                value={draft.jsonPath}
                onChange={(e) => set({ jsonPath: e.target.value })}
              />
            </Field>
            <Field
              id={`${uid}-json_expected`}
              label="Expected value at that path"
              hint="Leave empty to check only that the path exists."
              error={errFor("json_expected")}
            >
              <Input
                {...bind("json_expected")}
                data
                spellCheck={false}
                disabled={!draft.jsonPath.trim() && draft.jsonExpected === ""}
                value={draft.jsonExpected}
                onChange={(e) => set({ jsonExpected: e.target.value })}
              />
            </Field>
          </div>
          <div className="mf-mon-grid">
            <Field label="Certificate" error={errFor("ssl_check")}>
              <Check
                id={`${uid}-ssl_check`}
                checked={draft.sslCheck}
                onChange={(v) => set({ sslCheck: v })}
                label="Check the certificate's expiry date"
                hint="Only for https addresses."
              />
            </Field>
            <Field
              id={`${uid}-ssl_expiry_days`}
              label="Fail when fewer days than this remain"
              hint="1 to 365."
              error={errFor("ssl_expiry_days")}
            >
              <Input
                {...bind("ssl_expiry_days")}
                data
                type="number"
                inputMode="numeric"
                min={1}
                max={365}
                disabled={!draft.sslCheck}
                value={draft.sslDays}
                onChange={(e) => set({ sslDays: e.target.value })}
              />
            </Field>
          </div>
        </fieldset>

        <fieldset className="mf-mon-form__part">
          <legend className="mf-mon-form__title">When to alert</legend>
          <div className="mf-mon-grid">
            <Field
              id={`${uid}-failure_threshold`}
              label="Failed checks in a row before an incident opens"
              hint="1 to 10. One failure can be a network blip; two or three in a row rarely are."
              error={errFor("failure_threshold")}
              wide
            >
              <Input
                {...bind("failure_threshold")}
                data
                type="number"
                inputMode="numeric"
                min={1}
                max={10}
                className="mf-mon-input--short"
                value={draft.threshold}
                onChange={(e) => set({ threshold: e.target.value })}
              />
            </Field>
            <Field
              id={`${uid}-channels.emails`}
              label="Email addresses"
              hint="Separate addresses with commas."
              error={errFor("channels.emails")}
              wide
            >
              <TextArea
                {...bind("channels.emails")}
                rows={2}
                spellCheck={false}
                autoComplete="off"
                placeholder="ops@example.com"
                value={draft.emails}
                onChange={(e) => set({ emails: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-channels.webhook`} label="Webhook address" error={errFor("channels.webhook")}>
              <Input
                {...bind("channels.webhook")}
                data
                type="url"
                spellCheck={false}
                placeholder="https://example.com/hooks/uptime"
                value={draft.webhook}
                onChange={(e) => set({ webhook: e.target.value })}
              />
            </Field>
            <Field id={`${uid}-channels.slack`} label="Slack incoming webhook" error={errFor("channels.slack")}>
              <Input
                {...bind("channels.slack")}
                data
                type="url"
                spellCheck={false}
                placeholder="https://hooks.slack.com/services/…"
                value={draft.slack}
                onChange={(e) => set({ slack: e.target.value })}
              />
            </Field>
          </div>
          {errFor("channels") ? <p className="mf-mon-field__err">{errFor("channels")}</p> : null}
          <p className="mf-mon-quiet">
            Leave all three empty to record incidents on this screen without sending any message.
          </p>
        </fieldset>

        <fieldset className="mf-mon-form__part">
          <legend className="mf-sr">Schedule</legend>
          <Check
            id={`${uid}-enabled`}
            checked={draft.enabled}
            onChange={(v) => set({ enabled: v })}
            label="Run this monitor"
            hint="A paused monitor sends no requests and opens no incidents."
          />
        </fieldset>

        {test.data && !test.isPending ? <CheckOutcome result={test.data} /> : null}

        <div className="mf-mon-acts">
          <Button type="submit" tone="primary" icon={<IconCheck size={14} />} disabled={busy}>
            {busy ? "Saving…" : monitor ? "Save changes" : "Save monitor"}
          </Button>
          <Button disabled={test.isPending || busy} onClick={() => run("test")}>
            {test.isPending ? "Testing…" : "Test now"}
          </Button>
          <Button tone="quiet" disabled={busy} onClick={() => onDone()}>
            Cancel
          </Button>
          {tried && Object.keys(local).length ? (
            <Say tone="warn">Fix the fields marked above, then try again.</Say>
          ) : null}
          {unplaced ? <Say tone="warn">{unplaced}</Say> : null}
        </div>
      </form>
    </Panel>
  );
}

export default MonitorEditor;
