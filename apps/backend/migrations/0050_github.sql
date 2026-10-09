-- Micaforge: the GitHub App.
--
-- An organisation installs the App on a GitHub account; a site links one
-- repository through that installation; GitHub then sends this server its
-- deploys, releases and pushes, and each one the site asked for becomes an
-- annotation on the site's charts. The four tables below are that chain:
--
--   github_installations  which GitHub installations an organisation proved
--                         it can reach (the install callback checks it with
--                         the person's own GitHub token before a row exists)
--   github_repo_links     the one repository a site follows, and what from it
--                         becomes an annotation
--   github_deliveries     every verified webhook delivery, by GitHub's own
--                         delivery id: the dedupe key, and the list a site
--                         admin reads to see what arrived
--   github_events         the deploys, releases and pushes recorded for a site,
--                         one row per GitHub object, which is what makes a
--                         redelivery a no-op and what the AI analyst reads
--
-- No GitHub credential is stored here. Installation tokens live an hour and
-- are minted on demand from the App's private key (held in the environment,
-- never in Postgres) and cached, sealed, in Valkey.

CREATE TABLE IF NOT EXISTS github_installations (
    id                    uuid PRIMARY KEY,
    org_id                uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    -- GitHub's installation id. One installation may serve two organisations
    -- (an agency and its client, say) when a person in each proved access.
    installation_id       bigint NOT NULL,
    account_login         text NOT NULL,
    -- GitHub's own word: `User` or `Organization`.
    account_type          text NOT NULL,
    -- `all` or `selected`: whether the App sees every repository of the account.
    repository_selection  text,
    -- Set while the account has the App suspended; nothing is recorded then.
    suspended_at          timestamptz,
    installed_by          uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, installation_id)
);

-- The webhook resolves a delivery's installation id to its rows.
CREATE INDEX IF NOT EXISTS github_installations_github_idx
    ON github_installations (installation_id);

CREATE TABLE IF NOT EXISTS github_repo_links (
    id                 uuid PRIMARY KEY,
    -- One repository per site. A monorepo that deploys two sites is linked
    -- from both, so the same repository may appear on several rows.
    site_id            uuid NOT NULL UNIQUE REFERENCES sites (id) ON DELETE CASCADE,
    installation_ref   uuid NOT NULL REFERENCES github_installations (id) ON DELETE CASCADE,
    repo_id            bigint NOT NULL,
    repo_full_name     text NOT NULL,
    -- Refreshed from every push, so a renamed default branch keeps working.
    default_branch     text NOT NULL,
    html_url           text NOT NULL CHECK (html_url ~ '^https://'),
    annotate_deploys   boolean NOT NULL DEFAULT true,
    annotate_releases  boolean NOT NULL DEFAULT true,
    -- Off by default: a busy default branch is dozens of notes a day.
    annotate_pushes    boolean NOT NULL DEFAULT false,
    -- The deployment environments recorded, matched without regard to case.
    -- An empty list records every environment.
    environments       jsonb NOT NULL DEFAULT '["production"]'::jsonb
                       CHECK (jsonb_typeof(environments) = 'array'),
    -- When true, an agent whose key or OAuth grant holds `manage:write` may turn
    -- the feedback it sent about this site into an issue in this repository.
    -- A site admin decides that here; no credential can grant it to itself.
    agent_issues       boolean NOT NULL DEFAULT false,
    created_by         uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

-- The webhook finds the sites that follow a delivery's repository.
CREATE INDEX IF NOT EXISTS github_repo_links_repo_idx ON github_repo_links (repo_id);

CREATE TABLE IF NOT EXISTS github_deliveries (
    -- `X-GitHub-Delivery`. A redelivery from GitHub carries the same id.
    delivery_id      text PRIMARY KEY,
    event            text NOT NULL,
    action           text,
    installation_id  bigint,
    repo_id          bigint,
    repo_full_name   text,
    -- `processing` while being handled; `failed` may be claimed again by a
    -- redelivery, `recorded` and `ignored` may not.
    outcome          text NOT NULL DEFAULT 'processing'
                     CHECK (outcome IN ('processing', 'recorded', 'ignored', 'failed')),
    -- One plain sentence for the settings screen: what was recorded, or why not.
    detail           text,
    received_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS github_deliveries_repo_idx
    ON github_deliveries (repo_id, received_at DESC);
-- Old deliveries are pruned by age as new ones arrive.
CREATE INDEX IF NOT EXISTS github_deliveries_age_idx ON github_deliveries (received_at);

CREATE TABLE IF NOT EXISTS github_events (
    id              uuid PRIMARY KEY,
    site_id         uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    kind            text NOT NULL CHECK (kind IN ('deploy', 'release', 'push')),
    -- GitHub's id for the object: the deployment id, the release id, or the
    -- head commit of a push. With `site_id` and `kind` it is the dedupe key.
    external_id     text NOT NULL,
    repo_full_name  text NOT NULL,
    -- The branch or tag.
    git_ref         text,
    sha             text,
    -- One line: the release name, the head commit's subject, the deployment's
    -- description.
    title           text NOT NULL,
    environment     text,
    -- The GitHub login that caused it.
    actor           text,
    -- The object on GitHub: the release, the compare view of a push, the
    -- deployed commit.
    url             text NOT NULL CHECK (url ~ '^https://'),
    -- A push's commits, newest last, at most twenty: `[{sha, message, url, author}]`.
    commits         jsonb NOT NULL DEFAULT '[]'::jsonb,
    occurred_at     timestamptz NOT NULL,
    -- The annotation this event wrote, while it exists. Deleting the note
    -- leaves the event, so a redelivery does not write the note again.
    annotation_id   uuid REFERENCES annotations (id) ON DELETE SET NULL,
    delivery_id     text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (site_id, kind, external_id)
);

-- The settings screen, the AI analyst and the MCP tool read one site's events
-- over a window, newest first.
CREATE INDEX IF NOT EXISTS github_events_site_idx ON github_events (site_id, occurred_at DESC);
