//! The success envelope every JSON route answers with.
//!
//! `docs/CONTRACT.md` §3.3 fixes one shape for every successful response:
//!
//! ```json
//! { "data": <payload>, "meta": { "query": {}, "rows": 42, "sampled": false, "took_ms": 18 } }
//! ```
//!
//! The envelope is built here rather than in each handler for one reason worth
//! stating: a client that has to branch on whether a particular endpoint wrapped
//! its answer will eventually get the branch wrong, and the failure looks like
//! missing data rather than like a bug. So there are exactly two ways to answer
//! ([`ok`] and [`ok_meta`]) and both produce the same two keys.
//!
//! Both return [`axum::Json<Value>`] rather than a typed envelope. That is
//! deliberate: handlers assemble their payloads as `serde_json::Value` (a
//! breakdown row set is not one Rust type, it is whatever the requested
//! dimension produced), and a generic envelope would only move the erasure one
//! layer out while making every handler signature longer.

use axum::Json;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// Wrap a payload in the standard envelope.
///
/// `meta.rows` is filled in automatically when the payload is an array, because
/// that is the case where a client genuinely wants the count and where a handler
/// is most likely to forget to supply one. `meta.sampled` is always present and
/// always `false`: Micaforge answers from the whole table, and a key that only
/// appeared when sampling happened would be a key nobody checked.
pub fn ok<T: Serialize>(data: T) -> Json<Value> {
    let data = to_value(data);
    let rows = match &data {
        Value::Array(items) => Some(items.len()),
        _ => None,
    };

    let mut meta = Map::new();
    if let Some(rows) = rows {
        meta.insert("rows".to_string(), json!(rows));
    }
    meta.insert("sampled".to_string(), json!(false));

    Json(json!({ "data": data, "meta": Value::Object(meta) }))
}

/// Wrap a payload with meta the handler assembled itself.
///
/// The caller's `meta` wins on every key it sets, and `sampled` is filled in
/// underneath when it did not set one. Anything that is not a JSON object is
/// carried through under a `meta.value` key rather than discarded, because
/// silently dropping something a handler went to the trouble of computing is
/// worse than an odd-looking key.
pub fn ok_meta<T: Serialize, M: Serialize>(data: T, meta: M) -> Json<Value> {
    let mut base = Map::new();
    base.insert("sampled".to_string(), json!(false));

    match to_value(meta) {
        Value::Object(supplied) => {
            for (key, value) in supplied {
                base.insert(key, value);
            }
        }
        Value::Null => {}
        other => {
            base.insert("value".to_string(), other);
        }
    }

    Json(json!({ "data": to_value(data), "meta": Value::Object(base) }))
}

/// Serialise, treating a failure as an empty payload rather than a panic.
///
/// Every type that reaches here is a `serde_json::Value` or a struct built from
/// one, so this cannot fail in practice. If it ever does, a `null` payload with
/// a log line is a far better outcome than a `500` on a request whose work has
/// already been done, or an `unwrap` on the response path.
fn to_value<T: Serialize>(value: T) -> Value {
    match serde_json::to_value(value) {
        Ok(value) => value,
        Err(e) => {
            tracing::error!(error = %e, "a response payload could not be serialised");
            Value::Null
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_envelope_always_has_both_keys() {
        let Json(body) = ok(json!({ "visitors": 12 }));
        assert_eq!(body["data"]["visitors"], 12);
        assert_eq!(body["meta"]["sampled"], false);
        assert!(body["meta"]["rows"].is_null(), "an object has no row count");
    }

    #[test]
    fn an_array_payload_counts_itself() {
        let Json(body) = ok(vec![json!({ "a": 1 }), json!({ "a": 2 })]);
        assert_eq!(body["meta"]["rows"], 2);
    }

    #[test]
    fn supplied_meta_wins_and_is_completed() {
        let Json(body) = ok_meta(json!([]), json!({ "rows": 99, "took_ms": 4 }));
        assert_eq!(
            body["meta"]["rows"], 99,
            "the handler's count is authoritative"
        );
        assert_eq!(body["meta"]["took_ms"], 4);
        assert_eq!(body["meta"]["sampled"], false, "filled in underneath");
    }
}
