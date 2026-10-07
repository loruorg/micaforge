//! A `robots.txt` parser and matcher, per RFC 9309.
//!
//! This exists so Micaforge can answer one question honestly: *did this agent
//! fetch a path the site told it not to?* That is a comparison between what the
//! site said and what the logs show, and it is only worth making if the parser
//! agrees with the parsers the crawlers themselves use. A matcher that is merely
//! close would generate accusations that are wrong, which is worse than not
//! making the report at all.
//!
//! The rules that matter, and where each is implemented:
//!
//! - **Groups.** Consecutive `User-agent` lines share the following rules. A
//!   rule line ends the run, so the next `User-agent` starts a new group.
//!   Records naming the same product token are merged (§2.2.1), which is done
//!   at parse time so that selection is unambiguous afterwards.
//! - **Group selection.** The most specific matching group wins and the `*`
//!   group is the fallback. Only one group applies: rules from `*` are **not**
//!   merged into a matched specific group. Getting this backwards is the single
//!   most common robots.txt bug, and it flips the answer for the exact case
//!   this product reports on (`Disallow: /` under `*`, plus an explicit
//!   allowance for one named crawler).
//! - **Longest match wins** between `Allow` and `Disallow`, and a tie goes to
//!   `Allow` (§2.2.2).
//! - **Wildcards.** `*` matches any run of characters, `$` anchors the end.
//! - **Case.** User-agent tokens match case-insensitively; paths do not.
//! - **Percent-encoding.** Both sides are normalised so that `/a%2Fb` and
//!   `/caf%C3%A9` compare the way the crawler compares them.
//!
//! An empty or unfetchable `robots.txt` allows everything. That is the RFC's
//! answer and it is also the honest one: absence of a rule is not a rule.

use serde::Serialize;

/// One `Allow` or `Disallow` line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rule {
    /// True for `Allow`, false for `Disallow`.
    pub allow: bool,
    /// The path pattern as written, normalised for percent-encoding and given a
    /// leading `/` if it lacked one.
    pub pattern: String,
}

impl Rule {
    /// How specific this rule is. Longest pattern wins a conflict.
    pub fn specificity(&self) -> usize {
        self.pattern.chars().count()
    }
}

/// One record: the agents it names, and the rules that apply to them.
///
/// Not `Eq`: `crawl_delay` is a float, and a delay is a quantity read off a
/// file rather than a key anything is looked up by.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Group {
    /// Every spelling of the product token this group was written for. Normally
    /// one; more when a file writes the same token twice with different case.
    pub agents: Vec<String>,
    /// The `Allow` and `Disallow` lines, in document order.
    pub rules: Vec<Rule>,
    /// `Crawl-delay`, in seconds. Not part of RFC 9309, still widely written and
    /// widely honoured, so it is parsed and reported rather than dropped.
    pub crawl_delay: Option<f32>,
}

impl Group {
    /// True when this is the `*` fallback group.
    pub fn is_wildcard(&self) -> bool {
        self.agents.iter().any(|a| a == "*")
    }

    /// How well this group matches a product token, or `None` if it does not.
    ///
    /// The score is the length of the matched name, so `claudebot` beats
    /// `claude` for `ClaudeBot`. The `*` group scores zero and is only ever used
    /// when nothing else matched at all.
    pub fn specificity_for(&self, ua_token: &str) -> Option<usize> {
        let token = ua_token.trim().to_ascii_lowercase();
        if token.is_empty() {
            return None;
        }
        let mut best: Option<usize> = None;
        for name in &self.agents {
            let name = name.to_ascii_lowercase();
            if name == "*" {
                best = Some(best.unwrap_or(0));
                continue;
            }
            // A caller may pass a product token (`GPTBot`) or a whole user-agent
            // string; the first two arms cover the token, the third covers the
            // full string. A one- or two-letter name is never matched loosely,
            // so a stray `User-agent: a` cannot capture every crawler.
            let hit = token == name
                || token.starts_with(&name)
                || (name.chars().count() >= 3 && token.contains(&name));
            if hit {
                best = Some(best.map_or(name.len(), |b| b.max(name.len())));
            }
        }
        best
    }

    /// True when this group applies to the token.
    pub fn matches_agent(&self, ua_token: &str) -> bool {
        self.specificity_for(ua_token).is_some()
    }

    /// Apply this group's rules to a path.
    ///
    /// `path` should carry the query string when there is one, because a rule
    /// like `Disallow: /*?` is written to match on it.
    pub fn allows(&self, path: &str) -> bool {
        let target = normalize_path(path);
        let mut best: Option<(usize, bool)> = None;

        for rule in &self.rules {
            // An empty value is the RFC's "no restriction": it matches nothing
            // rather than matching everything, so it is skipped, not applied.
            if rule.pattern.is_empty() {
                continue;
            }
            if !pattern_matches(&rule.pattern, &target) {
                continue;
            }
            let spec = rule.specificity();
            best = match best {
                None => Some((spec, rule.allow)),
                Some((best_spec, _)) if spec > best_spec => Some((spec, rule.allow)),
                // Equal specificity: the permissive rule wins (§2.2.2).
                Some((best_spec, best_allow)) if spec == best_spec => {
                    Some((best_spec, best_allow || rule.allow))
                }
                keep => keep,
            };
        }

        best.is_none_or(|(_, allow)| allow)
    }
}

/// A parsed `robots.txt`.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Robots {
    groups: Vec<Group>,
    /// Every `Sitemap:` line. These are file-global, not part of any group.
    pub sitemaps: Vec<String>,
}

/// Parse a `robots.txt` document.
///
/// Never fails. A malformed file is a fact about the site, not an error to
/// propagate: unrecognised lines are ignored exactly as a crawler ignores them,
/// and what remains is what the crawler would have obeyed.
pub fn parse(text: &str) -> Robots {
    // Raw records, before same-token merging.
    let mut raw: Vec<Group> = Vec::new();
    let mut sitemaps: Vec<String> = Vec::new();
    let mut open_group: Option<Group> = None;
    // A `User-agent` line after a rule line starts a new record; one after
    // another `User-agent` line extends the current record.
    let mut seen_rule = false;

    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = strip_comment(line);
        let Some((field, value)) = split_field(line) else {
            continue;
        };
        let field = field.to_ascii_lowercase();

        match field.as_str() {
            "user-agent" | "useragent" => {
                if value.is_empty() {
                    continue;
                }
                if seen_rule {
                    if let Some(group) = open_group.take() {
                        raw.push(group);
                    }
                    seen_rule = false;
                }
                open_group
                    .get_or_insert_with(Group::default)
                    .agents
                    .push(value.to_string());
            }
            "allow" | "disallow" => {
                // A rule before any `User-agent` line belongs to no record and
                // is discarded, as every major crawler discards it.
                if let Some(group) = open_group.as_mut() {
                    group.rules.push(Rule {
                        allow: field == "allow",
                        pattern: normalize_pattern(value),
                    });
                    seen_rule = true;
                }
            }
            "crawl-delay" | "crawldelay" => {
                if let Some(group) = open_group.as_mut() {
                    if let Ok(delay) = value.parse::<f32>() {
                        if delay.is_finite() && delay >= 0.0 {
                            group.crawl_delay.get_or_insert(delay);
                        }
                    }
                    seen_rule = true;
                }
            }
            "sitemap" if !value.is_empty() => sitemaps.push(value.to_string()),
            // `Host:`, `Noindex:`, `Clean-param:` and anything else: ignored,
            // and deliberately not treated as a rule, so they cannot split a
            // record in half.
            _ => {}
        }
    }
    if let Some(group) = open_group.take() {
        raw.push(group);
    }

    Robots {
        groups: merge_by_token(raw),
        sitemaps,
    }
}

/// Collapse records naming the same product token into one, preserving document
/// order and rule order (§2.2.1). A record naming three agents becomes three
/// entries sharing its rules, which makes group selection a plain maximum
/// afterwards instead of a search across overlapping records.
fn merge_by_token(raw: Vec<Group>) -> Vec<Group> {
    let mut merged: Vec<Group> = Vec::new();

    for group in raw {
        for agent in &group.agents {
            let key = agent.to_ascii_lowercase();
            match merged
                .iter_mut()
                .find(|g| g.agents.iter().any(|a| a.to_ascii_lowercase() == key))
            {
                Some(existing) => {
                    if !existing.agents.contains(agent) {
                        existing.agents.push(agent.clone());
                    }
                    existing.rules.extend(group.rules.iter().cloned());
                    if existing.crawl_delay.is_none() {
                        existing.crawl_delay = group.crawl_delay;
                    }
                }
                None => merged.push(Group {
                    agents: vec![agent.clone()],
                    rules: group.rules.clone(),
                    crawl_delay: group.crawl_delay,
                }),
            }
        }
    }

    merged
}

impl Robots {
    /// Whether this file allows `ua_token` to fetch `path`.
    ///
    /// `path` should include the query string when the request had one. A file
    /// with no applicable group allows everything.
    pub fn allows(&self, ua_token: &str, path: &str) -> bool {
        match self.group_for(ua_token) {
            Some(group) => group.allows(path),
            None => true,
        }
    }

    /// The one group that applies to this token: the most specific match, or the
    /// `*` group, or nothing.
    ///
    /// Only one group ever applies. A site that writes `Disallow: /` under `*`
    /// and then `Disallow:` under `GPTBot` has allowed GPTBot everything, and a
    /// report that merged the two would call every one of those fetches a
    /// violation.
    pub fn group_for(&self, ua_token: &str) -> Option<&Group> {
        let mut best: Option<(usize, &Group)> = None;
        for group in &self.groups {
            if let Some(spec) = group.specificity_for(ua_token) {
                if spec == 0 {
                    // The wildcard group: only a fallback.
                    continue;
                }
                if best.is_none_or(|(best_spec, _)| spec > best_spec) {
                    best = Some((spec, group));
                }
            }
        }
        if let Some((_, group)) = best {
            return Some(group);
        }
        self.groups.iter().find(|g| g.is_wildcard())
    }

    /// Every product token the file names, in the order it names them.
    ///
    /// This is what the policy screen shows as "agents you have written a rule
    /// for", next to the agents actually observed. The difference between those
    /// two lists is usually the interesting part.
    pub fn agents_listed(&self) -> Vec<String> {
        self.groups
            .iter()
            .filter_map(|g| g.agents.first().cloned())
            .collect()
    }

    /// The `Crawl-delay` that applies to this token, if any.
    pub fn crawl_delay(&self, ua_token: &str) -> Option<f32> {
        self.group_for(ua_token).and_then(|g| g.crawl_delay)
    }

    /// Every parsed group.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// True when the file carried no rules at all, so nothing is restricted.
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// True when the file names this token explicitly, rather than covering it
    /// through the `*` group.
    pub fn names(&self, ua_token: &str) -> bool {
        self.groups
            .iter()
            .any(|g| !g.is_wildcard() && g.matches_agent(ua_token))
    }
}

// ---------------------------------------------------------------------------
// Line handling
// ---------------------------------------------------------------------------

/// Drop everything from an unescaped `#` onwards.
fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Split `field: value`, tolerating missing whitespace and an absent value.
fn split_field(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let idx = line.find(':')?;
    let field = line[..idx].trim();
    let value = line[idx + 1..].trim();
    if field.is_empty() {
        return None;
    }
    Some((field, value))
}

// ---------------------------------------------------------------------------
// Path matching
// ---------------------------------------------------------------------------

/// Normalise a request target for comparison.
fn normalize_path(path: &str) -> String {
    let path = path.trim();
    let path = if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        "/".to_string()
    } else if let Some(idx) = path.find("://") {
        // A whole URL was passed. Take everything from the path onwards.
        match path[idx + 3..].find('/') {
            Some(slash) => path[idx + 3 + slash..].to_string(),
            None => "/".to_string(),
        }
    } else {
        format!("/{path}")
    };
    normalize_percent(&path)
}

/// Normalise a rule pattern the same way, keeping its wildcards intact.
fn normalize_pattern(pattern: &str) -> String {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return String::new();
    }
    let pattern = if pattern.starts_with('/') || pattern.starts_with('*') {
        pattern.to_string()
    } else {
        format!("/{pattern}")
    };
    normalize_percent(&pattern)
}

/// Decode percent-escapes that never needed escaping, and upper-case the hex of
/// the ones that did, so that both sides of a comparison agree. RFC 9309 §2.2.2
/// requires this; without it `/caf%c3%a9` and `/caf%C3%A9` are different paths
/// and a rule silently stops applying.
fn normalize_percent(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                let byte = hi * 16 + lo;
                if is_unreserved(byte) {
                    out.push(byte as char);
                } else {
                    out.push('%');
                    out.push(HEX[(byte >> 4) as usize] as char);
                    out.push(HEX[(byte & 0x0f) as usize] as char);
                }
                i += 3;
                continue;
            }
        }
        // Not a valid escape: copy the character whole, so multi-byte UTF-8
        // survives untouched.
        let ch = s[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// Does a rule pattern match this target?
///
/// A pattern matches a *prefix* of the target unless it ends with `$`, in which
/// case it must match the whole of it. `*` matches any run of characters.
fn pattern_matches(pattern: &str, target: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(rest) => (rest, true),
        None => (pattern, false),
    };

    let mut segments = pattern.split('*');
    // Everything before the first `*` must sit at the very start.
    let head = segments.next().unwrap_or("");
    if !target.starts_with(head) {
        return false;
    }
    let rest: Vec<&str> = segments.collect();
    if rest.is_empty() {
        // No wildcard at all: a prefix match, or an exact one when anchored.
        return !anchored || target.len() == head.len();
    }
    match_segments(&rest, &target[head.len()..], anchored)
}

/// Match the segments that follow a `*`. Each may start anywhere at or after the
/// current position, so a failed choice backtracks to the next occurrence.
fn match_segments(segments: &[&str], target: &str, anchored: bool) -> bool {
    let Some((literal, rest)) = segments.split_first() else {
        // Nothing left to match, and the `*` before us absorbs the remainder.
        return true;
    };

    if rest.is_empty() {
        return if anchored {
            // The last literal has to land on the end of the target. An empty
            // literal means the pattern ended in `*`, which always can.
            target.ends_with(literal)
        } else {
            literal.is_empty() || target.contains(literal)
        };
    }

    let mut start = 0usize;
    while start <= target.len() {
        let Some(offset) = target[start..].find(literal) else {
            return false;
        };
        let next = start + offset + literal.len();
        if match_segments(rest, &target[next..], anchored) {
            return true;
        }
        // Try the next occurrence. Advance one whole character so the index
        // stays on a UTF-8 boundary, and stop at the end of the string.
        let from = start + offset;
        match target[from..].chars().next() {
            Some(ch) => start = from + ch.len_utf8(),
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_allows_everything() {
        let robots = parse("");
        assert!(robots.allows("GPTBot", "/anything"));
        assert!(robots.is_empty());
        assert!(robots.group_for("GPTBot").is_none());
    }

    #[test]
    fn the_wildcard_group_is_the_fallback() {
        let robots = parse("User-agent: *\nDisallow: /private\n");
        assert!(!robots.allows("GPTBot", "/private/x"));
        assert!(robots.allows("GPTBot", "/public"));
        assert!(!robots.names("GPTBot"));
    }

    #[test]
    fn a_specific_group_replaces_the_wildcard_group_entirely() {
        // The case the policy report lives or dies on: `*` bans everything, and
        // one named crawler is let back in with an empty Disallow.
        let robots = parse(
            "User-agent: *\n\
             Disallow: /\n\
             \n\
             User-agent: GPTBot\n\
             Disallow:\n",
        );
        assert!(!robots.allows("ClaudeBot", "/docs"));
        assert!(
            robots.allows("GPTBot", "/docs"),
            "rules from the * group must not be merged into a matched group"
        );
        assert!(robots.names("GPTBot"));
    }

    #[test]
    fn longest_match_wins_and_a_tie_goes_to_allow() {
        let robots = parse(
            "User-agent: *\n\
             Disallow: /docs\n\
             Allow: /docs/public\n",
        );
        assert!(!robots.allows("x", "/docs/private"));
        assert!(robots.allows("x", "/docs/public/a"));

        // Same length on both sides: Allow wins.
        let tie = parse("User-agent: *\nDisallow: /a/b\nAllow: /a/b\n");
        assert!(tie.allows("x", "/a/b"));

        // And the reverse order must give the same answer.
        let tie_reversed = parse("User-agent: *\nAllow: /a/b\nDisallow: /a/b\n");
        assert!(tie_reversed.allows("x", "/a/b"));
    }

    #[test]
    fn a_disallow_all_is_beaten_by_a_longer_allow() {
        let robots = parse("User-agent: *\nDisallow: /\nAllow: /p\n");
        assert!(robots.allows("x", "/page"));
        assert!(!robots.allows("x", "/other"));
    }

    #[test]
    fn wildcards_and_anchors() {
        let robots = parse(
            "User-agent: *\n\
             Disallow: /*.php$\n\
             Disallow: /tmp/*/private\n",
        );
        assert!(!robots.allows("x", "/index.php"));
        assert!(!robots.allows("x", "/a/b/c.php"));
        // The `$` anchor is the whole point: with a query string appended the
        // target no longer ends in `.php`, so the rule stops applying.
        assert!(robots.allows("x", "/index.php?a=1"));
        assert!(!robots.allows("x", "/tmp/a/private"));
        assert!(!robots.allows("x", "/tmp/a/b/private/x"));
        assert!(robots.allows("x", "/tmp/private"));
    }

    #[test]
    fn a_query_string_is_part_of_the_target() {
        // `/*?` is how a site excludes every parameterised URL, and it only
        // works if the caller passes the query string along with the path.
        let robots = parse("User-agent: *\nDisallow: /*?\n");
        assert!(!robots.allows("x", "/page?utm_source=chatgpt"));
        assert!(robots.allows("x", "/page"));
    }

    #[test]
    fn an_anchor_after_a_wildcard_backtracks() {
        // The greedy-first-match trap: `b` occurs twice and only the second one
        // sits at the end.
        let robots = parse("User-agent: *\nDisallow: /a*b$\n");
        assert!(!robots.allows("x", "/axbyb"));
        assert!(robots.allows("x", "/axbyc"));
    }

    #[test]
    fn a_trailing_wildcard_is_a_prefix_match() {
        let robots = parse("User-agent: *\nDisallow: /private*\n");
        assert!(!robots.allows("x", "/private"));
        assert!(!robots.allows("x", "/private/deep/path"));
        assert!(robots.allows("x", "/pri"));
    }

    #[test]
    fn user_agent_matching_is_case_insensitive() {
        let robots = parse("User-Agent: gptbot\nDisallow: /x\n");
        assert!(!robots.allows("GPTBot", "/x"));
        assert!(!robots.allows("gptbot/1.2", "/x"));
        assert!(!robots.allows(
            "Mozilla/5.0 (compatible; GPTBot/1.2; +https://openai.com/gptbot)",
            "/x"
        ));
        assert!(robots.allows("ClaudeBot", "/x"));
    }

    #[test]
    fn the_most_specific_token_wins_between_two_named_groups() {
        let robots = parse(
            "User-agent: claude\n\
             Disallow: /\n\
             \n\
             User-agent: claudebot\n\
             Allow: /\n\
             Disallow: /secret\n",
        );
        assert!(robots.allows("ClaudeBot", "/docs"));
        assert!(!robots.allows("ClaudeBot", "/secret"));
    }

    #[test]
    fn consecutive_user_agent_lines_share_one_group() {
        let robots = parse(
            "User-agent: GPTBot\n\
             User-agent: ClaudeBot\n\
             Disallow: /paid\n\
             \n\
             User-agent: *\n\
             Disallow:\n",
        );
        assert!(!robots.allows("GPTBot", "/paid/x"));
        assert!(!robots.allows("ClaudeBot", "/paid/x"));
        assert!(robots.allows("PerplexityBot", "/paid/x"));
        assert_eq!(robots.agents_listed().len(), 3);
    }

    #[test]
    fn records_naming_the_same_token_are_merged() {
        let robots = parse(
            "User-agent: GPTBot\n\
             Disallow: /a\n\
             \n\
             User-agent: GPTBot\n\
             Disallow: /b\n",
        );
        assert!(!robots.allows("GPTBot", "/a"));
        assert!(!robots.allows("GPTBot", "/b"));
        assert_eq!(robots.groups().len(), 1);
    }

    #[test]
    fn comments_blank_lines_and_odd_spacing_survive() {
        let robots = parse(
            "# a comment\n\
             \n\
             User-agent:*   # trailing comment\n\
             Disallow:   /private\n\
             \r\n\
             Sitemap: https://example.com/sitemap.xml\n\
             Sitemap: https://example.com/sitemap-2.xml\n\
             Host: example.com\n",
        );
        assert!(!robots.allows("x", "/private"));
        assert_eq!(robots.sitemaps.len(), 2);
        assert_eq!(robots.sitemaps[0], "https://example.com/sitemap.xml");
    }

    #[test]
    fn an_unknown_field_does_not_split_a_group() {
        let robots = parse(
            "User-agent: GPTBot\n\
             Noindex: /x\n\
             Disallow: /y\n",
        );
        assert!(!robots.allows("GPTBot", "/y"));
        assert_eq!(robots.groups().len(), 1);
    }

    #[test]
    fn rules_before_any_user_agent_are_discarded() {
        let robots = parse("Disallow: /everything\nUser-agent: *\nAllow: /\n");
        assert!(robots.allows("x", "/everything"));
    }

    #[test]
    fn crawl_delay_is_read_per_group() {
        let robots = parse(
            "User-agent: *\n\
             Crawl-delay: 10\n\
             Disallow:\n\
             \n\
             User-agent: GPTBot\n\
             Crawl-delay: 0.5\n",
        );
        assert_eq!(robots.crawl_delay("SomeBot"), Some(10.0));
        assert_eq!(robots.crawl_delay("GPTBot"), Some(0.5));
    }

    #[test]
    fn percent_encoding_is_normalised_on_both_sides() {
        let robots = parse("User-agent: *\nDisallow: /caf%c3%a9\n");
        assert!(!robots.allows("x", "/caf%C3%A9"));
        assert!(!robots.allows("x", "/caf%c3%a9/menu"));

        // An escape that never needed escaping decodes on both sides.
        let escaped = parse("User-agent: *\nDisallow: /%64ocs\n");
        assert!(!escaped.allows("x", "/docs"));
    }

    #[test]
    fn a_pattern_without_a_leading_slash_gets_one() {
        let robots = parse("User-agent: *\nDisallow: private\n");
        assert!(!robots.allows("x", "/private"));
    }

    #[test]
    fn a_full_url_can_be_passed_as_the_path() {
        let robots = parse("User-agent: *\nDisallow: /private\n");
        assert!(!robots.allows("x", "https://example.com/private/x"));
        assert!(robots.allows("x", "https://example.com"));
    }

    #[test]
    fn a_short_agent_name_cannot_capture_everything() {
        // `User-agent: a` must not match `ClaudeBot` through a substring.
        let robots = parse("User-agent: a\nDisallow: /\n");
        assert!(robots.allows("ClaudeBot", "/x"));
    }
}
