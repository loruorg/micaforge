/**
 * Accept an invitation.
 *
 * The invitation is read before anything is asked of the reader
 * (`POST /api/invitations/preview`), so the screen can name the organisation
 * that invited them and the address it was sent to. That address is where the
 * sign-in code goes: there is no password to set, and the same code signs in an
 * existing account or creates the new one, even on an install whose
 * registration is closed. An invitation that has expired, been withdrawn or
 * already been accepted is a normal outcome and is answered plainly.
 *
 * A brand-new account joins the inviting organisation as it is created (the
 * sign-in answer lists it in `joined`). An existing account joins with
 * `POST /api/invitations/accept` once it is signed in.
 */
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { ApiError, api } from "../../lib/api";
import { useAuth } from "../providers";
import { AuthFrame, ENTRY_PATHS, FormError, Waiting, formError } from "./AuthFrame";
import { CodeSignIn, type SignedIn } from "./CodeSignIn";

interface Invitation {
  email: string;
  org_id: string;
  org_name: string;
  role: string;
  /** The inviter's name, when they still have an account here. */
  invited_by: string | null;
  expires_at: string | null;
}

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
  const auth = useAuth();
  const routeParams = useParams();
  const [query] = useSearchParams();
  const token = routeParams.token ?? query.get("token") ?? "";

  // The token travels in a body, never a path, because paths are logged.
  const invitation = useQuery({
    queryKey: ["invitation", token],
    queryFn: () => api.post<Invitation>("/api/invitations/preview", { token }).then((r) => r.data),
    enabled: token.length > 0,
    retry: false,
  });

  const accept = useMutation({
    mutationFn: () => api.post<unknown>("/api/invitations/accept", { token }),
    onSuccess: async () => {
      await queryClient.invalidateQueries();
      navigate(ENTRY_PATHS.chooser, { replace: true });
    },
  });

  /** After a code sign-in: a new account has joined already; an existing one accepts now. */
  const signedIn = async (answer: SignedIn) => {
    if (invitation.data && answer.joined?.includes(invitation.data.org_id)) {
      await queryClient.invalidateQueries();
      navigate(ENTRY_PATHS.chooser, { replace: true });
      return;
    }
    accept.mutate();
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

  if (invitation.isPending || auth.isLoading) {
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
            ? "Ask whoever invited you to send a new one. If you have already accepted it, sign in with your email."
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

  const acceptError = accept.error;
  const wrongAddress = acceptError instanceof ApiError && acceptError.status === 403;
  const acceptBanner = wrongAddress
    ? `You are signed in with another address. Sign out, then open the invitation link again and sign in as ${inv.email}.`
    : acceptError instanceof ApiError && acceptError.status === 404
      ? "This invitation is no longer open. It has expired, been withdrawn, or already been accepted."
      : formError(acceptError);

  // Already signed in: one button, and the server checks the address.
  if (!auth.isSignedOut) {
    return (
      <AuthFrame title={`Join ${inv.org_name}`} note={note}>
        {acceptBanner ? <FormError>{acceptBanner}</FormError> : null}
        <div className="mf-form__foot">
          {wrongAddress ? (
            <Button tone="primary" onClick={() => void auth.signOut()}>
              Sign out
            </Button>
          ) : (
            <Button tone="primary" disabled={accept.isPending} onClick={() => accept.mutate()}>
              {accept.isPending ? "Joining…" : `Join ${inv.org_name}`}
            </Button>
          )}
        </div>
      </AuthFrame>
    );
  }

  return (
    <AuthFrame title={`Join ${inv.org_name}`} note={note}>
      {acceptBanner ? <FormError>{acceptBanner}</FormError> : null}
      <CodeSignIn email={inv.email} sendLabel="Send me a code" openToAll onSignedIn={signedIn} />
      <p className="mf-field__hint">
        There is no password to choose. The code signs you in, and creates your account if this is
        your first time here.
      </p>
    </AuthFrame>
  );
}

export default InvitationScreen;
