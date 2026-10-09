-- Micaforge: annotations written by the GitHub App.
--
-- A deploy, a release or a push recorded from GitHub is an annotation like any
-- other, so it appears on every chart that already draws annotations. It needs
-- two things the table did not have: `github` as a source, and the address of
-- the object on GitHub, so a reader can open the release or the deployed
-- commit from the note.
--
-- The source CHECK was written inline in 0034 with no name, so Postgres chose
-- one (`annotations_source_check` by its rule, but a rule is not a guarantee).
-- The block below finds every CHECK constraint on `annotations` whose only
-- column is `source`, whatever it is called, drops it, and the constraint is
-- added again under a name this file states. Run twice, it finds the named
-- constraint and replaces it with itself.

DO $$
DECLARE
    found record;
BEGIN
    FOR found IN
        SELECT con.conname
        FROM pg_constraint con
        JOIN pg_class rel ON rel.oid = con.conrelid
        JOIN pg_namespace nsp ON nsp.oid = rel.relnamespace
        JOIN pg_attribute att ON att.attrelid = rel.oid AND att.attname = 'source'
        WHERE rel.relname = 'annotations'
          AND nsp.nspname = current_schema()
          AND con.contype = 'c'
          AND con.conkey = ARRAY[att.attnum]
    LOOP
        EXECUTE format('ALTER TABLE annotations DROP CONSTRAINT %I', found.conname);
    END LOOP;
END
$$;

ALTER TABLE annotations
    ADD CONSTRAINT annotations_source_check
    CHECK (source IN ('dashboard', 'api', 'mcp', 'github'));

-- The object on GitHub. NULL for every note a person, a script or an agent
-- writes; only the GitHub App sets it.
ALTER TABLE annotations ADD COLUMN IF NOT EXISTS url text;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'annotations_url_https'
    ) THEN
        ALTER TABLE annotations
            ADD CONSTRAINT annotations_url_https CHECK (url IS NULL OR url ~ '^https://');
    END IF;
END
$$;
