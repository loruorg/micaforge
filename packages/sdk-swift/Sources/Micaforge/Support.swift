import Foundation

#if canImport(os)
import os
#endif

/// A value behind a lock, for the few places that must answer synchronously from any
/// thread (the shared client, flag reads from a SwiftUI body).
///
/// An actor cannot answer a synchronous call, and `Mutex` needs iOS 18, so this is the
/// classic lock-and-closure box. Every access goes through ``withLock(_:)``; nothing
/// escapes the closure, which is what makes the `@unchecked Sendable` true.
final class Locked<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Value

    init(_ value: Value) { self.value = value }

    func withLock<T>(_ body: (inout Value) throws -> T) rethrows -> T {
        lock.lock()
        defer { lock.unlock() }
        return try body(&value)
    }
}

/// The debug log: the unified log under subsystem `com.micaforge.sdk`, only when `debug` is
/// on. Messages are public on purpose; they hold event names and screen paths, which are
/// what a developer turning on debug wants to read in Console.app, and never a secret.
struct Log: Sendable {
    let enabled: Bool

    #if canImport(os)
    private static let logger = Logger(subsystem: "com.micaforge.sdk", category: "micaforge")
    #endif

    func debug(_ message: @autoclosure () -> String) {
        guard enabled else { return }
        let text = message()
        #if canImport(os)
        Self.logger.debug("\(text, privacy: .public)")
        #else
        print("micaforge: \(text)")
        #endif
    }

    func warning(_ message: @autoclosure () -> String) {
        guard enabled else { return }
        let text = message()
        #if canImport(os)
        Self.logger.warning("\(text, privacy: .public)")
        #else
        print("micaforge: \(text)")
        #endif
    }
}

/// A monotonic clock in seconds, injectable so tests can move time.
typealias MonotonicClock = @Sendable () -> TimeInterval

/// The system uptime: monotonic, unaffected by the user changing the wall clock.
let systemClock: MonotonicClock = { ProcessInfo.processInfo.systemUptime }
