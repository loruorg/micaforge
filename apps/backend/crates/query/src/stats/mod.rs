//! One function per "Analytics read" endpoint in `docs/CONTRACT.md` §3.3.
//!
//! Every function takes `(&Ch, &QueryEnvelope, ..)` and returns a
//! serde-serialisable result. Nothing here reads Postgres: a goal, a funnel and
//! a segment are configuration, and the caller passes them in already loaded.
//! That keeps the engine testable against ClickHouse alone and keeps the HTTP
//! layer the only place that knows what a site's configuration is.
//!
//! ## Filtering and the session rollup
//!
//! Reports that mix event-scoped and session-scoped numbers do not read
//! `sessions_mv`. The stored rollup carries no `event_name`, no `props.*` and no
//! `page_title`, so it cannot answer a filtered question at all. Instead these
//! reports rebuild the rollup from the filtered event stream with
//! [`crate::sql::sessions_subquery`], under the rollup's own column names, and
//! then apply [`micaforge_core::Metric::select_expr`] to it unchanged. A number
//! is either measured over exactly the rows the filter left, or it is not
//! reported.

mod behaviour;
mod content;
mod diagnostics;
mod frustration;
mod overview;
mod people;
mod profiles;
mod sampling;

pub use behaviour::{
    event_properties, events, funnel, goals, journeys, retention, Cohort, EventProperty,
    EventRowAgg, Funnel, FunnelStep, GoalRow, Journey, Link, Node, PropValue, Retention,
};
pub use content::{
    breakdown, devices, locations, pages, sources, BreakdownRow, CampaignRow, ChannelRow,
    DeviceRow, Devices, GeoRow, Locations, PageRow, SourceRow, SourcesTree,
};
pub use diagnostics::{
    error_detail, error_facets, error_groups, error_totals, errors, performance, ErrorDetail,
    ErrorFacet, ErrorFacets, ErrorGroup, ErrorNarrow, ErrorSession, ErrorTotals, PerfRating,
    PerfRow,
};
pub use frustration::{
    frustration, AbandonedForm, FrustratedElement, FrustratedPage, Frustration, LastField,
};
pub use overview::{
    overview, realtime, timeseries, Bucket, LiveRow, Overview, OverviewDelta, Realtime, RecentHit,
    Series,
};
pub use people::{
    session_detail, sessions, visitor_detail, visitors, Hit, SessionDetail, SessionRow,
    VisitorDetail, VisitorRow,
};
pub use profiles::{
    profile, search_profiles, AnonymousSession, LinkedVisitor, Profile, ProfileHit, TraitChange,
    MIN_QUERY,
};
pub use sampling::{sampling, Sampling};

use micaforge_core::{Error, FilterGroup, Result, Table};
use serde::{Deserialize, Serialize};

use crate::sql::{compile_filter_group, Param};

// ---------------------------------------------------------------------------
// Goal definitions
// ---------------------------------------------------------------------------

/// What kind of event can satisfy a goal.
///
/// The kind is not decoration: it decides which slice of the event stream is
/// even eligible, before the goal's own conditions narrow it. A `path` goal that
/// matched a custom event named `/pricing` would be a bug nobody would ever spot
/// in the numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalKind {
    /// A pageview whose path matches.
    Path,
    /// A custom event.
    Event,
    /// A click on a link leaving the site.
    Outbound,
    /// A file download.
    File,
    /// Any event carrying revenue.
    Revenue,
}

impl GoalKind {
    /// The wire spelling, matching the `goals.kind` check constraint.
    pub fn as_str(&self) -> &'static str {
        match self {
            GoalKind::Path => "path",
            GoalKind::Event => "event",
            GoalKind::Outbound => "outbound",
            GoalKind::File => "file",
            GoalKind::Revenue => "revenue",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "path" => Ok(GoalKind::Path),
            "event" => Ok(GoalKind::Event),
            "outbound" => Ok(GoalKind::Outbound),
            "file" => Ok(GoalKind::File),
            "revenue" => Ok(GoalKind::Revenue),
            other => Err(Error::validation(
                "kind",
                format!("unknown goal kind `{other}`"),
            )),
        }
    }

    /// The predicate that selects the events this kind of goal can be met by.
    pub fn base_predicate(&self) -> &'static str {
        match self {
            GoalKind::Path => "kind = 'pageview'",
            GoalKind::Event => "kind = 'custom'",
            GoalKind::Outbound => "kind = 'outbound'",
            GoalKind::File => "kind = 'file_download'",
            GoalKind::Revenue => "isNotNull(revenue) AND revenue > 0",
        }
    }
}

/// A goal, as the query engine needs it.
///
/// Deliberately not the Postgres row. The `goals` table stores `conditions` as
/// jsonb and the id as a UUID; this is the parsed, engine-ready form, so the
/// query crate never depends on how identity and configuration happen to be
/// stored. [`GoalDef::parse`] is the one-line bridge for the handler.
#[derive(Debug, Clone)]
pub struct GoalDef {
    /// Opaque identifier, echoed back on the row.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Which slice of the stream is eligible.
    pub kind: GoalKind,
    /// The conditions that narrow it, in the same shape as a segment.
    pub conditions: FilterGroup,
    /// Monetary value of one conversion, in minor units. Used when the events
    /// themselves carry no revenue.
    pub value_cents: i64,
}

impl GoalDef {
    /// Build from the columns of a `goals` row.
    ///
    /// `conditions` accepts both filter forms from `docs/CONTRACT.md` §3.2: the
    /// flat array and the `{logic, filters}` group. An empty or null jsonb means
    /// "every event of this kind counts", which is what a bare revenue goal is.
    pub fn parse(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: &str,
        conditions: &str,
        value_cents: i64,
    ) -> Result<Self> {
        let trimmed = conditions.trim();
        let conditions = if trimmed.is_empty() || trimmed == "null" {
            FilterGroup::default()
        } else {
            micaforge_core::filters::parse_filters(trimmed)?
        };
        Ok(GoalDef {
            id: id.into(),
            name: name.into(),
            kind: GoalKind::parse(kind)?,
            conditions,
            value_cents,
        })
    }

    /// The predicate that is true for an event row satisfying this goal.
    pub fn predicate(&self) -> Result<(String, Vec<Param>)> {
        let base = self.kind.base_predicate().to_string();
        match compile_filter_group(&self.conditions, Table::Events)? {
            Some((sql, params)) => Ok((format!("(({base}) AND {sql})"), params)),
            None => Ok((format!("({base})"), Vec::new())),
        }
    }
}

/// One step of a funnel.
///
/// A step is a condition on a single event, exactly like a goal's conditions,
/// because a funnel is a sequence of things that happened and not a sequence of
/// pages. That is what lets a step be "signed up" and the next one be "opened
/// the editor" rather than forcing everything through a URL.
#[derive(Debug, Clone)]
pub struct FunnelStepDef {
    /// Label shown on the chart.
    pub label: String,
    /// The condition that satisfies this step.
    pub conditions: FilterGroup,
}

impl FunnelStepDef {
    /// Build from one element of a `funnels.steps` jsonb array.
    pub fn parse(label: impl Into<String>, conditions: &str) -> Result<Self> {
        let trimmed = conditions.trim();
        let conditions = if trimmed.is_empty() || trimmed == "null" {
            FilterGroup::default()
        } else {
            micaforge_core::filters::parse_filters(trimmed)?
        };
        Ok(FunnelStepDef {
            label: label.into(),
            conditions,
        })
    }

    /// The predicate for this step.
    ///
    /// A step with no conditions would be true for every row, which would make
    /// the funnel report a straight line and hide the misconfiguration. It is
    /// refused instead.
    pub fn predicate(&self) -> Result<(String, Vec<Param>)> {
        match compile_filter_group(&self.conditions, Table::Events)? {
            Some((sql, params)) => Ok((sql, params)),
            None => Err(Error::validation(
                "steps",
                format!("funnel step `{}` has no conditions", self.label),
            )),
        }
    }
}
