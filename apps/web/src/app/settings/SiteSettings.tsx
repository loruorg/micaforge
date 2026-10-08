/**
 * SITE DETAILS: what this site is called, where it is, and what it keeps.
 *
 * The third panel is the one that matters most and is the one a competitor's
 * settings screen does not have: a plain statement of what Micaforge stores
 * about a visitor. The privacy claim is an engineering fact here, so it is
 * reported like any other setting rather than sold.
 */
import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { CopyField, Panel, PanelHead, Button } from "../../components/react/ui";
import { IconCheck, IconReplay } from "../../components/react/icons";
import { api } from "../../lib/api";
import { ago } from "../../lib/format";
import { useSite } from "../providers";
import {
  Caution,
  Foot,
  QueryState,
  Result,
  Select,
  Spec,
  SpecRow,
  Stated,
  TextInput,
  useSyncedForm,
} from "./SettingsLayout";

/* ── the site record, shared with the tracking screen ────────────────────── */

export interface Capture {
  outbound: boolean;
  downloads: boolean;
  forms: boolean;
  not_found: boolean;
  web_vitals: boolean;
  scroll: boolean;
}

export interface TrackingOptions {
  capture: Capture;
  hash_routing: boolean;
  respect_dnt: boolean;
  excluded_paths: string[];
}

export const DEFAULT_TRACKING: TrackingOptions = {
  capture: {
    outbound: true,
    downloads: true,
    forms: false,
    not_found: true,
    web_vitals: true,
    scroll: true,
  },
  hash_routing: false,
  respect_dnt: false,
  excluded_paths: [],
};

export interface SiteRecord {
  id: string;
  site_id: number;
  org_id: string;
  domain: string;
  name: string;
  timezone: string;
  public: boolean;
  salt_rotated_at: string | null;
  created_at: string;
  settings: { tracking?: Partial<TrackingOptions> } & Record<string, unknown>;
}

/** Both settings screens read one record, so they share one cache entry. */
export function useSiteRecord() {
  const { site, siteId } = useSite();
  const id = (site?.id as string | undefined) ?? String(siteId ?? "");
  const query = useQuery({
    queryKey: ["settings", "site", id],
    queryFn: () => api.get<SiteRecord>(`/api/sites/${id}`).then((r) => r.data),
    enabled: !!id,
  });
  return { id, query };
}

interface InstanceConfig {
  version?: string;
  edition?: string;
  retention_days?: number;
  self_hosted?: boolean;
}

function timeZones(): string[] {
  try {
    const f = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf;
    return f ? f("timeZone") : [];
  } catch {
    return [];
  }
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function SiteSettings() {
  const { id, query } = useSiteRecord();
  const qc = useQueryClient();
  const zones = useMemo(timeZones, []);

  const config = useQuery({
    queryKey: ["settings", "config"],
    queryFn: () => api.get<InstanceConfig>("/api/config").then((r) => r.data),
    staleTime: 5 * 60_000,
  });

  const details = useSyncedForm(
    query.data
      ? { name: query.data.name, domain: query.data.domain, timezone: query.data.timezone }
      : undefined,
  );

  const save = useMutation({
    mutationFn: (body: { name: string; domain: string; timezone: string }) =>
      api.patch<SiteRecord>(`/api/sites/${id}`, body).then((r) => r.data),
    onSuccess: (data) => qc.setQueryData(["settings", "site", id], data),
  });

  const rotate = useMutation({
    mutationFn: () => api.post<SiteRecord>(`/api/sites/${id}/rotate-salt`).then((r) => r.data),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["settings", "site", id] }),
  });

  return (
    <>
      <Panel>
        <PanelHead
          title="Site details"
          note="The domain is what the tracker checks its hits against, and the timezone is the day boundary every figure on this site is bucketed by."
        />
        <QueryState query={query} loading={undefined}>
          {(site) => {
            const f = details.form ?? { name: site.name, domain: site.domain, timezone: site.timezone };
            return (
              <>
                <Spec>
                  <SpecRow name="Name" hint="Shown in the site list and in scheduled reports." htmlFor="site-name">
                    <TextInput
                      id="site-name"
                      value={f.name}
                      onChange={(e) => details.set({ name: e.target.value })}
                      placeholder="Docs and blog"
                    />
                  </SpecRow>

                  <SpecRow
                    name="Domain"
                    hint="No scheme and no trailing slash. Hits from other hostnames are still recorded and reported under their own hostname."
                    htmlFor="site-domain"
                  >
                    <TextInput
                      id="site-domain"
                      data
                      value={f.domain}
                      spellCheck={false}
                      onChange={(e) => details.set({ domain: e.target.value })}
                      placeholder="example.com"
                    />
                  </SpecRow>

                  <SpecRow
                    name="Timezone"
                    hint="Days, weeks and months are cut on this clock. Changing it re-buckets every report."
                    htmlFor="site-tz"
                  >
                    {zones.length ? (
                      <Select
                        id="site-tz"
                        value={f.timezone}
                        onChange={(e) => details.set({ timezone: e.target.value })}
                      >
                        {(zones.includes(f.timezone) ? zones : [f.timezone, ...zones]).map((z) => (
                          <option key={z} value={z}>
                            {z}
                          </option>
                        ))}
                      </Select>
                    ) : (
                      <TextInput
                        id="site-tz"
                        data
                        value={f.timezone}
                        onChange={(e) => details.set({ timezone: e.target.value })}
                        placeholder="Europe/Lisbon"
                      />
                    )}
                  </SpecRow>
                </Spec>

                <Foot>
                  <Button
                    tone="primary"
                    icon={<IconCheck size={14} />}
                    disabled={!details.dirty || save.isPending}
                    onClick={() => save.mutate(f)}
                  >
                    Save details
                  </Button>
                  <Button tone="quiet" disabled={!details.dirty} onClick={details.reset}>
                    Discard
                  </Button>
                  <span className="mf-set-foot__spacer" />
                  <Result action={save} done="Details saved." />
                </Foot>
              </>
            );
          }}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="How this site is identified"
          note="The site number is what the tracker, the log shipper and the stats API all refer to. The site id addresses this site in the management API and in MCP."
        />
        <QueryState query={query} loading={undefined}>
          {(site) => (
            <div className="mf-set-block">
              <div className="mf-set-stack">
                <div className="mf-set-pair">
                  <CopyField label="Site number" value={String(site.site_id)} />
                  <CopyField label="Site id" value={site.id} />
                </div>
                <p className="mf-set-note">
                  Collecting data since{" "}
                  <span className="mf-data">
                    {new Date(site.created_at).toLocaleDateString(undefined, {
                      year: "numeric",
                      month: "long",
                      day: "numeric",
                    })}
                  </span>
                  .
                </p>
              </div>
            </div>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Identity and retention"
          note="What Micaforge keeps about a visitor, stated plainly. There is no cookie, no device id and no raw address on a visitor's row."
        />
        <QueryState query={query} loading={undefined}>
          {(site) => (
            <>
              <Spec>
                <SpecRow name="Visitor identity" hint="A hash, not a person. It cannot be reversed and it is not kept.">
                  <Stated>
                    <span className="mf-data">blake3(daily salt + site + address + user agent)</span>, rotated at
                    00:00 {site.timezone} and never stored beyond 48 hours.
                  </Stated>
                </SpecRow>
                <SpecRow name="Salt last rotated">
                  <Stated>
                    <span className="mf-data">
                      {site.salt_rotated_at ? ago(site.salt_rotated_at) : "not yet rotated"}
                    </span>
                  </Stated>
                </SpecRow>
                <SpecRow
                  name="Event retention"
                  hint="Instance-wide, set by MICAFORGE_RETENTION_DAYS at start. It is not per site."
                >
                  <Stated>
                    {config.isPending ? (
                      <span className="mf-wait" style={{ width: "9rem" }} />
                    ) : config.isError ? (
                      "Could not read the instance configuration."
                    ) : config.data?.retention_days ? (
                      <>
                        <span className="mf-data">{config.data.retention_days}</span> days, then dropped by
                        partition.
                      </>
                    ) : (
                      "Kept indefinitely. No TTL is set on this instance."
                    )}
                  </Stated>
                </SpecRow>
              </Spec>

              <Foot>
                <Caution>
                  Rotating now starts a new set of hashes immediately. Visitors already counted today will be
                  counted again as new. Nothing already recorded changes.
                </Caution>
              </Foot>
              <Foot tight>
                <Button
                  icon={<IconReplay size={14} />}
                  disabled={rotate.isPending}
                  onClick={() => rotate.mutate()}
                >
                  Rotate the visitor salt now
                </Button>
                <span className="mf-set-foot__spacer" />
                <Result action={rotate} done="Salt rotated." working="Rotating…" />
              </Foot>
            </>
          )}
        </QueryState>
      </Panel>
    </>
  );
}

export default SiteSettings;
