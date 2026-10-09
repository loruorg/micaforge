-- Micaforge: log drains, a hosting platform's request log as a source of agent hits.
--
-- A site on Vercel has no access log to follow and often no server of its own to
-- put a middleware in, yet Vercel saw every crawler fetch it answered. A log
-- drain is the address Vercel posts those request logs to:
-- `POST /api/log/vercel/{id}`, one row here per address. The route verifies the
-- delivery against the drain's own secret and files each request through the
-- same agent rules as `POST /api/log/edge` (`routes::log_drain`).
--
-- `secret_sealed` is the HMAC key Vercel signs each delivery with. It is a
-- secret, so `docs/CONTRACT.md` §2 requires it sealed at rest: XChaCha20-Poly1305
-- keyed off MICAFORGE_SECRET_KEY_BASE for the `log-drain` purpose, with the
-- drain's own id as additional authenticated data so the blob is worthless in
-- another row, stored as unpadded url-safe base64 like the ingest key. It is
-- shown in plaintext once, when it is minted, and never read back over the API.
--
-- `verify_token` is not a secret. It is the value Vercel's URL ownership check
-- asks the endpoint to echo in an `x-vercel-verify` response header, which the
-- legacy log-drain flow and the Terraform provider still perform. Null for a
-- drain created through the current Drains screen, which asks for none.
--
-- `last_received_at` is the last verified delivery, written at most once a
-- minute per drain, so the settings screen can say whether anything arrives.

CREATE TABLE IF NOT EXISTS log_drains (
    id                uuid PRIMARY KEY,
    site_id           uuid NOT NULL REFERENCES sites(id) ON DELETE CASCADE,
    provider          text NOT NULL CHECK (provider IN ('vercel')),
    secret_sealed     text NOT NULL,
    verify_token      text,
    -- Who created it. Kept when that account goes, as a blank rather than a
    -- reason to lose the drain.
    created_by        uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    last_received_at  timestamptz
);

-- The settings screen lists a site's drains; the collection route reads by id.
CREATE INDEX IF NOT EXISTS log_drains_site_id_idx ON log_drains (site_id);
