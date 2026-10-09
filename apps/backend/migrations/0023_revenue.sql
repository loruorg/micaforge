-- Micaforge: revenue from Stripe and Paddle webhooks.
--
-- The payments themselves are rows in ClickHouse (`revenue`). Postgres holds
-- what a webhook handler must read and write in place: the sealed signing
-- secret, the event ids already processed, the checkout reference remembered
-- for a customer, and how much of each charge has already been recorded as
-- refunded.
--
-- Additive only.

CREATE TABLE IF NOT EXISTS revenue_integrations (
    id             uuid PRIMARY KEY,
    site_id        uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    provider       text NOT NULL CHECK (provider IN ('stripe', 'paddle')),
    -- XChaCha20-Poly1305 under the `revenue` key, with the integration id as
    -- associated data, so the blob is worthless in any other row. NULL until
    -- the operator pastes it: Stripe and Paddle reveal the secret only after
    -- they have been given the webhook URL, which names this row.
    secret_sealed  bytea,
    enabled        boolean NOT NULL DEFAULT true,
    last_event_at  timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (site_id, provider)
);

-- Each provider event is recorded once. A redelivery finds its id here and is
-- answered without writing a second row.
CREATE TABLE IF NOT EXISTS revenue_events_seen (
    integration_id  uuid NOT NULL REFERENCES revenue_integrations (id) ON DELETE CASCADE,
    event_id        text NOT NULL,
    received_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (integration_id, event_id)
);

-- The handler prunes ids older than any provider retries for.
CREATE INDEX IF NOT EXISTS revenue_events_seen_age_idx
    ON revenue_events_seen (integration_id, received_at);

-- The checkout reference a customer's first checkout carried, so that the
-- subscription invoices and refunds that follow, which do not carry it, are
-- attributed to the same visit.
CREATE TABLE IF NOT EXISTS revenue_customer_refs (
    integration_id  uuid NOT NULL REFERENCES revenue_integrations (id) ON DELETE CASCADE,
    customer_id     text NOT NULL,
    checkout_ref    text NOT NULL,
    updated_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (integration_id, customer_id)
);

-- How much of one charge (Stripe) or adjustment (Paddle) is already recorded
-- as refunded. Stripe reports a charge's cumulative refunded amount on every
-- refund event, so only the difference from this total is new.
CREATE TABLE IF NOT EXISTS revenue_refund_totals (
    integration_id  uuid NOT NULL REFERENCES revenue_integrations (id) ON DELETE CASCADE,
    object_id       text NOT NULL,
    recorded_minor  bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (integration_id, object_id)
);
