/**
 * Feature flags.
 *
 * Reads `GET /api/flags/evaluate?site=<id>&visitor=<subject>&identity=<id>` and caches
 * the answer in memory for `ttlMs`. Reads are synchronous and answer in this order:
 *
 *   1. the evaluated set from the server, when it has arrived
 *   2. a ruleset handed to the client (`rules`), decided locally
 *   3. the `bootstrap` payload the server rendered into the page
 *   4. a deterministic local rule (`fallback`)
 *
 * Steps 2 and 4 run the shared evaluator (`flag-eval.ts`), the same rule the server and
 * the Node SDK run, held to it by one file of test vectors. So a fallback rule that
 * mirrors a flag's server configuration lands each visitor in the bucket the server would
 * choose, and a flag holds the same value before, during and after the network call.
 *
 * Evaluations and first reads (exposures) are counted and sent in batches to
 * `POST /api/flags/events`, so the flag's screen can show what each variant reached.
 * Turn that off with `stats: false`.
 *
 * No storage is used. The tracker's one storage key is the opt-out key, and that is all
 * this SDK will ever write.
 */
import { G } from "./core.js";
import { compileRuleset, flagBucket, type CompiledRuleset, type FlagDecision } from "./flag-eval.js";
import type { FlagFallback, FlagRecord, FlagsClient, FlagsConfig, PropValue } from "./types.js";

/**
 * FNV-1a, 32-bit. Deterministic across runtimes, no dependencies, and stable for a
 * given subject: the same visitor always lands in the same bucket for a given key.
 *
 * Kept for callers that bucket their own things with it. Flags no longer use it: they
 * bucket with {@link flagBucket}, the server's hash.
 */
export function hashBucket(input: string, buckets = 100): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0) % buckets;
}

/** Most entries one stats batch carries; the server reads no more. */
const BATCH_MAX = 200;

/** How long counts wait before they are sent, in ms. */
const FLUSH_DELAY = 10000;

const off = (key: string): FlagRecord => ({ key: key, enabled: false });

/** A shared-evaluator decision as the client's record. */
const fromDecision = (key: string, d: FlagDecision): FlagRecord =>
  d.variant ? { key: key, enabled: d.enabled, variant: d.variant } : { key: key, enabled: d.enabled };

/** Accept `true`, `"variant-a"` or a full record from a bootstrap payload. */
const normalise = (key: string, v: FlagRecord | boolean | string): FlagRecord => {
  if (v && typeof v == "object") return { ...v, key: key };
  if (typeof v == "string") return { key: key, enabled: true, variant: v };
  return { key: key, enabled: !!v };
};

const normaliseAll = (raw: any): Record<string, FlagRecord> => {
  const out: Record<string, FlagRecord> = {};
  if (raw && typeof raw == "object") {
    if (Array.isArray(raw)) {
      for (const r of raw) if (r && r.key) out[r.key] = normalise(r.key, r);
    } else {
      for (const k in raw) out[k] = normalise(k, raw[k]);
    }
  }
  return out;
};

/** One count waiting to be sent. */
interface Tally {
  key: string;
  variant: string | null;
  enabled: boolean;
  kind: "evaluation" | "exposure";
  count: number;
}

/** Build a flag client. Never throws, never rejects, works with no network at all. */
export function createFlags(config: FlagsConfig): FlagsClient {
  const host = String(config.host || "").replace(/\/+$/, "");
  const ttl = config.ttlMs == null ? 60000 : config.ttlMs;
  const bootstrap = normaliseAll(config.bootstrap);
  const fallback = config.fallback || {};
  const listeners: ((f: Record<string, FlagRecord>) => void)[] = [];
  const exposed: Record<string, 1> = {};
  const rules: CompiledRuleset | null = config.rules ? compileRuleset(config.rules) : null;

  let visitor = config.visitor || "";
  let cache: Record<string, FlagRecord> | null = null;
  let cachedAt = 0;
  let inflight: Promise<Record<string, FlagRecord>> | null = null;
  let ruled: Record<string, FlagRecord> | null = null;

  // Without a stable subject, bucketing is stable for this page load only. Pass
  // `visitor` (a user id, an account id, anything durable) for stable bucketing.
  if (!visitor) visitor = "anon-" + hashBucket("" + Math.random() + Date.now(), 1e9);

  const subject = () => config.identity || visitor;
  const input = () => ({ subject: subject(), context: config.context, environment: config.environment });

  /* ── stats ─────────────────────────────────────────────────────────── */

  const statsOn = config.stats !== false && !!config.site && !!G.window;
  const tallies = new Map<string, Tally>();
  let timer: any = 0;
  let wired = false;

  const send = (body: string) => {
    const url = host + "/api/flags/events";
    const nav: any = G.navigator;
    try {
      // `text/plain` keeps the beacon a simple request: no preflight, no CORS round trip.
      if (nav && nav.sendBeacon && nav.sendBeacon(url, new Blob([body], { type: "text/plain" }))) return;
    } catch {}
    try {
      const f: any = config.fetchImpl || G.fetch;
      if (f) {
        f(url, {
          method: "POST",
          body: body,
          keepalive: true,
          credentials: "omit",
          headers: { "content-type": "text/plain" },
        }).catch(() => {});
      }
    } catch {}
  };

  const flush = () => {
    if (timer) clearTimeout(timer);
    timer = 0;
    if (!tallies.size) return;
    const events = Array.from(tallies.values());
    tallies.clear();
    for (let i = 0; i < events.length; i += BATCH_MAX) {
      send(
        JSON.stringify({
          site: Number(config.site),
          environment: config.environment || undefined,
          source: "browser",
          events: events.slice(i, i + BATCH_MAX),
        }),
      );
    }
  };

  const wire = () => {
    if (wired) return;
    wired = true;
    try {
      const w: any = G.window;
      w.addEventListener("pagehide", flush);
      w.addEventListener("visibilitychange", () => {
        const d: any = G.document;
        if (d && d.visibilityState == "hidden") flush();
      });
    } catch {}
  };

  const count = (rec: FlagRecord, kind: Tally["kind"]) => {
    if (!statsOn) return;
    const id = kind + "\u0000" + rec.key + "\u0000" + (rec.enabled ? 1 : 0) + "\u0000" + (rec.variant || "");
    const t = tallies.get(id);
    if (t) t.count++;
    else
      tallies.set(id, {
        key: rec.key,
        variant: rec.variant || null,
        enabled: !!rec.enabled,
        kind: kind,
        count: 1,
      });
    wire();
    if (!timer) timer = setTimeout(flush, FLUSH_DELAY);
  };

  /* ── answering ─────────────────────────────────────────────────────── */

  /** Every flag of the ruleset, decided once per subject and counted once. */
  const fromRules = (): Record<string, FlagRecord> | null => {
    if (!rules) return null;
    if (!ruled) {
      const decided = rules.evaluateAll(input());
      const next: Record<string, FlagRecord> = {};
      for (const k of rules.keys) {
        next[k] = fromDecision(k, decided[k]);
        count(next[k], "evaluation");
      }
      ruled = next;
    }
    return ruled;
  };

  /** The deterministic client-side answer, used before and instead of the network. */
  const local = (key: string): FlagRecord => {
    const rule: FlagFallback | undefined = fallback[key];
    if (!rule) return bootstrap[key] || off(key);
    const decided = compileRuleset({
      flags: [
        {
          key: key,
          enabled: true,
          rollout_pct: rule.rolloutPct == null ? 100 : rule.rolloutPct,
          variants: rule.variants,
        },
      ],
    }).evaluate(key, input());
    return decided.enabled ? fromDecision(key, decided) : { key: key, enabled: !!rule.off };
  };

  const emit = () => {
    const all = cache || {};
    for (const cb of listeners.slice()) {
      try {
        cb(all);
      } catch {}
    }
  };

  const get = (key: string): FlagRecord => {
    const ruledSet = fromRules();
    const rec = (cache && cache[key]) || (ruledSet && ruledSet[key]) || bootstrap[key] || local(key);
    if (!exposed[key]) {
      exposed[key] = 1;
      count(rec, "exposure");
      if (config.onExposure) {
        try {
          config.onExposure(key, rec);
        } catch {}
      }
    }
    return rec;
  };

  const load = (force?: boolean): Promise<Record<string, FlagRecord>> => {
    try {
      if (!force && cache && Date.now() - cachedAt < ttl) return Promise.resolve(cache);
      if (inflight) return inflight;
      const f: any = config.fetchImpl || G.fetch;
      if (!f || !config.site) return Promise.resolve(cache || bootstrap);
      const url =
        host +
        "/api/flags/evaluate?site=" +
        encodeURIComponent("" + config.site) +
        "&visitor=" +
        encodeURIComponent(visitor) +
        (config.identity ? "&identity=" + encodeURIComponent(config.identity) : "") +
        (config.environment ? "&environment=" + encodeURIComponent(config.environment) : "") +
        (config.context ? "&context=" + encodeURIComponent(JSON.stringify(config.context)) : "");
      inflight = f(url, { credentials: "omit", mode: "cors" })
        .then((r: any) => (r && r.ok ? r.json() : null))
        .then((body: any) => {
          const data = body && body.data !== undefined ? body.data : body;
          const raw = data && data.flags !== undefined ? data.flags : data;
          const next = normaliseAll(raw);
          cache = next;
          cachedAt = Date.now();
          inflight = null;
          for (const k in next) count(next[k], "evaluation");
          emit();
          return next;
        })
        .catch(() => {
          inflight = null;
          return cache || bootstrap;
        });
      return inflight as Promise<Record<string, FlagRecord>>;
    } catch {
      return Promise.resolve(cache || bootstrap);
    }
  };

  return {
    load: load,
    get: get,
    isEnabled: (key, dflt) => {
      try {
        const r = get(key);
        return r ? r.enabled : !!dflt;
      } catch {
        return !!dflt;
      }
    },
    variant: (key, dflt) => {
      try {
        return get(key).variant || dflt;
      } catch {
        return dflt;
      }
    },
    payload: (key): PropValue | undefined => {
      try {
        return get(key).payload;
      } catch {
        return undefined;
      }
    },
    all: () => ({ ...bootstrap, ...fromRules(), ...cache }),
    onChange: (cb) => {
      listeners.push(cb);
      return () => {
        const i = listeners.indexOf(cb);
        if (i > -1) listeners.splice(i, 1);
      };
    },
    bucket: (key) => Math.floor(flagBucket(key, subject()) / 100),
    setVisitor: (v) => {
      if (v && v !== visitor) {
        visitor = v;
        cache = null;
        cachedAt = 0;
        ruled = null;
      }
    },
    clearCache: () => {
      cache = null;
      cachedAt = 0;
      ruled = null;
    },
    flushStats: () => {
      try {
        flush();
      } catch {}
    },
  };
}
