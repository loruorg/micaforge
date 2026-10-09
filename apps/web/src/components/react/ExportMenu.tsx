/**
 * Export: take the data on screen away as a file.
 *
 * Two formats for two different jobs: a comma-separated table for anyone who
 * is going to do their own arithmetic on it, and a printable report for anyone
 * who has to hand the numbers to someone else. Both carry the EXACT envelope
 * that is on screen (the same date range, audience and filters), so what comes
 * out of the file is what the user was looking at when they asked.
 *
 * The links are real links. A download that only works when JavaScript agrees
 * to run is not a download.
 *
 * A third kind of file sits below them: the raw rows (`GET /api/export/raw`),
 * every event, error and web vital the site recorded in the date range, as a
 * ZIP of CSV or JSON files. That one takes only the site and the dates, and
 * says so, because a raw export that quietly dropped rows would not be raw.
 */
import { useMemo } from "react";
import * as RMenu from "@radix-ui/react-dropdown-menu";

import { api } from "../../lib/api";
import type { Envelope } from "../../lib/api";
import { useAppState } from "../../app/state";
import { useSite } from "../../app/providers";
import { IconDownload } from "./icons";

export interface ExportMenuProps {
  /**
   * Which table is exported, in the same `area:dimension` form the tables
   * already use ("pages:pathname", "sources:channel"). The server reads it to
   * decide which query to run.
   */
  report?: string;
  /** Overrides the envelope. Defaults to the dashboard's current settings. */
  env?: Envelope;
  label?: string;
  /** A short, honest word for what the file will contain. */
  subject?: string;
}

/** A filename someone can find again in a downloads folder six weeks later. */
function fileStem(report: string, domain: string | undefined, range: string): string {
  const site = (domain ?? "micaforge").replace(/^www\./, "").replace(/[^a-z0-9.-]/gi, "-");
  const what = report.replace(/[^a-z0-9]+/gi, "-");
  const when = range.replace(/\.\./g, "_to_").replace(/[^a-z0-9_-]/gi, "");
  return `${site}-${what}-${when}`;
}

export function ExportMenu({
  report = "overview",
  env: envProp,
  label = "Export",
  subject = "this report",
}: ExportMenuProps) {
  const state = useAppState();
  const { siteId, domain } = useSite();

  const env = useMemo<Envelope | null>(() => {
    if (envProp) return { ...envProp, report };
    if (siteId == null) return null;
    return { ...state.toEnvelope(siteId), report };
    // `toEnvelope` reads the live store, so every setting that changes the
    // data has to be a dependency of the URL that exports it.
  }, [envProp, report, siteId, state.range, state.audience, state.compare, state.filters, state]);

  const stem = fileStem(report, domain, state.range);
  const ready = env != null;

  /** The raw export's envelope: the site and the dates, nothing else. */
  const rawUrl = (format: "csv" | "json"): string | undefined => {
    if (!env) return undefined;
    const { site, range, start, end } = env;
    return api.exportUrl("/api/export/raw", { site, range, start, end, format });
  };
  const rawStem = fileStem("raw", domain, state.range);

  return (
    <RMenu.Root>
      <RMenu.Trigger asChild disabled={!ready}>
        <button
          type="button"
          className="mfs-key"
          disabled={!ready}
          aria-label={ready ? `Export ${subject}` : "Export is available once a site is open"}
        >
          <IconDownload size={14} />
          <span className="mfs-key__label">{label}</span>
        </button>
      </RMenu.Trigger>

      <RMenu.Portal>
        <RMenu.Content className="mfs-slip-panel mfs-menu" sideOffset={8} align="end" collisionPadding={12}>
          <p className="mfs-menu__note">
            These two files use the date range, audience and filters shown on screen.
          </p>

          <RMenu.Item asChild>
            <a
              className="mfs-menu__item"
              href={ready ? api.exportUrl("/api/stats/export.csv", env) : undefined}
              download={`${stem}.csv`}
              target="_blank"
              rel="noopener noreferrer"
            >
              <span className="mfs-menu__item-name">Data table</span>
              <span className="mfs-menu__item-note">
                Comma-separated, one row per line. <span className="mf-data">.csv</span>
              </span>
            </a>
          </RMenu.Item>

          <RMenu.Item asChild>
            <a
              className="mfs-menu__item"
              href={ready ? api.exportUrl("/api/stats/export.pdf", env) : undefined}
              download={`${stem}.pdf`}
              target="_blank"
              rel="noopener noreferrer"
            >
              <span className="mfs-menu__item-name">Printable report</span>
              <span className="mfs-menu__item-note">
                Charts and tables, laid out to print. <span className="mf-data">.pdf</span>
              </span>
            </a>
          </RMenu.Item>

          <p className="mfs-menu__note">
            Raw data: every event, error and web vital recorded in this date range, one row per
            hit. Filters and the audience do not apply.
          </p>

          {(["csv", "json"] as const).map((format) => (
            <RMenu.Item asChild key={format}>
              <a
                className="mfs-menu__item"
                href={rawUrl(format)}
                download={`${rawStem}-${format}.zip`}
                target="_blank"
                rel="noopener noreferrer"
              >
                <span className="mfs-menu__item-name">
                  Raw data, {format === "csv" ? "CSV" : "JSON"}
                </span>
                <span className="mfs-menu__item-note">
                  {format === "csv" ? "One CSV file per table" : "One JSON object per line"}, in a
                  ZIP. <span className="mf-data">.zip</span>
                </span>
              </a>
            </RMenu.Item>
          ))}
        </RMenu.Content>
      </RMenu.Portal>
    </RMenu.Root>
  );
}

export default ExportMenu;
