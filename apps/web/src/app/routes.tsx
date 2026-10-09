/**
 * The channel list, as an address table. CONTRACT §5, in full.
 *
 * Three things worth knowing before changing it:
 *
 *  1. THE FRAME IS PART OF THE ROUTE. The two auth screens and the public
 *     share view draw their own full-page sheet, so they are mounted OUTSIDE
 *     the station. Everything else is mounted inside `AppRoot`, which supplies
 *     the spine, the live readout and the shared controls.
 *  2. WHERE THE IMPORTS COME FROM. Areas with no lazy channel are pulled from
 *     their barrel, which is the door they publish. The four areas that DO have
 *     a lazy channel are imported screen by screen instead, because a barrel
 *     re-exports its whole area: statically importing one screen through it
 *     would drag the heavy sibling into the first chunk and quietly undo the
 *     split. The stylesheets those barrels used to carry are gathered in
 *     `styles/app/index.css`, which the providers pull in once.
 *  3. NOTHING WAITS ON A SPINNER. A screen that is still loading shows grey
 *     placeholder shapes in its own layout, already in the document and
 *     announced to a screen reader. ("Channel" in this file means "screen";
 *     the name is historical.)
 */
import type { ErrorInfo, ReactNode } from "react";
import { Component, Suspense, lazy } from "react";
import { Navigate, Outlet, Route, Routes, useLocation } from "react-router";

import { AppRoot } from "./AppRoot";
import { useAuth } from "./providers";
import { Button, ErrorState, Panel } from "../components/react/ui";

/* ── the way in, and the way out ─────────────────────────────────────────── */

import {
  AddSiteScreen,
  InvitationScreen,
  LoginScreen,
  OnboardingScreen,
  ShareScreen,
  SiteChooserScreen,
} from "./entry";

/* ── the record, the wedge, the audience, the health of it ───────────────── */

import { OverviewScreen, RealtimeScreen } from "./record";
import { AgentsScreen, CitationGapScreen, DecayScreen, PolicyScreen } from "./machines";
import { DevicesScreen, LocationsScreen, PagesScreen, SourcesScreen } from "./audience";
import { ErrorDetail, ErrorsScreen, PerformanceScreen } from "./health";
import { FrustrationScreen } from "./signals/FrustrationScreen";

/* ── areas holding a lazy channel: imported screen by screen ─────────────── */

import { EventsScreen } from "./behaviour/EventsScreen";
import { GoalsScreen } from "./behaviour/GoalsScreen";
import { FunnelsScreen } from "./behaviour/FunnelsScreen";
import { RetentionScreen } from "./behaviour/RetentionScreen";

import { SessionsScreen } from "./people/SessionsScreen";
import { SessionDetail } from "./people/SessionDetail";
import { VisitorsScreen } from "./people/VisitorsScreen";
import { VisitorDetail } from "./people/VisitorDetail";

import { SettingsLayout } from "./settings/SettingsLayout";
import { TrackingSetup } from "./settings/TrackingSetup";
import { SiteSettings } from "./settings/SiteSettings";
import { AlertsSettings } from "./settings/AlertsSettings";
import { ReportsSettings } from "./settings/ReportsSettings";
import { ImportsSettings } from "./settings/ImportsSettings";
import { ShareSettings } from "./settings/ShareSettings";
import { DangerZone } from "./settings/DangerZone";
import { LogDrainsSettings } from "./settings/LogDrainsSettings";
import { AccountSettings } from "./settings/AccountSettings";
import { OrgSettings } from "./settings/OrgSettings";
import { TeamsSettings } from "./settings/TeamsSettings";
import { ApiKeysSettings } from "./settings/ApiKeysSettings";
import { AuditLogSettings } from "./settings/AuditLogSettings";
import { BillingSettings } from "./settings/BillingSettings";
import { IngestGuards } from "./settings/IngestGuards";

/* ── the OAuth consent sheet: outside the frame, behind sign-in ──────────── */

import { ConsentScreen } from "./oauth/ConsentScreen";

/* ── the heavy channels ──────────────────────────────────────────────────── */

const JourneysScreen = lazy(() => import("./behaviour/JourneysScreen"));
const ReplayScreen = lazy(() => import("./people/ReplayScreen"));
const ReplayPlayer = lazy(() => import("./people/ReplayPlayer"));
const DashboardsScreen = lazy(() => import("./bench/DashboardsScreen"));
const DashboardEditor = lazy(() => import("./bench/DashboardEditor"));
const QueryScreen = lazy(() => import("./bench/QueryScreen"));
const FlagsScreen = lazy(() => import("./bench/FlagsScreen"));
const ExperimentsScreen = lazy(() => import("./bench/ExperimentsScreen"));
const AdminScreen = lazy(() => import("./settings/AdminScreen"));
const AskScreen = lazy(() => import("./ask/AskScreen"));
const InsightsScreen = lazy(() => import("./insights/InsightsScreen"));
const InsightDetail = lazy(() => import("./insights/InsightDetail"));
/* Settings that are opened once, to connect something, not every day. */
const ChatAppsSettings = lazy(() => import("./settings/ChatAppsSettings"));
const AiInsightsSettings = lazy(() => import("./settings/AiInsightsSettings"));
const ConnectedAppsSettings = lazy(() => import("./settings/ConnectedAppsSettings"));
const GitHubSettings = lazy(() => import("./settings/GitHubSettings"));

/* ── monitoring and growth: each screen its own chunk ────────────────────── */

const UptimeScreen = lazy(() => import("./monitoring/UptimeScreen"));
const StatusPagesScreen = lazy(() => import("./monitoring/StatusPagesScreen"));
const LinksScreen = lazy(() => import("./growth/LinksScreen"));
const RevenueScreen = lazy(() => import("./growth/RevenueScreen"));
const SearchScreen = lazy(() => import("./search/SearchScreen"));

/* ── MCP server analytics: its own chunk ─────────────────────────────────── */

const McpScreen = lazy(() => import("./mcp/McpScreen"));

/* ── waiting ─────────────────────────────────────────────────────────────── */

/**
 * Grey placeholder shapes in the layout of a screen: a header, a chart and
 * some rows. The same shapes every other loading state in this product uses,
 * so a screen arriving looks like data loading rather than like a different
 * application.
 */
export function ScreenWait({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="mfs-wait-screen" aria-busy="true" aria-live="polite">
      <span className="mf-sr">{label}</span>
      <Panel>
        <div className="mfs-wait-screen__head">
          <span className="mf-wait" style={{ width: "34%", height: "17px" }} />
          <span className="mf-wait" style={{ width: "52%" }} />
        </div>
        <div className="mfs-wait-screen__plot">
          {[62, 41, 78, 55, 33, 69, 47].map((h, i) => (
            <span key={i} className="mf-wait" style={{ height: `${h}%`, width: "9px" }} />
          ))}
        </div>
      </Panel>
      <Panel>
        <div className="mfs-wait-screen__rows">
          {[74, 58, 66, 43, 51].map((w, i) => (
            <span key={i} className="mf-wait" style={{ width: `${w}%` }} />
          ))}
        </div>
      </Panel>
    </div>
  );
}

/* ── a channel that fails on its own ─────────────────────────────────────── */

/**
 * True when the failure is a channel that never arrived, rather than a bug in
 * one that did. It is the commonest failure a split application has in the
 * field: the page was loaded before a deploy, the chunk it names is gone, and
 * the import rejects. It matters because the recovery is different; see below.
 */
function isMissingChannel(error: Error): boolean {
  const m = `${error.name} ${error.message}`;
  return (
    /dynamically imported module|Importing a module script failed|error loading dynamically imported/i.test(m) ||
    error.name === "ChunkLoadError"
  );
}

/**
 * Keeps one channel's failure inside that channel.
 *
 * The spine and the top rail are not part of the channel that broke, so they
 * stay: the operator can walk to another channel instead of being handed a
 * whole dead page. `resetKey` is the address, so navigating anywhere clears the
 * failure without a reload.
 *
 * The recovery offered is the one that actually works. Rendering again fixes a
 * screen that threw while drawing; it CANNOT fix a channel whose code never
 * arrived, because a rejected lazy import is remembered and re-rendering
 * re-throws the same rejection forever. Offering "try again" there would be a
 * button that does nothing, so that case asks for the reload it genuinely needs.
 */
class ChannelBoundary extends Component<
  { children: ReactNode; resetKey: string },
  { error: Error | null }
> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidUpdate(prev: { resetKey: string }) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null });
    }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[micaforge] channel failed", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;

    const missing = isMissingChannel(error);
    return (
      <div className="mfs-wait-screen">
        <Panel>
          <ErrorState
            title={missing ? "This screen did not load" : "This screen stopped working"}
            detail={
              missing
                ? "The dashboard asked for this screen's code and the server did not send it. Usually the page was open during an update and asks for a file that was replaced. Reload to get the current version."
                : `${error.message || "The screen failed while it was being drawn."} Trying again often clears it. The full error is in the browser console.`
            }
            onRetry={missing ? undefined : () => this.setState({ error: null })}
          />
          <div className="mfs-rack__foot mfs-rack__foot--centre">
            {missing ? (
              <Button
                tone="primary"
                onClick={() => {
                  window.location.reload();
                }}
              >
                Reload the dashboard
              </Button>
            ) : null}
          </div>
        </Panel>
      </div>
    );
  }
}

function Waited({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  return (
    <ChannelBoundary resetKey={pathname}>
      <Suspense fallback={<ScreenWait />}>{children}</Suspense>
    </ChannelBoundary>
  );
}

/* ── the gate ────────────────────────────────────────────────────────────── */

/**
 * Sends an unauthenticated reader to the sign-in sheet, and ONLY then.
 *
 * A server that is unreachable, or one whose account endpoint is broken, is a
 * different problem with a different answer, and bouncing someone to a login
 * form because the machine is down tells them a lie about what is wrong. Those
 * cases fall through to the screens, which report what actually failed.
 */
function RequireAccount({ children }: { children: ReactNode }) {
  const { isLoading, isSignedOut } = useAuth();
  const location = useLocation();

  if (isLoading) return <ScreenWait label="Checking your account…" />;
  if (isSignedOut) {
    return <Navigate to="/app/login" replace state={{ from: location.pathname + location.search }} />;
  }
  return <>{children}</>;
}

/* ── an address that has moved ───────────────────────────────────────────── */

/**
 * Sends an address that used to exist to the one that does now, keeping the
 * query string when asked to. Links outlive routing decisions: a signup or
 * reset link in an old email, a bookmark, an invitation mailed before its
 * screen moved. Each one should land somewhere that still works, not on a
 * "no channel here" sheet.
 */
function Moved({ to, keepQuery = false }: { to: string; keepQuery?: boolean }) {
  const { search } = useLocation();
  return <Navigate to={keepQuery ? `${to}${search}` : to} replace />;
}

/* ── an address that is not a channel ────────────────────────────────────── */

function NotFound() {
  return (
    <div className="mfs-wait-screen">
      <Panel>
        <ErrorState
          title="There is no page at this address"
          detail="The link may be from an older build of the dashboard, or the site it pointed at has been removed."
        />
        <div className="mfs-rack__foot mfs-rack__foot--centre">
          <Button onClick={() => { window.location.href = "/app"; }}>Back to your sites</Button>
        </div>
      </Panel>
    </div>
  );
}

/* ── the table ───────────────────────────────────────────────────────────── */

export function AppRoutes() {
  return (
    <Routes>
      {/* The entry sheets draw their own page; no spine, no readout. */}
      <Route path="/app/login" element={<LoginScreen />} />
      <Route path="/app/invitation" element={<InvitationScreen />} />
      <Route path="/app/invitation/:token" element={<InvitationScreen />} />

      {/* Sign-in is by emailed code, so signing up, recovering and resetting
          are all signing in. Their old addresses, and the verification links
          older builds mailed, land there. Invitation links mailed under the old
          path keep their token. */}
      <Route path="/app/signup" element={<Moved to="/app/login" />} />
      <Route path="/app/forgot-password" element={<Moved to="/app/login" />} />
      <Route path="/app/reset-password" element={<Moved to="/app/login" />} />
      <Route path="/app/reset-password/:token" element={<Moved to="/app/login" />} />
      <Route path="/app/verify-email" element={<Moved to="/app/login" />} />
      <Route path="/app/invite" element={<Moved to="/app/invitation" keepQuery />} />

      {/* The public record. No account, no station chrome. */}
      <Route path="/share/:slug" element={<ShareScreen />} />

      {/* The OAuth consent sheet draws its own page like the entry sheets, but
          only for a signed-in reader: the gate sends anyone else to sign in and
          back here with the full query string. */}
      <Route
        path="/app/oauth/authorize"
        element={
          <RequireAccount>
            <ConsentScreen />
          </RequireAccount>
        }
      />

      {/* Everything else is read at the station. */}
      <Route
        path="/app"
        element={
          <RequireAccount>
            <AppRoot>
              <Waited>
                <Outlet />
              </Waited>
            </AppRoot>
          </RequireAccount>
        }
      >
        <Route index element={<SiteChooserScreen />} />
        <Route path="new" element={<AddSiteScreen />} />

        {/* The account, which belongs to the person rather than to a site. */}
        <Route path="settings" element={<SettingsLayout scope="account" />}>
          <Route index element={<AccountSettings />} />
          <Route path="organisation" element={<OrgSettings />} />
          <Route path="teams" element={<TeamsSettings />} />
          <Route path="keys" element={<ApiKeysSettings />} />
          <Route path="connected-apps" element={<ConnectedAppsSettings />} />
          <Route path="audit" element={<AuditLogSettings />} />
          <Route path="billing" element={<BillingSettings />} />
        </Route>

        <Route path="admin" element={<AdminScreen />} />

        {/* One site's record. */}
        <Route path=":site">
          <Route index element={<OverviewScreen />} />
          <Route path="onboarding" element={<OnboardingScreen />} />
          <Route path="realtime" element={<RealtimeScreen />} />

          {/* the wedge */}
          <Route path="agents" element={<AgentsScreen />} />
          <Route path="citation-gap" element={<CitationGapScreen />} />
          <Route path="policy" element={<PolicyScreen />} />
          <Route path="content-decay" element={<DecayScreen />} />

          {/* the reader's side */}
          <Route path="pages" element={<PagesScreen />} />
          <Route path="sources" element={<SourcesScreen />} />
          <Route path="locations" element={<LocationsScreen />} />
          <Route path="devices" element={<DevicesScreen />} />

          {/* behaviour */}
          <Route path="events" element={<EventsScreen />} />
          <Route path="goals" element={<GoalsScreen />} />
          <Route path="funnels" element={<FunnelsScreen />} />
          <Route path="journeys" element={<JourneysScreen />} />
          <Route path="retention" element={<RetentionScreen />} />

          {/* health */}
          <Route path="performance" element={<PerformanceScreen />} />
          <Route path="errors" element={<ErrorsScreen />} />
          <Route path="errors/:fingerprint" element={<ErrorDetail />} />
          <Route path="frustration" element={<FrustrationScreen />} />

          {/* people */}
          <Route path="sessions" element={<SessionsScreen />} />
          <Route path="sessions/:id" element={<SessionDetail />} />
          <Route path="visitors" element={<VisitorsScreen />} />
          <Route path="visitors/:id" element={<VisitorDetail />} />
          <Route path="replay" element={<ReplayScreen />} />
          <Route path="replay/:id" element={<ReplayPlayer />} />

          {/* the bench */}
          <Route path="flags" element={<FlagsScreen />} />
          <Route path="experiments" element={<ExperimentsScreen />} />
          <Route path="dashboards" element={<DashboardsScreen />} />
          <Route path="dashboards/:id" element={<DashboardEditor />} />
          <Route path="query" element={<QueryScreen />} />

          {/* monitoring and growth */}
          <Route path="uptime" element={<UptimeScreen />} />
          <Route path="status-pages" element={<StatusPagesScreen />} />
          <Route path="links" element={<LinksScreen />} />
          <Route path="revenue" element={<RevenueScreen />} />
          <Route path="search" element={<SearchScreen />} />

          {/* MCP server analytics */}
          <Route path="mcp" element={<McpScreen />} />

          {/* this site's setup */}
          <Route path="settings" element={<SettingsLayout scope="site" />}>
            <Route index element={<TrackingSetup />} />
            {/* The address onboarding and older links use for the same screen. */}
            <Route path="tracking" element={<TrackingSetup />} />
            <Route path="site" element={<SiteSettings />} />
            <Route path="alerts" element={<AlertsSettings />} />
            <Route path="reports" element={<ReportsSettings />} />
            <Route path="imports" element={<ImportsSettings />} />
            <Route path="share" element={<ShareSettings />} />
            <Route path="danger" element={<DangerZone />} />
            <Route path="chat" element={<ChatAppsSettings />} />
            <Route path="ai" element={<AiInsightsSettings />} />
            <Route path="guards" element={<IngestGuards />} />
            <Route path="log-drains" element={<LogDrainsSettings />} />
            <Route path="github" element={<GitHubSettings />} />
          </Route>

          {/* the AI analyst, and what it found. One route for Ask AI with an
              optional conversation, so opening the conversation a new question
              just started keeps the screen, and its answer in flight, mounted. */}
          <Route path="ask/:conversation?" element={<AskScreen />} />
          <Route path="insights" element={<InsightsScreen />} />
          <Route path="insights/:id" element={<InsightDetail />} />

          <Route path="*" element={<NotFound />} />
        </Route>

        <Route path="*" element={<NotFound />} />
      </Route>

      {/* Anything outside /app and /share belongs to the static site. */}
      <Route path="*" element={<Navigate to="/app" replace />} />
    </Routes>
  );
}

export default AppRoutes;
