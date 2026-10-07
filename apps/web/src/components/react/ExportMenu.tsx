/**
 * Taking the record away with you.
 *
 * Two ways off the instrument, and they are genuinely different jobs rather
 * than two buttons doing the same thing: a comma-separated table for anyone who
 * is going to do their own arithmetic on it, and a printable report for anyone
 * who has to hand the reading to someone else. Both carry the EXACT envelope
 * that is on screen (the same interval, audience and conditions), so what
 * comes out of the file is what the operator was looking at when they asked.
 *
 * The links are real links. A download that only works when JavaScript agrees
 * to run is not a download.
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
   * Which table is being taken, in the same `area:dimension` form the sheets
   * already use ("pages:pathname", "sources:channel"). The server reads it to
   * decide which query to run.
   */
  report?: string;
  /** Overrides the envelope. Defaults to the station's current settings. */
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
  subject = "this reading",
}: ExportMenuProps) {
  const state = useAppState();
  const { siteId, domain } = useSite();

  const env = useMemo<Envelope | null>(() => {
    if (envProp) return { ...envProp, report };
    if (siteId == null) return null;
    return { ...state.toEnvelope(siteId), report };
    // `toEnvelope` reads the live store, so every setting that changes the
    // reading has to be a dependency of the URL that exports it.
  }, [envProp, report, siteId, state.range, state.audience, state.compare, state.filters, state]);

  const stem = fileStem(report, domain, state.range);
  const ready = env != null;

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
            Both files carry the interval, the audience and every condition currently clipped on.
          </p>

          <RMenu.Item asChild>
            <a
              className="mfs-menu__item"
              href={ready ? api.exportUrl("/api/stats/export.csv", env) : undefined}
              download={`${stem}.csv`}
              target="_blank"
              rel="noopener noreferrer"
            >
              <span className="mfs-menu__item-name">Table of figures</span>
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
                The record and its sheets, laid out to print. <span className="mf-data">.pdf</span>
              </span>
            </a>
          </RMenu.Item>
        </RMenu.Content>
      </RMenu.Portal>
    </RMenu.Root>
  );
}

export default ExportMenu;
