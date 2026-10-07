//! The ClickHouse client, the schema it creates, and the rows it moves.
//!
//! ClickHouse is the event store. It is append-only: nothing in this module ever
//! updates a row, and the only mutation issued anywhere is the retention TTL,
//! which ClickHouse resolves as a partition drop rather than a rewrite.
//!
//! Reads go through the driver's positional `?` binds. The only strings this
//! module formats into SQL are its own DDL and the database name, and that name
//! is validated against `[A-Za-z0-9_]` before it is used, because it arrives
//! from an environment variable and an environment variable is still an input.

mod rows;
mod schema;

pub use rows::{
    country_code, country_str, decimal64_4, from_decimal64_4, AgentEventRow, BotEventRow, ErrorRow,
    EventRow, ImportRawRow, PageIndexRow, ReplayRow,
};
pub use schema::{TableDef, SESSIONS_VIEW, TABLES};

use micaforge_core::{Error, Result};

/// Longest accepted ClickHouse database name.
const MAX_DB_NAME: usize = 64;

/// How long [`Ch::ping`] waits for an answer.
const PING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// A connected ClickHouse database.
///
/// Cheap to clone: `clickhouse::Client` is an `Arc` around an HTTP pool, so
/// every background writer lane holds its own handle to the same connections.
#[derive(Clone)]
pub struct Ch {
    /// The working client, scoped to [`Ch::db`].
    client: clickhouse::Client,
    /// A second handle with no database selected.
    ///
    /// `CREATE DATABASE` cannot run through a connection whose default database
    /// does not exist yet, which on a first boot is precisely the situation.
    admin: clickhouse::Client,
    /// The database every table lives in.
    pub db: String,
}

impl std::fmt::Debug for Ch {
    /// Deliberately does not print the client: it holds the password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ch").field("db", &self.db).finish()
    }
}

/// Flatten a driver error into the crate error type.
///
/// ClickHouse error text carries the failing query, so it is a store error and
/// never public: `Error::Database` is the variant the HTTP layer replaces with a
/// fixed sentence before it answers a client.
fn ch_err(context: &str, e: clickhouse::error::Error) -> Error {
    Error::Database(format!("clickhouse {context}: {e}"))
}

impl Ch {
    /// Build a client for `url`, scoped to `db`.
    ///
    /// Three server settings are pinned on the client rather than on individual
    /// statements. `props` is a `JSON` column and the driver's binary formats
    /// can only carry a `JSON` value as a string, so reading and writing it that
    /// way has to be on for every statement or the column is unusable. That is
    /// what sets this product's floor at ClickHouse 24.10.
    ///
    /// No network call happens here. A wrong host surfaces on the first query,
    /// which in practice is [`Ch::init_schema`] during boot.
    ///
    /// The driver's default HTTP client has no connect timeout, and setting one
    /// means supplying a hyper client of our own. Instead, the paths that must
    /// not hang put their own deadline around each call: every writer insert
    /// (`writer::insert_all`, with the driver's send and response timeouts on
    /// top) and [`Ch::ping`].
    pub fn connect(url: &str, db: &str, user: &str, pass: &str) -> Result<Ch> {
        let url = url.trim();
        if url.is_empty() {
            return Err(Error::validation(
                "clickhouse_url",
                "MICAFORGE_CLICKHOUSE_URL is empty",
            ));
        }
        url::Url::parse(url)
            .map_err(|e| Error::validation("clickhouse_url", format!("not a URL: {e}")))?;

        let db = validate_db_name(db)?;

        let base = || {
            clickhouse::Client::default()
                .with_url(url)
                .with_user(user)
                .with_password(pass)
                // ClickHouse 24.10+: carry `JSON` columns as JSON strings over
                // RowBinary in both directions. See the note on `EventRow`.
                .with_option("allow_experimental_json_type", "1")
                .with_option("input_format_binary_read_json_as_string", "1")
                .with_option("output_format_binary_write_json_as_string", "1")
                .with_product_info("micaforge", env!("CARGO_PKG_VERSION"))
        };

        Ok(Ch {
            client: base().with_database(&db),
            admin: base(),
            db,
        })
    }

    /// The underlying driver client, for queries this module does not wrap.
    ///
    /// The query engine builds its own SQL and binds its own parameters, so it
    /// takes the client directly rather than going through a wrapper that could
    /// only ever be a passthrough.
    pub fn client(&self) -> &clickhouse::Client {
        &self.client
    }

    /// Run a statement that returns nothing.
    ///
    /// Note that the driver treats `?` in the SQL as a bind placeholder. Pass
    /// DDL and other parameter-free statements here; anything with a caller
    /// value in it belongs on [`Ch::client`] with a real bind.
    pub async fn exec(&self, sql: &str) -> Result<()> {
        self.client
            .query(sql)
            .execute()
            .await
            .map_err(|e| ch_err("exec", e))
    }

    /// Create the database, every table in `docs/CONTRACT.md` §1.1-§1.8, and the
    /// session rollup view.
    ///
    /// Safe to run on every boot, which is how it is actually used: a
    /// self-hosted install has no migration step of its own and no operator
    /// watching, so the schema has to converge by itself.
    ///
    /// Two passes, in this order:
    ///
    /// 1. `CREATE TABLE IF NOT EXISTS`, which brings a fresh install to the
    ///    current schema in one statement per table.
    /// 2. `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` for every column, which
    ///    brings an install created by an older version up to the same shape.
    ///    On an install that is already current this is a list of no-ops.
    ///
    /// Both passes are generated from the same column list, so the two can never
    /// disagree about what the schema is. Nothing here drops or retypes a
    /// column: destructive changes are not something an unattended boot path
    /// gets to make.
    pub async fn init_schema(&self) -> Result<()> {
        self.admin
            .query(&format!("CREATE DATABASE IF NOT EXISTS {}", self.db))
            .execute()
            .await
            .map_err(|e| ch_err("create database", e))?;

        for table in TABLES {
            self.exec(&table.create_sql(&self.db)).await?;
        }

        // Heal after every table exists, so a column added to `events` is in
        // place before the view that reads it is created.
        for table in TABLES {
            for sql in table.heal_sql(&self.db) {
                self.exec(&sql).await?;
            }
        }

        self.converge_sessions_view().await?;

        tracing::info!(db = %self.db, tables = TABLES.len(), "clickhouse schema ready");
        Ok(())
    }

    /// Create the session view, or bring an existing one's `SELECT` up to date.
    ///
    /// `CREATE MATERIALIZED VIEW IF NOT EXISTS` never touches a view that is
    /// already there, so on its own it would leave every upgraded install on
    /// the `SELECT` it was first created with. The view's comment carries
    /// [`schema::sessions_view_fingerprint`]; when that differs from this
    /// build's, the query is swapped with `ALTER TABLE ... MODIFY QUERY`, which
    /// is atomic and metadata-only for a view with a `TO` table, so no insert
    /// into `events` is ever without a view to feed.
    ///
    /// A refused `MODIFY QUERY` is a warning, not a failed boot: the old view
    /// keeps producing the old rollup, which is a smaller harm than an install
    /// that will not start. Recreating the view by hand (`DROP VIEW` then a
    /// restart) is the way out, and the log line says so.
    async fn converge_sessions_view(&self) -> Result<()> {
        let fingerprint = schema::sessions_view_fingerprint();
        let stored: Option<String> = self
            .client
            .query("SELECT comment FROM system.tables WHERE database = ? AND name = ?")
            .bind(&self.db)
            .bind(SESSIONS_VIEW)
            .fetch_optional::<String>()
            .await
            .map_err(|e| ch_err("read session view", e))?;

        match stored {
            None => {
                self.exec(&schema::sessions_view_sql(&self.db)).await?;
                self.label_sessions_view(&fingerprint).await;
            }
            Some(comment) if comment == fingerprint => {}
            Some(_) => {
                let sql = format!(
                    "ALTER TABLE {}.{SESSIONS_VIEW} MODIFY QUERY {}",
                    self.db,
                    schema::sessions_view_select(&self.db)
                );
                match self.exec(&sql).await {
                    Ok(()) => {
                        tracing::info!(view = SESSIONS_VIEW, "session view query updated");
                        self.label_sessions_view(&fingerprint).await;
                    }
                    Err(e) => tracing::warn!(
                        view = SESSIONS_VIEW,
                        error = %e,
                        "could not update the session view; it keeps its previous query. \
                         Drop the view and restart to recreate it"
                    ),
                }
            }
        }
        Ok(())
    }

    /// Store the view's fingerprint. Failing only costs a repeat next boot.
    async fn label_sessions_view(&self, fingerprint: &str) {
        let sql = format!(
            "ALTER TABLE {}.{SESSIONS_VIEW} MODIFY COMMENT '{fingerprint}'",
            self.db
        );
        if let Err(e) = self.exec(&sql).await {
            tracing::warn!(view = SESSIONS_VIEW, error = %e, "could not label the session view");
        }
    }

    /// Apply the configured retention as a TTL on every time-series table.
    ///
    /// `days` is `MICAFORGE_RETENTION_DAYS` and `replay_days` is
    /// `MICAFORGE_REPLAY_RETENTION_DAYS`. Zero means forever, and forever is the
    /// default: an analytics tool that quietly deleted an operator's history
    /// because a variable was unset would be indefensible.
    ///
    /// `bot_events` is a special case in one direction only. It carries a
    /// three-month ceiling from `docs/CONTRACT.md` §1.3, so a stricter retention
    /// shortens it and a looser one, including "forever", does not lengthen it.
    ///
    /// `page_index` is untouched. It is a register of URLs rather than a stream
    /// of hits, and expiring it would hide exactly the pages the Citation Gap
    /// exists to show: the ones agents crawl and no person ever visits.
    ///
    /// The ALTER is metadata-only. Materialising a TTL across every existing
    /// part on every boot would turn a restart into hours of mutation; new parts
    /// carry the TTL immediately and old ones pick it up on their next merge.
    pub async fn apply_retention(&self, days: u32, replay_days: u32) -> Result<()> {
        /// Ceiling on `bot_events`, from `docs/CONTRACT.md` §1.3.
        const BOT_MAX_DAYS: u32 = 90;

        // Which tables actually carry a TTL right now. Without this, a fresh
        // install running the default retention (forever) issues REMOVE TTL
        // against tables that never had one, and ClickHouse answers with an
        // error for each. It is harmless, but it puts a wall of warnings in
        // front of every first boot, and a first boot that looks like it went
        // wrong is a first boot people abandon.
        let with_ttl: std::collections::HashSet<String> = self
            .client
            .query(
                "SELECT name FROM system.tables \
                 WHERE database = ? AND notEmpty(toString(engine_full)) \
                   AND position(toString(engine_full), 'TTL ') > 0",
            )
            .bind(&self.db)
            .fetch_all::<String>()
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();

        for table in TABLES {
            let Some(time_column) = table.time_column else {
                continue;
            };

            let effective = match table.name {
                "replay_events" => replay_days,
                "bot_events" if days == 0 => BOT_MAX_DAYS,
                "bot_events" => days.min(BOT_MAX_DAYS),
                _ => days,
            };

            // Nothing to remove, so nothing to ask for.
            if effective == 0 && !with_ttl.contains(table.name) {
                continue;
            }

            let sql = if effective == 0 {
                format!("ALTER TABLE {}.{} REMOVE TTL", self.db, table.name)
            } else {
                format!(
                    "ALTER TABLE {}.{} MODIFY TTL {time_column} + INTERVAL {effective} DAY",
                    self.db, table.name
                )
            };

            let result = self
                .client
                .query(&sql)
                .with_option("materialize_ttl_after_modify", "0")
                .execute()
                .await;

            match result {
                Ok(()) => {
                    tracing::debug!(table = table.name, days = effective, "retention applied");
                }
                // The one failure that is genuinely benign: removing a TTL from
                // a table that never had one. Taking the process down over it
                // would be worse than the warning, and the retention state is
                // still exactly what was asked for.
                Err(e) if effective == 0 => {
                    tracing::warn!(table = table.name, error = %e, "could not clear TTL");
                }
                Err(e) => return Err(ch_err("apply retention", e)),
            }
        }
        Ok(())
    }

    /// A cheap readiness probe for `GET /api/health/ready`.
    ///
    /// Bounded by [`PING_DEADLINE`] on its own, because the driver's client has
    /// no connect timeout and a probe must never hang on a host that drops
    /// packets.
    pub async fn ping(&self) -> Result<()> {
        let probe = self.client.query("SELECT 1").execute();
        match tokio::time::timeout(PING_DEADLINE, probe).await {
            Ok(result) => result.map_err(|e| ch_err("ping", e)),
            Err(_) => Err(Error::Upstream(format!(
                "clickhouse ping: no answer within {}ms",
                PING_DEADLINE.as_millis()
            ))),
        }
    }
}

/// Reject a database name that is not a bare identifier.
///
/// The name is formatted into DDL rather than bound, because ClickHouse has no
/// bind form for an identifier. It comes from the operator's own environment, so
/// this is not a hostile-input defence so much as a refusal to build a string
/// that could ever be one.
fn validate_db_name(db: &str) -> Result<String> {
    let db = db.trim();
    if db.is_empty() {
        return Err(Error::validation(
            "clickhouse_db",
            "MICAFORGE_CLICKHOUSE_DB is empty",
        ));
    }
    if db.len() > MAX_DB_NAME {
        return Err(Error::validation(
            "clickhouse_db",
            format!("longer than {MAX_DB_NAME} characters"),
        ));
    }
    if !db.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::validation(
            "clickhouse_db",
            "may only contain letters, digits and underscores",
        ));
    }
    Ok(db.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_names_are_bare_identifiers_only() {
        assert!(validate_db_name("micaforge").is_ok());
        assert!(validate_db_name(" micaforge_2 ").is_ok());
        assert!(validate_db_name("").is_err());
        assert!(validate_db_name("mica-forge").is_err());
        // The reason the check exists at all.
        assert!(validate_db_name("db; DROP DATABASE default").is_err());
        assert!(validate_db_name(&"x".repeat(MAX_DB_NAME + 1)).is_err());
    }

    #[test]
    fn connect_validates_before_it_builds_anything() {
        assert!(Ch::connect("", "micaforge", "default", "").is_err());
        assert!(Ch::connect("not a url", "micaforge", "default", "").is_err());
        let ch = Ch::connect("http://localhost:8123", "micaforge", "default", "").unwrap();
        assert_eq!(ch.db, "micaforge");
    }
}
