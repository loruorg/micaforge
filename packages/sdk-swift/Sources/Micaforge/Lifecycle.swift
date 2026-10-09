import Foundation

#if canImport(UIKit)
import UIKit
#endif
#if canImport(AppKit) && !targetEnvironment(macCatalyst)
import AppKit
#endif
#if os(watchOS)
import WatchKit
#endif

/// The app's comings and goings, turned into engagement time and flushes.
///
/// Going to the background is the moment that matters: it is the last point at which the
/// system reliably lets an app run, so the SDK reports the current screen's foreground
/// time and sends the queue there, inside a background task on UIKit so the request is
/// allowed to finish. Coming back restarts the engagement clock.
///
/// - UIKit (iOS, iPadOS, tvOS, visionOS, Mac Catalyst): `didEnterBackground` and
///   `willEnterForeground`.
/// - AppKit (macOS): `didResignActive` and `didBecomeActive`. A Mac app that is not the
///   active app is not being read.
/// - watchOS 9 and later: `WKApplication`'s background and foreground notifications.
@MainActor
enum Lifecycle {
    /// Observer tokens, kept for the life of the process: the shared client lives as long.
    private static var tokens: [NSObjectProtocol] = []

    static func install(_ client: MicaforgeClient) {
        let center = NotificationCenter.default
        func on(_ name: Notification.Name, _ action: @escaping @MainActor () -> Void) {
            tokens.append(center.addObserver(forName: name, object: nil, queue: .main) { _ in
                MainActor.assumeIsolated { action() }
            })
        }

        #if canImport(UIKit) && !os(watchOS)
        on(UIApplication.didEnterBackgroundNotification) {
            let app = UIApplication.shared
            var task = UIBackgroundTaskIdentifier.invalid
            task = app.beginBackgroundTask(withName: "Micaforge flush") {
                app.endBackgroundTask(task)
                task = .invalid
            }
            let started = task
            Task {
                await client.enterBackground()
                await MainActor.run {
                    if started != .invalid { app.endBackgroundTask(started) }
                }
            }
        }
        on(UIApplication.willEnterForegroundNotification) { client.enterForeground() }
        #elseif os(watchOS)
        if #available(watchOS 9.0, *) {
            on(WKApplication.didEnterBackgroundNotification) { Task { await client.enterBackground() } }
            on(WKApplication.willEnterForegroundNotification) { client.enterForeground() }
        }
        #elseif canImport(AppKit)
        on(NSApplication.didResignActiveNotification) { Task { await client.enterBackground() } }
        on(NSApplication.didBecomeActiveNotification) { client.enterForeground() }
        #endif
    }

    /// Whether the app is in the foreground right now.
    static func isForeground() -> Bool {
        #if canImport(UIKit) && !os(watchOS)
        return UIApplication.shared.applicationState != .background
        #elseif os(watchOS)
        if #available(watchOS 9.0, *) { return WKApplication.shared().applicationState != .background }
        return WKExtension.shared().applicationState != .background
        #elseif canImport(AppKit)
        return NSApplication.shared.isActive || !NSApplication.shared.isRunning
        #else
        return true
        #endif
    }
}
