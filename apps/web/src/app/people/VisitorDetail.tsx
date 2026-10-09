/**
 * PEOPLE: one named visitor, and the erasure path.
 *
 * Micaforge is a privacy product, so deletion is not a hidden setting: it is a
 * first-class control on the profile itself. It states exactly what will be
 * removed, counts it, requires the word to be typed, and then really deletes.
 * Nothing here pretends to delete.
 *
 * `ErasureDialog` and `IconErase` live in this file because erasure is this
 * file's subject; the replay screen imports both rather than growing a second
 * confirmation of its own that could drift out of step with this one.
 *
 * The traits, their history and the earlier anonymous sessions come from
 * `GET /api/stats/profiles/:id`, which reads the site's own identify() and
 * setTraits() calls. Each panel says plainly that the site sent it: Micaforge
 * links a daily visitor hash to an id only where an identify() call carried
 * both, never by guessing.
 */
import type { ReactNode, SVGProps } from "react";
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "../../lib/api";
import type { Filter } from "../../lib/api";
import { ago, channelLabel, duration, num } from "../../lib/format";
import { Button, Empty, ErrorState, Panel, PanelHead, Readout, Sheet } from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import { IconChevron, IconGoals } from "../../components/react/icons";
import { useSite } from "../providers";
import {
  Hand,
  Subject,
  apiMessage,
  asRows,
  dayAt,
  sessionIdOf,
  stampAt,
  toMs,
  useBase,
} from "./SessionsScreen";
import type { SessionRow } from "./SessionsScreen";
import type { VisitorRow } from "./VisitorsScreen";
import "../../styles/app/people.css";
import "../../styles/app/profiles.css";

/* ── the erase mark ──────────────────────────────────────────────────────
   Drawn in the icon set's grammar (16x16, 1.4 stroke, square caps, one filled
   1.4 tick): an eraser over a baseline.
   It lives here rather than in the shared set so that two areas building in
   parallel cannot collide over the same file. */
export function IconErase({ size = 16, ...rest }: { size?: number } & SVGProps<SVGSVGElement>) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.4}
      strokeLinecap="square"
      strokeLinejoin="miter"
      aria-hidden="true"
      focusable="false"
      {...rest}
    >
      <path d="M1.5 13.5h13" />
      <path d="M4.5 10.6 10.1 5 13 7.9 7.4 13.5z" />
      <path d="M6.8 8.3 9.7 11.2" />
      <rect x={1.3} y={2.3} width={1.4} height={1.4} fill="currentColor" stroke="none" />
    </svg>
  );
}

/* ── the erasure dialog ─────────────────────────────────────────────────── */

export interface ErasureLine {
  label: string;
  value: string;
}

/**
 * A modal built on the platform's own `<dialog>`: it traps focus, answers the
 * Escape key and renders inert background content without a library or a
 * hand-rolled focus trap that would get it wrong.
 */
export function ErasureDialog({
  open,
  title,
  lead,
  lines,
  note,
  confirmWord = "erase",
  confirmLabel,
  busy,
  error,
  onDismiss,
  onConfirm,
}: {
  open: boolean;
  title: string;
  lead: ReactNode;
  lines: ErasureLine[];
  note?: ReactNode;
  confirmWord?: string;
  confirmLabel: string;
  busy?: boolean;
  error?: string | null;
  onDismiss: () => void;
  onConfirm: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const [typed, setTyped] = useState("");

  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) {
      setTyped("");
      d.showModal();
    } else if (!open && d.open) {
      d.close();
    }
  }, [open]);

  const ready = typed.trim().toLowerCase() === confirmWord.toLowerCase() && !busy;

  return (
    <dialog
      ref={ref}
      className="mf-p-dialog"
      aria-labelledby="mf-p-erase-title"
      onCancel={(e) => {
        if (busy) e.preventDefault();
      }}
      onClose={() => {
        if (open) onDismiss();
      }}
    >
      <div className="mf-p-dialog__head">
        <h2 className="mf-p-dialog__title" id="mf-p-erase-title">
          {title}
        </h2>
      </div>
      <div className="mf-p-dialog__body">
        <p>{lead}</p>
        {lines.length ? (
          <ul className="mf-p-dialog__list">
            {lines.map((l) => (
              <li key={l.label}>
                <span>{l.label}</span>
                <b>{l.value}</b>
              </li>
            ))}
          </ul>
        ) : null}
        {note ? <p className="mf-p-dialog__note">{note}</p> : null}
        <div className="mf-p-dialog__confirm">
          <label htmlFor="mf-p-erase-word">
            Type <b>{confirmWord}</b> to confirm
          </label>
          <input
            id="mf-p-erase-word"
            value={typed}
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => setTyped(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && ready) onConfirm();
            }}
          />
        </div>
      </div>
      {error ? <p className="mf-p-dialog__err">{error}</p> : null}
      <div className="mf-p-dialog__foot">
        <Button onClick={onDismiss} disabled={busy}>
          Keep it
        </Button>
        <Button tone="danger" icon={<IconErase size={14} />} disabled={!ready} onClick={onConfirm}>
          {busy ? "Erasing…" : confirmLabel}
        </Button>
      </div>
    </dialog>
  );
}

/* ── the screen ─────────────────────────────────────────────────────────── */

interface VisitorDetailPayload extends VisitorRow {
  replays?: number;
  events?: number;
}

/** One identify() or setTraits() call that changed something. */
export interface TraitChange {
  t: string;
  traits: Record<string, unknown>;
  changed: string[];
}

/** A session recorded under a linked daily hash before the id was attached. */
export interface AnonymousSession {
  session_id: string;
  visitor_id: string;
  started: string;
  ended: string;
  pageviews: number;
  entry_path: string;
  channel: string;
}

/** `GET /api/stats/profiles/:id`; `null` for an id the site never identified. */
export interface ProfilePayload {
  identified_id: string;
  traits: Record<string, unknown>;
  history: TraitChange[];
  linked: { visitor_id: string; linked_at: string }[];
  anonymous_sessions: AnonymousSession[];
}

interface GoalRow {
  id?: string;
  name: string;
  conversions?: number;
  rate?: number;
  revenue?: number | null;
  last_at?: number | string;
}

export function VisitorDetail() {
  const { id } = useParams();
  const base = useBase();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { siteId, timezone } = useSite();
  const [confirming, setConfirming] = useState(false);

  const visitorId = id ?? "";
  // A profile is the whole history of one id, so it deliberately ignores the
  // interval the rest of the dashboard is set to.
  const scope = useMemo(
    () => ({
      site: siteId as number,
      range: "all",
      audience: "all" as const,
      filters: [{ dimension: "identified_id", op: "is", value: visitorId } as Filter],
    }),
    [siteId, visitorId],
  );

  const q = useQuery({
    queryKey: ["stats", "visitor", siteId, visitorId],
    queryFn: () =>
      api.get<VisitorDetailPayload>(`/api/stats/visitors/${encodeURIComponent(visitorId)}`, {
        site: siteId as number,
        range: "all",
      }),
    enabled: siteId != null && !!visitorId,
  });

  const sessionsQ = useQuery({
    queryKey: ["stats", "sessions", siteId, "all", "all", JSON.stringify(scope.filters)],
    queryFn: () => api.get<unknown>("/api/stats/sessions", { ...scope, limit: 25, page: 0 }),
    enabled: siteId != null && !!visitorId,
  });

  const goalsQ = useQuery({
    queryKey: ["stats", "goals", siteId, "all", "all", JSON.stringify(scope.filters)],
    queryFn: () => api.get<unknown>("/api/stats/goals", scope),
    enabled: siteId != null && !!visitorId,
  });

  const profileQ = useQuery({
    queryKey: ["stats", "profile", siteId, visitorId],
    queryFn: () =>
      api
        .get<ProfilePayload | null>(`/api/stats/profiles/${encodeURIComponent(visitorId)}`, {
          site: siteId as number,
          range: "all",
        })
        .then((r) => r.data ?? null),
    enabled: siteId != null && !!visitorId,
  });

  const erase = useMutation({
    mutationFn: () => api.del<unknown>(`/api/stats/visitors/${encodeURIComponent(visitorId)}`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["stats"] });
      setConfirming(false);
      navigate(`${base}/visitors`, { replace: true });
    },
  });

  const visitor = q.data?.data;
  const sessions = useMemo(() => asRows<SessionRow>(sessionsQ.data?.data, "sessions"), [sessionsQ.data]);
  const goals = useMemo(() => asRows<GoalRow>(goalsQ.data?.data, "goals"), [goalsQ.data]);
  const profile = profileQ.data ?? null;
  const traits = useMemo(
    () => Object.entries(profile?.traits ?? visitor?.traits ?? {}).filter(([, v]) => v !== undefined),
    [profile, visitor],
  );
  const history = useMemo(() => [...(profile?.history ?? [])].reverse(), [profile]);
  const anonymous = profile?.anonymous_sessions ?? [];
  const linkedDays = profile?.linked.length ?? 0;
  const missing = q.isError && q.error instanceof ApiError && q.error.status === 404;

  const channels: Channel[] = visitor
    ? [
        { key: "sessions", label: "Sessions", value: num(visitor.sessions ?? sessions.length) },
        { key: "pageviews", label: "Pageviews", value: num(visitor.pageviews ?? 0) },
        {
          key: "goals",
          label: "Goals reached",
          value: num(goals.reduce((n, g) => n + (g.conversions ?? 0), 0)),
        },
        ...(visitor.events != null
          ? [{ key: "events", label: "Custom events", value: num(visitor.events) }]
          : []),
      ]
    : [];

  const eraseLines: ErasureLine[] = visitor
    ? [
        { label: "The identifier itself", value: visitorId },
        ...(visitor.sessions != null ? [{ label: "Sessions", value: num(visitor.sessions) }] : []),
        ...(visitor.pageviews != null ? [{ label: "Pageviews", value: num(visitor.pageviews) }] : []),
        ...(visitor.events != null ? [{ label: "Custom events", value: num(visitor.events) }] : []),
        ...(visitor.replays != null ? [{ label: "Replay recordings", value: num(visitor.replays) }] : []),
        ...(traits.length ? [{ label: "Traits", value: num(traits.length) }] : []),
        ...(history.length ? [{ label: "Trait changes", value: num(history.length) }] : []),
        ...(anonymous.length
          ? [{ label: "Earlier anonymous sessions", value: num(anonymous.length) }]
          : []),
      ]
    : [{ label: "The identifier itself", value: visitorId }];

  const anonymousColumns: Column<AnonymousSession>[] = [
    {
      key: "started",
      head: "Started",
      width: "15ch",
      render: (r) => (
        <Link
          className="mf-data"
          to={`${base}/sessions/${encodeURIComponent(r.session_id)}`}
          onClick={(e) => e.stopPropagation()}
        >
          {dayAt(r.started, timezone)}
        </Link>
      ),
    },
    {
      key: "entry",
      head: "First page",
      render: (r) => (
        <span className="mf-data" title={r.entry_path}>
          {r.entry_path || "—"}
        </span>
      ),
    },
    {
      key: "pages",
      head: "Pages",
      numeric: true,
      width: "8ch",
      render: (r) => <span className="mf-data">{num(r.pageviews)}</span>,
    },
    { key: "channel", head: "Channel", width: "16ch", render: (r) => <span>{channelLabel(r.channel || "direct")}</span> },
  ];

  const sessionColumns: Column<SessionRow>[] = [
    {
      key: "started",
      head: "Started",
      width: "15ch",
      render: (r) => (
        <Link
          className="mf-data"
          to={`${base}/sessions/${encodeURIComponent(sessionIdOf(r))}`}
          onClick={(e) => e.stopPropagation()}
        >
          {dayAt(r.started, timezone)}
        </Link>
      ),
    },
    {
      key: "duration",
      head: "Duration",
      numeric: true,
      width: "10ch",
      render: (r) => <span className="mf-data">{duration(r.duration_s)}</span>,
    },
    {
      key: "pages",
      head: "Pages",
      numeric: true,
      width: "8ch",
      render: (r) => <span className="mf-data">{num(r.pageviews ?? 0)}</span>,
    },
    { key: "channel", head: "Channel", width: "16ch", render: (r) => <span>{channelLabel(r.channel ?? "direct")}</span> },
    { key: "hand", head: "Type", width: "16ch", render: (r) => <Hand session={r} /> },
  ];

  if (missing) {
    return (
      <div className="mf-p-stack">
        <Link className="mf-p-back" to={`${base}/visitors`}>
          <IconChevron size={14} />
          All visitors
        </Link>
        <Panel>
          <Empty
            title="No visitor carries that id"
            body="It was never passed to identify(), or it has already been erased. Erasure is permanent by design."
            action={
              <Link className="mf-btn mf-btn--key mf-btn--md" to={`${base}/visitors`}>
                <span className="mf-btn__label">Back to visitors</span>
              </Link>
            }
          />
        </Panel>
      </div>
    );
  }

  return (
    <div className="mf-p-stack">
      <Link className="mf-p-back" to={`${base}/visitors`}>
        <IconChevron size={14} />
        All visitors
      </Link>

      {q.isError ? (
        <Panel>
          <ErrorState detail={apiMessage(q.error)} onRetry={() => q.refetch()} />
        </Panel>
      ) : (
        <>
          <Panel>
            <Subject
              id={q.isPending ? <span className="mf-wait mf-p-wait-line" /> : visitorId}
              when={
                visitor
                  ? `First seen ${stampAt(visitor.first_seen, timezone)} · last seen ${ago(toMs(visitor.last_seen))}`
                  : undefined
              }
            />
            {channels.length ? <Readout channels={channels} dense /> : null}
          </Panel>

          <div className="mf-p-split">
            <div className="mf-p-stack">
              <Panel>
                <PanelHead
                  title="Sessions"
                  note="Every session this id has been attached to, newest first, across all time."
                />
                {sessionsQ.isError ? (
                  <ErrorState detail={apiMessage(sessionsQ.error)} onRetry={() => sessionsQ.refetch()} />
                ) : (
                  <Sheet
                    rows={sessions}
                    columns={sessionColumns}
                    loading={sessionsQ.isPending}
                    getKey={(r, i) => sessionIdOf(r) || String(i)}
                    onRowClick={(r) => navigate(`${base}/sessions/${encodeURIComponent(sessionIdOf(r))}`)}
                    empty={
                      <Empty
                        title="No session carries this id yet"
                        body="identify() attaches the id to the session it is called in and to every session after it on the same device."
                      />
                    }
                  />
                )}
              </Panel>

              <Panel>
                <PanelHead
                  title="Before they were identified"
                  note={
                    linkedDays
                      ? `Sessions recorded under the ${num(linkedDays)} daily ${
                          linkedDays === 1 ? "hash" : "hashes"
                        } your site tied to this id with identify(), before the id was attached that day.`
                      : "Sessions from the same day as an identify() call, before the call, appear here."
                  }
                />
                {profileQ.isError ? (
                  <ErrorState detail={apiMessage(profileQ.error)} onRetry={() => profileQ.refetch()} />
                ) : (
                  <Sheet
                    rows={anonymous}
                    columns={anonymousColumns}
                    loading={profileQ.isPending}
                    getKey={(r) => r.session_id}
                    onRowClick={(r) => navigate(`${base}/sessions/${encodeURIComponent(r.session_id)}`)}
                    empty={
                      <Empty
                        title="No earlier anonymous sessions"
                        body="Either identify() ran at the start of every visit, or nothing was recorded before it on those days. Micaforge links only what an identify() call tied together."
                      />
                    }
                  />
                )}
              </Panel>

              <Panel>
                <PanelHead title="Goals reached" />
                {goalsQ.isError ? (
                  <ErrorState detail={apiMessage(goalsQ.error)} onRetry={() => goalsQ.refetch()} />
                ) : goalsQ.isPending ? (
                  <div className="mf-p-facts" aria-busy="true">
                    {[0, 1, 2].map((i) => (
                      <p key={i} className="mf-p-wait-row mf-p-wait-row--span">
                        <span className="mf-wait" style={{ width: `${45 + i * 12}%` }} />
                      </p>
                    ))}
                  </div>
                ) : goals.filter((g) => (g.conversions ?? 0) > 0).length === 0 ? (
                  <Empty
                    title="No goal has been reached by this visitor"
                    body="Goals are defined per site under Settings. Once one is defined, every conversion this id makes is listed here."
                  />
                ) : (
                  <ul className="mf-p-list">
                    {goals
                      .filter((g) => (g.conversions ?? 0) > 0)
                      .map((g) => (
                        <li key={g.id ?? g.name}>
                          <span className="mf-p-inline">
                            <IconGoals size={14} />
                            {g.name}
                          </span>
                          <span className="mf-p-list__value">
                            <b className="mf-data">{num(g.conversions ?? 0)}</b>
                            {g.last_at ? <span className="mf-p-dim">last {ago(toMs(g.last_at))}</span> : null}
                          </span>
                        </li>
                      ))}
                  </ul>
                )}
              </Panel>
            </div>

            <div className="mf-p-stack">
              <Panel>
                <PanelHead
                  title="Traits"
                  note="Whatever your site passed with identify() or setTraits(), latest value first."
                />
                {q.isPending || profileQ.isPending ? (
                  <div className="mf-p-facts" aria-busy="true">
                    {[0, 1, 2, 3].map((i) => (
                      <p key={i} className="mf-p-wait-row mf-p-wait-row--span">
                        <span className="mf-wait" style={{ width: `${50 + ((i * 11) % 35)}%` }} />
                      </p>
                    ))}
                  </div>
                ) : traits.length === 0 ? (
                  <Empty
                    title="No traits were sent with this id"
                    body="Pass a second argument to identify(), or call setTraits(), and it collects here: plan, role, company, whatever your product already knows."
                  />
                ) : (
                  <dl className="mf-p-facts">
                    {traits.map(([k, v]) => (
                      <Fragment key={k}>
                        <dt className="mf-data">{k}</dt>
                        <dd className="mf-data">{showTrait(v)}</dd>
                      </Fragment>
                    ))}
                  </dl>
                )}
              </Panel>

              <Panel>
                <PanelHead
                  title="Trait history"
                  note="Every call that changed a trait, newest first, with the time it arrived."
                />
                {profileQ.isPending ? (
                  <div className="mf-p-facts" aria-busy="true">
                    {[0, 1, 2].map((i) => (
                      <p key={i} className="mf-p-wait-row mf-p-wait-row--span">
                        <span className="mf-wait" style={{ width: `${40 + i * 15}%` }} />
                      </p>
                    ))}
                  </div>
                ) : profileQ.isError ? (
                  <ErrorState detail={apiMessage(profileQ.error)} onRetry={() => profileQ.refetch()} />
                ) : history.length === 0 ? (
                  <Empty
                    title="No trait has changed yet"
                    body="Each identify() or setTraits() call that changes a value is kept here with its time, so a plan upgrade shows when it happened."
                  />
                ) : (
                  <ol className="mf-pr-history">
                    {history.map((h, i) => (
                      <li key={`${h.t}-${i}`} className="mf-pr-history__item">
                        <span className="mf-pr-history__when mf-data" title={stampAt(h.t, timezone)}>
                          {dayAt(h.t, timezone)}
                        </span>
                        <dl className="mf-pr-history__changes">
                          {h.changed.map((k) => (
                            <Fragment key={k}>
                              <dt className="mf-data">{k}</dt>
                              <dd className="mf-data">{showTrait(h.traits[k])}</dd>
                            </Fragment>
                          ))}
                        </dl>
                      </li>
                    ))}
                  </ol>
                )}
              </Panel>

              <Panel>
                <div className="mf-p-erase">
                  <h2 className="mf-p-erase__title">Erasure</h2>
                  <p className="mf-p-erase__body">
                    Deletes the id and everything Micaforge holds against it from the event store, on request or on a
                    subject access demand. It cannot be undone, and it is not queued: the rows go when you confirm.
                  </p>
                  <div className="mf-p-erase__act">
                    <Button tone="danger" icon={<IconErase size={14} />} onClick={() => setConfirming(true)}>
                      Erase this visitor
                    </Button>
                    {erase.isError ? (
                      <span className="mf-p-erase__failed">{apiMessage(erase.error)}</span>
                    ) : null}
                  </div>
                </div>
              </Panel>
            </div>
          </div>

          <ErasureDialog
            open={confirming}
            title="Erase this visitor?"
            lead={<>Everything below is deleted from this site&rsquo;s event store. This cannot be undone.</>}
            lines={eraseLines}
            note="Rolled-up totals that no longer carry the id are unaffected, so past totals will not change."
            confirmLabel="Erase permanently"
            busy={erase.isPending}
            error={erase.isError ? apiMessage(erase.error) : null}
            onDismiss={() => {
              if (!erase.isPending) setConfirming(false);
            }}
            onConfirm={() => erase.mutate()}
          />
        </>
      )}
    </div>
  );
}

function showTrait(v: unknown): string {
  if (v == null) return "—";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

export default VisitorDetail;
