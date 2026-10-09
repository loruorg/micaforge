-- Micaforge: chat channels linked to sites.
--
-- The Slack, Discord and Telegram apps are one per install, configured by the
-- operator through the environment. What belongs to an organisation is the
-- link: this Slack channel, this Discord channel, this Telegram chat answers
-- questions about this site. A link is made with a one-time code generated in
-- the dashboard by a member of the site's organisation, so possession of the
-- chat channel alone never grants a read of anybody's analytics.
--
-- One channel answers for one site at a time: the unique key is the channel,
-- so linking it again moves it rather than making the answer ambiguous.

CREATE TABLE IF NOT EXISTS chat_links (
    id            uuid PRIMARY KEY,
    org_id        uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    site_id       uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    platform      text NOT NULL CHECK (platform IN ('slack', 'discord', 'telegram')),
    -- The Slack team id or the Discord guild id. Empty for Telegram, whose chat
    -- ids are global, and for a Discord DM.
    workspace_id  text NOT NULL DEFAULT '',
    channel_id    text NOT NULL,
    -- What the settings screen shows: the channel's name when the platform sent
    -- one, otherwise its id.
    label         text NOT NULL DEFAULT '',
    created_by    uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (platform, workspace_id, channel_id)
);

CREATE INDEX IF NOT EXISTS chat_links_site_idx ON chat_links (site_id);

-- Where an investigation was posted in a linked channel, so an update lands
-- as a reply in the same thread instead of as a new message every time.
CREATE TABLE IF NOT EXISTS chat_investigation_posts (
    investigation_id  uuid NOT NULL REFERENCES investigations (id) ON DELETE CASCADE,
    link_id           uuid NOT NULL REFERENCES chat_links (id) ON DELETE CASCADE,
    -- The Slack message `ts`, the Discord message id, or the Telegram
    -- message id of the first post.
    thread_ref        text NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (investigation_id, link_id)
);
