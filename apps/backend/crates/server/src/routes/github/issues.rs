//! Turning a feedback report into an issue in the site's linked repository.
//!
//! Two front doors, one function: the triage screen's "Create GitHub issue"
//! (`POST /api/github/feedback/{id}/issue`) and the MCP tool
//! `create_github_issue_from_feedback` both end in [`open_issue`], so the
//! rule below is written once.
//!
//! ## Who may
//!
//! Three things must hold, and the first two are the ones every other route
//! already enforces:
//!
//! 1. **The caller may read the report** ([`feedback::may_read`]): an
//!    instance admin, signed in, reads every report; anybody else reads only
//!    the ones they sent. A report the caller may not read reads as missing.
//! 2. **The caller holds a role on the site whose repository receives it.**
//!    For a person signed in, `admin`: the issue goes into a repository on
//!    GitHub, possibly a public one, and that is a site admin's decision. An
//!    instance admin who is not an admin of the site cannot write into its
//!    repository, however many reports they may read.
//! 3. **For an agent** (an API key or an OAuth grant, which act as `member`
//!    at most), the credential must hold `manage:write` **and** the site's
//!    admins must have switched on "agents may open issues" on the link. A
//!    credential cannot grant itself that; a site admin grants it to every
//!    agent at once, and can take it back.
//!
//! ## What the issue says
//!
//! The report's title, its text inside a fenced code block, and the facts that
//! carry no person: its kind, severity, where it was sent from, the site's
//! domain, the page's path (never its query string, which can hold a token or
//! an address), the app version, when it arrived, and its id. Never the
//! reporter's name or address, the user agent, the API key or the MCP client:
//! the repository may be public, and its readers are not the people the
//! reporter wrote to. The fence is longer than any run of backticks in the
//! text, so nothing a stranger typed renders as a mention, a link or an image.
//!
//! ## Once
//!
//! A report that already has an issue answers with that issue. Two requests
//! at once are kept apart by a short lock in Valkey, and the issue is stored
//! only on a report that has none, so a race cannot open two.

use chrono::SecondsFormat;
use micaforge_core::{Error, Result};
use micaforge_store::models::feedback::{self as feedback_store, Feedback};
use micaforge_store::models::github as store;
use micaforge_store::models::sites::{self, Site};
use serde_json::{json, Value};
use uuid::Uuid;

use super::client::GitHub;
use crate::audit::{self, Entry};
use crate::extract::{AuthMethod, AuthUser};
use crate::routes::admin::is_instance_admin;
use crate::routes::feedback::{may_read, viewer_of};
use crate::routes::orgs::{require_at_least, ADMIN, MEMBER};
use crate::routes::sites::{effective_site_role, resolve_site_ref};
use crate::state::AppState;

/// How long one request holds a report while it opens its issue.
const LOCK_SECS: u32 = 60;

/// Longest issue title GitHub is sent, in characters.
const MAX_TITLE: usize = 200;

/// The issue a report became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// Its page on GitHub.
    pub url: String,
    /// Its number.
    pub number: i32,
    /// `owner/name`.
    pub repo: String,
    /// False when the report already had this issue.
    pub created: bool,
}

impl Opened {
    /// On the wire.
    pub fn json(&self) -> Value {
        json!({ "url": self.url, "number": self.number, "repo": self.repo, "created": self.created })
    }
}

/// Open (or return) the issue for report `id`. See the module notes.
///
/// `site_ref` names the site when the report names only an organisation; a
/// report about a site always goes to that site's repository.
pub async fn open_issue(
    st: &AppState,
    auth: &AuthUser,
    id: Uuid,
    site_ref: Option<&str>,
) -> Result<Opened> {
    let github = GitHub::of(st)?;
    let report = readable_report(st, auth, id).await?;
    let site = target_site(st, &report, site_ref).await?;

    let role = effective_site_role(&st.pg, &site, auth.user.id).await?;
    let link = store::link_for_site(&st.pg, site.id).await?.ok_or_else(|| {
        Error::Conflict(format!(
            "{} has no linked GitHub repository; a site admin links one in the site's GitHub settings",
            site.domain
        ))
    })?;
    match auth.method {
        AuthMethod::Session(_) => require_at_least(&role, ADMIN)?,
        AuthMethod::ApiKey(_) | AuthMethod::OAuth(_) => {
            auth.require_scope("manage:write")?;
            require_at_least(&role, MEMBER)?;
            if !link.agent_issues {
                return Err(Error::Forbidden(format!(
                    "the admins of {} have not let agents open GitHub issues; a site admin can \
                     switch that on in the site's GitHub settings",
                    site.domain
                )));
            }
        }
    }
    if let Some(done) = existing(&report) {
        return Ok(done);
    }
    if link.suspended_at.is_some() {
        return Err(Error::Conflict(format!(
            "the GitHub App is suspended on {}; lift the suspension on GitHub first",
            link.account_login
        )));
    }

    let lock = format!("mf:github:issue-lock:{id}");
    if st.kv.count(&lock, 1, LOCK_SECS).await? > 1 {
        return Err(Error::Conflict(
            "an issue for this report is being opened now; try again in a minute".to_string(),
        ));
    }
    let outcome = open_locked(st, auth, &github, id, &site, &link).await;
    if let Err(e) = st.kv.del(&lock).await {
        tracing::warn!(error = %e, "an issue lock could not be released; it expires by itself");
    }
    outcome
}

/// The part of [`open_issue`] that runs under the lock.
async fn open_locked(
    st: &AppState,
    auth: &AuthUser,
    github: &GitHub<'_>,
    id: Uuid,
    site: &Site,
    link: &store::RepoLink,
) -> Result<Opened> {
    // Read again under the lock: the request that held it may have finished.
    let report = feedback_store::get(&st.pg, id).await?;
    if let Some(done) = existing(&report) {
        return Ok(done);
    }

    let token = github
        .installation_token(&st.kv, link.installation_id)
        .await?;
    let issue = github
        .create_issue(
            &token,
            &link.repo_full_name,
            &issue_title(&report),
            &issue_body(&report, Some(&site.domain)),
        )
        .await?;
    let number = i32::try_from(issue.number)
        .map_err(|_| Error::Upstream("github: the issue number is out of range".into()))?;
    if !feedback_store::set_github_issue(&st.pg, id, &issue.html_url, number, &link.repo_full_name)
        .await?
    {
        // Another request stored an issue between our read and our write. Its
        // issue is the report's; ours is reported in the log so it can be
        // closed by hand.
        tracing::warn!(
            feedback = %id,
            issue = %issue.html_url,
            "a second GitHub issue was opened for one report; the first one is kept"
        );
        let kept = feedback_store::get(&st.pg, id).await?;
        return existing(&kept)
            .ok_or_else(|| Error::internal("the stored GitHub issue could not be read back"));
    }

    let opened = Opened {
        url: issue.html_url,
        number,
        repo: link.repo_full_name.clone(),
        created: true,
    };
    audit::record(
        st,
        auth,
        Entry::new("github.issue_created")
            .target("feedback", id, &report.title)
            .site(site)
            .after(opened.json()),
    )
    .await;
    Ok(opened)
}

/// The report, when the caller may read it; otherwise "not found".
async fn readable_report(st: &AppState, auth: &AuthUser, id: Uuid) -> Result<Feedback> {
    let report = feedback_store::get(&st.pg, id).await?;
    let admin =
        matches!(auth.method, AuthMethod::Session(_)) && is_instance_admin(st, &auth.user).await?;
    if may_read(&viewer_of(auth, admin), &report) {
        Ok(report)
    } else {
        Err(Error::not_found("feedback"))
    }
}

/// The site whose repository receives the report.
async fn target_site(st: &AppState, report: &Feedback, site_ref: Option<&str>) -> Result<Site> {
    if let Some(site_id) = report.site_id {
        return sites::get(&st.pg, site_id).await;
    }
    let (Some(org_id), Some(reference)) =
        (report.org_id, site_ref.filter(|r| !r.trim().is_empty()))
    else {
        return Err(Error::validation(
            "site",
            "this report names no site; give the site whose repository should receive it",
        ));
    };
    let site = resolve_site_ref(&st.pg, reference).await?;
    if site.org_id != org_id {
        return Err(Error::not_found("site"));
    }
    Ok(site)
}

/// The issue a report already has.
fn existing(report: &Feedback) -> Option<Opened> {
    Some(Opened {
        url: report.github_issue_url.clone()?,
        number: report.github_issue_number?,
        repo: report.github_issue_repo.clone().unwrap_or_default(),
        created: false,
    })
}

/// The report's title, as one clean line.
pub fn issue_title(report: &Feedback) -> String {
    let line: String = report
        .title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = line.trim();
    if line.chars().count() <= MAX_TITLE {
        line.to_string()
    } else {
        let mut cut: String = line.chars().take(MAX_TITLE - 1).collect();
        cut.push('\u{2026}');
        cut
    }
}

/// A code span that shows `value` as written: no backticks inside, no line
/// breaks, so it cannot close itself or start a new block.
fn code(value: &str) -> String {
    let clean: String = value
        .chars()
        .map(|c| if c == '`' || c.is_control() { ' ' } else { c })
        .collect();
    format!("`{}`", clean.trim())
}

/// A fence longer than every run of backticks in `text`.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// The path of a page address, without its query or fragment.
fn page_path(raw: &str) -> Option<String> {
    url::Url::parse(raw)
        .ok()
        .map(|u| u.path().to_string())
        .filter(|p| !p.is_empty())
}

/// The issue's text. See "What the issue says" in the module notes.
pub fn issue_body(report: &Feedback, domain: Option<&str>) -> String {
    let kind = match report.kind.as_str() {
        "bug" => "Bug",
        "problem" => "Problem",
        "feature" => "Feature request",
        _ => "Other",
    };
    let source = match report.source.as_str() {
        "dashboard" => "the Micaforge dashboard",
        "mcp" => "an agent, through MCP",
        "api" => "the Micaforge API",
        "chat" => "a chat app",
        _ => "Micaforge",
    };
    let fence = fence_for(&report.body);
    let mut facts = vec![
        format!("- Kind: {}", code(kind)),
        format!(
            "- Severity: {}",
            code(report.severity.as_deref().unwrap_or("not set"))
        ),
        format!("- Sent from: {source}"),
    ];
    if let Some(domain) = domain.or(report.site_domain.as_deref()) {
        facts.push(format!("- Site: {}", code(domain)));
    }
    if let Some(path) = report.page_url.as_deref().and_then(page_path) {
        facts.push(format!("- Page: {}", code(&path)));
    }
    if let Some(version) = report.app_version.as_deref() {
        facts.push(format!("- App version: {}", code(version)));
    }
    facts.push(format!(
        "- Received: {}",
        code(&report.created_at.to_rfc3339_opts(SecondsFormat::Secs, true))
    ));
    facts.push(format!("- Feedback id: {}", code(&report.id.to_string())));

    format!(
        "Feedback sent to Micaforge about this site, opened here as an issue.\n\n\
         {fence}text\n{}\n{fence}\n\n{}\n",
        report.body.trim_end(),
        facts.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn report() -> Feedback {
        Feedback {
            id: Uuid::from_u128(77),
            kind: "bug".into(),
            title: "The chart\nis empty".into(),
            body:
                "Since Monday @octocat sees nothing. ![x](https://evil.example/p.png)\n```\nhi\n```"
                    .into(),
            status: "new".into(),
            severity: Some("high".into()),
            source: "mcp".into(),
            reporter_user_id: Some(Uuid::from_u128(5)),
            reporter_email: Some("ada@example.com".into()),
            reporter_name: Some("Ada".into()),
            client_name: Some("Claude Code".into()),
            api_key_id: Some(Uuid::from_u128(6)),
            oauth_grant_id: None,
            chat_reporter: None,
            org_id: Some(Uuid::from_u128(8)),
            org_name: Some("Acme".into()),
            site_id: Some(Uuid::from_u128(9)),
            site_number: Some(3),
            site_domain: Some("acme.example".into()),
            page_url: Some("https://app.example.com/app/3/pages?token=secret#x".into()),
            app_version: Some("1.4.0".into()),
            user_agent: Some("Mozilla/5.0 (Macintosh)".into()),
            created_at: Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
            github_issue_url: None,
            github_issue_number: None,
            github_issue_repo: None,
        }
    }

    #[test]
    fn the_body_carries_the_text_and_safe_facts_and_nothing_personal() {
        let body = issue_body(&report(), None);
        assert!(body.contains("Since Monday @octocat sees nothing."));
        for fact in [
            "- Kind: `Bug`",
            "- Severity: `high`",
            "- Sent from: an agent, through MCP",
            "- Site: `acme.example`",
            "- Page: `/app/3/pages`",
            "- App version: `1.4.0`",
            "- Received: `2026-10-08T09:00:00Z`",
            "- Feedback id: `00000000-0000-0000-0000-00000000004d`",
        ] {
            assert!(body.contains(fact), "missing {fact} in\n{body}");
        }
        for private in [
            "ada@example.com",
            "Ada",
            "Claude Code",
            "Mozilla",
            "token=secret",
            "00000000-0000-0000-0000-000000000006",
        ] {
            assert!(!body.contains(private), "{private} reached the issue");
        }
    }

    #[test]
    fn the_text_is_fenced_past_every_backtick_run_in_it() {
        let body = issue_body(&report(), None);
        // The report holds a run of three, so the fence is four long and the
        // mention, the image and the inner fence stay inert text.
        assert!(body.contains("````text\nSince Monday"));
        assert!(body.trim_end().contains("```\n````"));
        assert_eq!(fence_for("no ticks"), "```");
        assert_eq!(fence_for("a ````` b"), "``````");
    }

    #[test]
    fn metadata_cannot_break_out_of_its_code_span() {
        assert_eq!(code("1.0`<b>`\nx"), "`1.0 <b>  x`");
        let mut odd = report();
        odd.app_version = Some("`@everyone`".into());
        assert!(issue_body(&odd, None).contains("- App version: `@everyone`"));
    }

    #[test]
    fn the_title_is_one_clean_bounded_line() {
        assert_eq!(issue_title(&report()), "The chart is empty");
        let mut long = report();
        long.title = "x".repeat(300);
        let title = issue_title(&long);
        assert_eq!(title.chars().count(), MAX_TITLE);
        assert!(title.ends_with('\u{2026}'));
    }

    #[test]
    fn a_report_with_an_issue_answers_with_it() {
        assert_eq!(existing(&report()), None);
        let mut done = report();
        done.github_issue_url = Some("https://github.com/acme/site/issues/12".into());
        done.github_issue_number = Some(12);
        done.github_issue_repo = Some("acme/site".into());
        assert_eq!(
            existing(&done),
            Some(Opened {
                url: "https://github.com/acme/site/issues/12".into(),
                number: 12,
                repo: "acme/site".into(),
                created: false,
            })
        );
    }
}
