import Foundation
import XCTest

@testable import Micaforge

/// Paths, globs, retry timing, storage and the device description.
final class SupportTests: XCTestCase {
    func testGlobsFollowTheTrackersRules() {
        XCTAssertTrue(Paths.matches("/admin/users", glob: "/admin/*"))
        XCTAssertFalse(Paths.matches("/admin/users/1", glob: "/admin/*"), "* is one segment")
        XCTAssertTrue(Paths.matches("/admin/users/1", glob: "/admin/**"))
        XCTAssertTrue(Paths.matches("/a/b", glob: "/a/?"))
        XCTAssertFalse(Paths.matches("/a/bc", glob: "/a/?"))
        XCTAssertTrue(Paths.matches("/docs.html", glob: "/docs.html"))
        XCTAssertFalse(Paths.matches("/docsxhtml", glob: "/docs.html"), "a dot is a dot, not any character")
    }

    func testScreenNamesBecomePaths() {
        XCTAssertEqual(Paths.slug("Order history"), "/order-history")
        XCTAssertEqual(Paths.slug("  Settings › Profile  "), "/settings-profile")
        XCTAssertEqual(Paths.slug("Café"), "/café")
        XCTAssertEqual(Paths.screenURL(domain: "example.com", prefix: "ios/", path: "/café", mask: [], exclude: [], strip: .none), "https://example.com/ios/caf%C3%A9")
        XCTAssertEqual(Paths.screenURL(domain: "example.com", prefix: "", path: "a?x=1&y=2", mask: [], exclude: [], strip: .all), "https://example.com/a")
    }

    func testTheSamplingBucketIsTheBrowsersBucket() {
        // Computed with the browser tracker's `bucket` from packages/sdk/src/core.ts.
        XCTAssertEqual(Paths.fnvBucket("abc"), 31)
        XCTAssertEqual(Paths.fnvBucket("ünï😀"), 19)
        XCTAssertTrue(Paths.inSample(rate: nil, seed: "x"))
        XCTAssertTrue(Paths.inSample(rate: 100, seed: "x"))
        XCTAssertFalse(Paths.inSample(rate: 0, seed: "x"))
    }

    func testBackoffDoublesIsCappedAndBelievesRetryAfter() {
        let policy = RetryPolicy(baseDelay: 2, maxDelay: 300, jitter: { 1 })
        XCTAssertEqual(policy.delay(attempt: 1), 2)
        XCTAssertEqual(policy.delay(attempt: 5), 32)
        XCTAssertEqual(policy.delay(attempt: 30), 300)
        XCTAssertEqual(policy.delay(attempt: 1, retryAfter: 20), 20)
        XCTAssertEqual(policy.delay(attempt: 1, retryAfter: 9999), 300)
        let jittered = RetryPolicy(baseDelay: 2, maxDelay: 300, jitter: { 0.5 })
        XCTAssertEqual(jittered.delay(attempt: 3), 4)
        for _ in 0..<100 {
            let d = RetryPolicy().delay(attempt: 2)
            XCTAssertTrue((2...4).contains(d))
        }
    }

    func testStatusesAreClassifiedForRetry() {
        XCTAssertEqual(SendOutcome.of(status: 204, retryAfter: nil), .done(dropped: false))
        XCTAssertEqual(SendOutcome.of(status: 403, retryAfter: nil), .done(dropped: true))
        XCTAssertEqual(SendOutcome.of(status: 400, retryAfter: nil), .done(dropped: true))
        XCTAssertEqual(SendOutcome.of(status: 429, retryAfter: 3), .retry(after: 3))
        XCTAssertEqual(SendOutcome.of(status: 503, retryAfter: nil), .retry(after: nil))
    }

    func testTheFileStoreRoundTripsStaysOutOfBackupsAndCleansUp() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("micaforge-tests-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let store = FileEventStore(site: 7, directory: dir)
        let events = [QueuedEvent(seq: 1, payload: TrackPayload(s: 7, t: "custom", n: "x", u: "https://example.com/", v: "0.1.0"))]
        store.save(events)
        XCTAssertEqual(store.load(), events)
        XCTAssertTrue(store.url.path.hasSuffix("Micaforge/queue-7.json"))
        let values = try store.url.resourceValues(forKeys: [.isExcludedFromBackupKey])
        XCTAssertEqual(values.isExcludedFromBackup, true)
        store.save([])
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.url.path))
        XCTAssertTrue(store.load().isEmpty)
    }

    func testAnEmptyFieldIsLeftOffTheWire() throws {
        let payload = TrackPayload(s: 1, t: "pageview", u: "https://example.com/", v: "0.1.0")
        let json = try XCTUnwrap(JSONValue.parse(try JSONEncoder().encode(payload))?.objectValue)
        XCTAssertEqual(Set(json.keys), ["s", "t", "u", "v"])
    }

    func testJSONValuesKeepBooleansApartFromNumbers() throws {
        let parsed = try XCTUnwrap(JSONValue.parse(Data(#"{"a":true,"b":1,"c":1.5,"d":null,"e":[1,"x"]}"#.utf8)))
        XCTAssertEqual(parsed["a"], .bool(true))
        XCTAssertEqual(parsed["b"], .number(1))
        XCTAssertEqual(parsed["c"], 1.5)
        XCTAssertEqual(parsed["d"], .null)
        XCTAssertEqual(parsed["e"], [1, "x"])
        XCTAssertEqual(JSONValue.object(["b": 2, "a": "x"]).jsonString(), #"{"a":"x","b":2}"#)
        XCTAssertEqual(JSONValue.number(.infinity).jsonString(), "null")
    }

    @MainActor
    func testTheUserAgentNamesThePlatformAndNeverTheApp() {
        let device = DeviceInfo.current(sdkVersion: "0.1.0")
        XCTAssertTrue(device.userAgent.hasPrefix("Mozilla/5.0 ("), device.userAgent)
        XCTAssertTrue(device.userAgent.hasSuffix(") Micaforge-Swift/0.1.0"), device.userAgent)
        #if os(macOS)
        XCTAssertTrue(device.userAgent.contains("Macintosh; Intel Mac OS X "), device.userAgent)
        #endif
        let lower = device.userAgent.lowercased()
        for token in ["bot", "crawl", "spider", "feed", "preview", "monitor", "headless"] {
            XCTAssertFalse(lower.contains(token), "the server would score \(token) as a bot")
        }
        XCTAssertFalse(device.language.isEmpty)
        XCTAssertFalse(device.timeZone.isEmpty)
    }
}
