-- Micaforge: PostHog and Simple Analytics imports.
--
-- Two more tools can be imported from, so `imports.source` accepts two more
-- values. The column's CHECK was declared inline in `0001_init.sql`, which
-- gives it the name Postgres derives for a column check,
-- `imports_source_check`. It is replaced rather than altered, because a CHECK
-- cannot be altered in place; dropping it with `IF EXISTS` first makes the
-- pair safe to run against a database that already has the new one. The new
-- list is a superset of the old, so every existing row passes it.
--
-- `credentials_sealed` holds the API key of an import that reads from a
-- provider's API instead of an uploaded file: XChaCha20-Poly1305, keyed off
-- `MICAFORGE_SECRET_KEY_BASE` for the `imports` purpose, with the import's own
-- id as additional authenticated data, so the blob is worthless in any other
-- row (`micaforge_jobs::import_api`). It is NULL for a file import, and it is
-- set back to NULL when the import finishes or is given up for good, because a
-- credential nothing will use again is only a liability.

ALTER TABLE imports DROP CONSTRAINT IF EXISTS imports_source_check;
ALTER TABLE imports ADD CONSTRAINT imports_source_check
    CHECK (source IN ('ga4', 'plausible', 'umami', 'posthog', 'simple_analytics'));

ALTER TABLE imports ADD COLUMN IF NOT EXISTS credentials_sealed bytea;
