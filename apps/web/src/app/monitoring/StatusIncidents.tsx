/**
 * INCIDENTS AND MAINTENANCE for one status page: what its readers are told.
 *
 * Two kinds of notice go on a status page, and they are written differently:
 *
 *   - an INCIDENT is reported after something broke: a title, how bad it is,
 *     where the work stands, and a message;
 *   - MAINTENANCE is announced before planned work: a title, the window it
 *     runs in, and a message.
 *
 * After that, both are kept current the same way: post an update with a new
 * status and a message. Posting "Resolved" or "Completed" closes the notice;
 * there is no separate close button, because a reader deserves the closing
 * message as much as the opening one.
 *
 * Some incidents are opened by a monitor rather than a person. They say so in
 * words ("Opened by monitor"), and so do updates the monitor posted, so the
 * person reading the timeline knows which sentences a human wrote.
 *
 * Times are typed in the reader's own time zone (a datetime-local box knows no
 * zone) and sent as UTC, which is what the API stores.
 */
import { useId, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button, Empty, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { IconPlus } from "../../components/react/icons";
import {
  IMPACTS,
  IMPACT_WORD,
  KIND_WORD,
  STATUSES_FOR,
  STATUS_WORD,
  isClosing,
  when,
} from "../../components/status/words";
import type { Impact, IncidentKind, IncidentStatus } from "../../components/status/words";
import { api } from "../../lib/api";
import { Check, ConfirmRow, Field, Input, Say, Select, TextArea } from "./parts";
import type { NewStatusIncident, StatusIncident, StatusPage } from "./types";
import { LIVE_MS, fieldOf, keys, problem } from "./types";

type Names = Map<string, string>;

/** "2026-10-08T14:00" typed in local time, as an ISO string in UTC. */
function localToIso(v: string): string | null {
  if (!v) return null;
  const t = new Date(v);
  return Number.isFinite(t.getTime()) ? t.toISOString() : null;
}

function affected(ids: string[], names: Names): string {
  if (!ids.length) return "Affects the whole page";
  return `Affects ${ids.map((id) => names.get(id) ?? "a removed monitor").join(", ")}`;
}

/* ── a new notice ────────────────────────────────────────────────────────── */

function Composer({
  page,
  kind,
  names,
  onDone,
}: {
  page: StatusPage;
  kind: IncidentKind;
  names: Names;
  onDone: () => void;
}) {
  const uid = useId();
  const qc = useQueryClient();
  const [title, setTitle] = useState("");
  const [impact, setImpact] = useState<Impact>("minor");
  const [status, setStatus] = useState<IncidentStatus>("investigating");
  const [body, setBody] = useState("");
  const [start, setStart] = useState("");
  const [end, setEnd] = useState("");
  const [ids, setIds] = useState<string[]>([]);
  const [tried, setTried] = useState(false);
  const [server, setServer] = useState<{ field?: string; message: string } | null>(null);

  const maintenance = kind === "maintenance";

  const local: Record<string, string> = {};
  if (tried) {
    if (!title.trim()) local.title = "Give it a title readers will understand, for example \"Slow sign-in\".";
    if (!body.trim()) local.body = "Write the first message readers will see.";
    if (maintenance) {
      const s = localToIso(start);
      const e = localToIso(end);
      if (!s) local.scheduled_start = "Choose when the work starts.";
      if (!e) local.scheduled_end = "Choose when the work ends.";
      else if (s && Date.parse(e) <= Date.parse(s)) local.scheduled_end = "The end must be after the start.";
    }
  }
  const errFor = (k: string) => local[k] ?? (server?.field === k ? server.message : undefined);
  const bind = (k: string) => ({
    id: `${uid}-${k}`,
    "aria-invalid": errFor(k) ? true : undefined,
    "aria-describedby": errFor(k) ? `${uid}-${k}-err` : undefined,
  });

  const create = useMutation({
    mutationFn: (input: NewStatusIncident) =>
      api.post<StatusIncident>(`/api/status-pages/${page.id}/incidents`, input).then((r) => r.data),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: keys.pageIncidents(page.id) });
      onDone();
    },
    onError: (e) => {
      const f = fieldOf(e);
      const known = ["title", "impact", "status", "body", "monitor_ids", "scheduled_start", "scheduled_end"];
      setServer({ field: f && known.includes(f) ? f : undefined, message: problem(e) });
    },
  });

  const submit = () => {
    setTried(true);
    setServer(null);
    const s = localToIso(start);
    const e = localToIso(end);
    if (!title.trim() || !body.trim()) return;
    if (maintenance && (!s || !e || Date.parse(e) <= Date.parse(s))) return;
    const input: NewStatusIncident = maintenance
      ? { kind, title: title.trim(), status: "scheduled", impact: "none", body: body.trim(), monitor_ids: ids, scheduled_start: s ?? undefined, scheduled_end: e ?? undefined }
      : { kind, title: title.trim(), status, impact, body: body.trim(), monitor_ids: ids };
    create.mutate(input);
  };

  const unplaced = server && !server.field ? server.message : null;

  return (
    <form
      className="mf-mon-compose"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <h3 className="mf-mon-sub__title">{maintenance ? "Schedule maintenance" : "Report an incident"}</h3>
      <div className="mf-mon-grid">
        <Field id={`${uid}-title`} label="Title" error={errFor("title")} wide>
          <Input {...bind("title")} autoFocus maxLength={200} value={title} onChange={(e) => setTitle(e.target.value)} />
        </Field>
        {maintenance ? (
          <>
            <Field id={`${uid}-scheduled_start`} label="Starts (your local time)" error={errFor("scheduled_start")}>
              <Input {...bind("scheduled_start")} type="datetime-local" value={start} onChange={(e) => setStart(e.target.value)} />
            </Field>
            <Field id={`${uid}-scheduled_end`} label="Ends (your local time)" error={errFor("scheduled_end")}>
              <Input {...bind("scheduled_end")} type="datetime-local" value={end} onChange={(e) => setEnd(e.target.value)} />
            </Field>
          </>
        ) : (
          <>
            <Field id={`${uid}-impact`} label="Impact" error={errFor("impact")}>
              <Select {...bind("impact")} value={impact} onChange={(e) => setImpact(e.target.value as Impact)}>
                {IMPACTS.map((i) => (
                  <option key={i} value={i}>
                    {IMPACT_WORD[i]}
                  </option>
                ))}
              </Select>
            </Field>
            <Field id={`${uid}-status`} label="Status" error={errFor("status")}>
              <Select {...bind("status")} value={status} onChange={(e) => setStatus(e.target.value as IncidentStatus)}>
                {STATUSES_FOR.incident
                  .filter((s) => !isClosing(s))
                  .map((s) => (
                    <option key={s} value={s}>
                      {STATUS_WORD[s]}
                    </option>
                  ))}
              </Select>
            </Field>
          </>
        )}
        <Field id={`${uid}-body`} label="Message" hint="Shown on the public page. Plain text." error={errFor("body")} wide>
          <TextArea {...bind("body")} rows={3} value={body} onChange={(e) => setBody(e.target.value)} />
        </Field>
      </div>
      {page.monitors.length ? (
        <Field label="Affected monitors" hint="Choose none if it affects the whole page." error={errFor("monitor_ids")}>
          <div className="mf-mon-checks">
            {page.monitors.map((m) => (
              <Check
                key={m.monitor_id}
                id={`${uid}-m-${m.monitor_id}`}
                label={names.get(m.monitor_id) ?? m.name}
                checked={ids.includes(m.monitor_id)}
                onChange={(on) =>
                  setIds((cur) => (on ? [...cur, m.monitor_id] : cur.filter((x) => x !== m.monitor_id)))
                }
              />
            ))}
          </div>
        </Field>
      ) : null}
      <div className="mf-mon-acts">
        <Button type="submit" tone="primary" disabled={create.isPending}>
          {create.isPending ? "Posting…" : maintenance ? "Schedule it" : "Post the incident"}
        </Button>
        <Button tone="quiet" disabled={create.isPending} onClick={onDone}>
          Cancel
        </Button>
        {unplaced ? <Say tone="warn">{unplaced}</Say> : null}
      </div>
    </form>
  );
}

/* ── an update to an open notice ─────────────────────────────────────────── */

function UpdateForm({ incident, onDone }: { incident: StatusIncident; onDone: () => void }) {
  const uid = useId();
  const qc = useQueryClient();
  const [status, setStatus] = useState<IncidentStatus>(incident.status);
  const [body, setBody] = useState("");
  const [tried, setTried] = useState(false);

  const post = useMutation({
    mutationFn: () =>
      api
        .post<StatusIncident>(`/api/status-incidents/${incident.id}/updates`, { status, body: body.trim() })
        .then((r) => r.data),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: keys.pageIncidents(incident.page_id) });
      onDone();
    },
  });

  const missing = tried && !body.trim();
  return (
    <form
      className="mf-mon-compose mf-mon-compose--inset"
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        setTried(true);
        if (body.trim()) post.mutate();
      }}
    >
      <div className="mf-mon-grid">
        <Field id={`${uid}-status`} label="New status">
          <Select id={`${uid}-status`} value={status} onChange={(e) => setStatus(e.target.value as IncidentStatus)}>
            {STATUSES_FOR[incident.kind].map((s) => (
              <option key={s} value={s}>
                {STATUS_WORD[s]}
              </option>
            ))}
          </Select>
        </Field>
        <Field
          id={`${uid}-body`}
          label="Message"
          error={missing ? "Write what changed. Readers see this message." : undefined}
          hint={isClosing(status) ? `Posting with "${STATUS_WORD[status]}" closes this ${incident.kind}.` : undefined}
          wide
        >
          <TextArea
            id={`${uid}-body`}
            autoFocus
            rows={3}
            aria-invalid={missing ? true : undefined}
            aria-describedby={missing ? `${uid}-body-err` : undefined}
            value={body}
            onChange={(e) => setBody(e.target.value)}
          />
        </Field>
      </div>
      <div className="mf-mon-acts">
        <Button type="submit" tone="primary" disabled={post.isPending}>
          {post.isPending ? "Posting…" : "Post the update"}
        </Button>
        <Button tone="quiet" disabled={post.isPending} onClick={onDone}>
          Cancel
        </Button>
        {post.isError ? <Say tone="warn">{problem(post.error)}</Say> : null}
      </div>
    </form>
  );
}

/* ── one notice and its timeline ─────────────────────────────────────────── */

function Notice({ incident, names }: { incident: StatusIncident; names: Names }) {
  const qc = useQueryClient();
  const [mode, setMode] = useState<"idle" | "update" | "delete">("idle");
  const open = !incident.resolved_at;

  const remove = useMutation({
    mutationFn: () => api.del<{ deleted: string }>(`/api/status-incidents/${incident.id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: keys.pageIncidents(incident.page_id) }),
  });

  return (
    <article className="mf-mon-notice" data-open={open}>
      <header className="mf-mon-notice__head">
        <div className="mf-mon-notice__text">
          <h4 className="mf-mon-notice__title">{incident.title}</h4>
          <p className="mf-mon-notice__meta">
            <span>{KIND_WORD[incident.kind]}</span>
            <span className="mf-mon-notice__status">{STATUS_WORD[incident.status]}</span>
            {incident.kind === "incident" ? <span>{IMPACT_WORD[incident.impact]}</span> : null}
            {incident.automatic ? <span className="mf-mon-notice__auto">Opened by monitor</span> : null}
            <span>{affected(incident.monitor_ids, names)}</span>
          </p>
          {incident.kind === "maintenance" ? (
            <p className="mf-mon-notice__meta">
              Window: {when(incident.scheduled_start, true)} to {when(incident.scheduled_end, true)}
            </p>
          ) : null}
          <p className="mf-mon-notice__meta">
            Opened {when(incident.created_at)}
            {incident.resolved_at ? `, closed ${when(incident.resolved_at)}` : ", still open"}
          </p>
        </div>
        <div className="mf-mon-notice__acts">
          {open ? (
            <Button size="sm" disabled={mode === "update"} onClick={() => setMode("update")}>
              Post an update
            </Button>
          ) : null}
          <Button size="sm" tone="danger" disabled={mode === "delete"} onClick={() => setMode("delete")}>
            Delete
          </Button>
        </div>
      </header>

      {mode === "delete" ? (
        <ConfirmRow
          confirm={`Delete this ${incident.kind}`}
          busy={remove.isPending}
          onConfirm={() => remove.mutate()}
          onCancel={() => {
            setMode("idle");
            remove.reset();
          }}
        >
          Delete <strong>{incident.title}</strong> and all its updates? It disappears from the public page. This cannot be
          undone.
          {remove.isError ? (
            <>
              {" "}
              <Say tone="warn">{problem(remove.error)}</Say>
            </>
          ) : null}
        </ConfirmRow>
      ) : null}

      {mode === "update" ? <UpdateForm incident={incident} onDone={() => setMode("idle")} /> : null}

      {incident.updates.length ? (
        <ol className="mf-mon-timeline" aria-label={`Updates to ${incident.title}, newest first`}>
          {incident.updates.map((u) => (
            <li key={u.id} className="mf-mon-timeline__item">
              <p className="mf-mon-timeline__head">
                <span className="mf-mon-timeline__status">{STATUS_WORD[u.status]}</span>
                <time className="mf-data" dateTime={u.created_at}>
                  {when(u.created_at)}
                </time>
                {u.automatic ? <span className="mf-mon-notice__auto">Posted by monitor</span> : null}
              </p>
              <p className="mf-mon-timeline__body">{u.body}</p>
            </li>
          ))}
        </ol>
      ) : null}
    </article>
  );
}

/* ── the panel ───────────────────────────────────────────────────────────── */

export function StatusIncidents({ page }: { page: StatusPage }) {
  const [composing, setComposing] = useState<IncidentKind | null>(null);

  const list = useQuery({
    queryKey: keys.pageIncidents(page.id),
    queryFn: () => api.get<StatusIncident[]>(`/api/status-pages/${page.id}/incidents?limit=50`).then((r) => r.data),
    refetchInterval: LIVE_MS,
  });

  const names: Names = useMemo(
    () => new Map(page.monitors.map((m) => [m.monitor_id, m.display_name || m.name])),
    [page.monitors],
  );

  const rows = list.data ?? [];
  const open = rows.filter((r) => !r.resolved_at);
  const closed = rows.filter((r) => r.resolved_at);

  return (
    <Panel className="mf-mon-notices">
      <PanelHead
        title="Incidents and maintenance"
        note="What readers of this page are told. Incidents that a monitor on this page opens appear here by themselves."
        actions={
          <>
            <Button size="sm" icon={<IconPlus size={13} />} disabled={composing === "incident"} onClick={() => setComposing("incident")}>
              Report an incident
            </Button>
            <Button size="sm" icon={<IconPlus size={13} />} disabled={composing === "maintenance"} onClick={() => setComposing("maintenance")}>
              Schedule maintenance
            </Button>
          </>
        }
      />

      {composing ? (
        <Composer key={composing} page={page} kind={composing} names={names} onDone={() => setComposing(null)} />
      ) : null}

      {list.isPending ? (
        <div className="mf-mon-notice-wait" aria-busy="true" aria-live="polite">
          <span className="mf-sr">Loading…</span>
          {[58, 44, 66].map((w, i) => (
            <span key={i} className="mf-wait" style={{ width: `${w}%` }} />
          ))}
        </div>
      ) : list.isError ? (
        <ErrorState title="Could not load the incidents" detail={problem(list.error)} onRetry={() => void list.refetch()} />
      ) : rows.length === 0 ? (
        <Empty
          title="Nothing posted yet"
          body="When you report an incident or schedule maintenance, or a monitor on this page opens an incident, it is listed here and shown on the public page."
        />
      ) : (
        <>
          {open.length ? (
            <section aria-label="Open">
              <h3 className="mf-mon-sub mf-mon-sub__title">Open</h3>
              {open.map((i) => (
                <Notice key={i.id} incident={i} names={names} />
              ))}
            </section>
          ) : null}
          {closed.length ? (
            <section aria-label="Closed">
              <h3 className="mf-mon-sub mf-mon-sub__title">Closed</h3>
              {closed.map((i) => (
                <Notice key={i.id} incident={i} names={names} />
              ))}
            </section>
          ) : null}
        </>
      )}
    </Panel>
  );
}

export default StatusIncidents;
