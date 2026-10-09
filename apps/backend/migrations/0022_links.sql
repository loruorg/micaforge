-- Micaforge: short links and the folders they are filed in.
--
-- A link redirects `/l/<slug>` (and `<MICAFORGE_LINKS_HOST>/<slug>`) to its
-- target. The clicks are an append-only stream in ClickHouse (`link_clicks`);
-- this table is the link itself.
--
-- Additive only.

CREATE TABLE IF NOT EXISTS link_folders (
    id          uuid PRIMARY KEY,
    org_id      uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    name        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS links (
    id           uuid PRIMARY KEY,
    org_id       uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    -- NULL for an organisation-wide link. A site's links go with the site.
    site_id      uuid REFERENCES sites (id) ON DELETE CASCADE,
    folder_id    uuid REFERENCES link_folders (id) ON DELETE SET NULL,
    -- One link namespace per install: `/l/<slug>` on the API origin and
    -- `/<slug>` on the links host answer for the same link. The redirect looks
    -- a link up by this column on every click.
    slug         text NOT NULL UNIQUE,
    target_url   text NOT NULL,
    -- `{ source, medium, campaign, term, content }`, appended to the target as
    -- `utm_*` at redirect time.
    utm          jsonb NOT NULL DEFAULT '{}'::jsonb,
    title        text,
    description  text,
    image_url    text,
    ios_url      text,
    android_url  text,
    deep_link    text,
    expires_at   timestamptz,
    created_by   uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS links_org_idx ON links (org_id, created_at DESC);
CREATE INDEX IF NOT EXISTS links_site_idx ON links (site_id, created_at DESC) WHERE site_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS links_folder_idx ON links (folder_id) WHERE folder_id IS NOT NULL;
