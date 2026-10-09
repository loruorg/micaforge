import type { TrackerSetup } from "./tracker.js";

/**
 * The overlay's own vocabulary: what it records about each event, each flag and the
 * identity, and the options it takes.
 */

/** What became of one event. */
export type EventStatus =
  /** handed to the transport (`sendBeacon` took it, `fetch` was called, the pixel was set) */
  | "sent"
  /** the tracker decided not to send it; `reason` says which rule */
  | "blocked"
  /** the request went out and failed (a `fetch` that rejected) */
  | "failed";

/** One event, as the overlay saw it. */
export interface DevtoolsEvent {
  /** increasing, for keys and ordering */
  id: number;
  /** wall-clock time it was seen, ms since the epoch */
  at: number;
  /** ms since the page started loading, for reading timing at a glance */
  sincePageStart: number;
  /** the payload's `t`: pageview, custom, engagement, identify, error, ... */
  kind: string;
  /** the payload's `n`, for custom events and plugin events */
  name?: string;
  /** the address sent (`u`), or the address it would have been sent with */
  url: string;
  /** the properties sent (`p`) */
  props?: Record<string, unknown>;
  /** the wire payload exactly as sent, or the preview of what would have been sent */
  payload: Record<string, unknown>;
  status: EventStatus;
  /** how it left: `beacon`, `fetch`, `pixel` or `batch` */
  via?: string;
  /** for `blocked`: the rule, in a sentence */
  reason?: string;
  /** when the address sent differs from the address bar: masked, stripped or overridden */
  rewritten?: { from: string; to: string; why: string };
  /** problems worth fixing before this ships */
  warnings: string[];
}

/** One flag, as the server last answered it for this page. */
export interface DevtoolsFlag {
  key: string;
  enabled: boolean;
  variant: string | null;
  /** the server's reason code: match, rollout, conditions, ... */
  reason: string;
  /** what the reason means, in a sentence */
  explanation: string;
  /** evaluation and exposure counts the SDK reported */
  evaluations: number;
  exposures: number;
  /** when this answer arrived */
  at: number;
}

/** Who the tracker thinks this is. */
export interface DevtoolsIdentity {
  /** the id from `identify()`; empty when anonymous */
  id: string;
  /** the traits of the last identify or setTraits */
  traits: Record<string, unknown>;
  /** when it last changed */
  at: number;
}

/** How the overlay is set up. Every field is optional. */
export interface DevtoolsOptions {
  /**
   * The event names your site is meant to send. With a list, any other name is flagged
   * as unknown, which catches a typo (`sign_up` for `signup`) before it splits a goal in
   * two. Without one, names are only checked for shape.
   */
  knownEvents?: string[];
  /** open the panel at once rather than showing only the launcher. Default false. */
  open?: boolean;
  /**
   * The keyboard toggle, as `Alt+Shift+M` style text. Default `Alt+Shift+M`. Modifier
   * names: `Alt`, `Shift`, `Ctrl`, `Meta`; the key is matched by its physical position, so
   * a layout or an Option-key character does not change it.
   */
  hotkey?: string;
  /** the most events kept; older ones are forgotten. Default 500. */
  maxEvents?: number;
  /**
   * The tracker's settings, for a tracker started from the npm package with `init()`:
   * pass the same values (`{ site, host, debug, sample, mask, exclude, stripQuery, ... }`)
   * so the overlay can say why an event was not sent. A script-tag tracker is read from
   * its tag and needs none of this.
   */
  tracker?: Partial<Omit<TrackerSetup, "source">>;
}

/** What `mountDevtools` returns. */
export interface DevtoolsHandle {
  open(): void;
  close(): void;
  toggle(): void;
  /** forget every recorded event */
  clear(): void;
  /** remove the overlay and every hook it installed */
  destroy(): void;
  /** the events recorded so far, oldest first */
  events(): DevtoolsEvent[];
  /** the flags seen so far */
  flags(): DevtoolsFlag[];
  identity(): DevtoolsIdentity;
}
