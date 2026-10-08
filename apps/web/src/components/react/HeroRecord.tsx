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
 *
 * The record runs. Every few seconds a day is written at the right edge and the
 * paper moves on, so the first thing a reader sees is the instrument doing its
 * job: counting what the machines take, beside what comes back. It runs only
 * while it is on screen and the tab is visible, never under reduced motion, and
 * one key holds it still.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { Drum } from "./Drum";
import { demoAdvance } from "../../lib/demo";
import { gapRatio, num } from "../../lib/format";

/** One drum step. Slow enough to read a figure before the next one lands. */
const STEP_MS = 2800;

/** Is the reader asking for less motion? Assumed yes until the client says. */
function useReducedMotion() {
  const [reduce, setReduce] = useState(true);
  useEffect(() => {
    const mq = window.matchMedia?.("(prefers-reduced-motion: reduce)");
    if (!mq) {
      setReduce(false);
      return;
    }
    const sync = () => setReduce(mq.matches);
    sync();
    mq.addEventListener("change", sync);
    return () => mq.removeEventListener("change", sync);
  }, []);
  return reduce;
}

/** Is this element on screen, in a tab the reader is looking at? */
function useSeen(ref: { current: HTMLElement | null }) {
  const [onScreen, setOnScreen] = useState(true);
  const [tabVisible, setTabVisible] = useState(true);
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof IntersectionObserver === "undefined") return;
    const io = new IntersectionObserver((e) => setOnScreen(e.some((x) => x.isIntersecting)));
    io.observe(el);
    const vis = () => setTabVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", vis);
    return () => {
      io.disconnect();
      document.removeEventListener("visibilitychange", vis);
    };
  }, [ref]);
  return onScreen && tabVisible;
}

const WINDOWS: { key: string; label: string; days: number }[] = [
  { key: "30d", label: "30 days", days: 30 },
  { key: "60d", label: "60 days", days: 60 },
  { key: "90d", label: "90 days", days: 90 },
];

export function HeroRecord() {
  const [win, setWin] = useState("60d");
  const days = WINDOWS.find((w) => w.key === win)?.days ?? 60;

  // `steps` is how far this window has run; `advanced` counts every step ever
  // taken and never resets, so the drum can tell a step (slide) from a new
  // window (rewrite).
  const [steps, setSteps] = useState(0);
  const [advanced, setAdvanced] = useState(0);
  const [held, setHeld] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const reduce = useReducedMotion();
  const seen = useSeen(rootRef);
  const running = !reduce && !held && seen;

  useEffect(() => {
    if (!running) return;
    const id = window.setInterval(() => {
      setSteps((s) => s + 1);
      setAdvanced((a) => a + 1);
    }, STEP_MS);
    return () => window.clearInterval(id);
  }, [running]);

  const rec = useMemo(() => demoAdvance(days, steps), [days, steps]);

  const took = rec.machines.reduce((a, b) => a + b, 0);
  const back = rec.answers.reduce((a, b) => a + b, 0);

  return (
    <div className="mf-hero" ref={rootRef}>
      <div className="mf-hero__sheet mf-panel mf-panel--feed">
        {/* The caption, written on the record's own top margin. */}
        <div className="mf-hero__caption">
          <h1 className="mf-hero__head">
            What the machines took.
            <br />
            What came back.
          </h1>
          <p className="mf-hero__sub">
            See which AI crawlers read your pages, which answers send readers back, and which
            pages are worth writing more of. Open-source, cookieless analytics that counts the
            machines as an audience instead of throwing them away.
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
            {!reduce && (
              <button
                type="button"
                className="mf-caliper__stop mf-hero__hold"
                aria-pressed={held}
                onClick={() => setHeld((h) => !h)}
              >
                {held ? "Run the drum" : "Hold the drum"}
              </button>
            )}
            <div className="mf-hero__wins" role="group" aria-label="Window">
              {WINDOWS.map((w) => (
                <button
                  key={w.key}
                  type="button"
                  className="mf-caliper__stop"
                  aria-pressed={win === w.key}
                  onClick={() => {
                    setWin(w.key);
                    setSteps(0);
                  }}
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
            live={!reduce}
            writeOnMount={false}
            advance={advanced}
            annotations={rec.annotations}
            channels={[
              {
                id: "machines",
                label: "Machine fetches",
                color: "var(--mf-ai)",
                pen: "stepped",
                values: rec.machines,
                format: num,
              },
              {
                id: "readers",
                label: "Arrived from an answer engine",
                color: "var(--mf-people)",
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
