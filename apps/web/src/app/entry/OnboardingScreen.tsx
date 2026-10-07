/**
 * First run for a brand-new site.
 *
 * Two steps, and the second one is the reason this product exists. Every other
 * analytics tool stops after "paste the snippet"; agents do not run JavaScript,
 * so a snippet alone records exactly half of what reaches the site. The second
 * step connects the server log, and until it is done the machine pen has
 * nothing to write with.
 *
 * Progress is measured, never claimed: each step is confirmed by a real read
 * against the API, polling while it waits and stopping the moment it lands. The
 * reader can leave at any point and the steps are still here when they return.
 */
import { useNavigate, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";
import { Button, CopyField, LivePulse, Panel, PanelHead, Readout } from "../../components/react/ui";
import { IconCheck, IconOut } from "../../components/react/icons";
import type { AgentOverview, Overview, SiteSummary } from "../../lib/api";
import { publicOrigin, api } from "../../lib/api";
import { num } from "../../lib/format";
import { ENTRY_PATHS, Waiting } from "./AuthFrame";
import "../../styles/app/entry.css";

const RANGE = "24h";
const POLL = 6_000;

export interface OnboardingScreenProps {
  /** Where "go to the record" lands. The record for this site, unless overridden. */
  recordPath?: (siteId: string | number) => string;
}

export function OnboardingScreen({ recordPath = ENTRY_PATHS.record }: OnboardingScreenProps) {
  const navigate = useNavigate();
  const params = useParams();
  const siteParam = params.site ?? "";

  const siteQuery = useQuery({
    queryKey: ["site", siteParam],
    queryFn: () => api.get<SiteSummary>(`/api/sites/${encodeURIComponent(siteParam)}`).then((r) => r.data),
    enabled: siteParam.length > 0,
  });

  const site = siteQuery.data;
  const siteId = site?.site_id ?? siteParam;

  // Step one: has any browser hit arrived at all?
  const humanQuery = useQuery({
    queryKey: ["stats", "overview", siteId, RANGE, "human", "[]"],
    queryFn: () =>
      api
        .get<Overview>("/api/stats/overview", { site: siteId, range: RANGE, audience: "human" as const })
        .then((r) => r.data),
    enabled: !!site,
    refetchInterval: (q) => ((q.state.data?.pageviews ?? 0) > 0 ? false : POLL),
  });

  // Step two: has any named agent fetch arrived from the server log?
  const agentQuery = useQuery({
    queryKey: ["stats", "agents/overview", siteId, RANGE, "agent", "[]"],
    queryFn: () =>
      api
        .get<AgentOverview>("/api/agents/overview", { site: siteId, range: RANGE, audience: "agent" as const })
        .then((r) => r.data),
    enabled: !!site,
    refetchInterval: (q) => ((q.state.data?.fetches ?? 0) > 0 ? false : POLL),
  });

  const pageviews = humanQuery.data?.pageviews ?? 0;
  const fetches = agentQuery.data?.fetches ?? 0;
  const humanDone = pageviews > 0;
  const agentDone = fetches > 0;

  const snippet = site
    ? `<script defer data-site="${site.site_id}" src="${publicOrigin()}/mf.js"></script>`
    : "";
  const logEndpoint = `${publicOrigin()}/api/log/edge`;

  if (!siteParam) {
    return (
      <div className="mf-stack mf-stack--task">
        <Panel>
          <PanelHead
            title="No site is named in this address"
            note="This page sets up one site, and the address you opened does not say which."
          />
          <div className="mf-panel-form">
            <div className="mf-form__foot">
              <Button tone="primary" onClick={() => navigate(ENTRY_PATHS.chooser)}>
                Choose a site
              </Button>
            </div>
          </div>
        </Panel>
      </div>
    );
  }

  if (siteQuery.isPending) {
    return (
      <div className="mf-stack mf-stack--task">
        <Panel ruled feed>
          <PanelHead title="Bring the site online" note="Reading the site record." />
          <div className="mf-panel-form">
            <Waiting label="Reading the site" lines={3} />
          </div>
        </Panel>
      </div>
    );
  }

  if (siteQuery.isError || !site) {
    return (
      <div className="mf-stack mf-stack--task">
        <Panel>
          <PanelHead
            title="That site could not be read"
            note={
              siteQuery.error instanceof Error
                ? siteQuery.error.message
                : "The server did not answer for this site."
            }
          />
          <div className="mf-panel-form">
            <div className="mf-form__foot">
              <Button onClick={() => siteQuery.refetch()}>Read it again</Button>
              <Button tone="quiet" onClick={() => navigate(ENTRY_PATHS.chooser)}>
                Back to your sites
              </Button>
            </div>
          </div>
        </Panel>
      </div>
    );
  }

  return (
    <div className="mf-stack mf-stack--task">
      <div className="mf-chooser__mast">
        <div>
          <h1 className="mf-chooser__title">Bring {site.domain} online</h1>
          <p className="mf-chooser__sub">
            The snippet records the people. The server log records the machines. Both pens need
            connecting before the record means anything, and the second one is the half nobody else
            measures.
          </p>
        </div>
      </div>

      <Panel ruled feed>
        <PanelHead
          title="What has arrived so far"
          note="The last 24 hours, refreshed while you wait."
          actions={humanDone && agentDone ? undefined : <LivePulse label="Listening" />}
        />

        <Readout
          channels={[
            {
              key: "human",
              label: "Pageviews recorded",
              value: humanQuery.isPending ? "—" : num(pageviews),
              note: humanDone ? "the snippet is live" : "waiting for the first hit",
            },
            {
              key: "agent",
              label: "Agent fetches recorded",
              value: agentQuery.isPending ? "—" : num(fetches),
              note: agentDone
                ? `${num(agentQuery.data?.unique_agents ?? 0)} agents named`
                : "waiting for the first line of log",
            },
          ]}
        />

        <section className="mf-step" data-done={humanDone}>
          <span className="mf-step__n mf-data" aria-hidden="true">
            1
          </span>
          <div className="mf-step__head">
            <h3 className="mf-step__title">Install the snippet</h3>
            <span className="mf-step__state">
              {humanDone ? (
                <>
                  <IconCheck size={13} />
                  {num(pageviews)} pageviews recorded
                </>
              ) : (
                <LivePulse label="Nothing has arrived yet" />
              )}
            </span>
          </div>
          <div className="mf-step__body">
            <p className="mf-step__hint">
              Paste this into the <code className="mf-data">&lt;head&gt;</code> of every page. It is
              under 3 kB, sets no cookie, and never blocks the page. A pageview normally shows up
              here within a couple of seconds of the first load.
            </p>
            <CopyField label="Tracking snippet" value={snippet} />
          </div>
        </section>

        <section className="mf-step" data-done={agentDone}>
          <span className="mf-step__n mf-data" aria-hidden="true">
            2
          </span>
          <div className="mf-step__head">
            <h3 className="mf-step__title">Connect the server log</h3>
            <span className="mf-step__state">
              {agentDone ? (
                <>
                  <IconCheck size={13} />
                  {num(fetches)} agent fetches recorded
                </>
              ) : (
                <LivePulse label="Nothing has arrived yet" />
              )}
            </span>
          </div>
          <div className="mf-step__body">
            <p className="mf-step__hint">
              AI crawlers do not run JavaScript, so the snippet cannot see them. Point your reverse
              proxy, CDN worker or log shipper at this endpoint and Micaforge will name the operator
              behind each fetch, check it against the ranges that operator publishes, and record what
              it took.
            </p>
            <CopyField label="Server log endpoint" value={logEndpoint} />
            <p className="mf-step__hint">
              Each line is signed with this site's ingest key. The key, and the ready-made
              configurations for the common proxies, are in this site's tracking settings.
            </p>
            {agentDone && (agentQuery.data?.top_operators?.length ?? 0) > 0 ? (
              <p className="mf-step__found">
                Named so far:{" "}
                {agentQuery.data!.top_operators.slice(0, 4).map(([operator, n], i) => (
                  <span key={operator}>
                    {i > 0 ? ", " : ""}
                    <span className="mf-step__op">{operator}</span>{" "}
                    <span className="mf-data">{num(n)}</span>
                  </span>
                ))}
              </p>
            ) : null}
            <a className="mf-entry__link mf-entry__link--sm" href={ENTRY_PATHS.tracking(site.site_id)}>
              Open tracking settings for the ingest key
              <IconOut size={13} />
            </a>
          </div>
        </section>

        <div className="mf-onb__foot">
          <p className="mf-onb__foot-note">
            You can leave either step for later. These instructions stay in this site's
            tracking settings, and this page keeps listening whenever you come back to it.
          </p>
          <div className="mf-form__foot">
            <Button tone={humanDone ? "primary" : "key"} onClick={() => navigate(recordPath(site.site_id))}>
              {humanDone ? "Go to the record" : "Skip for now"}
            </Button>
          </div>
        </div>
      </Panel>
    </div>
  );
}

export default OnboardingScreen;
