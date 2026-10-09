/**
 * API KEYS.
 *
 * The warning that a key is legible exactly once is placed BEFORE the button
 * that issues it, not in the panel that appears afterwards. Micaforge stores
 * only the hash, so "we cannot show it to you again" is a fact about the
 * server rather than a policy, and the screen says which it is.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  Button,
  CopyField,
  Empty,
  Panel,
  PanelHead,
  Sheet,
  type Column,
} from "../../components/react/ui";
import { IconClose, IconPlus } from "../../components/react/icons";
import { publicOrigin, api, type SiteSummary } from "../../lib/api";
import { ago } from "../../lib/format";
import {
  Block,
  Caution,
  CheckRow,
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
import { useCurrentOrg } from "./OrgSettings";

interface ApiKeyRecord {
  id: string;
  name: string;
  prefix: string;
  scopes: string[];
  site_id: string | null;
  last_used_at: string | null;
  expires_at: string | null;
}

interface IssuedKey extends ApiKeyRecord {
  /** The only time the plaintext exists outside the caller's own notes. */
  key: string;
}

/**
 * The scope vocabulary, exactly as the server checks it (`routes/keys.rs`
 * SCOPES). A name the server does not know is refused when the key is made,
 * so this list and that one must say the same thing.
 */
const SCOPES: { key: string; label: string; hint: string }[] = [
  {
    key: "stats:read",
    label: "Read analytics",
    hint: "Every report under /api/stats, AI crawler traffic, the Citation gap, the robots.txt check, CSV and PDF export, and the MCP server.",
  },
  {
    key: "manage:read",
    label: "Read site configuration",
    hint: "The site list, its settings, goals, funnels, segments, alerts and imports.",
  },
  {
    key: "manage:write",
    label: "Change site configuration",
    hint: "Create and edit goals, funnels, segments, alerts and imports. A key never deletes a site or reads its secrets.",
  },
  {
    key: "ingest:write",
    label: "Send server logs",
    hint: "POST /api/log/edge, for the log shipper or a server middleware.",
  },
  {
    key: "export:read",
    label: "Export raw data",
    hint: "GET /api/export/raw: every recorded row for a site and date range, as a ZIP. Not implied by Read analytics.",
  },
  {
    key: "audit:read",
    label: "Read the audit log",
    hint: "The organisation's audit log, for a security tool. Only for a key not bound to one site.",
  },
];

export function ApiKeysSettings() {
  const qc = useQueryClient();
  const { orgId } = useCurrentOrg();

  const keys = useQuery({
    queryKey: ["settings", "keys", orgId],
    queryFn: () => api.get<ApiKeyRecord[]>(`/api/keys?org=${encodeURIComponent(orgId)}`).then((r) => r.data),
    enabled: !!orgId,
  });

  const sites = useQuery({
    queryKey: ["settings", "org-sites", orgId],
    queryFn: () => api.get<SiteSummary[]>(`/api/sites?org=${encodeURIComponent(orgId)}`).then((r) => r.data),
    enabled: !!orgId,
  });

  const [name, setName] = useState("");
  const [scopes, setScopes] = useState<string[]>(["stats:read"]);
  const [siteScope, setSiteScope] = useState("");
  const [expiry, setExpiry] = useState("365");
  const [issued, setIssued] = useState<IssuedKey | null>(null);

  const create = useMutation({
    mutationFn: () => {
      const days = expiry === "never" ? null : Number(expiry);
      return api
        .post<IssuedKey>("/api/keys", {
          org_id: orgId,
          name,
          scopes,
          site_id: siteScope || null,
          expires_at: days ? new Date(Date.now() + days * 86_400_000).toISOString() : null,
        })
        .then((r) => r.data);
    },
    onSuccess: (k) => {
      setIssued(k);
      setName("");
      void qc.invalidateQueries({ queryKey: ["settings", "keys", orgId] });
    },
  });

  const revoke = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/keys/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "keys", orgId] }),
  });

  const columns: Column<ApiKeyRecord>[] = [
    {
      key: "name",
      head: "Key",
      render: (k) => (
        <span>
          {k.name || "unnamed"} <span className="mf-data">{k.prefix}…</span>
        </span>
      ),
    },
    {
      key: "scopes",
      head: "Can do",
      width: "26%",
      render: (k) => (
        <span>
          {k.scopes?.length
            ? k.scopes.map((s) => SCOPES.find((x) => x.key === s)?.label ?? s).join(", ")
            : "nothing"}
        </span>
      ),
    },
    {
      key: "site",
      head: "Scope",
      width: "16%",
      render: (k) => (
        <span>
          {k.site_id
            ? sites.data?.find((s) => s.id === k.site_id)?.domain ?? "one site"
            : "All sites"}
        </span>
      ),
    },
    {
      key: "used",
      head: "Last used",
      width: "14%",
      render: (k) => <span className="mf-data">{k.last_used_at ? ago(k.last_used_at) : "never"}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (k) => (
        <Button
          size="sm"
          tone="quiet"
          icon={<IconClose size={12} />}
          disabled={revoke.isPending}
          onClick={() => revoke.mutate(k.id)}
        >
          Revoke
        </Button>
      ),
    },
  ];

  return (
    <>
      {issued ? (
        <Panel>
          <PanelHead title="Copy this key now" />
          <Block
            title={issued.name || "New key"}
            body="This is the only time the key is legible. Micaforge holds a hash of it and nothing else, so it genuinely cannot be shown again: a lost key is replaced, never recovered."
          >
            <CopyField label="Key" value={issued.key} />
            <div className="mf-set-row">
              <Button tone="primary" onClick={() => setIssued(null)}>
                I have copied it
              </Button>
              <Note>Closing this panel is the last chance.</Note>
            </div>
          </Block>
        </Panel>
      ) : null}

      <Panel>
        <PanelHead
          title="Keys"
          note="A key carries the scopes it was issued with and nothing more. Revoking one takes effect on the next request it makes."
        />
        <QueryState query={keys}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={columns}
                getKey={(k) => k.id}
                empty={
                  <Empty
                    title="No key has been issued"
                    body="A key lets a script, a CI job or an AI agent read this organisation's data without a browser session."
                  />
                }
              />
              {revoke.isError ? (
                <Foot tight>
                  <span className="mf-set-foot__say" data-tone="warn" role="alert">
                    {errText(revoke.error)}
                  </span>
                </Foot>
              ) : null}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead title="Issue a key" />
        <Spec>
          <SpecRow name="Name" hint="For you, so a key found in a log can be traced to what uses it." htmlFor="key-name">
            <TextInput
              id="key-name"
              placeholder="CI status badge"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </SpecRow>

          <SpecRow name="What it can do" hint="Unticked scopes return 403 to this key, on every endpoint.">
            <div className="mf-set-checks">
              {SCOPES.map((s) => (
                <CheckRow
                  key={s.key}
                  checked={scopes.includes(s.key)}
                  onChange={(v) => setScopes((cur) => (v ? [...cur, s.key] : cur.filter((x) => x !== s.key)))}
                  label={s.label}
                  hint={s.hint}
                />
              ))}
            </div>
          </SpecRow>

          <SpecRow name="Which sites" htmlFor="key-site">
            <Select id="key-site" value={siteScope} onChange={(e) => setSiteScope(e.target.value)}>
              <option value="">Every site in this organisation</option>
              {(sites.data ?? []).map((s) => (
                <option key={s.id} value={s.id}>
                  {s.domain}
                </option>
              ))}
            </Select>
          </SpecRow>

          <SpecRow name="Expires" hint="A key that never expires is a key nobody ever rotates." htmlFor="key-exp">
            <Select id="key-exp" value={expiry} onChange={(e) => setExpiry(e.target.value)}>
              <option value="30">In 30 days</option>
              <option value="90">In 90 days</option>
              <option value="365">In a year</option>
              <option value="never">Never</option>
            </Select>
          </SpecRow>
        </Spec>

        <Foot>
          <Caution>
            The key is shown once, on this screen, immediately after it is created. Have somewhere to put it
            before you press this.
          </Caution>
        </Foot>
        <Foot tight>
          <Button
            tone="primary"
            icon={<IconPlus size={14} />}
            disabled={!name.trim() || !scopes.length || create.isPending}
            onClick={() => create.mutate()}
          >
            Issue the key
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={create} done="Key issued. Copy it above." working="Issuing…" />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead
          title="Using a key"
          note="Send it as a bearer token. Every read endpoint takes the same query envelope as the dashboard does."
        />
        <div className="mf-set-block">
          <CopyField
            label="An example read"
            value={`curl -H "Authorization: Bearer ${issued?.key ?? "mfg_your_key"}" \\\n  "${publicOrigin()}/api/stats/overview?site=1&range=7d&audience=all"`}
          />
        </div>
      </Panel>
    </>
  );
}

export default ApiKeysSettings;
