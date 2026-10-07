/**
 * ORGANISATION: who is on the station, and what each of them can touch.
 *
 * Roles are explained on the screen rather than in a tooltip, because "member"
 * and "viewer" mean something different in every product that has them and a
 * wrong guess here hands somebody the delete button.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { IconCheck, IconClose, IconOut, IconPlus } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import {
  Block,
  Foot,
  Note,
  QueryState,
  Result,
  Select,
  Spec,
  SpecRow,
  TextInput,
  TypedConfirm,
  errText,
} from "./SettingsLayout";

export type Role = "owner" | "admin" | "member" | "viewer";

export interface Org {
  id: string;
  name: string;
  slug: string;
  owner_id: string;
  role?: Role;
}

export interface Member {
  user_id: string;
  name: string;
  email: string;
  role: Role;
  created_at?: string;
}

interface Invite {
  id: string;
  email: string;
  role: Role;
  created_at: string;
  expires_at?: string | null;
}

export const ROLE_NAME: Record<Role, string> = {
  owner: "Owner",
  admin: "Admin",
  member: "Member",
  viewer: "Viewer",
};

const ROLE_MEANING: { role: Role; what: string }[] = [
  { role: "owner", what: "Everything an admin can do, plus transferring and deleting the organisation itself. There is always exactly one." },
  { role: "admin", what: "Adds and deletes sites, invites and removes people, issues and revokes API keys, resets a record." },
  { role: "member", what: "Reads every site, and edits goals, funnels, segments, dashboards and alerts. Cannot delete a site or manage people." },
  { role: "viewer", what: "Reads. Nothing on any screen writes, and export is still allowed." },
];

/** The organisation whose settings are on screen. Most people have exactly one. */
export function useCurrentOrg() {
  const orgs = useQuery({
    queryKey: ["settings", "orgs"],
    queryFn: () => api.get<Org[]>("/api/orgs").then((r) => r.data),
    staleTime: 5 * 60_000,
  });
  const [chosen, setChosen] = useState<string | null>(null);
  const orgId = chosen ?? orgs.data?.[0]?.id ?? "";
  const org = orgs.data?.find((o) => o.id === orgId);
  return { orgs, orgId, org, setChosen };
}

export function OrgSettings() {
  const qc = useQueryClient();
  const { orgs, orgId, org, setChosen } = useCurrentOrg();

  const [name, setName] = useState<string | null>(null);
  const [slug, setSlug] = useState<string | null>(null);
  const detail = { name: name ?? org?.name ?? "", slug: slug ?? org?.slug ?? "" };
  const dirty = !!org && (detail.name !== org.name || detail.slug !== org.slug);

  const members = useQuery({
    queryKey: ["settings", "members", orgId],
    queryFn: () => api.get<Member[]>(`/api/orgs/${orgId}/members`).then((r) => r.data),
    enabled: !!orgId,
  });

  const invites = useQuery({
    queryKey: ["settings", "invites", orgId],
    queryFn: () => api.get<Invite[]>(`/api/orgs/${orgId}/invites`).then((r) => r.data),
    enabled: !!orgId,
  });

  const saveOrg = useMutation({
    mutationFn: () => api.patch<Org>(`/api/orgs/${orgId}`, detail),
    onSuccess: () => {
      setName(null);
      setSlug(null);
      void qc.invalidateQueries({ queryKey: ["settings", "orgs"] });
    },
  });

  const setRole = useMutation({
    mutationFn: (v: { userId: string; role: Role }) =>
      api.patch<Member>(`/api/orgs/${orgId}/members/${v.userId}`, { role: v.role }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "members", orgId] }),
  });

  const removeMember = useMutation({
    mutationFn: (userId: string) => api.del<null>(`/api/orgs/${orgId}/members/${userId}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "members", orgId] }),
  });

  const [inviteEmail, setInviteEmail] = useState("");
  const [inviteRole, setInviteRole] = useState<Role>("member");

  const invite = useMutation({
    mutationFn: () =>
      api.post<Invite>(`/api/orgs/${orgId}/invites`, { email: inviteEmail, role: inviteRole }),
    onSuccess: () => {
      setInviteEmail("");
      void qc.invalidateQueries({ queryKey: ["settings", "invites", orgId] });
    },
  });

  const cancelInvite = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/orgs/${orgId}/invites/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "invites", orgId] }),
  });

  const resendInvite = useMutation({
    mutationFn: (id: string) => api.post<null>(`/api/orgs/${orgId}/invites/${id}/resend`),
  });

  const [heir, setHeir] = useState("");
  const transfer = useMutation({
    mutationFn: () => api.post<null>(`/api/orgs/${orgId}/transfer`, { user_id: heir }),
    onSuccess: () => {
      setHeir("");
      void qc.invalidateQueries({ queryKey: ["settings", "members", orgId] });
      void qc.invalidateQueries({ queryKey: ["settings", "orgs"] });
    },
  });

  const memberColumns: Column<Member>[] = [
    {
      key: "who",
      head: "Person",
      render: (m) => (
        <span>
          {m.name || "no name yet"} <span className="mf-data">{m.email}</span>
        </span>
      ),
    },
    {
      key: "role",
      head: "Role",
      width: "22%",
      render: (m) =>
        m.role === "owner" ? (
          <span>Owner</span>
        ) : (
          <Select
            auto
            aria-label={`Role for ${m.email}`}
            value={m.role}
            disabled={setRole.isPending}
            onChange={(e) => setRole.mutate({ userId: m.user_id, role: e.target.value as Role })}
          >
            <option value="admin">Admin</option>
            <option value="member">Member</option>
            <option value="viewer">Viewer</option>
          </Select>
        ),
    },
    {
      key: "since",
      head: "Joined",
      width: "16%",
      render: (m) => <span className="mf-data">{m.created_at ? ago(m.created_at) : "—"}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (m) =>
        m.role === "owner" ? (
          <span className="mf-set-note">Cannot be removed</span>
        ) : (
          <Button
            size="sm"
            tone="quiet"
            icon={<IconClose size={12} />}
            disabled={removeMember.isPending}
            onClick={() => removeMember.mutate(m.user_id)}
          >
            Remove
          </Button>
        ),
    },
  ];

  const inviteColumns: Column<Invite>[] = [
    { key: "email", head: "Invited", render: (i) => <span className="mf-data">{i.email}</span> },
    { key: "role", head: "As", width: "16%", render: (i) => <span>{ROLE_NAME[i.role]}</span> },
    {
      key: "sent",
      head: "Sent",
      width: "16%",
      render: (i) => <span className="mf-data">{ago(i.created_at)}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (i) => (
        <span className="mf-set-row">
          <Button
            size="sm"
            tone="quiet"
            icon={<IconOut size={12} />}
            disabled={resendInvite.isPending}
            onClick={() => resendInvite.mutate(i.id)}
          >
            Send again
          </Button>
          <Button
            size="sm"
            tone="quiet"
            icon={<IconClose size={12} />}
            disabled={cancelInvite.isPending}
            onClick={() => cancelInvite.mutate(i.id)}
          >
            Cancel
          </Button>
        </span>
      ),
    },
  ];

  return (
    <>
      <Panel>
        <PanelHead
          title="Organisation"
          note="Sites, people and API keys all belong to an organisation rather than to a person, so nothing is lost when someone leaves."
          actions={
            (orgs.data?.length ?? 0) > 1 ? (
              <Select
                auto
                aria-label="Organisation"
                value={orgId}
                onChange={(e) => setChosen(e.target.value)}
              >
                {orgs.data?.map((o) => (
                  <option key={o.id} value={o.id}>
                    {o.name}
                  </option>
                ))}
              </Select>
            ) : undefined
          }
        />
        <QueryState query={orgs} emptyWhen={(list) => !list.length} empty={<Empty title="You are not in an organisation" body="Every site belongs to one. Create an organisation from the site chooser to begin." />}>
          {() => (
            <>
              <Spec>
                <SpecRow name="Name" htmlFor="org-name">
                  <TextInput id="org-name" value={detail.name} onChange={(e) => setName(e.target.value)} />
                </SpecRow>
                <SpecRow
                  name="Slug"
                  hint="Appears in share addresses and in the API. Lower case, dashes, no spaces."
                  htmlFor="org-slug"
                >
                  <TextInput
                    id="org-slug"
                    data
                    spellCheck={false}
                    value={detail.slug}
                    onChange={(e) => setSlug(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "-"))}
                  />
                </SpecRow>
              </Spec>
              <Foot>
                <Button
                  tone="primary"
                  icon={<IconCheck size={14} />}
                  disabled={!dirty || saveOrg.isPending}
                  onClick={() => saveOrg.mutate()}
                >
                  Save organisation
                </Button>
                <Button
                  tone="quiet"
                  disabled={!dirty}
                  onClick={() => {
                    setName(null);
                    setSlug(null);
                  }}
                >
                  Discard
                </Button>
                <span className="mf-set-foot__spacer" />
                <Result action={saveOrg} done="Organisation saved." />
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead title="Members" note="A role here applies to every site in the organisation. Per-site overrides live on the teams and access screen." />
        <QueryState query={members}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={memberColumns}
                getKey={(m) => m.user_id}
                empty={<Empty title="Only you" body="Invite someone below and they appear here once they accept." />}
              />
              {setRole.isError || removeMember.isError ? (
                <Foot tight>
                  <span className="mf-set-foot__say" data-tone="warn" role="alert">
                    {errText(setRole.error ?? removeMember.error)}
                  </span>
                </Foot>
              ) : null}
            </>
          )}
        </QueryState>

        <Block
          title="Transfer ownership"
          body="An organisation has exactly one owner. Handing it over makes you an admin and cannot be undone by you afterwards."
        >
          <Spec>
            <SpecRow name="New owner" htmlFor="org-heir">
              <Select
                id="org-heir"
                value={heir}
                onChange={(e) => setHeir(e.target.value)}
                disabled={!members.data?.length}
              >
                <option value="">Choose a member</option>
                {(members.data ?? [])
                  .filter((m) => m.role !== "owner")
                  .map((m) => (
                    <option key={m.user_id} value={m.user_id}>
                      {m.name || m.email}
                    </option>
                  ))}
              </Select>
            </SpecRow>
          </Spec>
          {heir ? (
            <TypedConfirm
              phrase={org?.slug ?? "organisation"}
              label="Transfer ownership"
              busy={transfer.isPending}
              destroys={
                <>
                  Makes the chosen member the owner of {org?.name ?? "this organisation"} and demotes you to
                  admin. They can then remove you, delete sites and delete the organisation. Only they can
                  hand it back.
                </>
              }
              onConfirm={() => transfer.mutate()}
            />
          ) : (
            <Note>Choose a member and the confirmation appears.</Note>
          )}
          <div className="mf-set-row">
            <Result action={transfer} done="Ownership transferred." working="Transferring…" />
          </div>
        </Block>
      </Panel>

      <Panel>
        <PanelHead title="Invitations" note="An invitation is an address and a role. It becomes a member the moment it is accepted." />
        <QueryState query={invites}>
          {(rows) => (
            <Sheet
              rows={rows}
              columns={inviteColumns}
              getKey={(i) => i.id}
              empty={<Empty title="Nothing outstanding" body="Invitations waiting to be accepted appear here." />}
            />
          )}
        </QueryState>
        <Spec>
          <SpecRow name="Invite someone" htmlFor="inv-email">
            <div className="mf-set-row">
              <TextInput
                id="inv-email"
                type="email"
                placeholder="name@example.com"
                value={inviteEmail}
                onChange={(e) => setInviteEmail(e.target.value)}
              />
              <Select
                auto
                aria-label="Role for the invitation"
                value={inviteRole}
                onChange={(e) => setInviteRole(e.target.value as Role)}
              >
                <option value="admin">Admin</option>
                <option value="member">Member</option>
                <option value="viewer">Viewer</option>
              </Select>
            </div>
          </SpecRow>
        </Spec>
        <Foot>
          <Button
            tone="primary"
            icon={<IconPlus size={14} />}
            disabled={!inviteEmail.includes("@") || invite.isPending}
            onClick={() => invite.mutate()}
          >
            Send the invitation
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={invite} done="Invitation sent." working="Sending…" />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead title="What the roles mean" />
        <div className="mf-set-block">
          <div className="mf-set-ledger">
            {ROLE_MEANING.map((r) => (
              <div className="mf-set-ledger__row" key={r.role}>
                <span className="mf-set-ledger__tick" aria-hidden="true" />
                <span>
                  <span className="mf-set-ledger__what">{ROLE_NAME[r.role]}</span>: {r.what}
                </span>
              </div>
            ))}
          </div>
        </div>
      </Panel>
    </>
  );
}

export default OrgSettings;
