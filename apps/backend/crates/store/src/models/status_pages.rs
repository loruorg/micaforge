//! `status_pages`, `status_page_monitors`, `status_incidents` and
//! `status_incident_updates`.
//!
//! A status page publishes chosen uptime monitors of one site at
//! `/status/<slug>`, read with no account. Two things write its incidents: a
//! person on the dashboard, and the uptime job when a monitor on the page goes
//! down or comes back ([`auto_open`], [`auto_resolve`]). Both go through the
//! same tables, so the public page has one timeline whoever wrote it.
//!
//! The status of an incident is always the status of its newest update.
//! [`add_update`] writes both in one transaction, which is what keeps the two
//! from disagreeing.

use chrono::{DateTime, Utc};
use micaforge_core::{new_id, Result};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// A public status page.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StatusPage {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// Site, by UUID.
    pub site_id: Uuid,
    /// The public address, `/status/<slug>`.
    pub slug: String,
    /// Heading of the page.
    pub title: String,
    /// A paragraph under the heading.
    pub description: String,
    /// An `https` image URL.
    pub logo_url: Option<String>,
    /// `#rrggbb`.
    pub accent_color: String,
    /// The footer link's text.
    pub footer_label: Option<String>,
    /// The footer link's address.
    pub footer_url: Option<String>,
    /// `false` takes the page off the public address without deleting it.
    pub published: bool,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
}

/// The fields an operator sets on a page.
#[derive(Debug, Clone)]
pub struct PageFields {
    /// The public address.
    pub slug: String,
    /// Heading.
    pub title: String,
    /// Paragraph under the heading.
    pub description: String,
    /// Logo URL.
    pub logo_url: Option<String>,
    /// Accent colour.
    pub accent_color: String,
    /// Footer link text.
    pub footer_label: Option<String>,
    /// Footer link address.
    pub footer_url: Option<String>,
    /// Whether the public address answers.
    pub published: bool,
    /// The monitors shown, in order, with an optional public name.
    pub monitors: Vec<(Uuid, Option<String>)>,
}

/// One monitor as a page shows it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PageMonitor {
    /// The page.
    pub page_id: Uuid,
    /// The monitor.
    pub monitor_id: Uuid,
    /// Order on the page.
    pub position: i32,
    /// The public name, when the operator set one.
    pub display_name: Option<String>,
    /// The monitor's own name.
    pub name: String,
    /// The monitor's current state.
    pub state: String,
    /// Whether the monitor is being checked.
    pub enabled: bool,
}

impl PageMonitor {
    /// The name the public reads.
    pub fn public_name(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(&self.name)
    }
}

/// An incident or maintenance window on a page.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StatusIncident {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The page.
    pub page_id: Uuid,
    /// `incident` | `maintenance`.
    pub kind: String,
    /// One line.
    pub title: String,
    /// The status of the newest update.
    pub status: String,
    /// `none` | `minor` | `major` | `critical`.
    pub impact: String,
    /// The uptime incident that opened it, for an automatic incident.
    pub uptime_incident_id: Option<Uuid>,
    /// `true` when the uptime job opened it.
    pub automatic: bool,
    /// The monitors it concerns, as a JSON array of ids.
    pub monitor_ids: Json,
    /// Start of a maintenance window.
    pub scheduled_start: Option<DateTime<Utc>>,
    /// End of a maintenance window.
    pub scheduled_end: Option<DateTime<Utc>>,
    /// When it was reported.
    pub created_at: DateTime<Utc>,
    /// When it was resolved or completed.
    pub resolved_at: Option<DateTime<Utc>>,
}

/// One entry on an incident's timeline.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IncidentUpdate {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The incident.
    pub incident_id: Uuid,
    /// The status this update set.
    pub status: String,
    /// What was said.
    pub body: String,
    /// `true` when the uptime job wrote it.
    pub automatic: bool,
    /// When.
    pub created_at: DateTime<Utc>,
}

/// A new incident, with the first entry of its timeline.
#[derive(Debug, Clone)]
pub struct NewIncident<'a> {
    /// The page.
    pub page_id: Uuid,
    /// `incident` | `maintenance`.
    pub kind: &'a str,
    /// One line.
    pub title: &'a str,
    /// Initial status.
    pub status: &'a str,
    /// Impact.
    pub impact: &'a str,
    /// The first update's text.
    pub body: &'a str,
    /// Affected monitors.
    pub monitor_ids: Json,
    /// Maintenance window start.
    pub scheduled_start: Option<DateTime<Utc>>,
    /// Maintenance window end.
    pub scheduled_end: Option<DateTime<Utc>>,
    /// The uptime incident behind an automatic incident.
    pub uptime_incident_id: Option<Uuid>,
    /// Whether the uptime job wrote it.
    pub automatic: bool,
}

/// The statuses that end an incident.
pub const CLOSING_STATUSES: &[&str] = &["resolved", "completed"];

const PAGE_COLS: &str = "id, site_id, slug, title, description, logo_url, accent_color, \
     footer_label, footer_url, published, created_at";
const INCIDENT_COLS: &str = "id, page_id, kind, title, status, impact, uptime_incident_id, \
     automatic, monitor_ids, scheduled_start, scheduled_end, created_at, resolved_at";
const UPDATE_COLS: &str = "id, incident_id, status, body, automatic, created_at";
const PAGE_MONITOR_SELECT: &str = "SELECT pm.page_id, pm.monitor_id, pm.position, \
     pm.display_name, m.name, m.state, m.enabled \
     FROM status_page_monitors pm JOIN uptime_monitors m ON m.id = pm.monitor_id";

// ---------------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------------

/// Create a page and its monitor list, in one transaction.
pub async fn create_page(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    f: &PageFields,
) -> Result<StatusPage> {
    let mut tx = pool.begin().await?;
    let page = sqlx::query_as::<_, StatusPage>(&format!(
        "INSERT INTO status_pages (id, site_id, slug, title, description, logo_url,
             accent_color, footer_label, footer_url, published)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING {PAGE_COLS}"
    ))
    .bind(id)
    .bind(site_id)
    .bind(&f.slug)
    .bind(&f.title)
    .bind(&f.description)
    .bind(&f.logo_url)
    .bind(&f.accent_color)
    .bind(&f.footer_label)
    .bind(&f.footer_url)
    .bind(f.published)
    .fetch_one(&mut *tx)
    .await?;
    write_monitors(&mut tx, id, &f.monitors).await?;
    tx.commit().await?;
    Ok(page)
}

/// Replace a page's fields and its monitor list, in one transaction.
pub async fn update_page(pool: &PgPool, id: Uuid, f: &PageFields) -> Result<StatusPage> {
    let mut tx = pool.begin().await?;
    let page = sqlx::query_as::<_, StatusPage>(&format!(
        "UPDATE status_pages SET slug = $2, title = $3, description = $4, logo_url = $5,
             accent_color = $6, footer_label = $7, footer_url = $8, published = $9
         WHERE id = $1 RETURNING {PAGE_COLS}"
    ))
    .bind(id)
    .bind(&f.slug)
    .bind(&f.title)
    .bind(&f.description)
    .bind(&f.logo_url)
    .bind(&f.accent_color)
    .bind(&f.footer_label)
    .bind(&f.footer_url)
    .bind(f.published)
    .fetch_optional(&mut *tx)
    .await?;
    let page = or_missing(page, "status page")?;
    sqlx::query("DELETE FROM status_page_monitors WHERE page_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    write_monitors(&mut tx, id, &f.monitors).await?;
    tx.commit().await?;
    Ok(page)
}

/// Insert a page's monitor rows in the order given.
async fn write_monitors(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    page_id: Uuid,
    monitors: &[(Uuid, Option<String>)],
) -> Result<()> {
    for (position, (monitor_id, display_name)) in monitors.iter().enumerate() {
        sqlx::query(
            "INSERT INTO status_page_monitors (page_id, monitor_id, position, display_name)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(page_id)
        .bind(monitor_id)
        .bind(i32::try_from(position).unwrap_or(i32::MAX))
        .bind(display_name)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Fetch by primary key.
pub async fn get_page(pool: &PgPool, id: Uuid) -> Result<StatusPage> {
    let row = sqlx::query_as::<_, StatusPage>(&format!(
        "SELECT {PAGE_COLS} FROM status_pages WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "status page")
}

/// Resolve the public address. An unpublished page reads as absent.
pub async fn get_published_by_slug(pool: &PgPool, slug: &str) -> Result<StatusPage> {
    let row = sqlx::query_as::<_, StatusPage>(&format!(
        "SELECT {PAGE_COLS} FROM status_pages WHERE slug = $1 AND published"
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "status page")
}

/// Every page of a site.
pub async fn list_pages(pool: &PgPool, site_id: Uuid) -> Result<Vec<StatusPage>> {
    let rows = sqlx::query_as::<_, StatusPage>(&format!(
        "SELECT {PAGE_COLS} FROM status_pages WHERE site_id = $1 ORDER BY created_at, id"
    ))
    .bind(site_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Delete a page with its monitors list and incidents.
pub async fn delete_page(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM status_pages WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The monitors of several pages, in page order.
pub async fn monitors_of(pool: &PgPool, page_ids: &[Uuid]) -> Result<Vec<PageMonitor>> {
    if page_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, PageMonitor>(&format!(
        "{PAGE_MONITOR_SELECT} WHERE pm.page_id = ANY($1) ORDER BY pm.page_id, pm.position"
    ))
    .bind(page_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Incidents
// ---------------------------------------------------------------------------

/// Create an incident and its first update, in one transaction.
pub async fn create_incident(pool: &PgPool, n: &NewIncident<'_>) -> Result<StatusIncident> {
    let mut tx = pool.begin().await?;
    let incident = insert_incident(&mut tx, n).await?;
    tx.commit().await?;
    Ok(incident)
}

async fn insert_incident(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    n: &NewIncident<'_>,
) -> Result<StatusIncident> {
    let closed = CLOSING_STATUSES.contains(&n.status);
    let incident = sqlx::query_as::<_, StatusIncident>(&format!(
        "INSERT INTO status_incidents (id, page_id, kind, title, status, impact,
             uptime_incident_id, automatic, monitor_ids, scheduled_start, scheduled_end,
             resolved_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 CASE WHEN $12 THEN now() ELSE NULL END)
         RETURNING {INCIDENT_COLS}"
    ))
    .bind(new_id())
    .bind(n.page_id)
    .bind(n.kind)
    .bind(n.title)
    .bind(n.status)
    .bind(n.impact)
    .bind(n.uptime_incident_id)
    .bind(n.automatic)
    .bind(&n.monitor_ids)
    .bind(n.scheduled_start)
    .bind(n.scheduled_end)
    .bind(closed)
    .fetch_one(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO status_incident_updates (id, incident_id, status, body, automatic)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(new_id())
    .bind(incident.id)
    .bind(n.status)
    .bind(n.body)
    .bind(n.automatic)
    .execute(&mut **tx)
    .await?;
    Ok(incident)
}

/// Fetch one incident.
pub async fn get_incident(pool: &PgPool, id: Uuid) -> Result<StatusIncident> {
    let row = sqlx::query_as::<_, StatusIncident>(&format!(
        "SELECT {INCIDENT_COLS} FROM status_incidents WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "incident")
}

/// A page's incidents, newest first.
pub async fn list_incidents(
    pool: &PgPool,
    page_id: Uuid,
    limit: i64,
) -> Result<Vec<StatusIncident>> {
    let rows = sqlx::query_as::<_, StatusIncident>(&format!(
        "SELECT {INCIDENT_COLS} FROM status_incidents WHERE page_id = $1
         ORDER BY created_at DESC, id DESC LIMIT $2"
    ))
    .bind(page_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The incidents a public page shows: every open one, and every one closed
/// since `since`.
pub async fn public_incidents(
    pool: &PgPool,
    page_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<StatusIncident>> {
    let rows = sqlx::query_as::<_, StatusIncident>(&format!(
        "SELECT {INCIDENT_COLS} FROM status_incidents
         WHERE page_id = $1 AND (resolved_at IS NULL OR resolved_at >= $2)
         ORDER BY created_at DESC, id DESC LIMIT 200"
    ))
    .bind(page_id)
    .bind(since)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The timelines of several incidents, newest first within each.
pub async fn updates_of(pool: &PgPool, incident_ids: &[Uuid]) -> Result<Vec<IncidentUpdate>> {
    if incident_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, IncidentUpdate>(&format!(
        "SELECT {UPDATE_COLS} FROM status_incident_updates WHERE incident_id = ANY($1)
         ORDER BY incident_id, created_at DESC, id DESC"
    ))
    .bind(incident_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Post an update, and move the incident to its status.
///
/// A closing status ([`CLOSING_STATUSES`]) stamps `resolved_at`; any other
/// clears it, so an incident reopened by a later update is open again.
pub async fn add_update(
    pool: &PgPool,
    incident_id: Uuid,
    status: &str,
    body: &str,
    automatic: bool,
) -> Result<StatusIncident> {
    let mut tx = pool.begin().await?;
    let incident = move_to(&mut tx, incident_id, status, body, automatic).await?;
    tx.commit().await?;
    Ok(incident)
}

async fn move_to(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    incident_id: Uuid,
    status: &str,
    body: &str,
    automatic: bool,
) -> Result<StatusIncident> {
    let closed = CLOSING_STATUSES.contains(&status);
    let incident = sqlx::query_as::<_, StatusIncident>(&format!(
        "UPDATE status_incidents SET status = $2,
             resolved_at = CASE WHEN $3 THEN coalesce(resolved_at, now()) ELSE NULL END
         WHERE id = $1 RETURNING {INCIDENT_COLS}"
    ))
    .bind(incident_id)
    .bind(status)
    .bind(closed)
    .fetch_optional(&mut **tx)
    .await?;
    let incident = or_missing(incident, "incident")?;
    sqlx::query(
        "INSERT INTO status_incident_updates (id, incident_id, status, body, automatic)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(new_id())
    .bind(incident_id)
    .bind(status)
    .bind(body)
    .bind(automatic)
    .execute(&mut **tx)
    .await?;
    Ok(incident)
}

/// The fields of an incident an operator may change after posting it.
#[derive(Debug, Clone)]
pub struct IncidentFields {
    /// One line.
    pub title: String,
    /// Impact.
    pub impact: String,
    /// Affected monitors.
    pub monitor_ids: Json,
    /// Maintenance window start.
    pub scheduled_start: Option<DateTime<Utc>>,
    /// Maintenance window end.
    pub scheduled_end: Option<DateTime<Utc>>,
}

/// Change an incident's title, impact, monitors or window.
pub async fn update_incident(
    pool: &PgPool,
    id: Uuid,
    f: &IncidentFields,
) -> Result<StatusIncident> {
    let row = sqlx::query_as::<_, StatusIncident>(&format!(
        "UPDATE status_incidents SET title = $2, impact = $3, monitor_ids = $4,
             scheduled_start = $5, scheduled_end = $6
         WHERE id = $1 RETURNING {INCIDENT_COLS}"
    ))
    .bind(id)
    .bind(&f.title)
    .bind(&f.impact)
    .bind(&f.monitor_ids)
    .bind(f.scheduled_start)
    .bind(f.scheduled_end)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "incident")
}

/// Delete an incident and its timeline.
pub async fn delete_incident(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM status_incidents WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Incidents the uptime job writes
// ---------------------------------------------------------------------------

/// Open an automatic incident on every published page that shows `monitor_id`.
///
/// `title` and `body` are built by the caller from the page's own name for
/// the monitor, so `display(name)` maps a page's public name to the text. A
/// page that already has an automatic incident for this uptime incident gets
/// no second one, which makes a retried sweep harmless. Returns how many
/// incidents were opened.
pub async fn auto_open(
    pool: &PgPool,
    monitor_id: Uuid,
    uptime_incident_id: Uuid,
    display: impl Fn(&str) -> (String, String),
) -> Result<usize> {
    let shown = sqlx::query_as::<_, PageMonitor>(&format!(
        "{PAGE_MONITOR_SELECT} JOIN status_pages p ON p.id = pm.page_id
         WHERE pm.monitor_id = $1 AND p.published"
    ))
    .bind(monitor_id)
    .fetch_all(pool)
    .await?;

    let mut opened = 0;
    for entry in shown {
        let mut tx = pool.begin().await?;
        let (exists,): (bool,) = sqlx::query_as(
            "SELECT EXISTS (SELECT 1 FROM status_incidents
                             WHERE page_id = $1 AND uptime_incident_id = $2)",
        )
        .bind(entry.page_id)
        .bind(uptime_incident_id)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            continue;
        }
        let (title, body) = display(entry.public_name());
        insert_incident(
            &mut tx,
            &NewIncident {
                page_id: entry.page_id,
                kind: "incident",
                title: &title,
                status: "investigating",
                impact: "major",
                body: &body,
                monitor_ids: Json::Array(vec![Json::String(monitor_id.to_string())]),
                scheduled_start: None,
                scheduled_end: None,
                uptime_incident_id: Some(uptime_incident_id),
                automatic: true,
            },
        )
        .await?;
        tx.commit().await?;
        opened += 1;
    }
    Ok(opened)
}

/// Resolve every still-open automatic incident opened for `uptime_incident_id`.
///
/// An operator who already resolved one by hand keeps their own words: only
/// incidents still open get the automatic update. Returns how many closed.
pub async fn auto_resolve(pool: &PgPool, uptime_incident_id: Uuid, body: &str) -> Result<usize> {
    let open: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM status_incidents
         WHERE uptime_incident_id = $1 AND resolved_at IS NULL",
    )
    .bind(uptime_incident_id)
    .fetch_all(pool)
    .await?;

    for (id,) in &open {
        add_update(pool, *id, "resolved", body, true).await?;
    }
    Ok(open.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_public_name_falls_back_to_the_monitor_name() {
        let mut m = PageMonitor {
            page_id: Uuid::nil(),
            monitor_id: Uuid::nil(),
            position: 0,
            display_name: None,
            name: "api.example.com health".into(),
            state: "up".into(),
            enabled: true,
        };
        assert_eq!(m.public_name(), "api.example.com health");
        m.display_name = Some("  ".into());
        assert_eq!(m.public_name(), "api.example.com health");
        m.display_name = Some("API".into());
        assert_eq!(m.public_name(), "API");
    }
}
