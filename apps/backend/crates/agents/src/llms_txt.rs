//! A parser for `/llms.txt`, the convention for telling a model what a site
//! contains and where the good version of it lives.
//!
//! The format is deliberately plain Markdown with a fixed skeleton:
//!
//! ```markdown
//! # Title
//!
//! > A one-line summary, as a blockquote.
//!
//! Any number of free paragraphs.
//!
//! ## Docs
//!
//! - [Quickstart](https://example.com/docs/quickstart): get running in a minute
//! - [API reference](https://example.com/docs/api)
//!
//! ## Optional
//!
//! - [Changelog](https://example.com/changelog)
//! ```
//!
//! Micaforge reads it for one reason: to put a site's *stated* intent next to
//! the observed behaviour of the agents that visited. `robots.txt` says what an
//! agent may not take; `llms.txt` says what the site would like it to read. A
//! page that is listed here and never fetched, and a page fetched constantly
//! that was never listed, are both findings worth showing.
//!
//! The parser is forgiving on purpose. `llms.txt` is a convention rather than a
//! standard, so a file that gets the shape slightly wrong still carries real
//! information and is worth reading anyway.

use serde::Serialize;

/// A parsed `llms.txt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LlmsTxt {
    /// The `H1`, when there is one.
    pub title: Option<String>,
    /// The blockquote summary that follows the title, with its `>` markers and
    /// line breaks folded into one line.
    pub summary: Option<String>,
    /// The `H2` sections, in document order.
    pub sections: Vec<LlmsSection>,
}

/// One `H2` and the links beneath it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LlmsSection {
    /// The heading text. Empty for links that appeared before any `H2`, which
    /// the convention does not describe but real files contain.
    pub heading: String,
    /// The links listed in this section, in document order.
    pub links: Vec<LlmsLink>,
}

/// One entry in a section's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct LlmsLink {
    /// The link text.
    pub title: String,
    /// The href, exactly as written. May be absolute or site-relative.
    pub url: String,
    /// The note after the link, with a leading `:` or dash removed.
    pub note: Option<String>,
}

impl LlmsLink {
    /// The path part of the URL, for comparison against a request path.
    ///
    /// An absolute URL is reduced to its path; a relative one is returned with a
    /// leading slash. Query strings and fragments are dropped, because a listing
    /// is about a document and a request is about a URL.
    pub fn path(&self) -> String {
        let url = self.url.trim();
        let after_scheme = match url.find("://") {
            Some(idx) => match url[idx + 3..].find('/') {
                Some(slash) => &url[idx + 3 + slash..],
                None => "/",
            },
            None => url,
        };
        let path = after_scheme.split(['?', '#']).next().unwrap_or("/").trim();
        if path.is_empty() {
            "/".to_string()
        } else if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        }
    }
}

impl LlmsTxt {
    /// Every link in the file, across every section.
    pub fn links(&self) -> impl Iterator<Item = &LlmsLink> {
        self.sections.iter().flat_map(|s| s.links.iter())
    }

    /// How many links the file lists.
    pub fn link_count(&self) -> usize {
        self.sections.iter().map(|s| s.links.len()).sum()
    }

    /// True when the file carried nothing usable.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.summary.is_none() && self.link_count() == 0
    }

    /// Whether this path is listed in the file.
    ///
    /// Trailing slashes are ignored, and a `.md` sibling counts as the same
    /// document: `llms.txt` files routinely link to `/docs/x.md` as the plain
    /// text version of the page served at `/docs/x`, and treating those as two
    /// different documents would report a listed page as unlisted.
    pub fn lists_path(&self, path: &str) -> bool {
        let wanted = canonical_path(path);
        self.links()
            .any(|link| canonical_path(&link.path()) == wanted)
    }

    /// The section a path is listed under, if any.
    pub fn section_for(&self, path: &str) -> Option<&LlmsSection> {
        let wanted = canonical_path(path);
        self.sections.iter().find(|section| {
            section
                .links
                .iter()
                .any(|link| canonical_path(&link.path()) == wanted)
        })
    }
}

/// Reduce a path to the form two spellings of the same document share.
fn canonical_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("/");
    let path = path.trim();
    let path = path.strip_suffix(".md").unwrap_or(path);
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse an `llms.txt` document.
///
/// Never fails, for the same reason [`crate::robots::parse`] never fails: the
/// file is evidence about a site, and a file we cannot fully understand still
/// tells us most of what we asked.
pub fn parse(md: &str) -> LlmsTxt {
    let mut out = LlmsTxt::default();
    let mut current: Option<LlmsSection> = None;
    let mut summary_lines: Vec<String> = Vec::new();
    let mut in_code_fence = false;

    for raw_line in md.trim_start_matches('\u{feff}').lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim_start();

        // Fenced code blocks are content, not structure. Without this a `#` in
        // a shell example becomes a heading and everything after it lands in
        // the wrong section.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        if let Some(rest) = heading(trimmed, 1) {
            if out.title.is_none() {
                out.title = Some(rest.to_string());
            }
            continue;
        }
        if let Some(rest) = heading(trimmed, 2) {
            if let Some(section) = current.take() {
                out.sections.push(section);
            }
            current = Some(LlmsSection {
                heading: rest.to_string(),
                links: Vec::new(),
            });
            continue;
        }
        // An H3 or deeper stays inside the section it appears in; the
        // convention gives it no meaning and dropping the links under it would
        // lose real entries.
        if heading(trimmed, 3).is_some() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            // Only the blockquote before the first section is the summary.
            if current.is_none() {
                summary_lines.push(rest.trim().to_string());
            }
            continue;
        }

        if let Some(link) = parse_link(trimmed) {
            current
                .get_or_insert_with(LlmsSection::default)
                .links
                .push(link);
        }
    }

    if let Some(section) = current.take() {
        out.sections.push(section);
    }

    let summary = summary_lines
        .iter()
        .filter(|l| !l.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if !summary.is_empty() {
        out.summary = Some(summary);
    }

    out
}

/// The text of a heading at exactly this level.
fn heading(line: &str, level: usize) -> Option<&str> {
    let hashes = "#".repeat(level);
    let rest = line.strip_prefix(&hashes)?;
    // `##` must not match a `###`.
    if rest.starts_with('#') {
        return None;
    }
    let rest = rest.trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// Parse one `- [Title](url): note` list item.
///
/// The bullet is optional, because files in the wild sometimes list bare links.
/// The note separator may be a colon, an en or em dash, or a plain hyphen.
fn parse_link(line: &str) -> Option<LlmsLink> {
    let after_bullet = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
        .unwrap_or(line)
        .trim_start();

    let rest = after_bullet.strip_prefix('[')?;
    let close = rest.find("](")?;
    let title = rest[..close].trim().to_string();

    let after_title = &rest[close + 2..];
    let end = after_title.find(')')?;
    let url = after_title[..end].trim();
    if url.is_empty() {
        return None;
    }
    // A Markdown link may carry a title after the href: [x](/a "the a page").
    let url = url.split_whitespace().next().unwrap_or(url).to_string();

    let tail = after_title[end + 1..].trim();
    let tail = tail
        .strip_prefix(':')
        .or_else(|| tail.strip_prefix('—'))
        .or_else(|| tail.strip_prefix('–'))
        .or_else(|| tail.strip_prefix('-'))
        .unwrap_or(tail)
        .trim();
    let note = if tail.is_empty() {
        None
    } else {
        Some(tail.to_string())
    };

    Some(LlmsLink { title, url, note })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Micaforge

> Open-source, cookieless analytics that treats machine readers as a first-class
> audience.

Some free prose that is not part of the summary structure.

## Docs

- [Quickstart](https://micaforge.example/docs/quickstart): running in a minute
- [Agent analytics](/docs/agents) — the wedge, explained
- [API](https://micaforge.example/docs/api.md)

## Optional

- [Changelog](https://micaforge.example/changelog)
";

    #[test]
    fn the_skeleton_parses() {
        let llms = parse(SAMPLE);
        assert_eq!(llms.title.as_deref(), Some("Micaforge"));
        assert_eq!(
            llms.summary.as_deref(),
            Some("Open-source, cookieless analytics that treats machine readers as a first-class audience.")
        );
        assert_eq!(llms.sections.len(), 2);
        assert_eq!(llms.sections[0].heading, "Docs");
        assert_eq!(llms.sections[0].links.len(), 3);
        assert_eq!(llms.sections[1].heading, "Optional");
        assert_eq!(llms.link_count(), 4);
        assert!(!llms.is_empty());
    }

    #[test]
    fn notes_are_separated_from_titles() {
        let llms = parse(SAMPLE);
        let first = &llms.sections[0].links[0];
        assert_eq!(first.title, "Quickstart");
        assert_eq!(first.url, "https://micaforge.example/docs/quickstart");
        assert_eq!(first.note.as_deref(), Some("running in a minute"));

        let second = &llms.sections[0].links[1];
        assert_eq!(second.url, "/docs/agents");
        assert_eq!(second.note.as_deref(), Some("the wedge, explained"));

        let third = &llms.sections[0].links[2];
        assert_eq!(third.note, None);
    }

    #[test]
    fn paths_come_out_comparable() {
        let llms = parse(SAMPLE);
        assert!(llms.lists_path("/docs/quickstart"));
        assert!(llms.lists_path("/docs/quickstart/"));
        assert!(llms.lists_path("/docs/agents"));
        // The `.md` sibling convention.
        assert!(llms.lists_path("/docs/api"));
        assert!(llms.lists_path("/docs/api.md"));
        assert!(!llms.lists_path("/pricing"));
        assert_eq!(
            llms.section_for("/changelog").map(|s| s.heading.as_str()),
            Some("Optional")
        );
    }

    #[test]
    fn a_code_fence_cannot_forge_a_section() {
        let md = "# T\n\n## Real\n\n```sh\n# Not a title\n## Not a section\n- [x](/y)\n```\n\n- [a](/a)\n";
        let llms = parse(md);
        assert_eq!(llms.title.as_deref(), Some("T"));
        assert_eq!(llms.sections.len(), 1);
        assert_eq!(llms.sections[0].heading, "Real");
        assert_eq!(llms.sections[0].links.len(), 1);
        assert_eq!(llms.sections[0].links[0].url, "/a");
    }

    #[test]
    fn links_before_any_section_are_kept() {
        let llms = parse("# T\n\n- [a](/a)\n");
        assert_eq!(llms.sections.len(), 1);
        assert_eq!(llms.sections[0].heading, "");
        assert!(llms.lists_path("/a"));
    }

    #[test]
    fn an_h3_does_not_open_a_section_but_keeps_its_links() {
        let llms = parse("# T\n\n## S\n\n### Sub\n\n- [a](/a)\n");
        assert_eq!(llms.sections.len(), 1);
        assert_eq!(llms.sections[0].heading, "S");
        assert_eq!(llms.sections[0].links.len(), 1);
    }

    #[test]
    fn an_empty_file_is_empty_not_a_failure() {
        let llms = parse("");
        assert!(llms.is_empty());
        assert!(!llms.lists_path("/anything"));
        assert_eq!(llms.title, None);
    }

    #[test]
    fn a_link_with_a_markdown_title_attribute_still_resolves() {
        let llms = parse("## S\n- [a](/a \"the a page\")\n");
        assert_eq!(llms.sections[0].links[0].url, "/a");
    }

    #[test]
    fn a_bare_domain_link_resolves_to_the_root() {
        let link = LlmsLink {
            title: "Home".into(),
            url: "https://example.com".into(),
            note: None,
        };
        assert_eq!(link.path(), "/");
    }
}
