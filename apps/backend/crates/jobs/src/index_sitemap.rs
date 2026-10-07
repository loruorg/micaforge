//! `index_sitemap` — discover a site's pages from its own sitemap.
//!
//! A sitemap is the only place a site *declares* what it publishes. Everything
//! else Micaforge knows about a site's URLs is observed: a page appears in
//! `events` when a person visits it and in `agent_events` when a crawler takes
//! it. The sitemap is what fills in the rest, and the rest is the interesting
//! part — a page an agent crawled and nobody visited, or a page nobody has
//! touched at all, only has a row in `page_index` because this job put one there.
//!
//! # What it handles, because the wild demands it
//!
//! - **Sitemap indexes**, walked to [`MAX_DEPTH`] so a nested index cannot become
//!   a cycle.
//! - **Gzip**, both as a transfer encoding and as a `.xml.gz` *file*. See
//!   [`crate::fetch`].
//! - **`Sitemap:` lines in `robots.txt`**, which is the discovery mechanism the
//!   protocol actually specifies, tried before the conventional
//!   `/sitemap.xml`.
//! - **Cross-host entries**, which are ignored. A sitemap may legitimately list
//!   URLs on another host; they are not this site's pages and counting them would
//!   put another property's URLs in this property's register.
//!
//! # The cap is visible
//!
//! At [`MAX_URLS`] the walk stops and says so, in the log and in the job's own
//! summary. A silent truncation would leave an operator with a content register
//! that is quietly missing half their site and no way to find out. When the walk
//! *is* truncated the job also declines to clear the `in_sitemap` flag on pages it
//! did not reach, because "I did not get that far" must never be recorded as "the
//! site removed this page".
//!
//! # No XML library
//!
//! [`extract_entries`] is a scanner over `<loc>` and `<lastmod>` rather than a
//! parser. That is a deliberate trade: the sitemaps schema is two element names
//! deep, a scanner cannot be talked into an entity-expansion bomb or an external
//! entity fetch, and the alternative was adding an XML dependency to the
//! workspace for one job. What it costs is tolerance of genuinely malformed
//! documents, which it answers by finding fewer URLs rather than by failing.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use micaforge_core::Result;
use micaforge_store::ch::PageIndexRow;
use micaforge_store::models::{catalog, sites};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fetch::{self, SITEMAP_MAX_BYTES};
use crate::pages::{self, Patch};
use crate::JobCtx;

/// Most URLs one site's register takes from a sitemap in one run.
///
/// Fifty thousand is the sitemaps protocol's own per-document limit, and it is
/// well past the size at which a content register stops being something a person
/// reads. A site larger than this is served by the page-class rollup rather than
/// by a URL list.
pub const MAX_URLS: usize = 50_000;

/// Most sitemap documents fetched for one site in one run.
pub const MAX_DOCS: usize = 100;

/// How deep a chain of sitemap indexes may nest.
///
/// An index of indexes is legitimate. An index that lists itself is not, and this
/// is the bound that makes the difference not matter.
pub const MAX_DEPTH: usize = 3;

/// An index of indexes is a legitimate shape for a large site to publish, so the
/// walk has to be able to follow at least two levels. Asserted at compile time,
/// because lowering this to one would not fail a test — it would just quietly
/// index a fraction of some customer's site.
const _: () = assert!(MAX_DEPTH >= 2);

/// Which sites to index.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    /// One site, by `sites.id`. Absent means every site.
    #[serde(default)]
    pub site: Option<Uuid>,
}

/// One URL a sitemap declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The URL, as written.
    pub loc: String,
    /// Its `lastmod`, when it had one.
    pub lastmod: Option<DateTime<Utc>>,
}

/// Walk one site's sitemaps, or every site's.
pub async fn run(ctx: &JobCtx, payload: Payload) -> Result<()> {
    let targets = match payload.site {
        Some(id) => vec![sites::get(&ctx.pg, id).await?],
        None => sites::list_all(&ctx.pg).await?,
    };

    let mut indexed = 0usize;
    for site in &targets {
        match index_one(ctx, site).await {
            Ok(count) => indexed += count,
            Err(e) => tracing::warn!(site = %site.domain, error = %e, "sitemap walk failed"),
        }
    }

    tracing::info!(
        sites = targets.len(),
        pages = indexed,
        "sitemap sweep finished"
    );
    Ok(())
}

/// Walk one site. Returns how many pages were registered.
async fn index_one(ctx: &JobCtx, site: &sites::Site) -> Result<usize> {
    let roots = discover(ctx, site).await?;
    if roots.is_empty() {
        tracing::debug!(site = %site.domain, "no sitemap found");
        return Ok(0);
    }

    let host = fetch::site_url(&site.domain, "/")?;
    let host = host.host_str().unwrap_or_default().to_ascii_lowercase();

    let mut found: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();
    let mut seen_docs: HashSet<String> = HashSet::new();
    let mut queue: Vec<(String, usize)> = roots.into_iter().map(|url| (url, 0)).collect();
    let mut pace = fetch::Pace::per_second();
    let mut truncated = false;
    let mut docs = 0usize;

    while let Some((url, depth)) = queue.pop() {
        if docs >= MAX_DOCS || found.len() >= MAX_URLS {
            truncated = true;
            break;
        }
        if !seen_docs.insert(url.clone()) {
            continue;
        }

        pace.tick().await;
        let doc = match fetch::get(&ctx.http, &url, SITEMAP_MAX_BYTES).await {
            Ok(Some(doc)) => doc,
            Ok(None) => continue,
            Err(e) => {
                // One unreachable sitemap in an index of forty must not cost the
                // other thirty-nine.
                tracing::debug!(url, error = %e, "sitemap not readable");
                continue;
            }
        };
        docs += 1;

        let entries = extract_entries(&doc.body);
        if is_index(&doc.body) {
            if depth >= MAX_DEPTH {
                tracing::warn!(url, depth, "sitemap index nested deeper than we follow");
                continue;
            }
            for entry in entries {
                queue.push((entry.loc, depth + 1));
            }
            continue;
        }

        for entry in entries {
            if found.len() >= MAX_URLS {
                truncated = true;
                break;
            }
            let Ok(parsed) = url::Url::parse(&entry.loc) else {
                continue;
            };
            // A sitemap may list another host. Those are not this site's pages.
            if parsed.host_str().unwrap_or_default().to_ascii_lowercase() != host {
                continue;
            }
            found.insert(pages::path_of(&parsed), entry.lastmod);
        }
    }

    if truncated {
        // Loud on purpose. An operator whose register silently stopped at half
        // their site would have no way to know the number they are reading is
        // partial.
        tracing::warn!(
            site = %site.domain,
            pages = found.len(),
            documents = docs,
            max_urls = MAX_URLS,
            max_documents = MAX_DOCS,
            "sitemap walk truncated at the cap; the content register is partial for this site"
        );
    }

    let written = store(ctx, site, &found, truncated).await?;
    tracing::info!(
        site = %site.domain,
        documents = docs,
        pages = found.len(),
        written,
        truncated,
        "sitemap indexed"
    );
    Ok(written)
}

/// Where to start looking.
///
/// The stored `robots.txt` first, because [`crate::crawl_site_policy`] has
/// usually already fetched it and re-reading somebody's server for a file we hold
/// is rude. Then a live `robots.txt`, then the two conventional locations.
async fn discover(ctx: &JobCtx, site: &sites::Site) -> Result<Vec<String>> {
    let mut roots: Vec<String> = Vec::new();

    if let Some(stored) = catalog::get_policy(&ctx.pg, site.id).await? {
        if let Some(text) = &stored.robots_txt {
            roots.extend(micaforge_agents::robots::parse(text).sitemaps);
        }
    }

    if roots.is_empty() {
        let url = fetch::site_url(&site.domain, "/robots.txt")?;
        if let Ok(Some(doc)) = fetch::get(&ctx.http, url.as_str(), fetch::ROBOTS_MAX_BYTES).await {
            roots.extend(micaforge_agents::robots::parse(&doc.body).sitemaps);
        }
    }

    if roots.is_empty() {
        for path in ["/sitemap.xml", "/sitemap_index.xml"] {
            roots.push(fetch::site_url(&site.domain, path)?.to_string());
        }
    }

    // Guard every discovered URL: a `Sitemap:` line is text from somebody else's
    // server, and it is the one place in this job where a URL we did not build
    // ourselves enters.
    roots.retain(|url| match fetch::guard_url(url) {
        Ok(_) => true,
        Err(e) => {
            tracing::debug!(url, error = %e, "declined a sitemap URL");
            false
        }
    });
    roots.dedup();
    Ok(roots)
}

/// Write what was found into the content register.
///
/// Pages that were listed before and are not listed now have their `in_sitemap`
/// flag cleared — but only when the walk completed. A truncated walk has no
/// opinion about the pages it never reached, and recording one would turn a cap
/// into a deletion.
async fn store(
    ctx: &JobCtx,
    site: &sites::Site,
    found: &HashMap<String, Option<DateTime<Utc>>>,
    truncated: bool,
) -> Result<usize> {
    let site_id = site.site_id_u16();
    let existing = pages::load(&ctx.ch, site_id).await?;
    let now = Utc::now();
    let mut rows: Vec<PageIndexRow> = Vec::with_capacity(found.len());

    for (path, lastmod) in found {
        let patch = Patch {
            in_sitemap: Some(true),
            last_modified: *lastmod,
            ..Patch::default()
        };
        rows.push(patch.apply(existing.get(path), site_id, path, now));
    }

    if !truncated {
        let cleared = Patch {
            in_sitemap: Some(false),
            ..Patch::default()
        };
        for (path, row) in &existing {
            if row.in_sitemap == 1 && !found.contains_key(path) {
                rows.push(cleared.apply(Some(row), site_id, path, now));
            }
        }
    }

    pages::write(&ctx.ch, &rows).await
}

/// Is this document a sitemap index rather than a sitemap?
///
/// Tested on the root element name. A document that is both would be invalid, and
/// one that is neither yields no entries anyway.
pub fn is_index(xml: &str) -> bool {
    xml.contains("<sitemapindex") || xml.contains(":sitemapindex")
}

/// Pull `<loc>` and `<lastmod>` pairs out of a sitemap document.
///
/// A `<lastmod>` belongs to the `<loc>` that precedes it, which is how every
/// sitemap in the wild is ordered and what the schema's sequence implies. A
/// `<lastmod>` with no preceding `<loc>` is dropped rather than attached to the
/// next one.
pub fn extract_entries(xml: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut rest = xml;

    while let Some(start) = find_tag(rest, "loc") {
        let after_open = &rest[start.after_open..];
        let Some(close) = find_close(after_open, "loc") else {
            break;
        };
        let loc = unescape(&after_open[..close]);
        let tail = &after_open[close..];

        // `lastmod` for this entry is the one before the next `<loc>`, if any.
        let boundary = find_tag(tail, "loc")
            .map(|next| next.before_open)
            .unwrap_or(tail.len());
        let lastmod = find_tag(&tail[..boundary], "lastmod").and_then(|m| {
            let after = &tail[..boundary][m.after_open..];
            let end = find_close(after, "lastmod")?;
            fetch::parse_sitemap_date(&unescape(&after[..end]))
        });

        if !loc.trim().is_empty() {
            entries.push(Entry {
                loc: loc.trim().to_string(),
                lastmod,
            });
        }
        rest = tail;
    }

    entries
}

/// Where an opening tag starts and where its content starts.
struct TagAt {
    before_open: usize,
    after_open: usize,
}

/// Find the next `<name>` or `<ns:name>` opening tag.
///
/// Namespace prefixes are common in sitemap documents and carry no meaning here,
/// so the match is on the local name preceded by `<` or `:`.
fn find_tag(haystack: &str, name: &str) -> Option<TagAt> {
    let mut from = 0usize;
    while let Some(idx) = haystack[from..].find('<') {
        let open = from + idx;
        let after = &haystack[open + 1..];
        // Skip the namespace prefix, when there is one.
        let local = match after.find(':') {
            Some(colon) if colon < after.find('>').unwrap_or(usize::MAX) => &after[colon + 1..],
            _ => after,
        };
        if local.starts_with(name) {
            let next = local.as_bytes().get(name.len()).copied();
            // `<loc>` and `<loc xml:lang="x">` match; `<location>` does not.
            if matches!(
                next,
                Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
            ) {
                if let Some(gt) = haystack[open..].find('>') {
                    return Some(TagAt {
                        before_open: open,
                        after_open: open + gt + 1,
                    });
                }
            }
        }
        from = open + 1;
    }
    None
}

/// Offset of the matching `</name>` within `haystack`.
fn find_close(haystack: &str, name: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(idx) = haystack[from..].find("</") {
        let open = from + idx;
        let after = &haystack[open + 2..];
        let local = match after.find(':') {
            Some(colon) if colon < after.find('>').unwrap_or(usize::MAX) => &after[colon + 1..],
            _ => after,
        };
        if local.starts_with(name) {
            // `</loc>` closes a `loc`; `</location>` does not.
            let next = local.as_bytes().get(name.len()).copied();
            if matches!(
                next,
                Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
            ) {
                return Some(open);
            }
        }
        from = open + 2;
    }
    None
}

/// Decode the entity forms a sitemap may use, and unwrap `CDATA`.
///
/// The sitemaps protocol requires `&amp; &apos; &quot; &lt; &gt;` in URLs, so a
/// document that does not decode them yields query strings with `&amp;` in them
/// and a register full of URLs that do not exist. Numeric references are decoded
/// too because generators emit them.
fn unescape(raw: &str) -> String {
    let raw = raw.trim();
    let raw = match raw.strip_prefix("<![CDATA[") {
        Some(inner) => inner.strip_suffix("]]>").unwrap_or(inner),
        None => raw,
    };

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
        let entity = &after[1..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => match decode_numeric(entity) {
                Some(c) => out.push(c),
                // An entity nobody defined is left as written rather than
                // dropped: it may be part of the URL.
                None => out.push_str(&after[..=end]),
            },
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Decode `#38` or `#x26`.
fn decode_numeric(entity: &str) -> Option<char> {
    let digits = entity.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u32>().ok()?,
    };
    char::from_u32(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URLSET: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url>
    <loc>https://example.com/docs/guide</loc>
    <lastmod>2026-08-01</lastmod>
    <changefreq>weekly</changefreq>
  </url>
  <url>
    <loc>https://example.com/pricing?a=1&amp;b=2</loc>
  </url>
  <url>
    <loc><![CDATA[https://example.com/blog/hello]]></loc>
    <lastmod>2026-08-21T09:30:00+01:00</lastmod>
  </url>
</urlset>"#;

    const INDEX: &str = r#"<?xml version="1.0"?>
<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/sitemap-1.xml.gz</loc><lastmod>2026-08-20</lastmod></sitemap>
  <sitemap><loc>https://example.com/sitemap-2.xml</loc></sitemap>
</sitemapindex>"#;

    #[test]
    fn a_urlset_yields_its_urls_and_their_dates() {
        let entries = extract_entries(URLSET);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].loc, "https://example.com/docs/guide");
        assert!(entries[0].lastmod.is_some());

        // The entity forms the protocol requires must decode, or the register
        // fills up with URLs that do not exist.
        assert_eq!(entries[1].loc, "https://example.com/pricing?a=1&b=2");
        // A `lastmod` belongs to its own entry, never to the next one.
        assert!(entries[1].lastmod.is_none());

        assert_eq!(entries[2].loc, "https://example.com/blog/hello");
        assert!(entries[2].lastmod.is_some());
    }

    #[test]
    fn an_index_is_told_apart_from_a_urlset() {
        assert!(is_index(INDEX));
        assert!(!is_index(URLSET));
        let entries = extract_entries(INDEX);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].loc, "https://example.com/sitemap-1.xml.gz");
    }

    #[test]
    fn namespaced_elements_are_matched_on_the_local_name() {
        let xml = "<sm:urlset><sm:url><sm:loc>https://example.com/a</sm:loc></sm:url></sm:urlset>";
        let entries = extract_entries(xml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].loc, "https://example.com/a");
    }

    #[test]
    fn an_element_that_merely_starts_with_loc_is_not_a_loc() {
        let xml =
            "<urlset><url><location>nope</location><loc>https://example.com/a</loc></url></urlset>";
        let entries = extract_entries(xml);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].loc, "https://example.com/a");
    }

    #[test]
    fn junk_yields_nothing_rather_than_panicking() {
        assert!(extract_entries("").is_empty());
        assert!(extract_entries("not xml at all").is_empty());
        // An unterminated element must not loop or index past the end.
        assert!(extract_entries("<urlset><url><loc>https://example.com/a").is_empty());
        assert!(extract_entries("<loc></loc>").is_empty());
    }

    #[test]
    fn entities_decode_including_the_numeric_forms() {
        assert_eq!(unescape("a&amp;b"), "a&b");
        assert_eq!(unescape("a&#38;b"), "a&b");
        assert_eq!(unescape("a&#x26;b"), "a&b");
        assert_eq!(unescape("&lt;x&gt;"), "<x>");
        assert_eq!(unescape("<![CDATA[/a/b]]>"), "/a/b");
        // A bare ampersand, which is invalid but appears anyway, survives.
        assert_eq!(unescape("a&b"), "a&b");
        // An entity nobody defined is kept rather than silently dropped.
        assert_eq!(unescape("a&nbsp;b"), "a&nbsp;b");
    }

    #[test]
    fn the_cap_is_the_protocols_own() {
        // The sitemaps protocol's per-document limit, and the number the
        // truncation notice quotes back to the operator.
        assert_eq!(MAX_URLS, 50_000);
    }
}
