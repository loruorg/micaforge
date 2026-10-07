/**
 * The cohort grid.
 *
 * Value is carried twice: as ink density stepped through the shellac ramp, and
 * as the figure itself, printed in every single cell. The ramp is the fast
 * read; the number is the true one. Neither is ever the only encoding, so the
 * grid survives greyscale, colour-vision deficiency and a printer.
 *
 * Density is always taken from the SHARE retained, even when the grid is set to
 * show counts, so a small cohort and a large one can be compared down a column
 * without the eye being fooled by cohort size. That decision is stated in the
 * legend rather than left for a reader to infer.
 *
 * A cell that is not yet old enough to have a value is drawn as an em dash, not
 * as a zero. A cohort that has not lived long enough has not "lost" anyone.
 */
import { useEffect, useRef, useState } from "react";
import { num, pct } from "../../lib/format";

export interface Cohort {
  /** The cohort's own label, e.g. "2026-W31" or "2026-07". */
  cohort: string;
  /** Start of the cohort, epoch ms. */
  started: number;
  /** How many visitors the cohort began with. */
  size: number;
  /** One entry per period; null where the cohort is not that old yet. */
  values: (number | null)[];
}

export type CohortMode = "share" | "count";
export type CohortUnit = "week" | "month";

export interface CohortGridProps {
  cohorts: Cohort[];
  periods: number;
  unit: CohortUnit;
  mode: CohortMode;
  /** Per-period average share across cohorts, when the server computes it. */
  average?: (number | null)[];
  /** Formats a cohort label; defaults to the label the server sent. */
  formatCohort?: (c: Cohort) => string;
}

/** Fixed thresholds, so the same share always reads as the same weight of ink. */
function bin(share: number | null): number {
  if (share == null || !Number.isFinite(share) || share <= 0.005) return 0;
  if (share < 0.1) return 1;
  if (share < 0.25) return 2;
  if (share < 0.45) return 3;
  if (share < 0.7) return 4;
  return 5;
}

export function CohortGrid({
  cohorts,
  periods,
  unit,
  mode,
  average,
  formatCohort,
}: CohortGridProps) {
  const unitWord = unit === "week" ? "Week" : "Month";
  const cols = Math.max(1, periods);
  const [wrapRef, scrolls] = useOverflow<HTMLDivElement>([cohorts.length, cols, mode]);

  return (
    <>
      <div className="mfb-cohort-wrap" ref={wrapRef}>
        <table
          className="mfb-cohort"
          /* Below this the grid scrolls inside its own container rather than
             crushing the figures; the page body never scrolls sideways. */
          style={{ minWidth: `calc(9rem + 5rem + ${cols} * 3.75rem)` }}
        >
          {/* The caption is the table's first child: that is where the parser
              and every assistive technology expect to find it. */}
          <caption className="mf-sr">
            {`Retention by ${unit} cohort. Each row is the visitors first seen in that ${unit}; each
              column is how many of them came back that many ${unit}s later, shown as ${
                mode === "share" ? "a share of the cohort" : "a count of people"
              }.`}
          </caption>
          <colgroup>
            <col />
            <col />
            {Array.from({ length: cols }).map((_, i) => (
              <col key={i} />
            ))}
          </colgroup>
          <thead>
            <tr>
              <th scope="col">Cohort</th>
              <th scope="col">People</th>
              {Array.from({ length: cols }).map((_, i) => (
                <th key={i} scope="col" className="is-period">
                  {i === 0 ? (
                    `${unitWord} 0`
                  ) : (
                    <>
                      {i}
                      {/* The column only has room for the numeral, so the unit
                          it counts is spoken rather than printed twice. */}
                      <span className="mf-sr">{` ${unitWord.toLowerCase()}s later`}</span>
                    </>
                  )}
                </th>
              ))}
            </tr>
          </thead>

          <tbody>
            {cohorts.map((c) => (
              <tr key={c.cohort}>
                <th scope="row">{formatCohort ? formatCohort(c) : c.cohort}</th>
                <td className="mfb-cell mfb-cell--size">{num(c.size)}</td>
                {Array.from({ length: cols }).map((_, i) => {
                  const raw = c.values[i];
                  if (raw == null) {
                    return (
                      <td
                        key={i}
                        className="mfb-cell mfb-cell--void"
                        data-bin="0"
                        title={`This cohort is not ${i} ${unit}${i === 1 ? "" : "s"} old yet.`}
                      >
                        —<span className="mf-sr"> not yet measurable</span>
                      </td>
                    );
                  }
                  const share = c.size > 0 ? raw / c.size : 0;
                  return (
                    <td
                      key={i}
                      className="mfb-cell"
                      data-bin={bin(share)}
                      title={`${num(raw)} of ${num(c.size)} came back, ${pct(share, 1)}`}
                    >
                      {mode === "share" ? pct(share, share < 0.1 ? 1 : 0) : num(raw)}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>

          {average?.length ? (
            <tfoot>
              <tr>
                <th scope="row">All cohorts</th>
                <td className="mfb-cell mfb-cell--size">
                  {num(cohorts.reduce((a, c) => a + c.size, 0))}
                </td>
                {Array.from({ length: cols }).map((_, i) => {
                  const a = average[i];
                  return (
                    <td key={i} className="mfb-cell" data-bin={bin(a ?? null)}>
                      {a == null ? "—" : pct(a, a < 0.1 ? 1 : 0)}
                    </td>
                  );
                })}
              </tr>
            </tfoot>
          ) : null}
        </table>
      </div>

      <div className="mfb-legend">
        <span>Ink weight</span>
        <span className="mfb-legend__ramp" aria-hidden="true">
          {[1, 2, 3, 4, 5].map((b) => (
            <span key={b} className="mfb-legend__step" data-bin={b} />
          ))}
        </span>
        <span>under 10% held, to over 70%</span>
      </div>

      <p className="mfb-note">
        Ink follows the share retained even when the grid is showing people, so two
        cohorts of different sizes can still be read down a column. A dash is a period
        the cohort has not lived through yet, not a zero.
        {scrolls
          ? " There are more periods than fit: drag the grid sideways to read the rest. The cohort's own column stays put."
          : ""}
      </p>
    </>
  );
}

/**
 * Whether an element is currently wider than the room it has. Used to tell a
 * reader that a grid scrolls, rather than leaving them to discover the clipped
 * edge for themselves.
 */
function useOverflow<T extends HTMLElement>(deps: unknown[]) {
  const ref = useRef<T | null>(null);
  const [over, setOver] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const check = () => setOver(el.scrollWidth > el.clientWidth + 1);
    check();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return [ref, over] as const;
}

export default CohortGrid;
