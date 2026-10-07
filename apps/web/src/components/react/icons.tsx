/**
 * Micaforge iconography.
 *
 * Drawn for this product, not pulled from a pack. One grammar throughout:
 *   - 16x16 grid, 1.4 stroke, square caps, mitred joins, currentColor
 *   - orthogonal and 45-degree geometry only, the way panel engraving is cut
 *   - every glyph carries exactly one filled 1.4-square "registration tick",
 *     the fiducial mark an instrument panel uses to say where zero is
 *
 * The tick is the invented detail that makes the set ours. If a glyph you add
 * later has no honest place for it, the glyph is wrong for this set.
 */
import type { SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement> & { size?: number; title?: string };

function Glyph({ size = 16, title, children, ...rest }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="square"
      strokeLinejoin="miter"
      aria-hidden={title ? undefined : true}
      role={title ? "img" : undefined}
      focusable="false"
      {...rest}
    >
      {title ? <title>{title}</title> : null}
      {children}
    </svg>
  );
}

/** The registration tick. One per glyph, never more. */
const Tick = ({ x, y }: { x: number; y: number }) => (
  <rect x={x} y={y} width={1.4} height={1.4} fill="currentColor" stroke="none" />
);

/* ── navigation ─────────────────────────────────────────────────────────── */

/** The record: paper edge plus a written trace. */
export const IconRecord = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2 3v10" />
    <path d="M2.8 10.5 5 10.5 6.4 6 8 12 9.6 8 11 9.5 14 9.5" />
    <Tick x={1.3} y={2.3} />
  </Glyph>
);

/** An agent: a stepped fetch pattern arriving from off-sheet. */
export const IconAgent = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 12.5h3V9h3V5.5h3V2.5h3" />
    <path d="M11.5 5.5h3v3" />
    <Tick x={0.8} y={11.8} />
  </Glyph>
);

/** The gap: two traces with the debt hatched between them. */
export const IconGap = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 4.5 5 4.5 8 3 11 4.5 14.5 4.5" />
    <path d="M1.5 12 5 12 8 12.5 11 11.5 14.5 11.5" />
    <path d="M4 6.4 4 10" opacity={0.55} />
    <path d="M7 5.4 7 10.9" opacity={0.55} />
    <path d="M10 6 10 10.2" opacity={0.55} />
    <Tick x={13.1} y={7.6} />
  </Glyph>
);

/** Policy: a sheet with a barred rule across it. */
export const IconPolicy = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.5 1.5h6l3 3v10h-9z" />
    <path d="M9.5 1.5v3h3" />
    <path d="M5.5 10.5h5" />
    <Tick x={5.3} y={7.3} />
  </Glyph>
);

export const IconPages = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2.5 2.5h8v11h-8z" />
    <path d="M5 5.5h3.5M5 8h3.5M5 10.5h2" />
    <path d="M12 4.5v9h-1.5" />
    <Tick x={12.6} y={7.3} />
  </Glyph>
);

/** Sources: three inbound channels converging on one entry. */
export const IconSources = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 3.5h4l3 4.5h6" />
    <path d="M1.5 12.5h4l2-3" />
    <path d="M12 6.2 14.8 8 12 9.8" />
    <Tick x={0.8} y={7.3} />
  </Glyph>
);

/** Locations: a ruled sphere, not a clip-art globe. */
export const IconLocations = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={8} cy={8} r={6.2} />
    <path d="M1.9 8h12.2" />
    <path d="M8 1.8c2 2 2 10.4 0 12.4-2-2-2-10.4 0-12.4z" />
    <Tick x={7.3} y={7.3} />
  </Glyph>
);

export const IconDevices = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 3.5h8v7h-8z" />
    <path d="M3.5 13.5h4" />
    <path d="M11.5 6.5h3v7h-3z" />
    <Tick x={12.3} y={11.4} />
  </Glyph>
);

/** Events: discrete marks on a time rule. */
export const IconEvents = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 11.5h13" />
    <path d="M4 11.5V7M8 11.5V3.5M12 11.5V8.5" />
    <Tick x={7.3} y={2} />
  </Glyph>
);

/** A goal: the target line the trace has to cross. */
export const IconGoals = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 12 4.5 9 7.5 10.5 14 4" />
    <path d="M14 4h-3.5M14 4v3.5" />
    <path d="M1.5 6.5h9.5" strokeDasharray="1.6 1.8" opacity={0.6} />
    <Tick x={0.8} y={5.8} />
  </Glyph>
);

export const IconFunnel = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h13l-4.5 5.5v6l-4-2v-4z" />
    <Tick x={7.3} y={7} />
  </Glyph>
);

/** Journeys: a branching path, drawn as a switchyard. */
export const IconJourneys = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 8h3.5l2.5-4.5h7" />
    <path d="M5 8l2.5 4.5h7" />
    <Tick x={0.8} y={7.3} />
  </Glyph>
);

/** Retention: a cohort grid thinning to the right. */
export const IconRetention = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h13v11h-13z" />
    <path d="M1.5 6.2h13M1.5 9.9h13" />
    <path d="M5.8 2.5v11M10.1 2.5v11" />
    <Tick x={2.6} y={3.7} />
  </Glyph>
);

/** Performance: a gain dial with its needle. */
export const IconPerformance = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2 12a6.6 6.6 0 1 1 12 0" />
    <path d="M8 12 11.2 7.4" />
    <Tick x={7.3} y={11.3} />
  </Glyph>
);

/** An error: the pen jumping its baseline. */
export const IconErrors = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 9.5h3l1.5-6 2.5 9 2-4.5h4" />
    <Tick x={13.6} y={2.4} />
  </Glyph>
);

export const IconSessions = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 4.5h13M1.5 8h9M1.5 11.5h11" />
    <Tick x={12.6} y={7.3} />
  </Glyph>
);

export const IconVisitors = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 8.2a2.9 2.9 0 1 0 0-5.8 2.9 2.9 0 0 0 0 5.8z" />
    <path d="M2.5 14c0-2.7 2.5-4.4 5.5-4.4s5.5 1.7 5.5 4.4" />
    <Tick x={7.3} y={4.4} />
  </Glyph>
);

/** Replay: the drum, wound back. */
export const IconReplay = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M14 8a6 6 0 1 1-2-4.5" />
    <path d="M14 1.5V5h-3.5" />
    <Tick x={7.3} y={7.3} />
  </Glyph>
);

export const IconFlags = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.5 14.5V2" />
    <path d="M3.5 2.5h9l-2 3 2 3h-9" />
    <Tick x={2.8} y={13.1} />
  </Glyph>
);

/** Experiments: two channels split from one source. */
export const IconExperiments = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 8h4l2-3.5h7" />
    <path d="M5.5 8l2 3.5h7" />
    <path d="M12 3 14.8 4.5 12 6" />
    <path d="M12 10 14.8 11.5 12 13" />
    <Tick x={0.8} y={7.3} />
  </Glyph>
);

export const IconDashboards = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h6v5h-6zM10 2.5h4.5v3h-4.5zM1.5 10h6v3.5h-6zM10 8h4.5v5.5h-4.5z" />
    <Tick x={11.6} y={3.3} />
  </Glyph>
);

/** Query: a console prompt. */
export const IconQuery = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h13v11h-13z" />
    <path d="M4 6.5 6 8.5 4 10.5" />
    <path d="M8 10.5h4" />
    <Tick x={2.8} y={3.8} />
  </Glyph>
);

/** Settings: the gain screw, not a cog. */
export const IconSettings = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2.5 4.5h11M2.5 11.5h11" />
    <path d="M6 2.5v4M10.5 9.5v4" />
    <Tick x={5.3} y={3.8} />
  </Glyph>
);

/* ── controls ───────────────────────────────────────────────────────────── */

/** The caliper: two jaws with the measured interval between them. */
export const IconCaliper = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.5 2v12M12.5 2v12" />
    <path d="M3.5 8h9" />
    <Tick x={7.3} y={7.3} />
  </Glyph>
);

export const IconSearch = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={7} cy={7} r={4.6} />
    <path d="M10.4 10.4 14.2 14.2" />
    <Tick x={6.3} y={2} />
  </Glyph>
);

export const IconFilter = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 3.5h13M4 8h8M6.5 12.5h3" />
    <Tick x={0.8} y={2.8} />
  </Glyph>
);

export const IconDownload = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 1.5v8.5" />
    <path d="M4.5 7 8 10.5 11.5 7" />
    <path d="M1.5 13.5h13" />
    <Tick x={7.3} y={0.8} />
  </Glyph>
);

export const IconCopy = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M5.5 5.5h9v9h-9z" />
    <path d="M10.5 5.5v-4h-9v9h4" />
    <Tick x={2.3} y={2.3} />
  </Glyph>
);

export const IconClose = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.2 3.2 12.8 12.8M12.8 3.2 3.2 12.8" />
    <Tick x={7.3} y={7.3} />
  </Glyph>
);

export const IconChevron = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M4 6 8 10 12 6" />
    <Tick x={7.3} y={2.3} />
  </Glyph>
);

export const IconCheck = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2.5 8.5 6 12 13.5 4" />
    <Tick x={1.8} y={7.8} />
  </Glyph>
);

/**
 * Partly. The pen wrote the first half of the trace and stopped: the capability
 * exists, but not the whole of it. Used in the comparison sheets, where "yes"
 * and "no" are both too strong to be honest.
 */
export const IconPartial = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 8h5.5" />
    <path d="M9 8h5.5" strokeDasharray="1.6 1.9" opacity={0.7} />
    <Tick x={7.3} y={7.3} />
  </Glyph>
);

/**
 * A machine at the door: a stepped fetch arriving at a sheet edge. Distinct
 * from IconAgent, which is the channel; this is one fetch landing on one page.
 */
export const IconFetch = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 11.5h3.5V8h3.5V4.5h3" />
    <path d="M13.5 2v12" />
    <Tick x={0.8} y={10.8} />
  </Glyph>
);

export const IconPlus = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 2.5v11M2.5 8h11" />
    <Tick x={7.3} y={7.3} />
  </Glyph>
);

/**
 * The outward arrow. Deliberately diagonal: the horizontal right-arrow is the
 * component-kit default and this product does not use it anywhere.
 */
export const IconOut = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M4 12 12 4" />
    <path d="M5.5 4H12v6.5" />
    <Tick x={3.3} y={11.3} />
  </Glyph>
);

/** Live: the pen tip resting on paper. */
export const IconPen = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 1.5 10.5 9H5.5z" />
    <path d="M1.5 13.5h13" />
    <Tick x={7.3} y={9.6} />
  </Glyph>
);

export const IconClock = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={8} cy={8} r={6.2} />
    <path d="M8 4.2V8l2.8 1.8" />
    <Tick x={7.3} y={1} />
  </Glyph>
);

export const IconAlert = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 1.8 15 13.5H1z" />
    <path d="M8 6v3.4" />
    <Tick x={7.3} y={10.8} />
  </Glyph>
);

export const IconInfo = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={8} cy={8} r={6.2} />
    <path d="M8 7.4v4" />
    <Tick x={7.3} y={4.2} />
  </Glyph>
);

/** Two record media: smoked drum and chart paper. Never a sun and a moon. */
export const IconSmoked = (p: IconProps) => (
  <Glyph {...p}>
    <rect x={1.7} y={2.7} width={12.6} height={10.6} fill="currentColor" stroke="none" opacity={0.9} />
    <path d="M3.5 10.5 6 6.5 8.5 11 11 7.5 12.8 9.5" stroke="var(--mf-ground)" />
    <Tick x={7.3} y={1.2} />
  </Glyph>
);

export const IconPaper = (p: IconProps) => (
  <Glyph {...p}>
    <rect x={1.7} y={2.7} width={12.6} height={10.6} />
    <path d="M3.5 10.5 6 6.5 8.5 11 11 7.5 12.8 9.5" />
    <Tick x={7.3} y={1.2} />
  </Glyph>
);

/**
 * The brand mark. It is the product, drawn: an upper stepped trace (what the
 * machines took), a lower written trace (what came back), and the debt between
 * them ruled out. There is deliberately no tile, chip or rounded square behind
 * it. The mark stands on the surface as itself.
 */
export function Mark({ size = 24, ...rest }: SVGProps<SVGSVGElement> & { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
      focusable="false"
      {...rest}
    >
      {/* machine pen: stepped, because a crawler fetches in discrete jumps */}
      <path
        d="M1.5 7.5h4v-3h4v3h5v-3h4"
        stroke="var(--mf-shellac, currentColor)"
        strokeWidth={1.7}
        strokeLinecap="square"
      />
      {/* the debt between the pens */}
      <path d="M4 9.4v5.2M9 9.4v5.2M14 9.4v5.2M19 9.4v5.2" stroke="currentColor" strokeWidth={1} opacity={0.34} />
      {/* human pen: written, because a reader arrives continuously */}
      <path
        d="M1.5 16.5c3 0 3.4-3 5.5-3s2.6 4 5 4 3.6-2.6 5-2.6h5.5"
        stroke="currentColor"
        strokeWidth={1.7}
        strokeLinecap="square"
        fill="none"
      />
      {/* the registration tick, same fiducial as every glyph */}
      <rect x={21.4} y={3.3} width={1.6} height={1.6} fill="var(--mf-shellac, currentColor)" />
    </svg>
  );
}
