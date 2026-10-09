-- Micaforge: sign-in with Google and GitHub.
--
-- An identity is the provider's stable id for a person, linked to one account.
-- The first sign-in through a provider links by email (the provider's
-- verified address equals the account's address); every later sign-in finds
-- the account through the identity, so a person who changes the address at the
-- provider still reaches the same account.
--
-- No token is stored. The access token is used once, inside the callback, to
-- read the profile, and then dropped: Micaforge never calls the provider on
-- the person's behalf afterwards, so keeping a credential for it would be a
-- liability with no use.

CREATE TABLE IF NOT EXISTS oauth_identities (
    provider      text NOT NULL CHECK (provider IN ('google', 'github')),
    -- Google's `sub`, GitHub's numeric user id, as text.
    subject       text NOT NULL,
    user_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The verified address the provider gave at the last sign-in. Shown in
    -- account settings so a person can tell which of their accounts is linked.
    email         citext NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz,
    PRIMARY KEY (provider, subject)
);

-- One identity per provider per account: linking a second GitHub account to
-- the same Micaforge account is refused, so "unlink GitHub" means one thing.
CREATE UNIQUE INDEX IF NOT EXISTS oauth_identities_user_provider_idx
    ON oauth_identities (user_id, provider);
