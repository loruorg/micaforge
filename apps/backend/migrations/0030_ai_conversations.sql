-- Micaforge: conversations with the AI analyst.
--
-- One conversation belongs to one person on one site. It is stored so a
-- question can be followed by another ("and last month?") with the earlier
-- answers in the model's context, and so the dashboard can show yesterday's
-- answer with the queries that produced it. The site is part of the key
-- because every tool the model may call is bounded to that site: a
-- conversation that could wander between sites would carry one site's numbers
-- into a question about another.
--
-- Deleting the site or the account takes its conversations with it. Neither
-- side is meaningful alone.

CREATE TABLE IF NOT EXISTS ai_conversations (
    id          uuid PRIMARY KEY,
    site_id     uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    user_id     uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The first question, shortened. Written once, when the conversation opens.
    title       text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- `ai::list_conversations` reads one person's conversations on one site, newest
-- first.
CREATE INDEX IF NOT EXISTS ai_conversations_owner_idx
    ON ai_conversations (user_id, site_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS ai_messages (
    id               uuid PRIMARY KEY,
    conversation_id  uuid NOT NULL REFERENCES ai_conversations (id) ON DELETE CASCADE,
    role             text NOT NULL CHECK (role IN ('user', 'assistant')),
    content          text NOT NULL,
    -- Every tool the model ran for this answer: its arguments, the equivalent
    -- stats request and the SQL each read executed. This is what the dashboard
    -- shows as "the queries behind this answer", so it is stored with the
    -- answer rather than recomputed: the data may have moved since.
    tool_calls       jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- Set when the answer failed. The question is still stored, so the thread
    -- reads honestly: a question, and the reason it went unanswered.
    error            text,
    created_at       timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ai_messages_conversation_idx
    ON ai_messages (conversation_id, created_at);
