/**
 * The small pieces the Uptime and Status pages screens are built from.
 *
 * Why this file exists instead of borrowing the settings controls: the
 * settings area keeps its form controls to itself (its own header says that
 * anything used beyond settings belongs in ui.tsx), and the monitoring screens
 * may not edit ui.tsx. So the few controls these two editors need are here,
 * drawn as native elements: a native checkbox and a native select are already
 * keyboard-complete and announced correctly, which a custom control has to earn.
 *
 * Three rules hold for everything below:
 *
 *   1. A state is a word AND a shape AND a tone. Never a coloured dot alone.
 *   2. A server error that names a field is printed under that field, in the
 *      server's own words. The person fixing the form should not have to guess
 *      which box "must be between 1 and 60" means.
 *   3. Destroying something is confirmed in place, in a sentence that names
 *      what goes. No modal.
 */
import type { ReactNode } from "react";
import { Button } from "../../components/react/ui";
import { IconChevron } from "../../components/react/icons";
import { StateShape } from "../../components/status/shapes";
import { when } from "../../components/status/words";
import type { MonitorState } from "../../components/status/words";
import { ms } from "../../lib/format";
import type { CheckResult } from "./types";
import { daysText } from "./types";

/* ── a monitor's state ───────────────────────────────────────────────────── */

const STATE_WORD: Record<MonitorState | "paused", string> = {
  up: "Up",
  down: "Down",
  unknown: "Not checked yet",
  paused: "Paused",
};

/**
 * A monitor's state as a shape and a word. A disabled monitor is "Paused"
 * whatever its last state was, because a paused monitor's last reading is
 * history, not the present.
 */
export function StateMark({
  state,
  enabled = true,
  big,
  label,
}: {
  state: MonitorState;
  enabled?: boolean;
  big?: boolean;
  /** Replaces the state word, for a count such as "2 down". */
  label?: string;
}) {
  const s = enabled ? state : "paused";
  return (
    <span className={`mf-mon-state${big ? " mf-mon-state--big" : ""}`} data-state={s}>
      <StateShape shape={s} size={big ? 14 : 11} />
      {label ?? STATE_WORD[s]}
    </span>
  );
}

/** One check's verdict: "Passed" or "Failed", with the same shapes. */
export function PassMark({ ok }: { ok: boolean }) {
  return (
    <span className="mf-mon-state" data-state={ok ? "up" : "down"}>
      <StateShape shape={ok ? "up" : "down"} size={11} />
      {ok ? "Passed" : "Failed"}
    </span>
  );
}

/* ── a metric strip whose figures may be more than text ──────────────────── */

export interface StripItem {
  key: string;
  label: string;
  value: ReactNode;
  note?: ReactNode;
}

/**
 * The shared metric strip, drawn with the same classes as `Readout`, for the
 * one figure `Readout` cannot hold: a state, which is a shape and a word
 * rather than a string.
 */
export function Strip({ items, dense }: { items: StripItem[]; dense?: boolean }) {
  return (
    <div className={`mf-readout mf-mon-strip${dense ? " mf-readout--dense" : ""}`} role="list">
      {items.map((it) => (
        <div key={it.key} className="mf-readout__chan" role="listitem">
          <span className="mf-readout__label">{it.label}</span>
          <span className="mf-readout__value">{it.value}</span>
          <span className="mf-readout__foot">
            {it.note ? <span className="mf-readout__note">{it.note}</span> : null}
          </span>
        </div>
      ))}
    </div>
  );
}

/** The strip while it loads: the same rhythm, grey shapes, no figures. */
export function StripWaiting({ count, dense }: { count: number; dense?: boolean }) {
  return (
    <div
      className={`mf-readout mf-mon-strip${dense ? " mf-readout--dense" : ""}`}
      aria-busy="true"
      aria-live="polite"
    >
      <span className="mf-sr">Loading…</span>
      {Array.from({ length: count }).map((_, i) => (
        <div key={i} className="mf-readout__chan">
          <span className="mf-wait mf-mon-wait-label" />
          <span className="mf-wait mf-mon-wait-value" />
          <span className="mf-wait mf-mon-wait-foot" />
        </div>
      ))}
    </div>
  );
}

/* ── a small segmented control ───────────────────────────────────────────── */

/** The date range control's look, for a control with its own few options. */
export function Segments<T extends string>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { v: T; label: string }[];
  onChange: (v: T) => void;
  label: string;
}) {
  return (
    <div className="mf-caliper mf-mon-segments">
      <div className="mf-caliper__jaws" role="group" aria-label={label}>
        {options.map((o) => (
          <button
            key={o.v}
            type="button"
            className="mf-caliper__stop"
            aria-pressed={value === o.v}
            onClick={() => onChange(o.v)}
          >
            {o.label}
          </button>
        ))}
      </div>
    </div>
  );
}

/* ── form fields ─────────────────────────────────────────────────────────── */

/**
 * A label, its control, and under it either the error or the hint. The error
 * carries `${id}-err` so the control can point at it with aria-describedby.
 */
export function Field({
  id,
  label,
  hint,
  error,
  children,
  wide,
}: {
  id?: string;
  label: ReactNode;
  hint?: ReactNode;
  error?: string;
  children: ReactNode;
  wide?: boolean;
}) {
  return (
    <div className={`mf-mon-field${wide ? " mf-mon-field--wide" : ""}`}>
      {id ? (
        <label className="mf-mon-field__label" htmlFor={id}>
          {label}
        </label>
      ) : (
        <span className="mf-mon-field__label">{label}</span>
      )}
      {children}
      {error ? (
        <p className="mf-mon-field__err" id={id ? `${id}-err` : undefined}>
          {error}
        </p>
      ) : hint ? (
        <p className="mf-mon-field__hint">{hint}</p>
      ) : null}
    </div>
  );
}

type InputProps = React.InputHTMLAttributes<HTMLInputElement> & { data?: boolean };

export function Input({ data, className, ...rest }: InputProps) {
  return (
    <input
      {...rest}
      className={`mf-mon-input${data ? " mf-mon-input--data" : ""}${className ? ` ${className}` : ""}`}
    />
  );
}

export function TextArea({ className, ...rest }: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea {...rest} className={`mf-mon-input mf-mon-area${className ? ` ${className}` : ""}`} />;
}

export function Select({ children, className, ...rest }: React.SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <span className="mf-mon-select">
      <select {...rest} className={`mf-mon-input${className ? ` ${className}` : ""}`}>
        {children}
      </select>
      <IconChevron size={14} className="mf-mon-select__caret" />
    </span>
  );
}

/** A native checkbox with its label beside it and an optional hint under it. */
export function Check({
  checked,
  onChange,
  label,
  hint,
  disabled,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: ReactNode;
  hint?: ReactNode;
  disabled?: boolean;
  id: string;
}) {
  return (
    <div className="mf-mon-check">
      <input
        id={id}
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
      <label htmlFor={id}>
        {label}
        {hint ? <span className="mf-mon-check__hint">{hint}</span> : null}
      </label>
    </div>
  );
}

/* ── one quiet line saying what an action did ────────────────────────────── */

export function Say({ tone, children }: { tone?: "good" | "warn"; children: ReactNode }) {
  return (
    <span className="mf-mon-say" data-tone={tone} role={tone === "warn" ? "alert" : undefined}>
      {children}
    </span>
  );
}

/* ── an in-place confirmation ────────────────────────────────────────────── */

export function ConfirmRow({
  children,
  confirm,
  busy,
  onConfirm,
  onCancel,
}: {
  children: ReactNode;
  confirm: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="mf-mon-confirm" role="group" aria-label="Confirm">
      <p className="mf-mon-confirm__text">{children}</p>
      <div className="mf-mon-confirm__acts">
        <Button tone="danger" size="sm" disabled={busy} onClick={onConfirm}>
          {busy ? "Deleting…" : confirm}
        </Button>
        <Button size="sm" disabled={busy} onClick={onCancel}>
          Keep it
        </Button>
      </div>
    </div>
  );
}

/* ── the result of one check run on request ──────────────────────────────── */

/**
 * What a "Test now" found. The check stores nothing and changes no state, so
 * this block says only what that one request saw, and where it was sent from.
 */
export function CheckOutcome({ result }: { result: CheckResult }) {
  return (
    <div className="mf-mon-outcome" data-ok={result.ok} role="status">
      <div className="mf-mon-outcome__head">
        <PassMark ok={result.ok} />
        <span className="mf-data">
          {result.status_code != null ? `HTTP ${result.status_code}` : "No response"} in {ms(result.latency_ms)}
        </span>
      </div>
      {result.error ? <p className="mf-mon-outcome__error">{result.error}</p> : null}
      {result.ssl_days_left != null ? (
        <p className="mf-mon-outcome__line">Certificate expires in {daysText(result.ssl_days_left)}.</p>
      ) : null}
      <p className="mf-mon-quiet">
        Checked from: {result.region}, {when(result.checked_at)}
      </p>
    </div>
  );
}
