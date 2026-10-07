//! `crawl_page_meta` — fill in what a page actually contains.
//!
//! The content register knows a page's URL from the sitemap and its traffic from
//! the event stream. What it does not know, and what several reports want, is
//! what is *on* the page: its title, how much of it there is, and when it last
//! changed. This job fetches a handful of the site's own pages a day and fills
//! those in.
//!
//! `word_count` in particular is the crude proxy for "how much of this page an
//! agent took". A crawler that fetched a four-thousand-word guide and a crawler
//! that fetched a redirect stub both count as one fetch in `agent_events`, and
//! only one of them took anything.
//!
//! # It obeys the site's own `robots.txt`
//!
//! Micaforge crawls under the token [`crate::fetch::ROBOTS_TOKEN`] and checks
//! every path against the site's own file before fetching it, using the same
//! RFC 9309 matcher the product uses to judge everybody else. A tool that
//! reports which crawlers ignored a site's `robots.txt` while ignoring it itself
//! would have no standing to publish the report.
//!
//! # One request a second, and a handful a day
//!
//! [`DEFAULT_LIMIT`] pages per site per run, paced at one request a second. A
//! full crawl of a large site would be both slow and rude; the register fills in
//! over a few weeks instead, oldest and least-known pages first, which is the
//! order that gets the most useful rows soonest.

use chrono::Utc;
use micaforge_agents::robots::Robots;
use micaforge_core::Result;
use micaforge_store::ch::PageIndexRow;
use micaforge_store::models::{catalog, sites};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fetch::{self, PAGE_MAX_BYTES, ROBOTS_TOKEN};
use crate::pages::{self, Patch};
use crate::JobCtx;

/// Pages fetched per site per run when the payload does not say.
pub const DEFAULT_LIMIT: usize = 25;

/// Hard ceiling, whatever a payload asks for.
///
/// Two hundred pages at one a second is over three minutes of somebody else's
/// server, which is the most this job should ever be in the middle of a crawl
/// for.
pub const MAX_LIMIT: usize = 200;

/// Which pages to read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One site, by `sites.id`. Absent means every site.
    #[serde(default)]
    pub site: Option<Uuid>,
    /// How many pages to fetch per site.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Read a handful of pages per site and fill in what they contain.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let targets = match payload.site {
        Some(id) => vec![sites::get(&ctx.pg, id).await?],
        None => sites::list_all(&ctx.pg).await?,
    };

    let limit = payload.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut fetched = 0usize;

    for site in &targets {
        match crawl_site(ctx, site, limit).await {
            Ok(count) => fetched += count,
            Err(e) => tracing::warn!(site = %site.domain, error = %e, "page crawl failed"),
        }
    }

    tracing::info!(
        sites = targets.len(),
        pages = fetched,
        "page crawl finished"
    );
    Ok(())
}

/// Read up to `limit` pages of one site.
async fn crawl_site(ctx: &JobCtx, site: &sites::Site, limit: usize) -> Result<usize> {
    let site_id = site.site_id_u16();
    let robots = load_robots(ctx, site).await;
    let known = pages::load(&ctx.ch, site_id).await?;
    let candidates = choose(&known, limit);

    if candidates.is_empty() {
        tracing::debug!(site = %site.domain, "no pages to read");
        return Ok(0);
    }

    let mut pace = fetch::Pace::per_second();
    let mut rows: Vec<PageIndexRow> = Vec::new();
    let mut read = 0usize;
    let mut skipped = 0usize;

    for path in candidates {
        // Our own rule, applied to ourselves, before anything is fetched.
        if !robots.allows(ROBOTS_TOKEN, &path) {
            skipped += 1;
            let patch = Patch {
                robots_directive: Some("Disallow".to_string()),
                ..Patch::default()
            };
            rows.push(patch.apply(known.get(&path), site_id, &path, Utc::now()));
            continue;
        }

        let url = match fetch::site_url(&site.domain, &path) {
            Ok(url) => url,
            Err(e) => {
                tracing::debug!(site = %site.domain, path, error = %e, "skipped an unfetchable path");
                continue;
            }
        };

        pace.tick().await;
        let doc = match fetch::get(&ctx.http, url.as_str(), PAGE_MAX_BYTES).await {
            Ok(Some(doc)) => doc,
            // A page in the sitemap that now 404s is a real finding, and it is
            // recorded by leaving the row alone rather than by writing a zero
            // over a word count that was true yesterday.
            Ok(None) => continue,
            Err(e) => {
                tracing::debug!(url = %url, error = %e, "page not readable");
                continue;
            }
        };

        // Only HTML is measured. A PDF or a JSON feed in the sitemap is a real
        // page with a real crawl trace, and counting the "words" in its bytes
        // would put a fictional number next to it.
        if !doc.content_type.is_empty() && !doc.content_type.starts_with("text/html") {
            continue;
        }

        let patch = Patch {
            title: Some(title_of(&doc.body)),
            word_count: Some(word_count(&doc.body)),
            last_modified: doc.last_modified,
            robots_directive: Some(directive(&robots, &path)),
            ..Patch::default()
        };
        rows.push(patch.apply(known.get(&path), site_id, &path, Utc::now()));
        read += 1;
    }

    let written = pages::write(&ctx.ch, &rows).await?;
    tracing::info!(
        site = %site.domain,
        read,
        skipped_by_robots = skipped,
        written,
        "page metadata updated"
    );
    Ok(read)
}

/// The site's `robots.txt`, from the stored copy when there is one.
///
/// Falls back to an empty file rather than to an error. An empty `Robots` allows
/// everything, which is the correct reading of "this site has published no
/// crawl policy" and is what every other crawler does with the same input.
async fn load_robots(ctx: &JobCtx, site: &sites::Site) -> Robots {
    if let Ok(Some(stored)) = catalog::get_policy(&ctx.pg, site.id).await {
        if let Some(text) = &stored.robots_txt {
            return micaforge_agents::robots::parse(text);
        }
    }

    let Ok(url) = fetch::site_url(&site.domain, "/robots.txt") else {
        return Robots::default();
    };
    match fetch::get(&ctx.http, url.as_str(), fetch::ROBOTS_MAX_BYTES).await {
        Ok(Some(doc)) => micaforge_agents::robots::parse(&doc.body),
        _ => Robots::default(),
    }
}

/// Which pages to read next.
///
/// Never-read pages first, then the ones read longest ago. That order fills the
/// most useful rows soonest: a page with no title and no word count contributes
/// nothing to a report, and a page whose word count is a month stale contributes
/// almost as much as a fresh one.
///
/// A site with no registered pages at all gets its root, so a new install has
/// something in the register before its first sitemap crawl.
fn choose(known: &std::collections::HashMap<String, PageIndexRow>, limit: usize) -> Vec<String> {
    if known.is_empty() {
        return vec!["/".to_string()];
    }

    let mut ranked: Vec<&PageIndexRow> = known.values().collect();
    ranked.sort_by(|a, b| {
        let unread = (a.word_count == 0).cmp(&(b.word_count == 0)).reverse();
        unread.then_with(|| a.updated_at.cmp(&b.updated_at))
    });

    ranked
        .into_iter()
        .take(limit)
        .map(|row| row.pathname.clone())
        .collect()
}

/// What the site's own file says about this path, as a decision.
///
/// The effective directive rather than the literal line. Reproducing the exact
/// `Disallow:` line would mean re-running the RFC 9309 group-and-specificity
/// match here, and that matcher lives in `micaforge-agents` and answers with a
/// decision. Two implementations of the same rule would eventually disagree, and
/// the one that mattered would be the other one.
///
/// An empty string means the file expresses no rule that reaches us, which is a
/// third answer and not the same as "allowed".
fn directive(robots: &Robots, path: &str) -> String {
    match robots.group_for(ROBOTS_TOKEN) {
        None => String::new(),
        Some(_) if robots.allows(ROBOTS_TOKEN, path) => "Allow".to_string(),
        Some(_) => "Disallow".to_string(),
    }
}

/// The document's `<title>`, trimmed and collapsed.
///
/// Returns an empty string when there is no title, which is a fact about the page
/// and is stored as one.
pub fn title_of(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<title") else {
        return String::new();
    };
    let Some(gt) = lower[open..].find('>') else {
        return String::new();
    };
    let start = open + gt + 1;
    let Some(end) = lower[start..].find("</title") else {
        return String::new();
    };

    collapse(&unescape(&html[start..start + end]))
}

/// How many words of readable text the page carries.
///
/// The `<head>`, every script and style body, and then every tag are removed,
/// and what is left is counted on whitespace. The head goes first because the
/// title and the meta description are not body copy: counting them would add a
/// fixed dozen words to every page on a site and flatter the short ones most.
///
/// It is a proxy and it is meant to be one: the question it answers is "is this a
/// substantial page or a stub", and it answers that correctly on a hand-written
/// article and on a rendered template alike.
pub fn word_count(html: &str) -> u32 {
    let text = strip_tags(&strip_elements(
        html,
        &["head", "script", "style", "noscript", "svg"],
    ));
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
        .min(u32::MAX as usize) as u32
}

/// Remove whole elements, contents and all.
///
/// The element name has to end where the name ends: `<head>` is a head and
/// `<header>` is not, and a prefix match would silently delete the contents of
/// every site's page header.
fn strip_elements(html: &str, names: &[&str]) -> String {
    let mut out = html.to_string();
    for name in names {
        let mut result = String::with_capacity(out.len());
        let lower = out.to_ascii_lowercase();
        let open_tag = format!("<{name}");
        let close_tag = format!("</{name}");
        let mut cursor = 0usize;

        while let Some(open) = find_element(&lower, &open_tag, cursor) {
            result.push_str(&out[cursor..open]);
            match find_element(&lower, &close_tag, open) {
                Some(close) => {
                    // Skip past the closing tag's own `>`.
                    cursor = match lower[close..].find('>') {
                        Some(gt) => close + gt + 1,
                        None => out.len(),
                    };
                }
                // An unclosed `<script>` swallows the rest, which is also what a
                // browser does with one.
                None => cursor = out.len(),
            }
        }
        result.push_str(&out[cursor..]);
        out = result;
    }
    out
}

/// Find `tag` in `lower` from `from`, where the element name actually ends.
fn find_element(lower: &str, tag: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some(found) = lower[cursor..].find(tag) {
        let at = cursor + found;
        let next = lower.as_bytes().get(at + tag.len()).copied();
        if matches!(
            next,
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'/')
        ) {
            return Some(at);
        }
        cursor = at + tag.len();
    }
    None
}

/// Remove every remaining tag, leaving the text between them.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut inside = false;

    for c in html.chars() {
        match c {
            '<' => inside = true,
            '>' => {
                inside = false;
                // A tag boundary is a word boundary: `<p>one</p><p>two</p>` is
                // two words, not one.
                out.push(' ');
            }
            _ if !inside => out.push(c),
            _ => {}
        }
    }

    unescape(&out)
}

/// Decode the handful of entities that change a word count or a title.
fn unescape(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }

    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let after = &rest[idx..];
        let Some(end) = after.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        match &after[1..end] {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" | "#39" => out.push('\''),
            "nbsp" | "#160" => out.push(' '),
            other => match numeric(other) {
                Some(c) => out.push(c),
                None => out.push_str(&after[..=end]),
            },
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Decode `#38` or `#x26`.
fn numeric(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code)
}

/// One line, single-spaced.
fn collapse(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html>
<html><head>
  <title>  Getting started &amp; other things
  </title>
  <style>body { color: red; content: "not words at all"; }</style>
</head><body>
  <script>const noise = "one two three four five";</script>
  <h1>Getting started</h1>
  <p>Install the tracker, then read the record.</p>
  <p>It&apos;s three lines.</p>
</body></html>"#;

    #[test]
    fn a_title_is_read_and_tidied() {
        assert_eq!(title_of(PAGE), "Getting started & other things");
        assert_eq!(title_of("<html><body>no title</body></html>"), "");
        // An attribute on the element must not break the match.
        assert_eq!(title_of(r#"<title lang="en">Hello</title>"#), "Hello");
        // An unterminated title is no title rather than the rest of the file.
        assert_eq!(title_of("<title>Hello"), "");
    }

    #[test]
    fn script_and_style_contents_are_not_words() {
        let words = word_count(PAGE);
        // "Getting started" (2) + "Install the tracker, then read the record." (7)
        // + "It's three lines." (3) = 12. Nothing from the script or the style.
        assert_eq!(words, 12);
    }

    #[test]
    fn tag_boundaries_are_word_boundaries() {
        assert_eq!(word_count("<p>one</p><p>two</p>"), 2);
        // Punctuation alone is not a word.
        assert_eq!(word_count("<p>one</p><p>-</p>"), 1);
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("<html></html>"), 0);
    }

    #[test]
    fn a_page_header_is_not_the_document_head() {
        // The prefix-match bug this guards against would delete the contents of
        // every site's `<header>` and quietly under-count every page.
        let html = "<head><title>T</title></head><body><header>one two</header><p>three</p></body>";
        assert_eq!(word_count(html), 3);
    }

    #[test]
    fn an_unclosed_script_swallows_the_rest_the_way_a_browser_would() {
        assert_eq!(word_count("<p>one</p><script>two three four"), 1);
    }

    #[test]
    fn entities_that_change_the_reading_are_decoded() {
        assert_eq!(unescape("a&amp;b"), "a&b");
        assert_eq!(unescape("it&apos;s"), "it's");
        assert_eq!(unescape("a&nbsp;b"), "a b");
        assert_eq!(unescape("a&#x27;b"), "a'b");
        assert_eq!(unescape("plain"), "plain");
        // An entity nobody defined is left alone rather than dropped.
        assert_eq!(unescape("a&zzz;b"), "a&zzz;b");
    }

    #[test]
    fn a_directive_is_allow_disallow_or_unstated() {
        let silent = micaforge_agents::robots::parse("User-agent: GPTBot\nDisallow: /\n");
        assert_eq!(directive(&silent, "/x"), "", "no rule reaches us");

        let open = micaforge_agents::robots::parse("User-agent: *\nDisallow: /private\n");
        assert_eq!(directive(&open, "/docs"), "Allow");
        assert_eq!(directive(&open, "/private/x"), "Disallow");

        let named = micaforge_agents::robots::parse("User-agent: Micaforge\nDisallow: /\n");
        assert_eq!(directive(&named, "/anything"), "Disallow");
    }

    #[test]
    fn the_least_known_pages_are_read_first() {
        use chrono::TimeZone;
        let mut known = std::collections::HashMap::new();

        let mut fresh = PageIndexRow {
            site_id: 1,
            pathname: "/fresh".into(),
            title: "Fresh".into(),
            first_seen: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            last_modified: None,
            word_count: 900,
            in_sitemap: 1,
            in_llms_txt: 0,
            robots_directive: String::new(),
            updated_at: Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap(),
        };
        known.insert(fresh.pathname.clone(), fresh.clone());

        let mut stale = fresh.clone();
        stale.pathname = "/stale".into();
        stale.updated_at = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        known.insert(stale.pathname.clone(), stale);

        fresh.pathname = "/never-read".into();
        fresh.word_count = 0;
        known.insert(fresh.pathname.clone(), fresh);

        let order = choose(&known, 3);
        assert_eq!(order[0], "/never-read");
        assert_eq!(order[1], "/stale");
        assert_eq!(order[2], "/fresh");
        assert_eq!(choose(&known, 1).len(), 1);
    }

    #[test]
    fn a_site_with_nothing_registered_starts_at_its_root() {
        let empty = std::collections::HashMap::new();
        assert_eq!(choose(&empty, 25), vec!["/".to_string()]);
    }
}
