/**
 * Ask for a reset link.
 *
 * The confirmation is identical whether or not the address belongs to an
 * account. A screen that says "no such user" is a free account-enumeration
 * tool, and this product's whole claim is that it does not leak what it knows.
 */
import type { FormEvent } from "react";
import { useState } from "react";
import { Link } from "react-router";
import { useMutation } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { ApiError, api } from "../../lib/api";
import { AuthFrame, ENTRY_PATHS, Field, FormError, describedBy, fieldError, formError } from "./AuthFrame";

export function ForgotPasswordScreen() {
  const [email, setEmail] = useState("");

  const ask = useMutation({
    mutationFn: (body: { email: string }) => api.post<null>("/api/auth/forgot", body),
  });

  const busy = ask.isPending;
  const err = ask.error;

  // A rate limit is the one refusal worth surfacing: it is about the request,
  // not about the account, so it reveals nothing and the reader needs it.
  const throttled = err instanceof ApiError && err.status === 429;
  const banner = throttled
    ? "Too many requests from here. Wait a minute, then ask again."
    : formError(err);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    ask.mutate({ email: email.trim() });
  };

  if (ask.isSuccess) {
    return (
      <AuthFrame
        title="Check your email"
        note="If that address belongs to a Micaforge account, a reset link is on its way to it."
      >
        <p className="mf-form__body">
          The link is good for one hour and can be used once. Nothing about the account has changed
          yet. The old password still works until you set a new one.
        </p>
        <div className="mf-form__foot">
          <Link className="mf-btn mf-btn--key mf-btn--md" to={ENTRY_PATHS.login}>
            <span className="mf-btn__label">Back to sign in</span>
          </Link>
          <button
            type="button"
            className="mf-entry__link mf-entry__link--sm"
            onClick={() => ask.reset()}
          >
            Send it to a different address
          </button>
        </div>
      </AuthFrame>
    );
  }

  return (
    <AuthFrame
      title="Reset your password"
      note="Give the address you sign in with and Micaforge will send a link that sets a new password."
    >
      <form className="mf-form" onSubmit={submit} noValidate>
        {banner ? <FormError>{banner}</FormError> : null}

        <div className="mf-form__fields">
          <Field label="Email" htmlFor="mf-forgot-email" error={fieldError(err, "email")}>
            <input
              id="mf-forgot-email"
              className="mf-input"
              type="email"
              inputMode="email"
              autoComplete="username"
              autoCapitalize="off"
              spellCheck={false}
              value={email}
              disabled={busy}
              required
              aria-invalid={fieldError(err, "email") ? true : undefined}
              aria-describedby={describedBy("mf-forgot-email", { error: fieldError(err, "email") })}
              onChange={(e) => setEmail(e.target.value)}
            />
          </Field>
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy}>
            {busy ? "Sending…" : "Send the reset link"}
          </Button>
          <Link className="mf-entry__link mf-entry__link--sm" to={ENTRY_PATHS.login}>
            Back to sign in
          </Link>
        </div>
      </form>
    </AuthFrame>
  );
}

export default ForgotPasswordScreen;
