/**
 * INGEST GUARDS: which addresses and hostnames this site records hits from.
 *
 * Three decisions shape the screen:
 *
 *   1. THE RULES ARE STATED, IN ORDER. The deny list always wins; a non-empty
 *      allow list admits only the addresses inside it; a hit naming a hostname
 *      that is not the site's is not recorded. The screen says this in words
 *      above the lists, so nobody has to guess which list beats which.
 *   2. A BLOCKED HIT IS NOT AN ERROR. The tracker is answered as if the hit was
 *      accepted, so a visitor's page never breaks. The only trace is the count
 *      by reason at the bottom, and the daily email when it passes a threshold.
 *   3. ONE SAVE FOR THE WHOLE SCREEN. The lists, the hostnames and the
 *      threshold are one setting on the server, and a refusal names the list it
 *      is about, next to that list.
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Panel, PanelHead } from "../../components/react/ui";
import { IconClose, IconPlus } from "../../components/react/icons";
import { ApiError, api } from "../../lib/api";
import { num } from "../../lib/format";
import { useSiteRecord } from "./SiteSettings";
import {
  Block,
  Field,
  NumberInput,
  QueryState,
  Result,
  Stated,
  TextInput,
  useSyncedForm,
} from "./SettingsLayout";
import "../../styles/app/workspace.css";

/* ── what the server sends ───────────────────────────────────────────────── */

interface Entry {
  cidr: string;
  note: string;
}

type Reason = "denylist" | "not_allowlisted" | "hostname";

interface Guards {
  allow: Entry[];
  deny: Entry[];
  alert_threshold: number;
  primary_domain: string;
  domains: string[];
  blocked: {
    days: ({ day: string } & Record<Reason, number>)[];
    totals: Record<Reason, number>;
  };
}

interface GuardForm {
  allow: Entry[];
  deny: Entry[];
  alert_threshold: number;
  domains: string[];
}

const REASONS: { key: Reason; label: string }[] = [
  { key: "denylist", label: "On the deny list" },
  { key: "not_allowlisted", label: "Not on the allow list" },
  { key: "hostname", label: "Unknown hostname" },
];

const MAX_ENTRIES = 200;

function dayLabel(day: string): string {
  const d = new Date(`${day}T00:00:00`);
  return Number.isNaN(d.getTime()) ? day : d.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
}

/* ── one list of addresses ───────────────────────────────────────────────── */

function CidrList({
  name,
  entries,
  onChange,
  error,
  placeholder,
}: {
  name: string;
  entries: Entry[];
  onChange: (next: Entry[]) => void;
  error?: string;
  placeholder: string;
}) {
  const set = (i: number, patch: Partial<Entry>) =>
    onChange(entries.map((e, j) => (j === i ? { ...e, ...patch } : e)));
  return (
    <div className="mf-ws-cidrs">
      {entries.length === 0 ? <p className="mf-ws-cidrs__empty">No entries.</p> : null}
      {entries.map((e, i) => (
        <div className="mf-ws-cidr" key={i}>
          <TextInput
            data
            spellCheck={false}
            autoCapitalize="off"
            aria-label={`${name}, address or range ${i + 1}`}
            placeholder={placeholder}
            value={e.cidr}
            aria-invalid={error ? true : undefined}
            onChange={(ev) => set(i, { cidr: ev.target.value })}
          />
          <TextInput
            aria-label={`${name}, note ${i + 1}`}
            placeholder="Note, such as “office VPN”"
            maxLength={120}
            value={e.note}
            onChange={(ev) => set(i, { note: ev.target.value })}
          />
          <Button
            tone="quiet"
            size="sm"
            icon={<IconClose size={13} />}
            aria-label={`Remove ${e.cidr || "this entry"} from the ${name.toLowerCase()}`}
            onClick={() => onChange(entries.filter((_, j) => j !== i))}
          >
            Remove
          </Button>
        </div>
      ))}
      {error ? (
        <p className="mf-set-field__err" role="alert">
          {error}
        </p>
      ) : null}
      <div>
        <Button
          size="sm"
          icon={<IconPlus size={13} />}
          disabled={entries.length >= MAX_ENTRIES}
          onClick={() => onChange([...entries, { cidr: "", note: "" }])}
        >
          Add an address or range
        </Button>
      </div>
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function IngestGuards() {
  const { id } = useSiteRecord();
  const qc = useQueryClient();
  const key = ["settings", "ingest-guards", id];

  const guards = useQuery({
    queryKey: key,
    queryFn: () => api.get<Guards>(`/api/sites/${id}/ingest-guards`).then((r) => r.data),
    enabled: !!id,
  });

  const form = useSyncedForm<GuardForm>(
    guards.data
      ? {
          allow: guards.data.allow,
          deny: guards.data.deny,
          alert_threshold: guards.data.alert_threshold,
          domains: guards.data.domains,
        }
      : undefined,
  );

  const save = useMutation({
    mutationFn: (body: GuardForm) =>
      api
        .patch<Guards>(`/api/sites/${id}/ingest-guards`, {
          allow: body.allow.filter((e) => e.cidr.trim()).map((e) => ({ cidr: e.cidr.trim(), note: e.note.trim() })),
          deny: body.deny.filter((e) => e.cidr.trim()).map((e) => ({ cidr: e.cidr.trim(), note: e.note.trim() })),
          alert_threshold: body.alert_threshold,
          domains: body.domains.map((d) => d.trim()).filter(Boolean),
        })
        .then((r) => r.data),
    onSuccess: (data) => qc.setQueryData(key, data),
  });

  const fieldError = (field: string) =>
    save.error instanceof ApiError && save.error.field === field ? save.error.message : undefined;
  const elsewhere =
    save.isError && !["allow", "deny", "alert_threshold", "domains"].includes((save.error as ApiError)?.field ?? "");

  return (
    <Panel>
      <PanelHead
        title="Ingest guards"
        note="Which addresses and hostnames this site records hits from. A blocked hit is answered as if it was accepted, so a visitor's page never breaks."
      />
      <QueryState query={guards}>
        {(g) => {
          const f = form.form;
          if (!f) return null;
          return (
            <>
              <Block title="How the rules apply">
                <ol className="mf-ws-rules">
                  <li>An address on the deny list is never recorded. The deny list always wins.</li>
                  <li>
                    When the allow list has entries, only addresses inside it are recorded. When it is empty, every
                    address is.
                  </li>
                  <li>A hit that names a hostname which is not one of this site's is not recorded.</li>
                </ol>
              </Block>

              <Block
                title="Deny list"
                body="Single addresses (203.0.113.7) or ranges in CIDR form (203.0.113.0/24, 2001:db8::/32). Up to 200 entries."
              >
                <CidrList
                  name="Deny list"
                  entries={f.deny}
                  placeholder="203.0.113.0/24"
                  error={fieldError("deny")}
                  onChange={(deny) => form.set({ deny })}
                />
              </Block>

              <Block
                title="Allow list"
                body="Leave this empty to record every address that is not denied. One entry here turns it on, and then only the listed addresses are recorded."
              >
                <CidrList
                  name="Allow list"
                  entries={f.allow}
                  placeholder="198.51.100.0/24"
                  error={fieldError("allow")}
                  onChange={(allow) => form.set({ allow })}
                />
              </Block>

              <Block
                title="Hostnames"
                body="Hits are recorded from the site's own domain and its subdomains. Add other hostnames that serve the same site, such as a docs domain."
              >
                <Field label="Primary domain">
                  <Stated>
                    <span className="mf-data">{g.primary_domain}</span> (change it in Site details)
                  </Stated>
                </Field>
                <div className="mf-ws-cidrs">
                  {f.domains.map((d, i) => (
                    <div className="mf-set-row" key={i}>
                      <TextInput
                        data
                        spellCheck={false}
                        autoCapitalize="off"
                        aria-label={`Extra hostname ${i + 1}`}
                        placeholder="docs.example.com"
                        value={d}
                        onChange={(e) =>
                          form.set({ domains: f.domains.map((x, j) => (j === i ? e.target.value : x)) })
                        }
                      />
                      <Button
                        tone="quiet"
                        size="sm"
                        icon={<IconClose size={13} />}
                        aria-label={`Remove ${d || "this hostname"}`}
                        onClick={() => form.set({ domains: f.domains.filter((_, j) => j !== i) })}
                      >
                        Remove
                      </Button>
                    </div>
                  ))}
                  {fieldError("domains") ? (
                    <p className="mf-set-field__err" role="alert">
                      {fieldError("domains")}
                    </p>
                  ) : null}
                  <div>
                    <Button
                      size="sm"
                      icon={<IconPlus size={13} />}
                      disabled={f.domains.length >= 32}
                      onClick={() => form.set({ domains: [...f.domains, ""] })}
                    >
                      Add a hostname
                    </Button>
                  </div>
                </div>
              </Block>

              <Block title="Daily email">
                <Field
                  label="Send an email when at least this many requests are blocked in a day"
                  htmlFor="guard-threshold"
                  hint="0 turns the email off. The email goes to the site's admins the next day."
                  error={fieldError("alert_threshold")}
                >
                  <NumberInput
                    id="guard-threshold"
                    wide
                    min={0}
                    step={1}
                    value={Number.isFinite(f.alert_threshold) ? f.alert_threshold : 0}
                    onChange={(e) => {
                      const n = Math.max(0, Math.floor(Number(e.target.value) || 0));
                      form.set({ alert_threshold: n });
                    }}
                  />
                </Field>
              </Block>

              <div className="mf-set-foot">
                <Button tone="primary" disabled={!form.dirty || save.isPending} onClick={() => save.mutate(f)}>
                  Save the guards
                </Button>
                <Button tone="quiet" disabled={!form.dirty || save.isPending} onClick={() => form.reset()}>
                  Discard changes
                </Button>
                {elsewhere || save.isPending || save.isSuccess ? <Result action={save} done="Saved. New hits follow these rules within a minute." /> : null}
              </div>

              <Block
                title="Blocked in the last 7 days"
                body="Requests that were not recorded, by reason. Counts are kept for 9 days."
              >
                <div className="mf-sheet-wrap">
                  <table className="mf-sheet mf-ws-blocked">
                    <thead>
                      <tr>
                        <th scope="col">Day</th>
                        {REASONS.map((r) => (
                          <th scope="col" className="is-num" key={r.key}>
                            {r.label}
                          </th>
                        ))}
                      </tr>
                    </thead>
                    <tbody>
                      {g.blocked.days.map((d) => (
                        <tr key={d.day}>
                          <td>
                            <time dateTime={d.day}>{dayLabel(d.day)}</time>
                          </td>
                          {REASONS.map((r) => (
                            <td className="is-num" key={r.key}>
                              {num(d[r.key] ?? 0)}
                            </td>
                          ))}
                        </tr>
                      ))}
                    </tbody>
                    <tfoot>
                      <tr>
                        <td>Total</td>
                        {REASONS.map((r) => (
                          <td className="is-num" key={r.key}>
                            {num(g.blocked.totals[r.key] ?? 0)}
                          </td>
                        ))}
                      </tr>
                    </tfoot>
                  </table>
                </div>
              </Block>
            </>
          );
        }}
      </QueryState>
    </Panel>
  );
}

export default IngestGuards;
