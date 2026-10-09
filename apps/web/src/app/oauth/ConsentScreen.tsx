/**
 * The OAuth consent screen, at /app/oauth/authorize.
 *
 * An MCP client (an AI assistant, an editor, a script) that wants to read this
 * person's analytics sends them here through the server's `/api/oauth/authorize`,
 * which checks the request and forwards its query string unchanged. This
 * screen is where the person decides, so it has to say plainly who is asking,
 * where the answer will be sent, and what the program will be able to do.
 *
 * Four decisions:
 *
 *   1. THE REQUEST IS THE SERVER'S, NOT THIS PAGE'S. The query string is handed
 *      back to the server untouched, both to read the request and to answer
 *      it. The page never parses or rewrites the client's parameters, so it
 *      cannot be made to approve something other than what the server checked.
 *   2. THE ADDRESS THE ANSWER GOES TO IS SHOWN IN FULL, in the data face. It is
 *      the one fact that tells a real client from an impostor using its name.
 *   3. THE PERSON CHOOSES THE ORGANISATION AND THE SCOPES. Reading reports is
 *      required for any client to be useful, so it is ticked and fixed; making
 *      changes (goals, annotations) is a separate choice. Whatever is chosen,
 *      the token can never act above the member role.
 *   4. BEHIND SIGN-IN, OUTSIDE THE DASHBOARD FRAME. The router wraps this screen
 *      in the account gate, which sends a signed-out reader to sign in and
 *      brings them back to this exact address, query string included. It draws
 *      its own sheet like the other entry screens, because a consent page with
 *      the dashboard's navigation around it invites a click away mid-decision.
 */
import { useEffect, useId, useMemo, useState } from "react";
import { useLocation } from "react-router";
import { useMutation, useQuery } from "@tanstack/react-query";
import * as RCheckbox from "@radix-ui/react-checkbox";

import { Button } from "../../components/react/ui";
import { IconCheck } from "../../components/react/icons";
import { ApiError, api } from "../../lib/api";
import type { OAuthConsent } from "../../lib/api";
import { useAuth } from "../providers";
import { AuthFrame, Field, FormError, Select, Waiting } from "../entry/AuthFrame";
import "../../styles/app/entry.css";
import "../../styles/app/connect.css";

const ROLE_WORDS: Record<string, string> = {
  owner: "owner",
  admin: "admin",
  member: "member",
  viewer: "viewer",
};

/** The failure, in words, with the one way out that works for it. */
function consentError(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network") return "Could not reach the Micaforge server. Check your connection, then try again.";
    if (e.status >= 400 && e.status < 500)
      return `${e.message} Go back to the app that sent you here and connect it again.`;
    return "The server failed on this request. Try again in a moment.";
  }
  return "Something went wrong on the way to the server. Try again.";
}

export function ConsentScreen() {
  const { search } = useLocation();
  const { user } = useAuth();
  const query = search.startsWith("?") ? search.slice(1) : search;
  const orgId = useId();

  const consent = useQuery({
    queryKey: ["oauth", "consent", query],
    queryFn: () => api.get<OAuthConsent>(`/api/oauth/consent?${query}`).then((r) => r.data),
    enabled: query.length > 0,
    retry: false,
    staleTime: Infinity,
  });

  const c = consent.data;
  const [org, setOrg] = useState("");
  const [scopes, setScopes] = useState<string[]>([]);

  // Once the request is read: the first organisation, and every scope the
  // client asked for plus every required one, ticked.
  useEffect(() => {
    if (!c) return;
    setOrg((o) => o || c.organisations[0]?.id || "");
    setScopes(
      c.scopes_available
        .filter((s) => s.required || c.scopes_requested.includes(s.scope))
        .map((s) => s.scope),
    );
  }, [c]);

  const required = useMemo(
    () => new Set((c?.scopes_available ?? []).filter((s) => s.required).map((s) => s.scope)),
    [c],
  );

  const answer = useMutation({
    mutationFn: (approve: boolean) =>
      api
        .post<{ redirect_to: string }>("/api/oauth/consent", approve
          ? { query, approve: true, org_id: org, scopes }
          : { query, approve: false })
        .then((r) => r.data),
    // http(s) and custom-scheme addresses alike: the browser hands a custom
    // scheme to the app that registered it.
    onSuccess: (r) => window.location.assign(r.redirect_to),
  });

  const sent = answer.isSuccess;
  const busy = answer.isPending || sent;

  if (!query) {
    return (
      <AuthFrame title="Nothing to approve">
        <FormError>
          This address carries no request. Go back to the app you were connecting and start again from there.
        </FormError>
      </AuthFrame>
    );
  }

  if (consent.isPending) {
    return (
      <AuthFrame title="Checking the request">
        <Waiting label="Reading the request from the app…" lines={4} />
      </AuthFrame>
    );
  }

  if (consent.isError || !c) {
    const network = consent.error instanceof ApiError && consent.error.code === "network";
    return (
      <AuthFrame title="This request cannot be approved">
        <FormError>{consentError(consent.error)}</FormError>
        {network ? (
          <div className="mf-form__foot">
            <Button onClick={() => void consent.refetch()}>Try again</Button>
          </div>
        ) : null}
      </AuthFrame>
    );
  }

  const noOrg = c.organisations.length === 0;

  return (
    <AuthFrame
      title={`Connect ${c.client.name || "this app"}?`}
      note="This app is asking to use Micaforge for you. Check where the answer goes and what it may do before you approve."
    >
      <dl className="mf-cx-consent__facts">
        <div>
          <dt>App</dt>
          <dd>{c.client.name || "unnamed app"}</dd>
        </div>
        <div>
          <dt>Sends the answer to</dt>
          <dd className="mf-cx-consent__uri mf-data">{c.client.redirect_uri}</dd>
        </div>
        {user ? (
          <div>
            <dt>Signed in as</dt>
            <dd className="mf-data">{user.email}</dd>
          </div>
        ) : null}
      </dl>

      {noOrg ? (
        <FormError>Your account belongs to no organisation, so there is nothing this app could read.</FormError>
      ) : c.organisations.length === 1 ? (
        <p className="mf-form__body">
          For <strong>{c.organisations[0].name}</strong>, where you are{" "}
          {ROLE_WORDS[c.organisations[0].role] ?? c.organisations[0].role}.
        </p>
      ) : (
        <Field label="Organisation" htmlFor={orgId} hint="The app reads only the sites of this organisation.">
          <Select id={orgId} value={org} onChange={setOrg} disabled={busy}>
            {c.organisations.map((o) => (
              <option key={o.id} value={o.id}>
                {o.name} ({ROLE_WORDS[o.role] ?? o.role})
              </option>
            ))}
          </Select>
        </Field>
      )}

      <fieldset className="mf-cx-consent__scopes">
        <legend className="mf-field__label">It may</legend>
        {c.scopes_available.map((s) => {
          const id = `scope-${s.scope.replace(/[^a-z0-9]/gi, "-")}`;
          const on = scopes.includes(s.scope);
          return (
            <div key={s.scope} className="mf-cx-consent__scope">
              <RCheckbox.Root
                id={id}
                className="mf-set-check"
                checked={on}
                disabled={s.required || busy}
                onCheckedChange={(v) =>
                  setScopes((cur) => (v === true ? [...cur, s.scope] : cur.filter((x) => x !== s.scope)))
                }
              >
                <RCheckbox.Indicator className="mf-set-check__mark">
                  <IconCheck size={11} />
                </RCheckbox.Indicator>
              </RCheckbox.Root>
              <label htmlFor={id} className="mf-cx-consent__scope-text">
                <span className="mf-cx-consent__scope-name">
                  {s.label}
                  {s.required ? <span className="mf-cx-consent__req"> (required)</span> : null}
                </span>
                <span className="mf-cx-consent__scope-desc">{s.description}</span>
                <code className="mf-data mf-cx-consent__scope-key">{s.scope}</code>
              </label>
            </div>
          );
        })}
      </fieldset>

      <p className="mf-field__hint">
        The app can never act above the member role, whatever your own role is. You can revoke it at any time
        under Account, Connected apps.
      </p>

      {answer.isError ? <FormError>{consentError(answer.error)}</FormError> : null}
      {sent ? (
        <p className="mf-form__body" role="status">
          Sending you back to {c.client.name || "the app"}…
        </p>
      ) : null}

      <div className="mf-form__foot">
        <Button
          tone="primary"
          disabled={busy || noOrg || !org || [...required].some((r) => !scopes.includes(r))}
          onClick={() => answer.mutate(true)}
        >
          {answer.isPending && answer.variables === true ? "Approving…" : "Approve"}
        </Button>
        <Button disabled={busy} onClick={() => answer.mutate(false)}>
          {answer.isPending && answer.variables === false ? "Denying…" : "Deny"}
        </Button>
      </div>
    </AuthFrame>
  );
}

export default ConsentScreen;
