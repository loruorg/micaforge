/**
 * The frame every unauthenticated screen is written on.
 *
 * This is the first sight anyone gets of Micaforge, so it is not a centred box
 * on an empty field. The page IS a sheet of the instrument's chart paper: the
 * sprocket perforations run down the left gutter, a fragment of a real two-pen
 * record is written across the top of the sheet, and the form is pinned onto
 * the sheet below it as a log slip. Someone who never signs in still leaves
 * knowing what the product measures.
 *
 * The trace is a SPECIMEN: a deterministic figure computed from two closed
 * formulae, carrying no axis and no figures, and stamped as a specimen. It is
 * drawn in full on first paint. Nothing here fades, slides or waits on an
 * observer; the only state that changes is what the reader types.
 *
 * The form primitives every entry screen shares live here too, because they
 * belong to the same slip.
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

/* ── the specimen trace ──────────────────────────────────────────────────── */

const SAMPLES = 44;
const BAND_W = 1200;
const BAND_H = 150;
const MID = BAND_H / 2;
/* Each pen has its own gain, the way a two-pen recorder gives each channel its
   own gain knob: at this size a shared scale flattens the reader pen onto the
   centre rule and the sheet reads as "no readers" rather than "far fewer". */
const GAIN_MACHINE = 60;
const GAIN_HUMAN = 56;

/**
 * Two closed formulae, no randomness, so the specimen is identical on every
 * build and in every session. The shapes are the argument the product makes:
 * machines arrive in bursts around a publish and then settle into a re-crawl
 * floor that keeps climbing; readers follow a weekly rhythm and stay small.
 */
const MACHINE = Array.from({ length: SAMPLES }, (_, i) => {
  const floor = 0.17 + 0.0042 * i;
  const burst1 = 0.7 * Math.exp(-Math.abs(i - 9) / 2.1);
  const burst2 = 0.58 * Math.exp(-Math.abs(i - 28) / 2.6);
  const chatter = 0.045 * Math.abs(Math.sin(i * 1.73));
  return Math.min(0.98, floor + burst1 + burst2 + chatter);
});

const HUMAN = Array.from({ length: SAMPLES }, (_, i) => {
  const week = 0.082 * Math.sin((i / 7) * Math.PI * 2);
  const daily = 0.031 * Math.sin(i * 1.31 + 0.7);
  const drift = 0.026 * Math.sin(i * 0.43 + 2.1);
  // The tail lifts: the answer engines start sending readers back late in the
  // window, which is the one thing on this sheet worth noticing.
  const lift = i > 29 ? (0.06 * (i - 29)) / 14 : 0;
  return Math.max(0.05, 0.148 + week + daily + drift + lift + 0.0016 * i);
});

const HUMAN_PEAK = Math.max(...HUMAN);
const xAt = (i: number) => (i / (SAMPLES - 1)) * BAND_W;

/** Catmull-Rom through the points: a written pen curves between samples. */
function written(pts: [number, number][]): string {
  if (!pts.length) return "";
  let d = `M${pts[0][0].toFixed(2)},${pts[0][1].toFixed(2)}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i === 0 ? 0 : i - 1];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[i + 2] ?? p2;
    const c1x = p1[0] + (p2[0] - p0[0]) / 6;
    const c1y = p1[1] + (p2[1] - p0[1]) / 6;
    const c2x = p2[0] - (p3[0] - p1[0]) / 6;
    const c2y = p2[1] - (p3[1] - p1[1]) / 6;
    d += `C${c1x.toFixed(2)},${c1y.toFixed(2)} ${c2x.toFixed(2)},${c2y.toFixed(2)} ${p2[0].toFixed(2)},${p2[1].toFixed(2)}`;
  }
  return d;
}

/** A crawler fetches in discrete jumps, so its pen steps rather than curves. */
function stepped(pts: [number, number][]): string {
  if (!pts.length) return "";
  let d = `M${pts[0][0].toFixed(2)},${pts[0][1].toFixed(2)}`;
  for (let i = 1; i < pts.length; i++) {
    const mid = ((pts[i - 1][0] + pts[i][0]) / 2).toFixed(2);
    d += `L${mid},${pts[i - 1][1].toFixed(2)}L${mid},${pts[i][1].toFixed(2)}L${pts[i][0].toFixed(2)},${pts[i][1].toFixed(2)}`;
  }
  return d;
}

const MACHINE_PTS = MACHINE.map((v, i) => [xAt(i), MID - v * GAIN_MACHINE] as [number, number]);
const HUMAN_PTS = HUMAN.map((v, i) => [xAt(i), MID + (v / HUMAN_PEAK) * GAIN_HUMAN] as [number, number]);

const MACHINE_LINE = stepped(MACHINE_PTS);
const HUMAN_LINE = written(HUMAN_PTS);
const MACHINE_AREA = `${MACHINE_LINE}L${BAND_W},${MID}L0,${MID}Z`;
const HUMAN_AREA = `${HUMAN_LINE}L${BAND_W},${MID}L0,${MID}Z`;

/** The debt between the pens, ruled out exactly as the brand mark rules it. */
const DEBT = MACHINE_PTS.map((m, i) => ({ x: m[0], top: m[1] + 5, bottom: HUMAN_PTS[i][1] - 5 })).filter(
  (d, i) => i % 3 === 1 && d.bottom - d.top > 8,
);

function SpecimenTrace() {
  return (
    <svg
      className="mf-entry__trace"
      viewBox={`0 0 ${BAND_W} ${BAND_H}`}
      preserveAspectRatio="none"
      aria-hidden="true"
      focusable="false"
    >
      {/* the paper's own ruling, clipped to this band and nowhere else */}
      <g stroke="var(--mf-rule-minor)" strokeWidth={1} vectorEffect="non-scaling-stroke" shapeRendering="crispEdges">
        {[25, 50, 100, 125].map((y) => (
          <line key={y} x1={0} x2={BAND_W} y1={y} y2={y} />
        ))}
        {Array.from({ length: 19 }, (_, i) => (i + 1) * 60).map((x) => (
          <line key={x} x1={x} x2={x} y1={0} y2={BAND_H} />
        ))}
      </g>
      {/* fiducial notches: the recorder's interval marks along the bottom edge */}
      <g stroke="var(--mf-rule-strong)" strokeWidth={1} vectorEffect="non-scaling-stroke" shapeRendering="crispEdges">
        {MACHINE.map((_, i) => (i % 6 === 0 ? <line key={i} x1={xAt(i)} x2={xAt(i)} y1={BAND_H - 7} y2={BAND_H} /> : null))}
      </g>

      <path d={MACHINE_AREA} fill="var(--mf-ai-fill)" />
      <path d={HUMAN_AREA} fill="var(--mf-people-fill)" />

      {/* the zero rule the two pens are opposed about, drawn over their fills */}
      <line
        x1={0}
        x2={BAND_W}
        y1={MID}
        y2={MID}
        stroke="var(--mf-rule-major)"
        strokeWidth={1}
        vectorEffect="non-scaling-stroke"
        shapeRendering="crispEdges"
      />

      <g stroke="var(--mf-ink-3)" strokeWidth={1} opacity={0.34} vectorEffect="non-scaling-stroke">
        {DEBT.map((d) => (
          <line key={d.x} x1={d.x} x2={d.x} y1={d.top} y2={d.bottom} />
        ))}
      </g>

      {/* Each pen is drawn twice: the ridge of soot the stylus displaced, then
          the bright scratch over it. That ridge is the whole depth treatment. */}
      <g fill="none" strokeLinecap="butt" strokeLinejoin="round" vectorEffect="non-scaling-stroke">
        <path d={MACHINE_LINE} stroke="var(--mf-well)" strokeWidth={3} opacity={0.85} transform="translate(0,1.2)" />
        <path d={MACHINE_LINE} stroke="var(--mf-ai)" strokeWidth={1.8} />
        <path d={HUMAN_LINE} stroke="var(--mf-well)" strokeWidth={3} opacity={0.85} transform="translate(0,1.2)" />
        <path d={HUMAN_LINE} stroke="var(--mf-people)" strokeWidth={1.8} />
      </g>
    </svg>
  );
}

/* ── the frame ───────────────────────────────────────────────────────────── */

export interface AuthFrameProps {
  /** The slip's heading. Rendered as the page's h1. */
  title: ReactNode;
  note?: ReactNode;
  children: ReactNode;
  /** Replaces the standing explanation of the two pens. */
  aside?: ReactNode;
}

export function AuthFrame({ title, note, children, aside }: AuthFrameProps) {
  return (
    <div className="mf-entry">
      <div className="mf-entry__mast">
        <a className="mf-entry__brand" href="/" aria-label="Micaforge home">
          <Mark size={26} />
          <span className="mf-entry__wordmark">Micaforge</span>
        </a>
      </div>

      <div className="mf-entry__stage">
        <div className="mf-entry__sheet mf-panel mf-panel--feed">
          <div className="mf-entry__band">
            <SpecimenTrace />
          </div>

          <div className="mf-entry__legend">
            <span className="mf-entry__chan">
              <span className="mf-entry__pen" data-pen="stepped" aria-hidden="true" />
              Machines fetching
            </span>
            <span className="mf-entry__chan">
              <span className="mf-entry__pen" data-pen="written" aria-hidden="true" />
              Readers arriving from answers
            </span>
            <span className="mf-stamp mf-entry__specimen">Specimen trace, not measured traffic</span>
          </div>

          <div className="mf-entry__body">
            <div className="mf-entry__slip">
              <div className="mf-form">
                <div>
                  <h1 className="mf-form__title">{title}</h1>
                  {note ? <p className="mf-form__note">{note}</p> : null}
                </div>
                {children}
              </div>
            </div>

            <div className="mf-entry__aside">
              {aside ?? (
                <>
                  <p>
                    Two pens on one sheet. The upper trace is every AI crawler that fetched a page.
                    The lower trace is every reader who arrived from an answer engine. The ratio
                    between them is the Citation Gap, and it is the number no other analytics tool
                    holds.
                  </p>
                  <a className="mf-entry__link" href="/why-agent-analytics">
                    Why this is a different measurement
                    <IconOut size={13} />
                  </a>
                </>
              )}
            </div>
          </div>
        </div>
      </div>

      <div className="mf-entry__colophon">
        <span>AGPL-3.0</span>
        <span className="mf-entry__dot" aria-hidden="true">
          ·
        </span>
        <span>Cookieless</span>
        <span className="mf-entry__dot" aria-hidden="true">
          ·
        </span>
        <span>No raw IP is ever stored</span>
        <span className="mf-entry__dot" aria-hidden="true">
          ·
        </span>
        <a className="mf-entry__link mf-entry__link--sm" href="/self-host">
          Self-host it
        </a>
      </div>
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
}

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
