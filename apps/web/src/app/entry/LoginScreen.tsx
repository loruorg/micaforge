/**
 * Sign in.
 *
 * Two rules shape this screen. First, a failed sign-in never reveals whether
 * the account exists: whatever the server says about a rejected credential, the
 * reader is told one sentence that fits both cases. Second, validation errors
 * the server pins to a field are shown against that field, so a typo does not
 * arrive as a vague banner.
 */
import type { FormEvent } from "react";
import { useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { ApiError, api } from "../../lib/api";
import { AuthFrame, ENTRY_PATHS, Field, FormError, PasswordField, describedBy, fieldError, formError, useInstanceConfig } from "./AuthFrame";

interface AuthedUser {
  id: string;
  email: string;
  name: string;
}

/** Only same-origin app paths are followed after sign-in. */
function safeNext(raw: string | null): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//")) return ENTRY_PATHS.chooser;
  return raw;
}

export function LoginScreen() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [params] = useSearchParams();
  const config = useInstanceConfig();

  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");

  const signIn = useMutation({
    mutationFn: (body: { email: string; password: string }) =>
      api.post<AuthedUser>("/api/auth/login", body).then((r) => r.data),
    onSuccess: async () => {
      await queryClient.invalidateQueries();
      navigate(safeNext(params.get("next")), { replace: true });
    },
  });

  const err = signIn.error;
  const rejected =
    err instanceof ApiError && (err.status === 401 || err.status === 403 || err.status === 404);

  // One sentence for every rejected credential, whether the address is unknown,
  // the password is wrong, or the account is disabled. The screen must not be
  // usable as a tool for finding out which accounts exist.
  const banner = rejected
    ? "Those details did not match an account. Check the address and the password, then try again."
    : formError(err);

  const busy = signIn.isPending;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    signIn.mutate({ email: email.trim(), password });
  };

  return (
    <AuthFrame title="Sign in" note="Read the record for the sites you have access to.">
      <form className="mf-form" onSubmit={submit} noValidate>
        {banner ? <FormError>{banner}</FormError> : null}

        <div className="mf-form__fields">
          <Field label="Email" htmlFor="mf-login-email" error={rejected ? undefined : fieldError(err, "email")}>
            <input
              id="mf-login-email"
              className="mf-input"
              type="email"
              inputMode="email"
              autoComplete="username"
              autoCapitalize="off"
              spellCheck={false}
              value={email}
              disabled={busy}
              required
              aria-invalid={!rejected && fieldError(err, "email") ? true : undefined}
              aria-describedby={describedBy("mf-login-email", { error: rejected ? undefined : fieldError(err, "email") })}
              onChange={(e) => setEmail(e.target.value)}
            />
          </Field>

          <PasswordField
            id="mf-login-password"
            label="Password"
            value={password}
            onChange={setPassword}
            autoComplete="current-password"
            disabled={busy}
            error={rejected ? undefined : fieldError(err, "password")}
          />
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy}>
            {busy ? "Signing in…" : "Sign in"}
          </Button>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.forgot}>
            Forgotten your password?
          </Link>
        </div>

        {/* An invite-only instance says so here rather than offering a link to
            a form that would only reject the reader. Until the instance has
            answered, the screen claims neither. */}
        {config.isPending ? null : (
        <p className="mf-field__hint">
          {config.data?.registration_open ? (
            <>
              No account yet?{" "}
              <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.signup}>
                Create one
              </Link>
            </>
          ) : (
            "This instance is invite-only. An owner has to send you an invitation before you can sign in."
          )}
        </p>
        )}
      </form>
    </AuthFrame>
  );
}

export default LoginScreen;
