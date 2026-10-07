/**
 * The station spine.
 *
 * Not a flush column of links. A recording station identifies itself down the
 * edge of every record it writes, and its channels are listed as channels, so
 * the rail carries the station code set vertically against the paper edge, and
 * the routes are grouped the way a channel bank is grouped.
 *
 * Three distinct small-text treatments live here on purpose: the wordmark in the
 * display face, the channel-bank headings in the data face (they are genuine
 * instrument labels), and the channel names in plain UI sentence case. One
 * costume on every small string is the thing this avoids.
 */
// React 19 no longer publishes a global JSX namespace; it is a named type
// export from "react" instead. Same type, one import.
import type { JSX } from "react";
import { useEffect, useState } from "react";
import { NavLink, useParams } from "react-router";
import {
  IconAgent,
  IconChevron,
  IconDashboards,
  IconDevices,
  IconErrors,
  IconEvents,
  IconExperiments,
  IconFlags,
  IconFunnel,
  IconGap,
  IconGoals,
  IconJourneys,
  IconLocations,
  IconPages,
  IconPaper,
  IconPen,
  IconPerformance,
  IconPolicy,
  IconQuery,
  IconRecord,
  IconReplay,
  IconRetention,
  IconSessions,
  IconSettings,
  IconSmoked,
  IconSources,
  IconVisitors,
  Mark,
} from "./icons";

type ChannelDef = {
  to: string;
  label: string;
  Icon: (p: { size?: number }) => JSX.Element;
  /** Marks the two routes that exist nowhere else. */
  signature?: boolean;
};

const BANKS: { bank: string; channels: ChannelDef[] }[] = [
  {
    bank: "Record",
    channels: [
      { to: "", label: "The record", Icon: IconRecord },
      { to: "/realtime", label: "Live pen", Icon: IconPen },
    ],
  },
  {
    bank: "Machines",
    channels: [
      { to: "/agents", label: "Agent traffic", Icon: IconAgent, signature: true },
      { to: "/citation-gap", label: "Citation gap", Icon: IconGap, signature: true },
      { to: "/policy", label: "Policy vs. observed", Icon: IconPolicy, signature: true },
    ],
  },
  {
    bank: "Audience",
    channels: [
      { to: "/pages", label: "Pages", Icon: IconPages },
      { to: "/sources", label: "Sources", Icon: IconSources },
      { to: "/locations", label: "Locations", Icon: IconLocations },
      { to: "/devices", label: "Devices", Icon: IconDevices },
      { to: "/visitors", label: "Visitors", Icon: IconVisitors },
      { to: "/sessions", label: "Sessions", Icon: IconSessions },
      { to: "/replay", label: "Replay", Icon: IconReplay },
    ],
  },
  {
    bank: "Behaviour",
    channels: [
      { to: "/events", label: "Events", Icon: IconEvents },
      { to: "/goals", label: "Goals", Icon: IconGoals },
      { to: "/funnels", label: "Funnels", Icon: IconFunnel },
      { to: "/journeys", label: "Journeys", Icon: IconJourneys },
      { to: "/retention", label: "Retention", Icon: IconRetention },
      { to: "/flags", label: "Feature flags", Icon: IconFlags },
      { to: "/experiments", label: "Experiments", Icon: IconExperiments },
    ],
  },
  {
    bank: "Health",
    channels: [
      { to: "/performance", label: "Performance", Icon: IconPerformance },
      { to: "/errors", label: "Errors", Icon: IconErrors },
    ],
  },
  {
    bank: "Bench",
    channels: [
      { to: "/dashboards", label: "Dashboards", Icon: IconDashboards },
      { to: "/query", label: "Query console", Icon: IconQuery },
      { to: "/settings", label: "Settings", Icon: IconSettings },
    ],
  },
];

/** Two record media, never a sun and a moon on a sliding pill. */
function MediumSwitch() {
  const [medium, setMedium] = useState<"smoked" | "paper">("smoked");

  useEffect(() => {
    const stored = (() => {
      try {
        return localStorage.getItem("mf-record");
      } catch {
        return null;
      }
    })();
    if (stored === "paper" || stored === "smoked") setMedium(stored);
    else
      setMedium(
        window.matchMedia("(prefers-color-scheme: light)").matches ? "paper" : "smoked",
      );
  }, []);

  const choose = (next: "smoked" | "paper") => {
    setMedium(next);
    document.documentElement.setAttribute("data-record", next);
    try {
      localStorage.setItem("mf-record", next);
    } catch {
      /* private mode: the choice simply does not persist */
    }
  };

  return (
    <div className="mf-medium" role="group" aria-label="Recording medium">
      <button
        type="button"
        className="mf-medium__opt"
        aria-pressed={medium === "smoked"}
        onClick={() => choose("smoked")}
      >
        <IconSmoked size={14} />
        <span>Smoked</span>
      </button>
      <button
        type="button"
        className="mf-medium__opt"
        aria-pressed={medium === "paper"}
        onClick={() => choose("paper")}
      >
        <IconPaper size={14} />
        <span>Paper</span>
      </button>
    </div>
  );
}

export interface AppShellProps {
  children: React.ReactNode;
  siteDomain?: string;
  /** Station code: the short identifier written down the record's edge. */
  stationCode?: string;
  liveHumans?: number;
  liveAgents?: number;
  right?: React.ReactNode;
}

export function AppShell({
  children,
  siteDomain,
  stationCode,
  liveHumans = 0,
  liveAgents = 0,
  right,
}: AppShellProps) {
  const { site } = useParams();
  const base = site ? `/app/${site}` : "/app";
  const [open, setOpen] = useState(false);

  useEffect(() => {
    setOpen(false);
  }, [site]);

  const code =
    stationCode ??
    (siteDomain ? siteDomain.replace(/^www\./, "").slice(0, 14).toUpperCase() : "STATION");

  return (
    <div className="mf-shell" data-rail-open={open ? "true" : "false"}>
      <nav id="mf-rail" className="mf-rail" aria-label="Channels">
        <div className="mf-rail__head">
          <a className="mf-rail__brand" href="/app" aria-label="Micaforge, all sites">
            <Mark size={26} />
            <span className="mf-rail__wordmark">Micaforge</span>
          </a>
        </div>

        {/* The station code, written down the paper's edge exactly as a recorder
            stamps its own identity onto every record it produces. */}
        {site && (
          <div className="mf-rail__station" aria-hidden="true">
            <span className="mf-rail__station-code">{code}</span>
          </div>
        )}

        <div className="mf-rail__banks">
          {BANKS.map(({ bank, channels }) => (
            <section key={bank} className="mf-bank">
              <h2 className="mf-bank__label">{bank}</h2>
              <ul className="mf-bank__list">
                {channels.map(({ to, label, Icon, signature }) => (
                  <li key={to || "root"}>
                    <NavLink
                      to={`${base}${to}`}
                      end={to === ""}
                      className={({ isActive }) =>
                        `mf-chan${isActive ? " is-live" : ""}${signature ? " is-signature" : ""}`
                      }
                    >
                      <Icon size={16} />
                      <span className="mf-chan__label">{label}</span>
                    </NavLink>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>

        <div className="mf-rail__foot">
          <MediumSwitch />
        </div>
      </nav>

      <div className="mf-shell__body">
        <header className="mf-topbar">
          {/* The channel drawer's key lives in the bar, not floating over the
              sheet. A fixed button at the corner covers whatever happens to
              scroll under it, and on a phone that was a verdict figure on one
              screen and a table row on another. The bar is already sticky, so
              the control is just as reachable and occludes nothing. */}
          <button
            type="button"
            className="mf-topbar__channels"
            aria-expanded={open}
            aria-controls="mf-rail"
            onClick={() => setOpen((v) => !v)}
          >
            <IconChevron
              size={15}
              style={{ transform: open ? "rotate(90deg)" : "rotate(-90deg)" }}
            />
            <span className="mf-topbar__channels-label">Channels</span>
          </button>

          <div className="mf-topbar__site">
            {siteDomain ? (
              <a
                className="mf-topbar__domain"
                href={`https://${siteDomain}`}
                target="_blank"
                rel="noreferrer noopener"
              >
                {siteDomain}
              </a>
            ) : (
              <span className="mf-topbar__domain">All sites</span>
            )}
          </div>

          {/* The live readout. Two channels, always both. The whole point is
              that a machine reader is as real as a human one. */}
          <div className="mf-live" aria-live="polite">
            <span className="mf-live__pair">
              <span className="mf-live__pen" data-pen="written" aria-hidden="true" />
              <span className="mf-live__n mf-data">{liveHumans}</span>
              <span className="mf-live__unit">reading</span>
            </span>
            <span className="mf-live__pair">
              <span className="mf-live__pen" data-pen="stepped" aria-hidden="true" />
              <span className="mf-live__n mf-data">{liveAgents}</span>
              <span className="mf-live__unit">fetching</span>
            </span>
          </div>

          <div className="mf-topbar__right">{right}</div>
        </header>

        <main id="main" className="mf-main">
          {children}
        </main>
      </div>

      {open && (
        <button
          type="button"
          className="mf-shell__scrim"
          aria-label="Close channels"
          onClick={() => setOpen(false)}
        />
      )}
    </div>
  );
}

export default AppShell;
