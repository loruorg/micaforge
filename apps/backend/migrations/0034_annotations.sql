-- Micaforge: annotations people write on a site's charts.
--
-- The overview chart already writes its own notes (the steepest rise, a peak
-- off the scale), computed from the series and never stored. These are the
-- other kind: "launched the pricing page", "outage 14:00 to 15:30", written by
-- a person, by a script through the API, or by an agent through MCP, and kept.
-- An annotation is a moment (`ends_at` NULL) or a range.
--
-- `color` is a small closed set on purpose. Blue means people and orange means
-- AI everywhere in this product, so neither may be borrowed by a note.

CREATE TABLE IF NOT EXISTS annotations (
    id          uuid PRIMARY KEY,
    site_id     uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    starts_at   timestamptz NOT NULL,
    ends_at     timestamptz,
    text        text NOT NULL,
    color       text NOT NULL DEFAULT 'neutral' CHECK (color IN ('neutral', 'green', 'red')),
    category    text,
    source      text NOT NULL DEFAULT 'dashboard' CHECK (source IN ('dashboard', 'api', 'mcp')),
    author_id   uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at IS NULL OR ends_at >= starts_at)
);

-- `annotations::list_in` reads one site's annotations overlapping a window.
CREATE INDEX IF NOT EXISTS annotations_site_idx ON annotations (site_id, starts_at);
