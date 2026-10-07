//! The ClickHouse schema, as data.
//!
//! Every table is one [`TableDef`], and the same column list generates both the
//! `CREATE TABLE IF NOT EXISTS` and the `ALTER TABLE ... ADD COLUMN IF NOT
//! EXISTS` healing pass. That is the whole point of the shape: an install
//! created on an older version and an install created today converge on the same
//! columns without anyone hand-writing a second, divergent list of ALTERs that
//! drifts from the CREATE.
//!
//! Adding a column is therefore one edit: append it to the `columns` slice. A
//! fresh install gets it from the CREATE, an existing install gets it from the
//! ALTER on the next boot. Columns are only ever appended, never reordered:
//! ClickHouse tolerates reordering, but the row structs in
//! [`super::rows`] are positional on the wire and would not.
//!
//! What this file may **not** do is change a column's type or drop one. Those
//! are destructive and belong in an explicit, reviewed migration, not in an
//! idempotent boot path that runs unattended on every self-hosted install.

/// One ClickHouse table, in the form both DDL passes are generated from.
pub struct TableDef {
    /// Table name, unqualified.
    pub name: &'static str,
    /// Columns in wire order: `(name, type-and-modifiers)`.
    pub columns: &'static [(&'static str, &'static str)],
    /// Engine clause, including any engine parameters.
    pub engine: &'static str,
    /// `PARTITION BY` expression. Every event table partitions by month, which
    /// is what makes a retention TTL a partition drop rather than a rewrite.
    pub partition_by: Option<&'static str>,
    /// `ORDER BY` expression, which is also the primary key.
    pub order_by: &'static str,
    /// A TTL written at creation time, independent of the configured retention.
    pub ttl: Option<&'static str>,
    /// The column a retention TTL is measured from, when this table has one.
    ///
    /// `None` means the table is not time-series and retention does not apply:
    /// `page_index` is a register of URLs, and expiring it would delete the
    /// content inventory the Citation Gap is joined against.
    pub time_column: Option<&'static str>,
}

impl TableDef {
    /// The `CREATE TABLE IF NOT EXISTS` for a fresh install.
    pub fn create_sql(&self, db: &str) -> String {
        let cols = self
            .columns
            .iter()
            .map(|(n, t)| format!("  {n} {t}"))
            .collect::<Vec<_>>()
            .join(",\n");

        let mut sql = format!(
            "CREATE TABLE IF NOT EXISTS {db}.{} (\n{cols}\n) ENGINE = {}",
            self.name, self.engine
        );
        if let Some(p) = self.partition_by {
            sql.push_str(&format!("\nPARTITION BY {p}"));
        }
        sql.push_str(&format!("\nORDER BY {}", self.order_by));
        if let Some(ttl) = self.ttl {
            sql.push_str(&format!("\nTTL {ttl}"));
        }
        sql
    }

    /// One `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` per column.
    ///
    /// Run against a table that already has every column, this is a list of
    /// no-ops that ClickHouse answers in microseconds, which is why it is safe
    /// to run on every boot rather than behind a version check.
    pub fn heal_sql(&self, db: &str) -> Vec<String> {
        self.columns
            .iter()
            .map(|(n, t)| {
                format!(
                    "ALTER TABLE {db}.{} ADD COLUMN IF NOT EXISTS {n} {t}",
                    self.name
                )
            })
            .collect()
    }
}

/// `events` — human and custom hits. `docs/CONTRACT.md` §1.1.
///
/// Sorted `(site_id, timestamp, session_id)`: every read is scoped to one site
/// and one time range, and the session id last keeps a session's rows adjacent
/// so a session detail view is a short contiguous scan.
const EVENTS: TableDef = TableDef {
    name: "events",
    columns: &[
        ("site_id", "UInt16"),
        ("timestamp", "DateTime64(3)"),
        ("event_id", "UUID"),
        ("session_id", "String"),
        ("visitor_id", "String"),
        ("identified_id", "String DEFAULT ''"),
        ("kind", "LowCardinality(String) DEFAULT 'pageview'"),
        ("event_name", "String DEFAULT ''"),
        ("hostname", "String"),
        ("pathname", "String"),
        ("page_title", "String DEFAULT ''"),
        ("querystring", "String DEFAULT ''"),
        ("url_params", "Map(String, String)"),
        ("hash", "String DEFAULT ''"),
        ("referrer", "String DEFAULT ''"),
        ("referrer_host", "String DEFAULT ''"),
        ("channel", "LowCardinality(String) DEFAULT 'direct'"),
        ("source", "LowCardinality(String) DEFAULT ''"),
        ("utm_source", "String DEFAULT ''"),
        ("utm_medium", "String DEFAULT ''"),
        ("utm_campaign", "String DEFAULT ''"),
        ("utm_term", "String DEFAULT ''"),
        ("utm_content", "String DEFAULT ''"),
        ("click_id", "String DEFAULT ''"),
        ("browser", "LowCardinality(String) DEFAULT ''"),
        ("browser_version", "LowCardinality(String) DEFAULT ''"),
        ("os", "LowCardinality(String) DEFAULT ''"),
        ("os_version", "LowCardinality(String) DEFAULT ''"),
        ("device_type", "LowCardinality(String) DEFAULT ''"),
        ("screen_w", "UInt16 DEFAULT 0"),
        ("screen_h", "UInt16 DEFAULT 0"),
        ("viewport_w", "UInt16 DEFAULT 0"),
        ("viewport_h", "UInt16 DEFAULT 0"),
        ("language", "LowCardinality(String) DEFAULT ''"),
        ("timezone", "LowCardinality(String) DEFAULT ''"),
        ("country", "LowCardinality(FixedString(2)) DEFAULT '  '"),
        ("region", "LowCardinality(String) DEFAULT ''"),
        ("city", "String DEFAULT ''"),
        ("lat", "Float32 DEFAULT 0"),
        ("lon", "Float32 DEFAULT 0"),
        ("asn", "UInt32 DEFAULT 0"),
        ("asn_org", "LowCardinality(String) DEFAULT ''"),
        ("is_datacenter", "UInt8 DEFAULT 0"),
        ("entry_path", "String DEFAULT ''"),
        ("exit_path", "String DEFAULT ''"),
        ("duration_ms", "UInt32 DEFAULT 0"),
        ("scroll_pct", "UInt8 DEFAULT 0"),
        ("props", "JSON"),
        ("revenue", "Nullable(Decimal64(4))"),
        ("currency", "LowCardinality(String) DEFAULT ''"),
        ("flags", "Map(String, String)"),
        ("experiment", "LowCardinality(String) DEFAULT ''"),
        ("variant", "LowCardinality(String) DEFAULT ''"),
        ("lcp", "Nullable(Float32)"),
        ("cls", "Nullable(Float32)"),
        ("inp", "Nullable(Float32)"),
        ("fcp", "Nullable(Float32)"),
        ("ttfb", "Nullable(Float32)"),
        ("import_id", "Nullable(UUID)"),
        ("sdk_version", "LowCardinality(String) DEFAULT ''"),
    ],
    engine: "MergeTree",
    partition_by: Some("toYYYYMM(timestamp)"),
    order_by: "(site_id, timestamp, session_id)",
    ttl: None,
    time_column: Some("timestamp"),
};

/// `agent_events` — named non-human fetches. `docs/CONTRACT.md` §1.2.
const AGENT_EVENTS: TableDef = TableDef {
    name: "agent_events",
    columns: &[
        ("site_id", "UInt16"),
        ("timestamp", "DateTime64(3)"),
        ("agent_id", "LowCardinality(String)"),
        ("operator", "LowCardinality(String)"),
        ("purpose", "LowCardinality(String)"),
        ("verified", "UInt8 DEFAULT 0"),
        ("user_agent", "String"),
        ("method", "LowCardinality(String) DEFAULT 'GET'"),
        ("hostname", "String"),
        ("pathname", "String"),
        ("querystring", "String DEFAULT ''"),
        ("status", "UInt16 DEFAULT 200"),
        ("bytes", "UInt32 DEFAULT 0"),
        ("duration_ms", "UInt32 DEFAULT 0"),
        ("content_type", "LowCardinality(String) DEFAULT ''"),
        ("asn", "UInt32 DEFAULT 0"),
        ("asn_org", "LowCardinality(String) DEFAULT ''"),
        ("country", "LowCardinality(FixedString(2)) DEFAULT '  '"),
        ("ip_hash", "String DEFAULT ''"),
        ("robots_allowed", "Int8 DEFAULT -1"),
        ("llms_listed", "Int8 DEFAULT -1"),
        ("conditional", "UInt8 DEFAULT 0"),
    ],
    engine: "MergeTree",
    partition_by: Some("toYYYYMM(timestamp)"),
    order_by: "(site_id, timestamp, agent_id)",
    ttl: None,
    time_column: Some("timestamp"),
};

/// `bot_events` — unnamed or suspected non-human traffic. §1.3.
///
/// The three-month TTL is written at creation and is a ceiling, not a default:
/// [`super::Ch::apply_retention`] will shorten it to match a stricter configured
/// retention but never lengthen it.
const BOT_EVENTS: TableDef = TableDef {
    name: "bot_events",
    columns: &[
        ("site_id", "UInt16"),
        ("timestamp", "DateTime64(3)"),
        ("agent_id", "LowCardinality(String) DEFAULT ''"),
        ("user_agent", "String"),
        ("method", "LowCardinality(String) DEFAULT 'GET'"),
        ("hostname", "String"),
        ("pathname", "String"),
        ("querystring", "String DEFAULT ''"),
        ("status", "UInt16 DEFAULT 200"),
        ("bytes", "UInt32 DEFAULT 0"),
        ("duration_ms", "UInt32 DEFAULT 0"),
        ("content_type", "LowCardinality(String) DEFAULT ''"),
        ("asn", "UInt32 DEFAULT 0"),
        ("asn_org", "LowCardinality(String) DEFAULT ''"),
        ("country", "LowCardinality(FixedString(2)) DEFAULT '  '"),
        ("ip_hash", "String DEFAULT ''"),
        ("robots_allowed", "Int8 DEFAULT -1"),
        ("llms_listed", "Int8 DEFAULT -1"),
        ("conditional", "UInt8 DEFAULT 0"),
        ("reasons", "String DEFAULT ''"),
        ("score", "UInt8 DEFAULT 0"),
    ],
    engine: "MergeTree",
    partition_by: Some("toYYYYMM(timestamp)"),
    order_by: "(site_id, timestamp, agent_id)",
    ttl: Some("timestamp + INTERVAL 3 MONTH"),
    time_column: Some("timestamp"),
};

/// `sessions_mv` — the session rollup. `docs/CONTRACT.md` §1.4.
///
/// Two decisions here are worth defending.
///
/// **Every aggregate is a `SimpleAggregateFunction`, never an
/// `AggregateFunction`.** A simple aggregate stores the value itself rather than
/// an opaque state, so `avg(duration_ms)` and `countIf(pageviews <= 1)` — which
/// is exactly how [`micaforge_core::Metric`] spells bounce rate and average
/// duration — read this table as ordinary columns. Storing `argMaxState` would
/// be more precise and would force every reader to know to write `-Merge`, and a
/// metric that is wrong when someone forgets is worse than one that is
/// approximate by design.
///
/// **The session's start column is `started`, with `timestamp` as an alias.**
/// `micaforge_core::time::bucket_expr` writes `toStartOf...(timestamp)` for every
/// table, so the rollup has to answer to that name. Writing `min(timestamp) AS
/// timestamp` inside the view would be a cyclic alias, so the real column is
/// `started` and `timestamp` reads through to it.
///
/// The cost of the simple-aggregate choice: across a merge boundary `exit_path`
/// resolves to *an* observed exit rather than provably the last one. Session
/// detail reads `events` directly, where it is exact.
const SESSIONS_MV: TableDef = TableDef {
    name: "sessions_mv",
    columns: &[
        ("site_id", "UInt16"),
        ("session_id", "String"),
        ("started", "SimpleAggregateFunction(min, DateTime64(3))"),
        ("ended", "SimpleAggregateFunction(max, DateTime64(3))"),
        ("visitor_id", "SimpleAggregateFunction(any, String)"),
        ("identified_id", "SimpleAggregateFunction(anyLast, String)"),
        ("pageviews", "SimpleAggregateFunction(sum, UInt64)"),
        ("events", "SimpleAggregateFunction(sum, UInt64)"),
        ("entry_path", "SimpleAggregateFunction(any, String)"),
        ("exit_path", "SimpleAggregateFunction(anyLast, String)"),
        ("hostname", "SimpleAggregateFunction(any, String)"),
        ("channel", "SimpleAggregateFunction(any, String)"),
        ("source", "SimpleAggregateFunction(any, String)"),
        ("referrer_host", "SimpleAggregateFunction(any, String)"),
        ("utm_source", "SimpleAggregateFunction(any, String)"),
        ("utm_medium", "SimpleAggregateFunction(any, String)"),
        ("utm_campaign", "SimpleAggregateFunction(any, String)"),
        ("country", "SimpleAggregateFunction(any, String)"),
        ("region", "SimpleAggregateFunction(any, String)"),
        ("city", "SimpleAggregateFunction(any, String)"),
        ("device_type", "SimpleAggregateFunction(any, String)"),
        ("browser", "SimpleAggregateFunction(any, String)"),
        ("os", "SimpleAggregateFunction(any, String)"),
        ("language", "SimpleAggregateFunction(any, String)"),
        ("is_datacenter", "SimpleAggregateFunction(max, UInt8)"),
        ("duration_ms", "SimpleAggregateFunction(sum, UInt64)"),
        ("scroll_pct", "SimpleAggregateFunction(max, UInt8)"),
        ("revenue", "SimpleAggregateFunction(max, Decimal64(4))"),
        ("experiment", "SimpleAggregateFunction(any, String)"),
        ("variant", "SimpleAggregateFunction(any, String)"),
        // Alias last, so it never sits between two physical columns the view
        // writes positionally.
        ("timestamp", "DateTime64(3) ALIAS toDateTime64(started, 3)"),
    ],
    engine: "AggregatingMergeTree",
    partition_by: Some("toYYYYMM(started)"),
    order_by: "(site_id, session_id)",
    ttl: None,
    time_column: Some("started"),
};

/// `replay_events` — rrweb session recordings. `docs/CONTRACT.md` §1.5.
///
/// Sorted by session before time, unlike every other event table, because a
/// replay is only ever read as one whole session and never scanned across them.
const REPLAY_EVENTS: TableDef = TableDef {
    name: "replay_events",
    columns: &[
        ("site_id", "UInt16"),
        ("session_id", "String"),
        ("timestamp", "DateTime64(3)"),
        ("seq", "UInt32"),
        ("type", "UInt8"),
        ("data", "String CODEC(ZSTD(3))"),
    ],
    engine: "MergeTree",
    partition_by: Some("toYYYYMM(timestamp)"),
    order_by: "(site_id, session_id, timestamp, seq)",
    ttl: Some("timestamp + INTERVAL 30 DAY"),
    time_column: Some("timestamp"),
};

/// `errors` — client-side exceptions. `docs/CONTRACT.md` §1.6.
const ERRORS: TableDef = TableDef {
    name: "errors",
    columns: &[
        ("site_id", "UInt16"),
        ("timestamp", "DateTime64(3)"),
        ("session_id", "String"),
        ("visitor_id", "String"),
        ("name", "LowCardinality(String) DEFAULT ''"),
        ("message", "String DEFAULT ''"),
        ("stack", "String DEFAULT '' CODEC(ZSTD(3))"),
        ("filename", "String DEFAULT ''"),
        ("lineno", "UInt32 DEFAULT 0"),
        ("colno", "UInt32 DEFAULT 0"),
        ("pathname", "String DEFAULT ''"),
        ("browser", "LowCardinality(String) DEFAULT ''"),
        ("os", "LowCardinality(String) DEFAULT ''"),
        ("release", "LowCardinality(String) DEFAULT ''"),
        ("handled", "UInt8 DEFAULT 0"),
        ("fingerprint", "String DEFAULT ''"),
    ],
    engine: "MergeTree",
    partition_by: Some("toYYYYMM(timestamp)"),
    order_by: "(site_id, timestamp, fingerprint)",
    ttl: None,
    time_column: Some("timestamp"),
};

/// `page_index` — the content register. `docs/CONTRACT.md` §1.7.
///
/// Not partitioned and not expired. It is a register of URLs rather than a
/// stream of hits: it stays small, and dropping it would take the inventory the
/// Citation Gap joins both traces against, hiding exactly the pages that are
/// crawled and never visited.
const PAGE_INDEX: TableDef = TableDef {
    name: "page_index",
    columns: &[
        ("site_id", "UInt16"),
        ("pathname", "String"),
        ("title", "String DEFAULT ''"),
        ("first_seen", "DateTime64(3)"),
        ("last_modified", "Nullable(DateTime64(3))"),
        ("word_count", "UInt32 DEFAULT 0"),
        ("in_sitemap", "UInt8 DEFAULT 0"),
        ("in_llms_txt", "UInt8 DEFAULT 0"),
        ("robots_directive", "String DEFAULT ''"),
        ("updated_at", "DateTime64(3)"),
    ],
    engine: "ReplacingMergeTree(updated_at)",
    partition_by: None,
    order_by: "(site_id, pathname)",
    ttl: None,
    time_column: None,
};

/// `imports_raw` — staging for third-party exports. `docs/CONTRACT.md` §1.8.
const IMPORTS_RAW: TableDef = TableDef {
    name: "imports_raw",
    columns: &[
        ("import_id", "UUID"),
        ("site_id", "UInt16"),
        ("timestamp", "DateTime64(3)"),
        ("source", "LowCardinality(String)"),
        ("seq", "UInt64"),
        ("payload", "String CODEC(ZSTD(3))"),
    ],
    engine: "MergeTree",
    partition_by: Some("toYYYYMM(timestamp)"),
    order_by: "(site_id, import_id, seq)",
    ttl: None,
    time_column: Some("timestamp"),
};

/// Every table, in creation order.
pub const TABLES: &[TableDef] = &[
    EVENTS,
    AGENT_EVENTS,
    BOT_EVENTS,
    SESSIONS_MV,
    REPLAY_EVENTS,
    ERRORS,
    PAGE_INDEX,
    IMPORTS_RAW,
];

/// Name of the view that feeds [`SESSIONS_MV`].
///
/// Kept distinct from the target table's name: `sessions_mv` is the table
/// readers query, and this is the trigger that populates it. Naming both the
/// same is the classic way to end up with a view that reads from itself.
pub const SESSIONS_VIEW: &str = "sessions_mv_from_events";

/// The materialised view that turns the event stream into the session rollup.
///
/// The `SELECT` list is in the target table's physical column order and uses the
/// target's column names, so it lines up whether ClickHouse matches the view's
/// output by position or by name.
///
/// Entry and exit path are resolved with `argMin`/`argMax` over the timestamp
/// rather than `any`/`anyLast`, because a single insert block carries no
/// ordering guarantee and "the first row we happened to see" is not the entry
/// page.
pub fn sessions_view_sql(db: &str) -> String {
    format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS {db}.{SESSIONS_VIEW} TO {db}.sessions_mv AS\n{}",
        sessions_view_select(db)
    )
}

/// The `SELECT` the session view runs, on its own.
///
/// Separate from [`sessions_view_sql`] because `CREATE ... IF NOT EXISTS` never
/// touches a view that already exists. An install created by an older build
/// keeps its old `SELECT` until [`crate::ch::Ch::init_schema`] swaps this one in
/// with `ALTER TABLE ... MODIFY QUERY`, which it does when
/// [`sessions_view_fingerprint`] no longer matches the one stored on the view.
pub fn sessions_view_select(db: &str) -> String {
    format!(
        "SELECT
  site_id,
  session_id,
  min(timestamp) AS started,
  max(timestamp) AS ended,
  any(visitor_id) AS visitor_id,
  argMax(identified_id, timestamp) AS identified_id,
  countIf(kind = 'pageview') AS pageviews,
  count() AS events,
  argMin(if(entry_path != '', entry_path, pathname), timestamp) AS entry_path,
  argMax(if(exit_path != '', exit_path, pathname), timestamp) AS exit_path,
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
  sum(toUInt64(duration_ms)) AS duration_ms,
  max(scroll_pct) AS scroll_pct,
  max(ifNull(revenue, toDecimal64(0, 4))) AS revenue,
  any(toString(experiment)) AS experiment,
  any(toString(variant)) AS variant
FROM {db}.events
GROUP BY site_id, session_id"
    )
}

/// A stable fingerprint of the session view's `SELECT`, stored as the view's
/// comment so a boot can tell whether the view on disk is the current one.
///
/// FNV-1a rather than `std`'s hasher, whose output is allowed to change between
/// Rust releases: a fingerprint that moved with the compiler would rewrite the
/// view on every upgrade of the toolchain. The database name is left out, so
/// the same build agrees with itself on every install.
pub fn sessions_view_fingerprint() -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in sessions_view_select("db").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("micaforge sessions view {hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_heal_cover_the_same_columns() {
        for t in TABLES {
            let create = t.create_sql("micaforge");
            let heal = t.heal_sql("micaforge");
            assert_eq!(heal.len(), t.columns.len(), "{}", t.name);
            for (name, ty) in t.columns {
                assert!(create.contains(&format!("  {name} {ty}")), "{}", t.name);
            }
            assert!(create.starts_with("CREATE TABLE IF NOT EXISTS micaforge."));
            assert!(create.contains(&format!("ORDER BY {}", t.order_by)));
        }
    }

    #[test]
    fn heal_is_add_only_and_conditional() {
        // An unconditional ALTER on a boot path that runs unattended would take
        // a self-hosted install down the first time it ran twice.
        for sql in EVENTS.heal_sql("micaforge") {
            assert!(sql.contains("ADD COLUMN IF NOT EXISTS"));
            assert!(!sql.contains("DROP"));
            assert!(!sql.contains("MODIFY COLUMN"));
        }
    }

    #[test]
    fn the_session_view_writes_every_physical_rollup_column() {
        let sql = sessions_view_sql("micaforge");
        let physical: Vec<_> = SESSIONS_MV
            .columns
            .iter()
            .filter(|(_, ty)| !ty.contains("ALIAS"))
            .collect();
        for (name, _) in &physical {
            assert!(
                sql.contains(&format!("AS {name}")) || sql.contains(&format!("  {name},")),
                "session view is missing {name}"
            );
        }
        // The alias column must NOT be written by the view: it has no storage.
        assert!(!sql.contains("AS timestamp"));
        assert!(sql.ends_with(&sessions_view_select("micaforge")));
    }

    #[test]
    fn the_view_fingerprint_is_stable_and_follows_the_select() {
        let a = sessions_view_fingerprint();
        assert_eq!(a, sessions_view_fingerprint());
        assert!(a.starts_with("micaforge sessions view "));
        // A comment is formatted into DDL, so it must never need escaping.
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == ' '));
    }

    #[test]
    fn only_time_series_tables_carry_a_retention_column() {
        let by_name = |n: &str| TABLES.iter().find(|t| t.name == n).expect("table");
        assert_eq!(by_name("events").time_column, Some("timestamp"));
        assert_eq!(by_name("sessions_mv").time_column, Some("started"));
        // The content register must survive retention, or the Citation Gap
        // loses the pages that have agent traffic and no human traffic.
        assert_eq!(by_name("page_index").time_column, None);
    }
}
