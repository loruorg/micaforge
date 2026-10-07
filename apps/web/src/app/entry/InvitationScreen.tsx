/**
 * Accept an invitation.
 *
 * The invitation is read before anything is asked of the reader, so the screen
 * can name the organisation that invited them and the address it was sent to.
 * An invitation that has expired, been withdrawn or already been accepted is a
 * normal outcome and is answered plainly.
 */
import type { FormEvent } from "react";
import { useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { ApiError, api } from "../../lib/api";
import {
  AuthFrame,
  ENTRY_PATHS,
  Field,
  FormError,
  PasswordField,
  Waiting,
  describedBy,
  fieldError,
  formError,
} from "./AuthFrame";

interface Invitation {
  email: string;
  org_name: string;
  role: string;
  invited_by: string | null;
  expires_at: string | null;
  /** True when the invited address already signs in to this instance. */
  has_account: boolean;
}

const MIN_PASSWORD = 12;

const ROLE_WORDS: Record<string, string> = {
  owner: "an owner",
  admin: "an admin",
  member: "a member",
  viewer: "a viewer",
};

function whenExpires(iso: string | null): string | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return null;
  return new Date(t).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
}

export function InvitationScreen() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const routeParams = useParams();
  const [query] = useSearchParams();
  const token = routeParams.token ?? query.get("token") ?? "";

  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [localError, setLocalError] = useState<string | undefined>();

  const invitation = useQuery({
    queryKey: ["invitation", token],
    queryFn: () => api.get<Invitation>(`/api/auth/invitation/${encodeURIComponent(token)}`).then((r) => r.data),
    enabled: token.length > 0,
    retry: false,
  });

  const accept = useMutation({
    mutationFn: (body: { name: string; password: string }) =>
      api.post<{ id: string }>(`/api/auth/invitation/${encodeURIComponent(token)}/accept`, body),
    onSuccess: async () => {
      await queryClient.invalidateQueries();
      navigate(ENTRY_PATHS.chooser, { replace: true });
    },
  });

  const busy = accept.isPending;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (password.length < MIN_PASSWORD) {
      setLocalError(`Use at least ${MIN_PASSWORD} characters.`);
      return;
    }
    setLocalError(undefined);
    accept.mutate({ name: name.trim(), password });
  };

  if (!token) {
    return (
      <AuthFrame
        title="That invitation link is incomplete"
        note="This page needs the token from the invitation email, and the address you opened does not carry one."
      >
        <p className="mf-form__body">
          Open the link straight from the email rather than retyping it. If it still does not work,
          ask whoever invited you to send another.
        </p>
        <div className="mf-form__foot">
          <Link className="mf-btn mf-btn--key mf-btn--md" to={ENTRY_PATHS.login}>
            <span className="mf-btn__label">Sign in</span>
          </Link>
        </div>
      </AuthFrame>
    );
  }

  if (invitation.isPending) {
    return (
      <AuthFrame title="Reading the invitation" note="Checking who sent it and whether it is still open.">
        <Waiting label="Reading the invitation" lines={3} />
      </AuthFrame>
    );
  }

  if (invitation.isError) {
    const e = invitation.error;
    const gone = e instanceof ApiError && (e.status === 404 || e.status === 410 || e.status === 409);
    return (
      <AuthFrame
        title={gone ? "This invitation is no longer open" : "The invitation could not be read"}
        note={
          gone
            ? "It has expired, been withdrawn, or already been accepted."
            : formError(e) ?? "The server did not answer for this invitation."
        }
      >
        <p className="mf-form__body">
          {gone
            ? "Ask whoever invited you to send a new one. If you have already accepted it, sign in with the password you set."
            : "Try again in a moment. If it keeps failing, whoever runs this instance can check the server log."}
        </p>
        <div className="mf-form__foot">
          {gone ? null : <Button onClick={() => invitation.refetch()}>Read it again</Button>}
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            Go to sign in
          </Link>
        </div>
      </AuthFrame>
    );
  }

  const inv = invitation.data;
  const role = ROLE_WORDS[inv.role] ?? `a ${inv.role}`;
  const expires = whenExpires(inv.expires_at);

  const note = (
    <>
      {inv.invited_by ? `${inv.invited_by} invited ` : "You have been invited "}
      <strong>{inv.email}</strong> to join <strong>{inv.org_name}</strong> as {role}.
      {expires ? ` The invitation is open until ${expires}.` : ""}
    </>
  );

  // The address already signs in here, so there is no password to set: the
  // invitation attaches to the existing account the moment they sign in.
  if (inv.has_account) {
    const next = `${ENTRY_PATHS.login}?next=${encodeURIComponent(ENTRY_PATHS.chooser)}`;
    return (
      <AuthFrame title={`Join ${inv.org_name}`} note={note}>
        <p className="mf-form__body">
          That address already has a Micaforge account. Sign in with it and {inv.org_name} will be in
          your list of sites.
        </p>
        <div className="mf-form__foot">
          <Link className="mf-btn mf-btn--primary mf-btn--md" to={next}>
            <span className="mf-btn__label">Sign in to accept</span>
          </Link>
        </div>
      </AuthFrame>
    );
  }

  return (
    <AuthFrame title={`Join ${inv.org_name}`} note={note}>
      <form className="mf-form" onSubmit={submit} noValidate>
        {formError(accept.error) ? <FormError>{formError(accept.error)}</FormError> : null}

        <div className="mf-form__fields">
          <Field label="Email" htmlFor="mf-inv-email" hint="Set by the invitation and cannot be changed here.">
            <input
              id="mf-inv-email"
              className="mf-input mf-input--data"
              type="email"
              value={inv.email}
              readOnly
              disabled
              aria-describedby="mf-inv-email-hint"
            />
          </Field>

          <Field label="Name" htmlFor="mf-inv-name" error={fieldError(accept.error, "name")}>
            <input
              id="mf-inv-name"
              className="mf-input"
              type="text"
              autoComplete="name"
              value={name}
              disabled={busy}
              required
              aria-invalid={fieldError(accept.error, "name") ? true : undefined}
              aria-describedby={describedBy("mf-inv-name", { error: fieldError(accept.error, "name") })}
              onChange={(e) => setName(e.target.value)}
            />
          </Field>

          <PasswordField
            id="mf-inv-password"
            label="Password"
            value={password}
            onChange={(v) => {
              setPassword(v);
              if (localError) setLocalError(undefined);
            }}
            autoComplete="new-password"
            disabled={busy}
            hint={`At least ${MIN_PASSWORD} characters.`}
            error={localError ?? fieldError(accept.error, "password")}
          />
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy}>
            {busy ? "Joining…" : `Join ${inv.org_name}`}
          </Button>
        </div>
      </form>
    </AuthFrame>
  );
}

export default InvitationScreen;
