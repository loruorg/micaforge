/**
 * React bindings for Micaforge.
 *
 * Router-agnostic: `usePageviews()` takes whatever string your router calls the current
 * location. Without one it leans on the core's history patching, which already covers
 * `pushState`, `replaceState`, `popstate` and `hashchange`.
 *
 * The directive below is preserved by the build, so the published entry is a client
 * module in the Next App Router without anything being injected at bundle time.
 */
"use client";

import {
  cloneElement,
  createContext,
  createElement,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  type ReactElement,
  type ReactNode,
} from "react";
import { getTracker, init, type InitConfig } from "../index.js";
import type { Props, TrackerApi } from "../types.js";

const Ctx = createContext<TrackerApi | undefined>(undefined);

export interface MicaforgeProviderProps {
  config: InitConfig;
  children?: ReactNode;
}

/**
 * Starts the tracker once and hands it down. Mounting it twice (React 18 strict mode,
 * a re-render, a second provider) reuses the one instance.
 */
export function MicaforgeProvider({ config, children }: MicaforgeProviderProps): ReactElement {
  const tracker = useMemo(() => {
    try {
      return init(config);
    } catch {
      return undefined;
    }
    // The config object is usually an inline literal; re-running on identity change
    // would restart the tracker on every render, so key off the values that matter.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [config.site, config.host]);

  return createElement(Ctx.Provider, { value: tracker }, children);
}

/** The tracker. Falls back to the global instance when there is no provider above. */
export function useMicaforge(): TrackerApi | undefined {
  const fromCtx = useContext(Ctx);
  return fromCtx || getTracker();
}

/** A stable `track` function: `const track = useTrackEvent(); track("signup", {...})`. */
export function useTrackEvent(): (name: string, props?: Props) => void {
  const mf = useMicaforge();
  return useCallback(
    (name: string, props?: Props) => {
      try {
        mf?.track(name, props);
      } catch {}
    },
    [mf],
  );
}

/**
 * Send a pageview whenever `location` changes.
 * `usePageviews(useLocation().pathname)` for React Router, `usePathname()` for Next.
 * Pass nothing and the core's own SPA listener does the work instead.
 */
export function usePageviews(location?: string | null, props?: Props): void {
  const mf = useMicaforge();
  const last = useRef<string | null>(null);
  useEffect(() => {
    if (location == null || !mf) return;
    if (last.current === location) return;
    last.current = location;
    try {
      mf.pageview({ props: props });
    } catch {}
    // `props` is deliberately not a dependency: a new object each render would
    // re-send the same pageview.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [location, mf]);
}

export interface TrackClickProps {
  /** custom event name */
  event: string;
  props?: Props;
  /** the element to wrap. Its own onClick still runs. */
  children: ReactElement<{ onClick?: (e: any) => void }>;
}

/** Wrap any clickable element: `<TrackClick event="cta_click"><button …/></TrackClick>`. */
export function TrackClick({ event, props, children }: TrackClickProps): ReactElement {
  const track = useTrackEvent();
  const own = children.props.onClick;
  return cloneElement(children, {
    onClick: (e: any) => {
      track(event, props);
      if (typeof own == "function") own(e);
    },
  });
}

export { init, getTracker } from "../index.js";
export type { InitConfig } from "../index.js";
export type { Props, TrackerApi, TrackerConfig } from "../types.js";
