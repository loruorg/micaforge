-- Micaforge: Google Search Console.
--
-- One row per site that has connected Search Console: the Google grant, the
-- property the site reads, and where the sync stands. The search data itself
-- (clicks, impressions, position per day, query, page, country and device)
-- lives in ClickHouse, in `search_console`; this table holds only what decides
-- what to fetch next and what the settings screen says about it.
--
-- `refresh_token_sealed` is Google's refresh token, XChaCha20-Poly1305 sealed
-- with a key derived from `MICAFORGE_SECRET_KEY_BASE` for the
-- `search-console-token` purpose and the site's own id as additional
-- authenticated data (`micaforge_jobs::search_console::seal_token`), so the
-- blob is worthless in any other row. It is set to NULL when Google refuses
-- it (the person revoked the grant, or it expired), and the whole row goes
-- when the site disconnects. No access token is ever stored: one is minted
-- from the refresh token for each sync and dropped when the sync ends.
--
-- Nothing here is a sign-in identity. The grant is for one site's property,
-- whoever approved it, and it is never used to identify a person.

CREATE TABLE IF NOT EXISTS search_console_connections (
    site_id                uuid PRIMARY KEY REFERENCES sites (id) ON DELETE CASCADE,
    -- Who approved the grant at Google, while that account exists.
    connected_by           uuid REFERENCES users (id) ON DELETE SET NULL,
    refresh_token_sealed   bytea,
    -- `sc-domain:example.com` or a URL prefix such as `https://example.com/`;
    -- NULL until the admin picks one of the account's properties.
    property               text,
    -- `pending`: connected, no property chosen yet. `active`: syncing.
    -- `revoked`: Google no longer accepts the grant; the site must connect
    -- again. The settings screen and the report both say which.
    status                 text NOT NULL DEFAULT 'pending'
                           CHECK (status IN ('pending', 'active', 'revoked')),
    -- The sync window, in Google's calendar (Pacific time). `backfill_until`
    -- is the oldest day the backfill will reach (sixteen months before the
    -- property was chosen, Google's own limit); `backfilled_from` is the oldest
    -- day fetched so far; `synced_through` the newest. All NULL before the
    -- first sync.
    backfill_until         date,
    backfilled_from        date,
    synced_through         date,
    -- One sync at a time per site: a worker takes the lease with a
    -- conditional UPDATE and gives it back when it finishes.
    lease_until            timestamptz,
    last_sync_started_at   timestamptz,
    last_sync_finished_at  timestamptz,
    -- The last failure, in a sentence safe to show an admin, and when. Cleared
    -- by the next sync that succeeds.
    last_error             text,
    last_error_at          timestamptz,
    rows_synced            bigint NOT NULL DEFAULT 0,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);

-- The daily sweep reads the active connections.
CREATE INDEX IF NOT EXISTS search_console_connections_status_idx
    ON search_console_connections (status);
