//! `feedback` — bug reports and requests from people and agents.
//!
//! Who may read a row is decided in the server (`routes::feedback`), not
//! here: an instance admin reads everything, a reporter reads their own. This
//! module offers exactly the reads those two rules need and no general query,
//! so there is no "list every report of this organisation" for a route to
//! reach for by mistake.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

/// One report, with the names its references resolve to.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Feedback {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// `bug` | `problem` | `feature` | `other`.
    pub kind: String,
    /// One line.
    pub title: String,
    /// The details.
    pub body: String,
    /// `new` | `triaged` | `planned` | `done` | `wont_fix`.
    pub status: String,
    /// `low` | `medium` | `high` | `critical`, when somebody set one.
    pub severity: Option<String>,
    /// `dashboard` | `mcp` | `api` | `chat`.
    pub source: String,
    /// The reporting account, while it exists.
    pub reporter_user_id: Option<Uuid>,
    /// The reporter's address at the time.
    pub reporter_email: Option<String>,
    /// The reporter's name at the time.
    pub reporter_name: Option<String>,
    /// The MCP client's own name.
    pub client_name: Option<String>,
    /// The API key an agent used.
    pub api_key_id: Option<Uuid>,
    /// The OAuth grant an MCP client used.
    pub oauth_grant_id: Option<Uuid>,
    /// Platform and sender, for a chat report.
    pub chat_reporter: Option<String>,
    /// The organisation the report came from.
    pub org_id: Option<Uuid>,
    /// Its name, resolved.
    pub org_name: Option<String>,
    /// The site the reporter was on, by UUID.
    pub site_id: Option<Uuid>,
    /// Its numeric id, resolved.
    pub site_number: Option<i32>,
    /// Its domain, resolved.
    pub site_domain: Option<String>,
    /// The page the report was sent from.
    pub page_url: Option<String>,
    /// The client's version.
    pub app_version: Option<String>,
    /// The client's user agent.
    pub user_agent: Option<String>,
    /// When it arrived.
    pub created_at: DateTime<Utc>,
    /// When its status or severity last changed.
    pub updated_at: DateTime<Utc>,
    /// The GitHub issue it was opened as, when it was.
    pub github_issue_url: Option<String>,
    /// That issue's number in its repository.
    pub github_issue_number: Option<i32>,
    /// That repository, `owner/name`, as it was named then.
    pub github_issue_repo: Option<String>,
}

/// Everything a new report carries.
#[derive(Debug, Clone, Default)]
pub struct NewFeedback<'a> {
    /// `bug` | `problem` | `feature` | `other`.
    pub kind: &'a str,
    /// One line.
    pub title: &'a str,
    /// The details.
    pub body: &'a str,
    /// Optional severity.
    pub severity: Option<&'a str>,
    /// `dashboard` | `mcp` | `api` | `chat`.
    pub source: &'a str,
    /// The reporting account.
    pub reporter_user_id: Option<Uuid>,
    /// Its address.
    pub reporter_email: Option<&'a str>,
    /// Its name.
    pub reporter_name: Option<&'a str>,
    /// The MCP client's name.
    pub client_name: Option<&'a str>,
    /// The API key used.
    pub api_key_id: Option<Uuid>,
    /// The OAuth grant used.
    pub oauth_grant_id: Option<Uuid>,
    /// Platform and sender of a chat report.
    pub chat_reporter: Option<&'a str>,
    /// The organisation.
    pub org_id: Option<Uuid>,
    /// The site.
    pub site_id: Option<Uuid>,
    /// The page.
    pub page_url: Option<&'a str>,
    /// The client version.
    pub app_version: Option<&'a str>,
    /// The user agent.
    pub user_agent: Option<&'a str>,
}

/// The triage filters. `None` is "any".
#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    /// One status.
    pub status: Option<&'a str>,
    /// One kind.
    pub kind: Option<&'a str>,
    /// One source.
    pub source: Option<&'a str>,
}

const SELECT: &str = "SELECT f.id, f.kind, f.title, f.body, f.status, f.severity, f.source, \
    f.reporter_user_id, f.reporter_email, f.reporter_name, f.client_name, f.api_key_id, \
    f.oauth_grant_id, f.chat_reporter, f.org_id, o.name AS org_name, f.site_id, \
    s.site_id AS site_number, s.domain AS site_domain, f.page_url, f.app_version, \
    f.user_agent, f.created_at, f.updated_at, f.github_issue_url, f.github_issue_number, \
    f.github_issue_repo \
    FROM feedback f \
    LEFT JOIN organizations o ON o.id = f.org_id \
    LEFT JOIN sites s ON s.id = f.site_id";

/// Store a report.
pub async fn create(pool: &PgPool, id: Uuid, new: &NewFeedback<'_>) -> Result<Feedback> {
    sqlx::query(
        "INSERT INTO feedback (id, kind, title, body, severity, source, reporter_user_id,
             reporter_email, reporter_name, client_name, api_key_id, oauth_grant_id,
             chat_reporter, org_id, site_id, page_url, app_version, user_agent)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
    )
    .bind(id)
    .bind(new.kind)
    .bind(new.title)
    .bind(new.body)
    .bind(new.severity)
    .bind(new.source)
    .bind(new.reporter_user_id)
    .bind(new.reporter_email)
    .bind(new.reporter_name)
    .bind(new.client_name)
    .bind(new.api_key_id)
    .bind(new.oauth_grant_id)
    .bind(new.chat_reporter)
    .bind(new.org_id)
    .bind(new.site_id)
    .bind(new.page_url)
    .bind(new.app_version)
    .bind(new.user_agent)
    .execute(pool)
    .await?;
    get(pool, id).await
}

/// Fetch by primary key.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Feedback> {
    let row = sqlx::query_as::<_, Feedback>(&format!("{SELECT} WHERE f.id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await?;
    or_missing(row, "feedback")
}

/// A person's own reports, newest first.
pub async fn for_user(pool: &PgPool, user_id: Uuid, limit: i64) -> Result<Vec<Feedback>> {
    let rows = sqlx::query_as::<_, Feedback>(&format!(
        "{SELECT} WHERE f.reporter_user_id = $1 AND f.api_key_id IS NULL
         ORDER BY f.created_at DESC LIMIT $2"
    ))
    .bind(user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The reports one API key sent, newest first.
pub async fn for_api_key(pool: &PgPool, key_id: Uuid, limit: i64) -> Result<Vec<Feedback>> {
    let rows = sqlx::query_as::<_, Feedback>(&format!(
        "{SELECT} WHERE f.api_key_id = $1 ORDER BY f.created_at DESC LIMIT $2"
    ))
    .bind(key_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// The triage list, newest first, with the total that matches.
pub async fn list(
    pool: &PgPool,
    filter: &Filter<'_>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Feedback>, i64)> {
    let rows = sqlx::query_as::<_, Feedback>(&format!(
        "{SELECT} WHERE ($1::text IS NULL OR f.status = $1)
           AND ($2::text IS NULL OR f.kind = $2)
           AND ($3::text IS NULL OR f.source = $3)
         ORDER BY f.created_at DESC LIMIT $4 OFFSET $5"
    ))
    .bind(filter.status)
    .bind(filter.kind)
    .bind(filter.source)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM feedback
         WHERE ($1::text IS NULL OR status = $1)
           AND ($2::text IS NULL OR kind = $2)
           AND ($3::text IS NULL OR source = $3)",
    )
    .bind(filter.status)
    .bind(filter.kind)
    .bind(filter.source)
    .fetch_one(pool)
    .await?;
    Ok((rows, total))
}

/// Change a report's status and severity.
///
/// `severity` of `Some(None)` clears it; `None` leaves it as it is.
pub async fn update(
    pool: &PgPool,
    id: Uuid,
    status: Option<&str>,
    severity: Option<Option<&str>>,
) -> Result<Feedback> {
    let done = sqlx::query(
        "UPDATE feedback SET
             status = COALESCE($2, status),
             severity = CASE WHEN $3 THEN $4 ELSE severity END,
             updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(severity.is_some())
    .bind(severity.flatten())
    .execute(pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(micaforge_core::Error::not_found("feedback"));
    }
    get(pool, id).await
}

/// Remember the GitHub issue a report was opened as.
///
/// Only a report that has none yet takes one: `false` means another request
/// got there first, and its issue is the one the report keeps.
pub async fn set_github_issue(
    pool: &PgPool,
    id: Uuid,
    url: &str,
    number: i32,
    repo: &str,
) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE feedback SET github_issue_url = $2, github_issue_number = $3,
             github_issue_repo = $4, updated_at = now()
         WHERE id = $1 AND github_issue_url IS NULL",
    )
    .bind(id)
    .bind(url)
    .bind(number)
    .bind(repo)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}
