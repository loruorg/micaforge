/**
 * IMPORTS: adding history from an older analytics tool.
 *
 * Everything is file-based on purpose. A self-hosted Micaforge makes no
 * outbound call at runtime, so there is no "connect your Google account" here
 * and there never will be: you export from the old tool and hand the file over.
 * That constraint is stated on the screen rather than hidden, because it is the
 * reason the product can be run on a machine with no egress at all.
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
  errText,
  type StateName,
} from "./SettingsLayout";

type Source = "ga4" | "plausible" | "umami";

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
};

const SOURCE_FILE: Record<Source, string> = {
  ga4: "The CSV an Exploration exports, or a BigQuery events export as newline-delimited JSON.",
  plausible: "The CSV bundle from the site's own export, or a stats API dump.",
  umami: "The JSON export from the website's settings.",
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
  { carried: true, what: "Custom events and goals", detail: "Plausible and Umami events, matched to goals of the same name." },
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
 */
async function uploadImport(site: string | number, source: Source, file: File): Promise<ImportRecord> {
  const body = new FormData();
  body.set("site", String(site));
  body.set("source", source);
  body.set("file", file);

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
  const { siteId } = useSite();
  const qc = useQueryClient();
  const key = ["settings", "imports", siteId];
  const fileRef = useRef<HTMLInputElement>(null);

  const [source, setSource] = useState<Source>("plausible");
  const [file, setFile] = useState<File | null>(null);

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
      if (!file) throw new ApiError(0, "no_file", "Choose an export file first.");
      return uploadImport(asSite(siteId), source, file);
    },
    onSuccess: () => {
      setFile(null);
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
          note="Micaforge never calls out to Google, Plausible or anyone else. A self-hosted instance makes no outbound request at all, so you export the file from the old tool and hand it over here."
        />
        <Spec>
          <SpecRow name="Coming from" hint={SOURCE_FILE[source]} htmlFor="imp-source">
            <Select id="imp-source" value={source} onChange={(e) => setSource(e.target.value as Source)}>
              <option value="ga4">Google Analytics 4</option>
              <option value="plausible">Plausible</option>
              <option value="umami">Umami</option>
            </Select>
          </SpecRow>

          <SpecRow
            name="Export file"
            hint="csv, json, ndjson or a zip of them. Large files are read in chunks, so the upload finishing is not the import finishing."
          >
            <div className="mf-set-row">
              <input
                ref={fileRef}
                id="imp-file"
                type="file"
                className="mf-set-file"
                accept=".csv,.json,.ndjson,.zip,.gz"
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
        </Spec>

        <Foot>
          <Button tone="primary" disabled={!file || start.isPending} onClick={() => start.mutate()}>
            {start.isPending ? "Uploading…" : "Start the import"}
          </Button>
          <span className="mf-set-foot__spacer" />
          <Result action={start} done="Uploaded. The import is queued." working="Uploading…" />
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
