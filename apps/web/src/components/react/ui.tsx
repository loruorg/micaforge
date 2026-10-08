/**
 * Micaforge's shared controls and containers for the dashboard.
 *
 * Two rules govern the whole file:
 *
 *   1. A row of metrics is one strip divided by hairlines, not a grid of
 *      separate cards, so the figures line up on one baseline.
 *   2. Nothing glows or bounces. State changes by tone and fill; depth comes
 *      only from the soft shadow tokens.
 */
import type { ReactNode } from "react";
import { forwardRef, useEffect, useRef, useState } from "react";
import { IconCheck, IconChevron, IconClose, IconCopy } from "./icons";

/* ── panel ───────────────────────────────────────────────────────────────── */

export interface PanelProps {
  children: ReactNode;
  /**
   * Accepted so that older call sites still compile. The single light theme
   * draws no ruling and no perforated gutter, so these two flags change nothing.
   */
  ruled?: boolean;
  feed?: boolean;
  className?: string;
  as?: "section" | "div" | "article";
}

export function Panel({ children, className, as: Tag = "section" }: PanelProps) {
  return (
    <Tag className={className ? `mf-panel ${className}` : "mf-panel"}>
      {children}
    </Tag>
  );
}

/**
 * A panel's masthead. The title is the heading; there is deliberately no small
 * label above it, here or anywhere in this product.
 */
export function PanelHead({
  title,
  note,
  actions,
}: {
  title: ReactNode;
  note?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="mf-panel__head">
      <div className="mf-panel__head-text">
        <h2 className="mf-panel__title">{title}</h2>
        {note ? <p className="mf-panel__note">{note}</p> : null}
      </div>
      {actions ? <div className="mf-panel__actions">{actions}</div> : null}
    </div>
  );
}

/* ── readout strip ───────────────────────────────────────────────────────── */

export interface Channel {
  key: string;
  label: string;
  value: string;
  /** Signed change against the comparison period, as a fraction. */
  delta?: number | null;
  /** For metrics where a fall is the good outcome (bounce rate, LCP). */
  inverse?: boolean;
  note?: string;
  active?: boolean;
  onSelect?: () => void;
}

/**
 * The metric strip. One continuous strip divided by hairlines. Every figure
 * sits on the same baseline across every metric: the row cannot go ragged
 * because a label ran long, since the label row has a reserved height.
 */
export function Readout({ channels, dense }: { channels: Channel[]; dense?: boolean }) {
  return (
    <div className={`mf-readout${dense ? " mf-readout--dense" : ""}`} role="list">
      {channels.map((c) => {
        const Tag = c.onSelect ? "button" : "div";
        // A change that rounds to 0% is flat, not a loss: it gets no colour.
        const flat = c.delta == null || Math.abs(c.delta * 100) < 0.05;
        const good = flat ? null : c.inverse ? (c.delta as number) < 0 : (c.delta as number) > 0;
        return (
          <Tag
            key={c.key}
            role="listitem"
            type={c.onSelect ? "button" : undefined}
            className={`mf-readout__chan${c.active ? " is-active" : ""}${c.onSelect ? " is-selectable" : ""}`}
            onClick={c.onSelect}
            aria-pressed={c.onSelect ? !!c.active : undefined}
          >
            <span className="mf-readout__label">{c.label}</span>
            {/* Keyed by the value, so a new figure animates in once when it
                lands and a re-render that changes nothing leaves it alone. */}
            <span key={String(c.value)} className="mf-readout__value mf-data mf-ink-write">
              {c.value}
            </span>
            <span className="mf-readout__foot">
              {c.delta != null && Number.isFinite(c.delta) ? (
                <span
                  className="mf-readout__delta mf-data"
                  data-tone={good == null ? "flat" : good ? "good" : "warn"}
                >
                  {/* Direction is carried by the caret AND the sign, so the
                      meaning survives with no colour at all. */}
                  <span aria-hidden="true">{c.delta > 0 ? "▲" : c.delta < 0 ? "▼" : "—"}</span>
                  {Math.abs(c.delta * 100) < 0.05
                    ? "0%"
                    : `${Math.abs(c.delta * 100).toFixed(Math.abs(c.delta) < 0.1 ? 1 : 0)}%`}
                </span>
              ) : null}
              {c.note ? <span className="mf-readout__note">{c.note}</span> : null}
            </span>
          </Tag>
        );
      })}
    </div>
  );
}

/* ── controls ────────────────────────────────────────────────────────────── */

type ButtonTone = "key" | "primary" | "quiet" | "danger";

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  tone?: ButtonTone;
  size?: "sm" | "md";
  icon?: ReactNode;
}

/**
 * A button. `key` is the everyday control (white, hairline edge), `primary` is
 * the one committed action on a screen, `quiet` is a text control and `danger`
 * is a destructive action. Two `key`s side by side is the normal pairing here.
 */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { tone = "key", size = "md", icon, children, className, ...rest },
  ref,
) {
  return (
    <button
      ref={ref}
      type={rest.type ?? "button"}
      className={`mf-btn mf-btn--${tone} mf-btn--${size}${className ? ` ${className}` : ""}`}
      {...rest}
    >
      {icon ? <span className="mf-btn__icon">{icon}</span> : null}
      {children ? <span className="mf-btn__label">{children}</span> : null}
    </button>
  );
});

/**
 * The audience selector: people, AI crawlers, or both together. Each option
 * carries the line style its series is drawn with, so the choice does not rest
 * on colour alone.
 */
export function AudienceSwitch({
  value,
  onChange,
}: {
  value: "human" | "agent" | "all";
  onChange: (v: "human" | "agent" | "all") => void;
}) {
  const opts: { v: "human" | "agent" | "all"; label: string; pen: string }[] = [
    { v: "human", label: "People", pen: "written" },
    { v: "agent", label: "AI crawlers", pen: "stepped" },
    { v: "all", label: "All", pen: "both" },
  ];
  return (
    <div className="mf-audience" role="group" aria-label="Audience">
      {opts.map((o) => (
        <button
          key={o.v}
          type="button"
          className="mf-audience__key"
          aria-pressed={value === o.v}
          onClick={() => onChange(o.v)}
        >
          <span className="mf-audience__pen" data-pen={o.pen} aria-hidden="true" />
          {o.label}
        </button>
      ))}
    </div>
  );
}

const RANGES: { v: string; label: string }[] = [
  { v: "24h", label: "24 hours" },
  { v: "7d", label: "7 days" },
  { v: "30d", label: "30 days" },
  { v: "90d", label: "90 days" },
  { v: "12mo", label: "12 months" },
  { v: "all", label: "All" },
];

/** The date range control: one segmented row of preset periods. */
export function RangePicker({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div className="mf-caliper">
      <div className="mf-caliper__jaws" role="group" aria-label="Date range">
        {RANGES.map((r) => (
          <button
            key={r.v}
            type="button"
            className="mf-caliper__stop"
            aria-pressed={value === r.v}
            onClick={() => onChange(r.v)}
          >
            {r.label}
          </button>
        ))}
      </div>
    </div>
  );
}

/* ── table ───────────────────────────────────────────────────────────────── */

export interface Column<T> {
  key: string;
  head: ReactNode;
  /** Right-aligned numeric columns are the default for measured values. */
  numeric?: boolean;
  width?: string;
  render: (row: T, index: number) => ReactNode;
  /** 0..1; draws the row's share as a light bar behind the cell. */
  share?: (row: T) => number | undefined;
}

/**
 * The data table. A row's share is drawn as a light bar behind the label,
 * running across the cell, rather than as a separate progress bar beside it.
 */
export function Sheet<T>({
  rows,
  columns,
  getKey,
  empty,
  onRowClick,
  loading,
}: {
  rows: T[];
  columns: Column<T>[];
  getKey: (row: T, i: number) => string;
  empty?: ReactNode;
  onRowClick?: (row: T) => void;
  loading?: boolean;
}) {
  if (loading) return <SheetSkeleton columns={columns.length} />;
  if (!rows.length) return <>{empty ?? <Empty />}</>;

  return (
    <div className="mf-sheet-wrap">
      <table className="mf-sheet">
        <thead>
          <tr>
            {columns.map((c) => (
              <th
                key={c.key}
                scope="col"
                style={c.width ? { width: c.width } : undefined}
                className={c.numeric ? "is-num" : undefined}
              >
                {c.head}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr
              key={getKey(row, i)}
              onClick={onRowClick ? () => onRowClick(row) : undefined}
              className={onRowClick ? "is-clickable" : undefined}
              tabIndex={onRowClick ? 0 : undefined}
              onKeyDown={
                onRowClick
                  ? (e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        onRowClick(row);
                      }
                    }
                  : undefined
              }
            >
              {columns.map((c) => {
                const share = c.share?.(row);
                return (
                  <td key={c.key} className={c.numeric ? "is-num" : undefined}>
                    {share != null ? (
                      <span
                        className="mf-sheet__ink"
                        style={{ width: `${Math.max(0, Math.min(1, share)) * 100}%` }}
                        aria-hidden="true"
                      />
                    ) : null}
                    <span className="mf-sheet__cell">{c.render(row, i)}</span>
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function SheetSkeleton({ columns }: { columns: number }) {
  return (
    <div className="mf-sheet-wrap" aria-busy="true" aria-live="polite">
      <span className="mf-sr">Loading…</span>
      <table className="mf-sheet mf-sheet--waiting">
        <tbody>
          {Array.from({ length: 6 }).map((_, r) => (
            <tr key={r}>
              {Array.from({ length: columns }).map((__, c) => (
                <td key={c}>
                  <span className="mf-wait" style={{ width: `${30 + ((r * 7 + c * 13) % 55)}%` }} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/* ── states ──────────────────────────────────────────────────────────────── */

export function Empty({
  title = "No data for this period",
  body = "Nothing matched this date range and these filters. Choose a longer range or clear a filter.",
  action,
}: {
  title?: string;
  body?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="mf-empty">
      {/* A flat, empty chart: the plain picture of no data. */}
      <svg className="mf-empty__paper" viewBox="0 0 120 48" aria-hidden="true">
        <rect x="0.5" y="0.5" width="119" height="47" rx="7.5" fill="var(--mf-ground-2)" stroke="var(--mf-rule)" />
        <path d="M14 14h92M14 24h92" stroke="var(--mf-rule-minor)" strokeWidth="1" />
        <path d="M14 34h92" stroke="var(--mf-rule-major)" strokeWidth="1" />
        <path d="M14 33.5h92" stroke="var(--mf-ink-3)" strokeWidth="1.5" strokeDasharray="3 4" strokeLinecap="round" />
      </svg>
      <h3 className="mf-empty__title">{title}</h3>
      {body ? <p className="mf-empty__body">{body}</p> : null}
      {action ? <div className="mf-empty__action">{action}</div> : null}
    </div>
  );
}

export function ErrorState({
  title = "Could not load this data",
  detail = "The request to the Micaforge server failed with no reason given. Try again. If it fails again, check that the server is running and reachable.",
  onRetry,
}: {
  title?: string;
  detail?: string;
  onRetry?: () => void;
}) {
  return (
    <div className="mf-empty mf-empty--error" role="alert">
      <h3 className="mf-empty__title">{title}</h3>
      {detail ? <p className="mf-empty__body">{detail}</p> : null}
      {onRetry ? (
        <div className="mf-empty__action">
          <Button onClick={onRetry}>Try again</Button>
        </div>
      ) : null}
    </div>
  );
}

/**
 * The Citation Gap verdict. Four states, each with its own drawn mark so the
 * meaning does not rest on colour alone.
 */
export function Verdict({ value }: { value: "extracted" | "compounding" | "invisible" | "quiet" }) {
  const copy = {
    extracted: { label: "Extracted", note: "crawled hard, little or nothing came back" },
    compounding: { label: "Compounding", note: "crawled, and sending visitors back" },
    invisible: { label: "Invisible", note: "sending visitors back with almost no crawling" },
    quiet: { label: "Quiet", note: "too little crawling or citing to judge yet" },
  }[value];

  return (
    <span className="mf-verdict" data-verdict={value} title={copy.note}>
      <svg viewBox="0 0 14 14" width="12" height="12" aria-hidden="true">
        {value === "extracted" && (
          <>
            <path d="M1 3h12" stroke="currentColor" strokeWidth="1.6" />
            <path d="M1 11h4" stroke="currentColor" strokeWidth="1.6" />
          </>
        )}
        {value === "compounding" && (
          <>
            <path d="M1 3h12" stroke="currentColor" strokeWidth="1.6" />
            <path d="M1 11h11" stroke="currentColor" strokeWidth="1.6" />
          </>
        )}
        {value === "invisible" && (
          <>
            <path d="M1 3h4" stroke="currentColor" strokeWidth="1.6" />
            <path d="M1 11h11" stroke="currentColor" strokeWidth="1.6" />
          </>
        )}
        {value === "quiet" && (
          <>
            <path d="M1 3h3" stroke="currentColor" strokeWidth="1.6" opacity="0.6" />
            <path d="M1 11h3" stroke="currentColor" strokeWidth="1.6" opacity="0.6" />
          </>
        )}
      </svg>
      {copy.label}
    </span>
  );
}

/** A verified-agent mark. "Unverified" is a real, honest third state. */
export function VerifiedMark({ state }: { state: "verified" | "unverified" | "failed" }) {
  const label =
    state === "verified"
      ? "Verified against the operator's published address ranges"
      : state === "failed"
        ? "Claimed this agent, but the address did not match the operator's ranges"
        : "No published ranges to check against";
  return (
    <span className="mf-verified" data-state={state} title={label}>
      {state === "verified" ? <IconCheck size={12} /> : state === "failed" ? <IconClose size={12} /> : "—"}
      <span className="mf-sr">{label}</span>
    </span>
  );
}

/* ── small utilities ─────────────────────────────────────────────────────── */

export function CopyField({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false);
  const t = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(t.current), []);
  return (
    <div className="mf-copy">
      <label className="mf-copy__label">{label}</label>
      <div className="mf-copy__row">
        <code className="mf-copy__value">{value}</code>
        <Button
          size="sm"
          icon={copied ? <IconCheck size={14} /> : <IconCopy size={14} />}
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(value);
              setCopied(true);
              t.current = window.setTimeout(() => setCopied(false), 1600);
            } catch {
              /* clipboard blocked: the value is selectable, which is the fallback */
            }
          }}
        >
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
    </div>
  );
}

export function LivePulse({ label }: { label: string }) {
  return (
    <span className="mf-pulse">
      <span className="mf-pulse__dot" aria-hidden="true" />
      {label}
    </span>
  );
}

export function Disclosure({
  summary,
  children,
  open: initial = false,
}: {
  summary: ReactNode;
  children: ReactNode;
  open?: boolean;
}) {
  const [open, setOpen] = useState(initial);
  return (
    <div className="mf-disc" data-open={open}>
      <button
        type="button"
        className="mf-disc__summary"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <IconChevron size={14} className="mf-disc__caret" />
        {summary}
      </button>
      {open ? <div className="mf-disc__body">{children}</div> : null}
    </div>
  );
}

/** Marks illustrative figures as illustrative. Never optional where it applies. */
export function DemoNotice({ children }: { children?: ReactNode }) {
  return (
    <p className="mf-demo-notice">
      {children ?? "Demo data. These figures are examples, not real traffic."}
    </p>
  );
}
