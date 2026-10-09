//! `get_search_queries`: what people searched on Google before they arrived.
//!
//! Reads Postgres for the Search Console property the site chose and
//! ClickHouse for Google's figures, the same reads `/api/stats/search/*`
//! makes (`micaforge_query::search`). Three views in one tool, so the model
//! learns one name: `queries` (search terms, the default), `pages` (with
//! Micaforge's own visitors on the same path beside Google's clicks) and
//! `overview` (Google's totals with the comparison window).
//!
//! The caller's filters are event filters, and Search Console rows have none
//! of those columns, so they cannot narrow these figures; the answer says so
//! rather than letting the model believe they applied. A site that has not
//! connected Search Console gets a note saying exactly that, not an error the
//! model would retry.

use async_trait::async_trait;
use micaforge_core::{Error, Result};
use micaforge_query::search::{self, DateWindow, ListQuery, QueryFilter, SearchScope, SortBy};
use micaforge_store::models::search_console;
use serde_json::{json, Value as Json};

use super::{schema, stats_request, string_arg, to_data, Tool, ToolCtx, ToolOutput};

struct SearchQueries;

/// The tool, for the registry.
pub fn tool() -> Box<dyn Tool> {
    Box::new(SearchQueries)
}

#[async_trait]
impl Tool for SearchQueries {
    fn name(&self) -> &'static str {
        "get_search_queries"
    }

    fn description(&self) -> &'static str {
        "Google Search data from Google Search Console: the search terms people typed before the \
         site showed up in Google, with clicks, impressions, click-through rate (ctr, a fraction) \
         and average position (1 is the top). view=queries (default) lists search terms; narrow \
         with query, path, country (two letters) or device. view=pages lists the pages that showed \
         up, with Micaforge's own visitors on the same page beside Google's clicks. view=overview \
         gives Google's totals for the window, with compare=previous for the change. Sort by \
         impressions to find terms the site shows up for but rarely wins the click. Use it for \
         questions about SEO, Google rankings or what people search for. Google reports whole days \
         and revises the last three. If the answer says Search Console is not connected, say so; \
         do not guess."
    }

    fn parameters(&self) -> Json {
        schema(
            &[
                (
                    "view",
                    json!({ "type": "string", "enum": ["queries", "pages", "overview"], "description": "What to return. Defaults to queries." }),
                ),
                (
                    "query",
                    json!({ "type": "string", "description": "With view=queries: only search terms containing this text." }),
                ),
                (
                    "path",
                    json!({ "type": "string", "description": "Only pages whose path or address contains this text." }),
                ),
                (
                    "country",
                    json!({ "type": "string", "description": "With view=queries: only searches from this country, two letters such as US." }),
                ),
                (
                    "device",
                    json!({ "type": "string", "enum": ["desktop", "mobile", "tablet"], "description": "With view=queries: only searches from this device." }),
                ),
                (
                    "sort",
                    json!({ "type": "string", "enum": ["clicks", "impressions", "ctr", "position"], "description": "What to rank by. Defaults to clicks." }),
                ),
            ],
            &[],
        )
    }

    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput> {
        let env = ctx.envelope(args)?;
        let view = string_arg(args, "view").unwrap_or_else(|| "queries".to_string());
        let path = match view.as_str() {
            "queries" => "/api/stats/search/queries",
            "pages" => "/api/stats/search/pages",
            "overview" => "/api/stats/search/overview",
            other => {
                return Err(Error::validation(
                    "view",
                    format!("`{other}` is not one of queries, pages, overview"),
                ))
            }
        };
        let request = stats_request(path, &env, &[]);

        let property = search_console::find(&ctx.pg, ctx.site.id)
            .await?
            .and_then(|c| c.property);
        let Some(property) = property else {
            return Ok(ToolOutput {
                data: json!({
                    "note": "Google Search Console is not connected for this site, so there is no Google Search data. A site admin connects it on the Search screen."
                }),
                request,
                rows: Some(0),
            });
        };

        let scope = SearchScope {
            site: ctx.site_id(),
            property: &property,
        };
        let window = DateWindow::of_range(&env.range);
        let list = ListQuery {
            filter: QueryFilter {
                query: string_arg(args, "query"),
                page: string_arg(args, "path"),
                country: string_arg(args, "country"),
                device: string_arg(args, "device"),
            }
            .checked()?,
            sort: SortBy::parse(string_arg(args, "sort").as_deref())?,
            limit: env.limit,
            page: 1,
        };
        let (report, rows) = match view.as_str() {
            "overview" => {
                let previous = search::previous_window(&env.range, env.compare);
                let report = search::overview(&ctx.ch, scope, window, previous).await?;
                (to_data(&report)?, None)
            }
            "pages" => {
                let pages = search::pages(&ctx.ch, scope, window, &list).await?;
                let paths: Vec<String> = pages.iter().map(|r| r.pathname.clone()).collect();
                let traffic = search::page_traffic(&ctx.ch, ctx.site_id(), &env.range, &paths).await?;
                let joined = search::join_pages(pages, &traffic);
                (to_data(&joined)?, Some(joined.len()))
            }
            _ => {
                let terms = search::queries(&ctx.ch, scope, window, &list).await?;
                (to_data(&terms)?, Some(terms.len()))
            }
        };

        let mut data = json!({ "property": property, "window": window, "report": report });
        if env.filters.is_some() {
            data["note"] = json!(
                "The filters in effect apply to Micaforge's own events, not to Google's Search Console figures, which are unfiltered."
            );
        }
        Ok(ToolOutput {
            data,
            request,
            rows,
        })
    }
}
