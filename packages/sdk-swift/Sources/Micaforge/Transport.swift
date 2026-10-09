import Foundation

/// How requests leave the device.
///
/// One method, so a test can stand in for the network and an app can route analytics
/// through its own proxy, certificate pinning or request signing without the SDK knowing.
/// A conforming type must be safe to call from any thread.
public protocol MicaforgeTransport: Sendable {
    /// Perform one request and return the response body and status. Throw for a request
    /// that never got an HTTP answer (offline, DNS, TLS); the SDK keeps the events and
    /// retries.
    func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse)
}

/// The default transport: `URLSession`, on a private ephemeral session that stores no
/// cookies, no cache and no credentials, so analytics requests can never pick up state
/// from the app's own networking or leave any behind.
public struct URLSessionTransport: MicaforgeTransport {
    public let session: URLSession

    public init(session: URLSession? = nil) {
        if let session {
            self.session = session
        } else {
            let config = URLSessionConfiguration.ephemeral
            config.httpShouldSetCookies = false
            config.httpCookieAcceptPolicy = .never
            config.urlCache = nil
            config.requestCachePolicy = .reloadIgnoringLocalCacheData
            config.timeoutIntervalForRequest = 15
            config.timeoutIntervalForResource = 30
            self.session = URLSession(configuration: config)
        }
    }

    public func send(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return (data, http)
    }
}

/// When to try again after a failed send.
///
/// Exponential backoff with jitter: the n-th consecutive failure waits
/// `min(maxDelay, baseDelay * 2^(n-1))`, multiplied by a random factor between one half
/// and one. The jitter matters more than it looks. When a Micaforge server comes back after
/// an outage, every app that queued events would otherwise retry on the same schedule and
/// arrive in synchronised waves; the random factor spreads them out.
struct RetryPolicy: Sendable {
    var baseDelay: TimeInterval = 2
    var maxDelay: TimeInterval = 300
    /// A number in `0.5...1`; injectable so a test can pin it.
    var jitter: @Sendable () -> Double = { Double.random(in: 0.5...1) }

    func delay(attempt: Int, retryAfter: TimeInterval? = nil) -> TimeInterval {
        let exponent = Double(max(0, min(attempt - 1, 20)))
        let backoff = min(maxDelay, baseDelay * pow(2, exponent)) * jitter()
        // A server that says when to come back is believed, within the same ceiling.
        if let retryAfter, retryAfter > backoff { return min(maxDelay, retryAfter) }
        return backoff
    }
}

/// What one delivery attempt came to.
enum SendOutcome: Equatable {
    /// Delivered (`2xx`), or refused for a reason that retrying cannot fix: drop the batch.
    case done(dropped: Bool)
    /// Not delivered; keep the batch and try again later.
    case retry(after: TimeInterval?)

    /// Classify an HTTP status from `POST /api/track/batch`.
    ///
    /// `2xx` is delivered. `408`, `425`, `429` and every `5xx` are worth retrying (the
    /// collection routes answer `503` with `Retry-After` when they run past their time
    /// budget). Every other `4xx` will be refused again, byte for byte, so the batch is
    /// dropped rather than retried forever: `403` in particular means the site does not
    /// exist or the screen hostname is not one of its domains.
    static func of(status: Int, retryAfter: TimeInterval?) -> SendOutcome {
        switch status {
        case 200..<300: return .done(dropped: false)
        case 408, 425, 429, 500...599: return .retry(after: retryAfter)
        default: return .done(dropped: true)
        }
    }
}
