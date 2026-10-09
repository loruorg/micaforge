import Foundation
import XCTest

@testable import Micaforge

/// The shared flag vectors, asserted against the Swift copy of the evaluator.
///
/// `packages/sdk/test/flag-vectors.json` is computed by the Rust evaluator the server runs
/// (`micaforge_core::flags`) and asserted by the Rust, browser, Node and Swift suites alike,
/// so a case that passes here is a user who gets the same answer from all four. The file is
/// read in place rather than copied, so there is one file and it cannot drift.
final class FlagVectorTests: XCTestCase {
    /// `packages/sdk/test/flag-vectors.json`, found from this file's own path.
    static func vectors() throws -> JSONValue {
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent() // MicaforgeTests
            .deletingLastPathComponent() // Tests
            .deletingLastPathComponent() // sdk-swift
            .deletingLastPathComponent() // packages
            .appendingPathComponent("sdk/test/flag-vectors.json")
        let data = try Data(contentsOf: url)
        return try XCTUnwrap(JSONValue.parse(data), "flag-vectors.json is not JSON")
    }

    static func hex(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02x", $0) }.joined()
    }

    func testBlake3MatchesEveryDigestTheTreeIncluded() throws {
        let cases = try XCTUnwrap(Self.vectors()["blake3"]?.arrayValue)
        XCTAssertGreaterThan(cases.count, 10)
        for c in cases {
            let input: [UInt8]
            if let text = c["text"]?.stringValue {
                input = Array(text.utf8)
            } else {
                let len = Int(c["len"]?.numberValue ?? 0)
                input = (0..<len).map { UInt8($0 % 251) }
            }
            XCTAssertEqual(Self.hex(BLAKE3.hash(input)), c["hex"]?.stringValue, "len \(input.count)")
        }
    }

    func testEverySubjectLandsInTheServersBucket() throws {
        let cases = try XCTUnwrap(Self.vectors()["buckets"]?.arrayValue)
        XCTAssertFalse(cases.isEmpty)
        for c in cases {
            let salt = try XCTUnwrap(c["salt"]?.stringValue)
            let subject = try XCTUnwrap(c["subject"]?.stringValue)
            let expected = try XCTUnwrap(c["bucket"]?.numberValue)
            XCTAssertEqual(FlagEvaluator.bucket(salt: salt, subject: subject), Int(expected), "\(salt) / \(subject)")
        }
    }

    func testBucketsStayInsideTheRange() {
        for i in 0..<500 {
            let b = FlagEvaluator.bucket(salt: "k", subject: "visitor-\(i)")
            XCTAssertTrue((0..<10_000).contains(b))
        }
    }

    func testEveryCaseIsDecidedExactlyAsTheServerDecidesIt() throws {
        let v = try Self.vectors()
        let cases = try XCTUnwrap(v["cases"]?.arrayValue)
        let rulesets = try XCTUnwrap(v["rulesets"]?.objectValue)
        XCTAssertGreaterThan(cases.count, 100)
        for c in cases {
            let name = c["name"]?.stringValue ?? "?"
            let ruleset: JSONValue
            if let named = c["ruleset"]?.stringValue {
                ruleset = try XCTUnwrap(rulesets[named], name)
            } else {
                ruleset = try XCTUnwrap(c["ruleset"], name)
            }
            let input = FlagInput(
                subject: c["subject"]?.stringValue ?? "",
                context: c["context"]?.objectValue ?? [:],
                environment: c["environment"]?.stringValue
            )
            let decided = FlagEvaluator.evaluate(ruleset, key: c["key"]?.stringValue ?? "", input: input)
            let expect = try XCTUnwrap(c["expect"], name)
            XCTAssertEqual(decided.enabled, expect["enabled"]?.boolValue, name)
            XCTAssertEqual(decided.variant, expect["variant"]?.stringValue, name)
            XCTAssertEqual(decided.reason.rawValue, expect["reason"]?.stringValue, name)
        }
    }

    func testAWholeRulesetDecidesTheSameAsOneFlagAtATime() throws {
        let ruleset = try XCTUnwrap(Self.vectors()["rulesets"]?["basic"])
        let input = FlagInput(subject: "visitor-3", context: ["plan": "pro", "country": "PT"])
        let compiled = FlagEvaluator.compile(ruleset)
        let all = compiled.evaluateAll(input)
        XCTAssertEqual(Set(all.keys), Set(compiled.keys))
        for key in compiled.keys {
            XCTAssertEqual(all[key], FlagEvaluator.evaluate(ruleset, key: key, input: input), key)
        }
    }

    func testJunkNeverCrashesAndEvaluatesOff() {
        let junk: [JSONValue] = [
            .null, 42, "rules", [], ["flags": "no"],
            ["flags": [nil, 1, "x", ["key": 5], ["key": "a", "enabled": true, "conditions": ["logic": "or", "filters": [[]]]]]],
            ["flags": [["key": "a", "enabled": true, "variants": "x", "dependencies": [nil, 1], "environments": [1]]]],
        ]
        for ruleset in junk {
            for key in ["a", "constructor", ""] {
                let d = FlagEvaluator.evaluate(ruleset, key: key, input: FlagInput(subject: "s"))
                if !d.enabled { XCTAssertNotEqual(d.reason, .match) }
            }
        }
    }

    func testNumbersPrintAndParseTheWayTheServerDoes() {
        XCTAssertEqual(FlagEvaluator.canonicalNumber(5), "5")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(-0.0), "0")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(2.5), "2.5")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(18.0), "18")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(1e21), "1000000000000000000000")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(1e16), "10000000000000000")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(1e-7), "0.0000001")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(1.5e-7), "0.00000015")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(1234567890123456), "1234567890123456")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(0.1), "0.1")
        XCTAssertEqual(FlagEvaluator.canonicalNumber(-12.75), "-12.75")
        XCTAssertEqual(FlagEvaluator.parseNumber(" 2.5 "), 2.5)
        XCTAssertEqual(FlagEvaluator.parseNumber("+3"), 3)
        XCTAssertEqual(FlagEvaluator.parseNumber(".5"), 0.5)
        XCTAssertEqual(FlagEvaluator.parseNumber("5."), 5)
        XCTAssertEqual(FlagEvaluator.parseNumber("1e3"), 1000)
        for refused in ["", "inf", "NaN", "Infinity", "1_000", "0x10", "1e", ".", "--1", "1e999"] {
            XCTAssertNil(FlagEvaluator.parseNumber(refused), refused)
        }
    }

    func testTextComparesByBytesNotByCanonicalEquivalence() {
        // A precomposed é and an e with a combining accent are equal Swift strings and
        // different Rust strings; the evaluator must side with the server.
        let ruleset: JSONValue = ["flags": [["key": "x", "enabled": true, "conditions": [["dimension": "name", "op": "is", "value": "caf\u{e9}"]]]]]
        XCTAssertTrue(FlagEvaluator.evaluate(ruleset, key: "x", input: FlagInput(subject: "s", context: ["name": "caf\u{e9}"])).enabled)
        XCTAssertFalse(FlagEvaluator.evaluate(ruleset, key: "x", input: FlagInput(subject: "s", context: ["name": "cafe\u{301}"])).enabled)
    }

    func testTrimUsesRustsWhitespaceSet() {
        XCTAssertEqual(FlagEvaluator.trim("\u{85} a \u{3000}"), "a")
        XCTAssertEqual(FlagEvaluator.trim("\u{feff}a"), "\u{feff}a")
    }
}
