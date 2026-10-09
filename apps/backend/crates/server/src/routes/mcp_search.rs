//! MCP reads over Google Search Console: `get_search_overview`,
//! `get_search_queries` and `get_search_breakdown`.
//!
//! Site-scoped like the monitoring reads: [`super::mcp`] authorises the site
//! before [`call`] is asked, and every tool reads that site's chosen property
//! through the same functions `/api/stats/search/*` calls
//! (`micaforge_query::search`). A site with no property connected answers a
//! `not_found` that says so, rather than an empty report that would read as
//! "nobody searched".

use micaforge_core::{Error, Result};
use micaforge_query::search::{self, DateWindow, ListQuery, QueryFilter, SearchScope, SortBy};
use micaforge_store::models::sites::Site;
use serde_json::{json, Value};

use super::mcp::{envelope_from_args, envelope_tool, string_argument};
use super::search_console::chosen_property;
use crate::state::AppState;

/// Rows a list returns when the call did not say.
const DEFAULT_ROWS: u32 = 50;

/// The note every description ends with: what of the envelope applies.
const WINDOW_NOTE: &str = "Google reports whole days in Pacific time and revises the last three; \
`range`, `start`, `end` and `compare` choose the days. `audience`, `filters` and `granularity` do not apply \
to Search Console data.";

/// This module's tools.
pub fn tools() -> Vec<Value> {
    let sort = json!({
        "type": "string",
        "enum": ["clicks", "impressions", "ctr", "position"],
        "default": "clicks",
        "description": "Rank by most clicks, most impressions, highest click-through rate, or best (lowest) average position."
    });
    vec![
        envelope_tool(
            "get_search_overview",
            &format!(
                "Google Search for the site from Google Search Console: clicks, impressions, click-through rate and \
average position over the window, each with its change against the comparison window, and one point per day. These are \
Google's own totals and match the Search Console screen. {WINDOW_NOTE}"
            ),
            &[],
            &[],
        ),
        envelope_tool(
            "get_search_queries",
            &format!(
                "The search terms people typed into Google before the site showed up, with clicks, impressions, \
click-through rate and average position per term. Narrow by `query` (terms containing a text), `path` (a page path or \
address containing a text), `country` (two-letter code) or `device` (desktop, mobile, tablet). Sort by impressions to find \
terms that show the site often but are rarely clicked. Google leaves out rare terms it anonymises, so these rows add up to \
less than the overview's totals. {WINDOW_NOTE}"
            ),
            &[
                ("query", json!({ "type": "string", "description": "Only terms containing this text." })),
                ("path", json!({ "type": "string", "description": "Only rows for pages whose path or address contains this text." })),
                ("country", json!({ "type": "string", "description": "Only searches from this country: ISO 3166-1 alpha-2, such as US." })),
                ("device", json!({ "type": "string", "enum": ["desktop", "mobile", "tablet"] })),
                ("sort", sort.clone()),
            ],
            &[],
        ),
        envelope_tool(
            "get_search_breakdown",
            &format!(
                "Google Search split by `dimension`: `page` (each URL that showed up, with Micaforge's own visitors, \
pageviews and visitors from Google Search on the same path beside Google's clicks), `country` or `device`. Each row has \
clicks, impressions, click-through rate and average position, from Google's own figures for that split. {WINDOW_NOTE}"
            ),
            &[
                ("dimension", json!({ "type": "string", "enum": ["page", "country", "device"], "description": "How to split. Required." })),
                ("path", json!({ "type": "string", "description": "With `page`: only pages whose path or address contains this text." })),
                ("sort", sort),
            ],
            &["dimension"],
        ),
    ]
}

/// Run one of this module's tools on an authorised site, or `None` for a
/// name it does not own.
pub async fn call(st: &AppState, site: &Site, name: &str, args: &Value) -> Option<Result<Value>> {
    let done = match name {
        "get_search_overview" => overview(st, site, args).await,
        "get_search_queries" => list(st, site, args, "query").await,
        "get_search_breakdown" => match string_argument(args, "dimension") {
            Some(d @ ("page" | "country" | "device")) => list(st, site, args, d).await,
            Some(other) => Err(Error::validation(
                "dimension",
                format!("`{other}` is not one of page, country, device"),
            )),
            None => Err(Error::validation(
                "dimension",
                "is required: page, country or device",
            )),
        },
        _ => return None,
    };
    Some(done)
}

/// The list arguments, checked.
fn list_query(args: &Value, limit: Option<u32>, page: u32) -> Result<ListQuery> {
    let text = |key: &str| string_argument(args, key).map(str::to_string);
    Ok(ListQuery {
        filter: QueryFilter {
            query: text("query"),
            page: text("path"),
            country: text("country"),
            device: text("device"),
        }
        .checked()?,
        sort: SortBy::parse(string_argument(args, "sort"))?,
        limit: limit.unwrap_or(DEFAULT_ROWS),
        page,
    })
}

async fn overview(st: &AppState, site: &Site, args: &Value) -> Result<Value> {
    let property = chosen_property(st, site).await?;
    let env = envelope_from_args(site, args)?;
    let scope = SearchScope {
        site: site.site_id_u16(),
        property: &property,
    };
    let window = DateWindow::of_range(&env.range);
    let previous = search::previous_window(&env.range, env.compare);
    let totals = search::overview(&st.ch, scope, window, previous).await?;
    let days = search::timeseries(&st.ch, scope, window).await?;
    Ok(json!({ "property": property, "overview": totals, "days": days }))
}

async fn list(st: &AppState, site: &Site, args: &Value, dimension: &str) -> Result<Value> {
    let property = chosen_property(st, site).await?;
    let env = envelope_from_args(site, args)?;
    let asked = args.get("limit").and_then(Value::as_u64).map(|_| env.limit);
    let list = list_query(args, asked, env.page)?;
    let scope = SearchScope {
        site: site.site_id_u16(),
        property: &property,
    };
    let window = DateWindow::of_range(&env.range);
    let rows = match dimension {
        "query" => serde_json::to_value(search::queries(&st.ch, scope, window, &list).await?)?,
        "page" => {
            let pages = search::pages(&st.ch, scope, window, &list).await?;
            let paths: Vec<String> = pages.iter().map(|r| r.pathname.clone()).collect();
            let traffic =
                search::page_traffic(&st.ch, site.site_id_u16(), &env.range, &paths).await?;
            serde_json::to_value(search::join_pages(pages, &traffic))?
        }
        "country" => serde_json::to_value(search::countries(&st.ch, scope, window, &list).await?)?,
        _ => serde_json::to_value(search::devices(&st.ch, scope, window, &list).await?)?,
    };
    Ok(json!({ "property": property, "window": window, "rows": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_requires_a_site_and_says_what_applies() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "get_search_overview",
                "get_search_queries",
                "get_search_breakdown"
            ]
        );
        for tool in &tools {
            let description = tool["description"].as_str().unwrap();
            assert!(description.contains("Pacific time"));
            assert!(tool["inputSchema"]["required"]
                .as_array()
                .is_some_and(|r| r.contains(&json!("site"))));
        }
        assert!(tools[2]["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("dimension")));
    }

    #[test]
    fn list_arguments_are_checked_before_any_read() {
        let ok = list_query(
            &json!({ "country": "us", "device": "Mobile", "sort": "impressions" }),
            None,
            1,
        )
        .unwrap();
        assert_eq!(ok.filter.country.as_deref(), Some("US"));
        assert_eq!(ok.sort, SortBy::Impressions);
        assert_eq!(ok.limit, DEFAULT_ROWS);
        assert_eq!(
            list_query(&json!({ "sort": "bounce" }), None, 1)
                .unwrap_err()
                .field(),
            Some("sort")
        );
        assert_eq!(
            list_query(&json!({ "device": "watch" }), None, 1)
                .unwrap_err()
                .field(),
            Some("device")
        );
        assert_eq!(list_query(&json!({}), Some(7), 2).unwrap().limit, 7);
    }
}
