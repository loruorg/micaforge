/**
 * The frame every unauthenticated screen sits in: the brand, one centred card
 * holding the form, and a short line about what Micaforge is.
 *
 * The form primitives every entry screen shares live here too, so the sign-in,
 * invitation and share screens draw their fields the same way.
 */
import type { ReactNode } from "react";
import { useId, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { IconAlert, IconChevron, IconOut, Mark } from "../../components/react/icons";
import { ApiError, api } from "../../lib/api";
import "../../styles/app/entry.css";

/* ── where the entry screens live ────────────────────────────────────────
   One place names every route this area navigates to, so the router mounts
   exactly these paths and no screen guesses at a sibling's URL. Signing up is
   signing in: a code sent to an address does both, so there is no separate
   signup, recovery or reset screen. */
export const ENTRY_PATHS = {
  login: "/app/login",
  invitation: "/app/invitation",
  chooser: "/app",
  addSite: "/app/new",
  record: (site: string | number) => `/app/${site}`,
  onboarding: (site: string | number) => `/app/${site}/onboarding`,
  tracking: (site: string | number) => `/app/${site}/settings/tracking`,
} as const;

/* ── the frame ───────────────────────────────────────────────────────────── */

export interface AuthFrameProps {
  /** The card's heading. Rendered as the page's h1. */
  title: ReactNode;
  note?: ReactNode;
  children: ReactNode;
  /** A line of context under the card, for screens that need one. */
  aside?: ReactNode;
}

export function AuthFrame({ title, note, children, aside }: AuthFrameProps) {
  return (
    <div className="mf-entry">
      <a className="mf-entry__brand" href="/" aria-label="Micaforge home">
        <Mark size={30} />
        <span className="mf-entry__wordmark">Micaforge</span>
      </a>

      <main id="main" className="mf-entry__card">
        <div className="mf-form">
          <div>
            <h1 className="mf-form__title">{title}</h1>
            {note ? <p className="mf-form__note">{note}</p> : null}
          </div>
          {children}
        </div>
      </main>

      {aside ? <div className="mf-entry__aside">{aside}</div> : null}

      <p className="mf-entry__colophon">
        Cookie-free, open-source web analytics.{" "}
        <a className="mf-entry__link" href="/why-agent-analytics">
          See what AI does with your site
          <IconOut size={12} />
        </a>
      </p>
    </div>
  );
}

/* ── form primitives ─────────────────────────────────────────────────────── */

export interface FieldProps {
  label: ReactNode;
  htmlFor: string;
  error?: string;
  hint?: ReactNode;
  /** A quiet control on the label line, e.g. the password reveal. */
  action?: ReactNode;
  children: ReactNode;
}

export function Field({ label, htmlFor, error, hint, action, children }: FieldProps) {
  return (
    <div className="mf-field">
      <div className="mf-field__row">
        <label className="mf-field__label" htmlFor={htmlFor}>
          {label}
        </label>
        {action}
      </div>
      {children}
      {hint && !error ? (
        <p className="mf-field__hint" id={`${htmlFor}-hint`}>
          {hint}
        </p>
      ) : null}
      {error ? (
        <p className="mf-field__err" id={`${htmlFor}-error`}>
          <IconAlert size={13} />
          {error}
        </p>
      ) : null}
    </div>
  );
}

/**
 * The `aria-describedby` for a field: its error when it has one, otherwise its
 * hint. Written once so no control quietly forgets to point at its own rules.
 */
export function describedBy(id: string, opts: { error?: string; hint?: unknown }): string | undefined {
  if (opts.error) return `${id}-error`;
  if (opts.hint) return `${id}-hint`;
  return undefined;
}

/**
 * A password input with a real reveal control instead of a decorative icon.
 * Accounts have no password; this is for the optional password on a shared
 * dashboard link (ShareScreen).
 */
export function PasswordField({
  id,
  label,
  value,
  onChange,
  error,
  hint,
  autoComplete,
  disabled,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  error?: string;
  hint?: ReactNode;
  autoComplete?: string;
  disabled?: boolean;
}) {
  const [shown, setShown] = useState(false);
  return (
    <Field
      label={label}
      htmlFor={id}
      error={error}
      hint={hint}
      action={
        <button
          type="button"
          className="mf-reveal"
          aria-controls={id}
          aria-label={shown ? "Hide the password" : "Show the password"}
          onClick={() => setShown((v) => !v)}
        >
          {shown ? "Hide" : "Show"}
        </button>
      }
    >
      <input
        id={id}
        className="mf-input"
        type={shown ? "text" : "password"}
        value={value}
        autoComplete={autoComplete}
        disabled={disabled}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy(id, { error, hint })}
        onChange={(e) => onChange(e.target.value)}
      />
    </Field>
  );
}

/** A native select, kept native for keyboard and screen readers; the caret is
    the house chevron rather than the platform's own. */
export function Select({
  id,
  value,
  onChange,
  disabled,
  invalid,
  describedBy,
  children,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
  invalid?: boolean;
  describedBy?: string;
  children: ReactNode;
}) {
  return (
    <span className="mf-select">
      <select
        id={id}
        className="mf-input mf-input--data"
        value={value}
        disabled={disabled}
        aria-invalid={invalid ? true : undefined}
        aria-describedby={describedBy}
        onChange={(e) => onChange(e.target.value)}
      >
        {children}
      </select>
      <IconChevron size={14} className="mf-select__caret" />
    </span>
  );
}

export function FormError({ children }: { children: ReactNode }) {
  return (
    <p className="mf-form__err" role="alert">
      <IconAlert size={14} />
      <span>{children}</span>
    </p>
  );
}

export function FormOk({ children }: { children: ReactNode }) {
  return (
    <p className="mf-form__ok" role="status">
      <span>{children}</span>
    </p>
  );
}

/** A short bar of ruled paper standing in while a read is in flight. */
export function Waiting({ label, lines = 3 }: { label: string; lines?: number }) {
  const uid = useId();
  return (
    <div className="mf-waiting" aria-busy="true" aria-live="polite">
      <span className="mf-sr">{label}</span>
      {Array.from({ length: lines }).map((_, i) => (
        <span key={`${uid}-${i}`} className="mf-wait" style={{ width: `${86 - i * 18}%` }} aria-hidden="true" />
      ))}
    </div>
  );
}

/* ── reading the server's errors ─────────────────────────────────────────── */

/** The message the server attached to one named field, if it named this one. */
export function fieldError(err: unknown, field: string): string | undefined {
  return err instanceof ApiError && err.field === field ? err.message : undefined;
}

/**
 * The message for the form as a whole. A failure the server pinned to a field
 * is already shown against that field, so it is not repeated here.
 */
export function formError(err: unknown): string | undefined {
  if (!err) return undefined;
  if (err instanceof ApiError) {
    if (err.field) return undefined;
    if (err.code === "network")
      return "Could not reach the Micaforge server. Check that it is running, then try again.";
    if (err.status === 429) return "Too many attempts. Wait a minute, then try again.";
    if (err.status >= 500) return "The server failed on that request. Try again in a moment.";
    return err.message;
  }
  return "Something went wrong on the way to the server. Try again.";
}

/* ── what this instance allows ───────────────────────────────────────────── */

/**
 * `GET /api/config` (CONTRACT §3.3). Only the fields the entry screens act on
 * are read here; the rest of the payload is ignored on purpose.
 */
export interface InstanceConfig {
  registration_open: boolean;
  /** True while the install has no account: the next one to sign in owns it. */
  setup_required?: boolean;
  /** Whether this install sends mail at all. */
  mail_enabled?: boolean;
  self_hosted?: boolean;
  version?: string;
  /** The sign-in providers this install has a client for. Empty when none is set up. */
  oauth_providers?: OAuthProvider[];
  /** Plans and payments. Off on every self-hosted install unless its operator turns them on. */
  billing?: { enabled: boolean; stripe: boolean };
}

export type OAuthProvider = "google" | "github";

/**
 * A closed instance is a real, common configuration, not an error, so the
 * sign-in screen asks before it offers to create an account. While the answer
 * is unknown the screen assumes closed, because promising an account the server
 * will not create is worse than a sentence explaining the door is shut.
 */
export function useInstanceConfig() {
  return useQuery({
    queryKey: ["config"],
    queryFn: () => api.get<InstanceConfig>("/api/config").then((r) => r.data),
    staleTime: 5 * 60 * 1000,
    retry: 1,
  });
}

export default AuthFrame;
