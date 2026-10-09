/**
 * A PUBLIC STATUS PAGE, at /status/<slug>.
 *
 * Read by a site's customers at the worst moment: something is not working
 * and they want to know whether it is them. So the page answers in the order
 * they ask: is everything working (the banner), what is wrong and what is
 * being done (current incidents), is anything planned (maintenance), how has
 * each part done over ninety days, and what happened recently.
 *
 * WHY THIS IS ITS OWN SMALL ISLAND. Every visitor to every customer's status
 * page downloads this code, and most of them have never heard of Micaforge.
 * So it does not import the dashboard, its router, its query cache or its
 * providers: one `fetch` with no credentials, a few components, and the shared
 * words in ./words.ts so a state is named the same here as where it is posted.
 *
 * THE ACCENT is the page owner's colour, and it is used for exactly two
 * things: the rule across the top and the links. A state is always a word and
 * a shape, in the product's own tones, so an owner whose brand is red does not
 * make every monitor look down. When the accent is too light to read as link
 * text on white, links fall back to the ink colour and keep their underline.
 *
 * The page refreshes itself every minute while it is open, quietly: a failed
 * refresh keeps the last good reading on screen rather than replacing it with
 * an error, and the "Updated" time says how old that reading is.
 */
import { useEffect, useMemo, useState } from "react";
import { API_BASE } from "../../lib/api";
import "../../styles/status.css";
import { DayBars, DayLegend } from "./DayBars";
import { StateShape } from "./shapes";
import type { Shape } from "./shapes";
import { IMPACT_WORD, KIND_WORD, STATUS_WORD, uptimeText, when } from "./words";
import type { MonitorState, Overall, PublicStatus, StatusIncidentPublic } from "./words";

/* ── reading the page ────────────────────────────────────────────────────── */

type Phase =
  | { kind: "loading" }
  | { kind: "missing" }
  | { kind: "error" }
  | { kind: "ready"; data: PublicStatus };

const REFRESH_MS = 60_000;

/** The slug is the path segment after /status/, wherever the site is mounted. */
function slugFromPath(path: string): string | null {
  const m = /\/status\/([^/]+)\/?$/.exec(path);
  if (!m) return null;
  try {
    return decodeURIComponent(m[1]);
  } catch {
    return null;
  }
}

/** A list the server should always send; an absent one is an empty one. */
const list = <T,>(v: T[] | null | undefined): T[] => (Array.isArray(v) ? v : []);

async function load(slug: string, signal: AbortSignal): Promise<PublicStatus | "missing"> {
  const res = await fetch(`${API_BASE}/api/status/${encodeURIComponent(slug)}`, {
    credentials: "omit",
    headers: { accept: "application/json" },
    signal,
  });
  if (res.status === 404) return "missing";
  if (!res.ok) throw new Error(`status ${res.status}`);
  const body = (await res.json()) as { data?: PublicStatus };
  const d = body.data;
  if (!d || typeof d.title !== "string") throw new Error("no page in the answer");
  return {
    ...d,
    monitors: list(d.monitors).map((m) => ({ ...m, days: list(m.days) })),
    active: list(d.active),
    scheduled: list(d.scheduled),
    history: list(d.history),
  };
}

/* ── the accent ──────────────────────────────────────────────────────────── */

const HEX = /^#[0-9a-fA-F]{6}$/;

/** WCAG relative luminance of a "#rrggbb" colour. */
function luminance(hex: string): number {
  const ch = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const lin = ch.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
}

/** The rule takes any valid accent; link text takes it only when it reads on white. */
function accentStyle(accent: string): React.CSSProperties {
  if (!HEX.test(accent)) return {};
  const contrast = 1.05 / (luminance(accent) + 0.05);
  return {
    ["--mf-status-accent" as string]: accent,
    ...(contrast >= 4.5 ? { ["--mf-status-link" as string]: accent } : {}),
  };
}

/* ── words for states ────────────────────────────────────────────────────── */

const OVERALL: Record<Overall, { shape: Shape; word: string }> = {
  operational: { shape: "up", word: "All systems operational" },
  degraded: { shape: "partial", word: "Some systems are degraded" },
  outage: { shape: "down", word: "Major outage" },
  maintenance: { shape: "maintenance", word: "Maintenance in progress" },
  unknown: { shape: "unknown", word: "Status unknown" },
};

const MONITOR_WORD: Record<MonitorState, string> = {
  up: "Operational",
  down: "Down",
  unknown: "Not checked yet",
};

function zoneName(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return "";
  }
}

/* ── pieces ──────────────────────────────────────────────────────────────── */

function Notice({ incident, names }: { incident: StatusIncidentPublic; names: Map<string, string> }) {
  const hit = incident.monitor_ids.map((id) => names.get(id)).filter((n): n is string => !!n);
  return (
    <article className="mf-status-notice" data-kind={incident.kind}>
      <h3 className="mf-status-notice__title">{incident.title}</h3>
      <p className="mf-status-notice__meta">
        <span>{KIND_WORD[incident.kind]}</span>
        <span className="mf-status-notice__status">{STATUS_WORD[incident.status]}</span>
        {incident.kind === "incident" ? <span>{IMPACT_WORD[incident.impact]}</span> : null}
        <span>{hit.length ? `Affects ${hit.join(", ")}` : "Affects all systems"}</span>
      </p>
      {incident.kind === "maintenance" && incident.scheduled_start ? (
        <p className="mf-status-notice__meta">
          From {when(incident.scheduled_start, true)} to {when(incident.scheduled_end, true)}
        </p>
      ) : null}
      {incident.updates.length ? (
        <ol className="mf-status-timeline">
          {incident.updates.map((u, i) => (
            <li key={`${u.created_at}-${i}`} className="mf-status-timeline__item">
              <p className="mf-status-timeline__head">
                <strong>{STATUS_WORD[u.status]}</strong>
                <time dateTime={u.created_at}>{when(u.created_at)}</time>
              </p>
              <p className="mf-status-timeline__body">{u.body}</p>
            </li>
          ))}
        </ol>
      ) : null}
    </article>
  );
}

function Waiting() {
  return (
    <div className="mf-status-wait" aria-busy="true" aria-live="polite">
      <span className="mf-status-sr">Loading the status page…</span>
      <span className="mf-status-wait__bar" style={{ width: "42%", height: "22px" }} />
      <span className="mf-status-wait__bar" style={{ width: "100%", height: "56px" }} />
      {[0, 1, 2].map((i) => (
        <span key={i} className="mf-status-wait__bar" style={{ width: "100%", height: "44px" }} />
      ))}
    </div>
  );
}

/* ── the page ────────────────────────────────────────────────────────────── */

export function StatusPage() {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });

  useEffect(() => {
    const slug = slugFromPath(window.location.pathname);
    if (!slug) {
      setPhase({ kind: "missing" });
      return;
    }
    let ctl = new AbortController();
    let hasData = false;

    const read = () => {
      ctl.abort();
      ctl = new AbortController();
      load(slug, ctl.signal)
        .then((r) => {
          if (r === "missing") {
            hasData = false;
            setPhase({ kind: "missing" });
          } else {
            hasData = true;
            setPhase({ kind: "ready", data: r });
          }
        })
        .catch((e: unknown) => {
          if (e instanceof DOMException && e.name === "AbortError") return;
          // A failed refresh keeps the last good reading; only a first load fails.
          if (!hasData) setPhase({ kind: "error" });
        });
    };

    read();
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") read();
    }, REFRESH_MS);
    return () => {
      window.clearInterval(timer);
      ctl.abort();
    };
  }, []);

  const data = phase.kind === "ready" ? phase.data : null;

  useEffect(() => {
    if (data) document.title = data.title;
  }, [data?.title]);

  const names = useMemo(() => new Map((data?.monitors ?? []).map((m) => [m.id, m.name])), [data]);

  if (phase.kind === "loading") {
    return (
      <div className="mf-status-frame">
        <main id="main" className="mf-status">
          <Waiting />
        </main>
      </div>
    );
  }

  if (phase.kind === "missing" || phase.kind === "error") {
    const missing = phase.kind === "missing";
    return (
      <div className="mf-status-frame">
        <main id="main" className="mf-status">
          <div className="mf-status-gone" role={missing ? undefined : "alert"}>
          <h1 className="mf-status-gone__title">
            {missing ? "There is no status page at this address" : "The status page could not be loaded. Reload to try again."}
          </h1>
          <p className="mf-status-gone__body">
            {missing
              ? "Check the address for a typing mistake. If you followed a link, the page may have been taken down or not published yet."
              : "The server did not answer, or answered with an error. Your connection may be offline, or the service that hosts this page may be having trouble."}
          </p>
          {missing ? null : (
            <button type="button" className="mf-status-button" onClick={() => window.location.reload()}>
              Reload
            </button>
          )}
          </div>
        </main>
      </div>
    );
  }

  const d = phase.data;
  const overall = OVERALL[d.overall] ?? OVERALL.unknown;
  const zone = zoneName();

  return (
    <div className="mf-status-frame" style={accentStyle(d.accent_color)}>
      <main id="main" className="mf-status">
        <header className="mf-status-head">
          {d.logo_url ? <img className="mf-status-head__logo" src={d.logo_url} alt="" height={40} /> : null}
          <h1 className="mf-status-head__title">{d.title}</h1>
          {d.description ? <p className="mf-status-head__desc">{d.description}</p> : null}
        </header>

        <section className="mf-status-banner" data-overall={d.overall} aria-live="polite">
          <StateShape shape={overall.shape} size={20} />
          <div>
            <h2 className="mf-status-banner__word">{overall.word}</h2>
            <p className="mf-status-banner__when">
              Updated <time dateTime={d.generated_at}>{when(d.generated_at)}</time>
            </p>
          </div>
        </section>

        {d.active.length ? (
          <section className="mf-status-section" aria-labelledby="mf-status-active">
            <h2 id="mf-status-active" className="mf-status-section__title">
              Current incidents
            </h2>
            {d.active.map((i) => (
              <Notice key={i.id} incident={i} names={names} />
            ))}
          </section>
        ) : null}

        {d.scheduled.length ? (
          <section className="mf-status-section" aria-labelledby="mf-status-planned">
            <h2 id="mf-status-planned" className="mf-status-section__title">
              Scheduled maintenance
            </h2>
            {d.scheduled.map((i) => (
              <Notice key={i.id} incident={i} names={names} />
            ))}
          </section>
        ) : null}

        <section className="mf-status-section" aria-labelledby="mf-status-systems">
          <div className="mf-status-section__head">
            <h2 id="mf-status-systems" className="mf-status-section__title">
              Systems
            </h2>
            {d.monitors.length ? <DayLegend /> : null}
          </div>
          {d.monitors.length ? (
            <ul className="mf-status-systems">
              {d.monitors.map((m) => (
                <li key={m.id} className="mf-status-system">
                  <div className="mf-status-system__head">
                    <span className="mf-status-system__name">{m.name}</span>
                    <span className="mf-status-state" data-state={m.state}>
                      <StateShape shape={m.state} size={11} />
                      {MONITOR_WORD[m.state] ?? MONITOR_WORD.unknown}
                    </span>
                  </div>
                  <DayBars days={m.days} name={m.name} />
                  <p className="mf-status-system__uptime">
                    <span className="mf-status-figure">{uptimeText(m.uptime_90d)}</span> uptime over 90 days
                  </p>
                </li>
              ))}
            </ul>
          ) : (
            <p className="mf-status-quiet">This page lists no systems yet.</p>
          )}
        </section>

        <section className="mf-status-section" aria-labelledby="mf-status-history">
          <h2 id="mf-status-history" className="mf-status-section__title">
            Past 14 days
          </h2>
          {d.history.length ? (
            d.history.map((i) => <Notice key={i.id} incident={i} names={names} />)
          ) : (
            <p className="mf-status-quiet">No incidents or maintenance in the last 14 days.</p>
          )}
        </section>

        <footer className="mf-status-foot">
          {d.footer ? (
            <a className="mf-status-link" href={d.footer.url} rel="noopener">
              {d.footer.label}
            </a>
          ) : null}
          <p className="mf-status-quiet">
            Times are in your time zone{zone ? ` (${zone})` : ""}. Daily bars count UTC days.
          </p>
        </footer>
      </main>
    </div>
  );
}

export default StatusPage;
