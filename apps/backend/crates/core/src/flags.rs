//! Feature-flag evaluation: one pure function, implemented three times.
//!
//! A flag is decided in three places. The server decides it for the browser
//! (`GET /api/flags/evaluate`), the Node SDK decides it locally from a cached
//! ruleset so a request handler never waits on the network, and the browser SDK
//! decides it locally from a ruleset or a fallback rule while the network is in
//! flight. If any two of those disagree for the same visitor, a person sees a
//! feature appear and vanish between a server render and a client render, and an
//! experiment's exposure counts stop meaning anything. So the rule is written
//! down once, here, as data in and data out, and the two TypeScript copies
//! (`packages/sdk/src/flag-eval.ts`, `packages/sdk-server/src/flag-eval.ts`) are
//! held to it by one shared file of test vectors,
//! `packages/sdk/test/flag-vectors.json`, which all three test suites assert.
//!
//! ## The decision, in order
//!
//! 1. An unknown key is off (`unknown`).
//! 2. An environment override replaces `enabled` and `rollout_pct` when the
//!    caller names an environment the flag configures.
//! 3. A flag that is not enabled is off (`disabled`), whatever else it says.
//! 4. Every prerequisite flag must evaluate to the state it names
//!    (`dependency`). Cycles are refused when a flag is saved; at evaluation a
//!    cycle or a chain deeper than [`MAX_DEPENDENCY_DEPTH`] is off rather than a
//!    stack overflow.
//! 5. With target groups attached, the visitor must match at least one of them
//!    (`target_group`).
//! 6. The flag's own conditions must match (`conditions`).
//! 7. The rollout: a visitor whose bucket falls outside the percentage is off
//!    (`rollout`), and a visitor with no stable subject cannot be held in or out
//!    of a partial rollout, so is off too (`no_visitor`).
//! 8. Otherwise on (`match`), with a variant picked by weight.
//!
//! Conditions run before the rollout on purpose: a 50% rollout among mobile
//! visitors is half of the mobile visitors, not a quarter of everybody.
//!
//! ## Bucketing is the visitor hash
//!
//! [`bucket`] is `blake3(salt ‖ subject)` reduced to `0..10 000`, computed
//! through [`crate::crypto::visitor_hash`] so flag bucketing and the cookieless
//! visitor id share one primitive. The rollout is salted with the flag key, the
//! variant pick with `"<key>:variant"`: two salts, so falling inside a narrow
//! rollout does not bias which variant somebody lands in.
//!
//! ## Lenient reading, strict writing
//!
//! A ruleset is read leniently, because it comes back from a cache, a database
//! row written by an older build, or a network: anything malformed evaluates
//! *off* rather than failing the request, which is the safe direction for a
//! flag. What is written is checked strictly, by [`check_conditions`],
//! [`check_environments`], [`check_dependencies`] and [`find_cycle`], so the
//! lenient path should only ever meet malformed data that somebody wrote by
//! hand.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

/// Buckets a visitor can fall into: a rollout can be expressed to a hundredth
/// of a percent even though the stored field is whole percent.
pub const BUCKETS: u64 = 10_000;

/// The deepest chain of prerequisites one evaluation follows.
///
/// Eight is far past any honest use (a flag that depends on a flag that
/// depends on a flag is already hard to reason about) and keeps a ruleset that
/// slipped a cycle past validation from recursing without end.
pub const MAX_DEPENDENCY_DEPTH: usize = 8;

/// Deepest nesting a condition tree may have when it is written.
const MAX_CONDITION_DEPTH: usize = 8;

/// Most leaves a condition tree may hold when it is written.
const MAX_CONDITION_LEAVES: usize = 64;

/// Longest attribute name a condition may test.
const MAX_ATTRIBUTE_LEN: usize = 64;

/// Longest environment name.
const MAX_ENVIRONMENT_LEN: usize = 32;

/// Most environments one flag may override.
const MAX_ENVIRONMENTS: usize = 16;

/// Most prerequisites one flag may have.
const MAX_DEPENDENCIES: usize = 16;

/// Why a flag evaluated the way it did. The wire spelling is the contract
/// with every SDK and with the stats table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// No flag has that key.
    Unknown,
    /// The flag is switched off.
    Disabled,
    /// A prerequisite flag did not evaluate to the state this flag needs.
    Dependency,
    /// The visitor matched none of the flag's target groups.
    TargetGroup,
    /// The visitor did not match the flag's own conditions.
    Conditions,
    /// A partial rollout, and no stable subject to bucket.
    NoVisitor,
    /// The visitor's bucket is outside the rollout.
    Rollout,
    /// On.
    Match,
}

impl Reason {
    /// The wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Reason::Unknown => "unknown",
            Reason::Disabled => "disabled",
            Reason::Dependency => "dependency",
            Reason::TargetGroup => "target_group",
            Reason::Conditions => "conditions",
            Reason::NoVisitor => "no_visitor",
            Reason::Rollout => "rollout",
            Reason::Match => "match",
        }
    }
}

/// One flag, decided for one visitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    /// Whether the visitor gets the flag.
    pub enabled: bool,
    /// The variant they get, or `None` for an on/off flag or an off decision.
    pub variant: Option<String>,
    /// Why.
    pub reason: Reason,
}

impl Decision {
    fn off(reason: Reason) -> Decision {
        Decision {
            enabled: false,
            variant: None,
            reason,
        }
    }

    /// The decision as the SDKs read it: `{ enabled, variant, reason }`.
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "variant": self.variant,
            "reason": self.reason.as_str(),
        })
    }

    /// The label the stats table and the dashboard group by: the variant key,
    /// `on` for an on/off flag that is on, `off` for any off decision.
    pub fn label(&self) -> &str {
        match (&self.variant, self.enabled) {
            (_, false) => "off",
            (Some(variant), true) => variant.as_str(),
            (None, true) => "on",
        }
    }
}

/// Who is asking, and about what.
#[derive(Debug, Clone, Default)]
pub struct EvalInput {
    /// The bucketing subject: the identified id when the caller has one, else
    /// the visitor id. Empty means no stable identity.
    pub subject: String,
    /// Attributes the conditions test, keyed lower-case.
    pub context: BTreeMap<String, String>,
    /// The environment the caller runs in, lower-case. Empty means none.
    pub environment: String,
}

impl EvalInput {
    /// Build an input, normalising the context the way every implementation
    /// does: keys trimmed and lower-cased, values in their canonical string
    /// form ([`canonical_value`]).
    pub fn new(
        subject: &str,
        context: &Map<String, Value>,
        environment: Option<&str>,
    ) -> EvalInput {
        let context = context
            .iter()
            .map(|(key, value)| (key.trim().to_lowercase(), canonical_value(value)))
            .collect();
        EvalInput {
            subject: subject.to_string(),
            context,
            environment: environment
                .map(|e| e.trim().to_lowercase())
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// The ruleset, read leniently
// ---------------------------------------------------------------------------

/// Every flag on a site and every target group they reference, parsed once.
#[derive(Debug, Clone, Default)]
pub struct Ruleset {
    flags: BTreeMap<String, FlagRule>,
    groups: BTreeMap<String, Node>,
}

/// One flag, as the evaluator reads it.
#[derive(Debug, Clone)]
struct FlagRule {
    enabled: bool,
    rollout: i64,
    variants: Vec<(String, u64)>,
    conditions: Node,
    target_groups: Vec<String>,
    dependencies: Vec<Option<Dependency>>,
    environments: BTreeMap<String, EnvOverride>,
}

/// A prerequisite.
#[derive(Debug, Clone)]
struct Dependency {
    flag: String,
    enabled: bool,
    variant: Option<String>,
}

/// What one environment changes.
#[derive(Debug, Clone, Default)]
struct EnvOverride {
    enabled: Option<bool>,
    rollout: Option<i64>,
}

impl Ruleset {
    /// Read a ruleset document: `{ flags: [...], target_groups: { id: { conditions } } }`.
    ///
    /// Never fails. A flag with no usable key is skipped, and the first of two
    /// flags with one key wins, which cannot happen for a ruleset the server
    /// built because keys are unique per site.
    pub fn from_json(doc: &Value) -> Ruleset {
        let mut flags = BTreeMap::new();
        if let Some(list) = doc.get("flags").and_then(Value::as_array) {
            for raw in list {
                let Some(key) = raw.get("key").and_then(Value::as_str) else {
                    continue;
                };
                if key.is_empty() || flags.contains_key(key) {
                    continue;
                }
                flags.insert(key.to_string(), FlagRule::from_json(raw));
            }
        }

        let mut groups = BTreeMap::new();
        if let Some(map) = doc.get("target_groups").and_then(Value::as_object) {
            for (id, group) in map {
                let conditions = group.get("conditions").unwrap_or(&Value::Null);
                groups.insert(id.clone(), parse_top(conditions));
            }
        }

        Ruleset { flags, groups }
    }

    /// Every flag key, in order.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.flags.keys()
    }

    /// Decide one flag.
    pub fn evaluate(&self, key: &str, input: &EvalInput) -> Decision {
        let mut stack = Vec::new();
        self.decide(key, input, &mut stack)
    }

    /// Decide every flag.
    pub fn evaluate_all(&self, input: &EvalInput) -> BTreeMap<String, Decision> {
        self.flags
            .keys()
            .map(|key| (key.clone(), self.evaluate(key, input)))
            .collect()
    }

    fn decide(&self, key: &str, input: &EvalInput, stack: &mut Vec<String>) -> Decision {
        let Some(rule) = self.flags.get(key) else {
            return Decision::off(Reason::Unknown);
        };

        let mut enabled = rule.enabled;
        let mut rollout = rule.rollout;
        if !input.environment.is_empty() {
            if let Some(over) = rule.environments.get(&input.environment) {
                enabled = over.enabled.unwrap_or(enabled);
                rollout = over.rollout.unwrap_or(rollout);
            }
        }

        if !enabled {
            return Decision::off(Reason::Disabled);
        }

        stack.push(key.to_string());
        let held = rule
            .dependencies
            .iter()
            .all(|dep| self.dependency_holds(dep.as_ref(), input, stack));
        stack.pop();
        if !held {
            return Decision::off(Reason::Dependency);
        }

        if !rule.target_groups.is_empty() {
            let matched = rule.target_groups.iter().any(|id| {
                self.groups
                    .get(id)
                    .is_some_and(|group| group.matches_top(&input.context))
            });
            if !matched {
                return Decision::off(Reason::TargetGroup);
            }
        }

        if !rule.conditions.matches_top(&input.context) {
            return Decision::off(Reason::Conditions);
        }

        let rollout = rollout.clamp(0, 100) as u64;
        if rollout < 100 {
            if input.subject.is_empty() {
                return Decision::off(Reason::NoVisitor);
            }
            if bucket(key, &input.subject) >= rollout * (BUCKETS / 100) {
                return Decision::off(Reason::Rollout);
            }
        }

        Decision {
            enabled: true,
            variant: pick_variant(&rule.variants, key, &input.subject),
            reason: Reason::Match,
        }
    }

    /// Does one prerequisite hold? A malformed one never does.
    fn dependency_holds(
        &self,
        dep: Option<&Dependency>,
        input: &EvalInput,
        stack: &mut Vec<String>,
    ) -> bool {
        let Some(dep) = dep else {
            return false;
        };
        if stack.iter().any(|k| k == &dep.flag) || stack.len() >= MAX_DEPENDENCY_DEPTH {
            return false;
        }
        let decided = self.decide(&dep.flag, input, stack);
        if decided.enabled != dep.enabled {
            return false;
        }
        match &dep.variant {
            Some(wanted) => decided.variant.as_deref() == Some(wanted.as_str()),
            None => true,
        }
    }
}

impl FlagRule {
    fn from_json(raw: &Value) -> FlagRule {
        let variants = raw
            .get("variants")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|v| {
                        let key = v.get("key").and_then(Value::as_str)?;
                        if key.is_empty() {
                            return None;
                        }
                        Some((key.to_string(), weight_of(v.get("weight"))))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let target_groups = raw
            .get("target_groups")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let dependencies = raw
            .get("dependencies")
            .and_then(Value::as_array)
            .map(|list| list.iter().map(Dependency::from_json).collect())
            .unwrap_or_default();

        let environments = raw
            .get("environments")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .map(|(name, over)| {
                        (
                            name.trim().to_lowercase(),
                            EnvOverride {
                                enabled: over.get("enabled").and_then(Value::as_bool),
                                rollout: over
                                    .get("rollout_pct")
                                    .and_then(Value::as_f64)
                                    .map(round_rollout),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        FlagRule {
            enabled: raw.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            rollout: rollout_of(raw.get("rollout_pct")),
            variants,
            conditions: parse_top(raw.get("conditions").unwrap_or(&Value::Null)),
            target_groups,
            dependencies,
            environments,
        }
    }
}

impl Dependency {
    /// `None` for an entry with no usable flag key: it can never hold.
    fn from_json(raw: &Value) -> Option<Dependency> {
        let flag = raw.get("flag").and_then(Value::as_str)?;
        if flag.is_empty() {
            return None;
        }
        Some(Dependency {
            flag: flag.to_string(),
            // Anything but an explicit `false` asks for the prerequisite on.
            enabled: raw.get("enabled").and_then(Value::as_bool) != Some(false),
            variant: raw
                .get("variant")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string),
        })
    }
}

/// A rollout: absent or null is everyone, a number is rounded and later
/// clamped, and anything else is nobody, which is the safe reading of a value
/// nobody can interpret.
fn rollout_of(raw: Option<&Value>) -> i64 {
    match raw {
        None | Some(Value::Null) => 100,
        Some(value) => value.as_f64().map(round_rollout).unwrap_or(0),
    }
}

/// Round a rollout to a whole percent, half away from zero, saturating.
fn round_rollout(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    value.round().clamp(-1.0, 101.0) as i64
}

/// A variant weight: absent or null is one, a whole non-negative number is
/// itself, and anything else is zero (never picked).
fn weight_of(raw: Option<&Value>) -> u64 {
    match raw {
        None | Some(Value::Null) => 1,
        Some(value) => match value.as_f64() {
            Some(w) if w >= 0.0 && w.fract() == 0.0 && w <= 9_007_199_254_740_991.0 => w as u64,
            _ => 0,
        },
    }
}

/// Which of [`BUCKETS`] a subject falls into for one salt.
///
/// The bytes hashed are `salt ‖ 0x1f ‖ 0x0000 ‖ 0x1f ‖ subject ‖ 0x1f`, which
/// is exactly [`crate::crypto::visitor_hash`] with site id zero and an empty
/// user agent; the first eight bytes of the digest, read big-endian, are
/// reduced modulo [`BUCKETS`]. The TypeScript copies hash the same bytes.
pub fn bucket(salt: &str, subject: &str) -> u64 {
    let digest = crate::crypto::visitor_hash(salt.as_bytes(), 0, subject, "");
    u64::from_str_radix(&digest, 16).unwrap_or(0) % BUCKETS
}

/// Choose a variant by weight, or `None` for an on/off flag.
fn pick_variant(variants: &[(String, u64)], key: &str, subject: &str) -> Option<String> {
    let first = variants.first()?;
    let total: u64 = variants.iter().map(|(_, w)| *w).sum();
    if total == 0 {
        return Some(first.0.clone());
    }
    let point = bucket(&format!("{key}:variant"), subject) * total / BUCKETS;
    let mut seen = 0u64;
    for (name, weight) in variants {
        seen += weight;
        if point < seen {
            return Some(name.clone());
        }
    }
    variants.last().map(|(name, _)| name.clone())
}

// ---------------------------------------------------------------------------
// Conditions
// ---------------------------------------------------------------------------

/// A parsed condition tree. `Invalid` is anything that could not be read and
/// always evaluates false.
#[derive(Debug, Clone)]
enum Node {
    Leaf(Leaf),
    Group { any: bool, nodes: Vec<Node> },
    Invalid,
}

/// One test of one attribute.
#[derive(Debug, Clone)]
struct Leaf {
    attribute: String,
    op: Op,
    wanted: Vec<String>,
    number: Option<f64>,
}

/// The operators a flag condition may use. `matches` is not one: a regular
/// expression is evaluated by ClickHouse in a report, and three runtimes would
/// each bring their own dialect to a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Is,
    IsNot,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    IsSet,
    IsNotSet,
    Gt,
    Lt,
    Gte,
    Lte,
}

impl Op {
    fn parse(raw: &str) -> Option<Op> {
        Some(match raw {
            "is" => Op::Is,
            "is_not" => Op::IsNot,
            "contains" => Op::Contains,
            "not_contains" => Op::NotContains,
            "starts_with" => Op::StartsWith,
            "ends_with" => Op::EndsWith,
            "is_set" => Op::IsSet,
            "is_not_set" => Op::IsNotSet,
            "gt" => Op::Gt,
            "lt" => Op::Lt,
            "gte" => Op::Gte,
            "lte" => Op::Lte,
            _ => return None,
        })
    }

    fn needs_value(&self) -> bool {
        !matches!(self, Op::IsSet | Op::IsNotSet)
    }

    fn is_numeric(&self) -> bool {
        matches!(self, Op::Gt | Op::Lt | Op::Gte | Op::Lte)
    }
}

/// Parse the top of a condition document.
///
/// Absent, `null` and `[]` are the empty group; an array is an `AND` of its
/// nodes; an object is a leaf when it has `dimension` and a group otherwise.
fn parse_top(raw: &Value) -> Node {
    match raw {
        Value::Null => Node::Group {
            any: false,
            nodes: Vec::new(),
        },
        Value::Array(items) => Node::Group {
            any: false,
            nodes: items.iter().map(parse_node).collect(),
        },
        Value::Object(map) if map.contains_key("dimension") => Node::Group {
            any: false,
            nodes: vec![parse_node(raw)],
        },
        Value::Object(_) => parse_node(raw),
        _ => Node::Invalid,
    }
}

/// Parse one node: a leaf or a nested group.
fn parse_node(raw: &Value) -> Node {
    let Some(map) = raw.as_object() else {
        return Node::Invalid;
    };
    if map.contains_key("dimension") {
        return parse_leaf(map).map(Node::Leaf).unwrap_or(Node::Invalid);
    }

    let any = match map.get("logic") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) if s == "and" => false,
        Some(Value::String(s)) if s == "or" => true,
        _ => return Node::Invalid,
    };
    let nodes = match map.get("filters") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(parse_node).collect(),
        _ => return Node::Invalid,
    };
    Node::Group { any, nodes }
}

/// Parse a leaf, or `None` when it cannot be evaluated.
fn parse_leaf(map: &Map<String, Value>) -> Option<Leaf> {
    let attribute = map
        .get("dimension")
        .and_then(Value::as_str)
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty())?;
    let op = map.get("op").and_then(Value::as_str).and_then(Op::parse)?;
    let value = map.get("value").unwrap_or(&Value::Null);

    let wanted = match value {
        Value::Null => vec![String::new()],
        Value::Array(items) => items
            .iter()
            .filter(|item| matches!(item, Value::String(_) | Value::Number(_) | Value::Bool(_)))
            .map(canonical_value)
            .collect(),
        other => vec![canonical_value(other)],
    };
    let number = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => parse_number(s),
        _ => None,
    };

    Some(Leaf {
        attribute,
        op,
        wanted,
        number,
    })
}

impl Node {
    /// The top-level rule: a group with nothing in it constrains nothing,
    /// whatever its logic says.
    fn matches_top(&self, context: &BTreeMap<String, String>) -> bool {
        match self {
            Node::Group { nodes, .. } if nodes.is_empty() => true,
            other => other.matches(context),
        }
    }

    fn matches(&self, context: &BTreeMap<String, String>) -> bool {
        match self {
            Node::Leaf(leaf) => leaf.matches(context),
            Node::Group { any: true, nodes } => nodes.iter().any(|n| n.matches(context)),
            Node::Group { any: false, nodes } => nodes.iter().all(|n| n.matches(context)),
            Node::Invalid => false,
        }
    }
}

impl Leaf {
    /// One test. An attribute the caller did not send is the empty string,
    /// which is what makes `is_not_set` work and `is` fail closed.
    fn matches(&self, context: &BTreeMap<String, String>) -> bool {
        let actual = context
            .get(&self.attribute)
            .map(String::as_str)
            .unwrap_or("");
        let lower = actual.to_lowercase();
        let wanted = || self.wanted.iter().map(|w| w.to_lowercase());

        match self.op {
            Op::Is => wanted().any(|w| w == lower),
            Op::IsNot => !wanted().any(|w| w == lower),
            Op::Contains => wanted().any(|w| lower.contains(&w)),
            Op::NotContains => !wanted().any(|w| lower.contains(&w)),
            Op::StartsWith => wanted().any(|w| lower.starts_with(&w)),
            Op::EndsWith => wanted().any(|w| lower.ends_with(&w)),
            Op::IsSet => !actual.is_empty(),
            Op::IsNotSet => actual.is_empty(),
            Op::Gt | Op::Lt | Op::Gte | Op::Lte => {
                let (Some(left), Some(right)) = (parse_number(actual), self.number) else {
                    return false;
                };
                match self.op {
                    Op::Gt => left > right,
                    Op::Lt => left < right,
                    Op::Gte => left >= right,
                    _ => left <= right,
                }
            }
        }
    }
}

/// The canonical string form of a context value or a condition value.
///
/// Strings as they are, booleans as `true`/`false`, numbers through
/// [`canonical_number`], `null` as empty; arrays and objects are not
/// attributes and read as empty.
pub fn canonical_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.as_f64().map(canonical_number).unwrap_or_default(),
        Value::Null | Value::Array(_) | Value::Object(_) => String::new(),
    }
}

/// A number the way a person types it: `5` rather than `5.0`, so `2` sent as a
/// JSON float and `2` sent as an integer compare equal in every runtime.
pub fn canonical_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// A decimal number in the one grammar all three runtimes accept identically,
/// `[+-]?(digits[.digits*] | .digits)([eE][+-]?digits)?`, or `None`.
///
/// Checked by hand rather than left to `str::parse`, which also accepts `inf`
/// and `NaN`, words JavaScript's `Number` reads differently.
pub fn parse_number(raw: &str) -> Option<f64> {
    let s = raw.trim();
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        frac_digits = i - frac_start;
    }
    if int_digits == 0 && frac_digits == 0 {
        return None;
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

// ---------------------------------------------------------------------------
// Strict checks, for what is written
// ---------------------------------------------------------------------------

/// Validate a condition document a person is about to save, and return it in
/// group form (`{ logic, filters }`) so every stored flag and target group
/// reads back the same way.
///
/// Refused: anything the lenient reader would treat as malformed, the
/// `matches` operator (see [`Op`]), an attribute name that is not 1 to 64
/// characters of letters, digits, `_`, `.` and `-`, a missing value where the
/// operator needs one, a non-number for an inequality, and a tree deeper than
/// eight levels or larger than sixty-four conditions.
pub fn check_conditions(raw: &Value, field: &str) -> Result<Value> {
    let group = match raw {
        Value::Null => return Ok(json!({ "logic": "and", "filters": [] })),
        Value::Array(items) => json!({ "logic": "and", "filters": items }),
        Value::Object(map) if map.contains_key("dimension") => {
            json!({ "logic": "and", "filters": [raw] })
        }
        Value::Object(_) => raw.clone(),
        _ => {
            return Err(Error::validation(
                field,
                "must be a list of conditions or a { logic, filters } group",
            ))
        }
    };
    let mut leaves = 0usize;
    check_group(&group, field, 0, &mut leaves)?;
    Ok(group)
}

fn check_group(group: &Value, field: &str, depth: usize, leaves: &mut usize) -> Result<()> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(Error::validation(
            field,
            format!("is nested more than {MAX_CONDITION_DEPTH} levels deep"),
        ));
    }
    let Some(map) = group.as_object() else {
        return Err(Error::validation(
            field,
            "every entry must be a condition or a group",
        ));
    };
    for key in map.keys() {
        if key != "logic" && key != "filters" {
            return Err(Error::validation(
                field,
                format!("a group has `logic` and `filters`, not `{key}`"),
            ));
        }
    }
    match map.get("logic") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if s == "and" || s == "or" => {}
        _ => return Err(Error::validation(field, "`logic` must be `and` or `or`")),
    }
    let nodes = match map.get("filters") {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::Array(items)) => items,
        _ => return Err(Error::validation(field, "`filters` must be a list")),
    };
    for node in nodes {
        if node.get("dimension").is_some() {
            *leaves += 1;
            if *leaves > MAX_CONDITION_LEAVES {
                return Err(Error::validation(
                    field,
                    format!("holds more than {MAX_CONDITION_LEAVES} conditions"),
                ));
            }
            check_leaf(node, field)?;
        } else {
            check_group(node, field, depth + 1, leaves)?;
        }
    }
    Ok(())
}

fn check_leaf(leaf: &Value, field: &str) -> Result<()> {
    let attribute = leaf
        .get("dimension")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let usable = (1..=MAX_ATTRIBUTE_LEN).contains(&attribute.len())
        && attribute
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !usable {
        return Err(Error::validation(
            field,
            "an attribute name is 1 to 64 letters, digits, underscores, dots or hyphens",
        ));
    }

    let op_name = leaf.get("op").and_then(Value::as_str).unwrap_or("");
    if op_name == "matches" {
        return Err(Error::validation(
            field,
            "`matches` cannot be used in a flag condition: flags are evaluated by the SDKs and \
             the server alike, and a regular expression would not mean the same thing in each",
        ));
    }
    let Some(op) = Op::parse(op_name) else {
        return Err(Error::validation(
            field,
            format!("`{op_name}` is not an operator a flag condition can use"),
        ));
    };

    if !op.needs_value() {
        return Ok(());
    }
    let value = leaf.get("value").unwrap_or(&Value::Null);
    let filled = match value {
        Value::String(s) => !s.is_empty(),
        Value::Number(_) | Value::Bool(_) => true,
        Value::Array(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|i| matches!(i, Value::String(_) | Value::Number(_) | Value::Bool(_)))
        }
        _ => false,
    };
    if !filled {
        return Err(Error::validation(
            field,
            format!("`{attribute} {op_name}` needs a value"),
        ));
    }
    if op.is_numeric() {
        let numeric = match value {
            Value::Number(_) => true,
            Value::String(s) => parse_number(s).is_some(),
            _ => false,
        };
        if !numeric {
            return Err(Error::validation(
                field,
                format!("`{attribute} {op_name}` needs a number to compare against"),
            ));
        }
    }
    Ok(())
}

/// Validate an environment name: 1 to 32 lower-case letters, digits, `_` or
/// `-`. Returned lower-cased and trimmed.
pub fn check_environment_name(raw: &str) -> Result<String> {
    let name = raw.trim().to_lowercase();
    let usable = (1..=MAX_ENVIRONMENT_LEN).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !usable {
        return Err(Error::validation(
            "environments",
            format!(
                "`{}` is not an environment name; use 1 to 32 letters, digits, hyphens or underscores",
                raw.trim()
            ),
        ));
    }
    Ok(name)
}

/// Validate the per-environment overrides and return them normalised:
/// `{ name: { enabled?, rollout_pct? } }`, names lower-case, rollouts whole
/// percents from 0 to 100, empty overrides dropped.
pub fn check_environments(raw: &Value) -> Result<Value> {
    let map = match raw {
        Value::Null => return Ok(json!({})),
        Value::Object(map) => map,
        _ => {
            return Err(Error::validation(
                "environments",
                "must be an object keyed by environment name",
            ))
        }
    };
    if map.len() > MAX_ENVIRONMENTS {
        return Err(Error::validation(
            "environments",
            format!("a flag can override at most {MAX_ENVIRONMENTS} environments"),
        ));
    }

    let mut out = Map::new();
    for (raw_name, over) in map {
        let name = check_environment_name(raw_name)?;
        let Some(over) = over.as_object() else {
            return Err(Error::validation(
                "environments",
                format!("`{name}` must be an object with `enabled` and `rollout_pct`"),
            ));
        };
        let mut clean = Map::new();
        for (key, value) in over {
            match (key.as_str(), value) {
                (_, Value::Null) => {}
                ("enabled", Value::Bool(b)) => {
                    clean.insert("enabled".into(), json!(b));
                }
                ("rollout_pct", Value::Number(n)) => {
                    let pct = n.as_f64().unwrap_or(-1.0);
                    if !(0.0..=100.0).contains(&pct) || pct.fract() != 0.0 {
                        return Err(Error::validation(
                            "environments",
                            format!("`{name}` rollout must be a whole number from 0 to 100"),
                        ));
                    }
                    clean.insert("rollout_pct".into(), json!(pct as i64));
                }
                _ => {
                    return Err(Error::validation(
                        "environments",
                        format!(
                        "`{name}` can set `enabled` (true or false) and `rollout_pct`, not `{key}`"
                    ),
                    ))
                }
            }
        }
        if out.contains_key(&name) {
            return Err(Error::validation(
                "environments",
                format!("`{name}` appears twice"),
            ));
        }
        if !clean.is_empty() {
            out.insert(name, Value::Object(clean));
        }
    }
    Ok(Value::Object(out))
}

/// Validate a prerequisite list and return it normalised:
/// `[{ flag, enabled, variant }]`. Whether each named flag exists, and whether
/// the graph has a cycle, needs the rest of the site and is the caller's job
/// ([`find_cycle`]).
pub fn check_dependencies(raw: &Value, own_key: &str) -> Result<Value> {
    let list = match raw {
        Value::Null => return Ok(json!([])),
        Value::Array(list) => list,
        _ => return Err(Error::validation("dependencies", "must be a list")),
    };
    if list.len() > MAX_DEPENDENCIES {
        return Err(Error::validation(
            "dependencies",
            format!("a flag can depend on at most {MAX_DEPENDENCIES} others"),
        ));
    }

    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let flag = entry
            .get("flag")
            .and_then(Value::as_str)
            .map(|f| f.trim().to_lowercase())
            .filter(|f| !f.is_empty())
            .ok_or_else(|| Error::validation("dependencies", "every entry needs a `flag` key"))?;
        if flag == own_key {
            return Err(Error::validation(
                "dependencies",
                "a flag cannot depend on itself",
            ));
        }
        if !seen.insert(flag.clone()) {
            return Err(Error::validation(
                "dependencies",
                format!("`{flag}` is listed twice"),
            ));
        }
        let enabled = match entry.get("enabled") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            _ => {
                return Err(Error::validation(
                    "dependencies",
                    "`enabled` must be true or false",
                ))
            }
        };
        let variant = match entry.get("variant") {
            None | Some(Value::Null) => None,
            Some(Value::String(v)) if v.trim().is_empty() => None,
            Some(Value::String(v)) if enabled => Some(v.trim().to_string()),
            Some(Value::String(_)) => {
                return Err(Error::validation(
                    "dependencies",
                    "a variant can only be required of a prerequisite that is on",
                ))
            }
            _ => {
                return Err(Error::validation(
                    "dependencies",
                    "`variant` must be a variant key",
                ))
            }
        };
        out.push(json!({ "flag": flag, "enabled": enabled, "variant": variant }));
    }
    Ok(Value::Array(out))
}

/// The first dependency cycle that passes through `start`, as the path that
/// closes it (`["a", "b", "a"]`), or `None`.
///
/// `edges` maps each flag key to the keys it depends on, with the flag being
/// saved already carrying its proposed prerequisites. Only cycles through the
/// flag being saved can be new, so only those are searched for.
pub fn find_cycle(edges: &BTreeMap<String, Vec<String>>, start: &str) -> Option<Vec<String>> {
    fn walk(
        edges: &BTreeMap<String, Vec<String>>,
        start: &str,
        at: &str,
        path: &mut Vec<String>,
        done: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        for next in edges.get(at).map(Vec::as_slice).unwrap_or(&[]) {
            if next == start {
                let mut cycle = path.clone();
                cycle.push(start.to_string());
                return Some(cycle);
            }
            if done.contains(next) || path.contains(next) {
                continue;
            }
            path.push(next.clone());
            if let Some(cycle) = walk(edges, start, next, path, done) {
                return Some(cycle);
            }
            path.pop();
            done.insert(next.clone());
        }
        None
    }

    let mut path = vec![start.to_string()];
    let mut done = BTreeSet::new();
    walk(edges, start, start, &mut path, &mut done)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectors every implementation is held to. See the module docs.
    const VECTORS: &str = include_str!("../../../../../packages/sdk/test/flag-vectors.json");

    fn vectors() -> Value {
        serde_json::from_str(VECTORS).expect("the vector file is JSON")
    }

    fn input_of(case: &Value) -> EvalInput {
        let empty = Map::new();
        EvalInput::new(
            case["subject"].as_str().unwrap_or(""),
            case["context"].as_object().unwrap_or(&empty),
            case["environment"].as_str(),
        )
    }

    fn ruleset_of<'a>(doc: &'a Value, case: &'a Value) -> &'a Value {
        match &case["ruleset"] {
            Value::String(name) => &doc["rulesets"][name.as_str()],
            inline => inline,
        }
    }

    #[test]
    fn every_bucket_vector_holds() {
        let doc = vectors();
        let list = doc["buckets"].as_array().expect("buckets");
        assert!(list.len() >= 10);
        for case in list {
            let salt = case["salt"].as_str().unwrap();
            let subject = case["subject"].as_str().unwrap();
            assert_eq!(
                bucket(salt, subject),
                case["bucket"].as_u64().unwrap(),
                "bucket({salt:?}, {subject:?})"
            );
        }
    }

    #[test]
    fn every_blake3_vector_holds() {
        let doc = vectors();
        for case in doc["blake3"].as_array().expect("blake3") {
            let input: Vec<u8> = match (&case["text"], case["len"].as_u64()) {
                (Value::String(text), _) => text.as_bytes().to_vec(),
                (_, Some(len)) => (0..len).map(|i| (i % 251) as u8).collect(),
                _ => panic!("a blake3 case needs `text` or `len`"),
            };
            assert_eq!(
                blake3::hash(&input).to_hex().as_str(),
                case["hex"].as_str().unwrap()
            );
        }
    }

    #[test]
    fn every_evaluation_vector_holds() {
        let doc = vectors();
        let cases = doc["cases"].as_array().expect("cases");
        assert!(cases.len() >= 30, "the vectors must cover the whole rule");
        for case in cases {
            let ruleset = Ruleset::from_json(ruleset_of(&doc, case));
            let decided = ruleset.evaluate(case["key"].as_str().unwrap(), &input_of(case));
            assert_eq!(
                decided.to_json(),
                case["expect"],
                "case {}",
                case["name"].as_str().unwrap_or("?")
            );
        }
    }

    /// Recompute every expectation in the vector file from this evaluator.
    ///
    /// Only for when the rule itself changes on purpose: run it with
    /// `MICAFORGE_WRITE_FLAG_VECTORS=1 cargo test -p micaforge-core -- --ignored`,
    /// then read the diff case by case before committing it, because a vector
    /// file regenerated without reading is a test that proves nothing.
    #[test]
    #[ignore = "rewrites packages/sdk/test/flag-vectors.json"]
    fn regenerate_the_vector_file() {
        if std::env::var("MICAFORGE_WRITE_FLAG_VECTORS").as_deref() != Ok("1") {
            return;
        }
        let mut doc = vectors();
        let snapshot = doc.clone();
        if let Some(list) = doc["buckets"].as_array_mut() {
            for case in list {
                let value = bucket(
                    case["salt"].as_str().unwrap(),
                    case["subject"].as_str().unwrap(),
                );
                case["bucket"] = json!(value);
            }
        }
        if let Some(list) = doc["blake3"].as_array_mut() {
            for case in list {
                let input: Vec<u8> = match (&case["text"], case["len"].as_u64()) {
                    (Value::String(text), _) => text.as_bytes().to_vec(),
                    (_, Some(len)) => (0..len).map(|i| (i % 251) as u8).collect(),
                    _ => panic!("a blake3 case needs `text` or `len`"),
                };
                case["hex"] = json!(blake3::hash(&input).to_hex().as_str());
            }
        }
        let count = doc["cases"].as_array().map(Vec::len).unwrap_or(0);
        for i in 0..count {
            let case = doc["cases"][i].clone();
            let ruleset = Ruleset::from_json(ruleset_of(&snapshot, &case));
            let decided = ruleset.evaluate(case["key"].as_str().unwrap(), &input_of(&case));
            doc["cases"][i]["expect"] = decided.to_json();
        }
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../packages/sdk/test/flag-vectors.json"
        );
        let mut text = serde_json::to_string_pretty(&doc).unwrap();
        text.push('\n');
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn bucketing_is_the_visitor_hash_reduced() {
        let a = bucket("new-nav", "visitor-1");
        assert_eq!(a, bucket("new-nav", "visitor-1"));
        assert_ne!(a, bucket("other-flag", "visitor-1"));
        assert!(a < BUCKETS);
        let digest = crate::crypto::visitor_hash(b"new-nav", 0, "visitor-1", "");
        assert_eq!(a, u64::from_str_radix(&digest, 16).unwrap() % BUCKETS);
    }

    #[test]
    fn a_partial_rollout_splits_roughly_where_it_says() {
        let rules = Ruleset::from_json(&json!({
            "flags": [{ "key": "half", "enabled": true, "rollout_pct": 50 }]
        }));
        let on = (0..2_000)
            .filter(|i| {
                let input = EvalInput {
                    subject: format!("visitor-{i}"),
                    ..EvalInput::default()
                };
                rules.evaluate("half", &input).enabled
            })
            .count();
        assert!(
            (900..1_100).contains(&on),
            "50% rollout reached {on} of 2000"
        );
    }

    #[test]
    fn variants_split_by_weight_and_never_pick_a_zero_weight() {
        let even = vec![("control".to_string(), 50), ("treatment".to_string(), 50)];
        let treatment = (0..2_000)
            .filter(|i| {
                pick_variant(&even, "exp", &format!("visitor-{i}")).as_deref() == Some("treatment")
            })
            .count();
        assert!(
            (900..1_100).contains(&treatment),
            "50/50 split gave {treatment}"
        );

        let skewed = vec![("on".to_string(), 1), ("never".to_string(), 0)];
        for i in 0..200 {
            assert_eq!(
                pick_variant(&skewed, "exp", &format!("v{i}")).as_deref(),
                Some("on")
            );
        }
    }

    #[test]
    fn a_runtime_cycle_is_off_rather_than_endless() {
        // Validation refuses this on save; a hand-edited row must still not
        // recurse without end.
        let rules = Ruleset::from_json(&json!({ "flags": [
            { "key": "a", "enabled": true, "dependencies": [{ "flag": "b" }] },
            { "key": "b", "enabled": true, "dependencies": [{ "flag": "a" }] }
        ]}));
        let decided = rules.evaluate("a", &EvalInput::default());
        assert_eq!(decided.reason, Reason::Dependency);
    }

    #[test]
    fn cycles_are_found_through_the_flag_being_saved_only() {
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        edges.insert("a".into(), vec!["b".into()]);
        edges.insert("b".into(), vec!["c".into()]);
        edges.insert("c".into(), vec!["a".into()]);
        assert_eq!(
            find_cycle(&edges, "a"),
            Some(vec!["a".into(), "b".into(), "c".into(), "a".into()])
        );

        // A diamond is not a cycle.
        let mut diamond: BTreeMap<String, Vec<String>> = BTreeMap::new();
        diamond.insert("top".into(), vec!["left".into(), "right".into()]);
        diamond.insert("left".into(), vec!["base".into()]);
        diamond.insert("right".into(), vec!["base".into()]);
        assert_eq!(find_cycle(&diamond, "top"), None);

        // A cycle elsewhere in the site is not this save's to report.
        let mut elsewhere: BTreeMap<String, Vec<String>> = BTreeMap::new();
        elsewhere.insert("new".into(), vec!["x".into()]);
        elsewhere.insert("x".into(), vec!["y".into()]);
        elsewhere.insert("y".into(), vec!["x".into()]);
        assert_eq!(find_cycle(&elsewhere, "new"), None);

        // Two flags pointing at each other.
        let mut pair: BTreeMap<String, Vec<String>> = BTreeMap::new();
        pair.insert("a".into(), vec!["b".into()]);
        pair.insert("b".into(), vec!["a".into()]);
        assert_eq!(
            find_cycle(&pair, "b"),
            Some(vec!["b".into(), "a".into(), "b".into()])
        );
    }

    #[test]
    fn conditions_are_checked_strictly_on_the_way_in() {
        assert!(check_conditions(&json!([]), "conditions").is_ok());
        assert_eq!(
            check_conditions(
                &json!([{ "dimension": "plan", "op": "is", "value": "pro" }]),
                "c"
            )
            .unwrap(),
            json!({ "logic": "and", "filters": [{ "dimension": "plan", "op": "is", "value": "pro" }] })
        );
        let refused = [
            json!([{ "dimension": "path", "op": "matches", "value": "^/docs" }]),
            json!([{ "dimension": "plan", "op": "equals", "value": "pro" }]),
            json!([{ "dimension": "plan", "op": "is" }]),
            json!([{ "dimension": "plan", "op": "is", "value": "" }]),
            json!([{ "dimension": "age", "op": "gt", "value": "old" }]),
            json!([{ "dimension": "has space", "op": "is_set" }]),
            json!({ "logic": "xor", "filters": [] }),
            json!({ "logic": "and", "filters": "no" }),
            json!("plan is pro"),
        ];
        for raw in refused {
            assert!(check_conditions(&raw, "conditions").is_err(), "{raw}");
        }
    }

    #[test]
    fn environments_and_dependencies_are_normalised() {
        let envs = check_environments(&json!({
            "Staging": { "enabled": true, "rollout_pct": 100 },
            "development": {}
        }))
        .unwrap();
        assert_eq!(
            envs,
            json!({ "staging": { "enabled": true, "rollout_pct": 100 } })
        );
        assert!(check_environments(&json!({ "prod uction": {} })).is_err());
        assert!(check_environments(&json!({ "staging": { "rollout_pct": 101 } })).is_err());
        assert!(check_environments(&json!({ "staging": { "variants": [] } })).is_err());

        let deps = check_dependencies(&json!([{ "flag": "Base" }]), "child").unwrap();
        assert_eq!(
            deps,
            json!([{ "flag": "base", "enabled": true, "variant": null }])
        );
        assert!(check_dependencies(&json!([{ "flag": "child" }]), "child").is_err());
        assert!(check_dependencies(&json!([{ "flag": "a" }, { "flag": "a" }]), "c").is_err());
        assert!(check_dependencies(
            &json!([{ "flag": "a", "enabled": false, "variant": "x" }]),
            "c"
        )
        .is_err());
    }

    #[test]
    fn numbers_read_the_same_in_every_runtime() {
        assert_eq!(parse_number(" 2.5 "), Some(2.5));
        assert_eq!(parse_number("+3"), Some(3.0));
        assert_eq!(parse_number(".5"), Some(0.5));
        assert_eq!(parse_number("5."), Some(5.0));
        assert_eq!(parse_number("1e3"), Some(1000.0));
        for refused in ["", "inf", "NaN", "1_000", "0x10", "1e", ".", "--1", "1e999"] {
            assert_eq!(parse_number(refused), None, "{refused}");
        }
        assert_eq!(canonical_number(5.0), "5");
        assert_eq!(canonical_number(-0.0), "0");
        assert_eq!(canonical_number(2.5), "2.5");
    }
}
