//! Reading and writing `page_index`, the content register.
//!
//! Three jobs write to this table — the sitemap walker, the page crawler and the
//! policy crawler — and each of them knows only part of a row. `page_index` is a
//! `ReplacingMergeTree(updated_at)`, which means an insert replaces the **whole**
//! row rather than the columns it names, so a naive write from the sitemap walker
//! would erase the title and word count the page crawler filled in an hour
//! earlier.
//!
//! Everything here exists to stop that. A job [`load`]s the site's current rows,
//! [`Patch`]es the fields it actually learned, and writes back a complete row.
//! `first_seen` in particular is carried forward: it is the answer to "how long
//! has this URL existed", and re-crawling a site must not reset it to today.
//!
//! The register is also why a page with no human traffic is still visible. The
//! Citation Gap joins agent fetches and answer-engine arrivals against *this*
//! table rather than against `events`, so a page that agents crawl and nobody
//! reads has a row, a number, and a place in the report.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use micaforge_store::ch::{Ch, PageIndexRow};

/// Every known page for one site, keyed by path.
///
/// `FINAL` is not optional on a `ReplacingMergeTree`: without it a path that has
/// been written twice comes back twice, once with each version, and a merge
/// operation the job then does on "the current row" would be done on whichever
/// copy arrived first. The table holds one row per URL of one site, so the cost
/// of `FINAL` here is a few thousand rows, not a scan.
pub async fn load(ch: &Ch, site_id: u16) -> Result<HashMap<String, PageIndexRow>> {
    let sql = format!(
        "SELECT ?fields FROM {}.page_index FINAL WHERE site_id = ?",
        ch.db
    );
    let rows: Vec<PageIndexRow> = ch
        .client()
        .query(&sql)
        .bind(site_id)
        .fetch_all()
        .await
        .map_err(|e| Error::Database(format!("clickhouse read page_index: {e}")))?;

    Ok(rows
        .into_iter()
        .map(|row| (row.pathname.clone(), row))
        .collect())
}

/// Write complete rows back.
///
/// Returns how many rows were written. An empty slice is not an error and does
/// not open a connection: a crawl that found nothing new is a normal night.
pub async fn write(ch: &Ch, rows: &[PageIndexRow]) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }

    let mut insert = ch
        .client()
        .insert::<PageIndexRow>("page_index")
        .map_err(|e| Error::Database(format!("clickhouse insert page_index: {e}")))?;

    for row in rows {
        insert
            .write(row)
            .await
            .map_err(|e| Error::Database(format!("clickhouse write page_index: {e}")))?;
    }

    insert
        .end()
        .await
        .map_err(|e| Error::Database(format!("clickhouse commit page_index: {e}")))?;

    Ok(rows.len())
}

/// The fields one job learned about one page.
///
/// Every field is optional, and `None` means "I did not find this out", never
/// "this is empty". That distinction is the whole point: the sitemap walker knows
/// nothing about word count and must not write a zero over one.
#[derive(Debug, Clone, Default)]
pub struct Patch {
    /// The page's `<title>`.
    pub title: Option<String>,
    /// `Last-Modified` as served, or `lastmod` from the sitemap.
    pub last_modified: Option<DateTime<Utc>>,
    /// Words of extracted body text.
    pub word_count: Option<u32>,
    /// Whether the path appears in the site's sitemap.
    pub in_sitemap: Option<bool>,
    /// Whether the path appears in the site's `llms.txt`.
    pub in_llms_txt: Option<bool>,
    /// The `robots.txt` rule that covers this path, as written.
    pub robots_directive: Option<String>,
}

impl Patch {
    /// Apply this patch to the row a site already has, or to a new one.
    ///
    /// `now` becomes `updated_at`, which is the `ReplacingMergeTree` version
    /// column, so the row this returns is the one that wins. It also becomes
    /// `first_seen` only when there was no existing row.
    pub fn apply(
        &self,
        existing: Option<&PageIndexRow>,
        site_id: u16,
        pathname: &str,
        now: DateTime<Utc>,
    ) -> PageIndexRow {
        let mut row = match existing {
            Some(row) => row.clone(),
            None => PageIndexRow {
                site_id,
                pathname: pathname.to_string(),
                title: String::new(),
                first_seen: now,
                last_modified: None,
                word_count: 0,
                in_sitemap: 0,
                in_llms_txt: 0,
                robots_directive: String::new(),
                updated_at: now,
            },
        };

        if let Some(title) = &self.title {
            row.title = title.clone();
        }
        if let Some(last_modified) = self.last_modified {
            row.last_modified = Some(last_modified);
        }
        if let Some(words) = self.word_count {
            row.word_count = words;
        }
        if let Some(in_sitemap) = self.in_sitemap {
            row.in_sitemap = u8::from(in_sitemap);
        }
        if let Some(in_llms) = self.in_llms_txt {
            row.in_llms_txt = u8::from(in_llms);
        }
        if let Some(directive) = &self.robots_directive {
            row.robots_directive = directive.clone();
        }

        row.updated_at = now;
        row
    }
}

/// Reduce a URL to the path this table keys on.
///
/// Query strings and fragments are dropped: `page_index` is a register of
/// *pages*, and `/docs?utm_source=x` is the same page as `/docs`. The path is
/// then normalised the same way the ingest path normalises a pageview's
/// `pathname`, because a register whose keys do not match the event stream's
/// keys would join against nothing.
pub fn path_of(url: &url::Url) -> String {
    micaforge_enrich::url_norm::normalise_path(url.path(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(iso: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_patch_never_erases_what_another_job_learned() {
        let first = at("2026-01-01T00:00:00Z");
        let later = at("2026-08-21T00:00:00Z");

        let crawled = Patch {
            title: Some("Guide".into()),
            word_count: Some(1_200),
            ..Patch::default()
        }
        .apply(None, 1, "/docs/guide", first);

        assert_eq!(crawled.first_seen, first);
        assert_eq!(crawled.word_count, 1_200);

        // The sitemap walker knows only that the page is listed.
        let listed = Patch {
            in_sitemap: Some(true),
            ..Patch::default()
        }
        .apply(Some(&crawled), 1, "/docs/guide", later);

        assert_eq!(listed.title, "Guide", "the title survived a sitemap pass");
        assert_eq!(listed.word_count, 1_200, "the word count survived");
        assert_eq!(listed.in_sitemap, 1);
        // The thing that must never move.
        assert_eq!(listed.first_seen, first);
        assert_eq!(listed.updated_at, later);
    }

    #[test]
    fn a_false_is_written_and_a_none_is_not() {
        let now = at("2026-08-21T00:00:00Z");
        let listed = Patch {
            in_sitemap: Some(true),
            in_llms_txt: Some(true),
            ..Patch::default()
        }
        .apply(None, 1, "/x", now);

        // A page dropped from the sitemap has to stop being marked as in it.
        let dropped = Patch {
            in_sitemap: Some(false),
            ..Patch::default()
        }
        .apply(Some(&listed), 1, "/x", now);
        assert_eq!(dropped.in_sitemap, 0);
        assert_eq!(dropped.in_llms_txt, 1, "an untouched flag is untouched");
    }

    #[test]
    fn a_url_reduces_to_the_page_the_register_keys_on() {
        let url = url::Url::parse("https://example.com/docs/guide/?utm_source=x#top").unwrap();
        assert_eq!(path_of(&url), "/docs/guide");
        let root = url::Url::parse("https://example.com").unwrap();
        assert_eq!(path_of(&root), "/");
    }
}
