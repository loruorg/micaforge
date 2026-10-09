/**
 * Next.js App Router bindings.
 *
 * This entry imports nothing from `next` itself, so it works in the App Router, the
 * Pages Router and any Next version that can render a client component. Route changes
 * are picked up two ways: the core already patches `pushState`/`replaceState`, and
 * `useNextPageviews(pathname, search)` lets you drive them straight from
 * `next/navigation` when you would rather be explicit.
 */
"use client";

import { useEffect, useRef, type ReactElement } from "react";
import { getTracker, init, type InitConfig } from "../index.js";
import type { Props, TrackerApi } from "../types.js";

export interface MicaforgeAnalyticsProps extends InitConfig {}

/**
 * Drop into `app/layout.tsx`:
 *
 * ```tsx
 * import { MicaforgeAnalytics } from "@micaforge/sdk/next";
 * …
 * <MicaforgeAnalytics site={1} host="https://micaforge.example.com" />
 * ```
 *
 * Renders nothing. Starts the tracker on the client and leaves it running across
 * client-side navigations.
 */
export function MicaforgeAnalytics(props: MicaforgeAnalyticsProps): ReactElement | null {
  const startedRef = useRef(false);
  useEffect(() => {
    if (startedRef.current) return;
    startedRef.current = true;
    try {
      init(props);
    } catch {}
    // Started once for the life of the document, exactly like the script tag.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return null;
}

/**
 * Explicit route-change listener for the App Router.
 *
 * ```tsx
 * "use client";
 * import { usePathname, useSearchParams } from "next/navigation";
 * import { useNextPageviews } from "@micaforge/sdk/next";
 *
 * useNextPageviews(usePathname(), useSearchParams()?.toString());
 * ```
 *
 * Use it when you have turned the core's SPA listener off (`spa: false`), or when you
 * want a pageview per search-param change.
 */
export function useNextPageviews(
  pathname: string | null | undefined,
  search?: string | null,
  props?: Props,
): void {
  const last = useRef<string | null>(null);
  useEffect(() => {
    if (pathname == null) return;
    const key = pathname + (search ? "?" + search : "");
    if (last.current === key) return;
    last.current = key;
    try {
      const t: TrackerApi | undefined = getTracker();
      if (t) {
        const w: any = typeof window != "undefined" ? window : null;
        t.pageview({
          url: w && w.location ? w.location.origin + key : key,
          props: props,
        });
      }
    } catch {}
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname, search]);
}

export { init, getTracker, track, pageview, identify, setTraits, setProps, optIn, optOut, flush } from "../index.js";
export type { InitConfig } from "../index.js";
export type { Props, TrackerApi } from "../types.js";
