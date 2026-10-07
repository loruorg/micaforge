/**
 * TEAMS AND ACCESS.
 *
 * Two different things live here on purpose. A TEAM is a group of people you
 * want to name and address together. Per-site ACCESS is an override on one
 * person for one site, because that is what `site_access` actually stores: a
 * role per user per site. Presenting an override as if it applied to a team
 * would be a comforting lie about what the server will enforce.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { IconClose, IconPlus } from "../../components/react/icons";
import { api, type SiteSummary } from "../../lib/api";
import {
  Note,
  QueryState,
  ResultFoot,
  Select,
  Spec,
  SpecRow,
  TextInput,
  errText,
} from "./SettingsLayout";
import { ROLE_NAME, useCurrentOrg, type Member, type Role } from "./OrgSettings";

interface Team {
  id: string;
  name: string;
  member_count?: number;
}

interface TeamMember {
  user_id: string;
  name: string;
  email: string;
}

interface AccessRow {
  site_id: string;
  role: Role;
}

export function TeamsSettings() {
  const qc = useQueryClient();
  const { orgs, orgId } = useCurrentOrg();

  const [teamId, setTeamId] = useState<string | null>(null);
  const [teamName, setTeamName] = useState("");
  const [personId, setPersonId] = useState("");
  const [addId, setAddId] = useState("");

  const teams = useQuery({
    queryKey: ["settings", "teams", orgId],
    queryFn: () => api.get<Team[]>(`/api/teams?org=${encodeURIComponent(orgId)}`).then((r) => r.data),
    enabled: !!orgId,
  });

  const members = useQuery({
    queryKey: ["settings", "members", orgId],
    queryFn: () => api.get<Member[]>(`/api/orgs/${orgId}/members`).then((r) => r.data),
    enabled: !!orgId,
  });

  const chosenTeam = teams.data?.find((t) => t.id === teamId) ?? teams.data?.[0];

  const teamMembers = useQuery({
    queryKey: ["settings", "team-members", chosenTeam?.id],
    queryFn: () => api.get<TeamMember[]>(`/api/teams/${chosenTeam?.id}/members`).then((r) => r.data),
    enabled: !!chosenTeam?.id,
  });

  const sites = useQuery({
    queryKey: ["settings", "org-sites", orgId],
    queryFn: () => api.get<SiteSummary[]>(`/api/sites?org=${encodeURIComponent(orgId)}`).then((r) => r.data),
    enabled: !!orgId,
  });

  const access = useQuery({
    queryKey: ["settings", "access", orgId, personId],
    queryFn: () =>
      api.get<AccessRow[]>(`/api/orgs/${orgId}/members/${personId}/access`).then((r) => r.data),
    enabled: !!orgId && !!personId,
  });

  const createTeam = useMutation({
    mutationFn: () => api.post<Team>("/api/teams", { org_id: orgId, name: teamName }),
    onSuccess: () => {
      setTeamName("");
      void qc.invalidateQueries({ queryKey: ["settings", "teams", orgId] });
    },
  });

  const deleteTeam = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/teams/${id}`),
    onSuccess: () => {
      setTeamId(null);
      void qc.invalidateQueries({ queryKey: ["settings", "teams", orgId] });
    },
  });

  const addToTeam = useMutation({
    mutationFn: () => api.post<null>(`/api/teams/${chosenTeam?.id}/members`, { user_id: addId }),
    onSuccess: () => {
      setAddId("");
      void qc.invalidateQueries({ queryKey: ["settings", "team-members", chosenTeam?.id] });
    },
  });

  const removeFromTeam = useMutation({
    mutationFn: (userId: string) => api.del<null>(`/api/teams/${chosenTeam?.id}/members/${userId}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "team-members", chosenTeam?.id] }),
  });

  const setAccess = useMutation({
    mutationFn: (v: { siteId: string; role: Role | "" }) =>
      v.role === ""
        ? api.del<null>(`/api/sites/${v.siteId}/access/${personId}`)
        : api.patch<null>(`/api/sites/${v.siteId}/access/${personId}`, { role: v.role }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "access", orgId, personId] }),
  });

  const orgRole = members.data?.find((m) => m.user_id === personId)?.role;

  const teamColumns: Column<Team>[] = [
    { key: "name", head: "Team", render: (t) => <span>{t.name}</span> },
    {
      key: "n",
      head: "People",
      numeric: true,
      width: "14%",
      render: (t) => <span className="mf-data">{t.member_count ?? "—"}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (t) => (
        <Button
          size="sm"
          tone="quiet"
          icon={<IconClose size={12} />}
          disabled={deleteTeam.isPending}
          onClick={() => deleteTeam.mutate(t.id)}
        >
          Delete
        </Button>
      ),
    },
  ];

  const siteColumns: Column<SiteSummary>[] = [
    {
      key: "site",
      head: "Site",
      render: (s) => (
        <span>
          {s.name || s.domain} <span className="mf-data">{s.domain}</span>
        </span>
      ),
    },
    {
      key: "role",
      head: "Access",
      width: "34%",
      render: (s) => {
        const override = access.data?.find((a) => a.site_id === s.id)?.role ?? "";
        return (
          <Select
            auto
            aria-label={`Access to ${s.domain}`}
            value={override}
            disabled={!personId || setAccess.isPending}
            onChange={(e) => setAccess.mutate({ siteId: s.id, role: e.target.value as Role | "" })}
          >
            <option value="">
              Inherit{orgRole ? ` (${ROLE_NAME[orgRole]})` : ""}
            </option>
            <option value="admin">Admin on this site</option>
            <option value="member">Member on this site</option>
            <option value="viewer">Viewer on this site</option>
          </Select>
        );
      },
    },
  ];

  return (
    <>
      <Panel>
        <PanelHead
          title="Teams"
          note="A team is a name for a group of people, useful for addressing a report or an alert at everyone in it at once."
        />
        <QueryState
          query={teams}
          emptyWhen={(t) => !t.length && !orgs.isPending}
          empty={
            <Empty
              title="No team yet"
              body="Create one below. A team does not grant access on its own; access is set per person, per site, further down this screen."
            />
          }
        >
          {(rows) => (
            <Sheet
              rows={rows}
              columns={teamColumns}
              getKey={(t) => t.id}
              onRowClick={(t) => setTeamId(t.id)}
            />
          )}
        </QueryState>

        <Spec>
          <SpecRow name="New team" htmlFor="team-name">
            <div className="mf-set-row">
              <TextInput
                id="team-name"
                placeholder="Docs crew"
                value={teamName}
                onChange={(e) => setTeamName(e.target.value)}
              />
              <Button
                icon={<IconPlus size={13} />}
                disabled={!teamName.trim() || createTeam.isPending}
                onClick={() => createTeam.mutate()}
              >
                Create
              </Button>
            </div>
          </SpecRow>
        </Spec>
        <ResultFoot action={createTeam} done="Team created." working="Creating…">
          {deleteTeam.isError ? (
            <span className="mf-set-foot__say" data-tone="warn" role="alert">
              {errText(deleteTeam.error)}
            </span>
          ) : null}
        </ResultFoot>
      </Panel>

      {chosenTeam ? (
        <Panel>
          <PanelHead
            title={`Who is in ${chosenTeam.name}`}
            note="Only people already in the organisation can be added to a team."
            actions={
              (teams.data?.length ?? 0) > 1 ? (
                <Select
                  auto
                  aria-label="Team"
                  value={chosenTeam.id}
                  onChange={(e) => setTeamId(e.target.value)}
                >
                  {teams.data?.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name}
                    </option>
                  ))}
                </Select>
              ) : undefined
            }
          />
          <QueryState query={teamMembers}>
            {(rows) => (
              <div className="mf-set-block">
                {rows.length ? (
                  <div className="mf-set-people">
                    {rows.map((m) => (
                      <div className="mf-set-people__row" key={m.user_id}>
                        <span className="mf-set-people__who">
                          {m.name ? `${m.name} ` : null}
                          <span className="mf-data">{m.email}</span>
                        </span>
                        <Button
                          size="sm"
                          tone="quiet"
                          icon={<IconClose size={12} />}
                          disabled={removeFromTeam.isPending}
                          onClick={() => removeFromTeam.mutate(m.user_id)}
                        >
                          Remove
                        </Button>
                      </div>
                    ))}
                  </div>
                ) : (
                  <Note>Nobody is in this team yet.</Note>
                )}
              </div>
            )}
          </QueryState>
          <Spec>
            <SpecRow name="Add someone" htmlFor="team-add">
              <div className="mf-set-row">
                <Select id="team-add" value={addId} onChange={(e) => setAddId(e.target.value)}>
                  <option value="">Choose a member</option>
                  {(members.data ?? [])
                    .filter((m) => !teamMembers.data?.some((t) => t.user_id === m.user_id))
                    .map((m) => (
                      <option key={m.user_id} value={m.user_id}>
                        {m.name || m.email}
                      </option>
                    ))}
                </Select>
                <Button
                  icon={<IconPlus size={13} />}
                  disabled={!addId || addToTeam.isPending}
                  onClick={() => addToTeam.mutate()}
                >
                  Add
                </Button>
              </div>
            </SpecRow>
          </Spec>
          <ResultFoot action={addToTeam} done="Added." working="Adding…" />
        </Panel>
      ) : null}

      <Panel>
        <PanelHead
          title="Per-site access"
          note="An override for one person on one site. Inherit means the organisation role applies, which is the right answer almost every time."
        />
        <Spec>
          <SpecRow name="Person" htmlFor="acc-person">
            <Select
              id="acc-person"
              value={personId}
              onChange={(e) => setPersonId(e.target.value)}
              disabled={!members.data?.length}
            >
              <option value="">Choose a member</option>
              {(members.data ?? []).map((m) => (
                <option key={m.user_id} value={m.user_id}>
                  {m.name || m.email} ({ROLE_NAME[m.role]} in the organisation)
                </option>
              ))}
            </Select>
          </SpecRow>
        </Spec>

        {!personId ? (
          <div className="mf-set-block">
            <Note>Choose a member and every site in the organisation is listed with the access they have.</Note>
          </div>
        ) : orgRole === "owner" ? (
          <div className="mf-set-block">
            <Note>
              The owner can read and change every site in the organisation, and an override cannot take that
              away. Transfer ownership first if that is what you meant to do.
            </Note>
          </div>
        ) : (
          <QueryState query={sites} emptyWhen={(s) => !s.length} empty={<Empty title="No sites yet" body="Sites created in this organisation appear here." />}>
            {(rows) => (
              <>
                <Sheet rows={rows} columns={siteColumns} getKey={(s) => s.id} />
                <ResultFoot action={setAccess} done="Access updated." working="Saving…">
                  {access.isError ? (
                    <span className="mf-set-foot__say" data-tone="warn" role="alert">
                      {errText(access.error)}
                    </span>
                  ) : null}
                </ResultFoot>
              </>
            )}
          </QueryState>
        )}
      </Panel>
    </>
  );
}

export default TeamsSettings;
