/**
 * The settings sub-layout, and the small set of controls the eleven settings
 * screens are built from.
 *
 * Two decisions govern everything here:
 *
 *   1. Settings is a list of rows, not a grid of cards: the name of a setting
 *      on the left, divided by a hairline from its control on the right. That
 *      is `Spec` / `SpecRow`.
 *   2. The section list sits inside the content area, never as a second app
 *      shell. It carries the section index and nothing else: no logo, no live
 *      counts, no second top bar.
 *
 * The controls live here rather than in a twelfth file because they exist only
 * for this area; components/react/ui.tsx is where anything used beyond
 * settings belongs.
 */
import type { ReactNode } from "react";
import { useEffect, useId, useRef, useState } from "react";
import { NavLink, Outlet, useParams } from "react-router";
import * as RCheckbox from "@radix-ui/react-checkbox";
import * as RSwitch from "@radix-ui/react-switch";
import type { UseQueryResult } from "@tanstack/react-query";

import { Button, Empty, ErrorState } from "../../components/react/ui";
import {
  IconAlert,
  IconCheck,
  IconChevron,
  IconClock,
  IconClose,
  IconPartial,
} from "../../components/react/icons";
import { ApiError } from "../../lib/api";
import "../../styles/app/settings.css";

/* ── the section index ───────────────────────────────────────────────────── */

type Tab = { to: string; label: string; end?: boolean };

const SITE_TABS: Tab[] = [
  { to: "", label: "Tracking setup", end: true },
  { to: "/site", label: "Site details" },
  { to: "/alerts", label: "Alerts" },
  { to: "/reports", label: "Reports" },
  { to: "/imports", label: "Imports" },
  { to: "/share", label: "Sharing" },
  { to: "/danger", label: "Danger zone" },
];

const ACCOUNT_TABS: Tab[] = [
  { to: "", label: "Your account", end: true },
  { to: "/organisation", label: "Organisation" },
  { to: "/teams", label: "Teams and access" },
  { to: "/keys", label: "API keys" },
];

export function SettingsLayout({
  scope,
  children,
}: {
  scope: "site" | "account";
  children?: ReactNode;
}) {
  const { site } = useParams();
  const base = scope === "site" ? `/app/${site ?? ""}/settings` : "/app/settings";
  const tabs = scope === "site" ? SITE_TABS : ACCOUNT_TABS;

  return (
    <div className="mf-set">
      <nav className="mf-set-nav" aria-label={scope === "site" ? "Site setup" : "Account setup"}>
        <h1 className="mf-set-nav__title">{scope === "site" ? "Site setup" : "Account"}</h1>
        <ul className="mf-set-nav__list">
          {tabs.map((t) => (
            <li key={t.to || "index"}>
              <NavLink
                to={`${base}${t.to}`}
                end={t.end}
                className={({ isActive }) => `mf-set-tab${isActive ? " is-live" : ""}`}
              >
                {t.label}
              </NavLink>
            </li>
          ))}
        </ul>
      </nav>

      <div className="mf-set-body">{children ?? <Outlet />}</div>
    </div>
  );
}

/* ── settings rows ───────────────────────────────────────────────────────── */

export function Spec({ children }: { children: ReactNode }) {
  return <div className="mf-set-spec">{children}</div>;
}

export function SpecRow({
  name,
  hint,
  htmlFor,
  stack,
  inline,
  children,
}: {
  name: ReactNode;
  hint?: ReactNode;
  htmlFor?: string;
  /** Full-width settings (a snippet, a long list) drop the label column. */
  stack?: boolean;
  /** Lays a control and its adjuncts on one line. */
  inline?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`mf-set-spec__row${stack ? " mf-set-spec__row--stack" : ""}`}>
      <div className="mf-set-spec__label">
        {htmlFor ? (
          <label className="mf-set-spec__name" htmlFor={htmlFor}>
            {name}
          </label>
        ) : (
          <div className="mf-set-spec__name">{name}</div>
        )}
        {hint ? <p className="mf-set-spec__hint">{hint}</p> : null}
      </div>
      <div className={`mf-set-spec__control${inline ? " mf-set-spec__control--inline" : ""}`}>
        {children}
      </div>
    </div>
  );
}

/** A setting that is reported rather than changed. */
export function Stated({ children }: { children: ReactNode }) {
  return <div className="mf-set-spec__stated">{children}</div>;
}

export function Block({
  title,
  body,
  children,
}: {
  title: ReactNode;
  body?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <section className="mf-set-block">
      <h3 className="mf-set-block__title">{title}</h3>
      {body ? <p className="mf-set-block__body">{body}</p> : null}
      {children ? <div className="mf-set-block__work">{children}</div> : null}
    </section>
  );
}

export function Foot({ children, tight }: { children: ReactNode; tight?: boolean }) {
  return <div className={`mf-set-foot${tight ? " mf-set-foot--tight" : ""}`}>{children}</div>;
}

/* ── fields ──────────────────────────────────────────────────────────────── */

export function Field({
  label,
  hint,
  error,
  htmlFor,
  children,
}: {
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div className="mf-set-field">
      {label ? (
        <label className="mf-set-field__label" htmlFor={htmlFor}>
          {label}
        </label>
      ) : null}
      {children}
      {error ? <p className="mf-set-field__err">{error}</p> : null}
      {hint && !error ? <p className="mf-set-field__hint">{hint}</p> : null}
    </div>
  );
}

type InputProps = React.InputHTMLAttributes<HTMLInputElement> & { data?: boolean };

export function TextInput({ data, className, ...rest }: InputProps) {
  return (
    <input
      {...rest}
      className={`mf-set-input${data ? " mf-set-input--data" : ""}${className ? ` ${className}` : ""}`}
    />
  );
}

/** A figure set inside a written rule: "…exceed [ 3 ]× the average". */
export function NumberInput({ wide, className, ...rest }: InputProps & { wide?: boolean }) {
  return (
    <input
      type="number"
      inputMode="decimal"
      {...rest}
      className={`mf-set-input mf-set-num${wide ? " mf-set-num--wide" : ""}${className ? ` ${className}` : ""}`}
    />
  );
}

export function TextArea({
  data,
  className,
  ...rest
}: React.TextareaHTMLAttributes<HTMLTextAreaElement> & { data?: boolean }) {
  return (
    <textarea
      {...rest}
      className={`mf-set-area${data ? " mf-set-area--data" : ""}${className ? ` ${className}` : ""}`}
    />
  );
}

export function Select({
  auto,
  children,
  className,
  ...rest
}: React.SelectHTMLAttributes<HTMLSelectElement> & { auto?: boolean }) {
  return (
    <span className={`mf-set-selwrap${auto ? " mf-set-selwrap--auto" : ""}`}>
      <select {...rest} className={`mf-set-select${className ? ` ${className}` : ""}`}>
        {children}
      </select>
      <IconChevron size={14} className="mf-set-selwrap__caret" />
    </span>
  );
}

/**
 * An on/off switch. State reads by the thumb's POSITION first and the fill
 * second, and the word beside it says On or Off, so the setting is legible
 * with no colour at all.
 */
export function Toggle({
  checked,
  onChange,
  label,
  disabled,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  /** Names the setting for a screen reader when no visible label sits beside it. */
  label: string;
  disabled?: boolean;
  id?: string;
}) {
  return (
    <span className="mf-set-swrow">
      <RSwitch.Root
        id={id}
        className="mf-set-switch"
        checked={checked}
        onCheckedChange={onChange}
        disabled={disabled}
        aria-label={label}
      >
        <RSwitch.Thumb className="mf-set-switch__lever" />
      </RSwitch.Root>
      <span className="mf-set-swrow__state" aria-hidden="true">
        {checked ? "On" : "Off"}
      </span>
    </span>
  );
}

export function CheckRow({
  checked,
  onChange,
  label,
  hint,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: ReactNode;
  hint?: ReactNode;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className="mf-set-checkrow">
      <RCheckbox.Root
        id={id}
        className="mf-set-check"
        checked={checked}
        disabled={disabled}
        onCheckedChange={(v) => onChange(v === true)}
      >
        <RCheckbox.Indicator className="mf-set-check__mark">
          <IconCheck size={11} />
        </RCheckbox.Indicator>
      </RCheckbox.Root>
      <label className="mf-set-checkrow__text" htmlFor={id}>
        {label}
        {hint ? <span className="mf-set-checkrow__hint">{hint}</span> : null}
      </label>
    </div>
  );
}

/* ── marks and small states ──────────────────────────────────────────────── */

export type StateName = "done" | "failed" | "running" | "waiting";

/** Each state has its own drawn glyph as well as its tone. No box behind it. */
export function Status({ state, label }: { state: StateName; label: string }) {
  const Glyph =
    state === "done" ? IconCheck : state === "failed" ? IconClose : state === "running" ? IconPartial : IconClock;
  return (
    <span className="mf-set-status" data-state={state}>
      <Glyph size={12} />
      {label}
    </span>
  );
}

export function Caution({ children }: { children: ReactNode }) {
  return (
    <p className="mf-set-caution">
      <IconAlert size={14} />
      <span>{children}</span>
    </p>
  );
}

export function Note({ children }: { children: ReactNode }) {
  return <p className="mf-set-note">{children}</p>;
}

/** A progress bar: a light track with the done part filled. */
export function Gauge({
  value,
  label,
  inline,
}: {
  value: number;
  label: string;
  /** Sized to sit beside a word in a table row rather than fill a panel. */
  inline?: boolean;
}) {
  const v = Math.max(0, Math.min(1, Number.isFinite(value) ? value : 0));
  return (
    <span
      className={`mf-set-gauge${inline ? " mf-set-gauge--inline" : ""}`}
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(v * 100)}
      aria-label={label}
    >
      <span className="mf-set-gauge__fill" style={{ width: `${v * 100}%` }} />
    </span>
  );
}

/* ── query and mutation plumbing ─────────────────────────────────────────── */

/**
 * The site identifier as the query envelope wants it. A helper rather than a
 * cast at forty call sites, and it keeps this area honest about the fact that
 * the provider owns the shape.
 */
export const asSite = (v: unknown): string | number => v as string | number;

/** Names the problem and the way out of it. Never just "an error occurred". */
export function errText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === "network")
      return "Could not reach the Micaforge server. Check that it is running, then try again.";
    if (e.status === 401) return "Your session has expired. Sign in again to continue.";
    if (e.status === 403) return "Your role on this organisation does not allow that change.";
    if (e.status === 409) return `${e.message} Reload to see the current state, then try again.`;
    return e.message;
  }
  if (e instanceof Error && e.message) return e.message;
  return "Something failed and the server did not say what. Try again.";
}

/** The placeholder rows a settings list shows while it loads. */
export function Waiting({ rows = 3 }: { rows?: number }) {
  return (
    <div className="mf-set-spec" aria-busy="true" aria-live="polite">
      <span className="mf-sr">Loading settings…</span>
      {Array.from({ length: rows }).map((_, i) => (
        <div className="mf-set-spec__row" key={i}>
          <div className="mf-set-spec__label">
            <span className="mf-wait" style={{ width: `${52 + ((i * 17) % 34)}%` }} />
          </div>
          <div className="mf-set-spec__control">
            <span className="mf-wait" style={{ width: `${38 + ((i * 23) % 48)}%` }} />
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * Loading, error and empty for one panel's body. Every panel in this area goes
 * through it, so no screen can quietly ship only its happy path.
 */
export function QueryState<T>({
  query,
  loading,
  emptyWhen,
  empty,
  children,
}: {
  query: UseQueryResult<T>;
  loading?: ReactNode;
  emptyWhen?: (data: T) => boolean;
  empty?: ReactNode;
  children: (data: T) => ReactNode;
}) {
  if (query.isPending) return <>{loading ?? <Waiting />}</>;
  if (query.isError)
    return (
      <ErrorState
        title="Could not load these settings"
        detail={errText(query.error)}
        onRetry={() => void query.refetch()}
      />
    );
  const data = query.data as T;
  if (emptyWhen?.(data)) return <>{empty ?? <Empty />}</>;
  return <>{children(data)}</>;
}

export interface ActionState {
  isPending: boolean;
  isError: boolean;
  isSuccess: boolean;
  error: unknown;
}

/** One quiet line reporting what an action did. Never a floating toast. */
export function Result({
  action,
  done = "Saved.",
  working = "Working…",
}: {
  action: ActionState;
  done?: string;
  working?: string;
}) {
  if (action.isPending) return <span className="mf-set-foot__say">{working}</span>;
  if (action.isError)
    return (
      <span className="mf-set-foot__say" data-tone="warn" role="alert">
        {errText(action.error)}
      </span>
    );
  if (action.isSuccess)
    return (
      <span className="mf-set-foot__say" data-tone="good">
        {done}
      </span>
    );
  return null;
}

/**
 * The same line, in its own action row, and no row at all while there is
 * nothing to say. An empty strip under a panel is a visible band of nothing,
 * which is worse than no strip.
 */
export function ResultFoot({
  action,
  done,
  working,
  children,
}: {
  action: ActionState;
  done?: string;
  working?: string;
  children?: ReactNode;
}) {
  if (!action.isPending && !action.isError && !action.isSuccess && !children) return null;
  return (
    <Foot tight>
      <Result action={action} done={done} working={working} />
      {children}
    </Foot>
  );
}

/**
 * Holds an editable copy of what the server said, and knows whether the copy
 * has drifted from it. A save bar that cannot tell clean from dirty is a save
 * bar that lies.
 */
export function useSyncedForm<T>(server: T | undefined) {
  const [form, setForm] = useState<T | null>(null);
  const baseline = server === undefined ? null : JSON.stringify(server);
  const last = useRef<string | null>(null);

  useEffect(() => {
    if (baseline == null || last.current === baseline) return;
    last.current = baseline;
    setForm(JSON.parse(baseline) as T);
  }, [baseline]);

  const dirty = form != null && baseline != null && JSON.stringify(form) !== baseline;

  return {
    form,
    dirty,
    set: (patch: Partial<T>) => setForm((f) => (f ? { ...f, ...patch } : f)),
    setAll: (v: T) => setForm(v),
    reset: () => {
      if (baseline != null) setForm(JSON.parse(baseline) as T);
    },
  };
}

/* ── typed confirmation ──────────────────────────────────────────────────── */

/**
 * A destructive action is confirmed by typing the exact name of the thing it
 * will destroy. No "are you sure" dialog.
 */
export function TypedConfirm({
  phrase,
  label,
  destroys,
  busy,
  onConfirm,
}: {
  /** The exact string that must be typed: always the thing being destroyed. */
  phrase: string;
  /** The action's own words, e.g. "Delete this site". */
  label: string;
  /** Named, itemised, in plain words. This is not reversible, so say so. */
  destroys: ReactNode;
  busy?: boolean;
  onConfirm: () => void;
}) {
  const id = useId();
  const [typed, setTyped] = useState("");
  const armed = typed.trim() === phrase;

  return (
    <div className="mf-set-stack">
      <Caution>{destroys}</Caution>
      <div className="mf-set-sign">
        <label className="mf-set-field__label" htmlFor={id}>
          Type <span className="mf-set-sign__phrase">{phrase}</span> to confirm
        </label>
        {/* No placeholder: a greyed copy of the phrase reads as an already
            filled field, and this is the last field anyone should misread. */}
        <input
          id={id}
          className="mf-set-sign__line"
          value={typed}
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => setTyped(e.target.value)}
        />
      </div>
      <div>
        <Button
          tone="danger"
          disabled={!armed || busy}
          onClick={() => {
            if (!armed) return;
            setTyped("");
            onConfirm();
          }}
        >
          {busy ? "Working…" : label}
        </Button>
      </div>
    </div>
  );
}

export default SettingsLayout;
