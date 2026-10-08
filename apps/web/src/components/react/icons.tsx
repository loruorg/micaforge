/**
 * Micaforge iconography.
 *
 * Drawn for this product, not pulled from a pack. One grammar throughout: a
 * 16x16 grid, a 1.4 stroke, round caps and joins, currentColor, and mostly
 * orthogonal geometry so the set stays crisp at 16px.
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
      strokeLinecap="round"
      strokeLinejoin="round"
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

/* ── navigation ─────────────────────────────────────────────────────────── */

/** The record: paper edge plus a written trace. */
export const IconRecord = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2 3v10" />
    <path d="M2.8 10.5 5 10.5 6.4 6 8 12 9.6 8 11 9.5 14 9.5" />
  </Glyph>
);

/** An agent: a stepped fetch pattern arriving from off-sheet. */
export const IconAgent = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 12.5h3V9h3V5.5h3V2.5h3" />
    <path d="M11.5 5.5h3v3" />
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
  </Glyph>
);

/** Policy: a sheet with a barred rule across it. */
export const IconPolicy = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.5 1.5h6l3 3v10h-9z" />
    <path d="M9.5 1.5v3h3" />
    <path d="M5.5 10.5h5" />
  </Glyph>
);

export const IconPages = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2.5 2.5h8v11h-8z" />
    <path d="M5 5.5h3.5M5 8h3.5M5 10.5h2" />
    <path d="M12 4.5v9h-1.5" />
  </Glyph>
);

/** Sources: three inbound channels converging on one entry. */
export const IconSources = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 3.5h4l3 4.5h6" />
    <path d="M1.5 12.5h4l2-3" />
    <path d="M12 6.2 14.8 8 12 9.8" />
  </Glyph>
);

/** Locations: a ruled sphere, not a clip-art globe. */
export const IconLocations = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={8} cy={8} r={6.2} />
    <path d="M1.9 8h12.2" />
    <path d="M8 1.8c2 2 2 10.4 0 12.4-2-2-2-10.4 0-12.4z" />
  </Glyph>
);

export const IconDevices = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 3.5h8v7h-8z" />
    <path d="M3.5 13.5h4" />
    <path d="M11.5 6.5h3v7h-3z" />
  </Glyph>
);

/** Events: discrete marks on a time rule. */
export const IconEvents = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 11.5h13" />
    <path d="M4 11.5V7M8 11.5V3.5M12 11.5V8.5" />
  </Glyph>
);

/** A goal: the target line the trace has to cross. */
export const IconGoals = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 12 4.5 9 7.5 10.5 14 4" />
    <path d="M14 4h-3.5M14 4v3.5" />
    <path d="M1.5 6.5h9.5" strokeDasharray="1.6 1.8" opacity={0.6} />
  </Glyph>
);

export const IconFunnel = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h13l-4.5 5.5v6l-4-2v-4z" />
  </Glyph>
);

/** Journeys: a branching path, drawn as a switchyard. */
export const IconJourneys = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 8h3.5l2.5-4.5h7" />
    <path d="M5 8l2.5 4.5h7" />
  </Glyph>
);

/** Retention: a cohort grid thinning to the right. */
export const IconRetention = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h13v11h-13z" />
    <path d="M1.5 6.2h13M1.5 9.9h13" />
    <path d="M5.8 2.5v11M10.1 2.5v11" />
  </Glyph>
);

/** Performance: a gain dial with its needle. */
export const IconPerformance = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2 12a6.6 6.6 0 1 1 12 0" />
    <path d="M8 12 11.2 7.4" />
  </Glyph>
);

/** An error: the pen jumping its baseline. */
export const IconErrors = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 9.5h3l1.5-6 2.5 9 2-4.5h4" />
  </Glyph>
);

export const IconSessions = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 4.5h13M1.5 8h9M1.5 11.5h11" />
  </Glyph>
);

export const IconVisitors = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 8.2a2.9 2.9 0 1 0 0-5.8 2.9 2.9 0 0 0 0 5.8z" />
    <path d="M2.5 14c0-2.7 2.5-4.4 5.5-4.4s5.5 1.7 5.5 4.4" />
  </Glyph>
);

/** Replay: the drum, wound back. */
export const IconReplay = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M14 8a6 6 0 1 1-2-4.5" />
    <path d="M14 1.5V5h-3.5" />
  </Glyph>
);

export const IconFlags = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.5 14.5V2" />
    <path d="M3.5 2.5h9l-2 3 2 3h-9" />
  </Glyph>
);

/** Experiments: two channels split from one source. */
export const IconExperiments = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 8h4l2-3.5h7" />
    <path d="M5.5 8l2 3.5h7" />
    <path d="M12 3 14.8 4.5 12 6" />
    <path d="M12 10 14.8 11.5 12 13" />
  </Glyph>
);

export const IconDashboards = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h6v5h-6zM10 2.5h4.5v3h-4.5zM1.5 10h6v3.5h-6zM10 8h4.5v5.5h-4.5z" />
  </Glyph>
);

/** Query: a console prompt. */
export const IconQuery = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 2.5h13v11h-13z" />
    <path d="M4 6.5 6 8.5 4 10.5" />
    <path d="M8 10.5h4" />
  </Glyph>
);

/** Settings: the gain screw, not a cog. */
export const IconSettings = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2.5 4.5h11M2.5 11.5h11" />
    <path d="M6 2.5v4M10.5 9.5v4" />
  </Glyph>
);

/* ── controls ───────────────────────────────────────────────────────────── */

/** The caliper: two jaws with the measured interval between them. */
export const IconCaliper = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.5 2v12M12.5 2v12" />
    <path d="M3.5 8h9" />
  </Glyph>
);

export const IconSearch = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={7} cy={7} r={4.6} />
    <path d="M10.4 10.4 14.2 14.2" />
  </Glyph>
);

export const IconFilter = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M1.5 3.5h13M4 8h8M6.5 12.5h3" />
  </Glyph>
);

export const IconDownload = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 1.5v8.5" />
    <path d="M4.5 7 8 10.5 11.5 7" />
    <path d="M1.5 13.5h13" />
  </Glyph>
);

export const IconCopy = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M5.5 5.5h9v9h-9z" />
    <path d="M10.5 5.5v-4h-9v9h4" />
  </Glyph>
);

export const IconClose = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M3.2 3.2 12.8 12.8M12.8 3.2 3.2 12.8" />
  </Glyph>
);

export const IconChevron = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M4 6 8 10 12 6" />
  </Glyph>
);

export const IconCheck = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M2.5 8.5 6 12 13.5 4" />
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
  </Glyph>
);

export const IconPlus = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 2.5v11M2.5 8h11" />
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
  </Glyph>
);

/** Live: the pen tip resting on paper. */
export const IconPen = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 1.5 10.5 9H5.5z" />
    <path d="M1.5 13.5h13" />
  </Glyph>
);

export const IconClock = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={8} cy={8} r={6.2} />
    <path d="M8 4.2V8l2.8 1.8" />
  </Glyph>
);

export const IconAlert = (p: IconProps) => (
  <Glyph {...p}>
    <path d="M8 1.8 15 13.5H1z" />
    <path d="M8 6v3.4" />
  </Glyph>
);

export const IconInfo = (p: IconProps) => (
  <Glyph {...p}>
    <circle cx={8} cy={8} r={6.2} />
    <path d="M8 7.4v4" />
  </Glyph>
);


/**
 * The brand mark: a traffic line on an ink tile, ending in the orange dot that
 * stands for AI everywhere else in the product. Fixed colours, so the mark reads
 * the same on every surface it is placed on.
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
      <rect width={24} height={24} rx={6.5} fill="#18181b" />
      <path
        d="M5 16.2 9.2 11.6l3.3 2.7 4.3-5.6"
        stroke="#ffffff"
        strokeWidth={2.1}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx={18.1} cy={7.6} r={2.3} fill="#f25a1d" />
    </svg>
  );
}
