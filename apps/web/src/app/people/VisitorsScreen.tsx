/**
 * PEOPLE: visitors.
 *
 * This screen is empty by design until a site chooses to name someone.
 * Micaforge measures traffic, not individuals: the visitor hash rotates daily
 * and is not a person. A row appears here only where the site itself called
 * `identify()`, so the empty state is not an apology; it is the product
 * explaining a deliberate constraint, with the one line of code that changes it.
 *
 * The list endpoint also returns visitors that were never named (an empty
 * `identified_id` beside the daily hash). Those rows are shown as anonymous and
 * do not open a profile: the profile reads and erases by the named id, so a
 * link to it from a hash would open an empty page.
 *
 * The search finds a person by their id or by anything in their traits, an
 * email address say (`GET /api/stats/profiles?q=`), across the site's whole
 * retained history rather than the selected period: someone looking for a
 * customer does not know which week they last visited.
 */
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { api } from "../../lib/api";
import type { Envelope, Filter } from "../../lib/api";
import { ago, num } from "../../lib/format";
import { Button, CopyField, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { IconSearch } from "../../components/react/icons";
import { useAppState } from "../state";
import { useSite } from "../providers";
import {
  Pager,
  PEOPLE_PAGE_SIZE,
  apiMessage,
  asRows,
  countryName,
  stampAt,
  statsKey,
  toMs,
  totalOf,
  useBase,
  useEnvelope,
} from "./SessionsScreen";
import "../../styles/app/people.css";
import "../../styles/app/profiles.css";

/** One search result from `GET /api/stats/profiles?q=`. */
interface ProfileHit {
  identified_id: string;
  traits: Record<string, unknown>;
  last_seen: string;
  visitors: number;
}

/** The fewest characters the server searches for. */
const MIN_SEARCH = 2;

export interface VisitorRow {
  identified_id: string;
  visitor_id?: string;
  first_seen?: number | string;
  last_seen?: number | string;
  sessions?: number;
  pageviews?: number;
  events?: number;
  goals?: number;
  replays?: number;
  country?: string;
  revenue?: number | null;
  currency?: string;
  traits?: Record<string, unknown> | null;
}

/** The identify() call, exactly as a site would write it. */
export const IDENTIFY_LINE = 'micaforge.identify("user_1042", { plan: "pro" })';

/** Two traits at most, as running text. Metadata is hierarchy, not chips. */
export function traitSummary(traits: Record<string, unknown> | null | undefined): string {
  if (!traits) return "";
  const entries = Object.entries(traits).filter(([, v]) => v != null && v !== "");
  if (!entries.length) return "";
  return entries
    .slice(0, 2)
    .map(([k, v]) => `${k} ${typeof v === "object" ? "…" : String(v)}`)
    .join(" · ");
}

export function IdentifyEmpty({ searched }: { searched?: string }) {
  if (searched) {
    return (
      <Empty
        title="Nobody matches that"
        body={<>No person your site identified has an id or a trait containing “{searched}”. Clear the search to see every visitor.</>}
      />
    );
  }
  return (
    <Empty
      title="No visitor has been named yet"
      body="Micaforge counts traffic, not people: the visitor hash rotates every day and never leaves the server. A visitor appears here only once your site names one, after a sign-in say, and their sessions, goals and traits collect against that id from then on."
      action={
        <div className="mf-p-empty-code">
          <CopyField label="Call this once you know who the visitor is" value={IDENTIFY_LINE} />
        </div>
      }
    />
  );
}

export function VisitorsScreen() {
  const navigate = useNavigate();
  const base = useBase();
  const { siteId, timezone } = useSite();
  const { range, audience, filters } = useAppState();
  const [page, setPage] = useState(0);
  const [find, setFind] = useState("");
  const [findApplied, setFindApplied] = useState("");

  const filterKey = JSON.stringify(filters ?? []);
  useEffect(() => {
    setPage(0);
  }, [range, audience, filterKey, findApplied]);

  const env = useEnvelope({ limit: PEOPLE_PAGE_SIZE, page });
  const envelope: Envelope = { ...env, filters: (env.filters as Filter[]) ?? [] };
  const searching = findApplied.length >= MIN_SEARCH;

  const q = useQuery({
    queryKey: statsKey("visitors", siteId, range, audience, envelope.filters, page),
    queryFn: () => api.get<unknown>("/api/stats/visitors", envelope),
    enabled: siteId != null && !searching,
    placeholderData: keepPreviousData,
  });

  const found = useQuery({
    queryKey: ["stats", "profiles", siteId, findApplied],
    queryFn: () =>
      api
        .get<{ rows: ProfileHit[] }>("/api/stats/profiles", {
          site: siteId as number,
          range: "all",
          q: findApplied,
        })
        .then((r) => r.data?.rows ?? []),
    enabled: siteId != null && searching,
  });

  const rows = useMemo(() => asRows<VisitorRow>(q.data?.data, "visitors"), [q.data]);
  const total = totalOf(q.data?.data);

  const hitColumns: Column<ProfileHit>[] = [
    {
      key: "id",
      head: "Person",
      render: (r) => (
        <Link
          className="mf-data"
          to={`${base}/visitors/${encodeURIComponent(r.identified_id)}`}
          onClick={(e) => e.stopPropagation()}
        >
          {r.identified_id}
        </Link>
      ),
    },
    {
      key: "traits",
      head: "Traits",
      render: (r) => {
        const s = traitSummary(r.traits);
        return <span className="mf-p-dim">{s || "—"}</span>;
      },
    },
    {
      key: "last",
      head: "Last identified",
      width: "16ch",
      render: (r) => <span title={stampAt(r.last_seen, timezone)}>{ago(toMs(r.last_seen))}</span>,
    },
    {
      key: "days",
      head: "Days linked",
      numeric: true,
      width: "11ch",
      render: (r) => <span className="mf-data">{num(r.visitors)}</span>,
    },
  ];

  const columns: Column<VisitorRow>[] = [
    {
      key: "id",
      head: "Visitor",
      render: (r) =>
        r.identified_id ? (
          <Link
            className="mf-data"
            to={`${base}/visitors/${encodeURIComponent(r.identified_id)}`}
            onClick={(e) => e.stopPropagation()}
          >
            {r.identified_id}
          </Link>
        ) : (
          <span className="mf-p-dim" title={r.visitor_id ? `Daily hash ${r.visitor_id}` : undefined}>
            Anonymous
            {r.visitor_id ? <span className="mf-data"> · {r.visitor_id.slice(0, 8)}</span> : null}
          </span>
        ),
    },
    {
      key: "traits",
      head: "Traits",
      render: (r) => {
        const s = traitSummary(r.traits);
        return s ? <span className="mf-p-dim">{s}</span> : <span className="mf-p-dim">—</span>;
      },
    },
    {
      key: "first",
      head: "First seen",
      width: "16ch",
      render: (r) => (
        <span title={stampAt(r.first_seen, timezone)}>{ago(toMs(r.first_seen))}</span>
      ),
    },
    {
      key: "last",
      head: "Last seen",
      width: "16ch",
      render: (r) => (
        <span title={stampAt(r.last_seen, timezone)}>{ago(toMs(r.last_seen))}</span>
      ),
    },
    {
      key: "sessions",
      head: "Sessions",
      numeric: true,
      width: "10ch",
      render: (r) => <span className="mf-data">{num(r.sessions ?? 0)}</span>,
    },
    {
      key: "pageviews",
      head: "Pageviews",
      numeric: true,
      width: "11ch",
      render: (r) => <span className="mf-data">{num(r.pageviews ?? 0)}</span>,
    },
    {
      key: "country",
      head: "Country",
      width: "14ch",
      render: (r) => <span>{countryName(r.country)}</span>,
    },
  ];

  return (
    <div className="mf-p-stack">
      <Panel>
        <PanelHead
          title="Visitors"
          note={
            total != null
              ? `${num(total)} ${total === 1 ? "visitor" : "visitors"}. A visitor gets a name only where your site called identify().`
              : "Only the ids your own site passed to identify() appear here. Everything else stays an anonymous, daily-rotating hash."
          }
          actions={
            <>
              <form
                className="mf-p-find"
                onSubmit={(e) => {
                  e.preventDefault();
                  setFindApplied(find.trim());
                }}
              >
                <IconSearch size={14} />
                <input
                  type="search"
                  value={find}
                  placeholder="Email, id or trait…"
                  aria-label="Find a person by the id or any trait your site passed to identify() or setTraits(), such as an email address"
                  onChange={(e) => setFind(e.target.value)}
                  onBlur={() => setFindApplied(find.trim())}
                />
              </form>
              {findApplied ? (
                <Button
                  size="sm"
                  onClick={() => {
                    setFind("");
                    setFindApplied("");
                  }}
                >
                  Clear search
                </Button>
              ) : null}
            </>
          }
        />

        {findApplied && !searching ? (
          <p className="mf-pr-hint">Type at least {MIN_SEARCH} characters to search.</p>
        ) : null}

        {searching ? (
          found.isError ? (
            <ErrorState detail={apiMessage(found.error)} onRetry={() => found.refetch()} />
          ) : (
            <Sheet
              rows={found.data ?? []}
              columns={hitColumns}
              loading={found.isPending}
              getKey={(r) => r.identified_id}
              onRowClick={(r) => navigate(`${base}/visitors/${encodeURIComponent(r.identified_id)}`)}
              empty={<IdentifyEmpty searched={findApplied} />}
            />
          )
        ) : q.isError ? (
          <ErrorState detail={apiMessage(q.error)} onRetry={() => q.refetch()} />
        ) : (
          <>
            <Sheet
              rows={rows}
              columns={columns}
              loading={q.isPending}
              getKey={(r, i) => r.identified_id || String(i)}
              onRowClick={(r) => {
                if (r.identified_id) navigate(`${base}/visitors/${encodeURIComponent(r.identified_id)}`);
              }}
              empty={<IdentifyEmpty searched={findApplied} />}
            />
            {rows.length || page > 0 ? (
              <Pager page={page} onPage={setPage} count={rows.length} total={total} noun="visitors" />
            ) : null}
          </>
        )}
      </Panel>
    </div>
  );
}

export default VisitorsScreen;
