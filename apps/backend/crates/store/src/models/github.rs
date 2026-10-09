//! `github_*` — the GitHub App: installations, repository links, webhook
//! deliveries, and the deploys, releases and pushes recorded from them.
//!
//! Who may change what is decided in the server (`routes::github`); this
//! module holds the four tables of migration 0050 and one rule of its own:
//! **an event is recorded once per site, and its annotation with it, in one
//! transaction.** [`record_event`] inserts the event under its unique key
//! `(site_id, kind, external_id)` and writes the annotation only when that
//! insert took. A redelivery, a second delivery for the same deployment, or
//! two deliveries racing each other therefore leave exactly one event and at
//! most one note, whatever the delivery table says.

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use serde_json::Value as Json;
use uuid::Uuid;

use crate::models::or_missing;
use crate::pg::PgPool;

// ---------------------------------------------------------------------------
// Installations
// ---------------------------------------------------------------------------

/// One GitHub installation an organisation proved it can reach.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Installation {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The organisation it belongs to.
    pub org_id: Uuid,
    /// GitHub's installation id.
    pub installation_id: i64,
    /// The GitHub account the App is installed on.
    pub account_login: String,
    /// `User` or `Organization`.
    pub account_type: String,
    /// `all` or `selected`.
    pub repository_selection: Option<String>,
    /// Set while the account has the App suspended.
    pub suspended_at: Option<DateTime<Utc>>,
    /// Who connected it, while the account exists.
    pub installed_by: Option<Uuid>,
    /// When it was first connected.
    pub created_at: DateTime<Utc>,
    /// When GitHub last told us something about it.
    pub updated_at: DateTime<Utc>,
}

/// What GitHub says about an installation.
#[derive(Debug, Clone)]
pub struct InstallationFields<'a> {
    /// GitHub's installation id.
    pub installation_id: i64,
    /// The account's login.
    pub account_login: &'a str,
    /// `User` or `Organization`.
    pub account_type: &'a str,
    /// `all` or `selected`.
    pub repository_selection: Option<&'a str>,
    /// When the account suspended the App, if it has.
    pub suspended_at: Option<DateTime<Utc>>,
}

const INSTALLATION_COLUMNS: &str = "id, org_id, installation_id, account_login, account_type, \
    repository_selection, suspended_at, installed_by, created_at, updated_at";

/// Record an installation for an organisation, or refresh the one it has.
pub async fn upsert_installation(
    pool: &PgPool,
    id: Uuid,
    org_id: Uuid,
    fields: &InstallationFields<'_>,
    installed_by: Option<Uuid>,
) -> Result<Installation> {
    let row = sqlx::query_as::<_, Installation>(&format!(
        "INSERT INTO github_installations (id, org_id, installation_id, account_login,
             account_type, repository_selection, suspended_at, installed_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (org_id, installation_id) DO UPDATE SET
             account_login = EXCLUDED.account_login,
             account_type = EXCLUDED.account_type,
             repository_selection = EXCLUDED.repository_selection,
             suspended_at = EXCLUDED.suspended_at,
             updated_at = now()
         RETURNING {INSTALLATION_COLUMNS}"
    ))
    .bind(id)
    .bind(org_id)
    .bind(fields.installation_id)
    .bind(fields.account_login)
    .bind(fields.account_type)
    .bind(fields.repository_selection)
    .bind(fields.suspended_at)
    .bind(installed_by)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Fetch by primary key.
pub async fn installation(pool: &PgPool, id: Uuid) -> Result<Installation> {
    let row = sqlx::query_as::<_, Installation>(&format!(
        "SELECT {INSTALLATION_COLUMNS} FROM github_installations WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    or_missing(row, "GitHub installation")
}

/// An organisation's installations, oldest first.
pub async fn installations_for_org(pool: &PgPool, org_id: Uuid) -> Result<Vec<Installation>> {
    let rows = sqlx::query_as::<_, Installation>(&format!(
        "SELECT {INSTALLATION_COLUMNS} FROM github_installations
         WHERE org_id = $1 ORDER BY created_at, id"
    ))
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Forget one organisation's installation, and with it every repository link
/// made through it.
pub async fn delete_installation(pool: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM github_installations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Forget an installation in every organisation, because GitHub says it is
/// gone. Returns the rows removed, for the audit log.
pub async fn delete_by_github_id(pool: &PgPool, installation_id: i64) -> Result<Vec<Installation>> {
    let rows = sqlx::query_as::<_, Installation>(&format!(
        "DELETE FROM github_installations WHERE installation_id = $1
         RETURNING {INSTALLATION_COLUMNS}"
    ))
    .bind(installation_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Mark an installation suspended (`Some`) or active again (`None`) in every
/// organisation. Returns the rows changed.
pub async fn set_suspended(
    pool: &PgPool,
    installation_id: i64,
    at: Option<DateTime<Utc>>,
) -> Result<Vec<Installation>> {
    let rows = sqlx::query_as::<_, Installation>(&format!(
        "UPDATE github_installations SET suspended_at = $2, updated_at = now()
         WHERE installation_id = $1
         RETURNING {INSTALLATION_COLUMNS}"
    ))
    .bind(installation_id)
    .bind(at)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Keep an installation's account details current in every organisation that
/// holds it, from a delivery. Its suspension is left to [`set_suspended`].
pub async fn refresh_installation(pool: &PgPool, fields: &InstallationFields<'_>) -> Result<()> {
    sqlx::query(
        "UPDATE github_installations SET account_login = $2, account_type = $3,
             repository_selection = COALESCE($4, repository_selection), updated_at = now()
         WHERE installation_id = $1",
    )
    .bind(fields.installation_id)
    .bind(fields.account_login)
    .bind(fields.account_type)
    .bind(fields.repository_selection)
    .execute(pool)
    .await?;
    Ok(())
}

/// How many organisations still hold this installation.
pub async fn organisations_using(pool: &PgPool, installation_id: i64) -> Result<i64> {
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM github_installations WHERE installation_id = $1")
            .bind(installation_id)
            .fetch_one(pool)
            .await?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// Repository links
// ---------------------------------------------------------------------------

/// The repository a site follows, with its installation's state.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RepoLink {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The site, by UUID.
    pub site_id: Uuid,
    /// The installation row it was linked through.
    pub installation_ref: Uuid,
    /// That installation's GitHub id.
    pub installation_id: i64,
    /// That installation's account.
    pub account_login: String,
    /// Set while that installation is suspended.
    pub suspended_at: Option<DateTime<Utc>>,
    /// GitHub's repository id, which survives a rename.
    pub repo_id: i64,
    /// `owner/name`.
    pub repo_full_name: String,
    /// The branch whose pushes are recorded.
    pub default_branch: String,
    /// The repository on GitHub.
    pub html_url: String,
    /// Successful deployments become annotations.
    pub annotate_deploys: bool,
    /// Published releases become annotations.
    pub annotate_releases: bool,
    /// Pushes to the default branch become annotations.
    pub annotate_pushes: bool,
    /// The deployment environments recorded; empty means all of them.
    pub environments: Json,
    /// Agents may open issues from the feedback they sent about this site.
    pub agent_issues: bool,
    /// Who linked it, while the account exists.
    pub created_by: Option<Uuid>,
    /// When it was linked.
    pub created_at: DateTime<Utc>,
    /// When its options last changed.
    pub updated_at: DateTime<Utc>,
}

impl RepoLink {
    /// The environment names, as stored.
    pub fn environment_list(&self) -> Vec<String> {
        self.environments
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Is a deployment to `environment` one this site records?
    ///
    /// Without regard to case, because `Production` and `production` are the
    /// same place to the people who name them. An empty list records all.
    pub fn records_environment(&self, environment: &str) -> bool {
        let wanted = self.environment_list();
        wanted.is_empty()
            || wanted
                .iter()
                .any(|name| name.eq_ignore_ascii_case(environment.trim()))
    }
}

/// The repository half of a link, as GitHub described it.
#[derive(Debug, Clone)]
pub struct LinkRepo<'a> {
    /// GitHub's repository id.
    pub repo_id: i64,
    /// `owner/name`.
    pub full_name: &'a str,
    /// Its default branch.
    pub default_branch: &'a str,
    /// Its page on GitHub.
    pub html_url: &'a str,
}

/// The choices a site admin makes about a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOptions {
    /// Successful deployments become annotations.
    pub annotate_deploys: bool,
    /// Published releases become annotations.
    pub annotate_releases: bool,
    /// Pushes to the default branch become annotations.
    pub annotate_pushes: bool,
    /// The environments recorded; empty means all.
    pub environments: Vec<String>,
    /// Agents may open issues from their own feedback.
    pub agent_issues: bool,
}

impl Default for LinkOptions {
    /// Deploys and releases on, pushes off, production only, no agent issues.
    fn default() -> LinkOptions {
        LinkOptions {
            annotate_deploys: true,
            annotate_releases: true,
            annotate_pushes: false,
            environments: vec!["production".to_string()],
            agent_issues: false,
        }
    }
}

const LINK_SELECT: &str = "SELECT l.id, l.site_id, l.installation_ref, i.installation_id, \
    i.account_login, i.suspended_at, l.repo_id, l.repo_full_name, l.default_branch, l.html_url, \
    l.annotate_deploys, l.annotate_releases, l.annotate_pushes, l.environments, l.agent_issues, \
    l.created_by, l.created_at, l.updated_at \
    FROM github_repo_links l JOIN github_installations i ON i.id = l.installation_ref";

/// Link a site to a repository, replacing the repository it followed.
pub async fn put_link(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    installation_ref: Uuid,
    repo: &LinkRepo<'_>,
    options: &LinkOptions,
    created_by: Option<Uuid>,
) -> Result<RepoLink> {
    sqlx::query(
        "INSERT INTO github_repo_links (id, site_id, installation_ref, repo_id, repo_full_name,
             default_branch, html_url, annotate_deploys, annotate_releases, annotate_pushes,
             environments, agent_issues, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (site_id) DO UPDATE SET
             installation_ref = EXCLUDED.installation_ref,
             repo_id = EXCLUDED.repo_id,
             repo_full_name = EXCLUDED.repo_full_name,
             default_branch = EXCLUDED.default_branch,
             html_url = EXCLUDED.html_url,
             annotate_deploys = EXCLUDED.annotate_deploys,
             annotate_releases = EXCLUDED.annotate_releases,
             annotate_pushes = EXCLUDED.annotate_pushes,
             environments = EXCLUDED.environments,
             agent_issues = EXCLUDED.agent_issues,
             created_by = EXCLUDED.created_by,
             updated_at = now()",
    )
    .bind(id)
    .bind(site_id)
    .bind(installation_ref)
    .bind(repo.repo_id)
    .bind(repo.full_name)
    .bind(repo.default_branch)
    .bind(repo.html_url)
    .bind(options.annotate_deploys)
    .bind(options.annotate_releases)
    .bind(options.annotate_pushes)
    .bind(Json::from(options.environments.clone()))
    .bind(options.agent_issues)
    .bind(created_by)
    .execute(pool)
    .await?;
    link_for_site(pool, site_id)
        .await?
        .ok_or_else(|| micaforge_core::Error::not_found("repository link"))
}

/// Change what a link records.
pub async fn set_link_options(
    pool: &PgPool,
    site_id: Uuid,
    options: &LinkOptions,
) -> Result<RepoLink> {
    let done = sqlx::query(
        "UPDATE github_repo_links SET annotate_deploys = $2, annotate_releases = $3,
             annotate_pushes = $4, environments = $5, agent_issues = $6, updated_at = now()
         WHERE site_id = $1",
    )
    .bind(site_id)
    .bind(options.annotate_deploys)
    .bind(options.annotate_releases)
    .bind(options.annotate_pushes)
    .bind(Json::from(options.environments.clone()))
    .bind(options.agent_issues)
    .execute(pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(micaforge_core::Error::not_found("repository link"));
    }
    link_for_site(pool, site_id)
        .await?
        .ok_or_else(|| micaforge_core::Error::not_found("repository link"))
}

/// The repository a site follows, if any.
pub async fn link_for_site(pool: &PgPool, site_id: Uuid) -> Result<Option<RepoLink>> {
    let row = sqlx::query_as::<_, RepoLink>(&format!("{LINK_SELECT} WHERE l.site_id = $1"))
        .bind(site_id)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Stop a site following its repository. Recorded events stay.
pub async fn delete_link(pool: &PgPool, site_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM github_repo_links WHERE site_id = $1")
        .bind(site_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Every site that follows `repo_id` through installation `installation_id`.
pub async fn links_for_repo(
    pool: &PgPool,
    installation_id: i64,
    repo_id: i64,
) -> Result<Vec<RepoLink>> {
    let rows = sqlx::query_as::<_, RepoLink>(&format!(
        "{LINK_SELECT} WHERE i.installation_id = $1 AND l.repo_id = $2 ORDER BY l.created_at"
    ))
    .bind(installation_id)
    .bind(repo_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Remove the links to repositories an installation lost access to. Returns
/// the links removed, for the audit log.
pub async fn unlink_repos(
    pool: &PgPool,
    installation_id: i64,
    repo_ids: &[i64],
) -> Result<Vec<RepoLink>> {
    if repo_ids.is_empty() {
        return Ok(Vec::new());
    }
    let gone = sqlx::query_as::<_, RepoLink>(&format!(
        "{LINK_SELECT} WHERE i.installation_id = $1 AND l.repo_id = ANY($2)"
    ))
    .bind(installation_id)
    .bind(repo_ids)
    .fetch_all(pool)
    .await?;
    let ids: Vec<Uuid> = gone.iter().map(|l| l.id).collect();
    sqlx::query("DELETE FROM github_repo_links WHERE id = ANY($1)")
        .bind(&ids)
        .execute(pool)
        .await?;
    Ok(gone)
}

/// Keep a repository's name and default branch current, from a delivery.
pub async fn refresh_repo(pool: &PgPool, installation_id: i64, repo: &LinkRepo<'_>) -> Result<()> {
    sqlx::query(
        "UPDATE github_repo_links l SET repo_full_name = $3, default_branch = $4,
             html_url = $5, updated_at = now()
         FROM github_installations i
         WHERE i.id = l.installation_ref AND i.installation_id = $1 AND l.repo_id = $2
           AND (l.repo_full_name, l.default_branch, l.html_url) IS DISTINCT FROM ($3, $4, $5)",
    )
    .bind(installation_id)
    .bind(repo.repo_id)
    .bind(repo.full_name)
    .bind(repo.default_branch)
    .bind(repo.html_url)
    .execute(pool)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Deliveries
// ---------------------------------------------------------------------------

/// One verified webhook delivery.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Delivery {
    /// `X-GitHub-Delivery`.
    pub delivery_id: String,
    /// `X-GitHub-Event`.
    pub event: String,
    /// The payload's `action`, when it has one.
    pub action: Option<String>,
    /// The installation it came through.
    pub installation_id: Option<i64>,
    /// The repository it is about, when it is about one.
    pub repo_id: Option<i64>,
    /// That repository's `owner/name`.
    pub repo_full_name: Option<String>,
    /// `processing` | `recorded` | `ignored` | `failed`.
    pub outcome: String,
    /// What happened, in a sentence.
    pub detail: Option<String>,
    /// When it arrived (or was last claimed again).
    pub received_at: DateTime<Utc>,
}

/// What a delivery's headers and envelope say before it is handled.
#[derive(Debug, Clone)]
pub struct DeliveryHead<'a> {
    /// `X-GitHub-Delivery`.
    pub delivery_id: &'a str,
    /// `X-GitHub-Event`.
    pub event: &'a str,
    /// The payload's `action`.
    pub action: Option<&'a str>,
    /// `installation.id`.
    pub installation_id: Option<i64>,
    /// `repository.id`.
    pub repo_id: Option<i64>,
    /// `repository.full_name`.
    pub repo_full_name: Option<&'a str>,
}

/// Claim a delivery for handling. `false` means it was handled already.
///
/// A delivery that `failed`, or one stuck in `processing` for five minutes
/// (the process died under it), may be claimed again: that is what GitHub's
/// "Redeliver" button is for. One that was `recorded` or `ignored` may not.
pub async fn claim_delivery(pool: &PgPool, head: &DeliveryHead<'_>) -> Result<bool> {
    let claimed: Option<(String,)> = sqlx::query_as(
        "INSERT INTO github_deliveries (delivery_id, event, action, installation_id, repo_id,
             repo_full_name)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (delivery_id) DO UPDATE SET
             outcome = 'processing', detail = NULL, received_at = now()
         WHERE github_deliveries.outcome = 'failed'
            OR (github_deliveries.outcome = 'processing'
                AND github_deliveries.received_at < now() - interval '5 minutes')
         RETURNING delivery_id",
    )
    .bind(head.delivery_id)
    .bind(head.event)
    .bind(head.action)
    .bind(head.installation_id)
    .bind(head.repo_id)
    .bind(head.repo_full_name)
    .fetch_optional(pool)
    .await?;
    Ok(claimed.is_some())
}

/// Say how a claimed delivery ended.
pub async fn finish_delivery(
    pool: &PgPool,
    delivery_id: &str,
    outcome: &str,
    detail: &str,
) -> Result<()> {
    sqlx::query("UPDATE github_deliveries SET outcome = $2, detail = $3 WHERE delivery_id = $1")
        .bind(delivery_id)
        .bind(outcome)
        .bind(detail)
        .execute(pool)
        .await?;
    Ok(())
}

/// Drop deliveries older than `days`, a bounded batch at a time.
///
/// Called as deliveries arrive rather than from a job: the table only grows
/// when deliveries arrive, so that is when it needs trimming, and the batch
/// bound keeps one delivery from paying for a year of backlog.
pub async fn prune_deliveries(pool: &PgPool, days: i32) -> Result<u64> {
    let done = sqlx::query(
        "DELETE FROM github_deliveries WHERE delivery_id IN (
             SELECT delivery_id FROM github_deliveries
             WHERE received_at < now() - make_interval(days => $1)
             LIMIT 500)",
    )
    .bind(days)
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// The latest deliveries about one repository of one installation, and the
/// installation's own (suspended, permissions changed), newest first.
pub async fn deliveries_for_repo(
    pool: &PgPool,
    installation_id: i64,
    repo_id: i64,
    limit: i64,
) -> Result<Vec<Delivery>> {
    let rows = sqlx::query_as::<_, Delivery>(
        "SELECT delivery_id, event, action, installation_id, repo_id, repo_full_name, outcome,
             detail, received_at
         FROM github_deliveries
         WHERE installation_id = $1 AND (repo_id = $2 OR repo_id IS NULL)
         ORDER BY received_at DESC LIMIT $3",
    )
    .bind(installation_id)
    .bind(repo_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// A deploy, a release or a push recorded for a site.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Event {
    /// UUIDv7 primary key.
    pub id: Uuid,
    /// The site, by UUID.
    pub site_id: Uuid,
    /// `deploy` | `release` | `push`.
    pub kind: String,
    /// GitHub's id for the object.
    pub external_id: String,
    /// `owner/name`.
    pub repo_full_name: String,
    /// The branch or tag.
    pub git_ref: Option<String>,
    /// The commit.
    pub sha: Option<String>,
    /// One line.
    pub title: String,
    /// The deployment environment.
    pub environment: Option<String>,
    /// The GitHub login that caused it.
    pub actor: Option<String>,
    /// The object on GitHub.
    pub url: String,
    /// A push's commits.
    pub commits: Json,
    /// When it happened.
    pub occurred_at: DateTime<Utc>,
    /// The annotation it wrote, while that exists.
    pub annotation_id: Option<Uuid>,
    /// The delivery that carried it.
    pub delivery_id: Option<String>,
    /// When it was recorded.
    pub created_at: DateTime<Utc>,
}

/// An event as a delivery describes it.
#[derive(Debug, Clone)]
pub struct NewEvent<'a> {
    /// `deploy` | `release` | `push`.
    pub kind: &'a str,
    /// GitHub's id for the object.
    pub external_id: &'a str,
    /// `owner/name`.
    pub repo_full_name: &'a str,
    /// The branch or tag.
    pub git_ref: Option<&'a str>,
    /// The commit.
    pub sha: Option<&'a str>,
    /// One line.
    pub title: &'a str,
    /// The deployment environment.
    pub environment: Option<&'a str>,
    /// The GitHub login.
    pub actor: Option<&'a str>,
    /// The object on GitHub.
    pub url: &'a str,
    /// A push's commits.
    pub commits: &'a Json,
    /// When it happened.
    pub occurred_at: DateTime<Utc>,
    /// The delivery that carried it.
    pub delivery_id: Option<&'a str>,
}

/// The annotation an event writes.
#[derive(Debug, Clone)]
pub struct NoteDraft<'a> {
    /// The annotation's id, minted by the caller.
    pub id: Uuid,
    /// The note, already within the annotation length limit.
    pub text: &'a str,
    /// `deploy` | `release` | `push`.
    pub category: &'a str,
}

const EVENT_COLUMNS: &str = "id, site_id, kind, external_id, repo_full_name, git_ref, sha, \
    title, environment, actor, url, commits, occurred_at, annotation_id, delivery_id, created_at";

/// Record an event for a site, and its annotation when `note` is given.
///
/// `None` when the site already has this event: nothing is written then, not
/// even the annotation. See the module notes.
pub async fn record_event(
    pool: &PgPool,
    id: Uuid,
    site_id: Uuid,
    new: &NewEvent<'_>,
    note: Option<&NoteDraft<'_>>,
) -> Result<Option<Event>> {
    let mut tx = pool.begin().await?;
    let inserted: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO github_events (id, site_id, kind, external_id, repo_full_name, git_ref, sha,
             title, environment, actor, url, commits, occurred_at, delivery_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         ON CONFLICT (site_id, kind, external_id) DO NOTHING
         RETURNING id",
    )
    .bind(id)
    .bind(site_id)
    .bind(new.kind)
    .bind(new.external_id)
    .bind(new.repo_full_name)
    .bind(new.git_ref)
    .bind(new.sha)
    .bind(new.title)
    .bind(new.environment)
    .bind(new.actor)
    .bind(new.url)
    .bind(new.commits)
    .bind(new.occurred_at)
    .bind(new.delivery_id)
    .fetch_optional(&mut *tx)
    .await?;
    if inserted.is_none() {
        tx.commit().await?;
        return Ok(None);
    }

    if let Some(note) = note {
        sqlx::query(
            "INSERT INTO annotations (id, site_id, starts_at, text, color, category, source, url)
             VALUES ($1, $2, $3, $4, 'neutral', $5, 'github', $6)",
        )
        .bind(note.id)
        .bind(site_id)
        .bind(new.occurred_at)
        .bind(note.text)
        .bind(note.category)
        .bind(new.url)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE github_events SET annotation_id = $2 WHERE id = $1")
            .bind(id)
            .bind(note.id)
            .execute(&mut *tx)
            .await?;
    }

    let row = sqlx::query_as::<_, Event>(&format!(
        "SELECT {EVENT_COLUMNS} FROM github_events WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(row))
}

/// A site's events of the given kinds in `[start, end)`, newest first.
pub async fn events_in(
    pool: &PgPool,
    site_id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    kinds: &[String],
    limit: i64,
) -> Result<Vec<Event>> {
    let rows = sqlx::query_as::<_, Event>(&format!(
        "SELECT {EVENT_COLUMNS} FROM github_events
         WHERE site_id = $1 AND occurred_at >= $2 AND occurred_at < $3 AND kind = ANY($4)
         ORDER BY occurred_at DESC, id DESC LIMIT $5"
    ))
    .bind(site_id)
    .bind(start)
    .bind(end)
    .bind(kinds)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// A site's latest events, newest first.
pub async fn recent_events(pool: &PgPool, site_id: Uuid, limit: i64) -> Result<Vec<Event>> {
    let rows = sqlx::query_as::<_, Event>(&format!(
        "SELECT {EVENT_COLUMNS} FROM github_events WHERE site_id = $1
         ORDER BY occurred_at DESC, id DESC LIMIT $2"
    ))
    .bind(site_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(environments: Json) -> RepoLink {
        RepoLink {
            id: Uuid::from_u128(1),
            site_id: Uuid::from_u128(2),
            installation_ref: Uuid::from_u128(3),
            installation_id: 42,
            account_login: "acme".into(),
            suspended_at: None,
            repo_id: 7,
            repo_full_name: "acme/site".into(),
            default_branch: "main".into(),
            html_url: "https://github.com/acme/site".into(),
            annotate_deploys: true,
            annotate_releases: true,
            annotate_pushes: false,
            environments,
            agent_issues: false,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn environments_match_without_case_and_an_empty_list_takes_all() {
        let production = link(json!(["production"]));
        assert!(production.records_environment("Production"));
        assert!(production.records_environment(" production "));
        assert!(!production.records_environment("staging"));

        let every = link(json!([]));
        assert!(every.records_environment("preview-42"));

        let two = link(json!(["staging", "production"]));
        assert_eq!(two.environment_list(), ["staging", "production"]);
        assert!(two.records_environment("STAGING"));
    }

    #[test]
    fn the_default_options_record_deploys_and_releases_to_production() {
        let options = LinkOptions::default();
        assert!(options.annotate_deploys && options.annotate_releases);
        assert!(
            !options.annotate_pushes,
            "a busy branch would flood the chart"
        );
        assert_eq!(options.environments, ["production"]);
        assert!(!options.agent_issues, "a site admin must switch this on");
    }
}
