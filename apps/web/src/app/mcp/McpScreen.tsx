/**
 * MCP servers: the Model Context Protocol servers this team runs, as their own process
 * measured them.
 *
 * Every figure comes from `instrumentMcpServer` in `@micaforge/sdk-server`, which times
 * each `tools/call`, `resources/read` and `prompts/get` and sends the result. Nothing is
 * estimated, and nothing a model sent or received is shown: argument and result sizes,
 * the error's class and JSON-RPC code, never a value or a message.
 *
 * The order is the order of the questions: is it healthy (calls, error rate, latency),
 * when (two charts), what (tools), who (clients), and which single calls to look at
 * (slowest, failing). The calls are AI traffic, so they are drawn in orange; errors are
 * dashed and latency is neutral ink, so no line relies on its colour alone.
 *
 * Reads `/api/stats/mcp/*`. The page filters and the audience switch describe visits,
 * not MCP calls, so this screen uses only the date range and its own two filters.
 */
import { useMemo, useState } from "react";
import { Link, useParams } from "react-router";
import { keepPreviousData, useQuery } from "@tanstack/react-query";

import {
  CopyField,
  Empty,
  ErrorState,
  Panel,
  PanelHead,
  Readout,
  Sheet,
  type Channel,
  type Column,
} from "../../components/react/ui";
import { api, ApiError, publicOrigin, type Envelope } from "../../lib/api";
import { ago, bytes, num, pct } from "../../lib/format";
import { againstLeader } from "../display";
import { Bank, Choice } from "../growth/controls";
import { ChartWaiting, LineChart } from "../growth/LineChart";
import { ReadoutWaiting } from "../health/PerformanceScreen";
import { useSite } from "../providers";
import { rangeWords, useAppState } from "../state";
import "../../styles/app/mcp.css";

/* ── what the endpoints return ───────────────────────────────────────────── */

type Method = "tools/call" | "resources/read" | "prompts/get";

interface ServerSeen {
  name: string;
  version: string;
  calls: number;
  last_seen: string;
}

interface Overview {
  calls: number;
  errors: number;
  tool_errors: number;
  error_rate: number;
  p50_ms: number;
  p95_ms: number;
  sessions: number;
  clients: number;
  tools: number;
  servers: ServerSeen[];
}

interface Bucket {
  t: number;
  calls: number;
  errors: number;
  p50_ms: number;
  p95_ms: number;
}

interface ToolRow {
  method: Method;
  name: string;
  calls: number;
  errors: number;
  tool_errors: number;
  error_rate: number;
  p50_ms: number;
  p95_ms: number;
  avg_arg_bytes: number;
  avg_result_bytes: number;
  clients: number;
  top_error: string;
}

interface ClientRow {
  client: string;
  versions: string[];
  calls: number;
  share: number;
  sessions: number;
  errors: number;
  error_rate: number;
  p50_ms: number;
}

interface CallRow {
  timestamp: string;
  server_name: string;
  method: Method;
  name: string;
  client_name: string;
  client_version: string;
  transport: string;
  duration_ms: number;
  status: "ok" | "error" | "tool_error";
  error_type: string;
  error_code: number;
  arg_bytes: number;
  result_bytes: number;
}

/* ── words ───────────────────────────────────────────────────────────────── */

const METHOD_WORDS: Record<Method, string> = {
  "tools/call": "Tool",
  "resources/read": "Resource",
  "prompts/get": "Prompt",
};

const METHOD_FILTER: { v: "all" | Method; label: string }[] = [
  { v: "all", label: "All" },
  { v: "tools/call", label: "Tools" },
  { v: "resources/read", label: "Resources" },
  { v: "prompts/get", label: "Prompts" },
];

/**
 * The product names behind the `clientInfo.name` values the common clients send. A
 * client not listed here is shown by the name it sent, in the code face.
 */
const CLIENT_NAMES: Record<string, string> = {
  "claude-ai": "Claude",
  "claude-code": "Claude Code",
  "cursor-vscode": "Cursor",
  "Visual Studio Code": "VS Code",
  "windsurf-client": "Windsurf",
  "mcp-inspector": "MCP Inspector",
  Zed: "Zed",
  cline: "Cline",
  continue: "Continue",
  goose: "Goose",
};

/** A latency figure: whole milliseconds under a second, seconds above. */
function latency(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return "—";
  return v >= 1000 ? `${(v / 1000).toFixed(v >= 10_000 ? 0 : 1)}s` : `${Math.round(v)}ms`;
}

function reason(err: unknown): string | undefined {
  if (err instanceof ApiError) return err.message;
  return err instanceof Error ? err.message : undefined;
}

/** The date window alone: the page filters and the audience do not apply to MCP calls. */
function windowOf(siteId: number, range: string): Envelope {
  const cut = range.indexOf("..");
  return cut > 0
    ? { site: siteId, start: range.slice(0, cut), end: range.slice(cut + 2) }
    : { site: siteId, range };
}

/* ── small parts ─────────────────────────────────────────────────────────── */

function ClientName({ name }: { name: string }) {
  if (!name) return <span className="mf-mcp-dim">Not sent</span>;
  const known = CLIENT_NAMES[name];
  return known ? (
    <span title={name}>{known}</span>
  ) : (
    <code className="mf-mcp-code" title={name}>
      {name}
    </code>
  );
}

function MethodTag({ method }: { method: Method }) {
  return <span className="mf-mcp-tag">{METHOD_WORDS[method] ?? method}</span>;
}

/** How a call ended, as a word and a drawn mark. */
function Status({ status }: { status: CallRow["status"] }) {
  const words = { ok: "Answered", error: "Failed", tool_error: "Tool error" } as const;
  return (
    <span className="mf-mcp-status" data-status={status}>
      <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
        {status === "ok" ? (
          <path d="M2.5 6.2 5 8.5l4.5-5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
        ) : status === "error" ? (
          <path d="M3 3l6 6M9 3 3 9" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" />
        ) : (
          <circle cx="6" cy="6" r="3.5" fill="none" stroke="currentColor" strokeWidth="1.4" />
        )}
      </svg>
      {words[status]}
    </span>
  );
}

/** What to do when nothing has arrived: the install, with this site's numbers in it. */
function Setup({ siteId, siteKey, inRange }: { siteId: number; siteKey: string | undefined; inRange: string }) {
  const snippet = `import { Micaforge } from "@micaforge/sdk-server";
import { instrumentMcpServer } from "@micaforge/sdk-server/mcp";

const micaforge = new Micaforge({
  host: "${publicOrigin()}",
  siteId: ${siteId},
  ingestKey: process.env.MICAFORGE_INGEST_KEY!,
});

// Your McpServer or Server, before or after registering tools.
instrumentMcpServer(server, { client: micaforge });`;
  return (
    <Panel>
      <PanelHead
        title="Measure an MCP server"
        note={`No MCP server call was recorded in ${inRange.toLowerCase()}. If a server is already instrumented, choose a longer range.`}
      />
      <div className="mf-mcp-setup">
        <CopyField label="Install" value="npm install @micaforge/sdk-server" />
        <CopyField label="Wrap the server" value={snippet} />
        <p className="mf-mcp-setup__note">
          The ingest key is in{" "}
          {siteKey ? <Link to={`/app/${siteKey}/settings/tracking`}>Settings, Tracking</Link> : "Settings, Tracking"}. It
          is a server-side secret: read it from the environment. Every tool call, resource read and prompt is then
          recorded with its client, its duration and how it ended. Argument values, results and error messages stay on
          your server. Any other MCP server can send its calls with <code>micaforge.recordMcpCall()</code>; see{" "}
          <a href="/docs/tracking/mcp-server-analytics">MCP server analytics</a>.
        </p>
      </div>
    </Panel>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function McpScreen() {
  const { siteId, isLoading: siteLoading } = useSite();
  const { site: siteKey } = useParams();
  const range = useAppState((s) => s.range);
  const [server, setServer] = useState("");
  const [method, setMethod] = useState<"all" | Method>("all");
  const [view, setView] = useState<"slowest" | "failing">("slowest");

  const base = useMemo(() => (siteId != null ? windowOf(siteId, range) : null), [siteId, range]);
  const narrowed = useMemo<Envelope | null>(
    () =>
      base ? { ...base, ...(server ? { server } : {}), ...(method !== "all" ? { method } : {}) } : null,
    [base, server, method],
  );
  const filtered = server !== "" || method !== "all";
  const rangeText = rangeWords(range);

  // Unfiltered, so the server list does not shrink to the one server chosen.
  const all = useQuery({
    queryKey: ["mcp", "overview", base],
    queryFn: () => api.get<Overview>("/api/stats/mcp/overview", base as Envelope).then((r) => r.data),
    enabled: base != null,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
  const narrow = useQuery({
    queryKey: ["mcp", "overview", narrowed],
    queryFn: () => api.get<Overview>("/api/stats/mcp/overview", narrowed as Envelope).then((r) => r.data),
    enabled: narrowed != null && filtered,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
  const overview = filtered ? narrow : all;
  const series = useQuery({
    queryKey: ["mcp", "timeseries", narrowed],
    queryFn: () => api.get<Bucket[]>("/api/stats/mcp/timeseries", narrowed as Envelope).then((r) => r.data),
    enabled: narrowed != null,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
  const tools = useQuery({
    queryKey: ["mcp", "tools", narrowed],
    queryFn: () =>
      api.get<ToolRow[]>("/api/stats/mcp/tools", { ...(narrowed as Envelope), limit: 50 }).then((r) => r.data),
    enabled: narrowed != null,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
  const clients = useQuery({
    queryKey: ["mcp", "clients", narrowed],
    queryFn: () =>
      api.get<ClientRow[]>("/api/stats/mcp/clients", { ...(narrowed as Envelope), limit: 50 }).then((r) => r.data),
    enabled: narrowed != null,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });
  const calls = useQuery({
    queryKey: ["mcp", "calls", narrowed, view],
    queryFn: () =>
      api
        .get<CallRow[]>("/api/stats/mcp/calls", { ...(narrowed as Envelope), view, limit: 50 })
        .then((r) => r.data),
    enabled: narrowed != null,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });

  const heading = <h1 className="mf-sr">MCP servers</h1>;

  if (siteLoading || siteId == null || all.isPending) {
    return (
      <div className="mf-mcp">
        {heading}
        <Panel>
          <PanelHead title="MCP servers" />
          <ReadoutWaiting count={6} />
        </Panel>
        <Panel>
          <div className="mf-gr-pad">
            <ChartWaiting />
          </div>
        </Panel>
      </div>
    );
  }

  if (all.isError) {
    return (
      <div className="mf-mcp">
        {heading}
        <Panel>
          <ErrorState
            title="Could not load MCP server calls"
            detail={reason(all.error)}
            onRetry={() => void all.refetch()}
          />
        </Panel>
      </div>
    );
  }

  if ((all.data?.calls ?? 0) === 0) {
    return (
      <div className="mf-mcp">
        {heading}
        <Setup siteId={siteId} siteKey={siteKey} inRange={rangeText} />
      </div>
    );
  }

  const data = overview.data ?? all.data;
  const servers = all.data?.servers ?? [];
  const channels: Channel[] = data
    ? [
        { key: "calls", label: "Calls", value: num(data.calls), note: `${num(data.tools)} tools called` },
        {
          key: "errors",
          label: "Error rate",
          value: pct(data.error_rate),
          note: `${num(data.errors)} failed, ${num(data.tool_errors)} tool errors`,
        },
        { key: "p50", label: "Median time", value: latency(data.p50_ms), note: "p50, in the handler" },
        { key: "p95", label: "Slow calls", value: latency(data.p95_ms), note: "p95, in the handler" },
        { key: "sessions", label: "Sessions", value: num(data.sessions) },
        { key: "clients", label: "Clients", value: num(data.clients), note: "by the name each client sent" },
      ]
    : [];

  const buckets = series.data ?? [];
  const toolRows = tools.data ?? [];
  const clientRows = clients.data ?? [];
  const callRows = calls.data ?? [];

  const toolColumns: Column<ToolRow>[] = [
    {
      key: "name",
      head: "Tool, resource or prompt",
      width: "34%",
      share: againstLeader(toolRows, (r) => r.calls),
      render: (r) => (
        <span className="mf-mcp-name">
          <MethodTag method={r.method} />
          <code className="mf-mcp-code" title={r.name}>
            {r.name || "Not named"}
          </code>
        </span>
      ),
    },
    { key: "calls", head: "Calls", numeric: true, render: (r) => <span className="mf-data">{num(r.calls)}</span> },
    {
      key: "rate",
      head: "Error rate",
      numeric: true,
      render: (r) => (
        <span
          className="mf-data"
          data-tone={r.error_rate > 0.05 ? "warn" : undefined}
          title={`${num(r.errors)} failed, ${num(r.tool_errors)} tool errors`}
        >
          {pct(r.error_rate)}
        </span>
      ),
    },
    { key: "p50", head: "p50", numeric: true, render: (r) => <span className="mf-data">{latency(r.p50_ms)}</span> },
    { key: "p95", head: "p95", numeric: true, render: (r) => <span className="mf-data">{latency(r.p95_ms)}</span> },
    {
      key: "size",
      head: "Result size",
      numeric: true,
      render: (r) => (
        <span className="mf-data" title={`Arguments ${bytes(r.avg_arg_bytes)} on average`}>
          {bytes(r.avg_result_bytes)}
        </span>
      ),
    },
    {
      key: "error",
      head: "Commonest error",
      render: (r) =>
        r.top_error ? <code className="mf-mcp-code">{r.top_error}</code> : <span className="mf-mcp-dim">—</span>,
    },
  ];

  const clientColumns: Column<ClientRow>[] = [
    {
      key: "client",
      head: "Client",
      width: "30%",
      share: (r) => r.share,
      render: (r) => <ClientName name={r.client} />,
    },
    {
      key: "versions",
      head: "Versions",
      render: (r) =>
        r.versions.length ? (
          <span className="mf-data">{r.versions.join(", ")}</span>
        ) : (
          <span className="mf-mcp-dim">—</span>
        ),
    },
    { key: "calls", head: "Calls", numeric: true, render: (r) => <span className="mf-data">{num(r.calls)}</span> },
    { key: "share", head: "Share", numeric: true, render: (r) => <span className="mf-data">{pct(r.share)}</span> },
    { key: "sessions", head: "Sessions", numeric: true, render: (r) => <span className="mf-data">{num(r.sessions)}</span> },
    { key: "rate", head: "Error rate", numeric: true, render: (r) => <span className="mf-data">{pct(r.error_rate)}</span> },
    { key: "p50", head: "p50", numeric: true, render: (r) => <span className="mf-data">{latency(r.p50_ms)}</span> },
  ];

  const callColumns: Column<CallRow>[] = [
    {
      key: "when",
      head: "When",
      render: (r) => (
        <span className="mf-data" title={new Date(r.timestamp).toLocaleString()}>
          {ago(r.timestamp)}
        </span>
      ),
    },
    {
      key: "name",
      head: "Call",
      width: "28%",
      render: (r) => (
        <span className="mf-mcp-name">
          <MethodTag method={r.method} />
          <code className="mf-mcp-code" title={r.name}>
            {r.name || "Not named"}
          </code>
        </span>
      ),
    },
    {
      key: "client",
      head: "Client",
      render: (r) => (
        <span title={r.client_version ? `${r.client_name} ${r.client_version}` : r.client_name}>
          <ClientName name={r.client_name} />
        </span>
      ),
    },
    {
      key: "time",
      head: "Time",
      numeric: true,
      render: (r) => <span className="mf-data">{latency(r.duration_ms)}</span>,
    },
    { key: "status", head: "Ended", render: (r) => <Status status={r.status} /> },
    {
      key: "error",
      head: "Error",
      render: (r) =>
        r.error_type || r.error_code ? (
          <code className="mf-mcp-code">
            {r.error_type || "Error"}
            {r.error_code ? ` ${r.error_code}` : ""}
          </code>
        ) : (
          <span className="mf-mcp-dim">—</span>
        ),
    },
    {
      key: "size",
      head: "Sizes",
      numeric: true,
      render: (r) => (
        <span className="mf-data" title="Arguments in, result out">
          {bytes(r.arg_bytes)} / {bytes(r.result_bytes)}
        </span>
      ),
    },
  ];

  const noneHere = (
    <Empty
      title="No calls match"
      body="No call in this period matches the server and kind chosen above. Choose All to see every call."
    />
  );

  return (
    <div className="mf-mcp">
      {heading}

      <div className="mf-mcp-filters">
        {servers.length > 1 ? (
          <Choice aria-label="Server" value={server} onChange={(e) => setServer(e.target.value)}>
            <option value="">All servers</option>
            {servers.map((s) => (
              <option key={s.name} value={s.name}>
                {s.name || "Not named"}
                {s.version ? ` ${s.version}` : ""}
              </option>
            ))}
          </Choice>
        ) : servers[0] ? (
          <span className="mf-mcp-server">
            <code className="mf-mcp-code">{servers[0].name || "Not named"}</code>
            {servers[0].version ? <span className="mf-mcp-dim"> {servers[0].version}</span> : null}
            <span className="mf-mcp-dim"> · last call {ago(servers[0].last_seen)}</span>
          </span>
        ) : null}
        <Bank<"all" | Method> label="Kind of call" value={method} options={METHOD_FILTER} onChange={setMethod} />
      </div>

      <Panel>
        <PanelHead
          title="MCP servers"
          note="Tool calls, resource reads and prompts your MCP servers answered, timed inside the server."
        />
        {overview.isError ? (
          <ErrorState
            title="Could not load these figures"
            detail={reason(overview.error)}
            onRetry={() => void overview.refetch()}
          />
        ) : overview.isPending ? (
          <ReadoutWaiting count={6} />
        ) : (
          <Readout channels={channels} dense />
        )}
      </Panel>

      <div className="mf-mcp-charts">
        <Panel>
          <PanelHead title="Calls over time" note="Every call, and the ones that failed or returned a tool error." />
          <div className="mf-gr-pad">
            {series.isError ? (
              <ErrorState title="Could not load the chart" detail={reason(series.error)} onRetry={() => void series.refetch()} />
            ) : series.isPending ? (
              <ChartWaiting />
            ) : buckets.every((b) => !b.calls) ? (
              noneHere
            ) : (
              <LineChart
                label={`MCP calls, ${rangeText.toLowerCase()}`}
                times={buckets.map((b) => b.t)}
                format={num}
                integer
                series={[
                  { key: "calls", label: "Calls", color: "var(--mf-ai)", values: buckets.map((b) => b.calls) },
                  {
                    key: "errors",
                    label: "Errors",
                    color: "var(--mf-danger)",
                    dashed: true,
                    values: buckets.map((b) => b.errors),
                  },
                ]}
              />
            )}
          </div>
        </Panel>
        <Panel>
          <PanelHead title="Latency" note="The median and the 95th percentile of handler time." />
          <div className="mf-gr-pad">
            {series.isError ? (
              <ErrorState title="Could not load the chart" detail={reason(series.error)} onRetry={() => void series.refetch()} />
            ) : series.isPending ? (
              <ChartWaiting />
            ) : buckets.every((b) => !b.calls) ? (
              noneHere
            ) : (
              <LineChart
                label={`MCP call latency, ${rangeText.toLowerCase()}`}
                times={buckets.map((b) => b.t)}
                format={latency}
                series={[
                  { key: "p50", label: "p50", color: "var(--mf-ink-2)", values: buckets.map((b) => b.p50_ms) },
                  { key: "p95", label: "p95", color: "var(--mf-ink-3)", dashed: true, values: buckets.map((b) => b.p95_ms) },
                ]}
              />
            )}
          </div>
        </Panel>
      </div>

      <Panel>
        <PanelHead
          title="Tools"
          note="Busiest first. The error rate counts failed calls and tool errors; hover it to see the two apart."
        />
        {tools.isError ? (
          <ErrorState title="Could not load the tools" detail={reason(tools.error)} onRetry={() => void tools.refetch()} />
        ) : (
          <Sheet<ToolRow>
            rows={toolRows}
            columns={toolColumns}
            loading={tools.isPending}
            getKey={(r) => `${r.method}-${r.name}`}
            empty={noneHere}
          />
        )}
      </Panel>

      <Panel>
        <PanelHead title="Clients" note="The MCP clients that called, by the name each sent when it connected." />
        {clients.isError ? (
          <ErrorState title="Could not load the clients" detail={reason(clients.error)} onRetry={() => void clients.refetch()} />
        ) : (
          <Sheet<ClientRow>
            rows={clientRows}
            columns={clientColumns}
            loading={clients.isPending}
            getKey={(r) => r.client || "(none)"}
            empty={noneHere}
          />
        )}
      </Panel>

      <Panel>
        <PanelHead
          title="Calls"
          note="Single calls, to look at one by one. Argument values and error messages are never recorded."
          actions={
            <Bank<"slowest" | "failing">
              label="Which calls"
              value={view}
              options={[
                { v: "slowest", label: "Slowest" },
                { v: "failing", label: "Failing" },
              ]}
              onChange={setView}
            />
          }
        />
        {calls.isError ? (
          <ErrorState title="Could not load the calls" detail={reason(calls.error)} onRetry={() => void calls.refetch()} />
        ) : (
          <Sheet<CallRow>
            rows={callRows}
            columns={callColumns}
            loading={calls.isPending}
            getKey={(r, i) => `${r.timestamp}-${r.name}-${i}`}
            empty={
              view === "failing" ? (
                <Empty title="No failed calls" body="Every call in this period was answered without an error." />
              ) : (
                noneHere
              )
            }
          />
        )}
      </Panel>
    </div>
  );
}

export default McpScreen;
