/**
 * The first viewport.
 *
 * The record leads. The headline is not stacked above a chart: it is written
 * into the record's own top margin, the way an operator captions the sheet they
 * just pulled off the drum. The two pens beneath it are the argument: what the
 * machines took, drawn upward; what came back, drawn downward; the debt between
 * them hatched.
 *
 * Deliberately not the category's hero: no eyebrow, no centred stack, no
 * product panel floated to the right, no filled-button-next-to-ghost-button.
 */
import { useMemo, useState } from "react";
import { Drum } from "./Drum";
import { demoRecord } from "../../lib/demo";
import { gapRatio, num } from "../../lib/format";

const WINDOWS: { key: string; label: string; days: number }[] = [
  { key: "30d", label: "30 days", days: 30 },
  { key: "60d", label: "60 days", days: 60 },
  { key: "90d", label: "90 days", days: 90 },
];

export function HeroRecord() {
  const [win, setWin] = useState("60d");
  const days = WINDOWS.find((w) => w.key === win)?.days ?? 60;
  const rec = useMemo(() => demoRecord(days), [days]);

  const took = rec.machines.reduce((a, b) => a + b, 0);
  const back = rec.answers.reduce((a, b) => a + b, 0);

  return (
    <div className="mf-hero">
      <div className="mf-hero__sheet mf-panel mf-panel--feed">
        {/* The caption, written on the record's own top margin. */}
        <div className="mf-hero__caption">
          <h1 className="mf-hero__head">
            What the machines took.
            <br />
            What came back.
          </h1>
          <p className="mf-hero__sub">
            Micaforge is open-source, cookieless analytics that counts AI crawlers and
            answer-engine referrals as an audience rather than as noise, then shows you the
            distance between the two.
          </p>

          <div className="mf-hero__act">
            <a className="mf-btn mf-btn--primary mf-btn--md" href="/self-host">
              <span className="mf-btn__label">Self-host it</span>
            </a>
            <a className="mf-hero__link" href="/why-agent-analytics">
              Why this is a different measurement
              <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true" fill="none">
                <path d="M4 12 12 4" stroke="currentColor" strokeWidth="1.4" strokeLinecap="square" />
                <path d="M5.5 4H12v6.5" stroke="currentColor" strokeWidth="1.4" strokeLinecap="square" />
              </svg>
            </a>
          </div>
        </div>

        <div className="mf-hero__record">
          <div className="mf-hero__winbar">
            <div className="mf-hero__wins" role="group" aria-label="Window">
              {WINDOWS.map((w) => (
                <button
                  key={w.key}
                  type="button"
                  className="mf-caliper__stop"
                  aria-pressed={win === w.key}
                  onClick={() => setWin(w.key)}
                >
                  {w.label}
                </button>
              ))}
            </div>
          </div>

          <Drum
            times={rec.times}
            mode="mirrored"
            height={300}
            demo
            annotations={rec.annotations}
            channels={[
              {
                id: "machines",
                label: "Machine fetches",
                color: "var(--mf-trace-agent)",
                pen: "stepped",
                values: rec.machines,
                format: num,
              },
              {
                id: "readers",
                label: "Arrived from an answer engine",
                color: "var(--mf-trace-human)",
                pen: "written",
                values: rec.answers,
                format: num,
              },
            ]}
          />
        </div>

        {/* The readout bank. One strip, ruled, not a row of stat cards. */}
        <div className="mf-hero__readout">
          <div className="mf-hero__chan">
            <span className="mf-hero__chan-label">Taken</span>
            <span className="mf-hero__chan-val mf-data">{num(took)}</span>
            <span className="mf-hero__chan-note">fetches by named AI agents</span>
          </div>
          <div className="mf-hero__chan">
            <span className="mf-hero__chan-label">Returned</span>
            <span className="mf-hero__chan-val mf-data">{num(back)}</span>
            <span className="mf-hero__chan-note">readers arriving from an answer</span>
          </div>
          <div className="mf-hero__chan mf-hero__chan--gap">
            <span className="mf-hero__chan-label">The Citation Gap</span>
            <span className="mf-hero__chan-val mf-data">{gapRatio(took, back)}</span>
            <span className="mf-hero__chan-note">taken for every one that came back</span>
          </div>
        </div>
      </div>
    </div>
  );
}

export default HeroRecord;
