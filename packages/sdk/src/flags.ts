/**
 * Feature flags.
 *
 * Reads `GET /api/flags/evaluate?site=<id>&visitor=<subject>&identity=<id>` and caches
 * the answer in memory for `ttlMs`. Reads are synchronous and answer in this order:
 *
 *   1. the evaluated set from the server, when it has arrived
 *   2. the `bootstrap` payload the server rendered into the page
 *   3. a deterministic local rule, bucketed by `hash(visitor + ":" + key)`
 *
 * Because step 3 is a pure function of the subject and the key, a flag holds the same
 * value on every render and never flickers while the network is in flight.
 *
 * No storage is used. The tracker's one storage key is the opt-out key, and that is all
 * this SDK will ever write.
 */
import { G } from "./core.js";
import type { FlagFallback, FlagRecord, FlagsClient, FlagsConfig, PropValue } from "./types.js";

/**
 * FNV-1a, 32-bit. Deterministic across runtimes, no dependencies, and stable for a
 * given subject: the same visitor always lands in the same bucket for a given key.
 */
export function hashBucket(input: string, buckets = 100): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < input.length; i++) {
    h ^= input.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0) % buckets;
}

const off = (key: string): FlagRecord => ({ key: key, enabled: false });

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

/** Build a flag client. Never throws, never rejects, works with no network at all. */
export function createFlags(config: FlagsConfig): FlagsClient {
  const host = String(config.host || "").replace(/\/+$/, "");
  const ttl = config.ttlMs == null ? 60000 : config.ttlMs;
  const bootstrap = normaliseAll(config.bootstrap);
  const fallback = config.fallback || {};
  const listeners: ((f: Record<string, FlagRecord>) => void)[] = [];
  const exposed: Record<string, 1> = {};

  let visitor = config.visitor || "";
  let cache: Record<string, FlagRecord> | null = null;
  let cachedAt = 0;
  let inflight: Promise<Record<string, FlagRecord>> | null = null;

  // Without a stable subject, bucketing is stable for this page load only. Pass
  // `visitor` (a user id, an account id, anything durable) for stable bucketing.
  if (!visitor) visitor = "anon-" + hashBucket("" + Math.random() + Date.now(), 1e9);

  const subject = () => config.identity || visitor;

  const bucket = (key: string) => hashBucket(subject() + ":" + key);

  /** The deterministic client-side answer, used before and instead of the network. */
  const local = (key: string): FlagRecord => {
    const rule: FlagFallback | undefined = fallback[key];
    if (!rule) return bootstrap[key] || off(key);
    const b = bucket(key);
    const pct = rule.rolloutPct == null ? 100 : rule.rolloutPct;
    if (b >= pct) return { key: key, enabled: !!rule.off };
    const variants = rule.variants;
    if (variants && variants.length) {
      let total = 0;
      for (const v of variants) total += v.weight == null ? 1 : v.weight;
      let point = (hashBucket(subject() + ":" + key + ":v", 1e6) / 1e6) * total;
      for (const v of variants) {
        point -= v.weight == null ? 1 : v.weight;
        if (point < 0) return { key: key, enabled: true, variant: v.key };
      }
      return { key: key, enabled: true, variant: variants[variants.length - 1].key };
    }
    return { key: key, enabled: true };
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
    const rec = (cache && cache[key]) || bootstrap[key] || local(key);
    if (!exposed[key] && config.onExposure) {
      exposed[key] = 1;
      try {
        config.onExposure(key, rec);
      } catch {}
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
        (config.identity ? "&identity=" + encodeURIComponent(config.identity) : "");
      inflight = f(url, { credentials: "omit", mode: "cors" })
        .then((r: any) => (r && r.ok ? r.json() : null))
        .then((body: any) => {
          const data = body && body.data !== undefined ? body.data : body;
          const raw = data && data.flags !== undefined ? data.flags : data;
          const next = normaliseAll(raw);
          cache = next;
          cachedAt = Date.now();
          inflight = null;
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
    all: () => ({ ...bootstrap, ...cache }),
    onChange: (cb) => {
      listeners.push(cb);
      return () => {
        const i = listeners.indexOf(cb);
        if (i > -1) listeners.splice(i, 1);
      };
    },
    bucket: bucket,
    setVisitor: (v) => {
      if (v && v !== visitor) {
        visitor = v;
        cache = null;
        cachedAt = 0;
      }
    },
    clearCache: () => {
      cache = null;
      cachedAt = 0;
    },
  };
}
