//! The row of the `mcp_calls` table: one request an MCP server answered.
//!
//! The people who run a Model Context Protocol server want the same few
//! answers a web operator wants from an access log: which tools are called,
//! by which clients, how fast, and how often they fail. The server SDK's
//! wrapper (`instrumentMcpServer` in `@micaforge/sdk-server`) measures each
//! `tools/call`, `resources/read` and `prompts/get` in process and sends the
//! measurements in signed batches to `POST /api/log/mcp`; one measurement is
//! one row here.
//!
//! What a row never holds is as deliberate as what it does. There is no
//! argument value and no result unless the operator opted in to
//! `captureArguments` (and then only what their redaction hook returned), no
//! error message (only its class and its JSON-RPC code), and no session id
//! (only a keyed digest the SDK made of it, which nothing can turn back).
//!
//! Kept in its own file rather than at the end of `rows.rs` so that agents
//! adding tables in parallel do not edit the same lines.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A row in `mcp_calls`.
///
/// The same rules as [`super::rows`]: field names are the column names, fields
/// are in the schema's column order, and an enum-like column is a `String`.
#[derive(Debug, Clone, Default, PartialEq, clickhouse::Row, Serialize, Deserialize)]
pub struct McpCallRow {
    /// Numeric site id the batch was signed for.
    pub site_id: u16,
    /// When the call started, as the server's clock read it, clamped by
    /// ingest to a sane window around the receiving server's own clock.
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub timestamp: DateTime<Utc>,
    /// The MCP server's own name (`serverInfo.name`), so one site can hold
    /// several servers.
    pub server_name: String,
    /// The MCP server's version (`serverInfo.version`).
    pub server_version: String,
    /// The JSON-RPC method: `tools/call`, `resources/read` or `prompts/get`.
    pub method: String,
    /// The tool or prompt name, or the resource URI without its query.
    pub name: String,
    /// The client's own name from `initialize` (`clientInfo.name`).
    pub client_name: String,
    /// The client's version from `initialize` (`clientInfo.version`).
    pub client_version: String,
    /// The protocol revision the session negotiated.
    pub protocol_version: String,
    /// `stdio`, `streamable-http`, `sse`, or whatever the SDK could name.
    pub transport: String,
    /// A keyed digest of the MCP session id, empty when there was none.
    pub session_hash: String,
    /// How long the handler took, in milliseconds.
    pub duration_ms: u32,
    /// `ok`, `error` (the request failed) or `tool_error` (a tool answered
    /// with `isError: true`).
    pub status: String,
    /// The error's class name, such as `McpError` or `TypeError`. Never its
    /// message.
    pub error_type: String,
    /// The JSON-RPC error code, `0` when there was none.
    pub error_code: i32,
    /// Size of the serialised arguments, in bytes.
    pub arg_bytes: u32,
    /// Size of the serialised result, in bytes.
    pub result_bytes: u32,
    /// The arguments as JSON, only when the operator opted in, after their
    /// redaction hook. Empty otherwise.
    pub arguments: String,
    /// The SDK that measured the call, such as `sdk-server/0.1.0`.
    pub sdk_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_match_the_schema() {
        use clickhouse::Row;
        let schema = super::super::TABLES
            .iter()
            .find(|t| t.name == "mcp_calls")
            .map(|t| t.columns.iter().map(|(n, _)| *n).collect::<Vec<_>>())
            .expect("table");
        assert_eq!(McpCallRow::COLUMN_NAMES, schema);
    }
}
