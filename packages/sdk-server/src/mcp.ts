/**
 * MCP server analytics: measure the calls an MCP server answers.
 *
 * ```ts
 * import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
 * import { Micaforge } from "@micaforge/sdk-server";
 * import { instrumentMcpServer } from "@micaforge/sdk-server/mcp";
 *
 * const micaforge = new Micaforge({ host, siteId: 1, ingestKey });
 * const server = instrumentMcpServer(new McpServer({ name: "docs", version: "1.2.0" }), {
 *   client: micaforge,
 * });
 * ```
 *
 * # What it measures
 *
 * Every `tools/call`, `resources/read` and `prompts/get` the server answers: the tool,
 * resource or prompt, the client that asked (its `clientInfo` from `initialize`), the
 * negotiated protocol revision, the transport, how long the handler ran, and how it
 * ended (`ok`, `error` with the error's class and JSON-RPC code, or `tool_error` for a
 * result marked `isError`). Argument and result SIZES are recorded; argument VALUES only
 * when you opt in with `captureArguments`, and results and error messages never. The MCP
 * session id is replaced by a keyed digest before anything is queued.
 *
 * # How, without importing the MCP SDK
 *
 * This package has no dependencies and `@modelcontextprotocol/sdk` stays yours to pick
 * the version of, so the server is typed structurally and nothing of the MCP SDK is
 * imported. The official SDK keeps every request handler in one `Map` on the low-level
 * `Server` (`McpServer` exposes that as `.server`), and `setRequestHandler`, including the
 * lazy one `McpServer` runs on its first `tool()`, ends in that map's `set`. So the
 * wrapper wraps the handlers already in the map and the map's own `set`, which catches a
 * handler registered before or after this call, whatever version of the schema library
 * the SDK uses to name its methods. A server without that map but with
 * `setRequestHandler` has that method wrapped instead, which sees only later handlers;
 * anything else is told to use {@link Micaforge.recordMcpCall}.
 *
 * # The promises
 *
 * Measuring never changes an answer. A handler's result and its thrown error reach the
 * MCP SDK exactly as they left the handler, and a failure inside the measuring is caught
 * and logged, never thrown into the request.
 */
import { randomUUID } from "node:crypto";

import { jsonText, Micaforge, utf8Length } from "./index.js";
import type { Logger, McpMethod, McpStatus } from "./types.js";

/** A request handler, as the MCP SDK stores it: `(request, extra) => result`. */
type Handler = (request: any, extra?: any) => unknown;

/**
 * The parts of an MCP server this wrapper reads, structurally.
 *
 * The official low-level `Server` has all of them; `McpServer` holds one as `.server`.
 * `_requestHandlers` and `_serverInfo` are private in the SDK's types, which is why the
 * wrapper accepts any object and reads them through this shape at run time.
 */
export interface McpServerLike {
  /** The low-level server inside an `McpServer`. */
  server?: unknown;
  /** Where the official SDK keeps its request handlers, keyed by method. */
  _requestHandlers?: Map<string, Handler>;
  /** The official SDK's way to add a handler. */
  setRequestHandler?: (...args: any[]) => unknown;
  /** `clientInfo` from `initialize`, once the client has sent it. */
  getClientVersion?: () => { name?: string; version?: string } | undefined;
  /** The connected transport. Its class names the transport; it may carry a `sessionId`. */
  transport?: unknown;
  /** `serverInfo`, as the server was constructed with it. */
  _serverInfo?: { name?: string; version?: string };
}

/**
 * Opt in to storing arguments.
 *
 * `true` stores them after the built-in redaction: the value of any key that looks like
 * a secret or a contact (`password`, `token`, `apiKey`, `authorization`, `cookie`,
 * `session`, `email`, `phone` and the like) and any string that looks like an email
 * address become `"[redacted]"`. A function is your own redaction: it gets the arguments
 * and the call, and what it returns is stored (return `undefined` to store nothing).
 * Either way at most 4 KB of JSON is kept.
 */
export type CaptureArguments =
  | boolean
  | ((args: unknown, call: { method: McpMethod; name: string }) => unknown);

/** Options for {@link instrumentMcpServer}. */
export interface InstrumentOptions {
  /**
   * The client to record through. Pass one you already have, so its queue, its flush and
   * its `shutdown()` are yours; otherwise give `host`, `siteId` and `ingestKey` and one is
   * built (it flushes on a clean exit, see `flushOnExit`).
   */
  client?: Micaforge;
  /** With `siteId` and `ingestKey`, when no `client` is given. */
  host?: string;
  /** With `host` and `ingestKey`, when no `client` is given. */
  siteId?: number;
  /** With `host` and `siteId`, when no `client` is given. A server-side secret. */
  ingestKey?: string;
  /** Your server's name. Default: the `serverInfo.name` the server was built with. */
  serverName?: string;
  /** Your server's version. Default: `serverInfo.version`. */
  serverVersion?: string;
  /** The transport label. Default: read from the connected transport's class. */
  transport?: string;
  /** Store argument values too. Off by default; see {@link CaptureArguments}. */
  captureArguments?: CaptureArguments;
  /** Where the wrapper reports its own problems. `null` silences it. Default: console. */
  logger?: Logger | null;
}

/** The methods measured, plus `initialize`, which is watched for `clientInfo`. */
const MEASURED = new Set<string>(["tools/call", "resources/read", "prompts/get"]);

/** Marks a handler this module made, so wrapping twice is a no-op. */
const WRAPPED = Symbol.for("micaforge.mcp.wrapped");

/** Marks a server already instrumented. */
const INSTRUMENTED = Symbol.for("micaforge.mcp.instrumented");

/** Keys whose values the built-in redaction replaces. */
const SECRET_KEY =
  /pass(word)?|secret|token|auth|cookie|session|api[-_]?key|credential|private|email|e-mail|phone|ssn/i;

/** A string that looks like an email address. */
const EMAIL = /[^\s@]+@[^\s@]+\.[^\s@]+/;

/** What one instrumented server has learned about its session. */
interface SessionFacts {
  clientName?: string;
  clientVersion?: string;
  protocolVersion?: string;
  /** A stand-in session id for stdio, where one process is one session. */
  stdioSession: string;
}

/** Milliseconds on a monotonic clock where there is one. */
const clock = (): number =>
  typeof performance != "undefined" && typeof performance.now == "function" ? performance.now() : Date.now();

const text = (value: unknown): string | undefined =>
  typeof value == "string" && value ? value : undefined;

/**
 * Wrap an MCP server so every `tools/call`, `resources/read` and `prompts/get` it
 * answers is recorded. Returns the same server, so it can wrap a constructor call.
 *
 * Call it before or after registering tools, and before or after `connect()`: both
 * work. Calling it twice on one server records each call once.
 */
export function instrumentMcpServer<T extends object>(server: T, options: InstrumentOptions): T {
  const log: Logger | null =
    options.logger === null
      ? null
      : (options.logger ?? {
          warn: (message, detail) => console.warn(`[micaforge] ${message}`, detail ?? ""),
          error: (message, detail) => console.error(`[micaforge] ${message}`, detail ?? ""),
        });
  try {
    const low = lowLevel(server);
    if (!low) {
      log?.warn(
        "instrumentMcpServer: this is not an MCP SDK server; record calls with micaforge.recordMcpCall() instead",
      );
      return server;
    }
    if ((low as Record<symbol, unknown>)[INSTRUMENTED]) {
      log?.warn("instrumentMcpServer: this server is already instrumented; the first options stay in force");
      return server;
    }

    const client =
      options.client ??
      (options.host && options.siteId !== undefined && options.ingestKey
        ? new Micaforge({
            host: options.host,
            siteId: options.siteId,
            ingestKey: options.ingestKey,
            logger: options.logger,
          })
        : undefined);
    if (!client) {
      log?.error("instrumentMcpServer needs a `client`, or `host`, `siteId` and `ingestKey`; nothing is recorded");
      return server;
    }

    const facts: SessionFacts = { stdioSession: randomUUID() };
    const record = recorder(low, client, facts, options, log);
    const wrap = (handler: Handler, fixed?: string): Handler => {
      if ((handler as unknown as Record<symbol, unknown>)[WRAPPED]) return handler;
      const wrapped = function (this: unknown, request: any, extra?: any): unknown {
        const method = fixed ?? request?.method;
        if (method === "initialize") return observeInitialize(handler, this, request, extra, facts);
        if (!MEASURED.has(method)) return handler.call(this, request, extra);
        return measure(handler, this, request, extra, method as McpMethod, record);
      };
      (wrapped as unknown as Record<symbol, unknown>)[WRAPPED] = true;
      return wrapped;
    };

    const map = low._requestHandlers;
    if (map instanceof Map) {
      for (const [method, handler] of [...map]) {
        if ((MEASURED.has(method) || method === "initialize") && typeof handler == "function") {
          map.set(method, wrap(handler, method));
        }
      }
      const set = map.set;
      map.set = function (method: string, handler: Handler) {
        const watched = (MEASURED.has(method) || method === "initialize") && typeof handler == "function";
        return set.call(this, method, watched ? wrap(handler, method) : handler);
      };
    } else if (typeof low.setRequestHandler == "function") {
      const original = low.setRequestHandler;
      low.setRequestHandler = function (this: unknown, schema: unknown, handler: unknown, ...rest: unknown[]) {
        const next = typeof handler == "function" ? wrap(handler as Handler) : handler;
        return original.call(this, schema, next, ...rest);
      };
      log?.warn(
        "instrumentMcpServer: this server keeps its handlers out of reach, so only handlers registered after this call are measured",
      );
    }
    (low as Record<symbol, unknown>)[INSTRUMENTED] = true;
  } catch (error) {
    log?.error("instrumentMcpServer failed; the server runs unmeasured", error);
  }
  return server;
}

/** The object that holds the handlers: the server itself, or the `Server` in an `McpServer`. */
function lowLevel(server: object): McpServerLike | null {
  const holds = (s: unknown): s is McpServerLike =>
    !!s &&
    typeof s == "object" &&
    ((s as McpServerLike)._requestHandlers instanceof Map ||
      typeof (s as McpServerLike).setRequestHandler == "function");
  const inner = (server as McpServerLike).server;
  if (holds(inner)) return inner;
  return holds(server) ? server : null;
}

/** Remember `clientInfo` and the protocol revision, and hand the answer back untouched. */
function observeInitialize(
  handler: Handler,
  self: unknown,
  request: any,
  extra: unknown,
  facts: SessionFacts,
): unknown {
  try {
    const params = request?.params;
    facts.clientName = text(params?.clientInfo?.name) ?? facts.clientName;
    facts.clientVersion = text(params?.clientInfo?.version) ?? facts.clientVersion;
    facts.protocolVersion = text(params?.protocolVersion) ?? facts.protocolVersion;
  } catch {
    /* reading the request is never allowed to break the handshake */
  }
  const answer = handler.call(self, request, extra);
  // The server's answer names the revision actually negotiated, which is the one to keep.
  Promise.resolve(answer).then(
    (result: any) => {
      const negotiated = text(result?.protocolVersion);
      if (negotiated) facts.protocolVersion = negotiated;
    },
    () => {},
  );
  return answer;
}

/** What a finished call reports to {@link recorder}. */
interface Outcome {
  method: McpMethod;
  request: any;
  extra: any;
  started: number;
  startedAt: number;
  result?: unknown;
  error?: unknown;
  threw: boolean;
}

/** Run the handler, time it, record it, and return or rethrow exactly what it did. */
async function measure(
  handler: Handler,
  self: unknown,
  request: any,
  extra: any,
  method: McpMethod,
  record: (outcome: Outcome) => void,
): Promise<unknown> {
  const startedAt = Date.now();
  const started = clock();
  try {
    const result = await handler.call(self, request, extra);
    record({ method, request, extra, started, startedAt, result, threw: false });
    return result;
  } catch (error) {
    record({ method, request, extra, started, startedAt, error, threw: true });
    throw error;
  }
}

/** Build the function that turns one finished call into a recorded one. */
function recorder(
  low: McpServerLike,
  client: Micaforge,
  facts: SessionFacts,
  options: InstrumentOptions,
  log: Logger | null,
): (outcome: Outcome) => void {
  return (outcome) => {
    try {
      const durationMs = clock() - outcome.started;
      const params = outcome.request?.params ?? {};
      const name = text(outcome.method === "resources/read" ? params.uri : params.name) ?? "";
      const args = outcome.method === "resources/read" ? undefined : params.arguments;
      const argText = jsonText(args);
      const resultText = outcome.threw ? undefined : jsonText(outcome.result);

      let status: McpStatus = "ok";
      let errorType: string | undefined;
      let errorCode: number | undefined;
      if (outcome.threw) {
        status = "error";
        errorType = errorClass(outcome.error);
        errorCode = rpcCode(outcome.error);
      } else if ((outcome.result as { isError?: unknown } | undefined)?.isError === true) {
        status = "tool_error";
      }

      const transport = options.transport ?? transportName(low.transport);
      const sessionId =
        text(outcome.extra?.sessionId) ??
        text((low.transport as { sessionId?: unknown } | undefined)?.sessionId) ??
        (transport === "stdio" ? facts.stdioSession : undefined);
      const info = facts.clientName ? undefined : low.getClientVersion?.();

      client.recordMcpCall({
        method: outcome.method,
        name,
        durationMs,
        status,
        errorType,
        errorCode,
        argBytes: argText === undefined ? 0 : utf8Length(argText),
        resultBytes: resultText === undefined ? 0 : utf8Length(resultText),
        arguments: captured(options.captureArguments, args, outcome.method, name),
        clientName: facts.clientName ?? text(info?.name),
        clientVersion: facts.clientVersion ?? text(info?.version),
        protocolVersion: facts.protocolVersion,
        transport,
        sessionId,
        serverName: options.serverName ?? text(low._serverInfo?.name),
        serverVersion: options.serverVersion ?? text(low._serverInfo?.version),
        timestamp: outcome.startedAt,
      });
    } catch (error) {
      log?.error("recording an MCP call failed", error);
    }
  };
}

/** The arguments to store, after redaction, or `undefined` when not opted in. */
function captured(
  capture: CaptureArguments | undefined,
  args: unknown,
  method: McpMethod,
  name: string,
): unknown {
  if (!capture || args === undefined) return undefined;
  if (typeof capture == "function") return capture(args, { method, name });
  return redact(args, 0);
}

/**
 * The built-in redaction: secret-looking keys and email-looking strings become
 * `"[redacted]"`, deep, with a depth and width bound so a hostile document cannot make
 * the redaction itself the expensive part of a call.
 */
export function redact(value: unknown, depth = 0): unknown {
  if (depth > 6) return "[redacted]";
  if (Array.isArray(value)) return value.slice(0, 50).map((item) => redact(item, depth + 1));
  if (value && typeof value == "object") {
    const out: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value).slice(0, 100)) {
      out[key] = SECRET_KEY.test(key) ? "[redacted]" : redact(item, depth + 1);
    }
    return out;
  }
  if (typeof value == "string" && EMAIL.test(value)) return "[redacted]";
  return value;
}

/** The error's class: its `name` when it has one, else its constructor's. */
function errorClass(error: unknown): string {
  if (error && typeof error == "object") {
    const named = text((error as { name?: unknown }).name);
    if (named) return named.slice(0, 128);
    const ctor = text((error as { constructor?: { name?: unknown } }).constructor?.name);
    if (ctor) return ctor.slice(0, 128);
  }
  return typeof error;
}

/** A JSON-RPC error code, which is an integer; a Node error's `ECONNREFUSED` is not one. */
function rpcCode(error: unknown): number | undefined {
  const code = (error as { code?: unknown } | null | undefined)?.code;
  return typeof code == "number" && Number.isInteger(code) ? code : undefined;
}

/** A transport's label from its class name: `StdioServerTransport` is `stdio`. */
export function transportName(transport: unknown): string | undefined {
  const ctor = text((transport as { constructor?: { name?: unknown } } | undefined)?.constructor?.name);
  if (!ctor || ctor === "Object") return undefined;
  if (/stdio/i.test(ctor)) return "stdio";
  if (/streamable/i.test(ctor)) return "streamable-http";
  if (/sse/i.test(ctor)) return "sse";
  if (/websocket/i.test(ctor)) return "websocket";
  if (/inmemory/i.test(ctor)) return "in-memory";
  return ctor.replace(/(Server)?Transport$/, "").toLowerCase() || undefined;
}
