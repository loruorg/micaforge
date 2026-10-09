-- Micaforge: the audit log.
--
-- One row per change somebody made: who (a person, an API key, or the server
-- itself), what (`site.created`, `member.role_changed`, ...), to which object,
-- and the object's state before and after. `routes::audit` in the server
-- reads it back for the Organisation screen and the CSV export, and
-- `crate::audit::record` is the one function that writes it.
--
-- Three columns deliberately carry no foreign key:
--
--   * `org_id`. An audit log that disappeared with the thing it audits would
--     be no audit at all: deleting an organisation must leave the record that
--     somebody deleted it. Nobody outside the organisation can read its rows
--     (reads are scoped by current membership), so the rows of a deleted one
--     are readable by nobody but an operator with database access.
--   * `actor_id`, `site_id`. The same reasoning: a removed member and a deleted
--     site leave their history behind, labelled by the text columns that were
--     written at the time.
--
-- `org_id` is NULL for account events (a sign-in, a session ended). Those are
-- shown in the log of every organisation the actor is currently a member of,
-- because they are about the person, not about any one organisation.
--
-- `ip_hash` is a keyed digest of the address (the same construction as
-- `sessions_auth.ip_hash`, under its own key), never the address.

CREATE TABLE IF NOT EXISTS audit_events (
    id            uuid PRIMARY KEY,
    org_id        uuid,
    actor_kind    text NOT NULL CHECK (actor_kind IN ('user', 'api_key', 'system')),
    actor_id      uuid,
    actor_label   text NOT NULL DEFAULT '',
    action        text NOT NULL,
    target_type   text NOT NULL,
    target_id     text,
    target_label  text NOT NULL DEFAULT '',
    site_id       uuid,
    before        jsonb,
    after         jsonb,
    metadata      jsonb,
    ip_hash       text,
    user_agent    text,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- The screen pages one organisation newest first; ids are UUIDv7, so `id`
-- orders by time and doubles as the page cursor.
CREATE INDEX IF NOT EXISTS audit_events_org_idx ON audit_events (org_id, id DESC);
-- Account events are found through their actor, and the actor filter uses it.
CREATE INDEX IF NOT EXISTS audit_events_actor_idx ON audit_events (actor_id, id DESC);
-- "Everything that happened to this site / this flag".
CREATE INDEX IF NOT EXISTS audit_events_target_idx ON audit_events (target_type, target_id);
