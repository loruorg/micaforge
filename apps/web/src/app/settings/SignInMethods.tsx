/**
 * SIGN-IN METHODS: the providers an account is linked to.
 *
 * A provider is linked by signing in with it once, with an address that the
 * provider has verified and that is this account's address. There is no
 * "link" button here on purpose: a provider sign-in started from a signed-in
 * page would sign in as whichever account the provider's address belongs to,
 * which is not always this one. Unlinking only removes the shortcut; the
 * emailed code always works.
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Panel, PanelHead } from "../../components/react/ui";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import { useInstanceConfig, type OAuthProvider } from "../entry/AuthFrame";
import { PROVIDER_NAME } from "../entry/OAuthSignIn";
import { Block, QueryState, ResultFoot } from "./SettingsLayout";
import "../../styles/app/workspace.css";

interface Identity {
  provider: OAuthProvider;
  email: string;
  created_at: string;
  last_used_at: string | null;
}

const KEY = ["account", "identities"];

export function SignInMethods({ email }: { email?: string }) {
  const qc = useQueryClient();
  const config = useInstanceConfig();
  const providers = config.data?.oauth_providers ?? [];

  const identities = useQuery({
    queryKey: KEY,
    queryFn: () => api.get<Identity[]>("/api/auth/oauth/identities").then((r) => r.data ?? []),
  });

  const unlink = useMutation({
    mutationFn: (provider: OAuthProvider) => api.del(`/api/auth/oauth/identities/${provider}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: KEY }),
  });

  return (
    <Panel>
      <PanelHead
        title="Sign-in methods"
        note="An emailed code always signs you in. A linked provider is a second way in to the same account."
      />
      <QueryState query={identities}>
        {(rows) => {
          const linked = new Set(rows.map((r) => r.provider));
          const others = providers.filter((p) => !linked.has(p));
          return (
            <>
              <Block title="Linked providers">
                <ul className="mf-ws-methods">
                  <li className="mf-ws-method">
                    <span className="mf-ws-method__name">Emailed code</span>
                    <span className="mf-ws-method__about">
                      Sent to <span className="mf-data">{email ?? "your address"}</span>. Always on.
                    </span>
                  </li>
                  {rows.map((r) => (
                    <li className="mf-ws-method" key={r.provider}>
                      <span className="mf-ws-method__name">{PROVIDER_NAME[r.provider] ?? r.provider}</span>
                      <span className="mf-ws-method__about">
                        <span className="mf-data">{r.email}</span>, linked {ago(r.created_at)}
                        {r.last_used_at ? `, last used ${ago(r.last_used_at)}` : ""}.
                        {providers.includes(r.provider) ? "" : " This install no longer offers it."}
                      </span>
                      <Button
                        tone="quiet"
                        size="sm"
                        disabled={unlink.isPending}
                        onClick={() => unlink.mutate(r.provider)}
                      >
                        Unlink
                      </Button>
                    </li>
                  ))}
                  {others.map((p) => (
                    <li className="mf-ws-method" key={p}>
                      <span className="mf-ws-method__name">{PROVIDER_NAME[p] ?? p}</span>
                      <span className="mf-ws-method__about">
                        Not linked. Sign out, then use “Continue with {PROVIDER_NAME[p] ?? p}” with an account
                        whose verified address is {email ? <span className="mf-data">{email}</span> : "yours"}. It
                        is linked on that first sign-in.
                      </span>
                    </li>
                  ))}
                </ul>
              </Block>
              <ResultFoot action={unlink} done="Unlinked. The emailed code still signs you in." working="Unlinking…" />
            </>
          );
        }}
      </QueryState>
    </Panel>
  );
}

export default SignInMethods;
