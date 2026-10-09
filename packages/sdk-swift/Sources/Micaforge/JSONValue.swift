import Foundation

/// A JSON value: what an event's properties, a profile's traits, a flag's payload and a
/// flag ruleset are made of.
///
/// # Why one enum rather than `[String: Any]`
///
/// Properties cross three boundaries in this SDK: they are queued in an actor, written to
/// disk while the device is offline, and encoded onto the wire. `Any` is not `Sendable`,
/// cannot be encoded without a cast at every step, and cannot tell a boolean from the
/// number one once Foundation has boxed both into an `NSNumber`. The flag evaluator cares
/// about that difference (`true` and `1` are different condition values on the server),
/// so the type keeps it.
///
/// Literals work, so a call site reads like the JSON it sends:
///
/// ```swift
/// Micaforge.track("signup", props: ["plan": "pro", "seats": 5, "trial": true])
/// ```
///
/// Numbers are held as `Double`, which is what every JSON parser in the three runtimes
/// that decide flags reads them as. An integer above 2^53 loses precision, exactly as it
/// does in the browser.
public enum JSONValue: Sendable, Equatable, Hashable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    /// The string, when this is one.
    public var stringValue: String? {
        if case .string(let s) = self { return s }
        return nil
    }

    /// The number, when this is one.
    public var numberValue: Double? {
        if case .number(let n) = self { return n }
        return nil
    }

    /// The boolean, when this is one. A number is never read as a boolean.
    public var boolValue: Bool? {
        if case .bool(let b) = self { return b }
        return nil
    }

    /// The members, when this is an object.
    public var objectValue: [String: JSONValue]? {
        if case .object(let o) = self { return o }
        return nil
    }

    /// The elements, when this is an array.
    public var arrayValue: [JSONValue]? {
        if case .array(let a) = self { return a }
        return nil
    }

    /// A member of an object, read the way `serde_json`'s `Value::get` reads it: `nil` for
    /// a missing member and for anything that is not an object.
    public subscript(key: String) -> JSONValue? {
        if case .object(let o) = self { return o[key] }
        return nil
    }

    /// Parse JSON text. `nil` for anything that is not a JSON document.
    public static func parse(_ data: Data) -> JSONValue? {
        try? JSONDecoder().decode(JSONValue.self, from: data)
    }

    /// The value as compact JSON text, with object keys sorted so equal values always
    /// print the same text.
    public func jsonString() -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(self) else { return "null" }
        return String(decoding: data, as: UTF8.self)
    }
}

extension JSONValue: Codable {
    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        // The order matters: Foundation's decoder refuses to read `true` as a number and
        // `1` as a boolean, so trying the boolean first keeps the two apart.
        if c.decodeNil() {
            self = .null
        } else if let b = try? c.decode(Bool.self) {
            self = .bool(b)
        } else if let n = try? c.decode(Double.self) {
            self = .number(n)
        } else if let s = try? c.decode(String.self) {
            self = .string(s)
        } else if let a = try? c.decode([JSONValue].self) {
            self = .array(a)
        } else if let o = try? c.decode([String: JSONValue].self) {
            self = .object(o)
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "not a JSON value")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case .bool(let b): try c.encode(b)
        // JSON has no NaN or infinity; a property that held one is sent as null rather
        // than failing the whole batch it rides in.
        case .number(let n): if n.isFinite { try c.encode(n) } else { try c.encodeNil() }
        case .string(let s): try c.encode(s)
        case .array(let a): try c.encode(a)
        case .object(let o): try c.encode(o)
        }
    }
}

extension JSONValue: ExpressibleByNilLiteral {
    public init(nilLiteral: ()) { self = .null }
}

extension JSONValue: ExpressibleByBooleanLiteral {
    public init(booleanLiteral value: Bool) { self = .bool(value) }
}

extension JSONValue: ExpressibleByIntegerLiteral {
    public init(integerLiteral value: Int) { self = .number(Double(value)) }
}

extension JSONValue: ExpressibleByFloatLiteral {
    public init(floatLiteral value: Double) { self = .number(value) }
}

extension JSONValue: ExpressibleByStringLiteral {
    public init(stringLiteral value: String) { self = .string(value) }
}

extension JSONValue: ExpressibleByArrayLiteral {
    public init(arrayLiteral elements: JSONValue...) { self = .array(elements) }
}

extension JSONValue: ExpressibleByDictionaryLiteral {
    public init(dictionaryLiteral elements: (String, JSONValue)...) {
        var o: [String: JSONValue] = [:]
        for (k, v) in elements { o[k] = v }
        self = .object(o)
    }
}

/// Custom event properties (`props` on the server, `p` on the wire), and profile traits.
public typealias Props = [String: JSONValue]
