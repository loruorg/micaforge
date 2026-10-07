/**
 * Set a new password from a reset link.
 *
 * The token arrives either as a route parameter or as `?token=`, because the
 * link in the email is written once and outlives any routing decision. A used,
 * expired or forged token is a normal outcome, not a crash, and it is answered
 * with the one action that recovers from it.
 */
import type { FormEvent } from "react";
import { useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import { useMutation } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { ApiError, api } from "../../lib/api";
import { AuthFrame, ENTRY_PATHS, FormError, PasswordField, fieldError, formError } from "./AuthFrame";

const MIN_PASSWORD = 12;

export function ResetPasswordScreen() {
  const navigate = useNavigate();
  const routeParams = useParams();
  const [query] = useSearchParams();
  const token = routeParams.token ?? query.get("token") ?? "";

  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [localError, setLocalError] = useState<{ field: "password" | "confirm"; message: string } | undefined>();

  const reset = useMutation({
    mutationFn: (body: { token: string; password: string }) =>
      api.post<null>("/api/auth/reset", body),
  });

  const busy = reset.isPending;
  const err = reset.error;
  const dead =
    err instanceof ApiError && (err.status === 404 || err.status === 410 || err.code === "not_found");

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (password.length < MIN_PASSWORD) {
      setLocalError({ field: "password", message: `Use at least ${MIN_PASSWORD} characters.` });
      return;
    }
    if (password !== confirm) {
      setLocalError({ field: "confirm", message: "The two passwords are not the same." });
      return;
    }
    setLocalError(undefined);
    reset.mutate({ token, password });
  };

  if (!token) {
    return (
      <AuthFrame
        title="That link is incomplete"
        note="This page needs the token from the reset email, and the address you opened does not carry one."
      >
        <p className="mf-form__body">
          Open the link straight from the email rather than retyping it: mail clients sometimes cut
          long links across two lines. If the link is genuinely broken, ask for a fresh one.
        </p>
        <div className="mf-form__foot">
          <Link className="mf-btn mf-btn--primary mf-btn--md" to={ENTRY_PATHS.forgot}>
            <span className="mf-btn__label">Send a new link</span>
          </Link>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            Back to sign in
          </Link>
        </div>
      </AuthFrame>
    );
  }

  if (reset.isSuccess) {
    return (
      <AuthFrame
        title="Password set"
        note="The new password is live. Every other session signed in with the old one has been ended."
      >
        <div className="mf-form__foot">
          <Button tone="primary" onClick={() => navigate(ENTRY_PATHS.login, { replace: true })}>
            Sign in
          </Button>
        </div>
      </AuthFrame>
    );
  }

  if (dead) {
    return (
      <AuthFrame
        title="That link has already been used"
        note="A reset link works once and expires an hour after it is sent."
      >
        <p className="mf-form__body">
          Nothing has changed on the account. Ask for a fresh link and use it straight away.
        </p>
        <div className="mf-form__foot">
          <Link className="mf-btn mf-btn--primary mf-btn--md" to={ENTRY_PATHS.forgot}>
            <span className="mf-btn__label">Send a new link</span>
          </Link>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            Back to sign in
          </Link>
        </div>
      </AuthFrame>
    );
  }

  return (
    <AuthFrame title="Set a new password" note="This link works once, and only for this account.">
      <form className="mf-form" onSubmit={submit} noValidate>
        {formError(err) ? <FormError>{formError(err)}</FormError> : null}

        <div className="mf-form__fields">
          <PasswordField
            id="mf-reset-password"
            label="New password"
            value={password}
            onChange={(v) => {
              setPassword(v);
              if (localError?.field === "password") setLocalError(undefined);
            }}
            autoComplete="new-password"
            disabled={busy}
            hint={`At least ${MIN_PASSWORD} characters.`}
            error={localError?.field === "password" ? localError.message : fieldError(err, "password")}
          />

          <PasswordField
            id="mf-reset-confirm"
            label="New password again"
            value={confirm}
            onChange={(v) => {
              setConfirm(v);
              if (localError?.field === "confirm") setLocalError(undefined);
            }}
            autoComplete="new-password"
            disabled={busy}
            error={localError?.field === "confirm" ? localError.message : undefined}
          />
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy}>
            {busy ? "Setting…" : "Set the password"}
          </Button>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            Back to sign in
          </Link>
        </div>
      </form>
    </AuthFrame>
  );
}

export default ResetPasswordScreen;
