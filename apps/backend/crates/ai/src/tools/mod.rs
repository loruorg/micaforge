//! The tools the model may call, and the rules every one of them obeys.
//!
//! ## Adding a tool
//!
//! Write one file in this directory that exposes
//! `pub fn tool() -> Box<dyn Tool>`. The build script finds it, declares the
//! module and adds it to [`Registry::builtin`]; nothing else is edited. Look at
//! `overview.rs` for the smallest complete example, and at `goals.rs` for one
//! that reads Postgres as well as ClickHouse.
//!
//! ## What a tool may assume, and what it must do
//!
//! - **The site is already authorised.** [`ToolCtx::site`] is the one site the
//!   caller may read; a tool never takes a site argument and never reads
//!   another site's rows.
//! - **The caller's filters are a floor, not a default.** [`ToolCtx::envelope`]
//!   merges the filters the caller had applied with any the model adds, so the
//!   model can narrow the caller's view and never widen it.
//! - **Nothing identifies a person.** Visitor ids and identified ids are refused
//!   as a dimension and as a filter ([`refuse_personal`]): those rows would go
//!   to a third-party model, and a breakdown by visitor is a list of people.
//! - **Results are small.** A tool trims what it returns to the model
//!   ([`MAX_ROWS`]); a context window is not an export.
//! - **It says what it ran.** [`ToolOutput::request`] is the equivalent stats
//!   request, so a person can run the same read; the SQL is recorded for them
//!   by the query engine while the tool runs.

use std::sync::Arc;

use async_trait::async_trait;
use chrono_tz::Tz;
use micaforge_core::{
    filters, time, Audience, Compare, Config, Dimension, Error, FilterGroup, Granularity,
    QueryEnvelope, Result,
};
use micaforge_query::exec::RecordedQuery;
use micaforge_store::models::sites::Site;
use micaforge_store::{Ch, PgPool};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value as Json};

use crate::llm::ToolSpec;

include!(concat!(env!("OUT_DIR"), "/tools.rs"));

/// Most rows a tool hands back to the model.
pub const MAX_ROWS: u32 = 50;

/// The dimensions no tool may read or filter on. See the module notes.
const PERSONAL: &[&str] = &["visitor_id", "identified_id"];

/// One tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls it by. `snake_case`, unique.
    fn name(&self) -> &'static str;
    /// When to use it, written for the model choosing between tools.
    fn description(&self) -> &'static str;
    /// JSON Schema of the arguments (an object).
    fn parameters(&self) -> Json;
    /// Run it.
    async fn run(&self, ctx: &ToolCtx, args: &Json) -> Result<ToolOutput>;
}

/// What a tool returns.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// What the model reads.
    pub data: Json,
    /// The equivalent stats request, such as
    /// `GET /api/stats/breakdown?site=3&range=7d&dimension=source`.
    pub request: String,
    /// How many rows came back, for a list.
    pub rows: Option<usize>,
}

/// One tool call, as stored with an answer and shown under it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolTrace {
    /// The tool's name.
    pub tool: String,
    /// The arguments the model sent.
    pub arguments: Json,
    /// Whether it succeeded.
    pub ok: bool,
    /// Why it failed, in a sentence safe to show.
    pub error: Option<String>,
    /// The equivalent stats request.
    pub request: String,
    /// The SQL each read ran.
    pub queries: Vec<RecordedQuery>,
    /// How many rows came back.
    pub rows: Option<usize>,
    /// How long it took.
    pub took_ms: u64,
}

/// The caller's view: the window, the filters and the audience they were
/// looking at when they asked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BaseQuery {
    /// A named range, such as `7d`.
    #[serde(default)]
    pub range: Option<String>,
    /// An explicit start.
    #[serde(default)]
    pub start: Option<String>,
    /// An explicit end.
    #[serde(default)]
    pub end: Option<String>,
    /// The timezone the window is cut in. Defaults to the site's.
    #[serde(default)]
    pub tz: Option<String>,
    /// The caller's filters. Every tool keeps them.
    #[serde(default)]
    pub filters: Option<FilterGroup>,
    /// The population the caller was looking at.
    #[serde(default)]
    pub audience: Option<Audience>,
}

/// Everything a tool may reach.
#[derive(Clone)]
pub struct ToolCtx {
    /// The event store.
    pub ch: Ch,
    /// Configuration rows: goals, funnels, annotations.
    pub pg: PgPool,
    /// An outbound client, for tools that read another service.
    pub http: reqwest::Client,
    /// Process configuration.
    pub cfg: Arc<Config>,
    /// The one site this conversation is about, already authorised.
    pub site: Site,
    /// The caller's view.
    pub base: BaseQuery,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("site", &self.site.site_id)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

impl ToolCtx {
    /// The numeric site id every report keys on.
    pub fn site_id(&self) -> u16 {
        self.site.site_id_u16()
    }

    /// The timezone the window is cut in.
    pub fn tz(&self) -> Tz {
        self.base
            .tz
            .as_deref()
            .and_then(|name| name.parse::<Tz>().ok())
            .or_else(|| self.site.timezone.parse::<Tz>().ok())
            .unwrap_or(chrono_tz::UTC)
    }

    /// The query envelope for one call.
    ///
    /// The window is the model's when it named one and the caller's otherwise;
    /// the audience likewise. The filters are both: the caller's and the
    /// model's, joined with AND, and checked for anything personal.
    pub fn envelope(&self, args: &Json) -> Result<QueryEnvelope> {
        let (range, start, end) = match string_arg(args, "range") {
            Some(range) => (Some(range), None, None),
            None => match string_arg(args, "start") {
                Some(start) => (None, Some(start), string_arg(args, "end")),
                None => (
                    self.base.range.clone(),
                    self.base.start.clone(),
                    self.base.end.clone(),
                ),
            },
        };
        let resolved = time::resolve_range(
            range.as_deref(),
            start.as_deref(),
            end.as_deref(),
            self.tz(),
        )?;
        let mut env = QueryEnvelope::new(self.site_id(), resolved);

        if let Some(raw) = string_arg(args, "compare") {
            env.compare = Compare::parse(&raw)?;
        }
        if let Some(raw) = string_arg(args, "granularity") {
            env.granularity = time::fit_granularity(&env.range, Granularity::parse(&raw)?);
        }
        env.audience = match string_arg(args, "audience") {
            Some(raw) => Audience::parse(&raw)?,
            None => self.base.audience.unwrap_or_default(),
        };

        let added = match args.get("filters").filter(|v| !v.is_null()) {
            Some(raw) => {
                let text = serde_json::to_string(raw)
                    .map_err(|e| Error::validation("filters", format!("could not be read: {e}")))?;
                Some(filters::parse_filters(&text)?)
            }
            None => None,
        };
        let merged = filters::merge(self.base.filters.clone(), added);
        if let Some(group) = merged.as_ref() {
            group.validate()?;
            for dimension in group.dimensions() {
                refuse_personal(&dimension)?;
            }
        }
        env.filters = merged.filter(|g| !g.is_empty());

        let limit = args
            .get("limit")
            .and_then(Json::as_u64)
            .unwrap_or(u64::from(MAX_ROWS / 2));
        env.limit = limit.clamp(1, u64::from(MAX_ROWS)) as u32;
        Ok(env)
    }
}

/// Refuse a dimension that names a person.
pub fn refuse_personal(dimension: &Dimension) -> Result<()> {
    let name = dimension.as_str();
    if PERSONAL.contains(&name.as_str()) {
        return Err(Error::validation(
            "dimension",
            format!(
                "`{name}` identifies individual visitors and is not available to the AI analyst"
            ),
        ));
    }
    Ok(())
}

/// A trimmed, non-empty string argument.
pub fn string_arg(args: &Json, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// The equivalent stats request for an envelope and a few extra parameters.
///
/// The window is written as the explicit start and end the envelope resolved
/// to, not the named range: "7d" read tomorrow is a different week, and this
/// string is meant to reproduce the read that produced the answer.
pub fn stats_request(path: &str, env: &QueryEnvelope, extra: &[(&str, String)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("site", &env.site.to_string());
    query.append_pair("start", &env.range.start.to_rfc3339());
    query.append_pair("end", &env.range.end.to_rfc3339());
    query.append_pair("tz", env.range.tz.name());
    if env.audience != Audience::Human {
        query.append_pair("audience", env.audience.as_str());
    }
    if env.compare != Compare::None {
        query.append_pair("compare", env.compare.as_str());
    }
    if let Some(group) = env.filters.as_ref() {
        if let Ok(text) = filters::serialize_filters(group) {
            query.append_pair("filters", &text);
        }
    }
    for (key, value) in extra {
        query.append_pair(key, value);
    }
    query.append_pair("limit", &env.limit.to_string());
    format!("GET {path}?{}", query.finish())
}

/// Serialise a report for the model, keeping at most [`MAX_ROWS`] list items.
pub fn to_data<T: Serialize>(value: &T) -> Result<Json> {
    let mut data = serde_json::to_value(value)?;
    trim_lists(&mut data, MAX_ROWS as usize);
    Ok(data)
}

/// Cut every array in a document down to `max` items, saying so where it did.
fn trim_lists(value: &mut Json, max: usize) {
    match value {
        Json::Array(items) => {
            items.truncate(max);
            for item in items {
                trim_lists(item, max);
            }
        }
        Json::Object(map) => {
            for item in map.values_mut() {
                trim_lists(item, max);
            }
        }
        _ => {}
    }
}

/// The properties every envelope-taking tool accepts, for its schema.
pub fn envelope_properties() -> Map<String, Json> {
    let mut props = Map::new();
    props.insert(
        "range".into(),
        json!({
            "type": "string",
            "description": "A named window in the site's timezone: today, yesterday, 24h, 7d, 30d, 90d, 12mo, mtd, ytd or all. Leave it out to use the window the person is looking at."
        }),
    );
    props.insert(
        "start".into(),
        json!({ "type": "string", "description": "ISO-8601 start, instead of `range`." }),
    );
    props.insert(
        "end".into(),
        json!({ "type": "string", "description": "ISO-8601 end, with `start`." }),
    );
    props.insert(
        "compare".into(),
        json!({ "type": "string", "enum": ["none", "previous", "year"], "description": "Report the previous period (or the same period a year ago) beside this one." }),
    );
    props.insert(
        "audience".into(),
        json!({ "type": "string", "enum": ["human", "agent", "all"], "description": "human = people in browsers (the default), agent = named AI crawlers and bots, all = both." }),
    );
    props.insert(
        "filters".into(),
        json!({
            "type": "array",
            "description": "Extra conditions, all of which must hold, added to the ones the person already applied. Each is {dimension, op, value}; op is one of is, is_not, contains, not_contains, starts_with, ends_with, gt, lt, gte, lte, matches, is_set, is_not_set.",
            "items": { "type": "object" }
        }),
    );
    props.insert(
        "limit".into(),
        json!({ "type": "integer", "minimum": 1, "maximum": MAX_ROWS, "description": "Most rows to return." }),
    );
    props
}

/// A JSON Schema object over the envelope properties and a tool's own.
pub fn schema(extra: &[(&str, Json)], required: &[&str]) -> Json {
    let mut props = envelope_properties();
    for (key, value) in extra {
        props.insert((*key).to_string(), value.clone());
    }
    json!({
        "type": "object",
        "properties": Json::Object(props),
        "required": required,
    })
}

/// The tools one conversation may use.
pub struct Registry {
    tools: Vec<Box<dyn Tool>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.tools.iter().map(|t| t.name()))
            .finish()
    }
}

impl Registry {
    /// Every tool in this directory.
    pub fn builtin() -> Registry {
        Registry {
            tools: discovered(),
        }
    }

    /// A registry of exactly these tools. For tests, and for a caller that
    /// wants a narrower set.
    pub fn of(tools: Vec<Box<dyn Tool>>) -> Registry {
        Registry { tools }
    }

    /// The tools, as the model is shown them.
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
    }

    /// Find a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.as_ref())
    }

    /// The tool names, in order.
    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_tool_has_a_unique_name_and_an_object_schema() {
        let registry = Registry::builtin();
        let names = registry.names();
        assert!(
            names.len() >= 10,
            "the analyst needs its reports: {names:?}"
        );
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "tool names must be unique");
        for spec in registry.specs() {
            assert_eq!(spec.parameters["type"], "object", "{}", spec.name);
            assert!(
                spec.description.len() > 40,
                "{} needs a real description",
                spec.name
            );
        }
        for expected in [
            "get_overview",
            "get_timeseries",
            "get_breakdown",
            "get_pages",
            "get_sources",
            "get_agent_traffic",
            "get_citation_gap",
            "get_goals",
            "get_funnel",
            "get_retention",
            "list_annotations",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn lists_are_cut_for_the_model() {
        let mut doc = json!({ "rows": (0..80).collect::<Vec<_>>(), "nested": [[1, 2, 3]] });
        trim_lists(&mut doc, 5);
        assert_eq!(doc["rows"].as_array().unwrap().len(), 5);
        assert_eq!(doc["nested"][0].as_array().unwrap().len(), 3);
    }

    #[test]
    fn personal_dimensions_are_refused() {
        assert!(refuse_personal(&Dimension::parse("visitor_id").unwrap()).is_err());
        assert!(refuse_personal(&Dimension::parse("identified_id").unwrap()).is_err());
        assert!(refuse_personal(&Dimension::parse("source").unwrap()).is_ok());
    }
}
