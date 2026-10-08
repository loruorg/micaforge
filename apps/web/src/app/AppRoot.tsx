/**
 * The station itself: the spine, the live readout, and the controls that belong
 * to the instrument rather than to any one sheet.
 *
 * What lives up here is exactly what is true no matter which channel you are
 * reading: the interval the caliper is set to, which pens are patched in, the
 * conditions clipped to the roll, and the way off the machine. Screens that
 * repeat one of those controls in their own masthead are not fighting this bar;
 * they drive the same settings, so moving either moves both.
 *
 * The live figures poll on their own clock and stop entirely when the tab is
 * hidden. Nobody's ClickHouse should be answering a question for a window
 * nobody is looking at.
 */
import type { ReactNode } from "react";
import { useEffect, useId, useMemo, useState } from "react";
import { Link, Outlet, useLocation } from "react-router";
import { useQuery } from "@tanstack/react-query";
import * as RPopover from "@radix-ui/react-popover";

import { api } from "../lib/api";
import type { Compare, Realtime } from "../lib/api";
import { AppShell } from "../components/react/AppShell";
import { AudienceSwitch, Button, RangePicker } from "../components/react/ui";
import { FilterBar } from "../components/react/FilterBar";
import { ExportMenu } from "../components/react/ExportMenu";
import { IconCaliper, IconChevron, IconSettings } from "../components/react/icons";
import { COMPARE_WORDS, rangeWords, useAppState } from "./state";
import { useAuth, useSite } from "./providers";

/* ── the tab's own attention ─────────────────────────────────────────────── */

/** True while this document is not the one on screen. */
function useHidden(): boolean {
  const [hidden, setHidden] = useState(
    () => typeof document !== "undefined" && document.visibilityState === "hidden",
  );
  useEffect(() => {
    const on = () => setHidden(document.visibilityState === "hidden");
    document.addEventListener("visibilitychange", on);
    return () => document.removeEventListener("visibilitychange", on);
  }, []);
  return hidden;
}

const LIVE_POLL_MS = 10_000;

/* ── the caliper, and what it is being measured against ──────────────────── */

/**
 * The interval control. The named stops sit on the caliper's jaws; a measured
 * interval is written on the two date fields beneath them, which is the only
 * honest way to ask for "the fortnight of the launch". The comparison lives in
 * the same slip because it is a property of the interval, not a separate idea.
 */
function IntervalKey() {
  const { range, setRange, customRange, setCustomRange, compare, setCompare } = useAppState();
  const [open, setOpen] = useState(false);
  const [start, setStart] = useState(customRange?.start ?? "");
  const [end, setEnd] = useState(customRange?.end ?? "");
  const startId = useId();
  const endId = useId();

  // Reopening the slip should show what is actually set, not what was typed and
  // abandoned the last time it was open.
  useEffect(() => {
    if (!open) return;
    setStart(customRange?.start ?? "");
    setEnd(customRange?.end ?? "");
  }, [open, customRange?.start, customRange?.end]);

  const bothWritten = start.length > 0 && end.length > 0;
  const backwards = bothWritten && start > end;
  const canMeasure = bothWritten && !backwards;

  const COMPARES: Compare[] = ["none", "previous", "year"];

  return (
    <RPopover.Root open={open} onOpenChange={setOpen}>
      <RPopover.Trigger asChild>
        <button type="button" className="mfs-key" aria-label={`Interval: ${rangeWords(range)}`}>
          <IconCaliper size={14} />
          <span className="mfs-key__label">{rangeWords(range)}</span>
          <IconChevron size={12} className="mfs-key__caret" />
        </button>
      </RPopover.Trigger>
      <RPopover.Portal>
        <RPopover.Content className="mfs-slip-panel" sideOffset={8} align="end" collisionPadding={12}>
          <div className="mfs-rack">
            <section className="mfs-rack__part">
              <h4 className="mfs-rack__head">Interval</h4>
              <RangePicker value={range} onChange={setRange} />
            </section>

            <section className="mfs-rack__part">
              <h4 className="mfs-rack__head">Or measure one off</h4>
              <div className="mfs-span">
                <div className="mfs-span__field">
                  <label className="mfs-build__lab" htmlFor={startId}>
                    From
                  </label>
                  <input
                    id={startId}
                    type="date"
                    className="mfs-input mf-data"
                    value={start}
                    max={end || undefined}
                    onChange={(e) => setStart(e.target.value)}
                  />
                </div>
                <div className="mfs-span__field">
                  <label className="mfs-build__lab" htmlFor={endId}>
                    To
                  </label>
                  <input
                    id={endId}
                    type="date"
                    className="mfs-input mf-data"
                    value={end}
                    min={start || undefined}
                    onChange={(e) => setEnd(e.target.value)}
                  />
                </div>
              </div>
              {backwards ? (
                <p className="mfs-say" data-tone="warn" role="alert">
                  The first date falls after the second. Swap them, or move one.
                </p>
              ) : null}
              <div className="mfs-rack__foot">
                <Button
                  size="sm"
                  tone="primary"
                  disabled={!canMeasure}
                  onClick={() => {
                    if (!canMeasure) return;
                    setCustomRange({ start, end });
                    setOpen(false);
                  }}
                >
                  Set the caliper
                </Button>
                {customRange ? (
                  <Button size="sm" tone="quiet" onClick={() => setCustomRange(null)}>
                    Back to a named interval
                  </Button>
                ) : null}
              </div>
            </section>

            <section className="mfs-rack__part">
              <h4 className="mfs-rack__head">Compare against</h4>
              <div className="mfs-bank" role="group" aria-label="Compare against">
                {COMPARES.map((c) => (
                  <button
                    key={c}
                    type="button"
                    className="mfs-bank__key"
                    aria-pressed={compare === c}
                    onClick={() => setCompare(c)}
                  >
                    {COMPARE_WORDS[c]}
                  </button>
                ))}
              </div>
            </section>
          </div>
        </RPopover.Content>
      </RPopover.Portal>
    </RPopover.Root>
  );
}

/* ── who is at the bench ─────────────────────────────────────────────────── */

/** The operator's own key: their account, the instance, and the way out. */
function AccountKey() {
  const { user, isSignedOut, signOut } = useAuth();
  const [open, setOpen] = useState(false);

  if (isSignedOut) {
    return (
      <Link className="mfs-key" to="/app/login">
        <span className="mfs-key__label">Sign in</span>
      </Link>
    );
  }

  /* A placeholder character is not an identity. Until the account read lands,
     the key is the settings glyph alone: never a literal question mark, which
     reads as a broken control rather than as a signed-in operator. */
  const who = (user?.name || user?.email || "").trim();
  const initial = who ? who.charAt(0).toUpperCase() : null;

  return (
    <RPopover.Root open={open} onOpenChange={setOpen}>
      <RPopover.Trigger asChild>
        <button
          type="button"
          className="mfs-key mfs-key--who"
          aria-label={user ? `Account: ${user.name || user.email}` : "Account"}
        >
          <IconSettings size={14} />
          {initial ? (
            <span className="mfs-key__label mfs-key__label--who">{initial}</span>
          ) : null}
        </button>
      </RPopover.Trigger>
      <RPopover.Portal>
        <RPopover.Content className="mfs-slip-panel mfs-menu" sideOffset={8} align="end" collisionPadding={12}>
          {user ? (
            <p className="mfs-menu__note">
              Signed in as <span className="mf-data">{user.email}</span>
            </p>
          ) : null}
          <Link className="mfs-menu__item" to="/app/settings" onClick={() => setOpen(false)}>
            <span className="mfs-menu__item-name">Your account</span>
            <span className="mfs-menu__item-note">Profile, sign-in, organisation, API keys.</span>
          </Link>
          <Link className="mfs-menu__item" to="/app/admin" onClick={() => setOpen(false)}>
            <span className="mfs-menu__item-name">This instance</span>
            <span className="mfs-menu__item-note">Storage, jobs and users on this server.</span>
          </Link>
          <button
            type="button"
            className="mfs-menu__item"
            onClick={() => {
              setOpen(false);
              void signOut();
            }}
          >
            <span className="mfs-menu__item-name">Sign out</span>
          </button>
        </RPopover.Content>
      </RPopover.Portal>
    </RPopover.Root>
  );
}

/* ── the bar in the top rail ─────────────────────────────────────────────── */

/**
 * What each channel already carries in its own masthead.
 *
 * Several screens were built with a caliper, an audience bank or an export key
 * of their own, sitting in the panel head where the figures they govern are.
 * Those are better placed than a control in the top rail, so the station does
 * not repeat them: two identical audience banks seventy pixels apart read as a
 * bug, not as a design. The station supplies only what the live channel is
 * missing, which is why the thirteen screens that carry nothing are still
 * fully operable.
 *
 * The record's own channel is the empty string, because its address is just
 * `/app/<site>`.
 */
const OWN_CALIPER = new Set([
  "",
  "pages",
  "sources",
  "locations",
  "devices",
  "agents",
  "citation-gap",
  "policy",
  "content-decay",
  "query",
]);
const OWN_AUDIENCE = new Set(["", "pages", "sources", "locations", "devices", "query"]);
/* The record no longer keeps an export key of its own. Two sibling screens put
   the same control in two places under two names, so the way off the machine is
   the station's everywhere: one key, in the top rail, reading "Export". The
   screens listed here still carry theirs because what they take is ONE sheet at
   the dimension it is currently tabulated by, which the station cannot name. */
const OWN_EXPORT = new Set(["pages", "sources", "locations", "devices", "query", "sessions"]);

/**
 * The wedge screens ARE the machines, and say so. Offering an audience switch
 * on them would invite a reading they do not answer.
 */
const NO_AUDIENCE = new Set(["agents", "citation-gap", "policy", "content-decay"]);

/** Setup is not a reading. None of the reading controls belong on it. */
const NOT_A_READING = new Set(["settings", "onboarding"]);

/** The channel a path is on: `/app/<site>/<channel>/…`, or "" for the record. */
export function channelOf(pathname: string): string {
  const parts = pathname.split("/").filter(Boolean);
  return parts.length >= 3 ? (parts[2] as string) : "";
}

/**
 * Which table the export key would take. The station bar cannot see what a
 * screen is showing, but it knows which channel is live, and that is the name
 * the server wants.
 */
const EXPORTABLE = new Set([
  "pages",
  "sources",
  "locations",
  "devices",
  "events",
  "goals",
  "funnels",
  "journeys",
  "retention",
  "performance",
  "errors",
  "sessions",
  "visitors",
  "agents",
  "citation-gap",
  "policy",
  "content-decay",
]);

const reportFor = (channel: string): string => (EXPORTABLE.has(channel) ? channel : "overview");

function StationControls({ hasSite, pathname }: { hasSite: boolean; pathname: string }) {
  const { audience, setAudience } = useAppState();
  const channel = channelOf(pathname);
  const reading = hasSite && !NOT_A_READING.has(channel);

  return (
    <>
      {reading ? (
        <>
          {OWN_AUDIENCE.has(channel) || NO_AUDIENCE.has(channel) ? null : (
            <AudienceSwitch value={audience} onChange={setAudience} />
          )}
          {OWN_CALIPER.has(channel) ? null : <IntervalKey />}
          <FilterBar />
          {OWN_EXPORT.has(channel) ? null : <ExportMenu report={reportFor(channel)} />}
        </>
      ) : null}
      <AccountKey />
    </>
  );
}

/* ── the station ─────────────────────────────────────────────────────────── */

export function AppRoot({ children }: { children?: ReactNode }) {
  const { siteId, domain, notFound, requested } = useSite();
  const { pathname } = useLocation();
  const hidden = useHidden();

  const live = useQuery({
    // The same key the live-pen screen reads on, so one poll feeds both rather
    // than two clocks asking the same question out of step.
    queryKey: ["stats", "realtime", siteId ?? null, "5m", "all", "[]"],
    enabled: siteId != null,
    queryFn: () =>
      api
        .get<Realtime>("/api/stats/realtime", { site: siteId as number, audience: "all" })
        .then((r) => r.data),
    refetchInterval: hidden ? false : LIVE_POLL_MS,
    refetchIntervalInBackground: false,
    staleTime: 0,
    retry: false,
  });

  return (
    <AppShell
      siteDomain={domain}
      liveHumans={live.data?.visitors_now ?? 0}
      liveAgents={live.data?.agents_now ?? 0}
      right={<StationControls hasSite={siteId != null} pathname={pathname} />}
    >
      {notFound ? (
        <div className="mf-empty mf-empty--error" role="alert">
          <h3 className="mf-empty__title">No site at this address</h3>
          <p className="mf-empty__body">
            Nothing on this account answers to <span className="mf-data">{requested}</span>. It may
            have been removed, or the address may name a site you cannot read.
          </p>
          <div className="mf-empty__action">
            <Link className="mf-btn mf-btn--key" to="/app">
              <span className="mf-btn__label">Choose a site</span>
            </Link>
          </div>
        </div>
      ) : (
        (children ?? <Outlet />)
      )}
    </AppShell>
  );
}

export default AppRoot;
