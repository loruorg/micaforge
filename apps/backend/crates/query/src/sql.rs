//! The safe query builder.
//!
//! Two rules hold everywhere in this module, and every report in the crate
//! depends on them:
//!
//! 1. **Caller text is bound, never interpolated.** A filter value, a property
//!    key, a regular expression, an agent slug and a path all leave here as a
//!    `?` placeholder plus a [`Param`]. The driver escapes and quotes them.
//! 2. **The only strings formatted into SQL are ours.** Column names come from
//!    [`micaforge_core::Dimension::column_expr`], aggregate expressions from
//!    [`micaforge_core::Metric::select_expr`], and the two integers that cannot
//!    be bound in ClickHouse (`LIMIT`, `OFFSET`) are clamped `u32`/`u64` values.
//!
//! ## A warning about `?`
//!
//! The driver splits the statement on `?` to find its bind slots, including a
//! `?` inside a string literal. Nothing in this crate writes a literal question
//! mark into SQL; if a future report needs one, it is spelled `??`.
//!
//! ## What a metric means per audience
//!
//! [`micaforge_core::Audience`] does not just filter rows, it changes the
//! population being measured, so the same metric name means three different
//! things. This is deliberate and it is the toggle the whole product turns on.
//!
//! | metric | `human` (`events`) | `agent` (`agent_events`) | `all` (union) |
//! |---|---|---|---|
//! | `visitors` | distinct daily visitor hashes | distinct named agents | both, agents counted as one "visitor" per agent |
//! | `sessions` | distinct session ids | distinct fetching hosts (`ip_hash` per agent) | both |
//! | `pageviews` | `pageview` events | fetches | both, one fetch counted as one view |
//! | `bounce_rate`, `avg_duration`, `views_per_session` | measured | **not reported** — a crawler has no session to bounce out of | measured over the human half only |
//! | `agent_fetches`, `agent_bytes`, `unique_agents` | **not reported** | measured | measured over the agent half only |
//!
//! A report that cannot answer a metric for the selected audience returns
//! `null` for it rather than a zero. A zero is a measurement; a null is an
//! absence, and conflating the two is how an analytics tool starts lying.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use micaforge_core::{
    Audience, Dimension, Error, Filter, FilterGroup, FilterNode, FilterOp, Granularity,
    QueryEnvelope, Result, Table, TimeRange,
};

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

/// Deepest filter tree accepted.
///
/// A saved segment nests to express `(a AND b) OR c`; nothing legitimate nests
/// eight deep. The bound exists so a hand-written `filters` parameter cannot
/// turn into a planner cost by itself.
const MAX_FILTER_DEPTH: usize = 8;

/// Most leaf conditions accepted across the whole tree.
const MAX_FILTER_LEAVES: usize = 64;

/// Longest `matches` pattern accepted.
///
/// ClickHouse compiles `match()` with RE2, which has no catastrophic
/// backtracking, but the pattern is still compiled once per query and a
/// kilobyte of alternation is a cost with no use case behind it.
const MAX_MATCH_PATTERN: usize = 200;

/// Largest page a list endpoint will return.
const MAX_LIMIT: u32 = 10_000;

/// Deepest page a list endpoint will skip to. Past this, a caller wants an
/// export, not pagination.
const MAX_OFFSET: u64 = 1_000_000;

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// One bind parameter, in the order its `?` appears in the statement.
///
/// The variants exist because the driver serialises by Rust type: a `String`
/// becomes a quoted, escaped literal and a number becomes a bare one. Binding a
/// number as a string would compare `status = '404'` against a `UInt16`, which
/// ClickHouse would either reject or silently coerce.
#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    /// Quoted and escaped by the driver.
    Str(String),
    /// Unsigned count.
    U64(u64),
    /// Signed count.
    I64(i64),
    /// Float, for the inequality operators and for ratios.
    F64(f64),
    /// A site id.
    U16(u16),
}

/// Count the bind slots in a statement, the way the driver counts them.
///
/// The driver splits on `?` to find its slots, and `??` is how a literal
/// question mark is spelled. Nothing in this crate writes a literal `?` into
/// SQL, so this is really an assertion tool: it lets a caller check, before the
/// statement leaves the process, that it has exactly as many parameters as
/// slots. Positional binding is unforgiving, and a count that is one out does
/// not fail cleanly — every value after the mismatch lands in the wrong slot.
pub fn placeholder_count(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut index = 0usize;
    let mut count = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'?' {
            if bytes.get(index + 1) == Some(&b'?') {
                index += 2;
                continue;
            }
            count += 1;
        }
        index += 1;
    }
    count
}

/// Bind every parameter onto a driver query, in order.
///
/// Positional and unforgiving on purpose: the number of parameters must equal
/// the number of `?` in the statement, which is what [`QueryBuilder::build`]
/// guarantees by construction.
pub fn bind_all(q: clickhouse::query::Query, params: &[Param]) -> clickhouse::query::Query {
    let mut q = q;
    for p in params {
        q = match p {
            Param::Str(v) => q.bind(v.as_str()),
            Param::U64(v) => q.bind(*v),
            Param::I64(v) => q.bind(*v),
            Param::F64(v) => q.bind(*v),
            Param::U16(v) => q.bind(*v),
        };
    }
    q
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// What a builder reads from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// One physical table.
    One(Table),
    /// The normalised projection of `events` and `agent_events`, for
    /// [`Audience::All`].
    Union,
    /// A session rollup rebuilt from the filtered event stream. Columns are the
    /// ones `sessions_mv` declares.
    SessionRollup,
}

impl Source {
    /// How this source is named in a validation message.
    fn describe(self) -> &'static str {
        match self {
            Source::One(t) => match t {
                Table::Events => "events",
                Table::AgentEvents => "agent_events",
                Table::BotEvents => "bot_events",
                Table::Sessions => "sessions_mv",
                Table::Errors => "errors",
                Table::PageIndex => "page_index",
            },
            Source::Union => "the human + agent union",
            Source::SessionRollup => "the session rollup",
        }
    }
}

/// The column a time range is measured against, per table.
///
/// `sessions_mv` measures from `started`, because `timestamp` there is an alias
/// and an alias does not prune partitions. `page_index` is a register of URLs
/// with no time axis at all, so a range simply does not narrow it — which is
/// the whole reason a page nothing has crawled still appears in coverage.
fn time_column(table: Table) -> Option<&'static str> {
    match table {
        Table::Events | Table::AgentEvents | Table::BotEvents | Table::Errors => Some("timestamp"),
        Table::Sessions => Some("started"),
        Table::PageIndex => None,
    }
}

/// Format an instant the way ClickHouse parses a `DateTime64(3)` literal.
///
/// Bound as a string and converted server-side with an explicit `'UTC'`, so the
/// server's own default timezone can never shift a range boundary.
pub fn fmt_ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// The bucket expression for a granularity, applied to an arbitrary column.
///
/// [`micaforge_core::time::bucket_expr`] does the same thing for a column
/// literally named `timestamp`. Retention cohorts and the decay report need to
/// bucket a *different* expression (a visitor's first-seen instant), so this
/// takes the operand. The two must agree, and they do: same functions, same
/// zone, same Monday-start week.
pub fn bucket_of(g: Granularity, tz: Tz, operand: &str) -> String {
    let name = tz.name();
    match g {
        Granularity::Minute => format!("toStartOfMinute(toTimeZone({operand}, '{name}'))"),
        Granularity::Hour => format!("toStartOfHour(toTimeZone({operand}, '{name}'))"),
        Granularity::Day => format!("toStartOfDay(toTimeZone({operand}, '{name}'))"),
        Granularity::Week => {
            format!("toDateTime(toStartOfWeek(toTimeZone({operand}, '{name}'), 1), '{name}')")
        }
        Granularity::Month => {
            format!("toDateTime(toStartOfMonth(toTimeZone({operand}, '{name}')), '{name}')")
        }
    }
}

/// Wrap a bucket expression so it comes back as epoch milliseconds.
///
/// Chart axes are numbers on the wire. See the crate documentation for why a
/// bucket is a number and a record timestamp is an RFC-3339 string.
pub fn bucket_millis(g: Granularity, tz: Tz, operand: &str) -> String {
    format!(
        "toUnixTimestamp64Milli(toDateTime64({}, 3))",
        bucket_of(g, tz, operand)
    )
}

/// Wrap any DateTime expression so it comes back as epoch milliseconds.
pub fn millis(expr: &str) -> String {
    format!("toUnixTimestamp64Milli(toDateTime64({expr}, 3))")
}

/// Coerce an aggregate to a plain, non-null `Float64`.
///
/// Three things make this necessary at once. Quantile functions over a
/// `Nullable(Float32)` column come back `Nullable(Float32)`; core's rate
/// expressions guard their divisor with `nullIf` and are therefore nullable by
/// design; and RowBinary is positional, so a column that is `Nullable` in one
/// query and not in another would decode into a different Rust type. `ifNull`
/// runs first because `toFloat64` of a null is still a null.
pub fn f64_of(expr: &str) -> String {
    format!("toFloat64(ifNull({expr}, 0))")
}

// ---------------------------------------------------------------------------
// Dimension availability
// ---------------------------------------------------------------------------

/// Whether a dimension exists on a source.
///
/// Caught here rather than at ClickHouse, because the driver's answer to
/// `Unknown identifier: agent_id` is a paragraph of query text, and the honest
/// answer to "filter human traffic by operator" is that the human trace does not
/// record one.
fn dimension_available(d: &Dimension, source: Source) -> bool {
    use micaforge_core::Dimension as D;
    match source {
        Source::One(Table::Events) => !matches!(
            d,
            D::AgentId
                | D::Operator
                | D::Purpose
                | D::Verified
                | D::RobotsAllowed
                | D::Status
                | D::ContentType
        ),
        Source::One(Table::AgentEvents) | Source::One(Table::BotEvents) => matches!(
            d,
            D::Hostname
                | D::Pathname
                | D::Querystring
                | D::Country
                | D::AsnOrg
                | D::AgentId
                | D::Operator
                | D::Purpose
                | D::Verified
                | D::RobotsAllowed
                | D::Status
                | D::ContentType
        ),
        Source::One(Table::Sessions) | Source::SessionRollup => matches!(
            d,
            D::EntryPath
                | D::ExitPath
                | D::Hostname
                | D::Channel
                | D::Source
                | D::ReferrerHost
                | D::UtmSource
                | D::UtmMedium
                | D::UtmCampaign
                | D::Country
                | D::Region
                | D::City
                | D::DeviceType
                | D::Browser
                | D::Os
                | D::Language
                | D::IsDatacenter
                | D::VisitorId
                | D::IdentifiedId
                | D::Experiment
                | D::Variant
                | D::Revenue
        ),
        Source::One(Table::Errors) => matches!(d, D::Pathname | D::Browser | D::Os | D::VisitorId),
        Source::One(Table::PageIndex) => matches!(d, D::Pathname | D::PageTitle),
        Source::Union => matches!(
            d,
            D::Hostname
                | D::Pathname
                | D::Querystring
                | D::Country
                | D::Region
                | D::City
                | D::AsnOrg
                | D::IsDatacenter
                | D::Channel
                | D::Source
                | D::ReferrerHost
                | D::DeviceType
                | D::Browser
                | D::Os
                | D::Language
                | D::EventName
                | D::VisitorId
                | D::AgentId
                | D::Operator
                | D::Purpose
                | D::Verified
                | D::RobotsAllowed
                | D::Status
                | D::ContentType
        ),
    }
}

// ---------------------------------------------------------------------------
// The `matches` guard
// ---------------------------------------------------------------------------

/// Reject a `matches` pattern that is too long or nests a quantifier.
///
/// ClickHouse uses RE2, which is linear-time and cannot be made to backtrack, so
/// this is not the last line of defence — it is the cheap one. It exists because
/// a pattern arrives from a filter box, `(a+)+b` is the shape a person reaches
/// for when they are probing, and refusing it costs a caller nothing while
/// making the intent of the filter box unambiguous.
///
/// The scan tracks one bit per open group: did anything inside it carry a
/// quantifier. Closing a group that did, immediately followed by another
/// quantifier, is the nested case. Two quantifiers in a row (`a**`) is rejected
/// on the same reasoning.
pub fn guard_match_pattern(pattern: &str) -> Result<()> {
    if pattern.len() > MAX_MATCH_PATTERN {
        return Err(Error::validation(
            "filters",
            format!("`matches` pattern is longer than {MAX_MATCH_PATTERN} characters"),
        ));
    }

    let nested = Error::validation(
        "filters",
        "`matches` pattern nests a quantifier inside a quantified group",
    );

    let bytes: Vec<char> = pattern.chars().collect();
    let mut stack: Vec<bool> = Vec::new();
    let mut prev_was_quantifier = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i];
        match c {
            '\\' => {
                // An escaped character is a literal, whatever it is.
                i += 2;
                prev_was_quantifier = false;
                continue;
            }
            '[' => {
                // Inside a character class every metacharacter is a literal.
                i += 1;
                while i < bytes.len() && bytes[i] != ']' {
                    if bytes[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                prev_was_quantifier = false;
            }
            '(' => {
                stack.push(false);
                prev_was_quantifier = false;
            }
            ')' => {
                let inner_had_quantifier = stack.pop().unwrap_or(false);
                let next_quantifies = matches!(bytes.get(i + 1), Some('*' | '+' | '{'))
                    || matches!(bytes.get(i + 1), Some('?') if matches!(bytes.get(i + 2), Some('?')));
                if inner_had_quantifier && next_quantifies {
                    return Err(nested);
                }
                if inner_had_quantifier {
                    if let Some(top) = stack.last_mut() {
                        *top = true;
                    }
                }
                prev_was_quantifier = false;
            }
            '*' | '+' | '{' => {
                if prev_was_quantifier {
                    return Err(nested);
                }
                if let Some(top) = stack.last_mut() {
                    *top = true;
                }
                prev_was_quantifier = true;
            }
            _ => prev_was_quantifier = false,
        }
        i += 1;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Filter compilation
// ---------------------------------------------------------------------------

/// Compile a filter tree into one parenthesised predicate plus its parameters.
///
/// Returns `None` when the tree constrains nothing, so a caller can leave the
/// `WHERE` clause alone rather than appending a tautology.
fn compile_group(
    g: &FilterGroup,
    source: Source,
    params: &mut Vec<Param>,
    depth: usize,
    leaves: &mut usize,
) -> Result<Option<String>> {
    if depth > MAX_FILTER_DEPTH {
        return Err(Error::validation(
            "filters",
            format!("filter groups nest deeper than {MAX_FILTER_DEPTH}"),
        ));
    }

    let mut parts: Vec<String> = Vec::new();
    for node in &g.filters {
        match node {
            FilterNode::Leaf(f) => {
                *leaves += 1;
                if *leaves > MAX_FILTER_LEAVES {
                    return Err(Error::validation(
                        "filters",
                        format!("more than {MAX_FILTER_LEAVES} conditions in one query"),
                    ));
                }
                parts.push(compile_leaf(f, source, params)?);
            }
            FilterNode::Group(inner) => {
                if let Some(sql) = compile_group(inner, source, params, depth + 1, leaves)? {
                    parts.push(sql);
                }
            }
        }
    }

    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "({})",
        parts.join(&format!(" {} ", g.logic.as_sql()))
    )))
}

/// The single value a substring, prefix, suffix or pattern operator compares
/// against.
///
/// Only `is` and `is_not` take a set. Every other operator is one comparison, so
/// a caller who sends an array gets the first element rather than a silently
/// concatenated string.
fn first_value(values: &[String]) -> String {
    values.first().cloned().unwrap_or_default()
}

/// Compile one condition.
///
/// The dimension's own expression goes in first and the value after it, because
/// placeholders are positional and a property dimension carries its key as a
/// placeholder of its own. Every form below mentions the dimension expression
/// exactly once, which is what keeps that ordering true.
fn compile_leaf(f: &Filter, source: Source, params: &mut Vec<Param>) -> Result<String> {
    f.validate()?;

    if !dimension_available(&f.dimension, source) {
        return Err(Error::validation(
            "filters",
            format!(
                "dimension `{}` is not recorded on {}",
                f.dimension,
                source.describe()
            ),
        ));
    }

    let (expr, key_params) = f.dimension.column_expr();
    for k in key_params {
        params.push(Param::Str(k));
    }

    let numeric_dimension = f.dimension.is_numeric();
    let values = f.value.as_strings();

    let sql = match f.op {
        FilterOp::Is | FilterOp::IsNot => {
            let negate = matches!(f.op, FilterOp::IsNot);
            if values.len() == 1 {
                for v in &values {
                    params.push(Param::Str(v.clone()));
                }
                format!("toString({expr}) {} ?", if negate { "!=" } else { "=" })
            } else {
                let slots = vec!["?"; values.len()].join(", ");
                for v in &values {
                    params.push(Param::Str(v.clone()));
                }
                format!(
                    "toString({expr}) {} ({slots})",
                    if negate { "NOT IN" } else { "IN" }
                )
            }
        }

        // `position` rather than `LIKE '%…%'`: a value from a filter box would
        // otherwise have to have its `%` and `_` escaped, and an unescaped
        // wildcard is a filter that quietly matches more than the caller asked
        // for. Case-insensitive because a person typing "docs" means "Docs" too.
        FilterOp::Contains | FilterOp::NotContains => {
            params.push(Param::Str(first_value(&values)));
            let cmp = if matches!(f.op, FilterOp::Contains) {
                "> 0"
            } else {
                "= 0"
            };
            format!("positionCaseInsensitive(toString({expr}), ?) {cmp}")
        }

        FilterOp::StartsWith => {
            params.push(Param::Str(first_value(&values)));
            format!("startsWith(toString({expr}), ?)")
        }
        FilterOp::EndsWith => {
            params.push(Param::Str(first_value(&values)));
            format!("endsWith(toString({expr}), ?)")
        }

        FilterOp::Gt | FilterOp::Lt | FilterOp::Gte | FilterOp::Lte => {
            let n = f.value.as_f64().ok_or_else(|| {
                Error::validation("filters", "an inequality needs a number to compare against")
            })?;
            let op = match f.op {
                FilterOp::Gt => ">",
                FilterOp::Lt => "<",
                FilterOp::Gte => ">=",
                _ => "<=",
            };
            // A stored number is compared as a number; a custom property is
            // text in JSON and is read as a number where it parses as one,
            // which is what `props.cart_value > 50` has to mean.
            let left = if numeric_dimension {
                format!("toFloat64(ifNull({expr}, 0))")
            } else {
                format!("toFloat64OrZero(toString({expr}))")
            };
            params.push(Param::F64(n));
            format!("{left} {op} ?")
        }

        FilterOp::Matches => {
            let pattern = first_value(&values);
            guard_match_pattern(&pattern)?;
            params.push(Param::Str(pattern));
            format!("match(toString({expr}), ?)")
        }

        // "Set" means "carries a value". For text that is a non-empty string;
        // for a stored number it is a non-null one. Treating a numeric zero as
        // unset would erase every genuine `status = 0` and every free order.
        FilterOp::IsSet => {
            if numeric_dimension {
                format!("isNotNull({expr})")
            } else {
                format!("toString({expr}) != ''")
            }
        }
        FilterOp::IsNotSet => {
            if numeric_dimension {
                format!("isNull({expr})")
            } else {
                format!("toString({expr}) = ''")
            }
        }
    };

    Ok(sql)
}

// ---------------------------------------------------------------------------
// The builder
// ---------------------------------------------------------------------------

/// A `SELECT` under construction.
///
/// The builder holds each clause's parameters separately and concatenates them
/// in statement order at [`QueryBuilder::build`], because the driver's
/// placeholders are positional: a `SELECT` expression that binds a property key
/// must have its parameter appear before every `WHERE` parameter, and it will,
/// however the calls were interleaved.
#[derive(Debug)]
pub struct QueryBuilder {
    source: Source,
    from_sql: String,
    from_params: Vec<Param>,
    selects: Vec<String>,
    select_params: Vec<Param>,
    wheres: Vec<String>,
    where_params: Vec<Param>,
    groups: Vec<String>,
    group_params: Vec<Param>,
    orders: Vec<String>,
    limit: Option<(u32, u64)>,
}

impl QueryBuilder {
    /// Read from one physical table.
    pub fn new(table: Table) -> Self {
        QueryBuilder {
            source: Source::One(table),
            from_sql: table.as_str().to_string(),
            from_params: Vec::new(),
            selects: Vec::new(),
            select_params: Vec::new(),
            wheres: Vec::new(),
            where_params: Vec::new(),
            groups: Vec::new(),
            group_params: Vec::new(),
            orders: Vec::new(),
            limit: None,
        }
    }

    /// Read from the table this envelope's audience selects.
    ///
    /// For [`Audience::All`] this builds the normalised union of the human and
    /// agent traces, with the site and the range already applied inside both
    /// branches so each one still prunes its own partitions. On a union source
    /// [`QueryBuilder::where_site`] and [`QueryBuilder::where_range`] are
    /// therefore no-ops: the scope is already in the `FROM`.
    pub fn for_envelope(env: &QueryEnvelope) -> Self {
        match env.audience {
            Audience::Human => QueryBuilder::new(Table::Events),
            Audience::Agent => QueryBuilder::new(Table::AgentEvents),
            Audience::All => {
                let (sql, params) = union_projection(env);
                QueryBuilder {
                    source: Source::Union,
                    from_sql: format!("({sql})"),
                    from_params: params,
                    selects: Vec::new(),
                    select_params: Vec::new(),
                    wheres: Vec::new(),
                    where_params: Vec::new(),
                    groups: Vec::new(),
                    group_params: Vec::new(),
                    orders: Vec::new(),
                    limit: None,
                }
            }
        }
    }

    /// Read from a session rollup rebuilt from the filtered event stream.
    ///
    /// `sessions_mv` cannot answer a filtered question, because the rollup does
    /// not carry `event_name`, `props.*` or any other event-level column. This
    /// source has the rollup's column names and is computed from exactly the
    /// rows the filter left, so [`micaforge_core::Metric::select_expr`] applies
    /// to it unchanged.
    pub fn session_rollup(sql: String, params: Vec<Param>) -> Self {
        QueryBuilder {
            source: Source::SessionRollup,
            from_sql: format!("({sql})"),
            from_params: params,
            selects: Vec::new(),
            select_params: Vec::new(),
            wheres: Vec::new(),
            where_params: Vec::new(),
            groups: Vec::new(),
            group_params: Vec::new(),
            orders: Vec::new(),
            limit: None,
        }
    }

    /// Add a projection with no parameters of its own.
    pub fn select(&mut self, expr: &str) -> &mut Self {
        self.selects.push(expr.to_string());
        self
    }

    /// Add a projection that carries bind parameters, such as a property
    /// dimension's `?` key.
    pub fn select_bound(&mut self, expr: &str, params: Vec<Param>) -> &mut Self {
        self.selects.push(expr.to_string());
        self.select_params.extend(params);
        self
    }

    /// Restrict to a time window.
    ///
    /// Bound as two `DateTime64(3)` literals in explicit UTC. Half-open, so
    /// adjacent ranges neither overlap nor drop the instant between them.
    pub fn where_range(&mut self, r: &TimeRange) -> &mut Self {
        let col = match self.source {
            Source::One(t) => time_column(t),
            // Both union branches already carry the range, and the rollup was
            // built from a range-scoped scan.
            Source::Union | Source::SessionRollup => None,
        };
        if let Some(col) = col {
            self.wheres.push(format!(
                "{col} >= toDateTime64(?, 3, 'UTC') AND {col} < toDateTime64(?, 3, 'UTC')"
            ));
            self.where_params.push(Param::Str(fmt_ts(r.start)));
            self.where_params.push(Param::Str(fmt_ts(r.end)));
        }
        self
    }

    /// Restrict to one site.
    pub fn where_site(&mut self, site: u16) -> &mut Self {
        // A union source already carries the site inside both branches, and a
        // session rollup was built from a site-scoped scan.
        if matches!(self.source, Source::One(_)) {
            self.wheres.push("site_id = ?".to_string());
            self.where_params.push(Param::U16(site));
        }
        self
    }

    /// Apply a filter tree.
    ///
    /// Fails rather than silently dropping a condition the source cannot answer:
    /// a filter that quietly did nothing would make a report look like a fact.
    pub fn where_filters(&mut self, g: Option<&FilterGroup>) -> Result<&mut Self> {
        let Some(g) = g else { return Ok(self) };
        if g.is_empty() {
            return Ok(self);
        }
        let mut params = Vec::new();
        let mut leaves = 0usize;
        if let Some(sql) = compile_group(g, self.source, &mut params, 0, &mut leaves)? {
            self.wheres.push(sql);
            self.where_params.extend(params);
        }
        Ok(self)
    }

    /// Add a predicate this crate wrote itself, with its own parameters.
    pub fn where_raw(&mut self, expr: &str, params: Vec<Param>) -> &mut Self {
        self.wheres.push(expr.to_string());
        self.where_params.extend(params);
        self
    }

    /// Group by an expression with no parameters. Grouping by a `SELECT` alias
    /// is the usual form here, which is why most calls need no parameters.
    pub fn group_by(&mut self, expr: &str) -> &mut Self {
        self.groups.push(expr.to_string());
        self
    }

    /// Group by an expression that carries bind parameters.
    pub fn group_by_bound(&mut self, expr: &str, params: Vec<Param>) -> &mut Self {
        self.groups.push(expr.to_string());
        self.group_params.extend(params);
        self
    }

    /// Order by an expression, descending or ascending.
    pub fn order_by(&mut self, expr: &str, desc: bool) -> &mut Self {
        self.orders
            .push(format!("{expr} {}", if desc { "DESC" } else { "ASC" }));
        self
    }

    /// Page the result. `page` is 1-based; page 0 and page 1 both mean the
    /// first page, matching [`QueryEnvelope::offset`].
    ///
    /// `LIMIT` and `OFFSET` are the two places ClickHouse accepts no bind
    /// parameter, so they are formatted — after being clamped, and from a `u32`
    /// that could not carry SQL in the first place.
    pub fn limit(&mut self, n: u32, page: u32) -> &mut Self {
        let n = n.clamp(1, MAX_LIMIT);
        let offset = (u64::from(page.saturating_sub(1)) * u64::from(n)).min(MAX_OFFSET);
        self.limit = Some((n, offset));
        self
    }

    /// Render the statement and its parameters, in placeholder order.
    pub fn build(self) -> (String, Vec<Param>) {
        let mut sql = String::from("SELECT ");
        if self.selects.is_empty() {
            sql.push('*');
        } else {
            sql.push_str(&self.selects.join(", "));
        }
        sql.push_str("\nFROM ");
        sql.push_str(&self.from_sql);

        if !self.wheres.is_empty() {
            sql.push_str("\nWHERE ");
            sql.push_str(&self.wheres.join(" AND "));
        }
        if !self.groups.is_empty() {
            sql.push_str("\nGROUP BY ");
            sql.push_str(&self.groups.join(", "));
        }
        if !self.orders.is_empty() {
            sql.push_str("\nORDER BY ");
            sql.push_str(&self.orders.join(", "));
        }
        if let Some((n, offset)) = self.limit {
            sql.push_str(&format!("\nLIMIT {n} OFFSET {offset}"));
        }

        // Statement order, which is placeholder order: FROM (a subquery can
        // bind), then SELECT, then WHERE, then GROUP BY.
        let mut params = self.from_params;
        params.extend(self.select_params);
        params.extend(self.where_params);
        params.extend(self.group_params);
        (sql, params)
    }
}

// ---------------------------------------------------------------------------
// Shared fragments
// ---------------------------------------------------------------------------

/// Compile a filter tree on its own.
///
/// A goal definition and a funnel step are both "a condition on one event row",
/// stored as the same `filters` JSON a segment uses. They need the predicate
/// without a statement around it, and they must get it from the same compiler
/// as everything else, or a goal would come to mean something slightly
/// different from the filter that describes it.
pub fn compile_filter_group(g: &FilterGroup, table: Table) -> Result<Option<(String, Vec<Param>)>> {
    if g.is_empty() {
        return Ok(None);
    }
    let mut params = Vec::new();
    let mut leaves = 0usize;
    Ok(compile_group(g, Source::One(table), &mut params, 0, &mut leaves)?.map(|sql| (sql, params)))
}

/// Compile a filter tree against the normalised union projection.
///
/// The union's column names are the ones [`union_projection`] declares, which
/// is a deliberate subset: a crawler has no UTM tag and a browser hit has no
/// operator, so a filter naming either is refused rather than silently ignored.
pub fn compile_union_filters(g: Option<&FilterGroup>) -> Result<Option<(String, Vec<Param>)>> {
    let Some(g) = g.filter(|g| !g.is_empty()) else {
        return Ok(None);
    };
    let mut params = Vec::new();
    let mut leaves = 0usize;
    Ok(compile_group(g, Source::Union, &mut params, 0, &mut leaves)?.map(|sql| (sql, params)))
}

/// The `site / range / filters` predicate for a hand-written statement.
///
/// The reports that need a window function, a join or a `UNION ALL` do not go
/// through [`QueryBuilder`], but they must scope their rows identically, so they
/// take the predicate from here rather than writing one.
pub fn scope_predicate(env: &QueryEnvelope, table: Table) -> Result<(String, Vec<Param>)> {
    let mut parts = vec!["site_id = ?".to_string()];
    let mut params = vec![Param::U16(env.site)];

    if let Some(col) = time_column(table) {
        parts.push(format!(
            "{col} >= toDateTime64(?, 3, 'UTC') AND {col} < toDateTime64(?, 3, 'UTC')"
        ));
        params.push(Param::Str(fmt_ts(env.range.start)));
        params.push(Param::Str(fmt_ts(env.range.end)));
    }

    if let Some(g) = env.filters.as_ref().filter(|g| !g.is_empty()) {
        let mut fp = Vec::new();
        let mut leaves = 0usize;
        if let Some(sql) = compile_group(g, Source::One(table), &mut fp, 0, &mut leaves)? {
            parts.push(sql);
            params.extend(fp);
        }
    }

    Ok((parts.join(" AND "), params))
}

/// The same envelope over a different window.
///
/// Every comparison in the crate — the previous period, the year-earlier
/// period, the earlier half of the decay report — is the same question asked of
/// a different week. Copying the envelope and moving only the range is what
/// guarantees the filters, the audience and the site cannot drift between the
/// two halves of a delta.
pub fn envelope_over(env: &QueryEnvelope, range: &TimeRange) -> QueryEnvelope {
    let mut shifted = env.clone();
    shifted.range = *range;
    shifted
}

/// The same predicate, over an explicit window rather than the envelope's.
///
/// Used by every comparison: the previous period, the year-earlier period and
/// the earlier half of the decay report all reuse the envelope's filters
/// unchanged and only move the window.
pub fn scope_predicate_over(
    env: &QueryEnvelope,
    table: Table,
    range: &TimeRange,
) -> Result<(String, Vec<Param>)> {
    scope_predicate(&envelope_over(env, range), table)
}

/// A session rollup rebuilt from the filtered event stream.
///
/// One row is one session, and every column is named the way `sessions_mv`
/// names it, so [`micaforge_core::Metric::select_expr`] reads this and the
/// stored rollup identically. `timestamp` is aliased to the session's start for
/// the same reason the stored rollup does it: bucket expressions name
/// `timestamp`.
///
/// `extra_key` adds one grouping column, which is how a breakdown attributes a
/// session to each dimension value it touched.
///
/// `conversion` adds an `is_conversion` column, which is what
/// [`micaforge_core::Metric::Conversions`] and `ConversionRate` read.
pub fn sessions_subquery(
    env: &QueryEnvelope,
    extra_key: Option<(&str, Vec<Param>)>,
    conversion: Option<(&str, Vec<Param>)>,
) -> Result<(String, Vec<Param>)> {
    let (scope, scope_params) = scope_predicate(env, Table::Events)?;

    let mut select_head = String::new();
    let mut params: Vec<Param> = Vec::new();
    let mut group = String::from("site_id, session_id");

    // A breakdown key and a conversion predicate are both written in terms of
    // raw `events` columns — `browser`, `country`, `device_type`. Those names
    // are also the aliases this SELECT gives its own session-level aggregates,
    // and ClickHouse resolves a bare name against a sibling alias before it
    // resolves it against the table. Evaluated up here, `toString(browser)`
    // would therefore mean `toString(any(toString(browser)))`, and the query
    // fails at run time with ILLEGAL_AGGREGATION rather than at compile time.
    //
    // So both are computed one level down, over `events` itself, where no alias
    // exists to shadow anything. The extra projection costs nothing: it adds
    // columns to a scan that was already reading those rows.
    let mut inner_extras: Vec<String> = Vec::new();

    if let Some((expr, p)) = extra_key {
        inner_extras.push(format!("{expr} AS bkey"));
        params.extend(p);
        select_head.push_str("bkey,\n  ");
        group.push_str(", bkey");
    }

    let mut conv_col = String::new();
    if let Some((expr, p)) = conversion {
        inner_extras.push(format!("{expr} AS conv_hit"));
        params.extend(p);
        conv_col = ",\n  max(conv_hit) AS is_conversion".to_string();
    }

    params.extend(scope_params);

    let from_clause = if inner_extras.is_empty() {
        format!("events\nWHERE {scope}")
    } else {
        format!(
            "(\n  SELECT *, {}\n  FROM events\n  WHERE {scope}\n)",
            inner_extras.join(", ")
        )
    };

    let sql = format!(
        "SELECT
  {select_head}site_id,
  session_id,
  any(visitor_id) AS visitor_id,
  argMax(identified_id, timestamp) AS identified_id,
  min(timestamp) AS started,
  max(timestamp) AS ended,
  countIf(kind = 'pageview') AS pageviews,
  count() AS events,
  argMin(pathname, timestamp) AS entry_path,
  argMax(pathname, timestamp) AS exit_path,
  any(hostname) AS hostname,
  any(toString(channel)) AS channel,
  any(toString(source)) AS source,
  any(referrer_host) AS referrer_host,
  any(utm_source) AS utm_source,
  any(utm_medium) AS utm_medium,
  any(utm_campaign) AS utm_campaign,
  any(trimBoth(toString(country))) AS country,
  any(toString(region)) AS region,
  any(city) AS city,
  any(toString(device_type)) AS device_type,
  any(toString(browser)) AS browser,
  any(toString(os)) AS os,
  any(toString(language)) AS language,
  max(is_datacenter) AS is_datacenter,
  sum(duration_ms) AS duration_ms,
  max(scroll_pct) AS scroll_pct,
  toFloat64(max(ifNull(revenue, 0))) AS revenue,
  any(toString(experiment)) AS experiment,
  any(toString(variant)) AS variant{conv_col}
FROM {from_clause}
GROUP BY {group}"
    );

    Ok((sql, params))
}

/// The normalised projection behind [`Audience::All`].
///
/// Two traces with different shapes are made to answer the same column names, so
/// one report can count both. What the agent half fills in is a decision, not a
/// default:
///
/// - `visitor_id` is `agent:<slug>`, so a crawler counts as **one** visitor
///   however many hosts it fetched from. An agent is a program, not a crowd.
/// - `session_id` is `agent:<slug>:<ip_hash>`, so a fetching host is the closest
///   honest analogue of a session.
/// - `kind` is `agent_fetch` and `channel` is `agent`, so the machine half never
///   hides inside `direct`.
/// - Everything a crawler does not have — a browser, a screen, a UTM tag — is
///   empty rather than guessed.
pub fn union_projection(env: &QueryEnvelope) -> (String, Vec<Param>) {
    let start = fmt_ts(env.range.start);
    let end = fmt_ts(env.range.end);

    let human_scope = "site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC') \
         AND timestamp < toDateTime64(?, 3, 'UTC')";

    // Not one `toString(x) AS x` anywhere below, deliberately. An alias that
    // shadows the column it is computed from is a cyclic alias in ClickHouse.
    // The casts are unnecessary in any case: `UNION ALL` resolves a supertype,
    // so `LowCardinality(String)` on one side and a literal on the other come
    // back as `String`, and every dimension expression wraps in `toString`
    // itself before it compares anything.
    let sql = format!(
        "SELECT
  site_id, timestamp, hostname, pathname, querystring,
  kind, event_name,
  channel, source, referrer_host,
  device_type, browser, os, language,
  country, region, city,
  asn_org, is_datacenter,
  visitor_id, session_id,
  '' AS agent_id, '' AS operator, '' AS purpose,
  toUInt8(0) AS verified, toInt8(-1) AS robots_allowed,
  toUInt16(200) AS status, '' AS content_type,
  toUInt32(0) AS bytes, duration_ms,
  toUInt8(0) AS is_agent
FROM events
WHERE {human_scope}
UNION ALL
SELECT
  site_id, timestamp, hostname, pathname, querystring,
  'agent_fetch' AS kind, '' AS event_name,
  'agent' AS channel, agent_id AS source, '' AS referrer_host,
  '' AS device_type, '' AS browser, '' AS os, '' AS language,
  country, '' AS region, '' AS city,
  asn_org, toUInt8(0) AS is_datacenter,
  concat('agent:', agent_id) AS visitor_id,
  concat('agent:', agent_id, ':', ip_hash) AS session_id,
  agent_id, operator, purpose,
  verified, robots_allowed,
  status, content_type,
  bytes, duration_ms,
  toUInt8(1) AS is_agent
FROM agent_events
WHERE {human_scope}"
    );

    let params = vec![
        Param::U16(env.site),
        Param::Str(start.clone()),
        Param::Str(end.clone()),
        Param::U16(env.site),
        Param::Str(start),
        Param::Str(end),
    ];

    (sql, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use micaforge_core::{Compare, FilterValue, Logic};

    fn range() -> TimeRange {
        TimeRange::new(
            Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 8, 8, 0, 0, 0).unwrap(),
            chrono_tz::Europe::Lisbon,
        )
    }

    fn envelope() -> QueryEnvelope {
        let mut env = QueryEnvelope::new(7, range());
        env.compare = Compare::None;
        env.granularity = Granularity::Day;
        env
    }

    fn leaf(dim: &str, op: FilterOp, value: FilterValue) -> FilterNode {
        FilterNode::Leaf(Filter::new(Dimension::parse(dim).unwrap(), op, value))
    }

    /// The whole point of the module: a filter value reaches ClickHouse as a
    /// parameter and never as SQL text.
    #[test]
    fn filter_values_are_parameters_not_text() {
        let g = FilterGroup::and(vec![leaf(
            "pathname",
            FilterOp::Is,
            FilterValue::One("/pricing' OR 1=1 --".into()),
        )]);

        let mut b = QueryBuilder::new(Table::Events);
        b.select("count()").where_site(7);
        b.where_filters(Some(&g)).unwrap();
        let (sql, params) = b.build();

        assert!(!sql.contains("/pricing"), "value leaked into SQL: {sql}");
        assert!(!sql.contains("OR 1=1"));
        assert!(sql.contains("toString(pathname) = ?"));
        assert_eq!(
            params.last(),
            Some(&Param::Str("/pricing' OR 1=1 --".to_string()))
        );
    }

    /// A property key belongs to the caller's own tracking code, so it is bound
    /// too, and its placeholder has to come before the value's.
    #[test]
    fn property_keys_bind_before_their_value() {
        let g = FilterGroup::and(vec![leaf(
            "props.plan",
            FilterOp::Is,
            FilterValue::One("pro".into()),
        )]);

        let mut b = QueryBuilder::new(Table::Events);
        b.select("count()");
        b.where_filters(Some(&g)).unwrap();
        let (sql, params) = b.build();

        assert!(sql.contains("JSONExtractString(toJSONString(props), ?)"));
        assert_eq!(
            params,
            vec![Param::Str("plan".into()), Param::Str("pro".into())]
        );
    }

    /// Placeholders are positional, so the count has to match exactly or every
    /// bind after the mismatch lands on the wrong slot.
    #[test]
    fn placeholder_count_matches_parameter_count() {
        let g = FilterGroup {
            logic: Logic::Or,
            filters: vec![
                leaf(
                    "pathname",
                    FilterOp::StartsWith,
                    FilterValue::One("/docs".into()),
                ),
                leaf(
                    "country",
                    FilterOp::Is,
                    FilterValue::Many(vec!["PT".into(), "ES".into(), "FR".into()]),
                ),
                FilterNode::Group(FilterGroup::and(vec![
                    leaf("props.plan", FilterOp::Is, FilterValue::One("pro".into())),
                    leaf("revenue", FilterOp::Gte, FilterValue::Num(10.0)),
                ])),
            ],
        };

        let env = envelope();
        let mut b = QueryBuilder::new(Table::Events);
        b.select("uniq(visitor_id)")
            .where_site(env.site)
            .where_range(&env.range);
        b.where_filters(Some(&g)).unwrap();
        b.group_by("1").order_by("1", true).limit(50, 2);
        let (sql, params) = b.build();

        assert_eq!(sql.matches('?').count(), params.len());
        assert!(sql.contains("LIMIT 50 OFFSET 50"));
    }

    /// A pattern arrives from a filter box. Length and nested quantifiers are
    /// refused with a named field, not passed to the engine.
    #[test]
    fn matches_patterns_are_guarded() {
        assert!(guard_match_pattern("^/docs/.*$").is_ok());
        assert!(guard_match_pattern("(a|b)+c").is_ok());
        assert!(
            guard_match_pattern(r"\(a+\)+").is_ok(),
            "escaped parens are literals"
        );

        let long = "a".repeat(MAX_MATCH_PATTERN + 1);
        let err = guard_match_pattern(&long).unwrap_err();
        assert_eq!(err.field(), Some("filters"));

        assert!(guard_match_pattern("(a+)+b").is_err());
        assert!(guard_match_pattern("(a*)*").is_err());
        assert!(guard_match_pattern("a**").is_err());

        // And through the real path, so the guard cannot be bypassed by going
        // via a filter tree.
        let g = FilterGroup::and(vec![leaf(
            "pathname",
            FilterOp::Matches,
            FilterValue::One("(x+)+y".into()),
        )]);
        let mut b = QueryBuilder::new(Table::Events);
        assert!(b.where_filters(Some(&g)).is_err());
    }

    /// A filter the source cannot answer fails loudly. Silently dropping it
    /// would report an unfiltered number as a filtered one.
    #[test]
    fn a_dimension_the_source_lacks_is_refused() {
        let g = FilterGroup::and(vec![leaf(
            "operator",
            FilterOp::Is,
            FilterValue::One("openai".into()),
        )]);
        let mut b = QueryBuilder::new(Table::Events);
        let err = b.where_filters(Some(&g)).unwrap_err();
        assert!(format!("{err}").contains("operator"));

        // The same filter is fine on the trace that records it.
        let mut b = QueryBuilder::new(Table::AgentEvents);
        assert!(b.where_filters(Some(&g)).is_ok());
    }

    /// The shape of a representative aggregate is stable. This is the test that
    /// notices when an edit changes the SQL every report is built from.
    #[test]
    fn a_breakdown_statement_is_stable() {
        let env = envelope();
        let mut b = QueryBuilder::new(Table::Events);
        b.select("toString(pathname) AS key")
            .select("uniq(visitor_id) AS visitors")
            .where_site(env.site)
            .where_range(&env.range)
            .group_by("key")
            .order_by("visitors", true)
            .limit(10, 1);
        let (sql, params) = b.build();

        assert_eq!(
            sql,
            "SELECT toString(pathname) AS key, uniq(visitor_id) AS visitors\n\
             FROM events\n\
             WHERE site_id = ? AND timestamp >= toDateTime64(?, 3, 'UTC') AND timestamp < toDateTime64(?, 3, 'UTC')\n\
             GROUP BY key\n\
             ORDER BY visitors DESC\n\
             LIMIT 10 OFFSET 0"
        );
        assert_eq!(
            params,
            vec![
                Param::U16(7),
                Param::Str("2026-08-01 00:00:00.000".into()),
                Param::Str("2026-08-08 00:00:00.000".into()),
            ]
        );
    }

    /// The session rollup answers to the same column names as `sessions_mv`, so
    /// core's metric expressions read either one.
    #[test]
    fn the_session_rollup_uses_the_rollup_column_names() {
        let env = envelope();
        let (sql, params) = sessions_subquery(&env, None, None).unwrap();
        // A column is produced either as an alias or as a bare projection.
        let produced = |col: &str| {
            sql.contains(&format!("AS {col},"))
                || sql.contains(&format!("AS {col}\n"))
                || sql.contains(&format!("\n  {col},"))
        };
        for col in [
            "pageviews",
            "duration_ms",
            "visitor_id",
            "session_id",
            "started",
            "ended",
            "entry_path",
            "exit_path",
            "revenue",
        ] {
            assert!(produced(col), "the rollup does not produce {col}:\n{sql}");
        }
        assert_eq!(sql.matches('?').count(), params.len());
    }

    /// The union has to project the same number of columns in both branches or
    /// ClickHouse rejects it outright. Adding a column to one half and
    /// forgetting the other is exactly the edit that breaks it.
    #[test]
    fn both_union_branches_project_the_same_width() {
        let env = envelope();
        let (sql, params) = union_projection(&env);
        assert_eq!(sql.matches('?').count(), params.len());

        /// Count top-level commas in one branch's projection list.
        fn width(branch: &str) -> usize {
            let body = branch
                .split_once("SELECT")
                .expect("a branch starts with SELECT")
                .1
                .split_once("\nFROM")
                .expect("a branch has a FROM")
                .0;
            let mut depth = 0i32;
            let mut columns = 1usize;
            for c in body.chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => columns += 1,
                    _ => {}
                }
            }
            columns
        }

        let (human, agent) = sql.split_once("UNION ALL").expect("two branches");
        assert_eq!(width(human), width(agent));
        assert!(width(human) > 25, "the projection is not a stub");
    }

    /// Every fragment the reports assemble by hand has to carry exactly as many
    /// parameters as it has slots, in every combination. This is the invariant
    /// `exec::fetch_all` enforces at runtime; here it is checked at build time
    /// across the shapes the reports actually produce.
    #[test]
    fn every_fragment_balances_its_placeholders() {
        let filters = FilterGroup {
            logic: Logic::Or,
            filters: vec![
                leaf(
                    "pathname",
                    FilterOp::StartsWith,
                    FilterValue::One("/docs".into()),
                ),
                leaf("props.plan", FilterOp::Is, FilterValue::One("pro".into())),
                FilterNode::Group(FilterGroup::and(vec![
                    leaf(
                        "country",
                        FilterOp::Is,
                        FilterValue::Many(vec!["PT".into(), "ES".into()]),
                    ),
                    leaf("flag.beta", FilterOp::IsSet, FilterValue::default()),
                ])),
            ],
        };

        let mut env = envelope();
        env.filters = Some(filters);

        let balanced = |sql: &str, params: &[Param], what: &str| {
            assert_eq!(
                placeholder_count(sql),
                params.len(),
                "{what} is unbalanced:\n{sql}"
            );
        };

        let (sql, params) = scope_predicate(&env, Table::Events).unwrap();
        balanced(&sql, &params, "scope_predicate(events)");

        // `page_index` has no time axis, so a range narrows nothing there and
        // only URL dimensions are available.
        let mut url_env = envelope();
        url_env.filters = Some(FilterGroup::and(vec![leaf(
            "pathname",
            FilterOp::StartsWith,
            FilterValue::One("/docs".into()),
        )]));
        let (sql, params) = scope_predicate(&url_env, Table::PageIndex).unwrap();
        balanced(&sql, &params, "scope_predicate(page_index)");
        assert!(
            !sql.contains("timestamp"),
            "a range must not narrow the content register: {sql}"
        );

        let (sql, params) = sessions_subquery(&env, None, None).unwrap();
        balanced(&sql, &params, "sessions_subquery");

        // A breakdown by a property, counting conversions against a goal: the
        // shape with the most independent parameter sources in the crate.
        let goal = crate::stats::GoalDef::parse(
            "g1",
            "Signed up",
            "event",
            r#"[{"dimension":"event_name","op":"is","value":"signup"}]"#,
            0,
        )
        .unwrap();
        let (conversion, conversion_params) = goal.predicate().unwrap();
        let (key_expr, key_params) = Dimension::parse("props.plan").unwrap().column_expr();
        let (sql, params) = sessions_subquery(
            &env,
            Some((&key_expr, key_params.into_iter().map(Param::Str).collect())),
            Some((&conversion, conversion_params)),
        )
        .unwrap();
        balanced(
            &sql,
            &params,
            "sessions_subquery with a key and a conversion",
        );

        let (sql, params) = union_projection(&env);
        balanced(&sql, &params, "union_projection");

        // The union projects a deliberate subset of columns, so it takes a
        // filter tree written against that subset. A property filter is refused
        // there, which is itself worth asserting.
        assert!(compile_union_filters(env.filters.as_ref()).is_err());

        let union_filters = FilterGroup::and(vec![
            leaf(
                "pathname",
                FilterOp::Contains,
                FilterValue::One("docs".into()),
            ),
            leaf("operator", FilterOp::Is, FilterValue::One("openai".into())),
        ]);
        let (sql, params) = compile_union_filters(Some(&union_filters))
            .unwrap()
            .expect("the tree is not empty");
        balanced(&sql, &params, "compile_union_filters");

        let prev = micaforge_core::time::previous_period(&env.range, Compare::Previous).unwrap();
        let (sql, params) = scope_predicate_over(&env, Table::Events, &prev).unwrap();
        balanced(&sql, &params, "scope_predicate_over");
    }

    /// A limit a caller can name is still a limit this crate chooses.
    #[test]
    fn paging_is_clamped() {
        let mut b = QueryBuilder::new(Table::Events);
        b.select("count()").limit(u32::MAX, u32::MAX);
        let (sql, _) = b.build();
        assert!(sql.contains(&format!("LIMIT {MAX_LIMIT}")));
        assert!(sql.contains(&format!("OFFSET {MAX_OFFSET}")));
    }
}
