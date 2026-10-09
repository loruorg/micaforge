//! The row of the `search_console` table: what Google Search Console reports
//! for one site's property, per day.
//!
//! One row is one line of a `searchAnalytics.query` answer at one **grain**,
//! the set of dimensions it was asked for:
//!
//! - `total`: by date only. The property's own daily totals, the figures the
//!   Search Console screen shows.
//! - `query`: by date, query, page, country and device. The table every
//!   question about search terms is answered from.
//! - `page`, `country`, `device`: by date and that one dimension.
//!
//! Five grains rather than one because Google drops anonymised queries from
//! any answer grouped by query, and counts an impression once per page rather
//! than once per result list when grouped by page. Summing the `query` grain
//! therefore under-reports clicks (by a third and more on a small site), and
//! the totals, pages, countries and devices are each read from the grain that
//! was asked for them, so every figure matches what Google itself shows.
//!
//! Kept in its own file rather than at the end of `rows.rs` so that the agents
//! adding tables in parallel do not edit the same lines.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// A row in `search_console`.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct SearchConsoleRow {
    /// Numeric site id.
    pub site_id: u16,
    /// The Search Console property the row came from, exactly as Google names
    /// it (`sc-domain:example.com`, `https://example.com/`). Part of the key,
    /// so a site that switches properties never mixes the two.
    pub property: String,
    /// `total` | `query` | `page` | `country` | `device`; see the module notes.
    pub grain: String,
    /// The day, in Google's calendar (Pacific time).
    #[serde(with = "clickhouse::serde::chrono::date")]
    pub date: NaiveDate,
    /// The search terms, or empty at a grain without them.
    pub query: String,
    /// The URL Google reports, or empty at a grain without it.
    pub page: String,
    /// The page's host, lower-cased, for the join with Micaforge's own rows.
    pub hostname: String,
    /// The page's path, normalised the way ingest normalises a pageview's.
    pub pathname: String,
    /// ISO 3166-1 alpha-2, upper case, or empty when Google could not say or
    /// at a grain without it. Google sends alpha-3; see the sync.
    pub country: String,
    /// `desktop` | `mobile` | `tablet`, or empty at a grain without it.
    pub device: String,
    /// Clicks from Google Search to the site.
    pub clicks: u32,
    /// Times a result for the site was shown.
    pub impressions: u32,
    /// Google's average position for the row (1 is the top). Averaged across
    /// rows weighted by impressions, never plainly.
    pub position: f64,
    /// When this copy was fetched. The `ReplacingMergeTree` version, and what
    /// reads use to keep only the newest fetch of each day.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub synced_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_match_the_schema() {
        use clickhouse::Row;
        let schema: Vec<&str> = super::super::TABLES
            .iter()
            .find(|t| t.name == "search_console")
            .map(|t| t.columns.iter().map(|(n, _)| *n).collect())
            .expect("table");
        assert_eq!(SearchConsoleRow::COLUMN_NAMES, schema);
    }
}
