-- Micaforge: uptime monitors and the incidents they open.
--
-- A monitor is a URL the server checks on its own schedule. The check results
-- themselves are an append-only stream and live in ClickHouse (`uptime_checks`);
-- this table holds only what the scheduler and the incident state machine need
-- to read and change in place: when the monitor is next due, who holds the
-- check in flight, and how many checks in a row have failed.
--
-- Additive only. Nothing here touches an existing table.

CREATE TABLE IF NOT EXISTS uptime_monitors (
    id                  uuid PRIMARY KEY,
    site_id             uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    name                text NOT NULL,
    url                 text NOT NULL,
    method              text NOT NULL DEFAULT 'GET' CHECK (method IN ('GET', 'HEAD', 'POST')),
    -- One minute to one hour. The scheduler sweeps once a minute, so a shorter
    -- interval would be a promise it cannot keep.
    interval_secs       integer NOT NULL DEFAULT 300 CHECK (interval_secs BETWEEN 60 AND 3600),
    timeout_ms          integer NOT NULL DEFAULT 10000 CHECK (timeout_ms BETWEEN 1000 AND 30000),
    status_min          integer NOT NULL DEFAULT 200 CHECK (status_min BETWEEN 100 AND 599),
    status_max          integer NOT NULL DEFAULT 299 CHECK (status_max BETWEEN 100 AND 599),
    keyword             text,
    json_path           text,
    json_expected       text,
    ssl_check           boolean NOT NULL DEFAULT true,
    ssl_expiry_days     integer NOT NULL DEFAULT 14 CHECK (ssl_expiry_days BETWEEN 1 AND 365),
    failure_threshold   integer NOT NULL DEFAULT 2 CHECK (failure_threshold BETWEEN 1 AND 10),
    -- The same channels document an alert carries, read by the same parser.
    channels            jsonb NOT NULL DEFAULT '[]'::jsonb,
    enabled             boolean NOT NULL DEFAULT true,
    state               text NOT NULL DEFAULT 'unknown' CHECK (state IN ('unknown', 'up', 'down')),
    consecutive_failures integer NOT NULL DEFAULT 0,
    last_checked_at     timestamptz,
    next_check_at       timestamptz NOT NULL DEFAULT now(),
    -- Set while a check is in flight, to when that check must have ended. The
    -- claim refuses a monitor whose lease is still running, which is what keeps
    -- two sweeps (or two replicas) from checking one monitor at once.
    lease_until         timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now(),
    CHECK (status_min <= status_max)
);

CREATE INDEX IF NOT EXISTS uptime_monitors_site_idx ON uptime_monitors (site_id, created_at);
-- The sweep's claim query: enabled monitors in due order.
CREATE INDEX IF NOT EXISTS uptime_monitors_due_idx ON uptime_monitors (next_check_at) WHERE enabled;

CREATE TABLE IF NOT EXISTS uptime_incidents (
    id             uuid PRIMARY KEY,
    monitor_id     uuid NOT NULL REFERENCES uptime_monitors (id) ON DELETE CASCADE,
    site_id        uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    started_at     timestamptz NOT NULL,
    resolved_at    timestamptz,
    -- The error of the check that crossed the threshold, as a sentence.
    cause          text NOT NULL DEFAULT '',
    failed_checks  integer NOT NULL DEFAULT 0
);

-- At most one open incident per monitor. The state machine relies on it: a
-- second "open" for a monitor that is already down is refused by Postgres
-- rather than trusted to the code that decided it.
CREATE UNIQUE INDEX IF NOT EXISTS uptime_incidents_one_open
    ON uptime_incidents (monitor_id) WHERE resolved_at IS NULL;
CREATE INDEX IF NOT EXISTS uptime_incidents_site_idx ON uptime_incidents (site_id, started_at DESC);
