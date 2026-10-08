/**
 * THE INSTANCE: the self-host admin screen.
 *
 * Whoever runs this instance is the person who has to answer for the disk it is
 * filling and the queue it is draining, so the storage and job panels report in
 * the units they will actually have to act on: bytes on disk per table, and the
 * age of the oldest job nobody has picked up.
 *
 * A reader without the instance-admin role sees one plain panel saying so. It
 * is not an error, and it is not dressed as one.
 */
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import {
  Button,
  Empty,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  type Channel,
  type Column,
} from "../../components/react/ui";
import { IconReplay } from "../../components/react/icons";
import { ApiError, api } from "../../lib/api";
import { ago, bytes, compact, duration, num } from "../../lib/format";
import {
  Foot,
  Note,
  QueryState,
  Result,
  Status,
  TextInput,
  errText,
} from "./SettingsLayout";
import "../../styles/app/settings.css";

interface AdminOverview {
  version: string;
  edition?: string;
  self_hosted?: boolean;
  uptime_s?: number;
  users: number;
  organizations: number;
  sites: number;
  events_30d?: number;
}

interface StorageTable {
  table: string;
  rows: number;
  bytes: number;
  parts?: number;
  oldest?: string | null;
  newest?: string | null;
}

interface JobHealth {
  pending: number;
  running: number;
  failed: number;
  oldest_pending_at: string | null;
  by_kind: { kind: string; pending: number; failed: number; last_run_at: string | null }[];
}

interface AdminUser {
  id: string;
  email: string;
  name: string;
  created_at: string;
  organizations?: number;
  last_seen_at?: string | null;
}

interface AdminOrg {
  id: string;
  name: string;
  slug: string;
  members: number;
  sites: number;
  created_at: string;
}

interface AdminSite {
  id: string;
  site_id: number;
  domain: string;
  org: string;
  events_30d?: number;
  created_at: string;
}

/** Keeps a search box from asking the server on every keystroke. */
function useDebounced<T>(value: T, ms = 250): T {
  const [held, setHeld] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setHeld(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return held;
}

export function AdminScreen() {
  const qc = useQueryClient();
  const [userQ, setUserQ] = useState("");
  const q = useDebounced(userQ);

  const overview = useQuery({
    queryKey: ["admin", "overview"],
    queryFn: () => api.get<AdminOverview>("/api/admin/overview").then((r) => r.data),
    retry: false,
  });

  const forbidden = overview.error instanceof ApiError && overview.error.status === 403;

  const storage = useQuery({
    queryKey: ["admin", "storage"],
    queryFn: () => api.get<StorageTable[]>("/api/admin/storage").then((r) => r.data),
    enabled: !forbidden,
  });

  const jobs = useQuery({
    queryKey: ["admin", "jobs"],
    queryFn: () => api.get<JobHealth>("/api/admin/jobs").then((r) => r.data),
    enabled: !forbidden,
    refetchInterval: 15_000,
  });

  const users = useQuery({
    queryKey: ["admin", "users", q],
    queryFn: () =>
      api.get<AdminUser[]>(`/api/admin/users?limit=50&q=${encodeURIComponent(q)}`).then((r) => r.data),
    enabled: !forbidden,
  });

  const orgs = useQuery({
    queryKey: ["admin", "orgs"],
    queryFn: () => api.get<AdminOrg[]>("/api/admin/orgs?limit=50").then((r) => r.data),
    enabled: !forbidden,
  });

  const sites = useQuery({
    queryKey: ["admin", "sites"],
    queryFn: () => api.get<AdminSite[]>("/api/admin/sites?limit=50").then((r) => r.data),
    enabled: !forbidden,
  });

  const retryJobs = useMutation({
    mutationFn: () => api.post<{ retried: number }>("/api/admin/jobs/retry-failed"),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["admin", "jobs"] }),
  });

  if (forbidden) {
    return (
      <div className="mf-set-page">
        <h1 className="mf-set-page__title">Instance</h1>
        <Panel>
          <Empty
            title="This page is for the instance administrator"
            body="Your account can read the sites it has been given, but not the users, storage or job queue of the whole instance. Whoever installed this Micaforge can grant that."
          />
        </Panel>
      </div>
    );
  }

  const storageChannels = (rows: StorageTable[]): Channel[] => {
    const totalRows = rows.reduce((a, r) => a + (r.rows ?? 0), 0);
    const totalBytes = rows.reduce((a, r) => a + (r.bytes ?? 0), 0);
    return [
      { key: "tables", label: "Tables", value: String(rows.length) },
      { key: "rows", label: "Rows stored", value: compact(totalRows) },
      { key: "disk", label: "On disk", value: bytes(totalBytes) },
    ];
  };

  const storageColumns: Column<StorageTable>[] = [
    { key: "t", head: "Table", render: (r) => <span className="mf-data">{r.table}</span> },
    { key: "rows", head: "Rows", numeric: true, render: (r) => <span className="mf-data">{num(r.rows)}</span> },
    { key: "b", head: "On disk", numeric: true, render: (r) => <span className="mf-data">{bytes(r.bytes)}</span> },
    {
      key: "parts",
      head: "Parts",
      numeric: true,
      render: (r) => <span className="mf-data">{typeof r.parts === "number" ? num(r.parts) : "—"}</span>,
    },
    {
      key: "span",
      head: "Oldest row",
      numeric: true,
      render: (r) => <span className="mf-data">{r.oldest ? ago(r.oldest) : "—"}</span>,
    },
  ];

  const jobColumns: Column<JobHealth["by_kind"][number]>[] = [
    { key: "k", head: "Job", render: (j) => <span className="mf-data">{j.kind}</span> },
    { key: "p", head: "Waiting", numeric: true, render: (j) => <span className="mf-data">{num(j.pending)}</span> },
    {
      key: "f",
      head: "Failed",
      numeric: true,
      render: (j) =>
        j.failed > 0 ? (
          <Status state="failed" label={num(j.failed)} />
        ) : (
          <span className="mf-data">0</span>
        ),
    },
    {
      key: "l",
      head: "Last run",
      numeric: true,
      render: (j) => <span className="mf-data">{j.last_run_at ? ago(j.last_run_at) : "never"}</span>,
    },
  ];

  const userColumns: Column<AdminUser>[] = [
    {
      key: "u",
      head: "Person",
      render: (u) => (
        <span>
          {u.name || "no name"} <span className="mf-data">{u.email}</span>
        </span>
      ),
    },
    {
      key: "o",
      head: "Organisations",
      numeric: true,
      width: "16%",
      render: (u) => <span className="mf-data">{typeof u.organizations === "number" ? u.organizations : "—"}</span>,
    },
    {
      key: "seen",
      head: "Last seen",
      numeric: true,
      width: "16%",
      render: (u) => <span className="mf-data">{u.last_seen_at ? ago(u.last_seen_at) : "—"}</span>,
    },
    {
      key: "c",
      head: "Joined",
      numeric: true,
      width: "16%",
      render: (u) => <span className="mf-data">{ago(u.created_at)}</span>,
    },
  ];

  const orgColumns: Column<AdminOrg>[] = [
    {
      key: "n",
      head: "Organisation",
      render: (o) => (
        <span>
          {o.name} <span className="mf-data">{o.slug}</span>
        </span>
      ),
    },
    { key: "m", head: "People", numeric: true, width: "14%", render: (o) => <span className="mf-data">{num(o.members)}</span> },
    { key: "s", head: "Sites", numeric: true, width: "14%", render: (o) => <span className="mf-data">{num(o.sites)}</span> },
    { key: "c", head: "Created", numeric: true, width: "16%", render: (o) => <span className="mf-data">{ago(o.created_at)}</span> },
  ];

  const siteColumns: Column<AdminSite>[] = [
    { key: "d", head: "Domain", render: (s) => <span className="mf-data">{s.domain}</span> },
    { key: "n", head: "Number", numeric: true, width: "12%", render: (s) => <span className="mf-data">{s.site_id}</span> },
    { key: "o", head: "Organisation", width: "22%", render: (s) => <span>{s.org}</span> },
    {
      key: "e",
      head: "Events, 30 days",
      numeric: true,
      width: "18%",
      render: (s) => <span className="mf-data">{typeof s.events_30d === "number" ? num(s.events_30d) : "—"}</span>,
    },
  ];

  return (
    <div className="mf-set-page">
      <div>
        <h1 className="mf-set-page__title">Instance</h1>
        <p className="mf-set-page__note">
          {overview.data ? (
            <>
              Micaforge <span className="mf-set-admin__version">{overview.data.version}</span>
              {overview.data.edition ? ` · ${overview.data.edition}` : ""}
              {overview.data.self_hosted ? " · self-hosted" : ""}
              {typeof overview.data.uptime_s === "number" ? ` · up ${duration(overview.data.uptime_s)}` : ""}
            </>
          ) : (
            "Everything running on this installation: the people, the data store and the job queue behind it."
          )}
        </p>
      </div>

      <Panel>
        <PanelHead title="What this instance holds" />
        <QueryState query={overview}>
          {(o) => (
            <Readout
              channels={[
                { key: "u", label: "People", value: num(o.users) },
                { key: "o", label: "Organisations", value: num(o.organizations) },
                { key: "s", label: "Sites", value: num(o.sites) },
                {
                  key: "e",
                  label: "Events, 30 days",
                  value: typeof o.events_30d === "number" ? compact(o.events_30d) : "—",
                },
              ]}
            />
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Event store"
          note="ClickHouse, table by table. Retention is applied as a TTL at start, so a table that keeps growing is a table with no TTL set."
        />
        <QueryState query={storage} emptyWhen={(r) => !r.length} empty={<Empty title="No tables reported" body="The server answered, but named no tables. Check that ClickHouse is reachable from the API." />}>
          {(rows) => (
            <>
              <Readout channels={storageChannels(rows)} dense />
              <Sheet rows={rows} columns={storageColumns} getKey={(r) => r.table} />
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Job queue"
          note="Crawls, imports, report sends and policy re-fetches. A job that has failed is retried on a back-off; one that keeps failing waits here."
        />
        <QueryState query={jobs}>
          {(j) => (
            <>
              <Readout
                dense
                channels={[
                  { key: "p", label: "Waiting", value: num(j.pending) },
                  { key: "r", label: "Running", value: num(j.running) },
                  { key: "f", label: "Failed", value: num(j.failed) },
                  {
                    key: "o",
                    label: "Oldest waiting",
                    value: j.oldest_pending_at ? ago(j.oldest_pending_at) : "—",
                  },
                ]}
              />
              <Sheet
                rows={j.by_kind ?? []}
                columns={jobColumns}
                getKey={(k) => k.kind}
                empty={<Empty title="The queue is empty" body="Nothing is waiting and nothing has failed." />}
              />
              <Foot>
                <Button
                  icon={<IconReplay size={14} />}
                  disabled={!j.failed || retryJobs.isPending}
                  onClick={() => retryJobs.mutate()}
                >
                  Retry every failed job
                </Button>
                <span className="mf-set-foot__spacer" />
                <Result action={retryJobs} done="Failed jobs queued again." working="Queueing…" />
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="People"
          note="Everyone with an account on this instance."
          actions={
            <TextInput
              aria-label="Search people by name or address"
              placeholder="Search"
              value={userQ}
              onChange={(e) => setUserQ(e.target.value)}
            />
          }
        />
        <QueryState query={users}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={userColumns}
                getKey={(u) => u.id}
                empty={
                  <Empty
                    title={q ? "Nobody matches that" : "No accounts yet"}
                    body={q ? "Try part of an address instead." : "The first account created becomes the instance administrator."}
                  />
                }
              />
              {users.isError ? (
                <Foot tight>
                  <span className="mf-set-foot__say" data-tone="warn" role="alert">
                    {errText(users.error)}
                  </span>
                </Foot>
              ) : null}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead title="Organisations" />
        <QueryState query={orgs}>
          {(rows) => (
            <Sheet
              rows={rows}
              columns={orgColumns}
              getKey={(o) => o.id}
              empty={<Empty title="No organisation yet" body="One is created with the first account." />}
            />
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead title="Sites" note="Site numbers are issued in order and never reused, including after a delete." />
        <QueryState query={sites}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={siteColumns}
                getKey={(s) => s.id}
                empty={<Empty title="No site is being recorded" body="A site appears here the moment anyone adds one." />}
              />
              <Foot tight>
                <Note>The first fifty by creation date. Use the stats API for the whole list.</Note>
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>
    </div>
  );
}

export default AdminScreen;
