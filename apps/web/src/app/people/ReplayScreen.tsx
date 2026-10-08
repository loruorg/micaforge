/**
 * PEOPLE: recorded sessions.
 *
 * A replay is the heaviest, most sensitive thing this product stores, so the
 * list treats it that way: every row carries what it costs on disk and a
 * button that really deletes it, and the panel says plainly when recordings expire.
 */
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router";
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../../lib/api";
import { bytes as fmtBytes, duration, num } from "../../lib/format";
import { Button, Empty, ErrorState, Panel, PanelHead, Sheet } from "../../components/react/ui";
import type { Column } from "../../components/react/ui";
import { useAppState } from "../state";
import { useSite } from "../providers";
import {
  Hand,
  Pager,
  PEOPLE_PAGE_SIZE,
  apiMessage,
  asRows,
  countryName,
  dayAt,
  deviceLabel,
  statsKey,
  stampAt,
  totalOf,
  useBase,
  useEnvelope,
} from "./SessionsScreen";
import { ErasureDialog, IconErase } from "./VisitorDetail";
import type { ErasureLine } from "./VisitorDetail";
import "../../styles/app/people.css";

export interface ReplayRow {
  session_id: string;
  started: number | string;
  ended?: number | string;
  duration_s?: number;
  pageviews?: number;
  events?: number;
  bytes?: number;
  device_type?: string;
  browser?: string;
  os?: string;
  country?: string;
  identified_id?: string;
  is_agent?: boolean | number;
  agent_id?: string;
  agent_name?: string;
  expires_at?: number | string;
}

export function ReplayScreen() {
  const navigate = useNavigate();
  const base = useBase();
  const qc = useQueryClient();
  const { siteId, timezone } = useSite();
  const { range, audience, filters } = useAppState();
  const [page, setPage] = useState(0);
  const [pending, setPending] = useState<ReplayRow | null>(null);

  const filterKey = JSON.stringify(filters ?? []);
  useEffect(() => {
    setPage(0);
  }, [range, audience, filterKey]);

  const envelope = useEnvelope({ limit: PEOPLE_PAGE_SIZE, page });

  const q = useQuery({
    queryKey: statsKey("replay", siteId, range, audience, envelope.filters, page),
    queryFn: () => api.get<unknown>("/api/replay", envelope),
    enabled: siteId != null,
    placeholderData: keepPreviousData,
  });

  const erase = useMutation({
    mutationFn: (sessionId: string) => api.del<unknown>(`/api/replay/${encodeURIComponent(sessionId)}`),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["stats", "replay"] });
      setPending(null);
    },
  });

  const rows = useMemo(() => asRows<ReplayRow>(q.data?.data, "recordings"), [q.data]);
  const total = totalOf(q.data?.data);
  const held = rows.reduce((n, r) => n + (r.bytes ?? 0), 0);

  const columns: Column<ReplayRow>[] = [
    {
      key: "started",
      head: "Recorded",
      width: "15ch",
      render: (r) => (
        <Link
          className="mf-data"
          to={`${base}/replay/${encodeURIComponent(r.session_id)}`}
          onClick={(e) => e.stopPropagation()}
        >
          {dayAt(r.started, timezone)}
        </Link>
      ),
    },
    {
      key: "duration",
      head: "Length",
      numeric: true,
      width: "10ch",
      render: (r) => <span className="mf-data">{duration(r.duration_s)}</span>,
    },
    {
      key: "pages",
      head: "Pages",
      numeric: true,
      width: "8ch",
      render: (r) => <span className="mf-data">{num(r.pageviews ?? 0)}</span>,
    },
    {
      key: "device",
      head: "Device",
      width: "22ch",
      render: (r) => (
        <span>
          {deviceLabel(r.device_type)}
          {r.browser ? <span className="mf-p-dim"> · {r.browser}</span> : null}
          {r.os ? <span className="mf-p-dim"> · {r.os}</span> : null}
        </span>
      ),
    },
    { key: "country", head: "Country", width: "14ch", render: (r) => <span>{countryName(r.country)}</span> },
    {
      key: "size",
      head: "On disk",
      numeric: true,
      width: "10ch",
      render: (r) => <span className="mf-data">{r.bytes != null ? fmtBytes(r.bytes) : "—"}</span>,
    },
    {
      key: "hand",
      head: "Type",
      width: "15ch",
      render: (r) => <Hand session={r} />,
    },
    {
      key: "erase",
      head: <span className="mf-p-sr">Erase</span>,
      width: "4ch",
      render: (r) => (
        <Button
          tone="quiet"
          size="sm"
          className="mf-p-rowkey"
          icon={<IconErase size={14} />}
          aria-label={`Erase the recording from ${stampAt(r.started, timezone)}`}
          onClick={(e) => {
            e.stopPropagation();
            setPending(r);
          }}
        />
      ),
    },
  ];

  const eraseLines: ErasureLine[] = pending
    ? [
        { label: "Recorded", value: stampAt(pending.started, timezone) },
        { label: "Length", value: duration(pending.duration_s) },
        ...(pending.pageviews != null ? [{ label: "Pages", value: num(pending.pageviews) }] : []),
        ...(pending.events != null ? [{ label: "Recorded events", value: num(pending.events) }] : []),
        ...(pending.bytes != null ? [{ label: "On disk", value: fmtBytes(pending.bytes) }] : []),
      ]
    : [];

  return (
    <div className="mf-p-stack">
      <Panel>
        <PanelHead
          title="Replay"
          note={
            q.isSuccess && rows.length
              ? `${total != null ? `${num(total)} recorded ${total === 1 ? "session" : "sessions"}, ` : ""}${fmtBytes(held)} on this page. Recordings are deleted when this site's replay retention runs out.`
              : "Recordings are stored on your own server and deleted when this site's replay retention runs out."
          }
        />

        {q.isError ? (
          <ErrorState detail={apiMessage(q.error)} onRetry={() => q.refetch()} />
        ) : (
          <>
            <Sheet
              rows={rows}
              columns={columns}
              loading={q.isPending}
              getKey={(r, i) => r.session_id || String(i)}
              onRowClick={(r) => navigate(`${base}/replay/${encodeURIComponent(r.session_id)}`)}
              empty={
                <Empty
                  title="Nothing has been recorded yet"
                  body="Replay is off until you switch it on for this site under Settings, and it only records the sessions you sample. Recorded sessions appear here within a minute of ending."
                />
              }
            />
            {rows.length || page > 0 ? (
              <Pager page={page} onPage={setPage} count={rows.length} total={total} noun="recordings" />
            ) : null}
          </>
        )}
      </Panel>

      <ErasureDialog
        open={pending != null}
        title="Erase this recording?"
        lead={<>The recording below is deleted from this site&rsquo;s replay store. This cannot be undone.</>}
        lines={eraseLines}
        note="The session itself stays in your data. Only the replay is removed."
        confirmLabel="Erase the recording"
        busy={erase.isPending}
        error={erase.isError ? apiMessage(erase.error) : null}
        onDismiss={() => {
          if (!erase.isPending) setPending(null);
        }}
        onConfirm={() => pending && erase.mutate(pending.session_id)}
      />
    </div>
  );
}

export default ReplayScreen;
