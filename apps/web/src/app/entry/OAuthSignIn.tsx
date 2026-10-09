/**
 * "Continue with Google" and "Continue with GitHub": the second door into an
 * account, beside the emailed code.
 *
 * The buttons are plain links to `/api/auth/oauth/{provider}/start`. The
 * browser goes to the provider and back to the server, and the server sends
 * it on to the dashboard: to where it was going, to `?oauth=totp` when the
 * account has two-factor on, or to `?oauth_error=<code>` when it refused. No
 * token from the provider ever reaches this page.
 *
 * Only the providers the install has a client for are shown, read from
 * `/api/config`, so a button is never offered that cannot work.
 */
import { useEffect, useRef, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Button } from "../../components/react/ui";
import { API_BASE, ApiError, api } from "../../lib/api";
import { Field, FormError, formError, type OAuthProvider } from "./AuthFrame";
import { CODE_LENGTH, codeDigits, type SignedIn } from "./CodeSignIn";

export const PROVIDER_NAME: Record<OAuthProvider, string> = {
  google: "Google",
  github: "GitHub",
};

/** What each refusal code means to the person who was sent back with it. */
export const OAUTH_ERRORS: Record<string, string> = {
  state: "The sign-in took too long or was started in another browser. Try again.",
  denied: "Sign-in was cancelled at the provider.",
  unverified_email: "That account has no verified email address, so Micaforge cannot sign you in with it.",
  closed: "This install is invite-only and that address has no account or invitation.",
  linked:
    "Your Micaforge account is already linked to a different account at that provider. Use that one, or an emailed code.",
  provider: "The provider did not answer as expected. Try again, or use an emailed code.",
  disabled: "That sign-in method is not set up on this install.",
};

export const oauthErrorText = (code: string | null): string | undefined =>
  code ? (OAUTH_ERRORS[code] ?? OAUTH_ERRORS.provider) : undefined;

/** The address that starts a provider sign-in and comes back to `next`. */
export const oauthStartUrl = (provider: OAuthProvider, next: string): string =>
  `${API_BASE}/api/auth/oauth/${provider}/start?next=${encodeURIComponent(next)}`;

export function OAuthButtons({ providers, next }: { providers: OAuthProvider[]; next: string }) {
  if (!providers.length) return null;
  return (
    <div className="mf-oauth">
      <p className="mf-oauth__or">or</p>
      {providers.map((p) => (
        <a key={p} className="mf-btn mf-btn--key mf-btn--md" href={oauthStartUrl(p, next)}>
          <span className="mf-btn__label">Continue with {PROVIDER_NAME[p] ?? p}</span>
        </a>
      ))}
    </div>
  );
}

type OAuthSignedIn = SignedIn & { next?: string };

/**
 * The authenticator step after a provider sign-in. The server holds the
 * half-finished sign-in in an `HttpOnly` cookie for ten minutes; this form only
 * adds the six digits.
 */
export function OAuthTotp({
  onSignedIn,
  onRestart,
}: {
  onSignedIn: (answer: OAuthSignedIn) => void | Promise<void>;
  onRestart: () => void;
}) {
  const [totp, setTotp] = useState("");
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => input.current?.focus(), []);

  const verify = useMutation({
    mutationFn: (value: string) =>
      api.post<OAuthSignedIn>("/api/auth/oauth/totp", { totp: value }).then((r) => r.data),
    onSuccess: (data) => onSignedIn(data),
    onError: () => {
      setTotp("");
      input.current?.focus();
    },
  });

  const submit = (value: string) => {
    if (value.length === CODE_LENGTH && !verify.isPending) verify.mutate(value);
  };

  const err = verify.error;
  const expired = err instanceof ApiError && err.status === 401 && !/authenticator/i.test(err.message);
  const banner = !err
    ? undefined
    : err instanceof ApiError && err.status === 401
      ? expired
        ? "This sign-in has expired, or too many wrong codes were tried. Start it again."
        : "That authenticator code is not right. The code changes every thirty seconds: enter the one your app shows now."
      : err instanceof ApiError && err.status === 429
        ? "Too many attempts. Wait a few minutes, then start the sign-in again."
        : formError(err);

  return (
    <form
      className="mf-form"
      onSubmit={(e) => {
        e.preventDefault();
        submit(totp);
      }}
      noValidate
    >
      <p className="mf-form__body">
        This account has two-factor turned on. Enter the six-digit code your authenticator app shows for
        Micaforge.
      </p>
      {banner ? <FormError>{banner}</FormError> : null}

      <div className="mf-form__fields">
        <Field label="Authenticator code" htmlFor="mf-oauth-totp">
          <input
            id="mf-oauth-totp"
            ref={input}
            className="mf-input mf-input--data mf-input--code"
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9]*"
            maxLength={CODE_LENGTH}
            value={totp}
            readOnly={verify.isPending || expired}
            aria-invalid={banner ? true : undefined}
            onChange={(e) => {
              const digits = codeDigits(e.target.value);
              setTotp(digits);
              if (digits.length === CODE_LENGTH) submit(digits);
            }}
          />
        </Field>
      </div>

      <div className="mf-form__foot">
        <Button
          tone="primary"
          type="submit"
          disabled={verify.isPending || expired || totp.length !== CODE_LENGTH}
        >
          {verify.isPending ? "Checking…" : "Sign in"}
        </Button>
        <Button tone="quiet" onClick={onRestart}>
          Start again
        </Button>
      </div>
    </form>
  );
}
