/**
 * One line that says the numbers on this screen are a sample, when they are.
 *
 * A site can tell its tracker to keep only some visitors (`data-sample`). No
 * figure is scaled up to pretend otherwise: a sampled count is shown as
 * counted. So the honest thing is to say so, beside the numbers, in plain
 * words, and to say nothing at all when every visitor was recorded.
 *
 * Reads `GET /api/stats/sampling` for the screen's own window. A failed read
 * shows nothing rather than an error: the screen's numbers are still right,
 * and a note about a note is noise.
 */
import { useQuery } from "@tanstack/react-query";
import { api } from "../../lib/api";
import { pct } from "../../lib/format";
import { IconInfo } from "../../components/react/icons";
import { useAppState } from "../state";
import { useSite } from "../providers";
import "../../styles/app/signals.css";

export interface Sampling {
  sampled: boolean;
  min_rate: number;
  max_rate: number;
  /** Share of the window's pageviews recorded under a rate below 100, 0..1. */
  sampled_share: number;
}

/** The sentence for a sampling reading, or `null` when nothing was sampled. */
export function samplingWords(s: Sampling | null | undefined): string | null {
  if (!s || !s.sampled) return null;
  const rate =
    s.min_rate === s.max_rate || s.max_rate >= 100
      ? `${s.min_rate}%`
      : `${s.min_rate}% to ${s.max_rate}%`;
  const part =
    s.sampled_share >= 0.995
      ? "All of this period"
      : `${pct(s.sampled_share)} of the pageviews in this period`;
  return `Sampled: ${part} came from a tracker that keeps ${rate} of visitors (data-sample). The figures are counted, not scaled up.`;
}

export function SamplingNote() {
  const { siteId, isLoading } = useSite();
  const { range, filters, toEnvelope } = useAppState();
  const q = useQuery({
    queryKey: ["stats", "sampling", siteId, range, JSON.stringify(filters ?? [])],
    queryFn: () =>
      api.get<Sampling>("/api/stats/sampling", toEnvelope(siteId as number)).then((r) => r.data),
    enabled: siteId != null && !isLoading,
    staleTime: 60_000,
  });
  const words = samplingWords(q.data);
  if (!words) return null;
  return (
    <p className="mf-sample-note" role="note">
      <IconInfo size={16} />
      <span>{words}</span>
    </p>
  );
}

export default SamplingNote;
