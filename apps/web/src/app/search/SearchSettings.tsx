/**
 * The Search screen's Settings tab: connect Google Search Console, choose the
 * property this site reads, see how far the sync has come, and disconnect.
 *
 * Connecting leaves the dashboard for Google's consent screen and comes back
 * here with `?gsc=connected` or `?gsc_error=<code>`, which SearchScreen reads
 * and shows above this panel. Every step is in the page: disconnecting asks
 * once, inline, with no dialog.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, CopyField, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { api } from "../../lib/api";
import { ago, num } from "../../lib/format";
import { Choice, Field, Say, WaitLines } from "../growth/controls";
import { moment, problem } from "../growth/model";
import { dayWords, propertyName } from "./model";
import type { SearchProperty, SearchStatus } from "./model";

/** Start the Google consent flow: the server answers the address to open. */
function useConnect(siteId: number) {
  return useMutation({
    mutationFn: () =>
      api
        .post<{ url: string }>(`/api/search-console/connect?site=${encodeURIComponent(String(siteId))}`)
        .then((r) => r.data.url),
    onSuccess: (url) => {
      window.location.assign(url);
    },
  });
}

/** The connect button, for an admin. */
export function ConnectButton({
  siteId,
  label = "Connect Google Search Console",
  tone = "primary",
}: {
  siteId: number;
  label?: string;
  tone?: "primary" | "key";
}) {
  const connect = useConnect(siteId);
  return (
    <div className="mf-sc-act">
      <Button tone={tone} disabled={connect.isPending} onClick={() => connect.mutate()}>
        {connect.isPending ? "Opening Google…" : label}
      </Button>
      {connect.isError ? <Say tone="warn">{problem(connect.error)}</Say> : null}
    </div>
  );
}

/** What the operator sets, when the server has no Google client. */
export function NotConfigured({ redirectUri }: { redirectUri: string }) {
  return (
    <Panel>
      <PanelHead
        title="Google Search Console is not configured on this server"
        note="The operator of this Micaforge server switches it on. Until then nothing is sent to Google."
      />
      <div className="mf-gr-pad">
        <p className="mf-gr-aside">
          Create an OAuth client of type “Web application” in Google Cloud, turn on the Google Search Console API,
          register the redirect address below, then set <code className="mf-data">MICAFORGE_SEARCH_CONSOLE_CLIENT_ID</code>{" "}
          and <code className="mf-data">MICAFORGE_SEARCH_CONSOLE_CLIENT_SECRET</code> and restart the server.
        </p>
        <CopyField value={redirectUri} label="Redirect address to register at Google" />
        <p className="mf-gr-aside">
          The steps are in the documentation under Integrations, Google Search Console.
        </p>
      </div>
    </Panel>
  );
}

/** Choose the property this site reads. */
function PropertyPicker({ siteId, current }: { siteId: number; current: string | null }) {
  const qc = useQueryClient();
  const [chosen, setChosen] = useState<string>(current ?? "");
  const list = useQuery({
    queryKey: ["search-console", "properties", siteId],
    queryFn: () =>
      api
        .get<SearchProperty[]>(`/api/search-console/properties?site=${encodeURIComponent(String(siteId))}`)
        .then((r) => r.data),
  });
  const save = useMutation({
    mutationFn: (property: string) =>
      api.put(`/api/search-console/property?site=${encodeURIComponent(String(siteId))}`, { property }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["search-console"] });
    },
  });

  if (list.isPending) return <WaitLines lines={2} label="Asking Google for your properties…" />;
  if (list.isError) {
    return (
      <ErrorState
        title="Could not list your Search Console properties"
        detail={problem(list.error)}
        onRetry={() => void list.refetch()}
      />
    );
  }
  const items = list.data;
  if (!items.length) {
    return (
      <Say tone="warn">
        The connected Google account cannot read any Search Console property. Add this site to Search Console with that
        account, or connect with an account that already has it.
      </Say>
    );
  }
  const value = chosen || items[0].site_url;
  return (
    <div className="mf-sc-pick">
      <Field
        label="Property"
        hint="A domain property covers every address on the domain; a URL prefix covers one address and what is under it."
      >
        {(id, describedBy) => (
          <Choice id={id} aria-describedby={describedBy} value={value} onChange={(e) => setChosen(e.target.value)}>
            {items.map((p) => (
              <option key={p.site_url} value={p.site_url}>
                {propertyName(p.site_url)}
              </option>
            ))}
          </Choice>
        )}
      </Field>
      <div className="mf-sc-act">
        <Button
          tone="primary"
          disabled={save.isPending || value === current}
          onClick={() => save.mutate(value)}
        >
          {save.isPending ? "Saving…" : current ? "Read this property instead" : "Read this property"}
        </Button>
        {save.isError ? <Say tone="warn">{problem(save.error)}</Say> : null}
        {save.isSuccess ? (
          <Say tone="good">Saved. The first sync has started and fills the last 30 days first.</Say>
        ) : null}
      </div>
    </div>
  );
}

/** The sync's progress, in plain words. */
function SyncFacts({ status }: { status: SearchStatus }) {
  const c = status.connection;
  if (!c) return null;
  const rows: { label: string; value: string }[] = [
    { label: "Property", value: c.property ? propertyName(c.property) : "Not chosen yet" },
    { label: "Connected", value: moment(c.connected_at) },
    {
      label: "Data from",
      value: c.backfilled_from
        ? `${dayWords(c.backfilled_from)}${c.backfill_done ? "" : `, going back to ${dayWords(c.backfill_until)}`}`
        : "No data yet",
    },
    { label: "Data up to", value: c.synced_through ? dayWords(c.synced_through) : "No data yet" },
    {
      label: "Last sync",
      value: c.syncing
        ? "Running now"
        : c.last_sync_finished_at
          ? `${ago(c.last_sync_finished_at)} (${moment(c.last_sync_finished_at)})`
          : "Not yet",
    },
    { label: "Rows stored", value: num(c.rows_synced) },
  ];
  return (
    <dl className="mf-sc-facts">
      {rows.map((r) => (
        <div key={r.label} className="mf-sc-facts__row">
          <dt>{r.label}</dt>
          <dd>{r.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** "Sync now", for anyone who may edit the site. */
export function SyncNow({ siteId }: { siteId: number }) {
  const qc = useQueryClient();
  const sync = useMutation({
    mutationFn: () => api.post(`/api/search-console/sync?site=${encodeURIComponent(String(siteId))}`),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["search-console", "status", siteId] });
    },
  });
  return (
    <div className="mf-sc-act">
      <Button size="sm" disabled={sync.isPending} onClick={() => sync.mutate()}>
        {sync.isPending ? "Starting…" : "Sync now"}
      </Button>
      {sync.isSuccess ? <Say tone="good">Sync started. New data appears within a few minutes.</Say> : null}
      {sync.isError ? <Say tone="warn">{problem(sync.error)}</Say> : null}
    </div>
  );
}

/** Disconnect, asked once inside the page. */
function Disconnect({ siteId }: { siteId: number }) {
  const qc = useQueryClient();
  const [asking, setAsking] = useState(false);
  const drop = useMutation({
    mutationFn: () => api.del(`/api/search-console?site=${encodeURIComponent(String(siteId))}`),
    onSuccess: () => {
      setAsking(false);
      void qc.invalidateQueries({ queryKey: ["search-console"] });
    },
  });
  if (!asking) {
    return (
      <Button tone="quiet" onClick={() => setAsking(true)}>
        Disconnect Search Console
      </Button>
    );
  }
  return (
    <div className="mf-sc-confirm" role="group" aria-label="Disconnect Search Console">
      <p>
        Micaforge asks Google to revoke its access and deletes the stored grant. The data already synced stays, and comes
        back if you connect the same property again.
      </p>
      <div className="mf-sc-act">
        <Button tone="danger" disabled={drop.isPending} onClick={() => drop.mutate()}>
          {drop.isPending ? "Disconnecting…" : "Disconnect"}
        </Button>
        <Button tone="quiet" onClick={() => setAsking(false)}>
          Keep it connected
        </Button>
      </div>
      {drop.isError ? <Say tone="warn">{problem(drop.error)}</Say> : null}
    </div>
  );
}

/** The whole tab. */
export function SearchSettings({ siteId, status }: { siteId: number; status: SearchStatus }) {
  const c = status.connection;
  const manage = status.can_manage;

  if (!status.configured && !c) return <NotConfigured redirectUri={status.redirect_uri} />;

  if (!c) {
    return (
      <Panel>
        <PanelHead
          title="Connect Google Search Console"
          note="See what people searched on Google before they arrived: clicks, impressions, click-through rate and position, by search term, page, country and device."
        />
        <div className="mf-gr-pad">
          <p className="mf-gr-aside">
            Micaforge asks Google for read-only access to Search Console data, nothing else. You choose which property
            this site reads after connecting. The first sync reads the last 30 days, then fills in up to 16 months.
          </p>
          {manage ? (
            <ConnectButton siteId={siteId} />
          ) : (
            <Say>Only a site admin can connect Search Console. Ask one of this site’s admins.</Say>
          )}
        </div>
      </Panel>
    );
  }

  return (
    <>
      {c.status === "revoked" ? (
        <Panel>
          <PanelHead
            title="Google no longer accepts this connection"
            note={c.last_error ?? "The access was revoked at Google, or it expired."}
          />
          <div className="mf-gr-pad">
            <p className="mf-gr-aside">The data already synced is still shown. Connect again to keep it up to date.</p>
            {manage ? <ConnectButton siteId={siteId} label="Connect again" /> : null}
          </div>
        </Panel>
      ) : null}

      {c.status !== "revoked" && manage ? (
        <Panel>
          <PanelHead
            title={c.property ? "Property" : "Choose a property"}
            note="The Search Console property this site’s Search report reads."
          />
          <div className="mf-gr-pad">
            <PropertyPicker siteId={siteId} current={c.property} />
          </div>
        </Panel>
      ) : null}

      <Panel>
        <PanelHead
          title="Sync"
          note="Micaforge reads Search Console once a day, and reads the last three days again each time, because Google keeps revising them."
          actions={c.status === "active" ? <SyncNow siteId={siteId} /> : undefined}
        />
        <div className="mf-gr-pad">
          <SyncFacts status={status} />
          {c.last_error && c.status !== "revoked" ? (
            <Say tone="warn">
              The last sync failed{c.last_error_at ? ` ${ago(c.last_error_at)}` : ""}: {c.last_error}
            </Say>
          ) : null}
        </div>
      </Panel>

      {manage ? (
        <Panel>
          <PanelHead title="Connection" note="Connect with another Google account, or remove the connection." />
          <div className="mf-gr-pad mf-sc-row">
            {status.configured ? (
              <ConnectButton siteId={siteId} label="Connect with another Google account" tone="key" />
            ) : null}
            <Disconnect siteId={siteId} />
          </div>
        </Panel>
      ) : null}
    </>
  );
}
