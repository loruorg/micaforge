//! Stated policy versus observed behaviour.
//!
//! A site owner writes `robots.txt` and, increasingly, `llms.txt`, and then has
//! no way to find out whether any of it was honoured. Every server log holds the
//! answer and no analytics tool reads it. This module is the join: for one agent
//! and one path, what did the site say, and is what happened consistent with it?
//!
//! Two deliberate restraints, because this is the part of the product most
//! capable of being unfair:
//!
//! - **A violation is a fetch of a path the site disallowed, and nothing more.**
//!   Not an unverified fetch, not a fetch by an agent the owner dislikes, not an
//!   agent whose operator declines to say it obeys `robots.txt`. One rule, one
//!   observation, both shown.
//! - **Unknown is a real answer.** [`PolicyReport::allowed`] is `Option<bool>`
//!   and maps to the `robots_allowed Int8` column's `-1`. A site with no
//!   `robots.txt`, or one that never mentions this agent and has no `*` group,
//!   has expressed no policy, and reporting that as "allowed" would put words in
//!   its mouth.

use serde::Serialize;

use crate::catalog::{AgentDef, CATALOG};
use crate::llms_txt::LlmsTxt;
use crate::robots::Robots;

/// One row of the policy screen: what a site said about an agent, and what that
/// agent did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct PolicyReport {
    /// The agent's catalog slug.
    pub agent: String,
    /// What `robots.txt` says. `None` when the file expresses no rule that
    /// reaches this agent.
    pub allowed: Option<bool>,
    /// Whether the path is listed in `llms.txt`.
    pub listed_in_llms: bool,
    /// Fetches observed in the reporting window.
    pub observed_fetches: u64,
    /// Of those, how many hit a path this agent was disallowed from.
    pub violations: u64,
}

impl PolicyReport {
    /// A report for an agent with nothing observed yet.
    ///
    /// The zero-observation row is not noise: an agent that a site explicitly
    /// allowed and that has never once fetched anything is a finding, and it
    /// only appears if the row is emitted before any data exists.
    pub fn new(agent: &str, allowed: Option<bool>, listed_in_llms: bool) -> Self {
        PolicyReport {
            agent: agent.to_string(),
            allowed,
            listed_in_llms,
            observed_fetches: 0,
            violations: 0,
        }
    }

    /// Attach observed counts.
    pub fn with_observations(mut self, fetches: u64, violations: u64) -> Self {
        self.observed_fetches = fetches;
        self.violations = violations;
        self
    }

    /// True when the site disallowed this agent and it fetched anyway.
    pub fn is_violating(&self) -> bool {
        self.violations > 0
    }

    /// True when the site said nothing that applies to this agent.
    pub fn is_unstated(&self) -> bool {
        self.allowed.is_none()
    }
}

/// Evaluate one agent against one path.
///
/// Returns the two facts that belong on an `agent_events` row: whether
/// `robots.txt` allowed the fetch, and whether `llms.txt` listed the page.
///
/// The robots token used is [`AgentDef::robots_token`], not the slug: a site
/// owner writes `User-agent: Google-Extended`, and the group matcher is
/// case-insensitive, so the operator's own spelling is the one to look for.
pub fn evaluate(
    robots: &Robots,
    llms: Option<&LlmsTxt>,
    agent: &AgentDef,
    path: &str,
) -> (Option<bool>, bool) {
    let token = agent.robots_token();
    let allowed = if robots.group_for(token).is_some() {
        Some(robots.allows(token, path))
    } else {
        // No named group and no `*` group: the file exists but says nothing that
        // reaches this agent.
        None
    };
    let listed = llms.is_some_and(|l| l.lists_path(path));
    (allowed, listed)
}

/// Evaluate every catalog agent against one path.
///
/// This is what `GET /api/policy` renders: the whole catalog, each row carrying
/// what the site's own files say about it. Observed counts are joined on by the
/// query layer afterwards.
pub fn evaluate_all(robots: &Robots, llms: Option<&LlmsTxt>, path: &str) -> Vec<PolicyReport> {
    CATALOG
        .iter()
        .map(|def| {
            let (allowed, listed) = evaluate(robots, llms, def, path);
            PolicyReport::new(def.slug, allowed, listed)
        })
        .collect()
}

/// The `agent_events.robots_allowed` column value: `-1` unknown, `0`
/// disallowed, `1` allowed.
pub fn robots_allowed_flag(allowed: Option<bool>) -> i8 {
    match allowed {
        None => -1,
        Some(false) => 0,
        Some(true) => 1,
    }
}

/// The `agent_events.llms_listed` column value: `-1` when the site has no
/// `llms.txt` at all, `0` when it has one and this path is absent from it, `1`
/// when it is listed.
///
/// The three-way answer matters: "no llms.txt" and "an llms.txt that leaves this
/// page out" are different statements about a site, and only the second is a
/// decision the owner made.
pub fn llms_listed_flag(llms: Option<&LlmsTxt>, path: &str) -> i8 {
    match llms {
        None => -1,
        Some(llms) if llms.lists_path(path) => 1,
        Some(_) => 0,
    }
}

/// Whether this fetch contradicts the site's stated policy.
///
/// Only a disallowed path counts. An agent whose operator does not claim to read
/// `robots.txt` still shows up here when it fetches a disallowed path, and that
/// is correct: the report is about what the site asked for, and
/// [`AgentDef::respects_robots`] is shown beside it so the reader can see
/// whether the operator ever promised to listen.
pub fn is_violation(allowed: Option<bool>) -> bool {
    allowed == Some(false)
}

/// Agents the site names in `robots.txt` that are not in the catalog.
///
/// Usually a typo (`GPT-Bot`), an agent that has been renamed, or a crawler
/// Micaforge does not know about yet. Surfacing it is more useful than silently
/// ignoring it: a rule written for a token no crawler sends is a rule that does
/// nothing, and the site owner believes it is protecting them.
pub fn unrecognised_tokens(robots: &Robots) -> Vec<String> {
    robots
        .agents_listed()
        .into_iter()
        .filter(|token| {
            if token.as_str() == "*" {
                return false;
            }
            !CATALOG.iter().any(|def| {
                def.robots_token().eq_ignore_ascii_case(token)
                    || def
                        .ua_patterns
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(token))
            })
        })
        .collect()
}

/// Catalog agents the site has written no rule for.
///
/// The `*` group covers everyone, so this is about *named* rules: which of the
/// agents that matter to this site has the owner never made a decision about?
pub fn unaddressed_agents(robots: &Robots) -> Vec<&'static AgentDef> {
    CATALOG
        .iter()
        .filter(|def| !robots.names(def.robots_token()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::find_by_slug;
    use crate::{llms_txt, robots};

    const ROBOTS: &str = "\
User-agent: *
Disallow: /private

User-agent: GPTBot
Disallow: /

User-agent: ClaudeBot
Allow: /
";

    const LLMS: &str = "\
# Example

> A site.

## Docs

- [Guide](/docs/guide)
";

    #[test]
    fn a_named_ban_is_reported_as_a_ban() {
        let robots = robots::parse(ROBOTS);
        let gptbot = find_by_slug("gptbot").unwrap();
        let (allowed, _) = evaluate(&robots, None, gptbot, "/docs/guide");
        assert_eq!(allowed, Some(false));
        assert!(is_violation(allowed));
        assert_eq!(robots_allowed_flag(allowed), 0);
    }

    #[test]
    fn a_named_allowance_beats_the_wildcard_ban() {
        let robots = robots::parse(ROBOTS);
        let claude = find_by_slug("claudebot").unwrap();
        // `/private` is banned for everyone under `*`, but ClaudeBot has its own
        // group, and only one group ever applies.
        let (allowed, _) = evaluate(&robots, None, claude, "/private/x");
        assert_eq!(allowed, Some(true));
        assert!(!is_violation(allowed));
    }

    #[test]
    fn an_unnamed_agent_falls_back_to_the_wildcard_group() {
        let robots = robots::parse(ROBOTS);
        let ccbot = find_by_slug("ccbot").unwrap();
        assert_eq!(evaluate(&robots, None, ccbot, "/private").0, Some(false));
        assert_eq!(evaluate(&robots, None, ccbot, "/public").0, Some(true));
    }

    #[test]
    fn no_applicable_rule_is_unknown_not_allowed() {
        let robots = robots::parse("User-agent: GPTBot\nDisallow: /\n");
        let ccbot = find_by_slug("ccbot").unwrap();
        let (allowed, _) = evaluate(&robots, None, ccbot, "/x");
        assert_eq!(allowed, None);
        assert_eq!(robots_allowed_flag(allowed), -1);
        assert!(!is_violation(allowed));

        // An empty file says nothing about anyone.
        let empty = robots::parse("");
        assert_eq!(evaluate(&empty, None, ccbot, "/x").0, None);
    }

    #[test]
    fn a_robots_token_only_agent_is_still_evaluated() {
        // Google-Extended never sends a request, but a site's opt-out of
        // training is exactly what the policy screen exists to show.
        let robots = robots::parse("User-agent: Google-Extended\nDisallow: /\n");
        let extended = find_by_slug("google-extended").unwrap();
        assert_eq!(
            evaluate(&robots, None, extended, "/anything").0,
            Some(false)
        );
    }

    #[test]
    fn llms_listing_is_reported_three_ways() {
        let llms = llms_txt::parse(LLMS);
        assert_eq!(llms_listed_flag(Some(&llms), "/docs/guide"), 1);
        assert_eq!(llms_listed_flag(Some(&llms), "/pricing"), 0);
        assert_eq!(llms_listed_flag(None, "/docs/guide"), -1);

        let robots = robots::parse(ROBOTS);
        let claude = find_by_slug("claudebot").unwrap();
        let (_, listed) = evaluate(&robots, Some(&llms), claude, "/docs/guide");
        assert!(listed);
    }

    #[test]
    fn the_whole_catalog_can_be_evaluated_at_once() {
        let robots = robots::parse(ROBOTS);
        let rows = evaluate_all(&robots, None, "/private");
        assert_eq!(rows.len(), CATALOG.len());
        let gptbot = rows.iter().find(|r| r.agent == "gptbot").unwrap();
        assert_eq!(gptbot.allowed, Some(false));
        let claude = rows.iter().find(|r| r.agent == "claudebot").unwrap();
        assert_eq!(claude.allowed, Some(true));
        assert!(!claude.is_unstated());
    }

    #[test]
    fn a_rule_written_for_a_token_nobody_sends_is_surfaced() {
        let robots =
            robots::parse("User-agent: *\nDisallow:\n\nUser-agent: GPT-Bot\nDisallow: /\n");
        assert_eq!(unrecognised_tokens(&robots), vec!["GPT-Bot".to_string()]);
    }

    #[test]
    fn a_recognised_token_is_not_flagged_whatever_its_case() {
        let robots = robots::parse("User-agent: gptbot\nDisallow: /\n");
        assert!(unrecognised_tokens(&robots).is_empty());
    }

    #[test]
    fn agents_without_a_named_rule_are_listed() {
        let robots = robots::parse("User-agent: GPTBot\nDisallow: /\n");
        let unaddressed = unaddressed_agents(&robots);
        assert!(unaddressed.iter().all(|d| d.slug != "gptbot"));
        assert!(unaddressed.iter().any(|d| d.slug == "claudebot"));
    }

    #[test]
    fn observations_attach_to_a_row() {
        let row = PolicyReport::new("gptbot", Some(false), false).with_observations(120, 7);
        assert!(row.is_violating());
        assert_eq!(row.observed_fetches, 120);
    }
}
