/**
 * The station's power and plumbing: one query client, one site, one identity,
 * and the one place the whole dashboard's styles are pulled in.
 *
 * `useSite()` and `useAppState()` are the two hooks every screen in this
 * application was written against, so their shapes are load-bearing. What
 * `useSite()` returns is deliberately forgiving: `siteId` is `undefined` until
 * the site is known rather than a guessed zero, because a read fired against
 * site 0 is a read against somebody else's data. Every screen gates on
 * `siteId != null` for exactly that reason.
 */
import type { ReactNode } from "react";
import { createContext, useContext, useEffect, useMemo } from "react";
import { useLocation } from "react-router";
import { QueryClient, QueryClientProvider, useQuery, useQueryClient } from "@tanstack/react-query";
import { Toaster } from "sonner";

import { ApiError, api } from "../lib/api";
import type { SiteSummary } from "../lib/api";
import { siteKeyFromPath, useAppState } from "./state";
import "../styles/app/index.css";

/* ── the query client ────────────────────────────────────────────────────── */

/**
 * Analytics reads are expensive and they are not urgent: a bucketed 90-day
 * timeseries does not change in the two seconds it takes to click back to a
 * tab, so refetching on window focus would spend the operator's ClickHouse
 * budget on nothing. Screens that genuinely are live (the realtime pen, the
 * import progress) set their own `refetchInterval` and override this.
 *
 * One retry, not three: a query that failed because the filter was malformed
 * will fail identically three times, and the operator waits three times as long
 * to be told so.
 */
export function makeQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        gcTime: 5 * 60_000,
        retry: (failureCount, error) => {
          // Nothing is gained by asking again for something we are not allowed
          // to see, or for a query the server has already called malformed.
          if (error instanceof ApiError && error.status >= 400 && error.status < 500) return false;
          return failureCount < 1;
        },
        refetchOnWindowFocus: false,
        refetchOnReconnect: true,
      },
      mutations: { retry: false },
    },
  });
}

/* ── who is signed in ────────────────────────────────────────────────────── */

export interface Account {
  id: string;
  email: string;
  name: string;
  avatar_url: string | null;
  email_verified_at: string | null;
  totp_enabled?: boolean;
  created_at?: string;
}

export interface AuthValue {
  user: Account | null;
  isLoading: boolean;
  /** True once the server has answered, either way. */
  isSettled: boolean;
  /** The server said, in as many words, that this reader is not signed in. */
  isSignedOut: boolean;
  /** Set only when the answer never arrived: a network or server failure. */
  error: Error | null;
  signOut: () => Promise<void>;
  refresh: () => void;
}

const AuthContext = createContext<AuthValue | null>(null);

/** The account query key, shared with the account settings screen. */
export const ACCOUNT_KEY = ["account", "me"] as const;

export function AuthProvider({ children }: { children: ReactNode }) {
  const qc = useQueryClient();

  const me = useQuery({
    queryKey: ACCOUNT_KEY,
    // The answer is `{ user, organizations, sites, instance }`; the account is `user`.
    queryFn: () => api.get<{ user: Account }>("/api/auth/me").then((r) => r.data.user),
    staleTime: 5 * 60_000,
    retry: false,
  });

  const value = useMemo<AuthValue>(() => {
    const err = me.error;
    // A 401 is not a failure. It is the server answering the question that was
    // asked, and the answer is "nobody". Only an unreachable or broken server
    // counts as an error the operator needs to see.
    const signedOut =
      err instanceof ApiError && (err.status === 401 || err.status === 403);
    return {
      user: me.data ?? null,
      isLoading: me.isPending,
      isSettled: !me.isPending,
      isSignedOut: signedOut,
      error: !err || signedOut ? null : err instanceof Error ? err : new Error(String(err)),
      signOut: async () => {
        try {
          await api.post("/api/auth/logout");
        } catch {
          /* The cookie may already be gone. Clearing the client either way is
             the honest outcome of asking to sign out. */
        }
        qc.clear();
        window.location.href = "/app/login";
      },
      refresh: () => void qc.invalidateQueries({ queryKey: ACCOUNT_KEY }),
    };
  }, [me.data, me.error, me.isPending, qc]);

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthValue {
  const v = useContext(AuthContext);
  if (!v) throw new Error("useAuth was called outside AuthProvider.");
  return v;
}

/* ── which site is being read ────────────────────────────────────────────── */

export interface SiteValue {
  site: SiteSummary | null;
  /**
   * The numeric `site_id` every analytics read is keyed on. `undefined` while
   * it is still unknown, never a placeholder.
   */
  siteId: number | undefined;
  domain: string | undefined;
  timezone: string | undefined;
  isLoading: boolean;
  error: Error | null;
  /**
   * The address names a site this account cannot read: the list has loaded and
   * nothing in it answers to the key. Screens would otherwise wait forever for
   * a site id that is never coming, so the shell says so instead.
   */
  notFound: boolean;
  /** The site key as written in the address, for that message. */
  requested: string | null;
}

const SiteContext = createContext<SiteValue | null>(null);

/** The site list key, shared with the site chooser so both read one answer. */
export const SITES_KEY = ["sites"] as const;

export function SiteProvider({ children }: { children: ReactNode }) {
  // Read from the location rather than from route params: this provider sits
  // ABOVE the route table so that every screen can call `useSite()`, and route
  // parameters only exist inside a matched route. One parser, shared with the
  // store, keeps the two readings of "which site is this" identical.
  const { pathname } = useLocation();
  const param = useMemo(() => siteKeyFromPath(pathname), [pathname]);
  const bindSite = useAppState((s) => s.bindSite);

  const sites = useQuery({
    queryKey: SITES_KEY,
    queryFn: () => api.get<SiteSummary[]>("/api/sites").then((r) => r.data),
    staleTime: 60_000,
  });

  // The interval, audience and comparison belong to the site being read, so
  // they follow the address bar. Binding the site already bound is a no-op, so
  // this cannot loop.
  useEffect(() => {
    bindSite(param);
  }, [param, bindSite]);

  const value = useMemo<SiteValue>(() => {
    const list = sites.data ?? [];
    // The rail links by numeric site_id, but a hand-typed URL may carry the
    // UUID or the domain, and all three should land on the same record.
    const match =
      param == null
        ? null
        : (list.find((s) => String(s.site_id) === param) ??
          list.find((s) => s.id === param) ??
          list.find((s) => s.domain === param) ??
          null);

    // Before the site list lands, a numeric parameter is already a usable
    // site_id. That is what lets the record start reading on first paint
    // instead of waiting on a second round trip for its own domain name.
    const numeric = param != null && /^\d+$/.test(param) ? Number(param) : undefined;
    const siteId = match?.site_id ?? numeric;

    const err = sites.error;
    return {
      site: match,
      siteId,
      domain: match?.domain,
      timezone: match?.timezone,
      isLoading: param != null && sites.isPending && siteId == null,
      error: err ? (err instanceof Error ? err : new Error(String(err))) : null,
      notFound: param != null && sites.isSuccess && match == null,
      requested: param,
    };
  }, [param, sites.data, sites.error, sites.isPending, sites.isSuccess]);

  return <SiteContext.Provider value={value}>{children}</SiteContext.Provider>;
}

export function useSite(): SiteValue {
  const v = useContext(SiteContext);
  if (!v) throw new Error("useSite was called outside SiteProvider.");
  return v;
}

/* ── the notice slip ─────────────────────────────────────────────────────── */

/**
 * A toast is a white panel like every other surface, not a floating glass card.
 * Its colours, edge and soft shadow come from the tokens, set in shell.css.
 */
function NoticeSlips() {
  return (
    <Toaster
      position="bottom-right"
      gap={8}
      visibleToasts={4}
      offset={16}
      toastOptions={{
        className: "mfs-slip",
        classNames: {
          title: "mfs-slip__title",
          description: "mfs-slip__body",
          actionButton: "mfs-slip__act",
          cancelButton: "mfs-slip__act",
          closeButton: "mfs-slip__close",
        },
      }}
    />
  );
}

/* ── everything, in the order it has to be in ────────────────────────────── */

/**
 * `SiteProvider` reads a route parameter, so it has to sit inside the router,
 * not around it. `main.tsx` mounts the router; this sits under it.
 */
export function Providers({
  children,
  client,
}: {
  children: ReactNode;
  client?: QueryClient;
}) {
  const qc = useMemo(() => client ?? makeQueryClient(), [client]);
  return (
    <QueryClientProvider client={qc}>
      <AuthProvider>
        <SiteProvider>
          {children}
          <NoticeSlips />
        </SiteProvider>
      </AuthProvider>
    </QueryClientProvider>
  );
}

export default Providers;
