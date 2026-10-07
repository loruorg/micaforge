/**
 * Events: the register of everything the site reported that was not a plain
 * pageview, and the tape of what is arriving right now.
 *
 * Two readings, in the order an operator actually wants them:
 *
 *   1. THE REGISTER. Every event name with its count and the number of distinct
 *      visitors behind it. Opening a row reads the event's property keys off the
 *      record and the top values for each key. Properties are fetched only when
 *      a row is opened, because a site with forty event names has hundreds of
 *      property keys and none of them are worth a request until asked for.
 *   2. THE TAPE. The live log. It runs on its own, it can be held so a row can
 *      be read without the list moving under the pointer, and any row opens to
 *      show the raw props exactly as they were received.
 *
 * Nothing here invents a figure. With no traffic the register says so and names
 * what would fill it.
 */
import { useEffect, useRef, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api, type Realtime } from "../../lib/api";
import { useAppState } from "../state";
import { useSite } from "../providers";
import { Button, Empty, ErrorState, Panel, PanelHead, Readout } from "../../components/react/ui";
import { IconChevron } from "../../components/react/icons";
import { ago, num, pct, shortPath } from "../../lib/format";
import { agentName, titleCase } from "../display";

/* ── what the record returns ─────────────────────────────────────────────── */

interface EventRow {
  name: string;
  /** pageview | custom | outbound | file_download | form | error | … */
  kind: string;
  count: number;
  visitors: number;
  sessions: number;
  share: number;
  last_seen: number | null;
  property_keys: number;
}

interface EventsPayload {
  total: number;
  distinct_names: number;
  visitors: number;
  per_session: number;
  rows: EventRow[];
}

interface PropertyValue {
  value: string;
  count: number;
  share: number;
}

interface PropertyRow {
  key: string;
  distinct: number;
  values: PropertyValue[];
}

interface EventPropsPayload {
  event: string;
  total: number;
  properties: PropertyRow[];
}

/** The live tape carries the same rows realtime does, plus the raw props. */
type TapeRow = Realtime["recent"][number] & {
  event_id?: string;
  name?: string;
  props?: Record<string, unknown> | null;
};
type TapePayload = Omit<Realtime, "recent"> & { recent: TapeRow[] };

/* ── the register ────────────────────────────────────────────────────────── */

export function EventsScreen() {
  const { siteId, domain } = useSite();
  const { range, audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);
  const [open, setOpen] = useState<string | null>(null);

  const events = useQuery({
    queryKey: ["stats", "events", siteId, range, audience, filtersJson],
    queryFn: () =>
      api.get<EventsPayload>("/api/stats/events", {
        site: siteId as number,
        range,
        audience,
        filters,
        limit: 100,
      }),
    enabled: siteId != null,
    placeholderData: keepPreviousData,
  });

  const payload = events.data?.data;
  const rows = payload?.rows ?? [];

  return (
    <div className="mfb-stack">
      <Panel ruled feed>
        <PanelHead
          title="Events"
          note={
            domain
              ? `Everything ${domain} reported that was not a plain pageview.`
              : "Everything this site reported that was not a plain pageview."
          }
        />

        <Readout
          channels={[
            { key: "total", label: "Events recorded", value: num(payload?.total) },
            { key: "names", label: "Distinct names", value: num(payload?.distinct_names) },
            {
              key: "visitors",
              label: "Visitors",
              value: num(payload?.visitors),
              note: "fired an event",
            },
            {
              key: "rate",
              label: "Per session",
              value:
                payload?.per_session == null ? "—" : payload.per_session.toFixed(2),
            },
          ]}
        />

        {events.isError ? (
          <ErrorState
            detail={errorText(events.error)}
            onRetry={() => void events.refetch()}
          />
        ) : events.isPending ? (
          <RegisterSkeleton />
        ) : rows.length === 0 ? (
          <Empty
            title="No events on this record"
            body={
              <>
                Events appear here as soon as the tracker reports one. Call{" "}
                <code className="mf-data">micaforge.track("signup")</code> from your site, or
                switch on outbound-link, download and form capture in tracking settings.
              </>
            }
          />
        ) : (
          <div className="mfb-reg mfb-reg--events">
            <div className="mfb-reg__head">
              <span aria-hidden="true" />
              <span className="mfb-reg__h">Event</span>
              <span className="mfb-reg__h--num">Count</span>
              <span className="mfb-reg__h--num mfb-col-3">Visitors</span>
              <span className="mfb-reg__h--num mfb-col-2">Share</span>
            </div>

            {rows.map((row) => (
              <EventEntry
                key={row.name}
                row={row}
                open={open === row.name}
                onToggle={() => setOpen((v) => (v === row.name ? null : row.name))}
              />
            ))}
          </div>
        )}
      </Panel>

      <LiveTape />
    </div>
  );
}

function EventEntry({
  row,
  open,
  onToggle,
}: {
  row: EventRow;
  open: boolean;
  onToggle: () => void;
}) {
  const bodyId = `mfb-ev-${row.name.replace(/[^a-z0-9_-]/gi, "-")}`;
  return (
    <>
      <button
        type="button"
        className="mfb-reg__row"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={onToggle}
      >
        <span
          className="mfb-reg__ink"
          style={{ width: `${Math.max(0, Math.min(1, row.share)) * 100}%` }}
          aria-hidden="true"
        />
        <IconChevron size={14} className="mfb-reg__caret" />
        <span className="mfb-reg__name">
          <span className="mfb-reg__title">{row.name || "(unnamed)"}</span>
          <span className="mfb-reg__sub">
            {row.kind}
            {row.property_keys > 0
              ? ` · ${row.property_keys} propert${row.property_keys === 1 ? "y" : "ies"}`
              : " · no properties"}
            {row.last_seen ? ` · last ${ago(row.last_seen)}` : ""}
          </span>
        </span>
        <span className="mfb-reg__fig">{num(row.count)}</span>
        <span className="mfb-reg__fig mfb-reg__fig--dim mfb-col-3">{num(row.visitors)}</span>
        <span className="mfb-reg__fig mfb-reg__fig--dim mfb-col-2">{pct(row.share)}</span>
      </button>

      {open ? <EventProperties id={bodyId} event={row.name} /> : null}
    </>
  );
}

/** Property keys and their top values. Requested only once a row is opened. */
function EventProperties({ id, event }: { id: string; event: string }) {
  const { siteId } = useSite();
  const { range, audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);

  const q = useQuery({
    queryKey: ["stats", "events", siteId, range, audience, filtersJson, "properties", event],
    queryFn: () =>
      api.get<EventPropsPayload>("/api/stats/events", {
        site: siteId as number,
        range,
        audience,
        filters,
        event,
        properties: 1,
        limit: 8,
      }),
    enabled: siteId != null,
  });

  const properties = q.data?.data.properties ?? [];

  return (
    <div className="mfb-reg__body" id={id}>
      {q.isError ? (
        <ErrorState
          title="Those properties did not come back"
          detail={errorText(q.error)}
          onRetry={() => void q.refetch()}
        />
      ) : q.isPending ? (
        <p className="mfb-note" aria-live="polite">
          Reading the properties of {event}…
        </p>
      ) : properties.length === 0 ? (
        <p className="mfb-note">
          {event} carries no properties. Pass an object as the second argument to{" "}
          <code className="mf-data">track()</code> and its keys are broken down here.
        </p>
      ) : (
        <div className="mfb-props">
          {properties.map((p) => (
            <div className="mfb-prop" key={p.key}>
              <div className="mfb-prop__head">
                <span className="mfb-prop__key">{p.key}</span>
                <span className="mfb-prop__distinct">
                  {num(p.distinct)} distinct value{p.distinct === 1 ? "" : "s"}
                </span>
              </div>
              <div>
                {p.values.map((v) => (
                  <div className="mfb-prop__val" key={v.value}>
                    <span
                      className="mfb-prop__ink"
                      style={{ width: `${Math.max(0, Math.min(1, v.share)) * 100}%` }}
                      aria-hidden="true"
                    />
                    <span className="mfb-prop__vname" title={v.value}>
                      {v.value === "" ? "(empty)" : v.value}
                    </span>
                    <span className="mfb-prop__vfig">{num(v.count)}</span>
                    <span className="mfb-prop__vfig mfb-prop__vfig--dim">{pct(v.share)}</span>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/* ── the tape ────────────────────────────────────────────────────────────── */

function LiveTape() {
  const { siteId } = useSite();
  const { audience, filters } = useAppState();
  const filtersJson = JSON.stringify(filters ?? []);
  const [held, setHeld] = useState(false);
  const [open, setOpen] = useState<string | null>(null);

  const q = useQuery({
    // The tape does not read the caliper: it is always the last five minutes.
    queryKey: ["stats", "realtime", siteId, "5m", audience, filtersJson],
    queryFn: () =>
      api.get<TapePayload>("/api/stats/realtime", {
        site: siteId as number,
        audience,
        filters,
        limit: 40,
      }),
    enabled: siteId != null,
    refetchInterval: held ? false : 5000,
    placeholderData: keepPreviousData,
  });

  const rows = q.data?.data.recent ?? [];
  const fresh = useFreshKeys(rows.map(rowKey), held);

  return (
    <Panel>
      <PanelHead
        title="Live tape"
        note="The last five minutes, as it is written. Hold the tape to read a row without the list moving."
        actions={
          <>
            <Button onClick={() => setHeld((v) => !v)} aria-pressed={held}>
              {held ? "Let it run" : "Hold the tape"}
            </Button>
            <Button
              tone="quiet"
              onClick={() => void q.refetch()}
              disabled={q.isFetching}
            >
              {q.isFetching ? "Reading…" : "Read now"}
            </Button>
          </>
        }
      />

      {q.isError ? (
        <ErrorState detail={errorText(q.error)} onRetry={() => void q.refetch()} />
      ) : q.isPending ? (
        <p className="mfb-note" aria-live="polite">
          Waiting for the pen…
        </p>
      ) : rows.length === 0 ? (
        <Empty
          title="Nothing has arrived in the last five minutes"
          body="Open your site in another tab and the hit shows up here within a few seconds."
        />
      ) : (
        /* A log, not a live region: a bare aria-live here would re-read all
           forty rows on every poll. As a log, only the rows that actually
           arrived are spoken, and holding the tape stops even that. */
        <div
          className="mfb-tape"
          role="log"
          aria-label="Live event tape"
          aria-live={held ? "off" : "polite"}
          aria-relevant="additions"
        >
          {rows.map((r) => {
            const key = rowKey(r);
            const isOpen = open === key;
            const machine = Boolean(r.agent);
            return (
              <div key={key}>
                <button
                  type="button"
                  className="mfb-tape__row"
                  data-fresh={!held && fresh.has(key) ? "true" : undefined}
                  aria-expanded={isOpen}
                  onClick={() => setOpen((v) => (v === key ? null : key))}
                >
                  <span className="mfb-tape__t">{clock(r.t)}</span>
                  <span className="mfb-tape__penbox">
                    <span
                      className="mfb-pen"
                      data-pen={machine ? "stepped" : "written"}
                      aria-hidden="true"
                    />
                    <span className="mf-sr">{machine ? "Machine" : "Reader"}</span>
                  </span>
                  <span className="mfb-tape__kind">{r.name || r.kind}</span>
                  <span className="mfb-tape__path" title={r.path}>
                    {shortPath(r.path)}
                  </span>
                  <span className="mfb-tape__from">
                    {machine
                      ? agentName(null, r.agent)
                      : titleCase(r.source) || r.country || "Direct"}
                  </span>
                  <IconChevron size={14} className="mfb-tape__caret" />
                </button>

                {isOpen ? (
                  <div className="mfb-tape__props">
                    <pre>{formatProps(r)}</pre>
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}

      <p className="mfb-note">
        {held
          ? "The tape is held. Nothing is lost: it catches up when you let it run."
          : `Reading every five seconds. ${num(q.data?.data.visitors_now)} reading, ${num(
              q.data?.data.agents_now,
            )} fetching.`}
      </p>
    </Panel>
  );
}

/* ── small helpers ───────────────────────────────────────────────────────── */

const rowKey = (r: TapeRow) => r.event_id ?? `${r.t}-${r.kind}-${r.path}-${r.agent ?? ""}`;

const clock = (t: number) =>
  new Date(t).toLocaleTimeString(undefined, {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });

/** The raw props, printed as received, with the row's own facts above them. */
function formatProps(r: TapeRow): string {
  const head: Record<string, unknown> = {
    at: new Date(r.t).toISOString(),
    kind: r.kind,
    path: r.path,
  };
  if (r.name) head.event = r.name;
  if (r.source) head.source = r.source;
  if (r.country && r.country.trim()) head.country = r.country;
  if (r.agent) head.agent = r.agent;
  const body = r.props && Object.keys(r.props).length ? r.props : null;
  try {
    return JSON.stringify(body ? { ...head, props: body } : head, null, 2);
  } catch {
    return `${JSON.stringify(head, null, 2)}\n\nprops could not be printed`;
  }
}

/**
 * Which rows arrived since the last read. Used only to retreat a wash from a
 * new row: the row itself is fully legible from the first frame, whether or
 * not the animation ever runs.
 */
function useFreshKeys(keys: string[], held: boolean) {
  const seen = useRef<Set<string> | null>(null);
  const [fresh, setFresh] = useState<Set<string>>(() => new Set());
  const signature = keys.join("|");

  useEffect(() => {
    if (held) return;
    const next = new Set(keys);
    if (seen.current == null) {
      seen.current = next;
      return;
    }
    const added = new Set<string>();
    for (const k of next) if (!seen.current.has(k)) added.add(k);
    seen.current = next;
    if (added.size) setFresh(added);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [signature, held]);

  return fresh;
}

function RegisterSkeleton() {
  return (
    <div className="mfb-reg mfb-reg--events" aria-busy="true">
      <span className="mf-sr">Reading the register…</span>
      {Array.from({ length: 6 }).map((_, i) => (
        <div className="mfb-reg__row mfb-reg__row--wait" key={i} aria-hidden="true">
          <span />
          <span className="mf-wait" style={{ width: `${34 + ((i * 11) % 42)}%` }} />
          <span className="mf-wait" />
          <span className="mf-wait mfb-col-3" />
          <span className="mf-wait mfb-col-2" />
        </div>
      ))}
    </div>
  );
}

/** Names the problem, so the panel does not just say something went wrong. */
export function errorText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) {
    const m = String((e as { message: unknown }).message);
    const code = "code" in e ? String((e as { code: unknown }).code) : "";
    if (code === "network") return "Could not reach the Micaforge server. Check that it is running, then read again.";
    return m;
  }
  return "The server did not say what went wrong.";
}

export default EventsScreen;
