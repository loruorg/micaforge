//! One page, spelled one way.
//!
//! `/docs/api`, `/docs/api/`, `/docs//api` and `/docs/api/index.html` are the
//! same page to a reader and four different rows in a naive pageviews report.
//! Normalisation is what stops the top-pages table from splitting a site's best
//! page across four entries, and it has to happen at ingest: once four spellings
//! are on disk, no query can safely merge them, because on some sites the
//! trailing slash genuinely is a different page.
//!
//! What is deliberately *not* normalised: case. Path case is significant on
//! every Unix-backed server, `/Docs` and `/docs` can be two different documents,
//! and lowercasing them would be a silent data loss. Host case is handled at the
//! ingest boundary, where the host is a separate column.

/// Longest path stored.
///
/// Past this a path is not a page, it is a probe or a broken redirect loop.
/// Storing the prefix keeps the row (a 404 scan is worth seeing in the report)
/// without letting one client dictate the width of a column.
const MAX_PATH_LEN: usize = 2048;

/// Directory index files folded into their directory.
const INDEX_FILES: &[&str] = &["index.html", "index.htm", "index.php", "index.jsp"];

/// Normalise one URL path.
///
/// `trailing_slash` states whether the site treats a trailing slash as
/// significant:
///
/// - `true` — keep it. `/docs` and `/docs/` stay two pages. Correct for a site
///   that serves different content at the two, and the safe choice when nothing
///   is known.
/// - `false` — strip it. `/docs/` becomes `/docs`, which is what most static
///   site generators and most frameworks actually serve.
///
/// The root path is always `/`, under either setting.
///
/// ```
/// # use micaforge_enrich::url_norm::normalise_path;
/// assert_eq!(normalise_path("/docs//api/index.html", false), "/docs/api");
/// assert_eq!(normalise_path("/docs/api/", true), "/docs/api/");
/// assert_eq!(normalise_path("", false), "/");
/// ```
pub fn normalise_path(path: &str, trailing_slash: bool) -> String {
    // Defensive: the caller should pass a path, but a full URL or a path with
    // its query still attached turns up often enough from server-side ingest
    // and from hand-written API calls.
    let mut raw = path.trim();
    if let Some(cut) = raw.find(['?', '#']) {
        raw = &raw[..cut];
    }
    if raw.is_empty() {
        return "/".to_string();
    }

    // Collapse repeated separators, dropping the empty segments they produce.
    let mut normalised = String::with_capacity(raw.len() + 1);
    let mut segments = raw.split('/').filter(|s| !s.is_empty()).peekable();
    let mut last_segment = "";
    while let Some(segment) = segments.next() {
        normalised.push('/');
        normalised.push_str(segment);
        if segments.peek().is_none() {
            last_segment = segment;
        }
    }

    // A directory index is the directory. Done after collapsing so that
    // `/docs//index.html` is caught too.
    if INDEX_FILES
        .iter()
        .any(|f| f.eq_ignore_ascii_case(last_segment))
    {
        // Drop the index file and the separator in front of it.
        normalised.truncate(normalised.len() - last_segment.len() - 1);
        if normalised.is_empty() {
            return "/".to_string();
        }
        if trailing_slash {
            normalised.push('/');
        }
    } else if trailing_slash && raw.ends_with('/') {
        normalised.push('/');
    }

    if normalised.is_empty() {
        normalised.push('/');
    }

    clamp(&normalised, MAX_PATH_LEN)
}

/// A coarse guess at the template a path was rendered from.
///
/// `/docs/getting-started` and `/docs/deploy/docker` both become `/docs/*`.
/// This is what makes the content report readable on a documentation site,
/// where a hundred article URLs are one template and the useful question is
/// "how is `/docs` doing" rather than "how is `/docs/deploy/docker` doing" —
/// and it is what the Citation Gap groups by when a site has more URLs than a
/// person can read.
///
/// It is a guess, and it is coarse on purpose. Anything past the first path
/// segment collapses, an identifier-looking first segment collapses too, and
/// the exact path is still stored on every row for anyone who wants it.
///
/// ```
/// # use micaforge_enrich::url_norm::page_class;
/// assert_eq!(page_class("/docs/getting-started"), "/docs/*");
/// assert_eq!(page_class("/pricing"), "/pricing");
/// assert_eq!(page_class("/"), "/");
/// ```
pub fn page_class(path: &str) -> String {
    let mut segments = path.split('/').filter(|s| !s.is_empty());

    let Some(first) = segments.next() else {
        return "/".to_string();
    };
    let head = if looks_like_id(first) { "*" } else { first };

    if segments.next().is_none() {
        format!("/{head}")
    } else {
        format!("/{head}/*")
    }
}

/// Does this segment look like an identifier rather than a name?
///
/// Three shapes cover almost everything a real site produces: a number
/// (`/orders/1024`), a UUID (`/sessions/018f…`), and a long hex or base32 token
/// (`/r/9f3a1c7e5b2d`). A slug with words in it is left alone, because a slug
/// is the page's name.
fn looks_like_id(segment: &str) -> bool {
    if segment.is_empty() {
        return false;
    }
    if segment.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if segment.len() == 36 && segment.matches('-').count() == 4 {
        return segment.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit());
    }
    segment.len() >= 12 && segment.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Cut a string to `max` bytes without splitting a character.
fn clamp(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_path_is_the_root() {
        assert_eq!(normalise_path("", false), "/");
        assert_eq!(normalise_path("   ", true), "/");
        assert_eq!(normalise_path("/", false), "/");
        assert_eq!(normalise_path("/", true), "/");
    }

    #[test]
    fn repeated_separators_collapse() {
        assert_eq!(normalise_path("//docs///api", false), "/docs/api");
        assert_eq!(normalise_path("docs/api", false), "/docs/api");
    }

    #[test]
    fn a_directory_index_is_its_directory() {
        assert_eq!(normalise_path("/docs/index.html", false), "/docs");
        assert_eq!(normalise_path("/docs/index.html", true), "/docs/");
        assert_eq!(normalise_path("/index.html", false), "/");
        assert_eq!(normalise_path("/docs//INDEX.HTM", false), "/docs");
        assert_eq!(normalise_path("/docs/index.php", false), "/docs");
    }

    #[test]
    fn the_trailing_slash_flag_is_honoured_both_ways() {
        assert_eq!(normalise_path("/docs/api/", true), "/docs/api/");
        assert_eq!(normalise_path("/docs/api/", false), "/docs/api");
        assert_eq!(normalise_path("/docs/api", true), "/docs/api");
    }

    #[test]
    fn a_query_or_fragment_is_not_part_of_the_path() {
        assert_eq!(normalise_path("/docs?utm_source=hn", false), "/docs");
        assert_eq!(normalise_path("/docs#install", false), "/docs");
    }

    #[test]
    fn case_is_preserved_because_paths_are_case_sensitive() {
        assert_eq!(normalise_path("/Docs/API", false), "/Docs/API");
    }

    #[test]
    fn a_path_is_bounded() {
        let long = format!("/{}", "a".repeat(9000));
        assert_eq!(normalise_path(&long, false).len(), MAX_PATH_LEN);
    }

    #[test]
    fn page_class_collapses_below_the_first_segment() {
        assert_eq!(page_class("/docs/getting-started"), "/docs/*");
        assert_eq!(page_class("/docs/deploy/docker"), "/docs/*");
        assert_eq!(page_class("/blog/2024/why-agents"), "/blog/*");
    }

    #[test]
    fn page_class_keeps_a_single_named_segment() {
        assert_eq!(page_class("/pricing"), "/pricing");
        assert_eq!(page_class("/"), "/");
        assert_eq!(page_class(""), "/");
    }

    #[test]
    fn page_class_collapses_an_identifier_at_the_root() {
        assert_eq!(page_class("/1024"), "/*");
        assert_eq!(page_class("/018f3a2b-4c5d-6e7f-8a9b-0c1d2e3f4a5b"), "/*");
        assert_eq!(page_class("/9f3a1c7e5b2d/edit"), "/*/*");
    }

    #[test]
    fn a_word_slug_is_never_an_identifier() {
        assert!(!looks_like_id("getting-started"));
        assert!(!looks_like_id("deploy"));
        assert!(!looks_like_id("v2"));
        assert!(looks_like_id("1024"));
        assert!(looks_like_id("deadbeefcafe"));
    }
}
