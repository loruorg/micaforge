/**
 * AUDIT LOG: who changed what in an organisation, and what it was before.
 *
 * Three decisions shape the screen:
 *
 *   1. A ROW IS ONE CHANGE, READ IN A LINE. Time, who, what, to which object,
 *      on which site. Opening a row shows the change field by field, before
 *      and after, because "site.updated" alone answers nothing during the
 *      incident this screen is opened for.
 *   2. THE FILTERS ARE THE SERVER'S. Every filter here is a query parameter of
 *      `GET /api/audit`, and the CSV link carries exactly the same ones, so
 *      the download is the table on screen, all pages of it. The link is a
 *      plain anchor: the browser sends the session cookie with it and saves
 *      what the server answers, with no copy of the data held in this tab.
 *   3. DAYS ARE THE READER'S DAYS. The date inputs pick calendar days in the
 *      reader's own timezone, sent as full timestamps from the first to the
 *      last millisecond of those days, so "the 3rd" means the 3rd where the
 *      reader is.
 *
 * Reading the log needs the admin or owner role on the organisation. The
 * server enforces that; the screen only says so in words when it refuses.
 */
import { useEffect, useMemo, useState } from "react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";

import { Button, Empty, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { IconChevron, IconDownload } from "../../components/react/icons";
import { API_BASE, ApiError, api, type Actor, type SiteSummary } from "../../lib/api";
import { num } from "../../lib/format";
import { SITES_KEY } from "../providers";
import { Field, Select, TextInput, Waiting, errText } from "./SettingsLayout";
import { useCurrentOrg, type Member } from "./OrgSettings";
import "../../styles/app/workspace.css";

/* ── what the server sends ───────────────────────────────────────────────── */

interface AuditEvent {
  id: string;
  org_id: string | null;
  created_at: string;
  actor: Actor;
  action: string;
  target: { type: string; id: string | null; label: string };
  site_id: string | null;
  before: unknown;
  after: unknown;
  metadata: unknown;
  ip_hash: string | null;
  user_agent: string | null;
}

interface AuditPage {
  events: AuditEvent[];
  /** The `before=` value for the next page, or null on the last one. */
  next: string | null;
}

interface AuditAction {
  action: string;
  label: string;
}

/* ── wording ─────────────────────────────────────────────────────────────── */

/** The first word of an action, named the way the rest of the dashboard names it. */
const GROUP_NAME: Record<string, string> = {
  auth: "Sign-in",
  site: "Sites",
  org: "Organisation",
  member: "Members",
  invite: "Invitations",
  team: "Teams",
  api_key: "API keys",
  share: "Shared links",
  alert: "Alerts",
  report: "Reports",
  flag: "Feature flags",
  target_group: "Target groups",
  import: "Imports",
  error: "Errors",
  billing: "Billing",
};

/** Object types worth suggesting; the filter takes any text the server records. */
const TARGET_SUGGESTIONS = [
  "site",
  "organisation",
  "member",
  "invitation",
  "team",
  "api_key",
  "share_link",
  "alert",
  "report",
  "flag",
  "target_group",
  "import",
  "error",
  "subscription",
  "account",
];

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const API_KEY = "__key";

function actorKind(a: Actor): string | null {
  if (a.kind === "api_key") return "API key";
  if (a.kind === "system") return "Micaforge";
  return null;
}

function when(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime())
    ? iso
    : d.toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

/** A stored value, written for a person. Objects stay JSON, in the data face. */
function shown(v: unknown): string {
  if (v === undefined || v === null) return "—";
  if (typeof v === "string") return v === "" ? "(empty)" : v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  return JSON.stringify(v);
}

const isRecord = (v: unknown): v is Record<string, unknown> =>
  !!v && typeof v === "object" && !Array.isArray(v);

/**
 * Before and after, field by field, keeping only the fields that changed. A
 * created object has no before and a deleted one no after; both still list
 * every field, against a dash.
 */
function fieldDiff(before: unknown, after: unknown): {
  rows: { field: string; before: unknown; after: unknown }[];
  unchanged: number;
} {
  if (isRecord(before) || isRecord(after)) {
    const b = isRecord(before) ? before : {};
    const a = isRecord(after) ? after : {};
    const keys = [...new Set([...Object.keys(b), ...Object.keys(a)])];
    const rows = keys
      .filter((k) => JSON.stringify(b[k]) !== JSON.stringify(a[k]))
      .map((k) => ({ field: k, before: b[k], after: a[k] }));
    return { rows, unchanged: keys.length - rows.length };
  }
  if (before == null && after == null) return { rows: [], unchanged: 0 };
  if (JSON.stringify(before) === JSON.stringify(after)) return { rows: [], unchanged: 1 };
  return { rows: [{ field: "value", before, after }], unchanged: 0 };
}

/* ── filters ─────────────────────────────────────────────────────────────── */

interface Filters {
  actor: string;
  action: string;
  targetType: string;
  /** Calendar days, YYYY-MM-DD, in the reader's timezone. */
  from: string;
  to: string;
}

const NO_FILTERS: Filters = { actor: "", action: "", targetType: "", from: "", to: "" };

/** Waits for typing to stop before a typed filter becomes a request. */
function useSettled<T>(value: T, ms = 400): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setSettled(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return settled;
}

function dayStart(day: string): string | null {
  const d = new Date(`${day}T00:00:00`);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

function dayEnd(day: string): string | null {
  const d = new Date(`${day}T23:59:59.999`);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

function auditParams(orgId: string, f: Filters): URLSearchParams {
  const q = new URLSearchParams({ org: orgId });
  if (f.actor) q.set("actor", f.actor);
  if (f.action) q.set("action", f.action);
  if (f.targetType.trim()) q.set("target_type", f.targetType.trim());
  const from = f.from ? dayStart(f.from) : null;
  const to = f.to ? dayEnd(f.to) : null;
  if (from) q.set("from", from);
  if (to) q.set("to", to);
  return q;
}

/* ── one change, opened ──────────────────────────────────────────────────── */

function EventDetail({ e }: { e: AuditEvent }) {
  const diff = fieldDiff(e.before, e.after);
  const meta = e.metadata;
  return (
    <div className="mf-ws-audit__detail">
      {diff.rows.length ? (
        <div className="mf-ws-diff">
          <div className="mf-ws-diff__head" aria-hidden="true">
            <span>Field</span>
            <span>Before</span>
            <span>After</span>
          </div>
          <dl className="mf-ws-diff__list">
            {diff.rows.map((r) => (
              <div className="mf-ws-diff__row" key={r.field}>
                <dt className="mf-ws-diff__field mf-data">{r.field}</dt>
                <dd className="mf-ws-diff__before mf-data">
                  <span className="mf-sr">Before: </span>
                  {shown(r.before)}
                </dd>
                <dd className="mf-ws-diff__after mf-data">
                  <span className="mf-sr">After: </span>
                  {shown(r.after)}
                </dd>
              </div>
            ))}
          </dl>
          {diff.unchanged ? (
            <p className="mf-set-note">
              {diff.unchanged === 1 ? "One other field was" : `${diff.unchanged} other fields were`} recorded and did
              not change.
            </p>
          ) : null}
        </div>
      ) : (
        <p className="mf-set-note">No before or after values were recorded for this change.</p>
      )}

      <dl className="mf-ws-facts">
        {meta != null ? (
          isRecord(meta) ? (
            Object.entries(meta).map(([k, v]) => (
              <div className="mf-ws-facts__row" key={k}>
                <dt>{k}</dt>
                <dd className="mf-data">{shown(v)}</dd>
              </div>
            ))
          ) : (
            <div className="mf-ws-facts__row">
              <dt>Details</dt>
              <dd className="mf-data">{shown(meta)}</dd>
            </div>
          )
        ) : null}
        <div className="mf-ws-facts__row">
          <dt>Action</dt>
          <dd className="mf-data">{e.action}</dd>
        </div>
        {e.target.id ? (
          <div className="mf-ws-facts__row">
            <dt>{e.target.type} id</dt>
            <dd className="mf-data">{e.target.id}</dd>
          </div>
        ) : null}
        {e.actor.id ? (
          <div className="mf-ws-facts__row">
            <dt>{e.actor.kind === "api_key" ? "API key id" : "Person id"}</dt>
            <dd className="mf-data">{e.actor.id}</dd>
          </div>
        ) : null}
        <div className="mf-ws-facts__row">
          <dt>Address hash</dt>
          <dd className="mf-data">{e.ip_hash || "not recorded"}</dd>
        </div>
        <div className="mf-ws-facts__row">
          <dt>Client</dt>
          <dd className="mf-data">{e.user_agent || "not recorded"}</dd>
        </div>
      </dl>
    </div>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function AuditLogSettings() {
  const { orgs, orgId, setChosen } = useCurrentOrg();

  const [filters, setFilters] = useState<Filters>(NO_FILTERS);
  const [actorChoice, setActorChoice] = useState("");
  const [keyId, setKeyId] = useState("");
  const [openId, setOpenId] = useState<string | null>(null);

  const typedTarget = useSettled(filters.targetType);
  const typedKey = useSettled(keyId.trim());
  const keyReady = UUID_RE.test(typedKey);

  const applied: Filters = {
    ...filters,
    targetType: typedTarget,
    actor: actorChoice === API_KEY ? (keyReady ? typedKey : "") : actorChoice,
  };
  const filtered =
    !!applied.actor || !!applied.action || !!applied.targetType.trim() || !!applied.from || !!applied.to;

  const members = useQuery({
    queryKey: ["settings", "members", orgId],
    queryFn: () => api.get<Member[]>(`/api/orgs/${orgId}/members`).then((r) => r.data),
    enabled: !!orgId,
  });

  const actions = useQuery({
    queryKey: ["settings", "audit-actions"],
    queryFn: () => api.get<AuditAction[]>("/api/audit/actions").then((r) => r.data ?? []),
    staleTime: 60 * 60_000,
  });

  const sites = useQuery({
    queryKey: SITES_KEY,
    queryFn: () => api.get<SiteSummary[]>("/api/sites").then((r) => r.data),
    staleTime: 60_000,
  });

  const params = auditParams(orgId, applied).toString();

  const events = useInfiniteQuery({
    queryKey: ["settings", "audit", params],
    enabled: !!orgId,
    initialPageParam: null as string | null,
    queryFn: ({ pageParam }) => {
      const q = new URLSearchParams(params);
      q.set("limit", "50");
      if (pageParam) q.set("before", pageParam);
      return api.get<AuditPage>(`/api/audit?${q.toString()}`).then((r) => r.data);
    },
    getNextPageParam: (last) => last.next ?? undefined,
  });

  const labels = useMemo(() => {
    const m = new Map<string, string>();
    for (const a of actions.data ?? []) m.set(a.action, a.label);
    return m;
  }, [actions.data]);

  const actionGroups = useMemo(() => {
    const groups = new Map<string, AuditAction[]>();
    for (const a of actions.data ?? []) {
      const prefix = a.action.split(".")[0] ?? a.action;
      groups.set(prefix, [...(groups.get(prefix) ?? []), a]);
    }
    return [...groups];
  }, [actions.data]);

  const siteName = (id: string | null) => {
    if (!id) return null;
    const s = sites.data?.find((x) => x.id === id);
    return s ? s.domain : `${id.slice(0, 8)}…`;
  };

  const rows = events.data?.pages.flatMap((p) => p.events) ?? [];
  const exportUrl = `${API_BASE}/api/audit/export.csv?${params}`;
  const refused = events.error instanceof ApiError && events.error.status === 403;

  const clear = () => {
    setFilters(NO_FILTERS);
    setActorChoice("");
    setKeyId("");
  };

  return (
    <Panel>
      <PanelHead
        title="Audit log"
        note="Every change made in this organisation: who made it, when, and what it was before and after. Sign-ins of its members are listed too."
        actions={
          (orgs.data?.length ?? 0) > 1 ? (
            <Select auto aria-label="Organisation" value={orgId} onChange={(e) => setChosen(e.target.value)}>
              {orgs.data?.map((o) => (
                <option key={o.id} value={o.id}>
                  {o.name}
                </option>
              ))}
            </Select>
          ) : undefined
        }
      />

      {orgs.isError ? (
        <ErrorState title="Could not load your organisations" detail={errText(orgs.error)} onRetry={() => void orgs.refetch()} />
      ) : orgs.isSuccess && !orgs.data.length ? (
        <Empty
          title="You are not in an organisation"
          body="The audit log belongs to an organisation. Create one from the site chooser to begin."
        />
      ) : (
        <>
          <div className="mf-ws-filters" role="group" aria-label="Filters">
            <Field label="Who" htmlFor="audit-actor">
              <Select
                id="audit-actor"
                value={actorChoice}
                onChange={(e) => {
                  setActorChoice(e.target.value);
                  setOpenId(null);
                }}
              >
                <option value="">Anyone</option>
                {(members.data ?? []).map((m) => (
                  <option key={m.user_id} value={m.user_id}>
                    {m.name ? `${m.name} (${m.email})` : m.email}
                  </option>
                ))}
                <option value={API_KEY}>An API key…</option>
              </Select>
            </Field>
            {actorChoice === API_KEY ? (
              <Field
                label="API key id"
                htmlFor="audit-key"
                hint="The key's id from the API keys screen, not the secret."
                error={typedKey && !keyReady ? "Paste the whole id: 36 characters with dashes." : undefined}
              >
                <TextInput
                  id="audit-key"
                  data
                  spellCheck={false}
                  autoCapitalize="off"
                  placeholder="0192f3a4-…"
                  value={keyId}
                  aria-invalid={typedKey && !keyReady ? true : undefined}
                  onChange={(e) => setKeyId(e.target.value)}
                />
              </Field>
            ) : null}
            <Field label="What" htmlFor="audit-action">
              <Select
                id="audit-action"
                value={filters.action}
                disabled={actions.isError}
                onChange={(e) => setFilters({ ...filters, action: e.target.value })}
              >
                <option value="">{actions.isError ? "Could not load the actions" : "Any change"}</option>
                {actionGroups.map(([prefix, list]) => {
                  const group = GROUP_NAME[prefix] ?? prefix;
                  return (
                    <optgroup key={prefix} label={group}>
                      <option value={`${prefix}.`}>Every {group.toLowerCase()} change</option>
                      {list.map((a) => (
                        <option key={a.action} value={a.action}>
                          {a.label}
                        </option>
                      ))}
                    </optgroup>
                  );
                })}
              </Select>
            </Field>
            <Field label="Object type" htmlFor="audit-target" hint="Such as site, flag or api_key.">
              <TextInput
                id="audit-target"
                data
                list="audit-target-types"
                spellCheck={false}
                autoCapitalize="off"
                placeholder="Any"
                value={filters.targetType}
                onChange={(e) => setFilters({ ...filters, targetType: e.target.value })}
              />
              <datalist id="audit-target-types">
                {TARGET_SUGGESTIONS.map((t) => (
                  <option key={t} value={t} />
                ))}
              </datalist>
            </Field>
            <Field label="From" htmlFor="audit-from">
              <TextInput
                id="audit-from"
                type="date"
                data
                value={filters.from}
                max={filters.to || undefined}
                onChange={(e) => setFilters({ ...filters, from: e.target.value })}
              />
            </Field>
            <Field label="To" htmlFor="audit-to" hint="Both days are included, in your timezone.">
              <TextInput
                id="audit-to"
                type="date"
                data
                value={filters.to}
                min={filters.from || undefined}
                onChange={(e) => setFilters({ ...filters, to: e.target.value })}
              />
            </Field>
          </div>

          {events.isPending ? (
            <Waiting rows={5} />
          ) : events.isError ? (
            <ErrorState
              title={refused ? "Only admins can read the audit log" : "Could not load the audit log"}
              detail={
                refused
                  ? "Reading an organisation's audit log needs the admin or owner role in it. Ask the owner, or choose another organisation."
                  : errText(events.error)
              }
              onRetry={refused ? undefined : () => void events.refetch()}
            />
          ) : rows.length === 0 ? (
            filtered ? (
              <Empty
                title="Nothing matches these filters"
                body="No recorded change fits every filter above. Widen the dates or clear a filter."
                action={<Button onClick={clear}>Clear the filters</Button>}
              />
            ) : (
              <Empty
                title="No changes recorded yet"
                body="Changes to sites, people, keys and flags appear here."
              />
            )
          ) : (
            <div className="mf-sheet-wrap">
              <table className="mf-sheet mf-ws-audit">
                <thead>
                  <tr>
                    <th scope="col">When</th>
                    <th scope="col">Who</th>
                    <th scope="col">What</th>
                    <th scope="col">Changed</th>
                    <th scope="col">Site</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((e) => {
                    const open = openId === e.id;
                    const kind = actorKind(e.actor);
                    const site = siteName(e.site_id);
                    return (
                      <AuditRow
                        key={e.id}
                        e={e}
                        open={open}
                        onToggle={() => setOpenId(open ? null : e.id)}
                        cells={[
                          <span key="who" className="mf-ws-audit__who">
                            {e.actor.label || "Unknown"}
                            {kind ? <span className="mf-ws-audit__kind">{kind}</span> : null}
                          </span>,
                          <span key="what">{labels.get(e.action) ?? <span className="mf-data">{e.action}</span>}</span>,
                          <span key="target" className="mf-ws-audit__target">
                            {e.target.label || e.target.id || "—"}
                            <span className="mf-ws-audit__kind">{e.target.type}</span>
                          </span>,
                          <span key="site" className="mf-data">
                            {site ?? "—"}
                          </span>,
                        ]}
                      />
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}

          <div className="mf-set-foot">
            {events.hasNextPage ? (
              <Button disabled={events.isFetchingNextPage} onClick={() => void events.fetchNextPage()}>
                {events.isFetchingNextPage ? "Loading…" : "Load more"}
              </Button>
            ) : null}
            {rows.length ? (
              <span className="mf-set-foot__say">
                {num(rows.length)} change{rows.length === 1 ? "" : "s"} shown
                {events.hasNextPage ? ", more are older" : ", the whole log for these filters"}.
              </span>
            ) : null}
            {events.isFetchNextPageError ? (
              <span className="mf-set-foot__say" data-tone="warn" role="alert">
                {errText(events.error)}
              </span>
            ) : null}
            <span className="mf-set-foot__spacer" />
            <a className="mf-btn mf-btn--key mf-btn--md" href={exportUrl}>
              <span className="mf-btn__icon">
                <IconDownload size={14} />
              </span>
              <span className="mf-btn__label">Download CSV</span>
            </a>
          </div>
          <p className="mf-ws-foot-note">
            The CSV holds every change matching the filters above, up to 50,000 rows, with the before and
            after values in full.
          </p>
        </>
      )}
    </Panel>
  );
}

/** One change and, when opened, its detail on the row under it. */
function AuditRow({
  e,
  open,
  onToggle,
  cells,
}: {
  e: AuditEvent;
  open: boolean;
  onToggle: () => void;
  cells: React.ReactNode[];
}) {
  const detailId = `audit-${e.id}`;
  return (
    <>
      <tr className={open ? "is-open" : undefined}>
        <td>
          <button
            type="button"
            className="mf-ws-audit__toggle"
            aria-expanded={open}
            aria-controls={detailId}
            onClick={onToggle}
          >
            <IconChevron size={13} className="mf-ws-audit__caret" />
            <time dateTime={e.created_at} className="mf-data">
              {when(e.created_at)}
            </time>
            <span className="mf-sr">{open ? ", hide the details" : ", show the details"}</span>
          </button>
        </td>
        {cells.map((c, i) => (
          <td key={i}>{c}</td>
        ))}
      </tr>
      {open ? (
        <tr className="mf-ws-audit__more" id={detailId}>
          <td colSpan={cells.length + 1}>
            <EventDetail e={e} />
          </td>
        </tr>
      ) : null}
    </>
  );
}

export default AuditLogSettings;
