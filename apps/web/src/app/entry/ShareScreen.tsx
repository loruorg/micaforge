/**
 * The public record at /share/:slug.
 *
 * Open-source projects publish their traffic as a trust signal, so this view is
 * a real dashboard rather than a screenshot: the same record, the same two pens,
 * read-only and with no station spine. It shows exactly what the link's owner
 * chose to publish and nothing else. Every panel is gated on the scope that
 * covers it, so a link scoped to the overview cannot be talked into rendering
 * the page list by editing the URL.
 *
 * Every read carries a share credential as `?share=` (docs/CONTRACT.md §3.4):
 * the slug itself for an open link, or the short-lived token a password unlock
 * hands back. It has to be in place before the first read leaves the browser;
 * see the note on setShareKey below. The server decides what each panel may
 * read; the gating here only avoids asking for what it will refuse.
 */
import type { FormEvent } from "react";
import { useEffect, useMemo, useState } from "react";
import { useParams } from "react-router";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Drum } from "../../components/react/Drum";
import type { Column } from "../../components/react/ui";
import {
  Button,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  RangePicker,
  Readout,
  Sheet,
} from "../../components/react/ui";
import { Mark } from "../../components/react/icons";
import type { AgentOverview, BreakdownRow, GapSummary, Overview, Series } from "../../lib/api";
import { ApiError, api, setShareKey } from "../../lib/api";
import { channelLabel, compact, delta, duration, num, pct, purposeLabel, shortPath } from "../../lib/format";
import { againstLeader, gapRatio, titleCase } from "../display";
import { AuthFrame, FormError, PasswordField, Waiting, formError } from "./AuthFrame";
import "../../styles/app/entry.css";

/* ── what a share link publishes ─────────────────────────────────────────── */

export interface ShareMeta {
  slug: string;
  /** A private link, or a site its owner made public. */
  kind: "link" | "public";
  /** Null until the reader may read it: a locked or expired link names no site. */
  site: { site_id: number; domain: string; name: string; timezone: string } | null;
  /** Empty is read as overview-only: a link never widens by saying nothing. */
  scopes: string[];
  password_required: boolean;
  /** True when this request's credential opens the link. */
  unlocked: boolean;
  expires_at: string | null;
  expired: boolean;
}

const SCOPE_OVERVIEW = "overview";
const SCOPE_PAGES = "pages";
const SCOPE_SOURCES = "sources";
const SCOPE_AGENTS = "agents";
const SCOPE_GAP = "citation_gap";

/** Hard bound on a displayed path: shortPath keeps the shape, not the length. */
function clip(v: string, max = 34): string {
  return v.length <= max ? v : `${v.slice(0, max - 1)}\u2026`;
}

function longDate(iso: string | null | undefined): string | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return null;
  return new Date(t).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function ShareScreen() {
  const params = useParams();
  const slug = params.slug ?? "";

  const [unlockToken, setUnlockToken] = useState<string | null>(null);
  const [range, setRange] = useState("30d");
  const [password, setPassword] = useState("");
  const [emptyPassword, setEmptyPassword] = useState(false);

  /**
   * The key has to be on the client BEFORE the first request is built, and a
   * query can fire in the same commit as this component's first render, so it
   * is set during render as well as in an effect. The effect re-arms it after
   * any teardown (React runs an extra mount/unmount cycle in development) and
   * clears it when the view is left, so the key never leaks into another view.
   */
  const shareToken = unlockToken ?? (slug || null);
  setShareKey(shareToken);
  useEffect(() => {
    setShareKey(shareToken);
    return () => setShareKey(null);
  }, [shareToken]);

  const meta = useQuery({
    queryKey: ["share", slug, unlockToken],
    queryFn: () => api.get<ShareMeta>(`/api/share/${encodeURIComponent(slug)}`).then((r) => r.data),
    enabled: slug.length > 0,
    retry: false,
  });

  const unlock = useMutation({
    mutationFn: (secret: string) =>
      api.post<{ token: string }>(`/api/share/${encodeURIComponent(slug)}/unlock`, { password: secret }).then((r) => r.data),
    onSuccess: (d) => {
      setPassword("");
      setUnlockToken(d.token);
    },
  });

  const info = meta.data;
  const metaError = meta.error;
  /* `unlocked` rather than "a token is held": a token that has lapsed comes back
     unlocked: false, and the reader is asked for the password again. */
  const locked =
    (metaError instanceof ApiError && (metaError.status === 401 || metaError.status === 403)) ||
    (!!info && !info.expired && !info.unlocked);
  const expired =
    !!info?.expired || (metaError instanceof ApiError && metaError.status === 410);
  const missing = metaError instanceof ApiError && metaError.status === 404;

  const scopes = useMemo(() => info?.scopes ?? [], [info]);
  const can = (scope: string) =>
    scopes.length === 0 ? scope === SCOPE_OVERVIEW : scopes.includes(scope);

  const siteId = info?.site?.site_id;
  const ready = !!siteId && !expired && !locked;
  const keyOf = (endpoint: string, audience: string) =>
    ["stats", endpoint, siteId ?? slug, range, audience, "[]"] as const;

  const overview = useQuery({
    queryKey: keyOf("overview", "human"),
    queryFn: () =>
      api
        .get<Overview>("/api/stats/overview", {
          site: siteId!,
          range,
          audience: "human" as const,
          compare: "previous" as const,
        })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_OVERVIEW),
  });

  const humanSeries = useQuery({
    queryKey: keyOf("timeseries", "human"),
    queryFn: () =>
      api
        .get<Series>("/api/stats/timeseries", {
          site: siteId!,
          range,
          audience: "human" as const,
          metric: "visitors",
        })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_OVERVIEW),
  });

  const agentSeries = useQuery({
    queryKey: keyOf("agents/timeseries", "agent"),
    queryFn: () =>
      api
        .get<Series>("/api/agents/timeseries", { site: siteId!, range, audience: "agent" as const })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_AGENTS),
  });

  const gap = useQuery({
    queryKey: keyOf("citation-gap/summary", "all"),
    queryFn: () =>
      api
        .get<GapSummary>("/api/citation-gap/summary", { site: siteId!, range, audience: "all" as const })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_GAP),
  });

  const pages = useQuery({
    queryKey: [...keyOf("breakdown", "human"), "pathname"],
    queryFn: () =>
      api
        .get<BreakdownRow[]>("/api/stats/breakdown", {
          site: siteId!,
          range,
          audience: "human" as const,
          dimension: "pathname",
          limit: 8,
        })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_PAGES),
  });

  const sources = useQuery({
    queryKey: [...keyOf("breakdown", "human"), "channel"],
    queryFn: () =>
      api
        .get<BreakdownRow[]>("/api/stats/breakdown", {
          site: siteId!,
          range,
          audience: "human" as const,
          dimension: "channel",
          limit: 8,
        })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_SOURCES),
  });

  const agents = useQuery({
    queryKey: keyOf("agents/overview", "agent"),
    queryFn: () =>
      api
        .get<AgentOverview>("/api/agents/overview", { site: siteId!, range, audience: "agent" as const })
        .then((r) => r.data),
    enabled: ready && can(SCOPE_AGENTS),
  });

  /* ── the ways in that are not a dashboard ─────────────────────────────── */

  if (!slug) {
    return (
      <AuthFrame
        title="That share link is incomplete"
        note="This address does not name a shared dashboard."
        aside={<p>A shared Micaforge dashboard lives at /share/ followed by its link name.</p>}
      >
        <p className="mf-form__body">
          Open the link you were given rather than retyping it, or ask whoever shared it to send it
          again.
        </p>
      </AuthFrame>
    );
  }

  if (meta.isPending) {
    return (
      <AuthFrame title="Opening the shared dashboard" note="Checking what this link publishes.">
        <Waiting label="Opening the shared dashboard" lines={3} />
      </AuthFrame>
    );
  }

  if (expired) {
    const when = longDate(info?.expires_at);
    return (
      <AuthFrame
        title="This link has expired"
        note={
          when
            ? `It stopped working on ${when}. Nothing is published at it any more.`
            : "It has passed its expiry date. Nothing is published at it any more."
        }
        aside={
          <p>
            A share link is set to expire by whoever created it. The dashboard itself is untouched; only
            this way in has closed.
          </p>
        }
      >
        <p className="mf-form__body">
          Ask whoever shared it for a fresh link. If it is your own site, create a new link in the
          site's share settings.
        </p>
      </AuthFrame>
    );
  }

  if (missing) {
    return (
      <AuthFrame
        title="Nothing is published here"
        note="No shared dashboard is using this link name."
        aside={<p>A share link that has been deleted stops resolving immediately, by design.</p>}
      >
        <p className="mf-form__body">
          Check the link for a missing character, or ask whoever shared it to send it again.
        </p>
      </AuthFrame>
    );
  }

  if (locked) {
    const submit = (e: FormEvent) => {
      e.preventDefault();
      if (unlock.isPending) return;
      if (!password) {
        setEmptyPassword(true);
        return;
      }
      setEmptyPassword(false);
      unlock.mutate(password);
    };
    const wrong =
      unlock.error instanceof ApiError &&
      (unlock.error.status === 401 || unlock.error.status === 403);

    return (
      <AuthFrame
        title="This dashboard is private"
        note={
          info?.site?.domain
            ? `The dashboard for ${info.site.domain} is behind a password. Whoever shared the link has it.`
            : "This dashboard is behind a password. Whoever shared the link has it."
        }
        aside={
          <p>
            The password protects the link, not an account. There is nothing to sign up for and
            nothing is stored in your browser beyond this session.
          </p>
        }
      >
        <form className="mf-form" onSubmit={submit} noValidate>
          {wrong ? (
            <FormError>That password did not open this link. Check it and try again.</FormError>
          ) : formError(unlock.error) ? (
            <FormError>{formError(unlock.error)}</FormError>
          ) : null}

          <div className="mf-form__fields">
            <PasswordField
              id="mf-share-password"
              label="Password"
              value={password}
              onChange={(v) => {
                setPassword(v);
                if (emptyPassword) setEmptyPassword(false);
              }}
              autoComplete="current-password"
              disabled={unlock.isPending}
              error={emptyPassword ? "Enter the password that came with the link." : undefined}
            />
          </div>

          <div className="mf-form__foot">
            <Button tone="primary" type="submit" disabled={unlock.isPending}>
              {unlock.isPending ? "Opening…" : "Open the dashboard"}
            </Button>
          </div>
        </form>
      </AuthFrame>
    );
  }

  if (meta.isError || !info || !info.site) {
    return (
      <AuthFrame
        title="The shared record could not be opened"
        note={formError(metaError) ?? "The server did not answer for this link."}
      >
        <div className="mf-form__foot">
          <Button tone="primary" onClick={() => meta.refetch()}>
            Try again
          </Button>
        </div>
      </AuthFrame>
    );
  }

  /* ── the record ───────────────────────────────────────────────────────── */

  const site = info.site;

  const times = humanSeries.data?.buckets.map((b) => b.t) ?? [];
  const agentByTime = new Map((agentSeries.data?.buckets ?? []).map((b) => [b.t, b.v]));
  const hourly = range === "24h" || range === "today" || range === "yesterday";
  const formatTime = (t: number) =>
    new Date(t).toLocaleString(undefined, hourly ? { hour: "2-digit", minute: "2-digit" } : { month: "short", day: "numeric" });

  const drumChannels = [
    ...(can(SCOPE_AGENTS)
      ? [
          {
            id: "machines",
            label: "Machines fetching",
            color: "var(--mf-ai)",
            values: times.map((t) => agentByTime.get(t) ?? null),
            pen: "stepped" as const,
            format: num,
          },
        ]
      : []),
    {
      id: "readers",
      label: "Readers",
      color: "var(--mf-people)",
      values: humanSeries.data?.buckets.map((b) => b.v) ?? [],
      pen: "written" as const,
      format: num,
    },
  ];

  const before = overview.data?.compare ?? null;

  /* A table of human traffic is inked in the reader's pen, not the machine's
     shellac, and the panels that carry one say so with mf-share--human. */
  const pageRows = pages.data ?? [];
  const sourceRows = sources.data ?? [];

  const pageColumns: Column<BreakdownRow>[] = [
    {
      key: "path",
      head: "Page",
      render: (r) => <span title={r.label || r.key}>{clip(shortPath(r.label || r.key, 34))}</span>,
      /* Read against the leading row of this sheet: shares of the whole draw
         the same short stub on every row of a typical table. */
      share: againstLeader(pageRows, (r) => r.share),
    },
    { key: "visitors", head: "Readers", numeric: true, render: (r) => <span className="mf-data">{num(r.visitors)}</span> },
    { key: "views", head: "Views", numeric: true, render: (r) => <span className="mf-data">{num(r.pageviews)}</span> },
  ];

  const sourceColumns: Column<BreakdownRow>[] = [
    {
      key: "channel",
      head: "Channel",
      render: (r) => <span>{channelLabel(r.key)}</span>,
      share: againstLeader(sourceRows, (r) => r.share),
    },
    { key: "visitors", head: "Readers", numeric: true, render: (r) => <span className="mf-data">{num(r.visitors)}</span> },
    { key: "share", head: "Share", numeric: true, render: (r) => <span className="mf-data">{pct(r.share)}</span> },
  ];

  const operatorRows = (agents.data?.top_operators ?? []).map(([operator, n]) => ({ operator, n }));
  const operatorColumns: Column<{ operator: string; n: number }>[] = [
    {
      key: "operator",
      head: "Operator",
      render: (r) => <span>{titleCase(r.operator)}</span>,
      share: (r) => (agents.data?.fetches ? r.n / agents.data.fetches : undefined),
    },
    { key: "fetches", head: "Fetches", numeric: true, render: (r) => <span className="mf-data">{num(r.n)}</span> },
  ];

  const gapReading = gap.data
    ? gapRatio(gap.data.agent_fetches, gap.data.answer_arrivals)
    : "—";

  return (
    <div className="mf-share">
      <header className="mf-share__head">
        <div className="mf-share__id">
          <Mark size={24} />
          <h1 className="mf-share__site">{site.domain}</h1>
          <span className="mf-stamp">{info.kind === "public" ? "Public record" : "Private link"}</span>
        </div>
        <div className="mf-share__tools">
          <RangePicker value={range} onChange={setRange} />
        </div>
      </header>

      <div className="mf-share__body mf-stack">
        {can(SCOPE_OVERVIEW) ? (
          <Panel ruled feed>
            <PanelHead
              title="Traffic"
              note={
                can(SCOPE_AGENTS)
                  ? "AI crawls above the line, people below it. The space between them is crawling that never sent a visitor back."
                  : "People over the chosen period. This link does not include AI crawler traffic."
              }
            />

            {overview.isError ? (
              <ErrorState
                title="The totals did not come back"
                detail={overview.error instanceof Error ? overview.error.message : undefined}
                onRetry={() => overview.refetch()}
              />
            ) : (
              <Readout
                channels={[
                  {
                    key: "visitors",
                    label: "Readers",
                    value: overview.isPending ? "—" : num(overview.data?.visitors ?? 0),
                    delta: overview.data ? delta(overview.data.visitors, before?.visitors) : null,
                  },
                  {
                    key: "sessions",
                    label: "Sessions",
                    value: overview.isPending ? "—" : num(overview.data?.sessions ?? 0),
                    delta: overview.data ? delta(overview.data.sessions, before?.sessions) : null,
                  },
                  {
                    key: "pageviews",
                    label: "Pageviews",
                    value: overview.isPending ? "—" : num(overview.data?.pageviews ?? 0),
                    delta: overview.data ? delta(overview.data.pageviews, before?.pageviews) : null,
                  },
                  {
                    key: "bounce",
                    label: "Bounce rate",
                    value: overview.isPending ? "—" : pct((overview.data?.bounce_rate ?? 0) / 100),
                    delta: overview.data ? delta(overview.data.bounce_rate, before?.bounce_rate) : null,
                    inverse: true,
                  },
                  {
                    key: "duration",
                    label: "Time on site",
                    value: overview.isPending ? "—" : duration(overview.data?.avg_duration_s ?? 0),
                    delta: overview.data ? delta(overview.data.avg_duration_s, before?.avg_duration_s) : null,
                  },
                ]}
              />
            )}

            <div className="mf-share__record">
              {humanSeries.isPending ? (
                <Waiting label="Reading the record" lines={4} />
              ) : humanSeries.isError ? (
                <ErrorState
                  title="The record did not come back"
                  detail={humanSeries.error instanceof Error ? humanSeries.error.message : undefined}
                  onRetry={() => humanSeries.refetch()}
                />
              ) : (
                <Drum
                  times={times}
                  channels={drumChannels}
                  mode={can(SCOPE_AGENTS) ? "mirrored" : "stacked"}
                  height={296}
                  formatTime={formatTime}
                  emptyLabel="No traffic in this period."
                />
              )}
            </div>
          </Panel>
        ) : null}

        {can(SCOPE_GAP) ? (
          <Panel ruled>
            <PanelHead
              title="Citation gap"
              note="How many agent fetches it took to bring one reader back from an answer engine."
            />
            {gap.isError ? (
              <ErrorState
                title="The gap did not come back"
                detail={gap.error instanceof Error ? gap.error.message : undefined}
                onRetry={() => gap.refetch()}
              />
            ) : (
              <>
                <Readout
                  channels={[
                    {
                      key: "taken",
                      label: "Agent fetches",
                      value: gap.isPending ? "—" : num(gap.data?.agent_fetches ?? 0),
                      note: "pages taken by machines",
                    },
                    {
                      key: "back",
                      label: "Answer arrivals",
                      value: gap.isPending ? "—" : num(gap.data?.answer_arrivals ?? 0),
                      note: "readers sent back",
                    },
                    {
                      key: "ratio",
                      label: "Taken per arrival",
                      value: gap.isPending ? "—" : gapReading,
                      note: "the gap",
                    },
                  ]}
                />
                {gap.data && gap.data.agent_fetches === 0 && gap.data.answer_arrivals === 0 ? (
                  <p className="mf-share__gap-note">
                    Neither pen has written over this interval. A gap appears once the server log is
                    connected and an answer engine sends its first reader.
                  </p>
                ) : null}
              </>
            )}
          </Panel>
        ) : null}

        <div className="mf-share__pair">
          {can(SCOPE_PAGES) ? (
            <Panel className="mf-share--human">
              <PanelHead title="Pages" note="Where the readers went." />
              {pages.isError ? (
                <ErrorState
                  title="The page list did not come back"
                  detail={pages.error instanceof Error ? pages.error.message : undefined}
                  onRetry={() => pages.refetch()}
                />
              ) : (
                <Sheet
                  rows={pageRows}
                  columns={pageColumns}
                  loading={pages.isPending}
                  getKey={(r) => r.key}
                  empty={
                    <Empty
                      title="No pages over this interval"
                      body="Nothing was read on this site in the interval you have selected. Widen the interval to look further back."
                    />
                  }
                />
              )}
            </Panel>
          ) : null}

          {can(SCOPE_SOURCES) ? (
            <Panel className="mf-share--human">
              <PanelHead title="Channels" note="Answer engines sit beside search, not inside it." />
              {sources.isError ? (
                <ErrorState
                  title="The channel list did not come back"
                  detail={sources.error instanceof Error ? sources.error.message : undefined}
                  onRetry={() => sources.refetch()}
                />
              ) : (
                <Sheet
                  rows={sourceRows}
                  columns={sourceColumns}
                  loading={sources.isPending}
                  getKey={(r) => r.key}
                  empty={
                    <Empty
                      title="No arrivals over this interval"
                      body="Nobody arrived at this site in the interval you have selected. Widen the interval to look further back."
                    />
                  }
                />
              )}
            </Panel>
          ) : null}
        </div>

        {can(SCOPE_AGENTS) ? (
          <Panel ruled>
            <PanelHead
              title="Machines"
              note="Named AI clients that fetched this site, checked against the address ranges their operators publish."
            />
            {agents.isError ? (
              <ErrorState
                title="The machine reading did not come back"
                detail={agents.error instanceof Error ? agents.error.message : undefined}
                onRetry={() => agents.refetch()}
              />
            ) : (
              <>
                <Readout
                  dense
                  channels={[
                    {
                      key: "fetches",
                      label: "Fetches",
                      value: agents.isPending ? "—" : num(agents.data?.fetches ?? 0),
                    },
                    {
                      key: "agents",
                      label: "Agents named",
                      value: agents.isPending ? "—" : num(agents.data?.unique_agents ?? 0),
                    },
                    {
                      key: "verified",
                      label: "Verified",
                      value: agents.isPending ? "—" : pct(agents.data?.verified_pct ?? 0),
                      note: "matched a published range",
                    },
                    {
                      key: "paths",
                      label: "Pages taken",
                      value: agents.isPending ? "—" : num(agents.data?.unique_paths ?? 0),
                    },
                  ]}
                />
                <Sheet
                  rows={operatorRows}
                  columns={operatorColumns}
                  loading={agents.isPending}
                  getKey={(r) => r.operator}
                  empty={
                    <Empty
                      title="No machine has fetched yet"
                      body="Agent traffic appears once this site's server log reaches Micaforge. Crawlers do not run JavaScript, so a tracking snippet alone cannot see them."
                    />
                  }
                />
                {(agents.data?.top_purposes?.length ?? 0) > 0 ? (
                  <p className="mf-share__scope-note">
                    Why they came:{" "}
                    {agents.data!.top_purposes.slice(0, 4).map(([purpose, n], i) => (
                      <span key={purpose}>
                        {i > 0 ? " · " : ""}
                        {purposeLabel(purpose)} <span className="mf-data">{compact(n)}</span>
                      </span>
                    ))}
                  </p>
                ) : null}
              </>
            )}
          </Panel>
        ) : null}
      </div>

      <footer className="mf-share__foot">
        <div className="mf-share__foot-inner">
          <span>
            Shared record for <span className="mf-data">{site.domain}</span>
          </span>
          <span aria-hidden="true">·</span>
          <span>
            Times in <span className="mf-data">{site.timezone}</span>
          </span>
          <span aria-hidden="true">·</span>
          <span>This link publishes only what its owner chose to publish.</span>
          <span aria-hidden="true">·</span>
          <a className="mf-entry__link mf-entry__link--sm" href="/">
            Recorded with Micaforge
          </a>
        </div>
      </footer>
    </div>
  );
}

export default ShareScreen;
