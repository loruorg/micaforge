import Foundation
import XCTest

@testable import Micaforge

/// The app-side flag client: the server's answer, the local ruleset, the fallback rule,
/// and the counts it reports.
final class FlagsClientTests: XCTestCase {
    static let evaluateBody = Data("""
    {"data":{"site":1,"flags":{
      "new-nav":{"enabled":true,"variant":"b","reason":"match"},
      "off":{"enabled":false,"variant":null,"reason":"disabled"}
    },"ttl_seconds":60}}
    """.utf8)

    func makeFlags(_ options: FlagOptions, transport: MockTransport, clock: TestClock = TestClock()) -> FlagsClient {
        FlagsClient(site: 1, host: URL(string: "https://analytics.example.com/")!, options: options, transport: transport, clock: clock.now)
    }

    func testTheServersAnswerIsReadAndCached() async throws {
        let net = MockTransport { _ in (200, Self.evaluateBody, ["Content-Type": "application/json"]) }
        let clock = TestClock()
        let flags = makeFlags(FlagOptions(environment: "production", context: ["plan": "pro"], stats: false), transport: net, clock: clock)
        XCTAssertFalse(flags.isEnabled("new-nav"), "off before the answer arrives, with no other source")

        let changed = Locked<[String: FlagRecord]?>(nil)
        let subscription = flags.onChange { all in changed.withLock { $0 = all } }
        await flags.load()
        XCTAssertTrue(flags.isEnabled("new-nav"))
        XCTAssertEqual(flags.variant("new-nav"), "b")
        XCTAssertEqual(flags.flag("off").reason, "disabled")
        XCTAssertNil(flags.variant("off"))
        XCTAssertEqual(changed.withLock { $0 }?["new-nav"]?.variant, "b")
        subscription.cancel()

        let url = try XCTUnwrap(net.requests.first?.url)
        let query = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems)
        XCTAssertEqual(url.path, "/api/flags/evaluate")
        XCTAssertEqual(query.first { $0.name == "site" }?.value, "1")
        XCTAssertTrue(query.first { $0.name == "visitor" }?.value?.hasPrefix("anon-") ?? false)
        XCTAssertEqual(query.first { $0.name == "environment" }?.value, "production")
        XCTAssertEqual(query.first { $0.name == "context" }?.value, #"{"plan":"pro"}"#)
        XCTAssertNil(query.first { $0.name == "identity" })

        // Within the cache lifetime, no second request.
        await flags.load()
        XCTAssertEqual(net.requests.count, 1)
        clock.advance(61)
        await flags.load()
        XCTAssertEqual(net.requests.count, 2)
    }

    func testIdentifyChangesTheSubjectAndDropsTheCache() async throws {
        let net = MockTransport { _ in (200, Self.evaluateBody, [:]) }
        let flags = makeFlags(FlagOptions(stats: false), transport: net)
        await flags.load()
        flags.setIdentity("u_42")
        XCTAssertEqual(flags.subject, "u_42")
        XCTAssertFalse(flags.isEnabled("new-nav"), "the cached answer was someone else's")
        await flags.load()
        let url = try XCTUnwrap(net.requests.last?.url)
        XCTAssertTrue(url.absoluteString.contains("identity=u_42"))
        flags.setIdentity(nil)
        XCTAssertTrue(flags.subject.hasPrefix("anon-"))
    }

    func testAFailedLoadKeepsWhatItHad() async {
        let net = MockTransport { _ in (200, Self.evaluateBody, [:]) }
        let clock = TestClock()
        let flags = makeFlags(FlagOptions(stats: false), transport: net, clock: clock)
        await flags.load()
        net.respond { _ in throw URLError(.timedOut) }
        clock.advance(120)
        await flags.load()
        XCTAssertTrue(flags.isEnabled("new-nav"))
        net.respond { _ in (500, Data(), [:]) }
        await flags.load(force: true)
        XCTAssertTrue(flags.isEnabled("new-nav"))
    }

    func testALocalRulesetDecidesWithTheSharedEvaluator() throws {
        let ruleset = try XCTUnwrap(FlagVectorTests.vectors()["rulesets"]?["basic"])
        let flags = makeFlags(FlagOptions(context: ["plan": "pro"], rules: ruleset, subject: "visitor-3", stats: false), transport: MockTransport())
        let input = FlagInput(subject: "visitor-3", context: ["plan": "pro"])
        for key in ["on-flag", "off-flag", "half", "ab", "pro-or-team", "needs-variant"] {
            let expected = FlagEvaluator.evaluate(ruleset, key: key, input: input)
            XCTAssertEqual(flags.flag(key).enabled, expected.enabled, key)
            XCTAssertEqual(flags.flag(key).variant, expected.variant, key)
            XCTAssertEqual(flags.flag(key).reason, expected.reason.rawValue, key)
        }
    }

    func testBootstrapThenFallbackThenOff() {
        let flags = makeFlags(
            FlagOptions(
                bootstrap: ["boot": FlagRecord(key: "boot", enabled: true, variant: "x")],
                fallback: ["half": FlagFallback(rolloutPct: 50, off: false), "never": FlagFallback(rolloutPct: 0, off: true)],
                subject: "visitor-1",
                stats: false
            ),
            transport: MockTransport()
        )
        XCTAssertEqual(flags.variant("boot"), "x")
        let expected = FlagEvaluator.bucket(salt: "half", subject: "visitor-1") < 5000
        XCTAssertEqual(flags.isEnabled("half"), expected, "the fallback buckets the way the server would")
        XCTAssertTrue(flags.isEnabled("never"), "outside a zero rollout the fallback answers its `off` value")
        XCTAssertFalse(flags.isEnabled("unknown"))
        XCTAssertEqual(flags.bucket("half"), FlagEvaluator.bucket(salt: "half", subject: "visitor-1") / 100)
    }

    func testExposuresAndEvaluationsAreCountedAndSent() async throws {
        let net = MockTransport { request in
            request.url?.path == "/api/flags/evaluate" ? (200, Self.evaluateBody, [:]) : (204, Data(), [:])
        }
        let flags = makeFlags(FlagOptions(environment: "staging"), transport: net)
        await flags.load()
        _ = flags.isEnabled("new-nav")
        _ = flags.isEnabled("new-nav")
        await flags.flushStats()

        let post = try XCTUnwrap(net.requests.first { $0.url?.path == "/api/flags/events" })
        XCTAssertEqual(post.httpMethod, "POST")
        let body = try XCTUnwrap(JSONValue.parse(post.httpBody ?? Data()))
        XCTAssertEqual(body["site"], 1)
        XCTAssertEqual(body["source"], "browser")
        XCTAssertEqual(body["environment"], "staging")
        let events = try XCTUnwrap(body["events"]?.arrayValue)
        let exposure = events.first { $0["kind"] == "exposure" }
        XCTAssertEqual(exposure?["key"], "new-nav")
        XCTAssertEqual(exposure?["count"], 1, "only the first read is an exposure")
        XCTAssertEqual(exposure?["variant"], "b")
        XCTAssertEqual(Set(events.filter { $0["kind"] == "evaluation" }.compactMap { $0["key"]?.stringValue }), ["new-nav", "off"])
        XCTAssertTrue(flags.pendingTallies.isEmpty)
    }

    func testRecordsAreReadFromEveryShapeTheServerOrAPageMightSend() {
        XCTAssertEqual(FlagsClient.records(from: ["flags": ["a": true, "b": "v2"]])["b"]?.variant, "v2")
        XCTAssertEqual(FlagsClient.records(from: [["key": "c", "enabled": true]])["c"]?.enabled, true)
        XCTAssertTrue(FlagsClient.records(from: "nonsense").isEmpty)
    }
}
