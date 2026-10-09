/**
 * Feature flags, decided on your server with no network call per request.
 *
 * The client fetches the site's whole ruleset once (`GET /api/flags/ruleset`, with an
 * API key holding `manage:read`), keeps it in memory, refreshes it in the background, and
 * decides every flag locally with the shared evaluator (`flag-eval.ts`). That evaluator
 * is the same rule the Micaforge server runs for browsers, held to it by one file of test
 * vectors that the server, the browser SDK and this package all assert, so a visitor gets
 * the same answer from a server render here as from the browser afterwards.
 *
 * # The promises
 *
 * - **A request never waits on Micaforge.** Decisions are synchronous reads of the
 *   cached ruleset. Before the first ruleset arrives every flag reads as its fallback
 *   (`false` unless you pass one), and a failed refresh keeps the last good ruleset.
 * - **It never throws into your code.** Network errors, bad JSON and an expired key end
 *   in `onError` and leave the cache as it was.
 * - **It never keeps your process alive.** Both timers are `unref()`ed.
 * - **The API key never leaves the `Authorization` header**: it is in no URL, no log line
 *   and no error this module builds.
 *
 * Evaluations and exposures are counted in memory and sent in batches to
 * `POST /api/flags/events` (no key needed there), so the flag's screen shows what each
 * variant reached. `stats: false` turns that off.
 */
import { compileRuleset, type CompiledRuleset, type FlagDecision, type FlagRuleset } from "./flag-eval.js";

/** How the client is set up. */
export interface FlagsClientConfig {
  /** the Micaforge origin, such as `https://analytics.example.com` */
  host: string;
  /** an API key (`mfg_...`) with the `manage:read` scope, for the site */
  apiKey: string;
  /** the numeric site id */
  site: number | string;
  /** the environment this process runs in (`production`, `staging`, ...) */
  environment?: string;
  /** how often the ruleset is fetched again, in ms. Default 30 000. */
  refreshIntervalMs?: number;
  /** override the fetch used, for tests or a custom transport */
  fetchImpl?: typeof fetch;
  /** send evaluation and exposure counts. Default true. */
  stats?: boolean;
  /** how often counts are sent, in ms. Default 10 000. */
  flushIntervalMs?: number;
  /** told about every failure, which is otherwise silent */
  onError?: (err: unknown) => void;
}

/** Who a decision is for. */
export interface FlagUser {
  /** a stable id for the visitor or the session */
  visitor?: string;
  /** the signed-in user's id; buckets ahead of `visitor` when present */
  identity?: string;
  /** attributes the conditions and target groups test, such as `{ plan: "pro" }` */
  context?: Record<string, unknown>;
}

/** A server-side flag client. */
export interface FlagsClient {
  /** resolves after the first ruleset load attempt, whether it worked or not */
  ready(): Promise<void>;
  /** decide one flag */
  evaluate(key: string, user?: FlagUser): FlagDecision;
  isEnabled(key: string, user?: FlagUser, fallback?: boolean): boolean;
  variant(key: string, user?: FlagUser, fallback?: string): string | undefined;
  /** decide every flag */
  all(user?: FlagUser): Record<string, FlagDecision>;
  /** fetch the ruleset now */
  refresh(): Promise<void>;
  /** send the counted evaluations and exposures now */
  flush(): Promise<void>;
  /** stop the timers and send what is counted */
  close(): Promise<void>;
}

/** Most entries one stats batch carries; the server reads no more. */
const BATCH_MAX = 200;

/** Most distinct (flag, subject) pairs remembered for exposure counting per window. */
const EXPOSURE_MEMORY = 50000;

const UNKNOWN: FlagDecision = { enabled: false, variant: null, reason: "unknown" };

/** One count waiting to be sent. */
interface Tally {
  key: string;
  variant: string | null;
  enabled: boolean;
  kind: "evaluation" | "exposure";
  count: number;
}

/** Build a flag client and start loading the ruleset. */
export function createFlagsClient(config: FlagsClientConfig): FlagsClient {
  const host = String(config.host || "").replace(/\/+$/, "");
  const refreshMs = config.refreshIntervalMs ?? 30000;
  const flushMs = config.flushIntervalMs ?? 10000;
  const statsOn = config.stats !== false;
  const fail = (err: unknown) => {
    try {
      config.onError?.(err);
    } catch {}
  };
  const doFetch = (): typeof fetch | undefined => config.fetchImpl ?? globalThis.fetch;

  let rules: CompiledRuleset | null = null;
  let version = "";
  const tallies = new Map<string, Tally>();
  const exposed = new Set<string>();

  const refresh = async (): Promise<void> => {
    const f = doFetch();
    if (!f) return fail(new Error("micaforge flags: no fetch is available"));
    try {
      const res = await f(`${host}/api/flags/ruleset?site=${encodeURIComponent(String(config.site))}`, {
        headers: { authorization: `Bearer ${config.apiKey}`, accept: "application/json" },
      });
      if (!res.ok) return fail(new Error(`micaforge flags: the ruleset answered ${res.status}`));
      const body: any = await res.json();
      const data = body && body.data !== undefined ? body.data : body;
      if (!data || typeof data != "object" || !data.ruleset) {
        return fail(new Error("micaforge flags: the ruleset answer has no ruleset"));
      }
      if (rules && data.version && data.version === version) return;
      rules = compileRuleset(data.ruleset as FlagRuleset);
      version = typeof data.version == "string" ? data.version : "";
    } catch (err) {
      fail(err);
    }
  };

  const count = (key: string, d: FlagDecision, kind: Tally["kind"]) => {
    const id = `${kind}\u0000${key}\u0000${d.enabled ? 1 : 0}\u0000${d.variant ?? ""}`;
    const t = tallies.get(id);
    if (t) t.count++;
    else tallies.set(id, { key, variant: d.variant, enabled: d.enabled, kind, count: 1 });
  };

  const record = (key: string, d: FlagDecision, subject: string) => {
    if (!statsOn || d.reason == "unknown") return;
    count(key, d, "evaluation");
    const pair = `${key}\u0000${subject}`;
    if (!exposed.has(pair) && exposed.size < EXPOSURE_MEMORY) {
      exposed.add(pair);
      count(key, d, "exposure");
    }
  };

  const flush = async (): Promise<void> => {
    exposed.clear();
    if (!tallies.size) return;
    const events = Array.from(tallies.values());
    tallies.clear();
    const f = doFetch();
    if (!f) return;
    for (let i = 0; i < events.length; i += BATCH_MAX) {
      try {
        const res = await f(`${host}/api/flags/events`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            site: Number(config.site),
            environment: config.environment,
            source: "server",
            events: events.slice(i, i + BATCH_MAX),
          }),
        });
        if (!res.ok && res.status != 204) fail(new Error(`micaforge flags: stats answered ${res.status}`));
      } catch (err) {
        fail(err);
      }
    }
  };

  const input = (user?: FlagUser) => {
    const subject = (user?.identity || user?.visitor || "").toString();
    return { subject, context: user?.context, environment: config.environment };
  };

  const evaluate = (key: string, user?: FlagUser): FlagDecision => {
    try {
      if (!rules) return UNKNOWN;
      const i = input(user);
      const d = rules.evaluate(key, i);
      record(key, d, i.subject);
      return d;
    } catch (err) {
      fail(err);
      return UNKNOWN;
    }
  };

  const first = refresh();
  const refreshTimer = setInterval(() => void refresh(), refreshMs);
  const flushTimer = statsOn ? setInterval(() => void flush(), flushMs) : undefined;
  // Never hold the process open for analytics.
  (refreshTimer as any).unref?.();
  (flushTimer as any)?.unref?.();

  return {
    ready: () => first,
    evaluate,
    isEnabled: (key, user, fallback = false) => {
      const d = evaluate(key, user);
      return d.reason == "unknown" ? fallback : d.enabled;
    },
    variant: (key, user, fallback) => {
      const d = evaluate(key, user);
      return d.variant ?? fallback;
    },
    all: (user) => {
      const out: Record<string, FlagDecision> = {};
      if (!rules) return out;
      for (const key of rules.keys) {
        Object.defineProperty(out, key, {
          value: evaluate(key, user),
          enumerable: true,
          writable: true,
          configurable: true,
        });
      }
      return out;
    },
    refresh,
    flush,
    close: async () => {
      clearInterval(refreshTimer);
      if (flushTimer) clearInterval(flushTimer);
      await flush();
    },
  };
}
