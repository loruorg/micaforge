-- Micaforge: public status pages, their incidents, and the incident timeline.
--
-- A status page publishes a chosen set of a site's uptime monitors at
-- `/status/<slug>` with no account. Its incidents are written by hand from the
-- dashboard, or opened by the uptime job when a monitor on the page goes down.
--
-- Additive only.

CREATE TABLE IF NOT EXISTS status_pages (
    id            uuid PRIMARY KEY,
    site_id       uuid NOT NULL REFERENCES sites (id) ON DELETE CASCADE,
    -- The public address. Unique on the install, because it is one path space.
    slug          text NOT NULL UNIQUE,
    title         text NOT NULL,
    description   text NOT NULL DEFAULT '',
    logo_url      text,
    accent_color  text NOT NULL DEFAULT '#2f6bff' CHECK (accent_color ~ '^#[0-9a-fA-F]{6}$'),
    footer_label  text,
    footer_url    text,
    published     boolean NOT NULL DEFAULT true,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((footer_label IS NULL) = (footer_url IS NULL))
);

CREATE INDEX IF NOT EXISTS status_pages_site_idx ON status_pages (site_id);

CREATE TABLE IF NOT EXISTS status_page_monitors (
    page_id       uuid NOT NULL REFERENCES status_pages (id) ON DELETE CASCADE,
    monitor_id    uuid NOT NULL REFERENCES uptime_monitors (id) ON DELETE CASCADE,
    position      integer NOT NULL DEFAULT 0,
    -- The name the public reads. NULL shows the monitor's own name.
    display_name  text,
    PRIMARY KEY (page_id, monitor_id)
);

-- The uptime job asks "which pages show this monitor" on every incident.
CREATE INDEX IF NOT EXISTS status_page_monitors_monitor_idx ON status_page_monitors (monitor_id);

CREATE TABLE IF NOT EXISTS status_incidents (
    id                  uuid PRIMARY KEY,
    page_id             uuid NOT NULL REFERENCES status_pages (id) ON DELETE CASCADE,
    kind                text NOT NULL DEFAULT 'incident' CHECK (kind IN ('incident', 'maintenance')),
    title               text NOT NULL,
    status              text NOT NULL CHECK (
                            status IN ('investigating', 'identified', 'monitoring', 'resolved',
                                       'scheduled', 'in_progress', 'completed')
                        ),
    impact              text NOT NULL DEFAULT 'minor' CHECK (impact IN ('none', 'minor', 'major', 'critical')),
    -- Set when the uptime job opened this incident; SET NULL keeps the public
    -- history readable after the monitor and its incidents are deleted.
    uptime_incident_id  uuid REFERENCES uptime_incidents (id) ON DELETE SET NULL,
    automatic           boolean NOT NULL DEFAULT false,
    -- The monitors this incident is about. Empty means the whole page.
    monitor_ids         jsonb NOT NULL DEFAULT '[]'::jsonb,
    scheduled_start     timestamptz,
    scheduled_end       timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now(),
    resolved_at         timestamptz
);

CREATE INDEX IF NOT EXISTS status_incidents_page_idx ON status_incidents (page_id, created_at DESC);
CREATE INDEX IF NOT EXISTS status_incidents_uptime_idx ON status_incidents (uptime_incident_id)
    WHERE uptime_incident_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS status_incident_updates (
    id           uuid PRIMARY KEY,
    incident_id  uuid NOT NULL REFERENCES status_incidents (id) ON DELETE CASCADE,
    status       text NOT NULL CHECK (
                     status IN ('investigating', 'identified', 'monitoring', 'resolved',
                                'scheduled', 'in_progress', 'completed')
                 ),
    body         text NOT NULL,
    automatic    boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS status_incident_updates_incident_idx
    ON status_incident_updates (incident_id, created_at DESC);
