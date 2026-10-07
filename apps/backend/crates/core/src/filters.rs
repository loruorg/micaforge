//! Parsing the `filters` query parameter.
//!
//! `docs/CONTRACT.md` §3.2 defines two shapes, and both are in active use: a
//! flat array of conditions (what the dashboard's filter bar produces, where
//! everything is `AND`ed) and a `{ logic, filters }` group (what a saved segment
//! stores, where nesting matters). A caller should not have to know which one an
//! endpoint prefers, so this module accepts either and always returns a group.

use crate::error::{Error, Result};
use crate::types::{Filter, FilterGroup, FilterNode, Logic};

/// Deepest nesting a filter tree may have.
///
/// The parameter comes off a URL, and a deeply nested tree costs a matching
/// depth of parentheses in the generated SQL. Nobody builds eight levels of
/// nested segment by hand; a request that does is a probe.
const MAX_DEPTH: usize = 8;

/// Most conditions one tree may hold, at any depth.
const MAX_FILTERS: usize = 64;

/// Parse the `filters` parameter.
///
/// Accepts, in order of how often they occur:
///
/// - a flat array, `[{"dimension":"pathname","op":"is","value":"/docs"}]`,
///   which becomes an `AND` group;
/// - a group, `{"logic":"or","filters":[...]}`, used as it is;
/// - a single bare condition object, which becomes a one-member `AND` group;
/// - an empty string, `null`, or `[]`, which becomes an empty group.
///
/// Every condition is validated, so a filter that ClickHouse would reject on a
/// type error is refused here with a message that names the operator instead.
pub fn parse_filters(json: &str) -> Result<FilterGroup> {
    let trimmed = json.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(FilterGroup::default());
    }

    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| Error::validation("filters", format!("is not valid JSON: {e}")))?;

    let group = match value {
        serde_json::Value::Null => FilterGroup::default(),

        serde_json::Value::Array(items) => {
            let nodes: Vec<FilterNode> = serde_json::from_value(serde_json::Value::Array(items))
                .map_err(|e| Error::validation("filters", describe(&e)))?;
            FilterGroup::and(nodes)
        }

        serde_json::Value::Object(ref map) => {
            // A bare condition and a group are told apart by shape, not by a
            // type tag, because the contract's wire form has no tag.
            if map.contains_key("dimension") {
                let filter: Filter = serde_json::from_value(value.clone())
                    .map_err(|e| Error::validation("filters", describe(&e)))?;
                FilterGroup::and(vec![FilterNode::Leaf(filter)])
            } else {
                serde_json::from_value(value.clone())
                    .map_err(|e| Error::validation("filters", describe(&e)))?
            }
        }

        _ => {
            return Err(Error::validation(
                "filters",
                "must be an array of conditions or a {logic, filters} group",
            ))
        }
    };

    check_size(&group, 0, &mut 0)?;
    group.validate()?;
    Ok(group)
}

/// Serialise a group back to the group form.
///
/// Segments are stored as JSON in Postgres, and storing the normalised group
/// rather than whatever the client sent means a segment saved from the filter
/// bar and one saved from the query console read back identically.
pub fn serialize_filters(group: &FilterGroup) -> Result<String> {
    serde_json::to_string(group).map_err(|e| Error::internal(format!("filters: {e}")))
}

/// Merge a saved segment into the request's own filters.
///
/// Both sides must hold, so the result is an `AND` of the two trees rather than
/// a concatenation of their members: concatenating would quietly turn a segment
/// with `OR` logic into something that matches far more than the operator saved.
pub fn merge(base: Option<FilterGroup>, segment: Option<FilterGroup>) -> Option<FilterGroup> {
    match (base, segment) {
        (None, None) => None,
        (Some(g), None) | (None, Some(g)) => Some(g),
        (Some(a), Some(b)) => {
            if a.is_empty() {
                return Some(b);
            }
            if b.is_empty() {
                return Some(a);
            }
            Some(FilterGroup::and(vec![
                FilterNode::Group(a),
                FilterNode::Group(b),
            ]))
        }
    }
}

/// serde's untagged-enum failure ("data did not match any variant") tells a
/// caller nothing, so it is replaced with the shape that was expected.
fn describe(e: &serde_json::Error) -> String {
    let raw = e.to_string();
    if raw.contains("did not match any variant") {
        "each entry must be {dimension, op, value} or {logic, filters}".to_string()
    } else {
        raw
    }
}

fn check_size(group: &FilterGroup, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::validation(
            "filters",
            format!("is nested more than {MAX_DEPTH} levels deep"),
        ));
    }
    for node in &group.filters {
        match node {
            FilterNode::Leaf(_) => {
                *count += 1;
                if *count > MAX_FILTERS {
                    return Err(Error::validation(
                        "filters",
                        format!("holds more than {MAX_FILTERS} conditions"),
                    ));
                }
            }
            FilterNode::Group(g) => check_size(g, depth + 1, count)?,
        }
    }
    Ok(())
}

/// The default logic for a flat array, exposed so callers building a group by
/// hand do not have to guess.
pub const FLAT_ARRAY_LOGIC: Logic = Logic::And;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Dimension, FilterOp, FilterValue};

    fn leaf(group: &FilterGroup, i: usize) -> &Filter {
        match &group.filters[i] {
            FilterNode::Leaf(f) => f,
            FilterNode::Group(_) => panic!("expected a condition at {i}"),
        }
    }

    #[test]
    fn a_flat_array_becomes_an_and_group() {
        let g = parse_filters(
            r#"[{"dimension":"pathname","op":"is","value":"/docs"},
                {"dimension":"country","op":"is_not","value":["US","CA"]}]"#,
        )
        .unwrap();

        assert_eq!(g.logic, Logic::And);
        assert_eq!(g.filters.len(), 2);
        assert_eq!(leaf(&g, 0).dimension, Dimension::Pathname);
        assert_eq!(leaf(&g, 0).op, FilterOp::Is);
        assert_eq!(leaf(&g, 0).value, FilterValue::One("/docs".into()));
        assert_eq!(
            leaf(&g, 1).value,
            FilterValue::Many(vec!["US".into(), "CA".into()])
        );
    }

    #[test]
    fn a_group_keeps_its_logic_and_nesting() {
        let g = parse_filters(
            r#"{"logic":"or","filters":[
                 {"dimension":"channel","op":"is","value":"answer_engine"},
                 {"logic":"and","filters":[
                   {"dimension":"referrer_host","op":"contains","value":"perplexity"},
                   {"dimension":"pathname","op":"starts_with","value":"/docs"}]}]}"#,
        )
        .unwrap();

        assert_eq!(g.logic, Logic::Or);
        assert_eq!(g.filters.len(), 2);
        match &g.filters[1] {
            FilterNode::Group(inner) => {
                assert_eq!(inner.logic, Logic::And);
                assert_eq!(inner.filters.len(), 2);
            }
            FilterNode::Leaf(_) => panic!("expected a nested group"),
        }
        assert_eq!(g.dimensions().len(), 3);
    }

    #[test]
    fn property_and_flag_dimensions_survive_a_round_trip() {
        let g = parse_filters(
            r#"[{"dimension":"props.plan","op":"is","value":"pro"},
                {"dimension":"flag.new_nav","op":"is","value":"b"}]"#,
        )
        .unwrap();

        assert_eq!(leaf(&g, 0).dimension, Dimension::Prop("plan".into()));
        assert_eq!(leaf(&g, 1).dimension, Dimension::Flag("new_nav".into()));

        let round = parse_filters(&serialize_filters(&g).unwrap()).unwrap();
        assert_eq!(round, g);
    }

    #[test]
    fn empty_inputs_are_an_empty_group_not_an_error() {
        for raw in ["", "   ", "null", "[]"] {
            let g = parse_filters(raw).unwrap();
            assert!(g.is_empty(), "`{raw}` should parse to an empty group");
        }
    }

    #[test]
    fn a_bare_condition_object_is_accepted() {
        let g = parse_filters(r#"{"dimension":"browser","op":"is","value":"Firefox"}"#).unwrap();
        assert_eq!(g.filters.len(), 1);
        assert_eq!(leaf(&g, 0).dimension, Dimension::Browser);
    }

    #[test]
    fn value_free_operators_need_no_value() {
        let g = parse_filters(r#"[{"dimension":"utm_campaign","op":"is_set"}]"#).unwrap();
        assert_eq!(leaf(&g, 0).op, FilterOp::IsSet);
    }

    #[test]
    fn numbers_and_mixed_arrays_are_normalised() {
        let g = parse_filters(
            r#"[{"dimension":"revenue","op":"gte","value":49.99},
                {"dimension":"status","op":"is","value":[200,301,"404"]}]"#,
        )
        .unwrap();

        assert_eq!(leaf(&g, 0).value.as_f64(), Some(49.99));
        assert_eq!(
            leaf(&g, 1).value.as_strings(),
            vec!["200".to_string(), "301".to_string(), "404".to_string()],
            "whole numbers must not gain a decimal point on the way to a bind"
        );
    }

    #[test]
    fn malformed_filters_are_refused_with_the_field_named() {
        // An unknown dimension is caught, and is not silently read as a group.
        let e = parse_filters(r#"[{"dimension":"favourite_colour","op":"is","value":"red"}]"#)
            .unwrap_err();
        assert_eq!(e.field(), Some("filters"));

        // An unknown operator.
        assert!(
            parse_filters(r#"[{"dimension":"pathname","op":"sounds_like","value":"x"}]"#).is_err()
        );
        // A comparison with nothing to compare against.
        assert!(parse_filters(r#"[{"dimension":"pathname","op":"is","value":""}]"#).is_err());
        // An inequality against text.
        assert!(parse_filters(r#"[{"dimension":"revenue","op":"gt","value":"lots"}]"#).is_err());
        // Not JSON at all.
        assert!(parse_filters("pathname is /docs").is_err());
        // A shape that is neither an array nor an object.
        assert!(parse_filters("42").is_err());
    }

    #[test]
    fn oversized_trees_are_refused() {
        let one = r#"{"dimension":"pathname","op":"is","value":"/a"}"#;
        let many = format!("[{}]", vec![one; MAX_FILTERS + 1].join(","));
        assert!(parse_filters(&many).is_err());

        let mut deep = r#"{"dimension":"pathname","op":"is","value":"/a"}"#.to_string();
        for _ in 0..(MAX_DEPTH + 2) {
            deep = format!(r#"{{"logic":"and","filters":[{deep}]}}"#);
        }
        assert!(parse_filters(&deep).is_err());
    }

    #[test]
    fn merging_a_segment_keeps_both_sides_intact() {
        let base =
            parse_filters(r#"[{"dimension":"pathname","op":"is","value":"/pricing"}]"#).unwrap();
        let segment = parse_filters(
            r#"{"logic":"or","filters":[
                 {"dimension":"channel","op":"is","value":"search"},
                 {"dimension":"channel","op":"is","value":"answer_engine"}]}"#,
        )
        .unwrap();

        let merged = merge(Some(base), Some(segment)).unwrap();
        assert_eq!(merged.logic, Logic::And);
        assert_eq!(
            merged.filters.len(),
            2,
            "the segment's OR must stay grouped"
        );
        assert_eq!(merged.dimensions().len(), 3);

        assert!(merge(None, None).is_none());
        assert_eq!(FLAT_ARRAY_LOGIC, Logic::And);
    }
}
