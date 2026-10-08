/**
 * SHARING: publishing the dashboard.
 *
 * Open-source projects routinely publish their own traffic as a trust signal,
 * so this is a first-class screen rather than a hidden switch. Everything that
 * follows says exactly what a reader of the link can see, because the failure
 * mode of a share feature is somebody learning what it exposed afterwards.
 */
import { useEffect, useRef, useState } from "react";
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
import { IconCheck, IconClose, IconCopy, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  asSite,
  Caution,
  CheckRow,
  Foot,
  Note,
  QueryState,
  Result,
  ResultFoot,
  Select,
  Spec,
  SpecRow,
  TextInput,
  Toggle,
  errText,
} from "./SettingsLayout";
import { useSiteRecord, type SiteRecord } from "./SiteSettings";

interface ShareLink {
  id: string;
  slug: string;
  has_password: boolean;
  expires_at: string | null;
  scopes: string[];
}

interface ShareState {
  public: boolean;
  public_slug: string | null;
  links: ShareLink[];
}

const SCOPES: { key: string; label: string; hint: string }[] = [
  { key: "overview", label: "Dashboard", hint: "Totals and the main chart, for people and AI crawlers." },
  { key: "agents", label: "AI crawlers", hint: "Which crawlers fetched what, and how often." },
  { key: "citation_gap", label: "Citation gap", hint: "What was taken against what came back." },
  { key: "pages", label: "Pages", hint: "Paths, entries, exits and time on page." },
  { key: "sources", label: "Sources", hint: "Channels, referrers and campaigns." },
  { key: "locations", label: "Locations", hint: "Country, region and city." },
  { key: "devices", label: "Devices", hint: "Browser, system, device and screen." },
];

const ALL_SCOPES = SCOPES.map((s) => s.key);

function origin(): string {
  return typeof window === "undefined" ? "" : window.location.origin;
}

/** A copy control small enough to live in a table row. */
function RowCopy({ value }: { value: string }) {
  const [done, setDone] = useState(false);
  const t = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(t.current), []);
  return (
    <Button
      size="sm"
      tone="quiet"
      icon={done ? <IconCheck size={12} /> : <IconCopy size={12} />}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(value);
          setDone(true);
          t.current = window.setTimeout(() => setDone(false), 1600);
        } catch {
          /* clipboard refused: the address is written in the row and selectable */
        }
      }}
    >
      {done ? "Copied" : "Copy"}
    </Button>
  );
}

export function ShareSettings() {
  const { siteId, domain } = useSite();
  const { id: recordId } = useSiteRecord();
  const qc = useQueryClient();
  const key = ["settings", "share", siteId];

  const share = useQuery({
    queryKey: key,
    queryFn: () => api.get<ShareState>("/api/share", { site: asSite(siteId) }).then((r) => r.data),
    enabled: siteId != null,
  });

  const setPublic = useMutation({
    mutationFn: (v: boolean) => api.patch<SiteRecord>(`/api/sites/${recordId}`, { public: v }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: key });
      void qc.invalidateQueries({ queryKey: ["settings", "site", recordId] });
    },
  });

  const [password, setPassword] = useState("");
  const [expiry, setExpiry] = useState("never");
  const [scopes, setScopes] = useState<string[]>(ALL_SCOPES);

  const createLink = useMutation({
    mutationFn: () => {
      const days = expiry === "never" ? null : Number(expiry);
      // The site travels in the query, like every site-scoped call: the server
      // authorises against `?site=` before it reads the body.
      return api.post<ShareLink>(`/api/share?site=${encodeURIComponent(String(asSite(siteId)))}`, {
        password: password || null,
        expires_at: days ? new Date(Date.now() + days * 86_400_000).toISOString() : null,
        scopes,
      });
    },
    onSuccess: () => {
      setPassword("");
      setExpiry("never");
      setScopes(ALL_SCOPES);
      void qc.invalidateQueries({ queryKey: key });
    },
  });

  const revoke = useMutation({
    mutationFn: (linkId: string) => api.del<null>(`/api/share/${linkId}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: key }),
  });

  const [embedSlug, setEmbedSlug] = useState("");

  const columns: Column<ShareLink>[] = [
    {
      key: "slug",
      head: "Address",
      render: (r) => (
        <>
          <span className="mf-data">/share/{r.slug}</span>
          <RowCopy value={`${origin()}/share/${r.slug}`} />
        </>
      ),
    },
    {
      key: "pw",
      head: "Password",
      width: "14%",
      render: (r) => <span>{r.has_password ? "Set" : "None"}</span>,
    },
    {
      key: "exp",
      head: "Expires",
      width: "18%",
      render: (r) => (
        <span className="mf-data">{r.expires_at ? ago(r.expires_at) : "never"}</span>
      ),
    },
    {
      key: "scopes",
      head: "Shows",
      width: "20%",
      render: (r) => (
        <span>
          {!r.scopes?.length || r.scopes.length === ALL_SCOPES.length
            ? "Everything"
            : r.scopes
                .map((s) => SCOPES.find((x) => x.key === s)?.label ?? s)
                .join(", ")}
        </span>
      ),
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (r) => (
        <Button
          size="sm"
          tone="quiet"
          icon={<IconClose size={12} />}
          disabled={revoke.isPending}
          onClick={() => revoke.mutate(r.id)}
        >
          Revoke
        </Button>
      ),
    },
  ];

  const embedTargets = (s: ShareState) => [
    ...(s.public && s.public_slug ? [{ slug: s.public_slug, label: "The public dashboard" }] : []),
    ...s.links.map((l) => ({ slug: l.slug, label: `/share/${l.slug}` })),
  ];

  return (
    <>
      <Panel>
        <PanelHead
          title="Public dashboard"
          note="Anyone with the address can read it, with no account and no sign-in."
        />
        <QueryState query={share}>
          {(s) => (
            <>
              <Spec>
                <SpecRow
                  name="Publish this site"
                  hint="Shows every aggregate figure this site records, including page paths and the Citation Gap. It never shows a session, a visitor row, a replay or an error stack."
                >
                  <Toggle
                    label="Publish this site's dashboard"
                    checked={s.public}
                    disabled={setPublic.isPending || !recordId}
                    onChange={(v) => setPublic.mutate(v)}
                  />
                </SpecRow>

              </Spec>
              {s.public ? (
                <div className="mf-set-block">
                  {s.public_slug ? (
                    <CopyField label="Public address" value={`${origin()}/share/${s.public_slug}`} />
                  ) : (
                    <Note>The address appears here as soon as the server has issued it.</Note>
                  )}
                </div>
              ) : null}
              <ResultFoot action={setPublic} done="Saved." working="Saving…" />
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Private links"
          note="An address that works without an account, optionally behind a password and an expiry date. Revoking one stops it immediately."
        />
        <QueryState query={share}>
          {(s) => (
            <>
              <Sheet
                rows={s.links}
                columns={columns}
                getKey={(r) => r.id}
                empty={
                  <Empty
                    title="No private link exists"
                    body="Create one below to give a client, a colleague or an investor a read-only view without adding them to the organisation."
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

        <Spec>
          <SpecRow
            name="Password"
            hint="Optional. Stored as a hash; it cannot be read back, only replaced."
            htmlFor="share-pw"
          >
            <TextInput
              id="share-pw"
              type="password"
              autoComplete="new-password"
              placeholder="Leave empty for no password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </SpecRow>
          <SpecRow name="Expires" htmlFor="share-exp">
            <Select id="share-exp" value={expiry} onChange={(e) => setExpiry(e.target.value)}>
              <option value="never">Never</option>
              <option value="7">In 7 days</option>
              <option value="30">In 30 days</option>
              <option value="90">In 90 days</option>
            </Select>
          </SpecRow>
          <SpecRow name="What the link shows" hint="Unticked screens return nothing at all to this link.">
            <div className="mf-set-checks mf-set-checks--wide">
              {SCOPES.map((sc) => (
                <CheckRow
                  key={sc.key}
                  checked={scopes.includes(sc.key)}
                  onChange={(v) =>
                    setScopes((cur) => (v ? [...cur, sc.key] : cur.filter((x) => x !== sc.key)))
                  }
                  label={sc.label}
                  hint={sc.hint}
                />
              ))}
            </div>
          </SpecRow>
        </Spec>

        <Foot>
          <Button
            tone="primary"
            icon={<IconPlus size={14} />}
            disabled={!scopes.length || createLink.isPending}
            onClick={() => createLink.mutate()}
          >
            Create the link
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={createLink} done="Link created." working="Creating…" />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead
          title="Embed"
          note="Puts a share address into a page of your own. The embedded view has no navigation and no account controls."
        />
        <QueryState query={share}>
          {(s) => {
            const targets = embedTargets(s);
            if (!targets.length)
              return (
                <Empty
                  title="Nothing to embed yet"
                  body="Publish the dashboard or create a private link, and its embed snippet appears here."
                />
              );
            const chosen = targets.find((t) => t.slug === embedSlug) ?? targets[0];
            return (
              <>
                <Spec>
                  <SpecRow name="Address to embed" htmlFor="embed-target">
                    <Select
                      id="embed-target"
                      value={chosen.slug}
                      onChange={(e) => setEmbedSlug(e.target.value)}
                    >
                      {targets.map((t) => (
                        <option key={t.slug} value={t.slug}>
                          {t.label}
                        </option>
                      ))}
                    </Select>
                  </SpecRow>
                  <SpecRow name="Snippet" stack>
                    <CopyField
                      label="HTML"
                      value={`<iframe\n  src="${origin()}/share/${chosen.slug}?embed=1"\n  title="Micaforge, ${domain ?? "this site"}"\n  width="100%" height="760" loading="lazy"\n  style="border:0"></iframe>`}
                    />
                  </SpecRow>
                </Spec>
                <Foot tight>
                  <Caution>
                    A password-protected link asks for its password inside the frame, which most embeds do not
                    want. Embed the public dashboard, or a link with no password.
                  </Caution>
                </Foot>
              </>
            );
          }}
        </QueryState>
      </Panel>
    </>
  );
}

export default ShareSettings;
