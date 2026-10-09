-- Micaforge: target groups, dependencies, environments and change history for
-- feature flags.
--
-- All additive. A flag written by an older build reads back with no target
-- groups, no prerequisites and no environment overrides, which is exactly what
-- it had, so it evaluates the same before and after this migration.
--
-- `target_groups`, `dependencies` and `environments` are jsonb on the flag row
-- rather than join tables because the flag's whole definition has to be
-- snapshotted into `flag_changes` and restored by a revert as one value. A
-- join table would make "the flag as it was at 14:02" a reconstruction across
-- three tables instead of a copy of one row. The shapes are validated by
-- `micaforge_core::flags` on every write, and a group id that no longer exists
-- simply matches nobody at evaluation.

ALTER TABLE feature_flags
    ADD COLUMN IF NOT EXISTS name          text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS description   text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS target_groups jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS dependencies  jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS environments  jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD COLUMN IF NOT EXISTS updated_at    timestamptz NOT NULL DEFAULT now();

-- A reusable audience: one condition document, attached to as many flags as
-- want it. Editing the group changes who every attached flag reaches.
CREATE TABLE IF NOT EXISTS flag_target_groups (
    id           uuid PRIMARY KEY,
    site_id      uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    name         text NOT NULL,
    description  text NOT NULL DEFAULT '',
    conditions   jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    -- Two groups called "Beta users" on one site would make every picker
    -- ambiguous.
    UNIQUE (site_id, name)
);

-- Every change to a flag, with the whole definition before and after it, so
-- the detail screen can show a field-by-field diff and put any earlier state
-- back. Deleting a flag takes its history with it; the audit log keeps the
-- record that it was deleted, and by whom.
CREATE TABLE IF NOT EXISTS flag_changes (
    id           uuid PRIMARY KEY,
    flag_id      uuid NOT NULL REFERENCES feature_flags (id) ON DELETE CASCADE,
    site_id      uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    action       text NOT NULL CHECK (action IN ('created', 'updated', 'toggled', 'reverted')),
    actor_kind   text NOT NULL CHECK (actor_kind IN ('user', 'api_key', 'system')),
    actor_id     uuid,
    actor_label  text NOT NULL DEFAULT '',
    before       jsonb,
    after        jsonb,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS flag_changes_flag_idx ON flag_changes (flag_id, id DESC);
CREATE INDEX IF NOT EXISTS flag_target_groups_site_idx ON flag_target_groups (site_id);
