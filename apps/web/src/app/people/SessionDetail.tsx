/**
 * PEOPLE: one session, read off the paper.
 *
 * The hits are not a table. They are a strip of the record: a time rule down
 * the left with the hit marked on it, the clock time written in the margin, and
 * the interval between two hits written on the paper between them. The paper
 * advances further when more time passed, exactly as it would on the drum, so a
 * three-second bounce and an eleven-minute read do not look alike.
 */
import type { CSSProperties, ReactNode } from "react";
import { Fragment, useMemo } from "react";
import { Link, useParams } from "react-router";
import { useQuery } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import { bytes as fmtBytes, channelLabel, duration, num, purposeLabel } from "../../lib/format";
import { titleCase } from "../display";
import {
  Disclosure,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Readout,
  VerifiedMark,
} from "../../components/react/ui";
import type { Channel } from "../../components/react/ui";
import {
  IconChevron,
  IconDownload,
  IconErrors,
  IconEvents,
  IconOut,
  IconPages,
  IconPerformance,
  IconReplay,
  IconVisitors,
} from "../../components/react/icons";
import { useSite } from "../providers";
import {
  Hand,
  Subject,
  apiMessage,
  asRows,
  clockAt,
  countryName,
  deviceLabel,
  isAgentSession,
  stampAt,
  toMs,
  useBase,
} from "./SessionsScreen";
import type { SessionHit, SessionRow } from "./SessionsScreen";
import "../../styles/app/people.css";

/* ── the hit marks ──────────────────────────────────────────────────────── */

const MARKS: Record<string, (p: { size?: number }) => ReactNode> = {
  pageview: IconPages,
  custom: IconEvents,
  outbound: IconOut,
  file_download: IconDownload,
  form: IconEvents,
  error: IconErrors,
  web_vitals: IconPerformance,
  engagement: IconPerformance,
};

/**
 * The paper advance between two hits, as a 0..1 ratio the stylesheet turns into
 * a height. Logarithmic, because a session's gaps span three orders of
 * magnitude and a linear scale would leave every short gap invisible.
 */
const SESSION_GAP_S = 1800; // the 30-minute inactivity window that closes a session
function advance(gapS: number): number {
  if (!Number.isFinite(gapS) || gapS <= 0) return 0;
  const k = Math.log1p(gapS) / Math.log1p(SESSION_GAP_S);
  return Math.max(0, Math.min(1, k));
}

/** "+ 1m 12s", written the way an operator annotates a gap in the trace. */
function interval(gapS: number): string {
  if (!Number.isFinite(gapS)) return "";
  if (gapS < 1) return "+ under a second";
  return `+ ${duration(gapS)}`;
}

function hitTitle(hit: SessionHit): string {
  if (hit.event_name) return hit.event_name;
  if (hit.page_title) return hit.page_title;
  return hit.pathname || "/";
}

/* ── the screen ─────────────────────────────────────────────────────────── */

interface SessionDetailPayload {
  session?: SessionRow;
  hits?: SessionHit[];
}

export function SessionDetail() {
  const { id } = useParams();
  const base = useBase();
  const { siteId, timezone } = useSite();

  const q = useQuery({
    // A session is addressed by its own id, so the interval, audience and
    // filters that scope every list read have no part in this key.
    queryKey: ["stats", "session", siteId, id],
    queryFn: () => api.get<SessionDetailPayload>(`/api/stats/sessions/${encodeURIComponent(id ?? "")}`, { site: siteId as number }),
    enabled: siteId != null && !!id,
  });

  const payload = q.data?.data;
  const session = payload?.session;
  const hits = useMemo(() => {
    const raw = asRows<SessionHit>(payload, "hits");
    return [...raw].sort((a, b) => toMs(a.timestamp) - toMs(b.timestamp));
  }, [payload]);

  const machine = session ? isAgentSession(session) : false;
  const missing = q.isError && q.error instanceof ApiError && q.error.status === 404;

  const channels: Channel[] = session
    ? [
        { key: "duration", label: "Duration", value: duration(session.duration_s), note: "first hit to last" },
        { key: "pages", label: machine ? "Fetches" : "Pageviews", value: num(session.pageviews ?? 0) },
        { key: "events", label: "Hits recorded", value: num(session.events ?? hits.length) },
        ...(session.revenue != null
          ? [{ key: "revenue", label: "Revenue", value: `${session.revenue}`, note: session.currency ?? undefined }]
          : []),
      ]
    : [];

  return (
    <div className="mf-p-stack">
      <Link className="mf-p-back" to={`${base}/sessions`}>
        <IconChevron size={14} />
        All sessions
      </Link>

      {missing ? (
        <Panel>
          <Empty
            title="That session is not on the record"
            body="It was never written, or it has passed the retention window this install keeps. Session ids are not recoverable once the rows expire."
            action={
              <Link className="mf-btn mf-btn--key mf-btn--md" to={`${base}/sessions`}>
                <span className="mf-btn__label">Back to sessions</span>
              </Link>
            }
          />
        </Panel>
      ) : q.isError ? (
        <Panel>
          <ErrorState detail={apiMessage(q.error)} onRetry={() => q.refetch()} />
        </Panel>
      ) : (
        <>
          <Panel ruled feed>
            <Subject
              id={q.isPending ? <span className="mf-wait mf-p-wait-line" /> : (id ?? "")}
              when={session ? stampAt(session.started, timezone) : undefined}
              actions={
                session ? (
                  <>
                    {session.has_replay ? (
                      <Link
                        className="mf-btn mf-btn--key mf-btn--sm"
                        to={`${base}/replay/${encodeURIComponent(session.session_id)}`}
                      >
                        <span className="mf-btn__icon">
                          <IconReplay size={14} />
                        </span>
                        <span className="mf-btn__label">Watch the replay</span>
                      </Link>
                    ) : null}
                    {session.identified_id ? (
                      <Link
                        className="mf-btn mf-btn--key mf-btn--sm"
                        to={`${base}/visitors/${encodeURIComponent(session.identified_id)}`}
                      >
                        <span className="mf-btn__icon">
                          <IconVisitors size={14} />
                        </span>
                        <span className="mf-btn__label">{session.identified_id}</span>
                      </Link>
                    ) : null}
                  </>
                ) : null
              }
            />
            {channels.length ? <Readout channels={channels} dense /> : null}
          </Panel>

          <div className="mf-p-split">
            <Panel ruled>
              <PanelHead
                title={machine ? "Fetches" : "Hits"}
                note={
                  machine
                    ? "Every request this agent made, with the wait between them written on the paper."
                    : "Every hit in order, with the time between them written on the paper."
                }
              />
              {q.isPending ? (
                <TrailWaiting />
              ) : hits.length === 0 ? (
                <Empty
                  title="No hits are left for this session"
                  body="The session rollup survives longer than the individual hits on some installs. Widen this site's retention if you need the detail."
                />
              ) : (
                <Trail hits={hits} session={session} tz={timezone} />
              )}
            </Panel>

            <div className="mf-p-stack">
              <Panel>
                <PanelHead title="Arrival" />
                {q.isPending ? <FactsWaiting rows={6} /> : <ArrivalFacts session={session} />}
              </Panel>
              <Panel>
                <PanelHead title={machine ? "Client" : "Device"} />
                {q.isPending ? <FactsWaiting rows={6} /> : <ClientFacts session={session} machine={machine} />}
              </Panel>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

/* ── the trail ──────────────────────────────────────────────────────────── */

function Trail({
  hits,
  session,
  tz,
}: {
  hits: SessionHit[];
  session?: SessionRow;
  tz?: string;
}) {
  return (
    <ol className="mf-p-trail">
      {hits.map((hit, i) => {
        const prev = i > 0 ? toMs(hits[i - 1].timestamp) : Number.NaN;
        const gapS = i > 0 ? (toMs(hit.timestamp) - prev) / 1000 : Number.NaN;
        const kind = hit.kind ?? "pageview";
        const Mark = MARKS[kind] ?? IconEvents;
        const props = hit.props && typeof hit.props === "object" ? Object.entries(hit.props) : [];
        const query = hit.querystring ? `?${hit.querystring.replace(/^\?/, "")}` : "";
        const hash = hit.hash ? `#${hit.hash.replace(/^#/, "")}` : "";

        return (
          <li
            key={`${hit.timestamp}-${i}`}
            className="mf-p-trail__hit"
            data-kind={kind}
            data-still={Number.isFinite(gapS) && gapS >= 60 ? "true" : "false"}
          >
            {i > 0 ? (
              <span
                className="mf-p-trail__lead"
                style={{ "--mf-p-k": advance(gapS) } as CSSProperties}
              >
                <span className="mf-p-trail__int mf-data">{interval(gapS)}</span>
              </span>
            ) : null}

            <time className="mf-p-trail__t" dateTime={new Date(toMs(hit.timestamp)).toISOString()}>
              {clockAt(hit.timestamp, tz)}
            </time>
            <span className="mf-p-trail__mark" aria-hidden="true">
              <Mark size={16} />
            </span>

            <div className="mf-p-trail__body">
              <p className="mf-p-trail__title">{hitTitle(hit)}</p>
              {hit.pathname ? (
                <p className="mf-p-trail__path">
                  {hit.pathname}
                  {query}
                  {hash}
                </p>
              ) : null}
              <p className="mf-p-trail__meta">
                {i === 0 ? <span><b>Entry</b></span> : null}
                {i === hits.length - 1 && hits.length > 1 ? <span><b>Exit</b></span> : null}
                {kind !== "pageview" ? <span>{kindLabel(kind)}</span> : null}
                {hit.method ? <span className="mf-data">{hit.method}</span> : null}
                {hit.status ? <span className="mf-data">{hit.status}</span> : null}
                {hit.bytes ? <span className="mf-data">{fmtBytes(hit.bytes)}</span> : null}
                {hit.duration_ms ? <span>Engaged {duration(hit.duration_ms / 1000)}</span> : null}
                {hit.scroll_pct ? <span>Scrolled {hit.scroll_pct}%</span> : null}
                {hit.revenue != null ? <span>Revenue {hit.revenue}</span> : null}
              </p>
              {props.length ? (
                <Disclosure summary={`${num(props.length)} ${props.length === 1 ? "property" : "properties"}`}>
                  <dl className="mf-p-facts mf-p-facts--flush">
                    {props.map(([k, v]) => (
                      <Fragment key={k}>
                        <dt className="mf-data">{k}</dt>
                        <dd className="mf-data">{formatProp(v)}</dd>
                      </Fragment>
                    ))}
                  </dl>
                </Disclosure>
              ) : null}
            </div>
          </li>
        );
      })}

      <li className="mf-p-trail__end">
        <span className="mf-p-trail__end-label">
          Record ends
          {session?.exit_path ? (
            <>
              {" "}
              at <span className="mf-data">{session.exit_path}</span>
            </>
          ) : null}
          .
        </span>
      </li>
    </ol>
  );
}

const KINDS: Record<string, string> = {
  custom: "Custom event",
  outbound: "Outbound link",
  file_download: "File download",
  form: "Form submitted",
  error: "Error",
  web_vitals: "Web vitals",
  engagement: "Engagement",
};
const kindLabel = (k: string): string => KINDS[k] ?? k;

function formatProp(v: unknown): string {
  if (v == null) return "—";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

function TrailWaiting() {
  return (
    <div className="mf-p-trail" aria-busy="true" aria-live="polite">
      <span className="mf-p-sr">Reading the record…</span>
      {[0, 1, 2, 3, 4].map((i) => (
        <p key={i} className="mf-p-wait-row">
          <span className="mf-wait" style={{ width: `${44 + ((i * 17) % 40)}%` }} />
        </p>
      ))}
    </div>
  );
}

function FactsWaiting({ rows }: { rows: number }) {
  return (
    <div className="mf-p-facts" aria-busy="true">
      {Array.from({ length: rows }).map((_, i) => (
        <p key={i} className="mf-p-wait-row mf-p-wait-row--span">
          <span className="mf-wait" style={{ width: `${50 + ((i * 13) % 40)}%` }} />
        </p>
      ))}
    </div>
  );
}

/* ── the facts ──────────────────────────────────────────────────────────── */

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{children}</dd>
    </>
  );
}

function ArrivalFacts({ session }: { session?: SessionRow }) {
  if (!session) return <Empty title="Nothing was recorded about this arrival" />;
  const place = [session.city, session.region].filter(Boolean).join(", ");
  return (
    <dl className="mf-p-facts">
      <Fact label="Channel">{channelLabel(session.channel ?? "direct")}</Fact>
      {session.source ? <Fact label="Source">{titleCase(session.source)}</Fact> : null}
      {session.referrer_host ? (
        <Fact label="Referrer">
          <span className="mf-data">{session.referrer_host}</span>
        </Fact>
      ) : null}
      {session.utm_campaign ? <Fact label="Campaign">{session.utm_campaign}</Fact> : null}
      {session.utm_medium ? <Fact label="Medium">{session.utm_medium}</Fact> : null}
      {session.utm_source ? <Fact label="UTM source">{session.utm_source}</Fact> : null}
      {session.utm_term ? <Fact label="Term">{session.utm_term}</Fact> : null}
      {session.utm_content ? <Fact label="Content">{session.utm_content}</Fact> : null}
      {session.click_id ? (
        <Fact label="Click id">
          <span className="mf-data">{session.click_id}</span>
        </Fact>
      ) : null}
      <Fact label="Entry">
        <span className="mf-data">{session.entry_path ?? "—"}</span>
      </Fact>
      <Fact label="Exit">
        <span className="mf-data">{session.exit_path ?? "—"}</span>
      </Fact>
      <Fact label="Country">{countryName(session.country)}</Fact>
      {place ? <Fact label="Place">{place}</Fact> : null}
    </dl>
  );
}

function ClientFacts({ session, machine }: { session?: SessionRow; machine: boolean }) {
  if (!session) return <Empty title="Nothing was recorded about this client" />;

  if (machine) {
    return (
      <dl className="mf-p-facts">
        <Fact label="Hand">
          <Hand session={session} />
        </Fact>
        {session.operator ? <Fact label="Operator">{session.operator}</Fact> : null}
        {session.purpose ? <Fact label="Purpose">{purposeLabel(session.purpose)}</Fact> : null}
        <Fact label="Verified">
          <span className="mf-p-inline">
            <VerifiedMark
              state={session.verified === true || session.verified === 1 ? "verified" : "unverified"}
            />
            {session.verified === true || session.verified === 1
              ? "Address matched the operator's published ranges"
              : "No published range matched this address"}
          </span>
        </Fact>
        {session.user_agent ? (
          <Fact label="User agent">
            <span className="mf-p-facts__ua">{session.user_agent}</span>
          </Fact>
        ) : null}
      </dl>
    );
  }

  const screen = session.screen_w && session.screen_h ? `${session.screen_w} × ${session.screen_h}` : null;
  const viewport = session.viewport_w && session.viewport_h ? `${session.viewport_w} × ${session.viewport_h}` : null;

  return (
    <dl className="mf-p-facts">
      <Fact label="Hand">
        <Hand session={session} />
      </Fact>
      <Fact label="Device">{deviceLabel(session.device_type)}</Fact>
      <Fact label="Browser">
        {session.browser ?? "—"}
        {session.browser_version ? <span className="mf-p-dim"> {session.browser_version}</span> : null}
      </Fact>
      <Fact label="System">
        {session.os ?? "—"}
        {session.os_version ? <span className="mf-p-dim"> {session.os_version}</span> : null}
      </Fact>
      {screen ? (
        <Fact label="Screen">
          <span className="mf-data">{screen}</span>
        </Fact>
      ) : null}
      {viewport ? (
        <Fact label="Viewport">
          <span className="mf-data">{viewport}</span>
        </Fact>
      ) : null}
      {session.language ? <Fact label="Language">{session.language}</Fact> : null}
      {session.timezone ? <Fact label="Timezone">{session.timezone}</Fact> : null}
      {session.visitor_id ? (
        <Fact label="Visitor">
          <span className="mf-p-facts__ua">{session.visitor_id}</span>
        </Fact>
      ) : null}
    </dl>
  );
}

export default SessionDetail;
