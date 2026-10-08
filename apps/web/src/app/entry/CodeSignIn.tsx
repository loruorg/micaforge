/**
 * Sign in with a code sent to an address. There is no password anywhere.
 *
 * Three steps, each one a form on the same slip:
 *
 *   1. EMAIL. The address, and for the very first account on an install, an
 *      optional name for the person and for their workspace.
 *      `POST /api/auth/code` answers the same for every address, so this
 *      screen never learns, and never says, whether the address has an account.
 *   2. CODE. Six digits. One input, typed, pasted ("482 913" and "482-913"
 *      both work) or filled in by the phone from the message; it submits
 *      itself at the sixth digit. A new code can be asked for after thirty
 *      seconds, and only the newest one works.
 *   3. AUTHENTICATOR, only when the server answers `totp_required`. The
 *      emailed code is still good at that point; it is sent again with the
 *      authenticator code.
 *
 * When the server says it sends no mail (`mail_delivered: false`), step 2 says
 * so and shows the command the operator runs on the host to print a code,
 * because waiting for an email that will never come is the worst outcome this
 * screen could produce.
 */
import type { ClipboardEvent, SubmitEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Button, CopyField } from "../../components/react/ui";
import { ApiError, api } from "../../lib/api";
import { Field, FormError, FormOk, describedBy, fieldError, formError } from "./AuthFrame";

/** Digits in a sign-in code, and in an authenticator code. */
export const CODE_LENGTH = 6;

/** How long "Send a new code" waits after a code was sent, in seconds. */
const RESEND_COOLDOWN_S = 30;

/** The service the server runs as in the shipped `docker-compose.yml`. */
const COMPOSE_SERVICE = "micaforge";

/** What `POST /api/auth/code/verify` answers once a session is open. */
export interface SignedIn {
  status: "ok";
  user: { id: string; email: string; name: string };
  expires_at: string;
  /** True when this sign-in created the account. */
  created: boolean;
  /** Organisations a new account joined through invitations waiting for it. */
  joined?: string[];
}

type VerifyAnswer = SignedIn | { status: "totp_required" };

interface CodeSent {
  status: "ok";
  /** Whether this install sends mail at all. A fact about the install, not the address. */
  mail_delivered: boolean;
}

export type CodeStep = "email" | "code" | "totp";

/** The digits of a typed or pasted code, at most six. */
export function codeDigits(raw: string): string {
  return raw.replace(/\D/g, "").slice(0, CODE_LENGTH);
}

export interface CodeSignInProps {
  /** A fixed address, from an invitation. The address field is shown read-only. */
  email?: string;
  /** Ask for a name and a workspace name. Only for an account that is certainly new. */
  askForProfile?: boolean;
  /**
   * True when any address may be sent a code here (registration is open, or the
   * install has no account yet), so the screen can say a code was sent rather
   * than that one was sent if the address can sign in.
   */
  openToAll?: boolean;
  /** The label of the first step's button. */
  sendLabel: string;
  /** Told whenever the step changes, so the frame can retitle the slip. */
  onStepChange?: (step: CodeStep) => void;
  /** Called once the server has opened a session. */
  onSignedIn: (answer: SignedIn) => void | Promise<void>;
}

/** A 401 here carries one of two fixed sentences; this one is the second factor's. */
function isAuthenticatorRefusal(err: unknown): boolean {
  return err instanceof ApiError && err.status === 401 && /authenticator/i.test(err.message);
}

export function CodeSignIn({
  email: fixedEmail,
  askForProfile,
  openToAll,
  sendLabel,
  onStepChange,
  onSignedIn,
}: CodeSignInProps) {
  const [step, setStepState] = useState<CodeStep>("email");
  const [email, setEmail] = useState(fixedEmail ?? "");
  const [name, setName] = useState("");
  const [orgName, setOrgName] = useState("");
  const [sentTo, setSentTo] = useState<{ email: string; mailDelivered: boolean } | null>(null);
  const [code, setCode] = useState("");
  const [totp, setTotp] = useState("");
  const [cooldown, setCooldown] = useState(0);
  const [resent, setResent] = useState(false);
  const codeRef = useRef<HTMLInputElement>(null);
  const totpRef = useRef<HTMLInputElement>(null);

  const setStep = (next: CodeStep) => {
    setStepState(next);
    onStepChange?.(next);
  };

  // One tick a second while the resend control is waiting.
  useEffect(() => {
    if (cooldown <= 0) return;
    const t = window.setTimeout(() => setCooldown((s) => s - 1), 1000);
    return () => window.clearTimeout(t);
  }, [cooldown]);

  // The next thing to type is where the cursor goes.
  useEffect(() => {
    if (step === "code") codeRef.current?.focus();
    if (step === "totp") totpRef.current?.focus();
  }, [step]);

  const profile = () =>
    askForProfile ? { name: name.trim() || undefined, org_name: orgName.trim() || undefined } : {};

  const send = useMutation({
    mutationFn: (to: string) =>
      api.post<CodeSent>("/api/auth/code", { email: to, ...profile() }).then((r) => r.data),
  });

  const verify = useMutation({
    mutationFn: (body: { code: string; totp?: string }) =>
      api
        .post<VerifyAnswer>("/api/auth/code/verify", {
          email: sentTo?.email ?? email.trim(),
          code: body.code,
          totp: body.totp,
          ...profile(),
        })
        .then((r) => r.data),
  });

  const sendCode = (to: string, again: boolean) => {
    send.mutate(to, {
      onSuccess: (data) => {
        setSentTo({ email: to, mailDelivered: data.mail_delivered });
        setCode("");
        setTotp("");
        verify.reset();
        setCooldown(RESEND_COOLDOWN_S);
        setResent(again);
        if (step === "code") codeRef.current?.focus();
        else setStep("code");
      },
    });
  };

  const submitCode = (value: string) => {
    if (verify.isPending || value.length !== CODE_LENGTH) return;
    verify.mutate(
      { code: value },
      {
        onSuccess: async (data) => {
          if (data.status === "totp_required") {
            setStep("totp");
            return;
          }
          await onSignedIn(data);
        },
        onError: () => {
          setCode("");
          codeRef.current?.focus();
        },
      },
    );
  };

  const submitTotp = (value: string) => {
    if (verify.isPending || value.length !== CODE_LENGTH) return;
    verify.mutate(
      { code, totp: value },
      {
        onSuccess: async (data) => {
          if (data.status === "ok") await onSignedIn(data);
        },
        onError: () => {
          setTotp("");
          totpRef.current?.focus();
        },
      },
    );
  };

  /** Typing, or a phone filling in the code: keep the digits, submit at six. */
  const typed = (raw: string, set: (v: string) => void, submit: (v: string) => void, current: string) => {
    const digits = codeDigits(raw);
    set(digits);
    if (digits.length === CODE_LENGTH && digits !== current) submit(digits);
  };

  /**
   * A paste is read before `maxLength` trims it, so "482 913" arrives as six
   * digits rather than as "482 91".
   */
  const pasted = (e: ClipboardEvent<HTMLInputElement>, set: (v: string) => void, submit: (v: string) => void) => {
    e.preventDefault();
    const digits = codeDigits(e.clipboardData.getData("text"));
    set(digits);
    if (digits.length === CODE_LENGTH) submit(digits);
  };

  const differentEmail = () => {
    setSentTo(null);
    setCode("");
    setTotp("");
    setResent(false);
    send.reset();
    verify.reset();
    setStep("email");
  };

  const resendControl = (
    <Button
      tone="quiet"
      disabled={cooldown > 0 || send.isPending || !sentTo}
      onClick={() => sentTo && sendCode(sentTo.email, true)}
    >
      {send.isPending ? "Sending…" : cooldown > 0 ? `Send a new code in ${cooldown} s` : "Send a new code"}
    </Button>
  );

  const differentEmailControl = fixedEmail ? null : (
    <Button tone="quiet" onClick={differentEmail}>
      Use a different email
    </Button>
  );

  /* ── step 1: the address ─────────────────────────────────────────────── */

  if (step === "email" || !sentTo) {
    const err = send.error;
    const throttled = err instanceof ApiError && err.status === 429;
    const banner = throttled
      ? "Too many codes have been asked for. Wait ten minutes, then try again."
      : formError(err);
    const emailError = fieldError(err, "email");
    const busy = send.isPending;

    const submit = (e: SubmitEvent<HTMLFormElement>) => {
      e.preventDefault();
      if (busy) return;
      sendCode(email.trim(), false);
    };

    return (
      <form className="mf-form" onSubmit={submit} noValidate>
        {banner ? <FormError>{banner}</FormError> : null}

        <div className="mf-form__fields">
          {fixedEmail ? (
            <Field label="Email" htmlFor="mf-code-email" hint="Set by the invitation. The code goes to this address.">
              <input
                id="mf-code-email"
                className="mf-input mf-input--data"
                type="email"
                value={fixedEmail}
                readOnly
                aria-describedby="mf-code-email-hint"
              />
            </Field>
          ) : (
            <Field label="Email" htmlFor="mf-code-email" error={emailError}>
              <input
                id="mf-code-email"
                className="mf-input"
                type="email"
                inputMode="email"
                autoComplete="email"
                autoCapitalize="off"
                spellCheck={false}
                value={email}
                disabled={busy}
                required
                aria-invalid={emailError ? true : undefined}
                aria-describedby={describedBy("mf-code-email", { error: emailError })}
                onChange={(e) => setEmail(e.target.value)}
              />
            </Field>
          )}

          {askForProfile ? (
            <>
              <Field
                label="Your name"
                htmlFor="mf-code-name"
                hint="Optional. Shown on invitations and in member lists."
                error={fieldError(err, "name")}
              >
                <input
                  id="mf-code-name"
                  className="mf-input"
                  type="text"
                  autoComplete="name"
                  value={name}
                  disabled={busy}
                  aria-invalid={fieldError(err, "name") ? true : undefined}
                  aria-describedby={describedBy("mf-code-name", { error: fieldError(err, "name"), hint: true })}
                  onChange={(e) => setName(e.target.value)}
                />
              </Field>
              <Field
                label="Workspace name"
                htmlFor="mf-code-org"
                hint="Optional. The organisation your sites go in; you can rename it later."
                error={fieldError(err, "org_name")}
              >
                <input
                  id="mf-code-org"
                  className="mf-input"
                  type="text"
                  autoComplete="organization"
                  value={orgName}
                  disabled={busy}
                  aria-invalid={fieldError(err, "org_name") ? true : undefined}
                  aria-describedby={describedBy("mf-code-org", { error: fieldError(err, "org_name"), hint: true })}
                  onChange={(e) => setOrgName(e.target.value)}
                />
              </Field>
            </>
          ) : null}
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy || (!fixedEmail && !email.trim())}>
            {busy ? "Sending…" : sendLabel}
          </Button>
        </div>
      </form>
    );
  }

  /* ── what the second and third steps say about where the code went ──── */

  // The code input points at this too, so a screen reader landing on the
  // field hears where the code went before it hears the field's own hint.
  const whereItWent = sentTo.mailDelivered ? (
    <p className="mf-form__body" id="mf-code-where" aria-live="polite">
      {openToAll ? "A six-digit code is on its way to " : "If it can sign in here, a six-digit code is on its way to "}
      <strong>{sentTo.email}</strong>. It works once and expires in ten minutes.
    </p>
  ) : (
    <div className="mf-form__fields" aria-live="polite">
      <p className="mf-form__body" id="mf-code-where">
        This server does not send email, so no message is on its way. Whoever runs it can print
        a code for <strong>{sentTo.email}</strong> on the host with the command below. Enter that
        code here. It works once and expires in ten minutes.
      </p>
      <CopyField
        label="Run on the host"
        value={`docker compose exec ${COMPOSE_SERVICE} micaforge-server sign-in-code ${sentTo.email}`}
      />
    </div>
  );

  /* ── step 3: the authenticator ───────────────────────────────────────── */

  if (step === "totp") {
    const err = verify.error;
    const banner = !err
      ? undefined
      : isAuthenticatorRefusal(err)
        ? "That authenticator code is not right. The code changes every thirty seconds: enter the one your app shows now."
        : err instanceof ApiError && err.status === 401
          ? "The emailed code has expired, or too many wrong codes were tried with it. Send a new code."
          : err instanceof ApiError && err.status === 429
            ? "Too many attempts for this address. Wait ten minutes, then send a new code."
            : formError(err);
    const busy = verify.isPending;

    return (
      <form
        className="mf-form"
        onSubmit={(e) => {
          e.preventDefault();
          submitTotp(totp);
        }}
        noValidate
      >
        <p className="mf-form__body">
          This account has two-factor turned on. Enter the six-digit code your authenticator app
          shows for Micaforge.
        </p>
        {banner ? <FormError>{banner}</FormError> : null}

        <div className="mf-form__fields">
          <Field label="Authenticator code" htmlFor="mf-code-totp">
            <input
              id="mf-code-totp"
              ref={totpRef}
              className="mf-input mf-input--data mf-input--code"
              type="text"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9]*"
              maxLength={CODE_LENGTH}
              value={totp}
              readOnly={busy}
              aria-invalid={banner ? true : undefined}
              onChange={(e) => typed(e.target.value, setTotp, submitTotp, totp)}
              onPaste={(e) => pasted(e, setTotp, submitTotp)}
            />
          </Field>
        </div>

        <div className="mf-form__foot">
          <Button tone="primary" type="submit" disabled={busy || totp.length !== CODE_LENGTH}>
            {busy ? "Checking…" : "Sign in"}
          </Button>
          {resendControl}
          {differentEmailControl}
        </div>
      </form>
    );
  }

  /* ── step 2: the code ────────────────────────────────────────────────── */

  const err = verify.error ?? send.error;
  const banner = !err
    ? undefined
    : err instanceof ApiError && err.status === 401
      ? "That code is not right, or it has expired. Check that it is from the newest email, or send a new code."
      : err instanceof ApiError && err.status === 429
        ? err === send.error
          ? "Too many codes have been asked for. Wait ten minutes, then try again."
          : "Too many attempts for this address. Wait ten minutes, then send a new code."
        : formError(err);
  const busy = verify.isPending;

  return (
    <form
      className="mf-form"
      onSubmit={(e) => {
        e.preventDefault();
        submitCode(code);
      }}
      noValidate
    >
      {whereItWent}
      {banner ? <FormError>{banner}</FormError> : null}
      {resent && !banner && !send.isPending ? (
        <FormOk>A new code is on its way. Only the newest code works.</FormOk>
      ) : null}

      <div className="mf-form__fields">
        <Field label="Sign-in code" htmlFor="mf-code" hint="Six digits. Spaces and dashes are fine.">
          <input
            id="mf-code"
            ref={codeRef}
            className="mf-input mf-input--data mf-input--code"
            type="text"
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9]*"
            maxLength={CODE_LENGTH}
            value={code}
            readOnly={busy}
            aria-invalid={banner ? true : undefined}
            aria-describedby="mf-code-where mf-code-hint"
            onChange={(e) => typed(e.target.value, setCode, submitCode, code)}
            onPaste={(e) => pasted(e, setCode, submitCode)}
          />
        </Field>
      </div>

      <div className="mf-form__foot">
        <Button tone="primary" type="submit" disabled={busy || code.length !== CODE_LENGTH}>
          {busy ? "Checking…" : "Sign in"}
        </Button>
        {resendControl}
        {differentEmailControl}
      </div>
    </form>
  );
}

export default CodeSignIn;
