/**
 * Sign in, or create an account, with a code sent to an address.
 *
 * One screen does both, because the server does: `POST /api/auth/code` sends a
 * code to an address that may sign in here, and the code signs it in, creating
 * the account first when there is none and the install allows one. The screen
 * never knows which case it is in until the code is accepted, and it never
 * says, so it cannot be used to find out which addresses have accounts.
 *
 * The wording follows what the install allows, read from `/api/config`: the
 * first account on a fresh install is told it will own the install and may name
 * itself and its workspace; an open install says "sign in or create an
 * account"; an invite-only one says so before anybody waits for a code that
 * will never come.
 *
 * When the install has a Google or GitHub client, their buttons sit under the
 * code form (OAuthSignIn.tsx). A provider sign-in comes back here only to ask
 * for an authenticator code (`?oauth=totp`) or to say why it was refused
 * (`?oauth_error=`).
 */
import { useState } from "react";
import { useLocation, useNavigate, useSearchParams } from "react-router";
import { useQueryClient } from "@tanstack/react-query";
import { AuthFrame, ENTRY_PATHS, FormError, useInstanceConfig } from "./AuthFrame";
import { CodeSignIn, type CodeStep } from "./CodeSignIn";
import { OAuthButtons, OAuthTotp, oauthErrorText } from "./OAuthSignIn";

/** Only same-origin app paths are followed after sign-in. */
function safeNext(raw: string | null | undefined): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//")) return ENTRY_PATHS.chooser;
  return raw;
}

export function LoginScreen() {
  const navigate = useNavigate();
  const location = useLocation();
  const queryClient = useQueryClient();
  const [params] = useSearchParams();
  const config = useInstanceConfig();
  const [step, setStep] = useState<CodeStep>("email");

  // Where the reader was going: `?next=` from a link, or the address the
  // account gate bounced them from.
  const from = (location.state as { from?: string } | null)?.from;
  const next = safeNext(params.get("next") ?? from);

  const setup = config.data?.setup_required === true;
  const open = config.data?.registration_open === true;
  const providers = config.data?.oauth_providers ?? [];

  // Back from a provider: either the authenticator step, or a refusal.
  const oauthTotp = params.get("oauth") === "totp";
  const oauthError = oauthErrorText(params.get("oauth_error"));

  const signedIn = async (to: string) => {
    await queryClient.invalidateQueries();
    navigate(to, { replace: true });
  };

  if (oauthTotp) {
    return (
      <AuthFrame title="One more code">
        <OAuthTotp
          onSignedIn={(answer) => signedIn(safeNext(answer.next ?? next))}
          onRestart={() => navigate(ENTRY_PATHS.login, { replace: true })}
        />
      </AuthFrame>
    );
  }

  const title =
    step === "code"
      ? "Enter your code"
      : step === "totp"
        ? "One more code"
        : setup
          ? "Create the owner account"
          : open
            ? "Sign in or create an account"
            : "Sign in";

  const note =
    step !== "email"
      ? undefined
      : setup
        ? "This install has no accounts yet. The first one owns it and can see every site and every setting. Enter your email and Micaforge sends you a six-digit code. There is no password."
        : open
          ? "Enter your email and Micaforge sends you a six-digit code. It signs you in, and creates your account if you do not have one yet. There is no password."
          : "Enter the email of your account and Micaforge sends you a six-digit code. There is no password.";

  return (
    <AuthFrame title={title} note={note}>
      {step === "email" && oauthError ? <FormError>{oauthError}</FormError> : null}
      <CodeSignIn
        sendLabel="Send a code"
        askForProfile={setup}
        openToAll={setup || open}
        onStepChange={setStep}
        onSignedIn={() => signedIn(next)}
      />

      {step === "email" ? <OAuthButtons providers={providers} next={next} /> : null}

      {/* An invite-only instance says so on the first step, rather than letting
          a stranger wait for a code. Until the instance has answered, the
          screen claims nothing. */}
      {step === "email" && !config.isPending && !setup && !open ? (
        <p className="mf-field__hint">
          This instance is invite-only. If you do not have an account, an owner has to invite you
          first; the invitation email has a link that opens straight onto this sign-in.
        </p>
      ) : null}
    </AuthFrame>
  );
}

export default LoginScreen;
