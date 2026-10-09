-- Micaforge: feedback from people and from agents.
--
-- Anyone who can sign in can report a bug or ask for a feature, and so can an
-- agent working through the MCP server or the API. The reporter is recorded
-- as precisely as the credential allows: a person by account, an agent by the
-- MCP client's own name and the API key or OAuth grant it used, a chat message
-- by its platform and sender. Triage is for instance admins only; a reporter
-- can read back what they sent and its status.
--
-- Nothing cascades from the organisation or the site: a report about a site
-- that was later deleted is still a report the operator should read, so those
-- references are cleared rather than taking the row with them. Deleting the
-- reporter's account clears the reporter in the same way.

CREATE TABLE IF NOT EXISTS feedback (
    id                uuid PRIMARY KEY,
    kind              text NOT NULL CHECK (kind IN ('bug', 'problem', 'feature', 'other')),
    title             text NOT NULL,
    body              text NOT NULL,
    status            text NOT NULL DEFAULT 'new'
                      CHECK (status IN ('new', 'triaged', 'planned', 'done', 'wont_fix')),
    severity          text CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    source            text NOT NULL CHECK (source IN ('dashboard', 'mcp', 'api', 'chat')),
    reporter_user_id  uuid REFERENCES users (id) ON DELETE SET NULL,
    -- Copied at write time so a report keeps its author's address after the
    -- account is gone, which is when an operator most needs to reply to it.
    reporter_email    text,
    reporter_name     text,
    -- The MCP client's `clientInfo.name`, when an agent reported through MCP.
    client_name       text,
    -- The API key an agent used, kept as a bare id rather than a foreign key
    -- so revoking the key does not erase who said what.
    api_key_id        uuid,
    -- The OAuth grant an MCP client used, likewise.
    oauth_grant_id    uuid,
    -- `slack:T123/C456 @ada` and the like, for a report sent from a chat app.
    chat_reporter     text,
    org_id            uuid REFERENCES organizations (id) ON DELETE SET NULL,
    site_id           uuid REFERENCES sites (id) ON DELETE SET NULL,
    page_url          text,
    app_version       text,
    user_agent        text,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS feedback_triage_idx ON feedback (status, created_at DESC);
CREATE INDEX IF NOT EXISTS feedback_reporter_idx ON feedback (reporter_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS feedback_key_idx ON feedback (api_key_id, created_at DESC)
    WHERE api_key_id IS NOT NULL;
