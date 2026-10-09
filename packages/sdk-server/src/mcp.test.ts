import { createHmac } from "node:crypto";

import { afterEach, describe, expect, it, vi } from "vitest";

import { hashSessionId, MCP_PATH, Micaforge, SIGNATURE_HEADER, toWireMcpCall } from "./index.js";
import { instrumentMcpServer, redact, transportName } from "./mcp.js";
import type { McpPayload, WireMcpCall } from "./types.js";

const KEY = "mfi_ingest_test_key";
const HOST = "https://micaforge.example.com";

type Handler = (request: any, extra?: any) => unknown;

/**
 * A stand-in for the official SDK's low-level `Server`, reduced to what matters here:
 * handlers live in a `Map` keyed by method, `setRequestHandler` stores a closure that
 * resolves the handler's answer (as `Protocol.setRequestHandler` does), `initialize` is
 * registered in the constructor, and `dispatch` does what `Protocol._onrequest` does
 * with a thrown error: answer with its integer `code`, or -32603.
 */
class FakeServer {
  _requestHandlers = new Map<string, Handler>();
  _serverInfo = { name: "docs", version: "1.2.0" };
  transport?: object;
  private clientInfo?: { name: string; version: string };

  constructor() {
    this.setRequestHandler("initialize", (request) => {
      this.clientInfo = request.params.clientInfo;
      return { protocolVersion: "2025-06-18", serverInfo: this._serverInfo, capabilities: {} };
    });
  }

  setRequestHandler(method: string, handler: Handler): void {
    this._requestHandlers.set(method, (request, extra) => Promise.resolve(handler(request, extra)));
  }

  getClientVersion() {
    return this.clientInfo;
  }

  async dispatch(method: string, params: unknown, extra: Record<string, unknown> = {}) {
    const handler = this._requestHandlers.get(method);
    if (!handler) return { error: { code: -32601, message: "Method not found" } };
    try {
      return { result: await handler({ jsonrpc: "2.0", id: 1, method, params }, extra) };
    } catch (error: any) {
      const code = Number.isSafeInteger(error?.code) ? error.code : -32603;
      return { error: { code, message: error?.message ?? "Internal error" } };
    }
  }

  initialize(name = "claude-ai", version = "0.9.1", protocolVersion = "2025-03-26") {
    return this.dispatch("initialize", { protocolVersion, clientInfo: { name, version }, capabilities: {} });
  }
}

class McpError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(`MCP error ${code}: ${message}`);
    this.name = "McpError";
  }
}

class StdioServerTransport {}
class StreamableHTTPServerTransport {
  sessionId = "5f0c8a52-1b9e-4c1e-9a77-3a0e3c7d2b11";
}

/** A client whose transport records every request. */
function setup(options: Partial<Parameters<typeof instrumentMcpServer>[1]> = {}) {
  const sent: { url: string; headers: Record<string, string>; raw: string; body: McpPayload }[] = [];
  const fetchStub = vi.fn(async (url: string | URL | Request, init?: RequestInit) => {
    const raw = String(init?.body ?? "");
    sent.push({
      url: String(url),
      headers: (init?.headers ?? {}) as Record<string, string>,
      raw,
      body: JSON.parse(raw),
    });
    return new Response(null, { status: 202 });
  });
  const client = new Micaforge({
    host: HOST,
    siteId: 7,
    ingestKey: KEY,
    logger: null,
    flushOnExit: false,
    flushInterval: 60_000,
    fetch: fetchStub as unknown as typeof fetch,
  });
  const server = new FakeServer();
  const instrumented = instrumentMcpServer(server, { client, logger: null, ...options });
  const rows = async (): Promise<WireMcpCall[]> => {
    await client.flush();
    return sent.flatMap((s) => s.body.mcp_calls ?? []);
  };
  return { client, server, instrumented, sent, rows };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("instrumentMcpServer()", () => {
  it("records a successful tool call with the client, the revision and the sizes", async () => {
    const { server, instrumented, sent, rows } = setup();
    expect(instrumented).toBe(server);
    server.setRequestHandler("tools/call", () => ({ content: [{ type: "text", text: "found 3 pages" }] }));
    server.transport = new StdioServerTransport();

    await server.initialize();
    const answer = await server.dispatch("tools/call", { name: "search", arguments: { query: "rust" } });
    expect(answer).toEqual({ result: { content: [{ type: "text", text: "found 3 pages" }] } });

    const [row] = await rows();
    expect(row).toMatchObject({
      method: "tools/call",
      name: "search",
      status: "ok",
      client_name: "claude-ai",
      client_version: "0.9.1",
      // The server's answer named the revision, not the client's request.
      protocol_version: "2025-06-18",
      transport: "stdio",
      server_name: "docs",
      server_version: "1.2.0",
      arg_bytes: JSON.stringify({ query: "rust" }).length,
      result_bytes: JSON.stringify({ content: [{ type: "text", text: "found 3 pages" }] }).length,
      sdk_version: "sdk-server/0.1.0",
    });
    expect(row?.session_hash).toMatch(/^[0-9a-f]{32}$/);
    expect(row).not.toHaveProperty("arguments");
    expect(sent[0]?.raw).not.toContain("rust");
    expect(sent[0]?.url).toBe(HOST + MCP_PATH);
  });

  it("records a thrown error by class and code, and rethrows it unchanged", async () => {
    const { server, rows, sent } = setup();
    const thrown = new McpError(-32602, "Unknown tool: secret-name-of-a-customer");
    server.setRequestHandler("tools/call", () => {
      throw thrown;
    });

    const answer = await server.dispatch("tools/call", { name: "delete_everything" });
    expect(answer).toEqual({ error: { code: -32602, message: thrown.message } });

    const [row] = await rows();
    expect(row).toMatchObject({ status: "error", error_type: "McpError", error_code: -32602 });
    expect(sent[0]?.raw).not.toContain("secret-name-of-a-customer");
  });

  it("records a plain thrown Error with no code as an error without one", async () => {
    const { server, rows } = setup();
    server.setRequestHandler("prompts/get", async () => {
      throw new TypeError("cannot read properties of undefined");
    });
    const answer = await server.dispatch("prompts/get", { name: "summarise" });
    expect(answer).toEqual({ error: { code: -32603, message: "cannot read properties of undefined" } });
    const [row] = await rows();
    expect(row).toMatchObject({ method: "prompts/get", status: "error", error_type: "TypeError" });
    expect(row).not.toHaveProperty("error_code");
  });

  it("records an isError result as a tool error and returns the result as it was", async () => {
    const { server, rows } = setup();
    const result = { isError: true, content: [{ type: "text", text: "rate limited upstream" }] };
    server.setRequestHandler("tools/call", () => result);
    const answer = await server.dispatch("tools/call", { name: "fetch_page" });
    expect(answer.result).toBe(result);
    const [row] = await rows();
    expect(row).toMatchObject({ status: "tool_error", name: "fetch_page" });
    expect(row).not.toHaveProperty("error_type");
  });

  it("times the handler, not the queue", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "performance", "Date"] });
    const { server, rows } = setup();
    server.setRequestHandler(
      "tools/call",
      () => new Promise((resolve) => setTimeout(() => resolve({ content: [] }), 120)),
    );
    const pending = server.dispatch("tools/call", { name: "slow" });
    await vi.advanceTimersByTimeAsync(120);
    await pending;
    vi.useRealTimers();
    const [row] = await rows();
    expect(row?.duration_ms).toBe(120);
  });

  it("measures a handler registered after it, the way McpServer registers tools lazily", async () => {
    const { server, rows } = setup();
    // Registered only now, after instrumentation.
    server.setRequestHandler("resources/read", () => ({ contents: [{ uri: "file:///docs/a.md", text: "#" }] }));
    await server.dispatch("resources/read", { uri: "file:///docs/a.md?user=ana@example.com#top" });
    const [row] = await rows();
    expect(row).toMatchObject({ method: "resources/read", name: "file:///docs/a.md" });
  });

  it("finds the low-level server inside an McpServer", async () => {
    const low = new FakeServer();
    const mcpServer = { server: low, tool: () => {} };
    const { client, sent } = setup();
    expect(instrumentMcpServer(mcpServer, { client, logger: null })).toBe(mcpServer);
    low.setRequestHandler("tools/call", () => ({ content: [] }));
    await low.dispatch("tools/call", { name: "search" });
    await client.flush();
    expect(sent.flatMap((s) => s.body.mcp_calls).map((r) => r.name)).toEqual(["search"]);
  });

  it("lets every other method through without recording it", async () => {
    const { server, rows } = setup();
    server.setRequestHandler("tools/list", () => ({ tools: [] }));
    expect(await server.dispatch("tools/list", {})).toEqual({ result: { tools: [] } });
    expect(await rows()).toEqual([]);
  });

  it("records each call once when instrumented twice", async () => {
    const { server, client, rows } = setup();
    instrumentMcpServer(server, { client, logger: null });
    server.setRequestHandler("tools/call", () => ({ content: [] }));
    await server.dispatch("tools/call", { name: "a" });
    expect(await rows()).toHaveLength(1);
  });

  it("hashes the session id and never sends it", async () => {
    const { server, rows, sent } = setup();
    const transport = new StreamableHTTPServerTransport();
    server.transport = transport;
    server.setRequestHandler("tools/call", () => ({ content: [] }));
    await server.dispatch("tools/call", { name: "a" }, { sessionId: transport.sessionId });
    await server.dispatch("tools/call", { name: "b" }, { sessionId: transport.sessionId });
    const [first, second] = await rows();
    expect(first?.transport).toBe("streamable-http");
    expect(first?.session_hash).toBe(hashSessionId(transport.sessionId));
    expect(second?.session_hash).toBe(first?.session_hash);
    expect(sent.map((s) => s.raw).join("")).not.toContain(transport.sessionId);
  });

  it("stores no session for a stateless HTTP request", async () => {
    const { server, rows } = setup();
    server.transport = new (class StreamableHTTPServerTransport {})();
    server.setRequestHandler("tools/call", () => ({ content: [] }));
    await server.dispatch("tools/call", { name: "a" });
    const [row] = await rows();
    expect(row).not.toHaveProperty("session_hash");
  });

  it("stores arguments only on request, redacted", async () => {
    const plain = setup({ captureArguments: true });
    plain.server.setRequestHandler("tools/call", () => ({ content: [] }));
    await plain.server.dispatch("tools/call", {
      name: "send",
      arguments: { to: "ana@example.com", apiKey: "sk_live_1", body: "hello", nested: { password: "x" } },
    });
    const [row] = await plain.rows();
    expect(row?.arguments).toEqual({
      to: "[redacted]",
      apiKey: "[redacted]",
      body: "hello",
      nested: { password: "[redacted]" },
    });

    const own = setup({ captureArguments: (args: any) => ({ query: String(args.query).slice(0, 3) }) });
    own.server.setRequestHandler("tools/call", () => ({ content: [] }));
    await own.server.dispatch("tools/call", { name: "search", arguments: { query: "rustlang", token: "t" } });
    const [mine] = await own.rows();
    expect(mine?.arguments).toEqual({ query: "rus" });
  });

  it("survives a result it cannot measure", async () => {
    const { server, rows } = setup();
    const circular: Record<string, unknown> = { content: [] };
    circular.self = circular;
    server.setRequestHandler("tools/call", () => circular);
    const answer = await server.dispatch("tools/call", { name: "loop" });
    expect(answer.result).toBe(circular);
    const [row] = await rows();
    expect(row).toMatchObject({ status: "ok", result_bytes: 0 });
  });

  it("signs the MCP batch with the ingest key, in a request of its own", async () => {
    const { server, client, sent } = setup();
    server.setRequestHandler("tools/call", () => ({ content: [] }));
    client.pageview({ url: "https://docs.example.com/", headers: { "user-agent": "Mozilla/5.0 Chrome/128" } });
    await server.dispatch("tools/call", { name: "a" });
    await client.flush();

    expect(sent.map((s) => new URL(s.url).pathname).sort()).toEqual(["/api/log/edge", MCP_PATH]);
    const mcp = sent.find((s) => s.url.endsWith(MCP_PATH));
    const [, t, v1] = /^t=(\d+),v1=([0-9a-f]{64})$/.exec(mcp?.headers[SIGNATURE_HEADER] ?? "") ?? [];
    const expected = createHmac("sha256", KEY).update(`${t}.${mcp?.raw}`).digest("hex");
    expect(v1).toBe(expected);
    expect(mcp?.body.site_id).toBe(7);
  });

  it("warns and leaves an object that is not a server alone", () => {
    const warn = vi.fn();
    const thing = { hello: "world" };
    expect(instrumentMcpServer(thing, { host: HOST, siteId: 1, ingestKey: KEY, logger: { warn, error: warn } })).toBe(
      thing,
    );
    expect(warn).toHaveBeenCalledOnce();
  });

  it("refuses to run without a client or the three settings", () => {
    const error = vi.fn();
    const server = new FakeServer();
    instrumentMcpServer(server, { logger: { warn: vi.fn(), error } });
    expect(error).toHaveBeenCalledOnce();
  });
});

describe("Micaforge.recordMcpCall()", () => {
  it("queues a call and drops a method no report reads", async () => {
    const { client, sent } = setup();
    client.recordMcpCall({ method: "tools/call", name: "search", durationMs: 12.6, clientName: "cursor-vscode" });
    client.recordMcpCall({ method: "tools/list" as never, durationMs: 1 });
    expect(client.queued).toBe(1);
    await client.flush();
    expect(sent[0]?.body.mcp_calls).toHaveLength(1);
    expect(sent[0]?.body.mcp_calls[0]).toMatchObject({ duration_ms: 13, status: "ok", client_name: "cursor-vscode" });
  });
});

describe("toWireMcpCall()", () => {
  it("derives the status from an error class or code", () => {
    expect(toWireMcpCall({ method: "tools/call", durationMs: 1, errorCode: -32000 })?.status).toBe("error");
    expect(toWireMcpCall({ method: "tools/call", durationMs: 1, errorType: "Error" })?.status).toBe("error");
    expect(toWireMcpCall({ method: "tools/call", durationMs: 1 })?.status).toBe("ok");
  });

  it("replaces oversized arguments with a note rather than cutting them", () => {
    const row = toWireMcpCall({ method: "tools/call", durationMs: 1, arguments: { blob: "x".repeat(5000) } });
    expect(row?.arguments).toEqual({ _dropped: "arguments exceeded the size limit" });
  });

  it("never sends a negative or fractional duration", () => {
    expect(toWireMcpCall({ method: "prompts/get", durationMs: -4 })?.duration_ms).toBe(0);
    expect(toWireMcpCall({ method: "prompts/get", durationMs: Number.NaN })?.duration_ms).toBe(0);
  });
});

describe("helpers", () => {
  it("names transports from their class", () => {
    expect(transportName(new StdioServerTransport())).toBe("stdio");
    expect(transportName(new StreamableHTTPServerTransport())).toBe("streamable-http");
    expect(transportName(new (class SSEServerTransport {})())).toBe("sse");
    expect(transportName({})).toBeUndefined();
    expect(transportName(undefined)).toBeUndefined();
  });

  it("redacts deep but bounded", () => {
    expect(redact({ a: [{ secret: 1, ok: "a@b.co" }] })).toEqual({ a: [{ secret: "[redacted]", ok: "[redacted]" }] });
    let deep: any = "x";
    for (let i = 0; i < 20; i++) deep = { d: deep };
    expect(JSON.stringify(redact(deep))).toContain("[redacted]");
  });
});
