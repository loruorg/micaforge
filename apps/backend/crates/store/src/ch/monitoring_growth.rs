//! Row structs for the monitoring and growth tables: `uptime_checks`,
//! `link_clicks` and `revenue`.
//!
//! The same rules as [`super::rows`] hold, and for the same reasons: field
//! names are the column names, fields are in the schema's column order, a
//! `FixedString(2)` country is `[u8; 2]`, and an enum-like column is a
//! `String` because RowBinary would otherwise write a variant index where
//! ClickHouse expects a name.
//!
//! [`insert_rows`] is here too. These three tables are written in small,
//! deliberate batches (a minute of checks, one webhook's rows), never on a
//! path a browser waits on except `link_clicks`, which goes through a
//! [`crate::writer::TableWriter`] lane instead.

use chrono::{DateTime, Utc};
use micaforge_core::{Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::Ch;

/// How long one direct insert may take, connection included.
///
/// The driver's client has no connect timeout of its own, so a ClickHouse
/// that drops packets would otherwise hold a job lane or a webhook request
/// until the kernel gave up on the handshake.
const INSERT_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// A row in `uptime_checks`: one check of one monitor.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct UptimeCheckRow {
    /// Numeric site id of the monitor's site.
    pub site_id: u16,
    /// When the check started, UTC.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// `uptime_monitors.id`.
    #[serde(with = "clickhouse::serde::uuid")]
    pub monitor_id: Uuid,
    /// The probe region label (`MICAFORGE_REGION`).
    pub region: String,
    /// `1` when every assertion held.
    pub ok: u8,
    /// The final HTTP status, `0` when no response arrived.
    pub status_code: u16,
    /// Time to the response headers, or to the failure.
    pub latency_ms: u32,
    /// Why the check failed, as a sentence. Empty on success.
    pub error: String,
    /// Whole days until the certificate expires, negative once it has.
    /// `None` for plain HTTP or when the check is off.
    pub ssl_days_left: Option<i32>,
}

/// A row in `link_clicks`: one short-link request.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct LinkClickRow {
    /// Numeric id of the link's site, `0` for an organisation-wide link.
    pub site_id: u16,
    /// When the request arrived, UTC.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// `links.id`.
    #[serde(with = "clickhouse::serde::uuid")]
    pub link_id: Uuid,
    /// `links.org_id`.
    #[serde(with = "clickhouse::serde::uuid")]
    pub org_id: Uuid,
    /// The `Referer` header, as sent.
    pub referrer: String,
    /// The referrer's host, normalised the way ingest normalises it.
    pub referrer_host: String,
    /// ISO country, two spaces when unknown.
    pub country: [u8; 2],
    /// ISO-3166-2 region, when the GeoIP database knows it.
    pub region: String,
    /// `desktop`, `mobile`, `tablet`, … from the same parser ingest uses.
    pub device_type: String,
    /// Browser family.
    pub browser: String,
    /// Operating system family.
    pub os: String,
    /// `1` for a link-preview bot or any other non-human client.
    pub is_bot: u8,
    /// The preview bot's or agent's name, when one was recognised.
    pub bot_name: String,
    /// What was served: `web`, `ios`, `android`, `deep_link`, `preview` or
    /// `expired`.
    pub target: String,
}

/// A row in `revenue`: one payment or refund.
#[derive(Debug, Clone, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct RevenueRow {
    /// Numeric site id.
    pub site_id: u16,
    /// When the provider says the payment or refund happened, UTC.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// `stripe` or `paddle`.
    pub provider: String,
    /// The provider's event id, the idempotency key of the webhook.
    pub event_id: String,
    /// The provider's object id: the Checkout Session, invoice, charge,
    /// transaction or adjustment.
    pub transaction_id: String,
    /// `sale`, `subscription` or `refund`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Minor units of `currency`, negative for a refund.
    pub amount: i64,
    /// ISO 4217, upper case.
    pub currency: String,
    /// The provider's customer id, or `email:<keyed hash>`.
    pub customer: String,
    /// The Micaforge session the payment was attributed to.
    pub session_id: String,
    /// The visitor hash of that session.
    pub visitor_id: String,
    /// `session`, `visitor`, `email` or `none`.
    pub attribution: String,
    /// Acquisition channel of the attributed visit.
    pub channel: String,
    /// Resolved source of the attributed visit.
    pub source: String,
    /// Referrer host of the attributed visit.
    pub referrer_host: String,
    /// `utm_source` of the attributed visit.
    pub utm_source: String,
    /// `utm_medium` of the attributed visit.
    pub utm_medium: String,
    /// `utm_campaign` of the attributed visit.
    pub utm_campaign: String,
    /// Country of the attributed visit.
    pub country: [u8; 2],
    /// Device type of the attributed visit.
    pub device_type: String,
    /// Browser of the attributed visit.
    pub browser: String,
    /// The first page of the attributed visit.
    pub entry_path: String,
}

/// Write `rows` to `table` in one insert, and wait for ClickHouse to accept it.
///
/// For the low-volume tables whose writer is not on a browser's path: the
/// uptime sweep and the revenue webhooks. A webhook in particular must know
/// whether its rows landed, because the honest answer to a provider when they
/// did not is an error, so that the provider delivers the event again.
pub async fn insert_rows<T>(ch: &Ch, table: &str, rows: &[T]) -> Result<()>
where
    T: clickhouse::Row + Serialize,
{
    if rows.is_empty() {
        return Ok(());
    }
    let write = async {
        let mut insert = ch
            .client()
            .insert(table)
            .map_err(|e| Error::Database(format!("clickhouse insert {table}: {e}")))?;
        for row in rows {
            insert
                .write(row)
                .await
                .map_err(|e| Error::Database(format!("clickhouse insert {table}: {e}")))?;
        }
        insert
            .end()
            .await
            .map_err(|e| Error::Database(format!("clickhouse insert {table}: {e}")))
    };
    match tokio::time::timeout(INSERT_DEADLINE, write).await {
        Ok(result) => result,
        Err(_) => Err(Error::Upstream(format!(
            "clickhouse insert {table}: no answer within {}s",
            INSERT_DEADLINE.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_match_the_schema() {
        use clickhouse::Row;
        let schema = |name: &str| {
            super::super::TABLES
                .iter()
                .find(|t| t.name == name)
                .map(|t| t.columns.iter().map(|(n, _)| *n).collect::<Vec<_>>())
                .expect("table")
        };
        assert_eq!(UptimeCheckRow::COLUMN_NAMES, schema("uptime_checks"));
        assert_eq!(LinkClickRow::COLUMN_NAMES, schema("link_clicks"));
        // `type` is a Rust keyword; the rename must survive into the column list.
        assert_eq!(RevenueRow::COLUMN_NAMES, schema("revenue"));
    }
}
