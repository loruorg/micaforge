/**
 * The overlay's record: events, flags and identity, and who to tell when they change.
 *
 * Bounded, because a page left open for an afternoon with the overlay on would otherwise
 * hold every engagement ping it ever sent.
 */
import type { DevtoolsEvent, DevtoolsFlag, DevtoolsIdentity } from "./types.js";

/** What a flag's reason code means. The codes are the server's (`micaforge_core::flags`). */
export const REASONS: Record<string, string> = {
  match: "Every rule passed: the flag is on for this visitor.",
  rollout: "The visitor's bucket is outside the rollout percentage.",
  conditions: "The flag's conditions did not match this visitor's context.",
  target_group: "The visitor is in none of the flag's target groups.",
  dependency: "A prerequisite flag did not evaluate the way this flag needs.",
  disabled: "The flag is switched off.",
  no_visitor: "A partial rollout needs a visitor id to bucket, and none was sent.",
  unknown: "The site has no flag with this key.",
};

export type Listener = () => void;

export class Store {
  events: DevtoolsEvent[] = [];
  flags = new Map<string, DevtoolsFlag>();
  identity: DevtoolsIdentity = { id: "", traits: {}, at: 0 };
  private nextId = 1;
  private listeners = new Set<Listener>();

  constructor(private readonly max = 500) {}

  add(event: Omit<DevtoolsEvent, "id">): DevtoolsEvent {
    const full = { ...event, id: this.nextId++ };
    this.events.push(full);
    if (this.events.length > this.max) this.events.splice(0, this.events.length - this.max);
    this.changed();
    return full;
  }

  /** Record the server's answer for a set of flags. */
  setFlags(decisions: Record<string, { enabled?: unknown; variant?: unknown; reason?: unknown }>): void {
    const at = Date.now();
    for (const key of Object.keys(decisions)) {
      const d = decisions[key] || {};
      const reason = typeof d.reason == "string" ? d.reason : d.enabled ? "match" : "";
      const prior = this.flags.get(key);
      this.flags.set(key, {
        key,
        enabled: d.enabled === true,
        variant: typeof d.variant == "string" ? d.variant : null,
        reason,
        explanation: REASONS[reason] || "Decided without a reason from the server (a bootstrap or local value).",
        evaluations: prior ? prior.evaluations : 0,
        exposures: prior ? prior.exposures : 0,
        at,
      });
    }
    this.changed();
  }

  /** Add the evaluation and exposure counts an SDK reported. */
  countFlag(key: string, kind: string, count: number, enabled: boolean, variant: string | null): void {
    const flag = this.flags.get(key) || {
      key,
      enabled,
      variant,
      reason: "",
      explanation: "Seen only in the SDK's counts; the server's answer was not observed on this page.",
      evaluations: 0,
      exposures: 0,
      at: Date.now(),
    };
    if (kind == "exposure") flag.exposures += count;
    else flag.evaluations += count;
    this.flags.set(key, flag);
    this.changed();
  }

  setIdentity(id: string, traits?: Record<string, unknown>): void {
    this.identity = { id, traits: traits && Object.keys(traits).length ? traits : this.identity.traits, at: Date.now() };
    this.changed();
  }

  clear(): void {
    this.events = [];
    this.changed();
  }

  /** Tell the listeners that a recorded event changed in place (a request that failed later). */
  touch(): void {
    this.changed();
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private changed(): void {
    for (const fn of Array.from(this.listeners)) {
      try {
        fn();
      } catch {}
    }
  }
}
