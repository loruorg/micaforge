//! The one error type.
//!
//! Every fallible public function in Micaforge returns [`Result`]. The variants
//! are chosen to map cleanly onto the error envelope in `docs/CONTRACT.md` §3.3
//! (`{ "error": { "code", "message", "field" } }`), so the HTTP layer never has
//! to guess a status code from a string.
//!
//! `Error` carries `String` payloads rather than boxed source errors on purpose:
//! the driver-level detail of a failed query is useful in a log line and is a
//! liability in a response body, so it is flattened at the boundary where it is
//! still safe to read it, and only the flattened form travels.

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Every failure Micaforge reports.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// The addressed resource does not exist, or the caller may not know that it
    /// does. Site lookups for a caller without access return this rather than
    /// [`Error::Forbidden`], so the API cannot be used to enumerate site ids.
    #[error("not found: {0}")]
    NotFound(String),

    /// No credential, or a credential that did not verify.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// A valid credential without the rights for this action.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The request collides with existing state: a duplicate domain, a slug
    /// already taken, a second owner for a single-owner row.
    #[error("conflict: {0}")]
    Conflict(String),

    /// One named input was wrong. `field` is echoed to the client so a form can
    /// highlight the offending control instead of showing a banner.
    #[error("invalid {field}: {message}")]
    Validation {
        /// The input's name, in the same spelling the client sent it.
        field: String,
        /// A human sentence describing what was expected.
        message: String,
    },

    /// The caller exceeded a rate limit. `retry_after` is whole seconds and is
    /// copied straight into the `Retry-After` header.
    #[error("rate limited, retry after {retry_after}s")]
    RateLimited {
        /// Seconds the caller should wait before retrying.
        retry_after: u64,
    },

    /// The request was malformed in a way that is not tied to one field.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Postgres or ClickHouse failed. Never surfaced verbatim to a client.
    #[error("database error: {0}")]
    Database(String),

    /// A dependency outside the process failed: Redis, SMTP, an operator's
    /// published IP-range endpoint, a site's own `robots.txt`.
    #[error("upstream error: {0}")]
    Upstream(String),

    /// A bug, or an invariant that did not hold.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// The stable `snake_case` code that goes in the error envelope.
    ///
    /// Clients branch on this, never on the message, so these strings are part
    /// of the public contract and are not free to change.
    pub fn code(&self) -> &'static str {
        match self {
            Error::NotFound(_) => "not_found",
            Error::Unauthorized(_) => "unauthorized",
            Error::Forbidden(_) => "forbidden",
            Error::Conflict(_) => "conflict",
            Error::Validation { .. } => "validation_error",
            Error::RateLimited { .. } => "rate_limited",
            Error::BadRequest(_) => "bad_request",
            Error::Database(_) => "database_error",
            Error::Upstream(_) => "upstream_error",
            Error::Internal(_) => "internal_error",
        }
    }

    /// The HTTP status this failure is reported with.
    pub fn status(&self) -> u16 {
        match self {
            Error::NotFound(_) => 404,
            Error::Unauthorized(_) => 401,
            Error::Forbidden(_) => 403,
            Error::Conflict(_) => 409,
            Error::Validation { .. } => 422,
            Error::RateLimited { .. } => 429,
            Error::BadRequest(_) => 400,
            Error::Database(_) => 500,
            Error::Upstream(_) => 502,
            Error::Internal(_) => 500,
        }
    }

    /// The offending input's name, when the failure names one.
    pub fn field(&self) -> Option<&str> {
        match self {
            Error::Validation { field, .. } => Some(field.as_str()),
            _ => None,
        }
    }

    /// True when the message is safe to hand to a client verbatim.
    ///
    /// Store and upstream failures carry driver text (host names, query
    /// fragments, occasionally credentials in a connection string), so the HTTP
    /// layer substitutes a fixed sentence for those and logs the real one.
    pub fn is_public(&self) -> bool {
        !matches!(
            self,
            Error::Database(_) | Error::Upstream(_) | Error::Internal(_)
        )
    }

    /// Shorthand for [`Error::Validation`].
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Error::Validation {
            field: field.into(),
            message: message.into(),
        }
    }

    /// Shorthand for [`Error::NotFound`].
    pub fn not_found(what: impl Into<String>) -> Self {
        Error::NotFound(what.into())
    }

    /// Shorthand for [`Error::Internal`].
    pub fn internal(what: impl Into<String>) -> Self {
        Error::Internal(what.into())
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        match e {
            // A missing row is a 404 far more often than it is a 500. Callers
            // that need "absent is fine" use `fetch_optional` and never get here.
            sqlx::Error::RowNotFound => Error::NotFound("row not found".to_string()),
            sqlx::Error::Database(db) => {
                if db.is_unique_violation() || db.is_foreign_key_violation() {
                    Error::Conflict(db.message().to_string())
                } else if db.is_check_violation() {
                    // Enums are TEXT + CHECK in this schema, so a bad role or a
                    // bad cadence lands here and is genuinely the caller's fault.
                    Error::BadRequest(db.message().to_string())
                } else {
                    Error::Database(db.message().to_string())
                }
            }
            other => Error::Database(other.to_string()),
        }
    }
}

impl From<redis::RedisError> for Error {
    fn from(e: redis::RedisError) -> Self {
        // Redis holds sessions, rate-limit counters and the realtime gauge. None
        // of those are the system of record, so a Redis failure is an upstream
        // fault and not a data loss.
        Error::Upstream(format!("redis: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        match e.classify() {
            // Only an I/O category failure is ours; the rest came off the wire.
            serde_json::error::Category::Io => Error::Internal(format!("json io: {e}")),
            _ => Error::BadRequest(format!("invalid json: {e}")),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(e: anyhow::Error) -> Self {
        Error::Internal(format!("{e:#}"))
    }
}

impl From<url::ParseError> for Error {
    fn from(e: url::ParseError) -> Self {
        // A URL only reaches this crate from a tracker payload or a site's
        // configured domain, so it is always an input, never an internal value.
        Error::validation("url", e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_statuses_line_up() {
        assert_eq!(Error::not_found("site").status(), 404);
        assert_eq!(Error::not_found("site").code(), "not_found");
        assert_eq!(Error::validation("site", "required").status(), 422);
        assert_eq!(Error::validation("site", "required").field(), Some("site"));
        assert_eq!(Error::RateLimited { retry_after: 30 }.status(), 429);
        assert!(!Error::Database("host=x password=y".into()).is_public());
        assert!(Error::not_found("site").is_public());
    }

    #[test]
    fn json_syntax_errors_are_the_callers_fault() {
        let e: Error = serde_json::from_str::<serde_json::Value>("{oops")
            .unwrap_err()
            .into();
        assert_eq!(e.status(), 400);
    }
}
