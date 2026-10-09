/**
 * CONNECTED APPS: the MCP clients this person has let in with OAuth.
 *
 * An OAuth grant is access a person handed to a program on their own behalf,
 * from the consent screen, so it belongs to the person rather than to an
 * organisation's key list. Each row says which program, for which
 * organisation, with what it may do and when it last used that, because "is
 * this still in use?" is the one question that decides whether to revoke it.
 *
 * Revoking ends the access token and the refresh token at once: the program's
 * next request fails, and it has to send the person through consent again.
 * API keys are a different credential with their own screen, and the empty
 * state says so, since "I connected it with a key" is the usual reason a
 * program is missing from this list.
 */
import { Link } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { IconClose } from "../../components/react/icons";
import { api } from "../../lib/api";
import type { OAuthGrant } from "../../lib/api";
import { ago } from "../../lib/format";
import { Foot, Note, QueryState, errText } from "./SettingsLayout";
import "../../styles/app/connect.css";

/** The OAuth scopes, in the words the consent screen used. */
const SCOPE_WORDS: Record<string, string> = {
  "stats:read": "Read reports, ask the AI analyst, send feedback",
  "manage:write": "Create goals and annotations",
};

const GRANTS_KEY = ["settings", "oauth-grants"] as const;

export function ConnectedAppsSettings() {
  const qc = useQueryClient();

  const grants = useQuery({
    queryKey: GRANTS_KEY,
    queryFn: () => api.get<OAuthGrant[]>("/api/oauth/grants").then((r) => r.data),
  });

  const revoke = useMutation({
    mutationFn: (id: string) => api.del<{ deleted: string }>(`/api/oauth/grants/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: GRANTS_KEY }),
  });

  const columns: Column<OAuthGrant>[] = [
    {
      key: "app",
      head: "App",
      render: (g) => (
        <span>
          {g.client_name || "unnamed app"} <span className="mf-data">{g.client_id}</span>
        </span>
      ),
    },
    { key: "org", head: "Organisation", width: "16%", render: (g) => <span>{g.org?.name ?? "—"}</span> },
    {
      key: "scopes",
      head: "Can do",
      width: "26%",
      render: (g) => (
        <span>{g.scopes?.length ? g.scopes.map((s) => SCOPE_WORDS[s] ?? s).join("; ") : "nothing"}</span>
      ),
    },
    {
      key: "created",
      head: "Connected",
      width: "12%",
      render: (g) => (
        <span className="mf-data" title={new Date(g.created_at).toLocaleString()}>
          {ago(g.created_at)}
        </span>
      ),
    },
    {
      key: "used",
      head: "Last used",
      width: "12%",
      render: (g) => <span className="mf-data">{g.last_used_at ? ago(g.last_used_at) : "never"}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (g) => (
        <Button
          size="sm"
          tone="quiet"
          icon={<IconClose size={12} />}
          aria-label={`Revoke ${g.client_name || "this app"}`}
          disabled={revoke.isPending}
          onClick={() => revoke.mutate(g.id)}
        >
          Revoke
        </Button>
      ),
    },
  ];

  return (
    <Panel>
      <PanelHead
        title="Connected apps"
        note="Programs you allowed to use Micaforge for you with OAuth. A connected app can never act above the member role, whatever your own role is. Revoking takes effect on its next request."
      />
      <QueryState query={grants}>
        {(rows) => (
          <>
            <Sheet
              rows={rows}
              columns={columns}
              getKey={(g) => g.id}
              empty={
                <Empty
                  title="No app is connected"
                  body={
                    <>
                      MCP clients that you connect with OAuth appear here, with what they may do and when they last
                      did it. A program that uses an API key instead is listed under{" "}
                      <Link className="mf-cx-link" to="/app/settings/keys">
                        API keys
                      </Link>
                      .
                    </>
                  }
                />
              }
            />
            {revoke.isError ? (
              <Foot tight>
                <span className="mf-set-foot__say" data-tone="warn" role="alert">
                  {errText(revoke.error)}
                </span>
              </Foot>
            ) : rows.length ? (
              <Foot tight>
                <Note>
                  Programs that use an API key are listed under{" "}
                  <Link className="mf-cx-link" to="/app/settings/keys">
                    API keys
                  </Link>
                  .
                </Note>
              </Foot>
            ) : null}
          </>
        )}
      </QueryState>
    </Panel>
  );
}

export default ConnectedAppsSettings;
