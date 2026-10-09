/**
 * One drawn shape per state, so a state never rests on colour alone.
 *
 * A monitor's state is said three ways at once: a word, a shape and a tone.
 * The shape is the one that survives a glance, greyscale and the common
 * colour-vision deficiencies: a filled disc is up, a triangle pointing down is
 * down, an empty ring is not checked yet, two bars are paused. The overall
 * state of a status page adds a half-filled disc for "degraded" and a clock
 * in a square for maintenance.
 *
 * Shared by the dashboard and the public status page, so a reader who learns
 * the shapes on one learns them for both. Drawn in currentColor; the parent
 * sets the tone.
 */
export type Shape = "up" | "down" | "unknown" | "paused" | "partial" | "maintenance";

export function StateShape({ shape, size = 12 }: { shape: Shape; size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 12 12"
      aria-hidden="true"
      focusable="false"
      style={{ flex: "none" }}
    >
      {shape === "up" ? <circle cx={6} cy={6} r={5} fill="currentColor" /> : null}
      {shape === "down" ? <path d="M0.8 2.2h10.4L6 11.2z" fill="currentColor" /> : null}
      {shape === "unknown" ? (
        <circle cx={6} cy={6} r={4.3} fill="none" stroke="currentColor" strokeWidth={1.5} />
      ) : null}
      {shape === "paused" ? (
        <path d="M3.6 2.2v7.6M8.4 2.2v7.6" stroke="currentColor" strokeWidth={2.2} strokeLinecap="round" />
      ) : null}
      {shape === "partial" ? (
        <>
          <circle cx={6} cy={6} r={4.4} fill="none" stroke="currentColor" strokeWidth={1.4} />
          <path d="M6 1.6a4.4 4.4 0 0 0 0 8.8z" fill="currentColor" />
        </>
      ) : null}
      {shape === "maintenance" ? (
        <>
          <rect x={1.2} y={1.2} width={9.6} height={9.6} rx={1.8} fill="none" stroke="currentColor" strokeWidth={1.4} />
          <path d="M6 3.4V6.2l1.9 1.3" fill="none" stroke="currentColor" strokeWidth={1.4} strokeLinecap="round" />
        </>
      ) : null}
    </svg>
  );
}
