import Foundation
import XCTest

@testable import Micaforge

/// The tracker, end to end through an injected transport: what it sends, when, and what it
/// keeps when it cannot.
final class ClientTests: XCTestCase {
    func testScreensAndEventsSendTheContractPayload() async throws {
        let net = MockTransport()
        let client = makeClient(options: testOptions { $0.pathPrefix = "/ios" }, transport: net)
        client.screen("Order history")
        client.track("signup", props: ["plan": "pro"])
        await client.flush()

        let request = try XCTUnwrap(net.requests.first)
        XCTAssertEqual(request.url?.absoluteString, "https://analytics.example.com/api/track/batch")
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
        XCTAssertEqual(request.value(forHTTPHeaderField: "User-Agent"), testDevice.userAgent)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Accept-Language"), "en-GB")

        let events = net.events
        XCTAssertEqual(events.count, 2)
        let page = events[0]
        XCTAssertEqual(page["s"], 1)
        XCTAssertEqual(page["t"], "pageview")
        XCTAssertEqual(page["u"], "https://example.com/ios/order-history")
        XCTAssertEqual(page["title"], "Order history")
        XCTAssertEqual(page["v"], "0.1.0")
        XCTAssertEqual(page["l"], "en-GB")
        XCTAssertEqual(page["tz"], "Europe/Lisbon")
        XCTAssertEqual(page["w"], 1512)
        XCTAssertEqual(page["h"], 982)
        XCTAssertNil(page["d"], "the first screen has no previous screen to report")
        XCTAssertNil(page["p"])
        XCTAssertNil(page["sr"])

        let custom = events[1]
        XCTAssertEqual(custom["t"], "custom")
        XCTAssertEqual(custom["n"], "signup")
        XCTAssertEqual(custom["u"], "https://example.com/ios/order-history")
        XCTAssertEqual(custom["p"], ["plan": "pro"])
        XCTAssertNil(custom["title"])
    }

    func testEveryKeyOnTheWireIsOneTheServerReads() async throws {
        let net = MockTransport()
        let client = makeClient(options: testOptions { $0.sampleRate = 99 }, transport: net)
        client.identify(id: "u_1", traits: ["plan": "pro"])
        client.screen("Home")
        client.track("x", props: ["a": 1])
        await client.flush()
        // CONTRACT.md section 4, plus `title`, which the batch route reads from SDKs.
        let allowed: Set<String> = ["s", "t", "n", "u", "r", "w", "h", "vw", "vh", "l", "tz", "d", "sc", "p", "id", "v", "e", "sr", "title"]
        for event in net.events {
            let keys = Set(try XCTUnwrap(event.objectValue).keys)
            XCTAssertTrue(keys.isSubset(of: allowed), "unexpected keys \(keys.subtracting(allowed))")
        }
    }

    func testTheNextScreenCarriesTheForegroundTimeOfThePrevious() async {
        let net = MockTransport()
        let clock = TestClock()
        let client = makeClient(transport: net, clock: clock)
        client.screen("A")
        await client.flush()
        clock.advance(4.2)
        client.screen("B")
        await client.flush()
        XCTAssertEqual(net.events.map { $0["d"] }, [nil, 4200])
    }

    func testGoingToTheBackgroundReportsEngagementAndSends() async {
        let net = MockTransport()
        let clock = TestClock()
        let client = makeClient(transport: net, clock: clock)
        client.screen("Reader")
        await client.flush()
        clock.advance(3)
        await client.enterBackground()
        XCTAssertEqual(net.events.last?["t"], "engagement")
        XCTAssertEqual(net.events.last?["d"], 3000)
        XCTAssertEqual(net.events.last?["u"], "https://example.com/reader")

        // Time in the background is not engagement.
        clock.advance(600)
        client.enterForeground()
        await client.flush() // a barrier: the foreground has been applied
        clock.advance(2)
        client.screen("Next")
        await client.flush()
        XCTAssertEqual(net.events.last?["d"], 2000)
    }

    func testIdentifySendsOncePerChangeAndTagsLaterEvents() async throws {
        let net = MockTransport()
        let client = makeClient(transport: net)
        client.identify(id: "u_1", traits: ["plan": "pro"])
        client.identify(id: "u_1", traits: ["plan": "pro"])
        client.track("x")
        client.reset()
        client.track("y")
        await client.flush()

        let events = net.events
        XCTAssertEqual(events.map { $0["t"] }, ["identify", "custom", "custom"])
        XCTAssertEqual(events[0]["id"], "u_1")
        XCTAssertEqual(events[0]["p"], ["plan": "pro"])
        XCTAssertEqual(events[1]["id"], "u_1")
        XCTAssertEqual(events[1]["p"], ["plan": "pro"], "traits become default properties, as in the browser")
        XCTAssertNil(events[2]["id"])
        XCTAssertNil(events[2]["p"])
    }

    func testRevenueRidesInPropsWhereTheServerLiftsIt() async {
        let net = MockTransport()
        let client = makeClient(transport: net)
        client.revenue(Decimal(string: "19.99")!, currency: "eur", props: ["sku": "A1"])
        client.revenue(Decimal(string: "-5.10")!, currency: "USD", event: "refund")
        client.revenue(19.99, currency: "EUR", event: "literal")
        await client.flush()
        let event = net.events.first
        XCTAssertEqual(event?["t"], "custom")
        XCTAssertEqual(event?["n"], "purchase")
        XCTAssertEqual(event?["p"]?["revenue"], 19.99)
        XCTAssertEqual(event?["p"]?["currency"], "EUR")
        XCTAssertEqual(event?["p"]?["sku"], "A1")
        XCTAssertEqual(net.events[1]["n"], "refund")
        XCTAssertEqual(net.events[1]["p"]?["revenue"], -5.1)
        XCTAssertEqual(net.events[2]["p"]?["revenue"], 19.99, "a float literal is rounded to the column's four places")
    }

    func testOptOutDropsTheQueueAndRecordsNothingUntilOptIn() async {
        let net = MockTransport()
        let optOut = MemoryOptOut()
        let client = makeClient(transport: net, optOut: optOut)
        client.track("before")
        client.optOut()
        client.track("during")
        await client.flush()
        XCTAssertTrue(net.requests.isEmpty)
        XCTAssertTrue(client.isOptedOut)

        client.optIn()
        client.track("after")
        await client.flush()
        XCTAssertEqual(net.events.map { $0["n"] }, ["after"])
        XCTAssertFalse(optOut.isOptedOut)
    }

    func testMaskExcludeAndQueryStrippingApplyBeforeSending() async {
        let net = MockTransport()
        let client = makeClient(
            options: testOptions {
                $0.mask = ["/users/*"]
                $0.exclude = ["/admin/**"]
                $0.stripQuery = .parameters(["token"])
            },
            transport: net
        )
        client.screen("User", path: "/users/42?token=abc&tab=2")
        client.screen("Admin", path: "/admin/x/y")
        client.track("clicked-in-admin")
        client.screen("Home", path: "/")
        await client.flush()
        XCTAssertEqual(net.events.map { $0["u"] }, ["https://example.com/users/*?tab=2", "https://example.com/"])
    }

    func testSamplingKeepsOrDropsTheWholeDeviceForTheDay() async {
        let bucket = Paths.fnvBucket("2026-10-09" + testDevice.userAgent)
        XCTAssertEqual(bucket, 78, "the same bucket the browser tracker computes for this seed")

        let kept = MockTransport()
        let inside = makeClient(options: testOptions { $0.sampleRate = Double(bucket + 1) }, transport: kept)
        inside.track("x")
        await inside.flush()
        XCTAssertEqual(kept.events.first?["sr"], .number(Double(bucket + 1)))

        let dropped = MockTransport()
        let outside = makeClient(options: testOptions { $0.sampleRate = Double(bucket) }, transport: dropped)
        outside.track("x")
        await outside.flush()
        XCTAssertTrue(dropped.requests.isEmpty)
    }

    func testFlushAtSendsByItselfInBatchesOfMaxBatchSize() async {
        let net = MockTransport()
        let client = makeClient(options: testOptions { $0.flushAt = 5; $0.maxBatchSize = 2 }, transport: net)
        for i in 0..<5 { client.track("e\(i)") }
        await eventually { net.events.count == 5 }
        XCTAssertEqual(net.batches.map(\.count), [2, 2, 1])
        XCTAssertEqual(net.events.map { $0["n"] }, ["e0", "e1", "e2", "e3", "e4"])
    }

    func testTheIntervalSendsWhatIsWaiting() async {
        let net = MockTransport()
        let client = makeClient(options: testOptions { $0.flushInterval = 0.05 }, transport: net)
        client.track("x")
        await eventually { net.events.count == 1 }
    }

    func testAFailedSendKeepsTheEventsAndBacksOff() async {
        let net = MockTransport { _ in (503, Data(), ["Retry-After": "5"]) }
        let clock = TestClock()
        let client = makeClient(transport: net, clock: clock)
        client.track("a")
        client.track("b")
        await client.flush()
        var queued = await client.engine.queue.count
        var attempt = await client.engine.attempt
        let notBefore = await client.engine.retryNotBefore
        XCTAssertEqual(queued, 2)
        XCTAssertEqual(attempt, 1)
        XCTAssertEqual(notBefore, clock.now() + 5, accuracy: 0.001, "Retry-After is longer than the first backoff, so it wins")

        net.respond { _ in throw URLError(.notConnectedToInternet) }
        await client.flush()
        attempt = await client.engine.attempt
        XCTAssertEqual(attempt, 2)

        net.respond { _ in (204, Data(), [:]) }
        await client.flush()
        queued = await client.engine.queue.count
        attempt = await client.engine.attempt
        XCTAssertEqual(queued, 0)
        XCTAssertEqual(attempt, 0)
        XCTAssertEqual(net.batches.last?.compactMap { $0["n"]?.stringValue }, ["a", "b"])
    }

    func testABatchTheServerRefusesIsDroppedNotRetried() async {
        let net = MockTransport { _ in (403, Data(), [:]) }
        let client = makeClient(transport: net)
        client.track("a")
        await client.flush()
        let queued = await client.engine.queue.count
        let attempt = await client.engine.attempt
        XCTAssertEqual(queued, 0)
        XCTAssertEqual(attempt, 0)
    }

    func testTheQueueSurvivesARelaunch() async {
        let store = MemoryEventStore()
        let offline = MockTransport { _ in throw URLError(.notConnectedToInternet) }
        let first = makeClient(transport: offline, store: store)
        first.track("one")
        first.track("two")
        await first.flush()
        await first.engine.persistNow()
        XCTAssertEqual(store.load().map(\.payload.n), ["one", "two"])

        let online = MockTransport()
        let second = makeClient(transport: online, store: store)
        second.track("three")
        await second.flush()
        XCTAssertEqual(online.events.map { $0["n"] }, ["one", "two", "three"])
        await second.engine.persistNow()
        XCTAssertTrue(store.load().isEmpty, "a delivered event leaves the disk copy too")
    }

    func testTheQueueIsBoundedOldestFirst() async {
        let net = MockTransport()
        let client = makeClient(options: testOptions { $0.maxQueueSize = 3 }, transport: net)
        for i in 0..<5 { client.track("e\(i)") }
        await client.flush()
        XCTAssertEqual(net.events.map { $0["n"] }, ["e2", "e3", "e4"])
    }

    func testADisabledClientOrABadSiteKeySendsNothing() async {
        let net = MockTransport()
        let off = makeClient(options: testOptions { $0.enabled = false }, transport: net)
        off.track("x")
        await off.flush()
        let bad = makeClient(siteKey: "abc", transport: net)
        bad.track("x")
        await bad.flush()
        XCTAssertTrue(net.requests.isEmpty)
        XCTAssertEqual(MicaforgeClient.parseSite(" 0001 "), 1)
        XCTAssertEqual(MicaforgeClient.parseSite("70000"), 0)
    }

    func testTheSwiftUIEntryDoesNotCountAReappearingScreenTwice() async {
        let net = MockTransport()
        let client = makeClient(transport: net)
        client.screenIfChanged("Home", path: nil, props: [:])
        client.screenIfChanged("Home", path: nil, props: [:])
        client.screenIfChanged("Settings", path: nil, props: [:])
        client.screenIfChanged("Home", path: nil, props: [:])
        client.screen("Home")
        await client.flush()
        XCTAssertEqual(net.events.map { $0["u"] }, [
            "https://example.com/home", "https://example.com/settings", "https://example.com/home", "https://example.com/home",
        ])
    }

    func testTheDomainDefaultsToTheHostAndIsNormalised() async {
        let net = MockTransport()
        let client = makeClient(options: testOptions { $0.domain = nil }, transport: net)
        XCTAssertEqual(client.domain, "analytics.example.com")
        XCTAssertEqual(MicaforgeClient.normaliseDomain("https://App.Example.com/path"), "app.example.com")
        XCTAssertEqual(MicaforgeClient.normaliseDomain("example.com/"), "example.com")
        XCTAssertNil(MicaforgeClient.normaliseDomain("  "))
    }

    func testAnEmptyEventNameIsNotRecorded() async {
        let net = MockTransport()
        let client = makeClient(transport: net)
        client.track("  ")
        client.track(" ok ")
        await client.flush()
        XCTAssertEqual(net.events.map { $0["n"] }, ["ok"])
    }
}
