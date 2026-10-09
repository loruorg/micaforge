/**
 * AI INSIGHTS: what the AI analyst can do on this instance, and the weekly
 * digest for the person reading.
 *
 * Two facts with two owners, stated side by side:
 *
 *   1. WHETHER THERE IS A MODEL is the instance operator's decision, made with
 *      server variables. Without one, Ask AI is off and investigations still
 *      open with their statistical findings, only without the written
 *      explanation. The screen says exactly that, and names the variables,
 *      rather than showing a switch that cannot do anything.
 *   2. THE WEEKLY DIGEST is each reader's own choice, per site, off by default.
 *      It is mail, so when this instance sends no mail the screen says so next
 *      to the switch instead of accepting a subscription that can never be
 *      delivered.
 */
import { Link, useParams } from "react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { Panel, PanelHead } from "../../components/react/ui";
import { IconOut } from "../../components/react/icons";
import { api } from "../../lib/api";
import type { AiStatus, InsightSubscription } from "../../lib/api";
import { num } from "../../lib/format";
import { useSite } from "../providers";
import { Note, QueryState, Result, Spec, SpecRow, Stated, Status, Toggle } from "./SettingsLayout";
import "../../styles/app/connect.css";

export function AiInsightsSettings() {
  const { siteId, domain } = useSite();
  const { site } = useParams();
  const qc = useQueryClient();
  const subKey = ["settings", "insights-subscription", siteId];

  const status = useQuery({
    queryKey: ["ai", "status"],
    queryFn: () => api.get<AiStatus>("/api/ai/status").then((r) => r.data),
  });

  const sub = useQuery({
    queryKey: subKey,
    queryFn: () =>
      api.get<InsightSubscription>("/api/insights/subscription", { site: siteId as number }).then((r) => r.data),
    enabled: siteId != null,
  });

  const save = useMutation({
    mutationFn: (weekly_digest: boolean) =>
      api
        .put<InsightSubscription>(`/api/insights/subscription?site=${encodeURIComponent(String(siteId))}`, {
          weekly_digest,
        })
        .then((r) => r.data),
    onSuccess: (next) => qc.setQueryData(subKey, next),
  });

  return (
    <>
      <Panel>
        <PanelHead
          title="AI analyst"
          note="Answers questions about this site's traffic on the Ask AI screen and in linked chat channels, and writes the likely causes on each investigation."
        />
        <QueryState query={status}>
          {(s) => (
            <Spec>
              <SpecRow name="Status">
                <Status state={s.configured ? "done" : "waiting"} label={s.configured ? "Configured" : "Not configured"} />
                {s.configured ? null : (
                  <Note>
                    The operator of this instance switches it on by setting{" "}
                    <code className="mf-data">MICAFORGE_LLM_BASE_URL</code> and{" "}
                    <code className="mf-data">MICAFORGE_LLM_MODEL</code> (and{" "}
                    <code className="mf-data">MICAFORGE_LLM_API_KEY</code> when the model needs one). Until then,
                    investigations still open with their findings, without a written explanation.
                  </Note>
                )}
              </SpecRow>
              {s.configured ? (
                <>
                  <SpecRow name="Model">
                    <Stated>
                      <span className="mf-data">{s.model ?? "not named"}</span>
                    </Stated>
                  </SpecRow>
                  <SpecRow
                    name="Daily limit"
                    hint="Shared by Ask AI, MCP clients, chat apps and investigations, per organisation, per UTC day."
                  >
                    <Stated>
                      {s.daily_limit == null ? "No limit" : `${num(s.daily_limit)} questions a day`}
                    </Stated>
                  </SpecRow>
                </>
              ) : null}
              <SpecRow name="Read more">
                <div className="mf-cx-links">
                  <a className="mf-cx-link" href="/docs/dashboard/ask-ai">
                    How Ask AI works
                    <IconOut size={12} />
                  </a>
                  {site ? (
                    <Link className="mf-cx-link" to={`/app/${site}/insights`}>
                      Open Insights
                      <IconOut size={12} />
                    </Link>
                  ) : null}
                </div>
              </SpecRow>
            </Spec>
          )}
        </QueryState>
      </Panel>

      <Panel>
        <PanelHead
          title="Weekly digest"
          note={`A summary of the week's investigations for ${domain ?? "this site"}, by email, to you only.`}
        />
        <QueryState query={sub}>
          {(d) => (
            <Spec>
              <SpecRow
                name="Send me the weekly digest"
                hint="Off by default. Each person who can read this site chooses for themselves."
              >
                <Toggle
                  label="Send me the weekly digest"
                  checked={d.weekly_digest}
                  disabled={save.isPending || (!d.mail_enabled && !d.weekly_digest)}
                  onChange={(v) => save.mutate(v)}
                />
                {d.mail_enabled ? null : (
                  <Note>
                    This instance does not send mail, so a digest cannot be delivered. The operator sets up mail
                    in the server configuration.
                  </Note>
                )}
                <Result action={save} done={save.data?.weekly_digest ? "Digest switched on." : "Digest switched off."} />
              </SpecRow>
            </Spec>
          )}
        </QueryState>
      </Panel>
    </>
  );
}

export default AiInsightsSettings;
