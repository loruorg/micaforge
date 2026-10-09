-- Micaforge: billing for the hosted service.
--
-- Off by default (`MICAFORGE_BILLING_ENABLED`), and a self-hosted install
-- never needs these tables to hold a row: with billing off nothing writes them
-- except the usage meter, which records what each organisation measured so the
-- Billing screen can show it.
--
-- Plans are configuration (`MICAFORGE_BILLING_PLANS`), not rows: a plan's
-- quota and limits are a product decision an operator ships, and a table would
-- invite editing them under a live subscription. Postgres only remembers which
-- plan an organisation is on, what Stripe last said about its subscription,
-- and what the meter measured.

CREATE TABLE IF NOT EXISTS org_billing (
    org_id                  uuid PRIMARY KEY REFERENCES organizations (id) ON DELETE CASCADE,
    -- A configured plan id. NULL means the default (free) plan.
    plan_id                 text,
    stripe_customer_id      text,
    stripe_subscription_id  text,
    -- Stripe's subscription status, or `none` when there has never been one.
    status                  text NOT NULL DEFAULT 'none' CHECK (status IN (
                                'none', 'incomplete', 'incomplete_expired', 'trialing',
                                'active', 'past_due', 'canceled', 'unpaid', 'paused'
                            )),
    current_period_end      timestamptz,
    cancel_at_period_end    boolean NOT NULL DEFAULT false,
    -- When Stripe last reported a failed invoice payment, until a payment works.
    payment_failed_at       timestamptz,
    updated_at              timestamptz NOT NULL DEFAULT now()
);

-- The webhook finds the organisation by customer when an event carries no
-- metadata of ours.
CREATE UNIQUE INDEX IF NOT EXISTS org_billing_customer_idx
    ON org_billing (stripe_customer_id) WHERE stripe_customer_id IS NOT NULL;

-- One row per organisation per calendar month (UTC), written by the
-- `meter_usage` job. It is the cache the Billing screen reads, the record of
-- how far an organisation went past its quota, and the memory of which usage
-- emails were already sent, so each threshold mails once a month.
CREATE TABLE IF NOT EXISTS billing_usage (
    org_id          uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    month           date NOT NULL,
    -- `events` rows (pageviews, custom events, errors, vitals): the metered unit.
    events          bigint NOT NULL DEFAULT 0,
    -- `agent_events` rows: shown beside the quota, never counted against it.
    agent_events    bigint NOT NULL DEFAULT 0,
    -- The quota in force when measured. NULL means the plan has none.
    quota           bigint,
    -- Events past the quota. Recorded, never dropped.
    overage_events  bigint NOT NULL DEFAULT 0,
    warned_80_at    timestamptz,
    warned_100_at   timestamptz,
    measured_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, month)
);

-- Stripe retries a webhook it did not see answered, and may deliver one event
-- twice. Each event id is recorded once, so a retry changes nothing.
CREATE TABLE IF NOT EXISTS stripe_events (
    id           text PRIMARY KEY,
    type         text NOT NULL,
    received_at  timestamptz NOT NULL DEFAULT now()
);
