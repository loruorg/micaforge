import Foundation
import XCTest

@testable import Micaforge

/// A network that records every request and answers from a script.
final class MockTransport: MicaforgeTransport, Sendable {
    typealias Responder = @Sendable (URLRequest) throws -> (status: Int, body: Data, headers: [String: String])

    private let recorded = Locked<[URLRequest]>([])
    private let responder: Locked<Responder>

    init(_ responder: @escaping Responder = { _ in (204, Data(), [:]) }) {
        self.responder = Locked(responder)
    }

    func respond(_ responder: @escaping Responder) {
        self.responder.withLock { $0 = responder }
    }

    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        recorded.withLock { $0.append(request) }
        let answer = try responder.withLock { $0 }(request)
        let response = HTTPURLResponse(url: request.url!, statusCode: answer.status, httpVersion: "HTTP/1.1", headerFields: answer.headers)!
        return (answer.body, response)
    }

    var requests: [URLRequest] { recorded.withLock { $0 } }

    /// Every `POST /api/track/batch` body, parsed.
    var batches: [[JSONValue]] {
        requests
            .filter { $0.url?.path == "/api/track/batch" }
            .map { JSONValue.parse($0.httpBody ?? Data())?.arrayValue ?? [] }
    }

    /// Every event delivered, in order.
    var events: [JSONValue] { batches.flatMap { $0 } }
}

/// A clock the test moves by hand.
final class TestClock: Sendable {
    private let value = Locked<TimeInterval>(1000)

    var now: MonotonicClock {
        { [value] in value.withLock { $0 } }
    }

    func advance(_ seconds: TimeInterval) {
        value.withLock { $0 += seconds }
    }
}

/// An opt-out held in memory, so tests never touch the real `UserDefaults`.
final class MemoryOptOut: OptOutStore, Sendable {
    private let value = Locked(false)

    var isOptedOut: Bool { value.withLock { $0 } }

    func set(_ optedOut: Bool) { value.withLock { $0 = optedOut } }
}

let testDevice = DeviceInfo(
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) Micaforge-Swift/0.1.0",
    language: "en-GB",
    timeZone: "Europe/Lisbon",
    screenWidth: 1512,
    screenHeight: 982
)

/// Options for a test: nothing sends by itself unless the test asks.
func testOptions(_ edit: (inout MicaforgeOptions) -> Void = { _ in }) -> MicaforgeOptions {
    var o = MicaforgeOptions(domain: "example.com", flushAt: 1000, flushInterval: 3600)
    edit(&o)
    return o
}

func makeClient(
    siteKey: String = "1",
    options: MicaforgeOptions = testOptions(),
    transport: MockTransport,
    store: any EventStore = MemoryEventStore(),
    optOut: any OptOutStore = MemoryOptOut(),
    clock: TestClock = TestClock()
) -> MicaforgeClient {
    let deps = EngineDependencies(
        transport: transport,
        store: store,
        optOut: optOut,
        clock: clock.now,
        retry: RetryPolicy(baseDelay: 2, maxDelay: 300, jitter: { 1 }),
        device: { testDevice },
        startsInForeground: { true },
        today: { "2026-10-09" }
    )
    return MicaforgeClient(siteKey: siteKey, host: URL(string: "https://analytics.example.com")!, options: options, deps: deps)
}

/// Wait until `condition` holds, or fail after `timeout` seconds.
func eventually(timeout: TimeInterval = 3, file: StaticString = #filePath, line: UInt = #line, _ condition: @escaping () -> Bool) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() {
        if Date() > deadline {
            XCTFail("condition not met in \(timeout) s", file: file, line: line)
            return
        }
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
}
