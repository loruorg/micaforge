-- Micaforge: insights and investigations.
--
-- An investigation is one significant change on one site, kept open until it
-- is understood or gone. The detector opens it with the statistical finding,
-- the model (when one is configured) adds its reading of the likely causes,
-- and later re-checks move it from `open` to `monitoring` to `resolved`, or
-- back. Each of those steps is an event, so the detail screen can show the
-- whole history rather than only the latest state.
--
-- `signal_key` names what changed (`wow:visitors`, `wow:source:chatgpt.com`,
-- `hour:visitors:spike`). It is how the detector finds the investigation it
-- already opened instead of opening a second one for the same change: at most
-- one unresolved investigation per site and signal, enforced by the partial
-- unique index below rather than by a check-then-insert that two workers could
-- both pass.

CREATE TABLE IF NOT EXISTS investigations (
    id                  uuid PRIMARY KEY,
    site_id             uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    signal_key          text NOT NULL,
    title               text NOT NULL,
    status              text NOT NULL DEFAULT 'open'
                        CHECK (status IN ('open', 'monitoring', 'resolved')),
    severity            text NOT NULL DEFAULT 'info'
                        CHECK (severity IN ('info', 'warning', 'critical')),
    metric              text NOT NULL,
    dimension           text,
    dimension_value     text,
    direction           text NOT NULL CHECK (direction IN ('up', 'down')),
    current_value       double precision NOT NULL DEFAULT 0,
    baseline_value      double precision NOT NULL DEFAULT 0,
    -- Percent, so 25 is a rise of a quarter. NULL when the baseline was zero:
    -- a change from nothing has no percentage, and inventing one would be a lie.
    change_pct          double precision,
    -- What was compared with what, in words ("7d vs previous 7d").
    window_label        text NOT NULL DEFAULT '',
    findings            jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- The queries behind the findings and the explanation, in the same shape
    -- the AI analyst stores on a message.
    evidence            jsonb NOT NULL DEFAULT '[]'::jsonb,
    explanation         text,
    explanation_status  text NOT NULL DEFAULT 'not_configured'
                        CHECK (explanation_status IN ('ready', 'not_configured', 'failed')),
    -- How many re-checks in a row found the change gone. Two moves an
    -- investigation from `monitoring` to `resolved`; one is a quiet day.
    calm_checks         integer NOT NULL DEFAULT 0,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    next_check_at       timestamptz,
    resolved_at         timestamptz
);

CREATE UNIQUE INDEX IF NOT EXISTS investigations_one_live_per_signal
    ON investigations (site_id, signal_key)
    WHERE status IN ('open', 'monitoring');

CREATE INDEX IF NOT EXISTS investigations_site_idx
    ON investigations (site_id, updated_at DESC);

-- The re-check sweep reads only what is due.
CREATE INDEX IF NOT EXISTS investigations_due_idx
    ON investigations (next_check_at)
    WHERE status IN ('open', 'monitoring') AND next_check_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS investigation_events (
    id                uuid PRIMARY KEY,
    investigation_id  uuid NOT NULL REFERENCES investigations (id) ON DELETE CASCADE,
    kind              text NOT NULL CHECK (kind IN ('opened', 'checked', 'explained', 'status', 'posted')),
    summary           text NOT NULL,
    data              jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS investigation_events_idx
    ON investigation_events (investigation_id, created_at);

-- Who wants the weekly digest of a site. A row exists only for someone who
-- chose; the absence of a row is "no", which is the default the product owes a
-- person who never asked for mail.
CREATE TABLE IF NOT EXISTS insight_subscriptions (
    site_id        uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    user_id        uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    weekly_digest  boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (site_id, user_id)
);
