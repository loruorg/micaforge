/**
 * BILLING: what an organisation used this month, and the plan it is on.
 *
 * The screen has three states, and the server says which one applies:
 *
 *   1. BILLING OFF (every self-hosted install, and the hosted beta). The
 *      month's usage only, and a plain statement that nothing is charged.
 *   2. BILLING ON, NO PAYMENTS. Usage, the current plan and its limits. No
 *      buy buttons, because there is nothing that could take the payment.
 *   3. BILLING ON WITH STRIPE. The plans, each with a button that starts a
 *      Stripe Checkout, and "Manage billing" once a subscription exists.
 *
 * Usage over the quota is never hidden or rounded away: the server keeps
 * recording past the quota, and the screen prints the server's own sentence
 * about what happens then, word for word.
 */
import { useEffect } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";

import { Button, Empty, ErrorState, Panel, PanelHead } from "../../components/react/ui";
import { api } from "../../lib/api";
import { ago, num } from "../../lib/format";
import { useCurrentOrg } from "./OrgSettings";
import { Block, Caution, Gauge, Note, Select, Spec, SpecRow, Stated, Waiting, errText } from "./SettingsLayout";
import "../../styles/app/workspace.css";

/* ── what the server sends ───────────────────────────────────────────────── */

interface Plan {
  id: string;
  name: string;
  /** Monthly event quota; null means none. */
  events: number | null;
  sites: number | null;
  members: number | null;
  price_label: string | null;
  /** The plan has a Stripe price and payments are set up. */
  purchasable: boolean;
  current: boolean;
}

interface Subscription {
  status: string;
  current_period_end: string | null;
  cancel_at_period_end: boolean;
  plan_id: string | null;
}

interface Billing {
  enabled: boolean;
  stripe: boolean;
  plan: Plan | null;
  subscription: Subscription | null;
  payment_failed_at: string | null;
  plans: Plan[];
  usage: {
    month: string;
    events: number;
    agent_events: number;
    quota: number | null;
    overage: number;
    percent: number | null;
    updated_at: string | null;
  };
  limits: {
    sites: { used: number; max: number | null };
    members: { used: number; max: number | null };
  };
  overage_policy: string;
}

/* ── wording ─────────────────────────────────────────────────────────────── */

const STATUS_WORDS: Record<string, string> = {
  active: "Active",
  trialing: "In its trial period",
  past_due: "Payment overdue",
  unpaid: "Unpaid",
  canceled: "Cancelled",
  incomplete: "Waiting for the first payment",
  incomplete_expired: "The first payment did not complete",
  paused: "Paused",
};

function monthName(month: string): string {
  const d = new Date(`${month}-01T00:00:00`);
  return Number.isNaN(d.getTime()) ? month : d.toLocaleDateString(undefined, { month: "long", year: "numeric" });
}

function day(iso: string | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}

const limitText = (n: number | null, unit: string) => (n == null ? `No limit on ${unit}` : `${num(n)} ${unit}`);

/* ── parts ───────────────────────────────────────────────────────────────── */

function Usage({ b }: { b: Billing }) {
  const u = b.usage;
  const pctUsed = u.percent;
  const level = pctUsed == null ? "none" : pctUsed >= 100 ? "over" : pctUsed >= 80 ? "warn" : "ok";
  return (
    <div className="mf-ws-usage" data-level={level}>
      <p className="mf-ws-usage__figure">
        <strong>{num(u.events)}</strong>
        <span>
          {u.quota != null ? `of ${num(u.quota)} events` : "events"} in {monthName(u.month)}
        </span>
      </p>
      {u.quota != null ? <Gauge value={u.events / Math.max(1, u.quota)} label="Events used this month" /> : null}
      {pctUsed != null ? (
        <p className="mf-ws-usage__level">
          {level === "over"
            ? `The quota is used up: ${num(u.overage)} event${u.overage === 1 ? "" : "s"} over it so far.`
            : `${Math.floor(pctUsed)}% of the quota is used.`}
        </p>
      ) : null}
      <Note>
        Page views, custom events, errors and Web Vitals count. {num(u.agent_events)} AI crawler visit
        {u.agent_events === 1 ? " was" : "s were"} also recorded this month; those are not counted toward the quota.
        {u.updated_at ? ` Counted ${ago(u.updated_at)}.` : ""}
      </Note>
    </div>
  );
}

function PlanCard({
  plan,
  canBuy,
  busy,
  onChoose,
}: {
  plan: Plan;
  canBuy: boolean;
  busy: boolean;
  onChoose: () => void;
}) {
  return (
    <article className="mf-ws-plan" data-current={plan.current ? "true" : undefined}>
      <div className="mf-ws-plan__head">
        <h4 className="mf-ws-plan__name">{plan.name}</h4>
        <span className="mf-ws-plan__price">{plan.price_label ?? (plan.purchasable ? "" : "Free")}</span>
      </div>
      {plan.current ? <span className="mf-ws-plan__current">Your current plan</span> : null}
      <ul className="mf-ws-plan__facts">
        <li>{plan.events == null ? "No event quota" : `${num(plan.events)} events a month`}</li>
        <li>{limitText(plan.sites, "sites")}</li>
        <li>{limitText(plan.members, "members")}</li>
      </ul>
      {canBuy && plan.purchasable && !plan.current ? (
        <div className="mf-ws-plan__act">
          <Button tone="primary" disabled={busy} onClick={onChoose}>
            Choose this plan
          </Button>
        </div>
      ) : null}
    </article>
  );
}

/* ── the screen ──────────────────────────────────────────────────────────── */

export function BillingSettings() {
  const { orgs, orgId, org, setChosen } = useCurrentOrg();
  const [query] = useSearchParams();
  const returned = query.get("billing");
  const returnedOrg = query.get("org");

  // A return from Stripe names the organisation it was for.
  useEffect(() => {
    if (returnedOrg && orgs.data?.some((o) => o.id === returnedOrg)) setChosen(returnedOrg);
  }, [returnedOrg, orgs.data, setChosen]);

  const billing = useQuery({
    queryKey: ["settings", "billing", orgId],
    queryFn: () => api.get<Billing>(`/api/billing?org=${encodeURIComponent(orgId)}`).then((r) => r.data),
    enabled: !!orgId,
  });

  const go = (url: string) => {
    window.location.href = url;
  };

  const checkout = useMutation({
    mutationFn: (plan: string) =>
      api.post<{ url: string }>("/api/billing/checkout", { org_id: orgId, plan }).then((r) => r.data),
    onSuccess: (d) => go(d.url),
  });

  const portal = useMutation({
    mutationFn: () => api.post<{ url: string }>("/api/billing/portal", { org_id: orgId }).then((r) => r.data),
    onSuccess: (d) => go(d.url),
  });

  const isOwner = org?.role === "owner";
  const b = billing.data;

  return (
    <Panel>
      <PanelHead
        title="Billing"
        note="What this organisation recorded this month, and the plan it is on."
        actions={
          (orgs.data?.length ?? 0) > 1 ? (
            <Select auto aria-label="Organisation" value={orgId} onChange={(e) => setChosen(e.target.value)}>
              {orgs.data?.map((o) => (
                <option key={o.id} value={o.id}>
                  {o.name}
                </option>
              ))}
            </Select>
          ) : undefined
        }
      />

      {returned === "success" ? (
        <div className="mf-set-block">
          <Note>
            Payment received. Stripe tells Micaforge about the new plan in a few seconds; reload this page if it
            does not show yet.
          </Note>
        </div>
      ) : returned === "cancelled" ? (
        <div className="mf-set-block">
          <Note>The checkout was cancelled. Nothing was charged and the plan did not change.</Note>
        </div>
      ) : null}

      {orgs.isError ? (
        <ErrorState title="Could not load your organisations" detail={errText(orgs.error)} onRetry={() => void orgs.refetch()} />
      ) : orgs.isSuccess && !orgs.data.length ? (
        <Empty title="You are not in an organisation" body="Billing belongs to an organisation. Create one to begin." />
      ) : billing.isPending ? (
        <Waiting rows={4} />
      ) : billing.isError ? (
        <ErrorState title="Could not load billing" detail={errText(billing.error)} onRetry={() => void billing.refetch()} />
      ) : b ? (
        <>
          <Block title="This month">
            <Usage b={b} />
          </Block>

          {!b.enabled ? (
            <Block
              title="Plan"
              body="Micaforge is free during the beta. Nothing is charged and there is no plan to choose."
            />
          ) : (
            <>
              <Block title="Plan">
                <Spec>
                  <SpecRow name="Current plan">
                    <Stated>
                      {b.plan ? `${b.plan.name}${b.plan.price_label ? `, ${b.plan.price_label}` : ""}` : "None"}
                    </Stated>
                  </SpecRow>
                  {b.subscription ? (
                    <>
                      <SpecRow name="Subscription">
                        <Stated>{STATUS_WORDS[b.subscription.status] ?? b.subscription.status}</Stated>
                      </SpecRow>
                      <SpecRow name={b.subscription.cancel_at_period_end ? "Ends on" : "Renews on"}>
                        <Stated>{day(b.subscription.current_period_end)}</Stated>
                      </SpecRow>
                    </>
                  ) : null}
                  <SpecRow name="Sites">
                    <Stated>
                      {num(b.limits.sites.used)}
                      {b.limits.sites.max != null ? ` of ${num(b.limits.sites.max)}` : ", no limit"}
                    </Stated>
                  </SpecRow>
                  <SpecRow name="Members">
                    <Stated>
                      {num(b.limits.members.used)}
                      {b.limits.members.max != null ? ` of ${num(b.limits.members.max)}` : ", no limit"}
                    </Stated>
                  </SpecRow>
                  <SpecRow name="Past the quota">
                    <Stated>{b.overage_policy}</Stated>
                  </SpecRow>
                </Spec>
                {b.payment_failed_at ? (
                  <Caution>
                    The last payment failed {ago(b.payment_failed_at)}. Update the card in Manage billing to keep the
                    plan.
                  </Caution>
                ) : null}
              </Block>

              {b.plans.length ? (
                <Block
                  title="Plans"
                  body={
                    !b.stripe
                      ? "Payments are not set up on this install, so plans are changed by the operator."
                      : isOwner
                        ? "Choosing a plan opens Stripe's checkout page. The plan changes when the payment completes."
                        : "Only the owner of this organisation can change its plan."
                  }
                >
                  <div className="mf-ws-plans">
                    {b.plans.map((p) => (
                      <PlanCard
                        key={p.id}
                        plan={p}
                        canBuy={b.stripe && isOwner}
                        busy={checkout.isPending || checkout.isSuccess}
                        onChoose={() => checkout.mutate(p.id)}
                      />
                    ))}
                  </div>
                </Block>
              ) : null}

              {checkout.isError || portal.isError || (b.stripe && isOwner && b.subscription) ? (
                <div className="mf-set-foot">
                  {checkout.isError ? (
                    <span className="mf-set-foot__say" data-tone="warn" role="alert">
                      {errText(checkout.error)}
                    </span>
                  ) : null}
                  {portal.isError ? (
                    <span className="mf-set-foot__say" data-tone="warn" role="alert">
                      {errText(portal.error)}
                    </span>
                  ) : null}
                  <span className="mf-set-foot__spacer" />
                  {b.stripe && isOwner && b.subscription ? (
                    <Button disabled={portal.isPending || portal.isSuccess} onClick={() => portal.mutate()}>
                      {portal.isPending ? "Opening…" : "Manage billing"}
                    </Button>
                  ) : null}
                </div>
              ) : null}
            </>
          )}
        </>
      ) : null}
    </Panel>
  );
}

export default BillingSettings;
