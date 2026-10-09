//! MCP tools for monitoring and growth: uptime, short links and revenue.
//!
//! The same rule as every other tool in [`super::mcp`]: each one is a second
//! front door onto a report the dashboard already reads, answered by the same
//! function the HTTP route calls, never a second implementation. All six are
//! reads, scoped to one site the key may reach; [`super::mcp`] authorises the
//! site before [`call`] is asked.

use micaforge_core::{Error, Result};
use micaforge_store::models::links::{self as link_store, FolderFilter, Scope};
use micaforge_store::models::sites::Site;
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::mcp::{envelope_from_args, envelope_tool};
use super::revenue::ReportQuery;
use crate::state::AppState;

/// The tool descriptors, appended to the list `tools/list` answers.
pub fn tools() -> Vec<Value> {
    let site_only =
        |name: &str, description: &str, extra: Map<String, Value>, required: &[&str]| {
            let mut props = Map::new();
            props.insert(
                "site".to_string(),
                json!({
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 65535,
                    "description": "Numeric site id, from `list_sites`. Required."
                }),
            );
            props.extend(extra);
            let mut needed = vec![json!("site")];
            needed.extend(required.iter().map(|r| json!(r)));
            json!({
                "name": name,
                "description": description,
                "inputSchema": {
                    "type": "object",
                    "properties": Value::Object(props),
                    "required": needed,
                    "additionalProperties": false
                }
            })
        };
    let one = |key: &str, schema: Value| {
        let mut m = Map::new();
        m.insert(key.to_string(), schema);
        m
    };

    vec![
        site_only(
            "get_uptime_summary",
            "The site's uptime monitors: each one's state (up, down or not checked yet), uptime over \
24 hours, 7 days and 30 days as fractions, the last response time, the days left on its TLS \
certificate, and how many incidents are open. Monitor URLs are included.",
            Map::new(),
            &[],
        ),
        site_only(
            "list_uptime_incidents",
            "The site's uptime incidents, newest first: which monitor went down, when, when it came \
back (null while still down), how long it lasted and the error that opened it.",
            one(
                "limit",
                json!({ "type": "integer", "minimum": 1, "maximum": 500, "description": "Rows to return. Defaults to 50." }),
            ),
            &[],
        ),
        site_only(
            "list_links",
            "The site's short links: slug, short URL, target, UTM parameters, expiry and clicks by \
people in the last 30 days. Use the `id` with `get_link_stats`.",
            one(
                "q",
                json!({ "type": "string", "description": "Only links whose slug, target or title contains this text." }),
            ),
            &[],
        ),
        site_only(
            "get_link_stats",
            "Clicks on one short link over a window: totals split into people and bots, a series, \
and the top referrers, countries, devices, browsers and served targets (web, ios, android, \
deep_link, preview, expired).",
            {
                let mut m = one(
                    "link",
                    json!({ "type": "string", "description": "The link's id, from `list_links`. Required." }),
                );
                m.insert(
                    "range".to_string(),
                    json!({ "type": "string", "enum": ["24h", "7d", "30d", "90d"], "description": "Defaults to 7d." }),
                );
                m
            },
            &["link"],
        ),
        envelope_tool(
            "get_revenue_overview",
            "Revenue recorded from Stripe and Paddle webhooks over the window, one row per currency \
(amounts in minor units and never summed across currencies): gross, refunds, net, payments, \
refunds count, customers, and the share of payments attributed to a visit.",
            &[(
                "currency",
                json!({ "type": "string", "description": "Only this ISO 4217 currency, upper case." }),
            )],
            &[],
        ),
        envelope_tool(
            "get_revenue_breakdown",
            "Net revenue over the window broken down by where the paying visit came from: channel, \
source, referrer_host, utm_source, utm_medium, utm_campaign, country, device_type, browser, \
entry_path or attribution. Amounts are in minor units, one row per value and currency.",
            &[
                (
                    "dimension",
                    json!({
                        "type": "string",
                        "enum": ["channel", "source", "referrer_host", "utm_source", "utm_medium",
                                 "utm_campaign", "country", "device_type", "browser", "entry_path",
                                 "attribution"],
                        "description": "Defaults to channel."
                    }),
                ),
                (
                    "currency",
                    json!({ "type": "string", "description": "Only this ISO 4217 currency, upper case." }),
                ),
            ],
            &[],
        ),
    ]
}

/// Run one of this module's tools, or `None` for a name it does not own.
///
/// `site` has already been authorised for the calling key's user by
/// [`super::mcp`]; every tool here reads that site and nothing else, so the
/// user is not needed again.
pub async fn call(
    st: &AppState,
    _user: Uuid,
    site: &Site,
    name: &str,
    args: &Value,
) -> Option<Result<Value>> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let outcome = match name {
        "get_uptime_summary" => super::uptime::summary_value(st, site).await,
        "list_uptime_incidents" => {
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(50);
            super::uptime::incidents_value(st, site.id, limit).await
        }
        "list_links" => {
            super::links::links_value(st, Scope::Site(site.id), FolderFilter::Any, text("q")).await
        }
        "get_link_stats" => link_stats(st, site, text("link"), text("range")).await,
        "get_revenue_overview" | "get_revenue_breakdown" => {
            let query = ReportQuery {
                currency: text("currency").map(str::to_string),
                dimension: text("dimension").map(str::to_string),
            };
            match envelope_from_args(site, args) {
                Ok(env) if name == "get_revenue_overview" => {
                    super::revenue::overview_value(st, site.id, &env, &query).await
                }
                Ok(env) => super::revenue::breakdown_value(st, &env, &query).await,
                Err(e) => Err(e),
            }
        }
        _ => return None,
    };
    Some(outcome)
}

/// `get_link_stats`: the link must be one of this site's.
async fn link_stats(
    st: &AppState,
    site: &Site,
    link: Option<&str>,
    range: Option<&str>,
) -> Result<Value> {
    let id: Uuid = link
        .ok_or_else(|| Error::validation("link", "is required; call `list_links` first"))?
        .parse()
        .map_err(|_| Error::validation("link", "is not a link id"))?;
    let found = link_store::get(&st.pg, id).await?;
    if found.site_id != Some(site.id) {
        return Err(Error::not_found("link"));
    }
    super::links::stats_value(st, &found, range, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_requires_a_site_and_describes_itself() {
        let tools = tools();
        assert_eq!(tools.len(), 6);
        for tool in &tools {
            assert!(tool["description"].as_str().is_some_and(|d| d.len() > 40));
            assert!(tool["inputSchema"]["required"]
                .as_array()
                .is_some_and(|r| r.contains(&json!("site"))));
        }
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "get_uptime_summary",
                "list_uptime_incidents",
                "list_links",
                "get_link_stats",
                "get_revenue_overview",
                "get_revenue_breakdown"
            ]
        );
    }
}
