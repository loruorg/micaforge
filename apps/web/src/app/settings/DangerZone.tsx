/**
 * DANGER ZONE.
 *
 * Every action here is confirmed by transcribing the domain it will act on, and
 * every one states what it destroys in counted, itemised terms before the field
 * appears. No modal, no "are you sure": you sign the record.
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router";

import { Panel, PanelHead } from "../../components/react/ui";
import { IconDownload } from "../../components/react/icons";
import { api, type AgentOverview, type Overview } from "../../lib/api";
import { num } from "../../lib/format";
import { useSite } from "../providers";
import {
  asSite,
  Block,
  Note,
  Result,
  Select,
  Spec,
  SpecRow,
  TypedConfirm,
} from "./SettingsLayout";
import { useSiteRecord } from "./SiteSettings";

interface OrgSummary {
  id: string;
  name: string;
  role?: string;
}

export function DangerZone() {
  const { siteId, domain } = useSite();
  const { id: recordId, query: siteQuery } = useSiteRecord();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const phrase = domain || siteQuery.data?.domain || "this site";

  // What is actually on the record, so the warnings count rather than gesture.
  const readers = useQuery({
    queryKey: ["stats", "overview", siteId, "all", "human", "[]"],
    queryFn: () =>
      api.get<Overview>("/api/stats/overview", { site: asSite(siteId), range: "all", audience: "human" }).then((r) => r.data),
    enabled: siteId != null,
    staleTime: 60_000,
  });
  const machines = useQuery({
    queryKey: ["stats", "agents/overview", siteId, "all", "agent", "[]"],
    queryFn: () =>
      api.get<AgentOverview>("/api/agents/overview", { site: asSite(siteId), range: "all", audience: "agent" }).then((r) => r.data),
    enabled: siteId != null,
    staleTime: 60_000,
  });

  const orgs = useQuery({
    queryKey: ["settings", "orgs"],
    queryFn: () => api.get<OrgSummary[]>("/api/orgs").then((r) => r.data),
    staleTime: 5 * 60_000,
  });

  const [targetOrg, setTargetOrg] = useState("");

  const reset = useMutation({
    mutationFn: () => api.post<null>(`/api/sites/${recordId}/reset`),
    onSuccess: () => {
      void qc.invalidateQueries();
    },
  });

  const transfer = useMutation({
    mutationFn: () => api.post<null>(`/api/sites/${recordId}/transfer`, { org_id: targetOrg }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["settings", "site", recordId] });
    },
  });

  const destroy = useMutation({
    mutationFn: () => api.del<null>(`/api/sites/${recordId}`),
    onSuccess: () => {
      qc.clear();
      navigate("/app");
    },
  });

  const counted = (() => {
    const r = readers.data ? `${num(readers.data.pageviews)} reader events` : "every reader event";
    const m = machines.data ? `${num(machines.data.fetches)} machine fetches` : "every machine fetch";
    return `${r} and ${m}`;
  })();

  const exportUrl = api.exportUrl("/api/stats/export.csv", {
    site: asSite(siteId),
    range: "all",
    audience: "all",
  });

  const targetName = orgs.data?.find((o) => o.id === targetOrg)?.name ?? "another organisation";

  return (
    <Panel>
      <PanelHead
        title="Danger zone"
        note="Three actions that cannot be undone. Each one names what it destroys and asks you to write the domain out before it will run."
      />

      <Block
        title="Reset the record"
        body="Wipes every event this site has ever recorded and leaves the site itself in place: same site number, same ingest key, same goals, alerts, reports and share links. The tag keeps working and a new record starts from the next hit."
      >
        <div className="mf-set-row">
          <a className="mf-btn mf-btn--key mf-btn--md" href={exportUrl}>
            <span className="mf-btn__icon">
              <IconDownload size={14} />
            </span>
            <span className="mf-btn__label">Export the record first</span>
          </a>
          <Note>A CSV of everything currently on the record, both traces, whole history.</Note>
        </div>

        <TypedConfirm
          phrase={phrase}
          label="Reset the record"
          busy={reset.isPending}
          destroys={
            <>
              Destroys {counted} for {phrase}, along with every session, replay and error grouped from them.
              Nothing is exported first and nothing can be recovered afterwards.
            </>
          }
          onConfirm={() => reset.mutate()}
        />
        <div className="mf-set-row">
          <Result action={reset} done="The record is empty. The next hit starts a new one." working="Resetting…" />
        </div>
      </Block>

      <Block
        title="Transfer the site"
        body="Moves this site, its record and everything attached to it into another organisation you belong to."
      >
        <Spec>
          <SpecRow name="Move to" hint="Only organisations where you can create sites are listed." htmlFor="dz-org">
            <Select
              id="dz-org"
              value={targetOrg}
              onChange={(e) => setTargetOrg(e.target.value)}
              disabled={orgs.isPending || !orgs.data?.length}
            >
              <option value="">Choose an organisation</option>
              {(orgs.data ?? [])
                .filter((o) => o.id !== siteQuery.data?.org_id)
                .map((o) => (
                  <option key={o.id} value={o.id}>
                    {o.name}
                  </option>
                ))}
            </Select>
          </SpecRow>
        </Spec>
        {targetOrg ? (
          <TypedConfirm
            phrase={phrase}
            label="Transfer the site"
            busy={transfer.isPending}
            destroys={
              <>
                Moves {phrase} to {targetName}. Everyone in the current organisation loses access unless they
                are also members there, and every per-site role override on this site is dropped. You cannot
                move it back unless you can administer {targetName}.
              </>
            }
            onConfirm={() => transfer.mutate()}
          />
        ) : (
          <Note>Choose a destination organisation and the confirmation appears.</Note>
        )}
        <div className="mf-set-row">
          <Result action={transfer} done="Site transferred." working="Transferring…" />
        </div>
      </Block>

      <Block
        title="Delete the site"
        body="Removes the site from Micaforge entirely. This is the only action here that takes the site number with it."
      >
        <TypedConfirm
          phrase={phrase}
          label="Delete this site"
          busy={destroy.isPending}
          destroys={
            <>
              Destroys {phrase}: {counted}, the ingest key, every goal, funnel, segment, dashboard, feature
              flag, experiment, alert, report and share link belonging to it, and any API key scoped to it
              alone. The site number is retired and not reused. Nothing here is recoverable.
            </>
          }
          onConfirm={() => destroy.mutate()}
        />
        <div className="mf-set-row">
          <Result action={destroy} done="Site deleted." working="Deleting…" />
        </div>
      </Block>
    </Panel>
  );
}

export default DangerZone;
