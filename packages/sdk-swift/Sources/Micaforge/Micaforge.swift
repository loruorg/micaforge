import Foundation

/// Micaforge for Swift: cookieless analytics for iOS, iPadOS, macOS, tvOS, watchOS and
/// visionOS apps.
///
/// Configure once, at launch:
///
/// ```swift
/// import Micaforge
///
/// Micaforge.configure(
///     siteKey: "1",
///     host: URL(string: "https://analytics.example.com")!,
///     options: MicaforgeOptions(domain: "example.com", pathPrefix: "/ios")
/// )
/// ```
///
/// then record screens and events from anywhere:
///
/// ```swift
/// Micaforge.screen("Settings")
/// Micaforge.track("signup", props: ["plan": "pro"])
/// Micaforge.identify(id: "u_123", traits: ["plan": "pro"])
/// if Micaforge.flags?.isEnabled("new-checkout") == true { ... }
/// ```
///
/// In SwiftUI, `.trackScreen("Settings")` on a view does the `screen` call for you.
///
/// Every call before ``configure(siteKey:host:options:)`` is ignored rather than queued: an
/// app that forgot to configure sends nothing instead of guessing where to send it.
public enum Micaforge {
    private static let current = Locked<MicaforgeClient?>(nil)

    /// The configured client, or `nil` before ``configure(siteKey:host:options:)``.
    public static var shared: MicaforgeClient? { current.withLock { $0 } }

    /// Start the tracker.
    ///
    /// - Parameters:
    ///   - siteKey: the numeric site id from the tracking snippet (`data-site="1"` means
    ///     `"1"`). It is public. Never put the site's ingest key in an app.
    ///   - host: your Micaforge server.
    ///   - options: see ``MicaforgeOptions``. Set ``MicaforgeOptions/domain`` unless your
    ///     server runs on a subdomain of the site.
    ///
    /// Calling it again does nothing and keeps the first client: one app reports once.
    @discardableResult
    public static func configure(siteKey: String, host: URL, options: MicaforgeOptions = MicaforgeOptions()) -> MicaforgeClient {
        if let existing = shared { return existing }
        let client = MicaforgeClient(siteKey: siteKey, host: host, options: options)
        let installed = current.withLock { slot -> MicaforgeClient in
            if let other = slot { return other }
            slot = client
            return client
        }
        if installed === client && options.trackLifecycle {
            Task { @MainActor in Lifecycle.install(client) }
        }
        return installed
    }

    /// A custom event: `track("signup", props: ["plan": "pro"])`.
    public static func track(_ name: String, props: Props = [:]) {
        shared?.track(name, props: props)
    }

    /// A screen view. See ``MicaforgeClient/screen(_:path:props:)``.
    public static func screen(_ name: String, path: String? = nil, props: Props = [:]) {
        shared?.screen(name, path: path, props: props)
    }

    /// Attach a stable id to following events and record traits on the profile.
    public static func identify(id: String, traits: Props = [:]) {
        shared?.identify(id: id, traits: traits)
    }

    /// Record traits on the profile without changing the id.
    public static func setTraits(_ traits: Props) {
        shared?.setTraits(traits)
    }

    /// Merge properties into every following event.
    public static func setProps(_ props: Props) {
        shared?.setProps(props)
    }

    /// Forget the identified id and the default properties (sign-out).
    public static func reset() {
        shared?.reset()
    }

    /// A money event: `revenue(19.99, currency: "EUR")`. See
    /// ``MicaforgeClient/revenue(_:currency:event:props:)``.
    public static func revenue(_ amount: Decimal, currency: String, event: String = "purchase", props: Props = [:]) {
        shared?.revenue(amount, currency: currency, event: event, props: props)
    }

    /// Stop recording on this device until ``optIn()``.
    public static func optOut() { shared?.optOut() }

    /// Undo ``optOut()``.
    public static func optIn() { shared?.optIn() }

    /// Whether this device opted out.
    public static var isOptedOut: Bool { shared?.isOptedOut ?? DefaultsOptOutStore(suiteName: nil).isOptedOut }

    /// Send everything waiting now.
    public static func flush() async { await shared?.flush() }

    /// Feature flags, or `nil` before ``configure(siteKey:host:options:)``.
    public static var flags: FlagsClient? { shared?.flags }
}
