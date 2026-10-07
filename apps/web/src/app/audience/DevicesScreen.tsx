/**
 * Devices: what the record was read on.
 *
 * Six readings that belong together, so they are drawn as one bench rather
 * than six cards: a single ruled surface divided into channel cells, the way a
 * recorder's input bank is divided. The cells share the panel's edges and sit
 * on one grid, so every title, every column head and every first row lands on
 * the same line across the row of cells no matter how long a browser name runs.
 */
import { useMemo } from "react";
import type { ReactNode } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { Empty, ErrorState, Panel, PanelHead, Readout, Sheet } from "../../components/react/ui";
import type { Channel, Column } from "../../components/react/ui";
import type { Envelope, Filter } from "../../lib/api";
import { pct } from "../../lib/format";
import { againstLeader } from "../display";
import { useAppState } from "../state";
import {
  AudienceBar,
  Caveat,
  ExportKey,
  FilterTrail,
  ReadoutWait,
  count,
  errorDetail,
  n,
  useAudienceQuery,
  useEnvelope,
} from "./PagesScreen";
import type { AudienceRow } from "./PagesScreen";
import "../../styles/app/audience.css";

/** How many rows a cell shows before it says it is showing the top of a list. */
const CELL_ROWS = 7;

const DEVICE_LABELS: Record<string, string> = {
  desktop: "Desktop",
  mobile: "Mobile",
  tablet: "Tablet",
  tv: "Television",
  console: "Console",
  wearable: "Wearable",
};

type CellQuery = UseQueryResult<{ rows: AudienceRow[]; total: number | undefined }>;

function BenchCell({
  title,
  query,
  columns,
  empty,
  onPick,
}: {
  title: string;
  query: CellQuery;
  columns: Column<AudienceRow>[];
  empty: ReactNode;
  onPick: (row: AudienceRow) => void;
}) {
  const rows = query.data?.rows ?? [];
  const capped = rows.length >= CELL_ROWS;
  /* The ink behind a row is drawn against the leading row of this cell. Drawn
     against the whole site instead, a cell of six similar shares is six
     identical stubs. */
  const inked = columns.map((c) => (c.share ? { ...c, share: againstLeader(rows, c.share) } : c));
  return (
    <section className="mfa-bench__cell">
      <div className="mfa-bench__head">
        <h3 className="mfa-bench__title">{title}</h3>
        <span className="mfa-bench__count">
          {query.isError
            ? "no reading"
            : query.isLoading
              ? "reading…"
              : capped
                ? `top ${CELL_ROWS}`
                : `${rows.length} seen`}
        </span>
      </div>
      {query.isError ? (
        /* A cell is a small space, and when the whole panel fails the reason is
           already stated once at the top. Inside a cell the failure is named
           and offered again, without repeating the diagnosis six times. */
        <ErrorState title="This reading did not come back" onRetry={() => void query.refetch()} />
      ) : (
        <Sheet<AudienceRow>
          rows={rows}
          columns={inked}
          loading={query.isLoading}
          getKey={(r, i) => `${r.key}-${i}`}
          onRowClick={onPick}
          empty={empty}
        />
      )}
    </section>
  );
}

export function DevicesScreen() {
  const { addFilter, audience } = useAppState();
  const machines = audience === "agent";
  const { env, ready } = useEnvelope();

  const envs = useMemo(() => {
    const cell = (dimension: string, limit = CELL_ROWS): Envelope => ({
      ...env,
      dimension,
      limit,
    });
    return {
      browser: cell("browser"),
      browser_version: cell("browser_version"),
      os: cell("os"),
      os_version: cell("os_version"),
      // The device split feeds the readout, so it is never truncated.
      device_type: cell("device_type", 12),
      screen_class: cell("screen_class"),
    };
  }, [env]);

  const D = "/api/stats/devices";
  const browsers = useAudienceQuery<AudienceRow>("devices:browser", D, envs.browser, ready);
  const browserVersions = useAudienceQuery<AudienceRow>(
    "devices:browser_version",
    D,
    envs.browser_version,
    ready,
  );
  const systems = useAudienceQuery<AudienceRow>("devices:os", D, envs.os, ready);
  const systemVersions = useAudienceQuery<AudienceRow>(
    "devices:os_version",
    D,
    envs.os_version,
    ready,
  );
  const deviceTypes = useAudienceQuery<AudienceRow>(
    "devices:device_type",
    D,
    envs.device_type,
    ready,
  );
  const screens = useAudienceQuery<AudienceRow>(
    "devices:screen_class",
    D,
    envs.screen_class,
    ready,
  );

  const cells = [browsers, browserVersions, systems, systemVersions, deviceTypes, screens];
  const allFailed = cells.every((q) => q.isError);
  const retryAll = () => cells.forEach((q) => void q.refetch());

  /* Device type is a closed set, so these four figures are a complete split of
     the located record rather than a share of a top-N list. */
  const split = useMemo(() => {
    const rows = deviceTypes.data?.rows ?? [];
    let total = 0;
    const by: Record<string, number> = {};
    for (const r of rows) {
      const v = n(r, "visitors") ?? n(r, "pageviews") ?? 0;
      by[r.key] = (by[r.key] ?? 0) + v;
      total += v;
    }
    const named = (by.desktop ?? 0) + (by.mobile ?? 0);
    return {
      total,
      desktop: by.desktop ?? 0,
      mobile: by.mobile ?? 0,
      other: Math.max(0, total - named),
    };
  }, [deviceTypes.data]);

  const readout = useMemo<Channel[]>(
    () => [
      {
        key: "total",
        label: machines ? "Clients" : "Visitors",
        value: count(split.total),
        note: "across all device types",
      },
      {
        key: "desktop",
        label: "On desktop",
        value: split.total > 0 ? pct(split.desktop / split.total, 1) : "—",
      },
      {
        key: "mobile",
        label: "On mobile",
        value: split.total > 0 ? pct(split.mobile / split.total, 1) : "—",
      },
      {
        key: "other",
        label: "Tablet and other",
        value: split.total > 0 ? pct(split.other / split.total, 1) : "—",
      },
    ],
    [split, machines],
  );

  /** A cell's two measured columns. Prose gets the UI face; a version string
      and a pixel bucket are genuine data and keep the data face. */
  const cols = (
    head: string,
    face: "ui" | "data",
    label?: (k: string) => string,
  ): Column<AudienceRow>[] => [
      {
        key: "value",
        head,
        width: "52%",
        share: (r: AudienceRow) => n(r, "share") ?? undefined,
        render: (r: AudienceRow) => {
          const text = r.label || (label ? label(r.key) : r.key) || "Unknown";
          return face === "ui" ? (
            <span className="mfa-name" title={text}>
              {text}
            </span>
          ) : (
            <span title={text}>{text}</span>
          );
        },
      },
      {
        key: "visitors",
        head: machines ? "Clients" : "Visitors",
        numeric: true,
        render: (r: AudienceRow) => count(n(r, "visitors") ?? n(r, "pageviews")),
      },
      {
        key: "share",
        head: "Share",
        numeric: true,
        render: (r: AudienceRow) => (n(r, "share") == null ? "—" : pct(n(r, "share")!, 0)),
      },
    ];

  const pick = (dimension: string) => (r: AudienceRow) =>
    addFilter({ dimension, op: "is", value: r.key } as Filter);

  const nothing = (what: string, why: string) => <Empty title={what} body={why} />;

  return (
    /* Which pen these sheets are written in: the ink behind a row is the trace
       of the audience being read, never the machine pen on a human table. */
    <div className="mfa-screen" data-trace={machines ? "agent" : "human"}>
      <Panel ruled feed>
        <PanelHead
          title="Devices"
          note="Browser, system and screen, read from the request rather than from anything stored on the reader."
          actions={<AudienceBar extra={<ExportKey report="devices:browser" env={env} />} />}
        />

        {allFailed ? (
          /* Every channel on the bench failed, so this is one failure with one
             cause. It is reported once, at the top, rather than six times. */
          <ErrorState
            title="The device readings did not come back"
            detail={errorDetail(deviceTypes.error)}
            onRetry={retryAll}
          />
        ) : deviceTypes.isError ? (
          <ErrorState
            title="The device split did not come back"
            detail={errorDetail(deviceTypes.error)}
            onRetry={() => void deviceTypes.refetch()}
          />
        ) : deviceTypes.isLoading ? (
          <ReadoutWait channels={4} />
        ) : (
          <Readout channels={readout} />
        )}

        <FilterTrail />

        {machines ? (
          <Caveat>
            A machine client reports a user-agent string, not a browser profile. Screen and
            viewport are blank for it by definition, and what is named here is whatever that
            string declares. Read Agent traffic for the classified, verified account.
          </Caveat>
        ) : null}

        {allFailed ? null : (
          <div className="mfa-bench">
            <BenchCell
              title="Browser"
              query={browsers}
              columns={cols("Browser", "ui")}
              onPick={pick("browser")}
              empty={nothing(
                "No browsers recorded",
                "A browser is named from the request's user-agent on the first hit.",
              )}
            />
            <BenchCell
              title="Browser version"
              query={browserVersions}
              columns={cols("Version", "data")}
              onPick={pick("browser_version")}
              empty={nothing(
                "No versions recorded",
                "Versions appear alongside the browsers above once traffic arrives.",
              )}
            />
            <BenchCell
              title="Operating system"
              query={systems}
              columns={cols("System", "ui")}
              onPick={pick("os")}
              empty={nothing(
                "No systems recorded",
                "The system is named from the same user-agent as the browser.",
              )}
            />
            <BenchCell
              title="System version"
              query={systemVersions}
              columns={cols("Version", "data")}
              onPick={pick("os_version")}
              empty={nothing(
                "No system versions recorded",
                "Several systems report no version at all; those hits are counted under the system above.",
              )}
            />
            <BenchCell
              title="Device type"
              query={deviceTypes}
              columns={cols("Type", "ui", (k) => DEVICE_LABELS[k] ?? k)}
              onPick={pick("device_type")}
              empty={nothing(
                "No device types recorded",
                "Every hit resolves to one of desktop, mobile, tablet, television, console or wearable.",
              )}
            />
            <BenchCell
              title="Screen class"
              query={screens}
              columns={cols("Class", "data")}
              onPick={pick("screen_class")}
              empty={nothing(
                machines ? "Machines report no screen" : "No screen classes recorded",
                machines
                  ? "A crawler has no viewport to report, so this cell stays empty for the machine trace."
                  : "Screen width is bucketed at ingest; the exact pixel size is never stored.",
              )}
            />
          </div>
        )}

        {allFailed ? null : (
          <div className="mfa-foot">
            <span>Click any row to clip the whole record to it.</span>
            <span className="mf-data">6 channels</span>
          </div>
        )}
      </Panel>
    </div>
  );
}

export default DevicesScreen;
