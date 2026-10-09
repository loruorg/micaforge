import Foundation

/// Feature-flag evaluation: the shared rule, in Swift.
///
/// # Why there is exactly one rule
///
/// A flag is decided in several places. The server decides it (`GET /api/flags/evaluate`),
/// the Node SDK decides it from a cached ruleset, the browser SDK decides it from a ruleset
/// or a fallback rule while the network is in flight, and this SDK does the same in an app.
/// If any two of those disagree for the same user, a person sees a feature appear and
/// vanish between launches, and an experiment's exposure counts stop meaning anything. So
/// the rule is written down once, in the Rust server
/// (`apps/backend/crates/core/src/flags.rs`), and this file is its Swift copy, held to it
/// by the shared vectors in `packages/sdk/test/flag-vectors.json` that the Rust, browser,
/// Node and Swift test suites all assert, case by case.
///
/// # The decision, in order
///
/// 1. An unknown key is off (`unknown`).
/// 2. An environment override replaces `enabled` and `rollout_pct` when the caller names
///    an environment the flag configures.
/// 3. A flag that is not enabled is off (`disabled`), whatever else it says.
/// 4. Every prerequisite flag must evaluate to the state it names (`dependency`). A cycle,
///    or a chain deeper than eight flags, is off rather than a stack overflow.
/// 5. With target groups attached, the user must match at least one (`target_group`).
/// 6. The flag's own conditions must match (`conditions`).
/// 7. The rollout: a subject whose bucket falls outside the percentage is off (`rollout`),
///    and with no subject at all a partial rollout cannot hold anybody in or out, so it is
///    off too (`no_visitor`).
/// 8. Otherwise on (`match`), with a variant picked by weight.
///
/// # Lenient reading
///
/// A ruleset arrives from a network, a cache or an app bundle, so it is read leniently:
/// anything malformed evaluates off instead of throwing, which is the safe direction for a
/// flag. The reading rules are the server's, one for one: `enabled` must be the boolean
/// `true`; a missing or `null` rollout is 100, a number is rounded to a whole percent,
/// anything else is 0; a variant weight that is missing or `null` is 1, a whole
/// non-negative number is itself, anything else is 0; a prerequisite is wanted on unless
/// its `enabled` is exactly `false`, and one without a usable `flag` never holds; a
/// malformed condition, group or leaf is false; an empty group at the top of a condition
/// document matches everybody, while an empty `or` group inside one is false.
///
/// # Where Swift's defaults differ from Rust's, and what this file does instead
///
/// - **String equality.** Swift compares strings by canonical equivalence, so a precomposed
///   `é` equals `e` followed by a combining accent. Rust compares bytes. Every comparison
///   here (equality, prefix, suffix, containment, map keys) is done on UTF-8 bytes.
/// - **Trimming.** Rust's `str::trim` strips the Unicode `White_Space` set, which
///   ``trim(_:)`` reproduces scalar by scalar rather than trusting Foundation's notion of
///   whitespace.
/// - **Duplicate keys after normalising.** The server reads a JSON object into a map sorted
///   by key, then trims and lower-cases each key, so when `Plan` and `plan` collide the
///   later one in sorted order wins. Keys are sorted by code point here first.
/// - **Numbers as text.** Rust prints a float without an exponent; Swift's `description`
///   uses one past 2^53 and below 1e-4, so ``canonicalNumber(_:)`` expands it.
public enum FlagEvaluator {
    /// Buckets a subject can fall into: a rollout percentage is compared at a hundredth of
    /// a percent.
    static let buckets: UInt64 = 10_000

    /// The deepest chain of prerequisites one evaluation follows. Far past any honest use;
    /// it stops a cycle that slipped past validation from recursing without end.
    static let maxDependencyDepth = 8

    /// The deepest condition nesting read; anything deeper is malformed (false). The
    /// server's JSON reader refuses documents deeper than this could produce, so the bound
    /// never changes an answer for a ruleset the server could have stored.
    static let maxNesting = 100

    /// The largest whole number a weight may be: beyond it a double no longer holds every
    /// integer.
    static let maxWeight: Double = 9_007_199_254_740_991

    static let operators: Set<String> = [
        "is", "is_not", "contains", "not_contains", "starts_with", "ends_with",
        "is_set", "is_not_set", "gt", "lt", "gte", "lte",
    ]

    // MARK: - Public API

    /// Which of 10 000 buckets a subject falls into for one salt.
    ///
    /// The bytes hashed are `UTF-8(salt) ‖ 1f 00 00 1f ‖ UTF-8(subject) ‖ 1f`, the server's
    /// cookieless visitor hash with site id zero and an empty user agent. The first eight
    /// digest bytes, read big-endian, are reduced modulo 10 000. Rollouts salt with the
    /// flag key and variant picks with `"<key>:variant"`, so falling inside a narrow
    /// rollout does not bias which variant somebody lands in.
    public static func bucket(salt: String, subject: String) -> Int {
        var bytes = Array(salt.utf8)
        bytes.append(contentsOf: [0x1f, 0, 0, 0x1f])
        bytes.append(contentsOf: Array(subject.utf8))
        bytes.append(0x1f)
        let d = BLAKE3.hash(bytes)
        var value: UInt64 = 0
        for i in 0..<8 { value = (value << 8) | UInt64(d[i]) }
        return Int(value % buckets)
    }

    /// Read a ruleset once, to evaluate it many times.
    ///
    /// Never throws. A flag with no usable key is skipped, and the first of two flags with
    /// one key wins. Pass the `ruleset` member of `GET /api/flags/ruleset`, or the same
    /// document from your own server or your app bundle.
    public static func compile(_ ruleset: JSONValue) -> CompiledRuleset {
        CompiledRuleset(ruleset)
    }

    /// Decide one flag. Reads the whole ruleset on every call; hold a ``CompiledRuleset``
    /// instead when deciding more than once.
    public static func evaluate(_ ruleset: JSONValue, key: String, input: FlagInput) -> FlagDecision {
        CompiledRuleset(ruleset).evaluate(key, input: input)
    }

    // MARK: - Text, the server's way

    /// Rust's `char::is_whitespace`: the Unicode `White_Space` property.
    static func isSpace(_ c: UInt32) -> Bool {
        (c >= 9 && c <= 13) || c == 32 || c == 0x85 || c == 0xa0 || c == 0x1680
            || (c >= 0x2000 && c <= 0x200a) || c == 0x2028 || c == 0x2029 || c == 0x202f
            || c == 0x205f || c == 0x3000
    }

    /// Rust's `str::trim`.
    static func trim(_ s: String) -> String {
        let scalars = Array(s.unicodeScalars)
        var a = 0
        var b = scalars.count
        while a < b && isSpace(scalars[a].value) { a += 1 }
        while b > a && isSpace(scalars[b - 1].value) { b -= 1 }
        if a == 0 && b == scalars.count { return s }
        var out = String.UnicodeScalarView()
        out.append(contentsOf: scalars[a..<b])
        return String(out)
    }

    /// Trimmed and lower-cased: how attribute names, context keys and environment names
    /// compare.
    static func norm(_ s: String) -> String { trim(s).lowercased() }

    /// Code point order, which is also UTF-8 byte order: the order Rust's `String` keys
    /// sort in. Swift's own `<` on strings is a Unicode collation order instead.
    static func byCodePoint(_ a: String, _ b: String) -> Bool {
        a.utf8.lexicographicallyPrecedes(b.utf8)
    }

    /// A number the way a person types it, and the way the server prints it: whole numbers
    /// below 1e15 as integer digits (`5`, not `5.0`; `-0` as `0`), anything else as the
    /// shortest decimal that reads back to the same double, written out without an
    /// exponent.
    public static func canonicalNumber(_ n: Double) -> String {
        if n.isNaN { return "NaN" }
        if n == .infinity { return "inf" }
        if n == -.infinity { return "-inf" }
        if n == n.rounded(.towardZero) && abs(n) < 1e15 { return String(Int64(n)) }
        var s = "\(n)"
        guard let e = s.firstIndex(where: { $0 == "e" || $0 == "E" }) else {
            if s.hasSuffix(".0") { s.removeLast(2) }
            return s
        }
        var mantissa = String(s[s.startIndex..<e])
        let exponent = Int(s[s.index(after: e)...]) ?? 0
        var sign = ""
        if mantissa.hasPrefix("-") {
            sign = "-"
            mantissa.removeFirst()
        }
        let dot = mantissa.firstIndex(of: ".").map { mantissa.distance(from: mantissa.startIndex, to: $0) }
        var digits = mantissa.replacingOccurrences(of: ".", with: "")
        // `1.0e+16` carries a zero that is not a significant digit.
        if dot != nil, digits.count > 1, digits.hasSuffix("0") {
            while digits.count > 1 && digits.hasSuffix("0") { digits.removeLast() }
        }
        let point = (dot ?? mantissa.count) + exponent
        if point <= 0 { return sign + "0." + String(repeating: "0", count: -point) + digits }
        if point >= digits.count { return sign + digits + String(repeating: "0", count: point - digits.count) }
        let cut = digits.index(digits.startIndex, offsetBy: point)
        return sign + digits[..<cut] + "." + digits[cut...]
    }

    /// The canonical string of a context or condition value: a string as it is, a boolean
    /// as `true`/`false`, a number through ``canonicalNumber(_:)``, and `""` for anything
    /// else.
    static func canonicalValue(_ v: JSONValue?) -> String {
        switch v {
        case .string(let s)?: return s
        case .bool(let b)?: return b ? "true" : "false"
        case .number(let n)?: return canonicalNumber(n)
        default: return ""
        }
    }

    /// A decimal number in the one grammar every runtime reads identically,
    /// `[+-]?(digits[.digits*] | .digits)([eE][+-]?digits)?` after trimming, or `nil`.
    ///
    /// Checked against the grammar by hand rather than handed to `Double(_:)`, which also
    /// reads `inf`, `nan` and hexadecimal, none of which the server accepts. A value too
    /// large for a double is not a number either.
    public static func parseNumber(_ raw: String) -> Double? {
        let s = trim(raw)
        let b = Array(s.utf8)
        var i = 0
        func digits() -> Int {
            let start = i
            while i < b.count && b[i] >= 0x30 && b[i] <= 0x39 { i += 1 }
            return i - start
        }
        if i < b.count && (b[i] == 0x2b || b[i] == 0x2d) { i += 1 }
        let whole = digits()
        if whole > 0 {
            if i < b.count && b[i] == 0x2e {
                i += 1
                _ = digits()
            }
        } else {
            guard i < b.count && b[i] == 0x2e else { return nil }
            i += 1
            guard digits() > 0 else { return nil }
        }
        if i < b.count && (b[i] == 0x65 || b[i] == 0x45) {
            i += 1
            if i < b.count && (b[i] == 0x2b || b[i] == 0x2d) { i += 1 }
            guard digits() > 0 else { return nil }
        }
        guard i == b.count, let n = Double(s), n.isFinite else { return nil }
        return n
    }

    /// A rollout rounded to a whole percent, half away from zero as the server rounds,
    /// saturating just outside 0-100 (it is clamped to 0-100 at decision time).
    static func roundRollout(_ v: Double) -> Int {
        if v.isNaN { return 0 }
        return Int(min(101, max(-1, v.rounded())))
    }

    /// A flag's rollout: missing or `null` is everyone, a number is rounded, anything else
    /// is nobody.
    static func rolloutOf(_ v: JSONValue?) -> Int {
        switch v {
        case nil, .null?: return 100
        case .number(let n)?: return roundRollout(n)
        default: return 0
        }
    }

    /// A variant weight: missing or `null` is 1, a whole non-negative number is itself,
    /// else 0 (never picked).
    static func weightOf(_ v: JSONValue?) -> Double {
        switch v {
        case nil, .null?: return 1
        case .number(let n)? where n.isFinite && n == n.rounded(.towardZero) && n >= 0 && n <= maxWeight:
            return n
        default: return 0
        }
    }
}

/// Why a flag evaluated the way it did. The spelling is the contract with the server and
/// the stats table.
public enum FlagReason: String, Sendable, Codable, Equatable {
    case unknown
    case disabled
    case dependency
    case targetGroup = "target_group"
    case conditions
    case noVisitor = "no_visitor"
    case rollout
    case match
}

/// One flag, decided for one subject.
public struct FlagDecision: Sendable, Equatable {
    /// Whether the subject gets the flag.
    public var enabled: Bool
    /// The variant they get; `nil` for an on/off flag and for every off decision.
    public var variant: String?
    /// Why.
    public var reason: FlagReason

    public init(enabled: Bool, variant: String?, reason: FlagReason) {
        self.enabled = enabled
        self.variant = variant
        self.reason = reason
    }

    static func off(_ reason: FlagReason) -> FlagDecision {
        FlagDecision(enabled: false, variant: nil, reason: reason)
    }
}

/// Who is asking.
public struct FlagInput: Sendable {
    /// The bucketing subject: the identified id when there is one, else a stable anonymous
    /// id; `""` for none.
    public var subject: String
    /// Attributes the conditions test. Strings, numbers and booleans count; anything else
    /// reads as unset.
    public var context: [String: JSONValue]
    /// The environment the caller runs in, such as `production`; `nil` or empty for none.
    public var environment: String?

    public init(subject: String, context: [String: JSONValue] = [:], environment: String? = nil) {
        self.subject = subject
        self.context = context
        self.environment = environment
    }
}

/// A string compared by its UTF-8 bytes, the way the server compares map keys, rather than
/// by Swift's canonical equivalence.
struct ExactKey: Hashable, Sendable {
    let bytes: [UInt8]
    init(_ s: String) { bytes = Array(s.utf8) }
}

/// A ruleset read once, to be evaluated many times.
///
/// Evaluating reads nothing from the original document again, so it is a value: copy it
/// across threads freely.
public struct CompiledRuleset: Sendable {
    // MARK: Shapes

    struct Leaf: Sendable {
        var attr: ExactKey
        var op: String
        /// Canonical and lower-cased, as UTF-8.
        var wanted: [[UInt8]]
        /// The value as a number, for the inequalities.
        var number: Double?
    }

    indirect enum Node: Sendable {
        case leaf(Leaf)
        case group(anyOf: Bool, nodes: [Node])
        case invalid
    }

    struct Variant: Sendable {
        var key: String
        var weight: Double
    }

    struct Dependency: Sendable {
        var flag: String
        var enabled: Bool
        var variant: String?
    }

    struct Override: Sendable {
        var enabled: Bool?
        var rollout: Int?
    }

    struct Rule: Sendable {
        var enabled: Bool
        var rollout: Int
        var variants: [Variant]
        var conditions: Node
        var groups: [String]
        /// `nil` for an entry with no usable flag key: it can never hold.
        var deps: [Dependency?]
        var envs: [ExactKey: Override]
    }

    /// The input as every implementation normalises it: context keys trimmed and
    /// lower-cased.
    struct Input {
        var subject: String
        var context: [ExactKey: String]
        var environment: String
    }

    /// Every flag key, in ruleset order (the first of two flags with one key wins).
    public let keys: [String]
    let flags: [ExactKey: Rule]
    let groups: [ExactKey: Node]

    init(_ ruleset: JSONValue) {
        var flags: [ExactKey: Rule] = [:]
        var keys: [String] = []
        if case .array(let list)? = ruleset["flags"] {
            for raw in list {
                guard case .string(let key)? = raw["key"], !key.isEmpty else { continue }
                let exact = ExactKey(key)
                if flags[exact] != nil { continue }
                flags[exact] = Self.readRule(raw)
                keys.append(key)
            }
        }
        var groups: [ExactKey: Node] = [:]
        if case .object(let raw)? = ruleset["target_groups"] {
            for (id, group) in raw { groups[ExactKey(id)] = Self.parseTop(group["conditions"]) }
        }
        self.keys = keys
        self.flags = flags
        self.groups = groups
    }

    /// Decide one flag.
    public func evaluate(_ key: String, input: FlagInput) -> FlagDecision {
        var stack: [ExactKey] = []
        return decide(ExactKey(key), Self.readInput(input), &stack)
    }

    /// Decide every flag, keyed by flag key.
    public func evaluateAll(_ input: FlagInput) -> [String: FlagDecision] {
        let normalised = Self.readInput(input)
        var out: [String: FlagDecision] = [:]
        for key in keys {
            var stack: [ExactKey] = []
            out[key] = decide(ExactKey(key), normalised, &stack)
        }
        return out
    }

    // MARK: Reading

    /// The top of a condition document: missing or `null` is the empty group, a list is an
    /// `and` of its nodes, an object with `dimension` is one leaf, any other object is a
    /// group, and anything else is malformed.
    static func parseTop(_ raw: JSONValue?) -> Node {
        switch raw {
        case nil, .null?:
            return .group(anyOf: false, nodes: [])
        case .array(let items)?:
            return .group(anyOf: false, nodes: items.map { parseNode($0, 1) })
        case .object(let o)?:
            return o["dimension"] != nil ? .group(anyOf: false, nodes: [parseNode(.object(o), 1)]) : parseNode(.object(o), 0)
        default:
            return .invalid
        }
    }

    /// One node: a leaf when it has `dimension`, a group otherwise.
    static func parseNode(_ raw: JSONValue, _ depth: Int) -> Node {
        guard case .object(let o) = raw, depth <= FlagEvaluator.maxNesting else { return .invalid }
        if o["dimension"] != nil { return parseLeaf(o) }
        var anyOf = false
        switch o["logic"] {
        case .string("or")?: anyOf = true
        case nil, .null?, .string("and")?: break
        default: return .invalid
        }
        switch o["filters"] {
        case nil, .null?: return .group(anyOf: anyOf, nodes: [])
        case .array(let items)?: return .group(anyOf: anyOf, nodes: items.map { parseNode($0, depth + 1) })
        default: return .invalid
        }
    }

    static func parseLeaf(_ raw: [String: JSONValue]) -> Node {
        let attr: String
        if case .string(let d)? = raw["dimension"] { attr = FlagEvaluator.norm(d) } else { attr = "" }
        guard !attr.isEmpty, case .string(let op)? = raw["op"], FlagEvaluator.operators.contains(op) else {
            return .invalid
        }
        let value = raw["value"]
        let wanted: [String]
        switch value {
        case nil, .null?:
            wanted = [""]
        case .array(let items)?:
            wanted = items.compactMap { item -> String? in
                switch item {
                case .string, .number, .bool: return FlagEvaluator.canonicalValue(item)
                default: return nil
                }
            }
        default:
            wanted = [FlagEvaluator.canonicalValue(value)]
        }
        var number: Double?
        switch value {
        case .number(let n)?: number = n.isFinite ? n : nil
        case .string(let s)?: number = FlagEvaluator.parseNumber(s)
        default: number = nil
        }
        return .leaf(Leaf(attr: ExactKey(attr), op: op, wanted: wanted.map { Array($0.lowercased().utf8) }, number: number))
    }

    static func readDependency(_ raw: JSONValue) -> Dependency? {
        guard case .string(let flag)? = raw["flag"], !flag.isEmpty else { return nil }
        var variant: String?
        if case .string(let v)? = raw["variant"], !v.isEmpty { variant = v }
        return Dependency(flag: flag, enabled: raw["enabled"] != .bool(false), variant: variant)
    }

    static func readRule(_ raw: JSONValue) -> Rule {
        var variants: [Variant] = []
        if case .array(let list)? = raw["variants"] {
            for v in list {
                if case .string(let key)? = v["key"], !key.isEmpty {
                    variants.append(Variant(key: key, weight: FlagEvaluator.weightOf(v["weight"])))
                }
            }
        }
        var groups: [String] = []
        if case .array(let list)? = raw["target_groups"] {
            for id in list { if case .string(let s) = id { groups.append(s) } }
        }
        var deps: [Dependency?] = []
        if case .array(let list)? = raw["dependencies"] {
            for d in list { deps.append(readDependency(d)) }
        }
        var envs: [ExactKey: Override] = [:]
        if case .object(let raw)? = raw["environments"] {
            for name in raw.keys.sorted(by: FlagEvaluator.byCodePoint) {
                let entry = raw[name]
                var enabled: Bool?
                if case .bool(let b)? = entry?["enabled"] { enabled = b }
                var rollout: Int?
                if case .number(let n)? = entry?["rollout_pct"] { rollout = FlagEvaluator.roundRollout(n) }
                envs[ExactKey(FlagEvaluator.norm(name))] = Override(enabled: enabled, rollout: rollout)
            }
        }
        return Rule(
            enabled: raw["enabled"] == .bool(true),
            rollout: FlagEvaluator.rolloutOf(raw["rollout_pct"]),
            variants: variants,
            conditions: parseTop(raw["conditions"]),
            groups: groups,
            deps: deps,
            envs: envs
        )
    }

    static func readInput(_ input: FlagInput) -> Input {
        var context: [ExactKey: String] = [:]
        for key in input.context.keys.sorted(by: FlagEvaluator.byCodePoint) {
            context[ExactKey(FlagEvaluator.norm(key))] = FlagEvaluator.canonicalValue(input.context[key])
        }
        return Input(
            subject: input.subject,
            context: context,
            environment: input.environment.map(FlagEvaluator.norm) ?? ""
        )
    }

    // MARK: Matching

    /// One test. An attribute the caller did not send is the empty string, which is what
    /// makes `is_not_set` work and `is` fail closed. Text compares in full Unicode lower
    /// case on both sides.
    static func matchLeaf(_ leaf: Leaf, _ context: [ExactKey: String]) -> Bool {
        let actual = context[leaf.attr] ?? ""
        let lower = Array(actual.lowercased().utf8)
        let wanted = leaf.wanted
        switch leaf.op {
        case "is": return wanted.contains { $0 == lower }
        case "is_not": return !wanted.contains { $0 == lower }
        case "contains": return wanted.contains { contains(lower, $0) }
        case "not_contains": return !wanted.contains { contains(lower, $0) }
        case "starts_with": return wanted.contains { lower.starts(with: $0) }
        case "ends_with": return wanted.contains { lower.count >= $0.count && Array(lower.suffix($0.count)) == $0 }
        case "is_set": return !actual.isEmpty
        case "is_not_set": return actual.isEmpty
        default: break
        }
        guard let left = FlagEvaluator.parseNumber(actual), let right = leaf.number else { return false }
        switch leaf.op {
        case "gt": return left > right
        case "lt": return left < right
        case "gte": return left >= right
        default: return left <= right
        }
    }

    /// Byte-wise containment; the empty needle is in every haystack, as in every runtime.
    static func contains(_ haystack: [UInt8], _ needle: [UInt8]) -> Bool {
        if needle.isEmpty { return true }
        if needle.count > haystack.count { return false }
        var i = 0
        while i + needle.count <= haystack.count {
            if haystack[i] == needle[0] && Array(haystack[i..<(i + needle.count)]) == needle { return true }
            i += 1
        }
        return false
    }

    static func matches(_ node: Node, _ context: [ExactKey: String]) -> Bool {
        switch node {
        case .leaf(let leaf): return matchLeaf(leaf, context)
        case .group(let anyOf, let nodes): return anyOf ? nodes.contains { matches($0, context) } : nodes.allSatisfy { matches($0, context) }
        case .invalid: return false
        }
    }

    /// The top-level rule: a group with nothing in it constrains nothing, whatever its
    /// logic says.
    static func matchesTop(_ node: Node, _ context: [ExactKey: String]) -> Bool {
        if case .group(_, let nodes) = node, nodes.isEmpty { return true }
        return matches(node, context)
    }

    // MARK: Deciding

    /// Choose a variant by weight, or `nil` for an on/off flag.
    ///
    /// `point = floor(bucket * total / 10000)`, computed as
    /// `bucket * q + floor(bucket * r / 10000)` with `total = q * 10000 + r`, in doubles,
    /// exactly as the browser computes it, so no product passes 2^53.
    func pickVariant(_ variants: [Variant], _ key: String, _ subject: String) -> String? {
        guard let first = variants.first else { return nil }
        var total = 0.0
        for v in variants { total += v.weight }
        if total == 0 { return first.key }
        let bucket = Double(FlagEvaluator.bucket(salt: key + ":variant", subject: subject))
        let size = Double(FlagEvaluator.buckets)
        let r = total.truncatingRemainder(dividingBy: size)
        let point = bucket * ((total - r) / size) + ((bucket * r) / size).rounded(.down)
        var seen = 0.0
        for v in variants {
            seen += v.weight
            if point < seen { return v.key }
        }
        return variants[variants.count - 1].key
    }

    func decide(_ key: ExactKey, _ input: Input, _ stack: inout [ExactKey]) -> FlagDecision {
        guard let rule = flags[key] else { return .off(.unknown) }

        var enabled = rule.enabled
        var rollout = rule.rollout
        if !input.environment.isEmpty, let over = rule.envs[ExactKey(input.environment)] {
            if let e = over.enabled { enabled = e }
            if let r = over.rollout { rollout = r }
        }

        if !enabled { return .off(.disabled) }

        stack.append(key)
        var held = true
        for dep in rule.deps where !holds(dep, input, &stack) {
            held = false
            break
        }
        stack.removeLast()
        if !held { return .off(.dependency) }

        if !rule.groups.isEmpty {
            let matched = rule.groups.contains { id in
                guard let group = groups[ExactKey(id)] else { return false }
                return Self.matchesTop(group, input.context)
            }
            if !matched { return .off(.targetGroup) }
        }

        if !Self.matchesTop(rule.conditions, input.context) { return .off(.conditions) }

        let pct = min(100, max(0, rollout))
        if pct < 100 {
            if input.subject.isEmpty { return .off(.noVisitor) }
            let keyText = String(decoding: key.bytes, as: UTF8.self)
            if FlagEvaluator.bucket(salt: keyText, subject: input.subject) >= pct * Int(FlagEvaluator.buckets / 100) {
                return .off(.rollout)
            }
        }

        let keyText = String(decoding: key.bytes, as: UTF8.self)
        return FlagDecision(enabled: true, variant: pickVariant(rule.variants, keyText, input.subject), reason: .match)
    }

    /// Does one prerequisite hold? A malformed one, a cycle, or a chain past the depth
    /// limit never does.
    func holds(_ dep: Dependency?, _ input: Input, _ stack: inout [ExactKey]) -> Bool {
        guard let dep else { return false }
        let target = ExactKey(dep.flag)
        if stack.contains(target) || stack.count >= FlagEvaluator.maxDependencyDepth { return false }
        let decided = decide(target, input, &stack)
        if decided.enabled != dep.enabled { return false }
        return dep.variant == nil || decided.variant == dep.variant
    }
}
