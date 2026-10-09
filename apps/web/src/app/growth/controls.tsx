/**
 * The small controls the Growth screens are built from.
 *
 * They live in this area rather than in components/react/ui.tsx because only
 * these two screens use them, and the settings and bench areas keep their own
 * field controls for the same reason. They follow the same rules as those:
 *
 *   - A field's label is a real <label> tied to its control, and an error is
 *     written under the field it belongs to, in words, never as a red border
 *     alone.
 *   - A segmented choice is a row of buttons with `aria-pressed`, the same grey
 *     track and white pressed key as the audience switch.
 *   - A ranked list is the shared Sheet with its share bar drawn against the
 *     leading row, the way every other ranked table in the product draws it.
 */
import type { ComponentProps, ReactNode, SelectHTMLAttributes } from "react";
import { useEffect, useId, useRef, useState } from "react";

import { Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconCheck, IconChevron, IconCopy } from "../../components/react/icons";
import { num, pct } from "../../lib/format";
import { againstLeader } from "../display";
import type { Ranked } from "./model";

/* ── hooks ───────────────────────────────────────────────────────────────── */

/** The value as it was a moment ago: for a search box, or an image address. */
export function useDebounced<T>(value: T, ms: number): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setSettled(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return settled;
}

/* ── fields ──────────────────────────────────────────────────────────────── */

export function Field({
  label,
  hint,
  error,
  wide,
  children,
}: {
  label: ReactNode;
  hint?: ReactNode;
  error?: string | null;
  wide?: boolean;
  children: (id: string, describedBy: string | undefined) => ReactNode;
}) {
  const id = useId();
  const noteId = `${id}-note`;
  const said = error || hint;
  return (
    <div className={`mf-gr-field${wide ? " mf-gr-field--wide" : ""}`}>
      <label className="mf-gr-field__label" htmlFor={id}>
        {label}
      </label>
      {children(id, said ? noteId : undefined)}
      {error ? (
        <p id={noteId} className="mf-gr-field__err" role="alert">
          {error}
        </p>
      ) : hint ? (
        <p id={noteId} className="mf-gr-field__hint">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function TextInput({
  mono,
  invalid,
  className,
  ...rest
}: ComponentProps<"input"> & { mono?: boolean; invalid?: boolean }) {
  return (
    <input
      type="text"
      {...rest}
      aria-invalid={invalid ? true : undefined}
      className={`mf-gr-input${mono ? " mf-data" : ""}${className ? ` ${className}` : ""}`}
    />
  );
}

export function TextArea({
  invalid,
  className,
  ...rest
}: ComponentProps<"textarea"> & { invalid?: boolean }) {
  return (
    <textarea
      {...rest}
      aria-invalid={invalid ? true : undefined}
      className={`mf-gr-input mf-gr-input--area${className ? ` ${className}` : ""}`}
    />
  );
}

export function Choice({
  children,
  className,
  ...rest
}: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <span className={`mf-gr-choice${className ? ` ${className}` : ""}`}>
      <select {...rest} className="mf-gr-choice__el">
        {children}
      </select>
      <IconChevron size={12} className="mf-gr-choice__caret" />
    </span>
  );
}

/* ── segmented choice ────────────────────────────────────────────────────── */

export function Bank<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: { v: T; label: ReactNode }[];
  onChange: (v: T) => void;
}) {
  return (
    <div className="mf-gr-bank" role="group" aria-label={label}>
      {options.map((o) => (
        <button
          key={o.v}
          type="button"
          className="mf-gr-bank__key"
          aria-pressed={value === o.v}
          onClick={() => onChange(o.v)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/**
 * An on/off switch. The thumb's position and the word beside it both say the
 * state, so it reads with no colour at all.
 */
export function Switch({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <span className="mf-gr-switch">
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={label}
        disabled={disabled}
        className="mf-gr-switch__track"
        onClick={() => onChange(!checked)}
      >
        <span className="mf-gr-switch__thumb" aria-hidden="true" />
      </button>
      <span className="mf-gr-switch__word" aria-hidden="true">
        {checked ? "On" : "Off"}
      </span>
    </span>
  );
}

/* ── one line of outcome ─────────────────────────────────────────────────── */

export function Say({ tone, children }: { tone?: "good" | "warn"; children: ReactNode }) {
  return (
    <p className="mf-gr-say" data-tone={tone} role={tone === "warn" ? "alert" : undefined}>
      {children}
    </p>
  );
}

/** Grey bars stand in for content while it loads. Never a spinner. */
export function WaitLines({ lines = 3, label = "Loading…" }: { lines?: number; label?: string }) {
  return (
    <div className="mf-gr-wait" aria-busy="true" aria-live="polite">
      <span className="mf-sr">{label}</span>
      {Array.from({ length: lines }).map((_, i) => (
        <span key={i} className="mf-wait" style={{ width: `${36 + ((i * 23) % 50)}%` }} />
      ))}
    </div>
  );
}

/* ── copy, as one icon key ───────────────────────────────────────────────── */

/**
 * Copies a value from inside a clickable table row. The click and the key
 * press stop here, so copying a link never also opens it.
 */
export function CopyKey({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const t = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(t.current), []);
  return (
    <button
      type="button"
      className="mf-gr-copykey"
      aria-label={copied ? "Copied" : label}
      title={copied ? "Copied" : label}
      onKeyDown={(e) => e.stopPropagation()}
      onClick={async (e) => {
        e.stopPropagation();
        try {
          await navigator.clipboard.writeText(value);
          setCopied(true);
          window.clearTimeout(t.current);
          t.current = window.setTimeout(() => setCopied(false), 1600);
        } catch {
          /* clipboard blocked: the address is on screen and selectable */
        }
      }}
    >
      {copied ? <IconCheck size={13} /> : <IconCopy size={13} />}
    </button>
  );
}

/* ── a ranked list with share bars ───────────────────────────────────────── */

export function RankedList({
  title,
  note,
  rows,
  name,
  mono,
  empty,
}: {
  title: string;
  note?: string;
  rows: Ranked[];
  name: (value: string) => string;
  mono?: boolean;
  empty: string;
}) {
  const total = rows.reduce((s, r) => s + (Number.isFinite(r.clicks) ? r.clicks : 0), 0);
  const share = (r: Ranked) => (total > 0 ? r.clicks / total : undefined);
  const columns: Column<Ranked>[] = [
    {
      key: "name",
      head: title,
      share: againstLeader(rows, share),
      render: (r) => {
        const text = name(r.value);
        return (
          <span className={mono && r.value ? "mf-data" : undefined} title={text}>
            {text}
          </span>
        );
      },
    },
    {
      key: "clicks",
      head: "Clicks",
      numeric: true,
      width: "4.5rem",
      render: (r) => <span className="mf-data">{num(r.clicks)}</span>,
    },
    {
      key: "share",
      head: "Share",
      numeric: true,
      width: "4rem",
      render: (r) => <span className="mf-data">{pct(share(r))}</span>,
    },
  ];
  return (
    <section className="mf-gr-rank" aria-label={title}>
      <Sheet<Ranked>
        rows={rows}
        columns={columns}
        getKey={(r, i) => `${r.value}-${i}`}
        empty={
          <p className="mf-gr-rank__none">
            <span className="mf-gr-rank__head">{title}</span>
            {empty}
          </p>
        }
      />
      {note ? <p className="mf-gr-rank__note">{note}</p> : null}
    </section>
  );
}
