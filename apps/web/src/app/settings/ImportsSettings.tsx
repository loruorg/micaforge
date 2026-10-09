/**
 * IMPORTS: adding history from an older analytics tool.
 *
 * A file is the default way in, and for GA4, Plausible and Umami the only one:
 * you export from the old tool and hand the file over, and Micaforge makes no
 * outbound call at all. PostHog and Simple Analytics can also be read through
 * their own APIs. That is the one outbound call an import ever makes, and it
 * happens only because somebody typed that provider's API key into this form,
 * which the screen says in so many words. The key is sealed on the server the
 * moment it arrives and dropped when the import ends; it is never shown again.
 */
import { useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Button, Empty, Panel, PanelHead, Sheet, type Column } from "../../components/react/ui";
import { IconCheck, IconClose, IconDownload } from "../../components/react/icons";
import { API_BASE, ApiError, api } from "../../lib/api";
import { ago, bytes, num } from "../../lib/format";
import { useSite } from "../providers";
import {
  asSite,
  Caution,
  Foot,
  Gauge,
  QueryState,
  Result,
  Select,
  Spec,
  SpecRow,
  Status,
  TextInput,
  errText,
  type StateName,
} from "./SettingsLayout";

type Source = "ga4" | "plausible" | "umami" | "posthog" | "simple_analytics";

/** The two sources with an API Micaforge can read. */
const API_SOURCES: Source[] = ["posthog", "simple_analytics"];

/** What an API import sends instead of a file. Never kept after the request. */
interface ApiFields {
  api_key: string;
  api_host: string;
  project: string;
  user_id: string;
  hostname: string;
  from: string;
  to: string;
}

const NO_FIELDS: ApiFields = {
  api_key: "",
  api_host: "",
  project: "",
  user_id: "",
  hostname: "",
  from: "",
  to: "",
};

interface ImportRecord {
  id: string;
  source: string;
  status: string;
  progress: number;
  error: string | null;
  created_at: string;
  rows?: number;
}

const SOURCE_NAME: Record<string, string> = {
  ga4: "Google Analytics 4",
  plausible: "Plausible",
  umami: "Umami",
  posthog: "PostHog",
  simple_analytics: "Simple Analytics",
};

const SOURCE_FILE: Record<Source, string> = {
  ga4: "The CSV an Exploration exports, or a BigQuery events export as newline-delimited JSON.",
  plausible: "The CSV bundle from the site's own export, or a stats API dump.",
  umami: "The JSON export from the website's settings.",
  posthog:
    "An events export as CSV or JSON (the event list's export, or a batch export with one event per line), or read it through PostHog's API.",
  simple_analytics:
    "The data points CSV from the website's export, or read it through Simple Analytics' export API.",
};

const CARRIES: { carried: boolean; what: string; detail: string }[] = [
  { carried: true, what: "Pageviews", detail: "Path, title, hostname and referrer, at their original timestamps." },
  { carried: true, what: "Sessions", detail: "Rebuilt from the export's own session boundaries, not re-cut." },
  {
    carried: true,
    what: "Channels and campaigns",
    detail: "utm values are kept as written and re-resolved by Micaforge's own channel rules, so AI answer engines appear in the history too.",
  },
  { carried: true, what: "Geography", detail: "Country, region and city wherever the export carries them." },
  { carried: true, what: "Device, browser and system", detail: "Normalised to Micaforge's own names." },
  { carried: true, what: "Custom events and goals", detail: "Plausible, Umami, PostHog and Simple Analytics events with their properties, matched to goals of the same name." },
  {
    carried: false,
    what: "PostHog's own events",
    detail: "Autocapture, page leave, identify and feature flag events have no Micaforge equivalent and are skipped. Page views, web vitals and your own events are imported.",
  },
  {
    carried: false,
    what: "People and their properties",
    detail: "PostHog's person properties, distinct ids and email addresses are never imported. Imported rows are never identified.",
  },
  {
    carried: false,
    what: "AI crawler traffic",
    detail: "No other tool records it. AI crawler history starts on the day the log shipper starts, and nothing can recover data that was never collected.",
  },
  {
    carried: false,
    what: "Visitor identity",
    detail: "Imported rows get fresh daily hashes. Someone who returned before and after the import counts as two visitors.",
  },
  { carried: false, what: "Replays, error stacks and web vitals", detail: "No export contains them." },
  { carried: false, what: "Revenue without a currency", detail: "A figure with no currency is dropped rather than guessed at." },
  {
    carried: false,
    what: "Anything the source sampled away",
    detail: "A sampled GA4 export is already an estimate. Micaforge imports what the file says and does not scale it up.",
  },
];

function stateOf(status: string): StateName {
  if (status === "done" || status === "complete" || status === "finished") return "done";
  if (status === "failed" || status === "error") return "failed";
  if (status === "running" || status === "importing") return "running";
  return "waiting";
}

function fraction(p: number | undefined): number {
  if (typeof p !== "number" || !Number.isFinite(p)) return 0;
  return p > 1 ? Math.min(1, p / 100) : Math.max(0, p);
}

/**
 * The one write in this area that cannot go through `api`: a multipart body.
 * It reproduces the same error envelope so failures read the same everywhere.
 * A file import sends the file; an API import sends the API fields and no file.
 */
async function startImport(
  site: string | number,
  source: Source,
  input: { file: File } | { fields: ApiFields },
): Promise<ImportRecord> {
  const body = new FormData();
  // `site` first: the server authorises the upload before it reads the file.
  body.set("site", String(site));
  body.set("source", source);
  if ("file" in input) {
    body.set("file", input.file);
  } else {
    for (const [name, value] of Object.entries(input.fields)) {
      if (value.trim()) body.set(name, value.trim());
    }
  }

  let res: Response;
  try {
    res = await fetch(`${API_BASE}/api/imports`, { method: "POST", body, credentials: "include" });
  } catch {
    throw new ApiError(0, "network", "Could not reach the Micaforge server.");
  }

  const text = await res.text();
  let parsed: { data?: ImportRecord; error?: { code?: string; message?: string } } = {};
  try {
    parsed = text ? JSON.parse(text) : {};
  } catch {
    throw new ApiError(res.status, "bad_response", "The server returned something that was not JSON.");
  }
  if (!res.ok) {
    throw new ApiError(
      res.status,
      parsed.error?.code ?? "error",
      parsed.error?.message ?? `The upload failed with status ${res.status}.`,
    );
  }
  return parsed.data as ImportRecord;
}

export function ImportsSettings() {
  const { siteId, domain } = useSite();
  const qc = useQueryClient();
  const key = ["settings", "imports", siteId];
  const fileRef = useRef<HTMLInputElement>(null);

  const [source, setSource] = useState<Source>("plausible");
  const [file, setFile] = useState<File | null>(null);
  const [viaApi, setViaApi] = useState(false);
  const [fields, setFields] = useState<ApiFields>(NO_FIELDS);
  const apiPossible = API_SOURCES.includes(source);
  const usingApi = apiPossible && viaApi;
  const field = (name: keyof ApiFields) => ({
    value: fields[name],
    onChange: (e: React.ChangeEvent<HTMLInputElement>) =>
      setFields((f) => ({ ...f, [name]: e.target.value })),
    disabled: start.isPending,
  });

  const imports = useQuery({
    queryKey: key,
    queryFn: () => api.get<ImportRecord[]>("/api/imports", { site: asSite(siteId) }).then((r) => r.data),
    enabled: siteId != null,
    // Follow anything still moving; stop asking when everything has settled.
    refetchInterval: (q) =>
      (q.state.data ?? []).some((i) => stateOf(i.status) === "running" || stateOf(i.status) === "waiting")
        ? 2000
        : false,
  });

  const start = useMutation({
    mutationFn: () => {
      if (usingApi) return startImport(asSite(siteId), source, { fields });
      if (!file) throw new ApiError(0, "no_file", "Choose an export file first.");
      return startImport(asSite(siteId), source, { file });
    },
    onSuccess: () => {
      setFile(null);
      // The key leaves the page with the request; nothing keeps a copy of it.
      setFields(NO_FIELDS);
      if (fileRef.current) fileRef.current.value = "";
      void qc.invalidateQueries({ queryKey: key });
    },
  });

  const remove = useMutation({
    mutationFn: (id: string) => api.del<null>(`/api/imports/${id}`),
    onSuccess: () => qc.invalidateQueries({ queryKey: key }),
  });

  const columns: Column<ImportRecord>[] = [
    {
      key: "source",
      head: "Source",
      width: "20%",
      render: (r) => <span>{SOURCE_NAME[r.source] ?? r.source}</span>,
    },
    {
      key: "status",
      head: "State",
      render: (r) => {
        const s = stateOf(r.status);
        return (
          <>
            <Status state={s} label={s === "done" ? "Imported" : s === "failed" ? "Failed" : s === "running" ? "Importing" : "Queued"} />
            {s === "running" || s === "waiting" ? (
              <Gauge inline value={fraction(r.progress)} label={`${SOURCE_NAME[r.source] ?? r.source} import progress`} />
            ) : null}
            {r.error ? <span className="mf-set-foot__say" data-tone="warn">{r.error}</span> : null}
          </>
        );
      },
    },
    {
      key: "rows",
      head: "Rows",
      numeric: true,
      width: "12%",
      render: (r) => <span className="mf-data">{typeof r.rows === "number" ? num(r.rows) : "—"}</span>,
    },
    {
      key: "started",
      head: "Started",
      width: "16%",
      render: (r) => <span className="mf-data">{ago(r.created_at)}</span>,
    },
    {
      key: "act",
      head: <span className="mf-sr">Actions</span>,
      numeric: true,
      width: "1%",
      render: (r) => (
        <Button
          size="sm"
          tone="quiet"
          icon={<IconClose size={12} />}
          disabled={remove.isPending}
          onClick={() => remove.mutate(r.id)}
        >
          {stateOf(r.status) === "done" ? "Remove rows" : "Cancel"}
        </Button>
      ),
    },
  ];

  return (
    <>
      <Panel>
        <PanelHead
          title="Bring history in"
          note="A file import makes no outbound request: you export the file from the old tool and hand it over here. Only an API import calls out, to PostHog or Simple Analytics, and only because you typed its key below."
        />
        <Spec>
          <SpecRow name="Coming from" hint={SOURCE_FILE[source]} htmlFor="imp-source">
            <Select id="imp-source" value={source} onChange={(e) => setSource(e.target.value as Source)}>
              <option value="ga4">Google Analytics 4</option>
              <option value="plausible">Plausible</option>
              <option value="umami">Umami</option>
              <option value="posthog">PostHog</option>
              <option value="simple_analytics">Simple Analytics</option>
            </Select>
          </SpecRow>

          {apiPossible ? (
            <SpecRow
              name="Read it from"
              hint="An API import reads the days you choose straight from the provider. Its key is sealed on this server, used only by the import, and deleted when the import ends."
              htmlFor="imp-via"
            >
              <Select id="imp-via" value={usingApi ? "api" : "file"} onChange={(e) => setViaApi(e.target.value === "api")}>
                <option value="file">A file I upload</option>
                <option value="api">{SOURCE_NAME[source]}'s API</option>
              </Select>
            </SpecRow>
          ) : null}

          {usingApi && source === "posthog" ? (
            <>
              <SpecRow name="PostHog address" hint="https://us.posthog.com, https://eu.posthog.com, or your own PostHog. Empty means the US cloud." htmlFor="imp-host">
                <TextInput id="imp-host" data placeholder="https://us.posthog.com" autoComplete="off" {...field("api_host")} />
              </SpecRow>
              <SpecRow name="Project id" hint="The number in /project/12345 in PostHog's address bar." htmlFor="imp-project">
                <TextInput id="imp-project" data inputMode="numeric" autoComplete="off" {...field("project")} />
              </SpecRow>
              <SpecRow name="Personal API key" hint="Starts with phx_ and needs the Query Read permission. The project token (phc_) cannot read events." htmlFor="imp-key">
                <TextInput id="imp-key" data type="password" autoComplete="off" spellCheck={false} {...field("api_key")} />
              </SpecRow>
            </>
          ) : null}

          {usingApi && source === "simple_analytics" ? (
            <>
              <SpecRow name="Website" hint={`As Simple Analytics lists it. Empty means ${domain ?? "this site's domain"}.`} htmlFor="imp-hostname">
                <TextInput id="imp-hostname" data placeholder={domain ?? "example.com"} autoComplete="off" {...field("hostname")} />
              </SpecRow>
              <SpecRow name="API key" hint="Starts with sa_api_key_. Create one in Simple Analytics' account settings." htmlFor="imp-key">
                <TextInput id="imp-key" data type="password" autoComplete="off" spellCheck={false} {...field("api_key")} />
              </SpecRow>
              <SpecRow name="User id" hint="Starts with sa_user_id_, on the same settings page. The export API needs both." htmlFor="imp-user">
                <TextInput id="imp-user" data autoComplete="off" spellCheck={false} {...field("user_id")} />
              </SpecRow>
            </>
          ) : null}

          {usingApi ? (
            <SpecRow name="Days" hint="Both days included, in UTC. Import only the days before you installed the Micaforge tag." htmlFor="imp-from">
              <div className="mf-set-row">
                <TextInput id="imp-from" type="date" aria-label="First day" {...field("from")} />
                <span className="mf-set-note">to</span>
                <TextInput id="imp-to" type="date" aria-label="Last day" {...field("to")} />
              </div>
            </SpecRow>
          ) : null}

          {usingApi ? null : (
          <SpecRow
            name="Export file"
            hint="CSV or JSON (an array, or one record per line). Large files are read in chunks, so the upload finishing is not the import finishing."
          >
            <div className="mf-set-row">
              <input
                ref={fileRef}
                id="imp-file"
                type="file"
                className="mf-set-file"
                accept=".csv,.json,.ndjson,.jsonl"
                disabled={start.isPending}
                onChange={(e) => setFile(e.target.files?.[0] ?? null)}
              />
              <label htmlFor="imp-file" className="mf-btn mf-btn--key mf-btn--md mf-set-filelabel">
                <span className="mf-btn__icon">
                  <IconDownload size={14} />
                </span>
                <span className="mf-btn__label">Choose a file</span>
              </label>
              <span className="mf-set-note">
                {file ? (
                  <>
                    <span className="mf-data">{file.name}</span> · {bytes(file.size)}
                  </>
                ) : (
                  "No file chosen yet."
                )}
              </span>
            </div>
          </SpecRow>
          )}
        </Spec>

        <Foot>
          <Button
            tone="primary"
            disabled={
              start.isPending ||
              (usingApi ? !fields.api_key.trim() || !fields.from || !fields.to : !file)
            }
            onClick={() => start.mutate()}
          >
            {start.isPending ? (usingApi ? "Starting…" : "Uploading…") : "Start the import"}
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result
            action={start}
            done={usingApi ? "Queued. The import reads the provider's API now." : "Uploaded. The import is queued."}
            working={usingApi ? "Starting…" : "Uploading…"}
          />
        </Foot>
      </Panel>

      <Panel>
        <PanelHead
          title="Imports"
          note="Every imported row is stamped with its import, so removing one takes its rows back out without touching anything recorded live."
        />
        <QueryState query={imports}>
          {(rows) => (
            <>
              <Sheet
                rows={rows}
                columns={columns}
                getKey={(r) => r.id}
                empty={
                  <Empty
                    title="No import has been run"
                    body="Upload an export above and it will appear here with its progress while it runs."
                  />
                }
              />
              {remove.isError ? (
                <Foot tight>
                  <span className="mf-set-foot__say" data-tone="warn" role="alert">
                    {errText(remove.error)}
                  </span>
                </Foot>
              ) : null}
            </>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="What carries over"
          note="Read this before you import. Half of it is what the export never contained in the first place."
        />
        <div className="mf-set-block">
          <div className="mf-set-stack">
            <div className="mf-set-ledger">
              {CARRIES.map((c) => (
                <div className="mf-set-ledger__row" key={c.what}>
                  <span className="mf-set-ledger__mark" data-carried={c.carried ? "true" : "false"}>
                    {c.carried ? <IconCheck size={13} /> : <IconClose size={13} />}
                    <span className="mf-sr">{c.carried ? "Carries over:" : "Does not carry over:"}</span>
                  </span>
                  <span>
                    <span className="mf-set-ledger__what">{c.what}</span>: {c.detail}
                  </span>
                </div>
              ))}
            </div>
            <Caution>
              Importing does not de-duplicate against live data. If the old tool and Micaforge both recorded
              the same day, import only the days before you installed the tag.
            </Caution>
          </div>
        </div>
      </Panel>
    </>
  );
}

export default ImportsSettings;
