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

/// What the SDK says about the device: the request headers the server reads and the
/// screen fields of the payload.
///
/// # Why the user agent is built by hand
///
/// The server derives browser, OS and device type from the `User-Agent` header and runs
/// the same bot heuristics on it that it runs on every browser hit. `URLSession`'s default
/// (`MyApp/1 CFNetwork/1494 Darwin/23.4.0`) names no platform, and an app's own name is
/// free text: an app called "FeedBot" would be filed as a crawler. So the SDK sends a
/// fixed, browser-shaped string that carries the platform in parentheses, the way the
/// server's parser expects, and never the app's name:
///
/// - iPhone: `Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) Micaforge-Swift/0.1.0`
/// - iPad: `Mozilla/5.0 (iPad; CPU OS 17_5 like Mac OS X) Micaforge-Swift/0.1.0`
/// - Mac: `Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) Micaforge-Swift/0.1.0`
/// - Apple TV, Apple Watch and Vision Pro name themselves (`Apple TV; tvOS 17.5`), which
///   the server reads as a television, a wearable and a desktop-class device.
///
/// The server folds the user agent into the cookieless visitor hash, so every device on one
/// network with one OS version counts as one visitor per day, exactly as two identical
/// browsers behind one address do. That is the product's privacy model, not a defect of
/// this SDK.
struct DeviceInfo: Sendable, Equatable {
    var userAgent: String
    var language: String
    var timeZone: String
    var screenWidth: Int?
    var screenHeight: Int?

    /// The headers every request carries. `Accept` and `Accept-Language` are what a browser
    /// sends; leaving them out would cost the hit bot-score points it has not earned.
    var headers: [String: String] {
        ["User-Agent": userAgent, "Accept": "application/json", "Accept-Language": language]
    }

    /// The device this process runs on. Main actor because UIKit's device and screen are.
    @MainActor
    static func current(sdkVersion: String) -> DeviceInfo {
        var width: Int?
        var height: Int?

        #if os(iOS) || os(tvOS)
        let bounds = UIScreen.main.bounds
        width = Int(bounds.width)
        height = Int(bounds.height)
        #elseif os(watchOS)
        let bounds = WKInterfaceDevice.current().screenBounds
        width = Int(bounds.width)
        height = Int(bounds.height)
        #elseif os(macOS)
        if let frame = NSScreen.main?.frame {
            width = Int(frame.width)
            height = Int(frame.height)
        }
        #endif

        return DeviceInfo(
            userAgent: "Mozilla/5.0 (\(platformComment())) Micaforge-Swift/\(sdkVersion)",
            language: Locale.preferredLanguages.first ?? Locale.current.identifier.replacingOccurrences(of: "_", with: "-"),
            timeZone: TimeZone.current.identifier,
            screenWidth: width,
            screenHeight: height
        )
    }

    /// The parenthesised platform of the user agent, in the spelling the server's parser
    /// recognises for each family.
    @MainActor
    static func platformComment() -> String {
        let v = ProcessInfo.processInfo.operatingSystemVersion
        func version(_ separator: String) -> String {
            [v.majorVersion, v.minorVersion].map(String.init).joined(separator: separator)
                + (v.patchVersion > 0 ? separator + String(v.patchVersion) : "")
        }
        #if os(iOS) && !targetEnvironment(macCatalyst)
        switch UIDevice.current.userInterfaceIdiom {
        case .pad: return "iPad; CPU OS \(version("_")) like Mac OS X"
        case .mac: return "Macintosh; Intel Mac OS X \(version("_"))"
        default: return "iPhone; CPU iPhone OS \(version("_")) like Mac OS X"
        }
        #elseif os(tvOS)
        return "Apple TV; tvOS \(version("."))"
        #elseif os(watchOS)
        return "Apple Watch; watchOS \(version("."))"
        #elseif os(visionOS)
        return "Apple Vision; visionOS \(version("."))"
        #else
        return "Macintosh; Intel Mac OS X \(version("_"))"
        #endif
    }
}
