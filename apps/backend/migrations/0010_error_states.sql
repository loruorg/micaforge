-- Micaforge: the state of an error group, as the people who read the Errors
-- screen decided it.
--
-- An error group lives in ClickHouse as a fingerprint over rows that are only
-- ever appended, so the one fact a person adds to it, "we fixed this" or "this
-- is not ours to fix", has to live somewhere that can be updated. One row per
-- (site, fingerprint) that has been marked; a group with no row is open, which
-- is what every group is until somebody decides otherwise.
--
-- `resolved_at` is when the state was set. A resolved group that fires again
-- after it is reported as open once more (`routes::behaviour`), because a fix
-- that did not hold is exactly what the screen exists to show; the row is kept
-- so the history of the decision is not lost.

CREATE TABLE IF NOT EXISTS error_states (
    site_id      uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    fingerprint  text NOT NULL CHECK (length(fingerprint) BETWEEN 1 AND 64),
    status       text NOT NULL CHECK (status IN ('resolved', 'ignored')),
    -- Who set it. Kept as a blank when that account goes.
    updated_by   uuid REFERENCES users (id) ON DELETE SET NULL,
    resolved_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (site_id, fingerprint)
);
