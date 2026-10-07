//! Turning a [`micaforge_core::Error`] into an HTTP response.
//!
//! `micaforge-core` deliberately knows nothing about `axum`, because it has to
//! stay usable from the worker, the importer and the tests without pulling in a
//! web framework. So the mapping lives here, in the one crate that already
//! depends on both.
//!
//! [`ApiError`] is a newtype rather than an `impl IntoResponse for Error`
//! because the orphan rule forbids the latter: `Error` and `IntoResponse` are
//! both foreign to this crate. The cost is a `?` conversion at each handler
//! boundary, which `From<Error>` supplies, and the benefit is that a handler
//! returning `Result<Json<Value>, ApiError>` can `?` straight from a repository
//! call.
//!
//! ## What reaches the client
//!
//! `docs/CONTRACT.md` §3.3 fixes the shape:
//!
//! ```json
//! { "error": { "code": "snake_case", "message": "human sentence", "field": "…" } }
//! ```
//!
//! The `code` is what a client branches on and is part of the public contract.
//! The `message` is for a person. Which message is safe to send is not decided
//! here. [`micaforge_core::Error::is_public`] decides it, because the crate
//! that constructed the error is the one that knows whether it carries a
//! connection string, a query fragment or a host name. Everything it says is
//! private is logged in full at `error` level and replaced on the wire by a
//! fixed sentence.
//!
//! There is exactly one deliberate omission: the response carries no internal
//! identifier for the log line. A correlation id already travels on every
//! response as `x-request-id` (see [`crate::middleware`]), so adding a second
//! one here would give an operator two things to grep for instead of one.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};

use micaforge_core::Error;

/// A [`micaforge_core::Error`] on its way to a client.
///
/// The field is public so a handler can reach the error it is carrying. A few
/// do, to decide whether a failure is worth a log line of their own before they
/// return it.
#[derive(Debug)]
pub struct ApiError(pub Error);

impl ApiError {
    /// The error underneath.
    pub fn inner(&self) -> &Error {
        &self.0
    }

    /// The HTTP status this failure is reported with.
    ///
    /// [`micaforge_core::Error`] owns the numeric mapping; this only converts it
    /// into `axum`'s type, falling back to `500` for a number that is somehow not
    /// a valid status, which cannot happen from the current variants and would be
    /// a bug rather than a request failure if it ever did.
    pub fn status(&self) -> StatusCode {
        StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// The error envelope's body, with the private detail already removed.
    fn body(&self) -> Value {
        let mut error = Map::new();
        error.insert("code".to_string(), json!(self.0.code()));
        error.insert("message".to_string(), json!(self.public_message()));
        if let Some(field) = self.0.field() {
            error.insert("field".to_string(), json!(field));
        }
        json!({ "error": Value::Object(error) })
    }

    /// The sentence that is safe to send.
    ///
    /// A store or upstream failure's own text names hosts, queries and sometimes
    /// credentials, so it never travels. The substitutes are written to be useful
    /// to the person reading them: "try again" is true and actionable for a
    /// transient database fault, and a `502` says plainly that the fault is not
    /// in this process.
    fn public_message(&self) -> String {
        if self.0.is_public() {
            return self.0.to_string();
        }
        match self.0 {
            Error::Upstream(_) => {
                "a service Micaforge depends on did not answer; try again shortly".to_string()
            }
            _ => "something went wrong on our side; the failure has been logged".to_string(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError(Error::from(e))
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError(Error::from(e))
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();

        // The log line is where the real message goes. `error` for the failures
        // that are ours, `warn` for the ones that are the caller's: an operator
        // grepping for `ERROR` should find things that need fixing, not a
        // hundred rejected passwords.
        if self.0.is_public() {
            tracing::debug!(code = self.0.code(), status = status.as_u16(), error = %self.0, "request refused");
        } else {
            tracing::error!(code = self.0.code(), status = status.as_u16(), error = %self.0, "request failed");
        }

        let mut response = (status, Json(self.body())).into_response();

        // `Retry-After` is the only header a failure carries, and it is the one
        // a well-behaved client actually reads. Whole seconds, per RFC 9110.
        if let Error::RateLimited { retry_after } = self.0 {
            if let Ok(value) = retry_after.to_string().parse() {
                response.headers_mut().insert("retry-after", value);
            }
        }

        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_validation_failure_names_its_field() {
        let e = ApiError(Error::validation("domain", "is not a hostname"));
        assert_eq!(e.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = e.body();
        assert_eq!(body["error"]["code"], "validation_error");
        assert_eq!(body["error"]["field"], "domain");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("hostname"));
    }

    #[test]
    fn a_database_failure_never_leaks_its_detail() {
        // The exact thing that must not be served: a driver message carrying a
        // connection string.
        let e = ApiError(Error::Database(
            "connection to host=db user=micaforge password=hunter2 failed".into(),
        ));
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let rendered = e.body().to_string();
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("host=db"));
        assert_eq!(e.body()["error"]["code"], "database_error");
    }

    #[test]
    fn an_upstream_failure_says_so_without_naming_the_upstream() {
        let e = ApiError(Error::Upstream(
            "redis://cache.internal:6379 refused".into(),
        ));
        assert_eq!(e.status(), StatusCode::BAD_GATEWAY);
        let rendered = e.body().to_string();
        assert!(!rendered.contains("cache.internal"));
        assert!(rendered.contains("did not answer"));
    }

    #[test]
    fn a_public_failure_is_reported_verbatim() {
        let e = ApiError(Error::Conflict("example.com is already tracked".into()));
        assert!(e.body()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("already tracked"));
        assert!(e.body()["error"]["field"].is_null());
    }

    #[test]
    fn a_rate_limit_carries_retry_after() {
        let response = ApiError(Error::RateLimited { retry_after: 30 }).into_response();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get("retry-after").unwrap(), "30");
    }
}
