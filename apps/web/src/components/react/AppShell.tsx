/**
 * The dashboard frame: a left navigation grouped by the question each screen
 * answers, and a top bar with the site and who is on it right now.
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
  IconFrustration,
  IconFunnel,
  IconGap,
  IconGoals,
  IconJourneys,
  IconLocations,
  IconPages,
  IconPen,
  IconPerformance,
  IconPolicy,
  IconQuery,
  IconRecord,
  IconReplay,
  IconRetention,
  IconSessions,
  IconSettings,
  IconSources,
  IconVisitors,
  Mark,
  IconAsk,
  IconInsight,
} from "./icons";
import {
  IconLink,
  IconMcp,
  IconRevenue,
  IconSearchResults,
  IconStatusPage,
  IconUptime,
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
    bank: "Overview",
    channels: [
      { to: "", label: "Dashboard", Icon: IconRecord },
      { to: "/realtime", label: "Realtime", Icon: IconPen },
      { to: "/ask", label: "Ask AI", Icon: IconAsk },
      { to: "/insights", label: "Insights", Icon: IconInsight },
    ],
  },
  {
    bank: "AI traffic",
    channels: [
      { to: "/agents", label: "AI crawlers", Icon: IconAgent, signature: true },
      { to: "/citation-gap", label: "Citation gap", Icon: IconGap, signature: true },
      { to: "/policy", label: "Robots.txt rules", Icon: IconPolicy, signature: true },
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
    bank: "Behavior",
    channels: [
      { to: "/events", label: "Events", Icon: IconEvents },
      { to: "/goals", label: "Goals", Icon: IconGoals },
      { to: "/funnels", label: "Funnels", Icon: IconFunnel },
      { to: "/journeys", label: "Journeys", Icon: IconJourneys },
      { to: "/retention", label: "Retention", Icon: IconRetention },
      { to: "/flags", label: "Feature flags", Icon: IconFlags },
      { to: "/experiments", label: "Experiments", Icon: IconExperiments },
      { to: "/frustration", label: "Frustration", Icon: IconFrustration },
    ],
  },
  {
    bank: "Health",
    channels: [
      { to: "/performance", label: "Performance", Icon: IconPerformance },
      { to: "/errors", label: "Errors", Icon: IconErrors },
      { to: "/mcp", label: "MCP servers", Icon: IconMcp },
    ],
  },
  {
    bank: "Monitoring",
    channels: [
      { to: "/uptime", label: "Uptime", Icon: IconUptime },
      { to: "/status-pages", label: "Status pages", Icon: IconStatusPage },
    ],
  },
  {
    bank: "Growth",
    channels: [
      { to: "/links", label: "Links", Icon: IconLink },
      { to: "/revenue", label: "Revenue", Icon: IconRevenue },
      { to: "/search", label: "Search", Icon: IconSearchResults },
    ],
  },
  {
    bank: "Workspace",
    channels: [
      { to: "/dashboards", label: "Dashboards", Icon: IconDashboards },
      { to: "/query", label: "Query", Icon: IconQuery },
      { to: "/settings", label: "Settings", Icon: IconSettings },
    ],
  },
];

export interface AppShellProps {
  children: React.ReactNode;
  siteDomain?: string;
  liveHumans?: number;
  liveAgents?: number;
  right?: React.ReactNode;
}

export function AppShell({
  children,
  siteDomain,
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

  return (
    <div className="mf-shell" data-rail-open={open ? "true" : "false"}>
      <nav id="mf-rail" className="mf-rail" aria-label="Dashboard">
        <div className="mf-rail__head">
          <a className="mf-rail__brand" href="/app" aria-label="Micaforge, all sites">
            <Mark size={26} />
            <span className="mf-rail__wordmark">Micaforge</span>
          </a>
        </div>

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
            <span className="mf-topbar__channels-label">Menu</span>
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

          {/* Who is on the site right now: people and AI, always both. */}
          <div className="mf-live" aria-live="polite">
            <span className="mf-live__pair">
              <span className="mf-live__pen" data-pen="written" aria-hidden="true" />
              <span className="mf-live__n mf-data">{liveHumans}</span>
              <span className="mf-live__unit">people online</span>
            </span>
            <span className="mf-live__pair">
              <span className="mf-live__pen" data-pen="stepped" aria-hidden="true" />
              <span className="mf-live__n mf-data">{liveAgents}</span>
              <span className="mf-live__unit">AI crawlers</span>
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
          aria-label="Close menu"
          onClick={() => setOpen(false)}
        />
      )}
    </div>
  );
}

export default AppShell;
