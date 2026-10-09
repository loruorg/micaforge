-- Micaforge: OAuth 2.1 for the MCP server.
--
-- An MCP client (an editor, a desktop assistant) registers itself, sends a
-- person to the dashboard to approve it, and receives tokens that act for that
-- person inside one organisation, with the scopes they chose. API keys keep
-- working beside this; OAuth exists so a person can connect a client without
-- minting, copying and pasting a key, and can see and revoke it later.
--
-- Clients come from dynamic registration (RFC 7591), so a row here is not a
-- vetted party: it is a name and a set of redirect addresses somebody posted.
-- What makes a token trustworthy is the consent the signed-in person gave, so
-- the grant records who consented, for which organisation and which scopes.
--
-- Tokens are stored as SHA-256 digests only, like API keys and sessions, so a
-- dump yields nothing usable. Authorization codes never touch Postgres: they
-- live ten minutes in Valkey and are consumed once.

CREATE TABLE IF NOT EXISTS oauth_clients (
    id                          uuid PRIMARY KEY,
    -- The public identifier the client sends. Random, unrelated to `id`.
    client_id                   text NOT NULL UNIQUE,
    client_name                 text NOT NULL,
    redirect_uris               jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- `none` for a public client (PKCE is its proof), or `client_secret_post` /
    -- `client_secret_basic` for one that keeps a secret.
    token_endpoint_auth_method  text NOT NULL DEFAULT 'none'
                                CHECK (token_endpoint_auth_method IN ('none', 'client_secret_post', 'client_secret_basic')),
    client_secret_hash          text,
    client_uri                  text,
    created_at                  timestamptz NOT NULL DEFAULT now()
);

-- One row per consent: a person allowed a client into an organisation. The
-- current access and refresh tokens hang off it, and revoking the grant kills
-- both at once.
CREATE TABLE IF NOT EXISTS oauth_grants (
    id                  uuid PRIMARY KEY,
    client_id           uuid NOT NULL REFERENCES oauth_clients (id) ON DELETE CASCADE,
    user_id             uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    org_id              uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    scopes              jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- The resource (RFC 8707) the tokens are bound to: the MCP endpoint URL.
    resource            text NOT NULL,
    access_prefix       text NOT NULL,
    access_hash         text NOT NULL,
    access_expires_at   timestamptz NOT NULL,
    refresh_hash        text NOT NULL,
    refresh_expires_at  timestamptz NOT NULL,
    -- The refresh token this one replaced. Presenting it again is a replay of a
    -- token that should have been thrown away, and revokes the whole grant.
    previous_refresh_hash text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    last_used_at        timestamptz,
    revoked_at          timestamptz
);

CREATE INDEX IF NOT EXISTS oauth_grants_access_idx ON oauth_grants (access_prefix);
CREATE INDEX IF NOT EXISTS oauth_grants_refresh_idx ON oauth_grants (refresh_hash);
CREATE INDEX IF NOT EXISTS oauth_grants_previous_idx ON oauth_grants (previous_refresh_hash)
    WHERE previous_refresh_hash IS NOT NULL;
CREATE INDEX IF NOT EXISTS oauth_grants_user_idx ON oauth_grants (user_id, created_at DESC);
