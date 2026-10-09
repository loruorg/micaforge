import Foundation

/// Everything ``Micaforge/configure(siteKey:host:options:)`` accepts beyond the site and the
/// host. Every field has a default that is right for most apps.
public struct MicaforgeOptions: Sendable {
    /// The hostname your screens are reported under, such as `example.com` or
    /// `app.example.com`.
    ///
    /// The server only accepts a hit whose page address is on one of the site's own
    /// domains (the site's domain, a subdomain of it, or a domain listed in the site's
    /// settings), because a site id is public and that check is what stops a stranger
    /// writing into your numbers. An app has no page address, so each screen is reported as
    /// `https://<domain><pathPrefix>/<screen>`. Leave it `nil` and the hostname of `host` is
    /// used, which is right when your Micaforge server runs on a subdomain of the site
    /// (`analytics.example.com` for `example.com`). When the server answers `403`, this is
    /// the setting to fix; the debug log says so.
    public var domain: String?

    /// A path put in front of every screen, such as `/ios`, to keep app screens apart from
    /// the web pages of the same site. Default none.
    public var pathPrefix: String

    /// Send as soon as this many events are waiting. Default 20.
    public var flushAt: Int

    /// Send whatever is waiting this many seconds after the first of it was recorded.
    /// Default 10. Events are also sent when the app goes to the background.
    public var flushInterval: TimeInterval

    /// The most events one request carries. Default 50; the server reads at most 200.
    public var maxBatchSize: Int

    /// The most events kept while the device is offline. Past it the oldest are dropped,
    /// so a phone that stays offline for a week does not fill its disk. Default 1000.
    public var maxQueueSize: Int

    /// Keep the queue on disk (Application Support, protected until the device is first
    /// unlocked) so events recorded offline survive a relaunch. Default true.
    public var persistQueue: Bool

    /// Percent of devices to keep, 1-99, drawn per device and day without storing
    /// anything, the same rule as the browser tracker's `data-sample`. `nil` keeps
    /// everyone. The rate rides on every hit, so the dashboard can say a report is sampled.
    public var sampleRate: Double?

    /// Screen path globs reported as the glob itself: `["/users/*"]` reports
    /// `/users/42` as `/users/*`, so the identifier never leaves the device. `*` matches one
    /// path segment, `**` any depth, `?` one character.
    public var mask: [String]

    /// Screen path globs that are never reported. Same glob rules as ``mask``.
    public var exclude: [String]

    /// Query parameters removed from screen paths before anything is sent.
    public var stripQuery: StripQuery

    /// Properties merged into every event. Change them later with `setProps`.
    public var defaultProps: Props

    /// Log every event and every request outcome to the unified log (subsystem
    /// `com.micaforge.sdk`). Default false.
    public var debug: Bool

    /// A kill switch: `false` builds a client that records and sends nothing. Default true.
    public var enabled: Bool

    /// Flush when the app goes to the background, and measure the time each screen was in
    /// the foreground (the engagement time the dashboard shows). Default true.
    public var trackLifecycle: Bool

    /// Feature flags.
    public var flags: FlagOptions

    /// The transport requests go through. `nil` uses a private, cookie-free `URLSession`.
    /// Inject your own to route through a proxy or to test.
    public var transport: (any MicaforgeTransport)?

    public init(
        domain: String? = nil,
        pathPrefix: String = "",
        flushAt: Int = 20,
        flushInterval: TimeInterval = 10,
        maxBatchSize: Int = 50,
        maxQueueSize: Int = 1000,
        persistQueue: Bool = true,
        sampleRate: Double? = nil,
        mask: [String] = [],
        exclude: [String] = [],
        stripQuery: StripQuery = .none,
        defaultProps: Props = [:],
        debug: Bool = false,
        enabled: Bool = true,
        trackLifecycle: Bool = true,
        flags: FlagOptions = FlagOptions(),
        transport: (any MicaforgeTransport)? = nil
    ) {
        self.domain = domain
        self.pathPrefix = pathPrefix
        self.flushAt = flushAt
        self.flushInterval = flushInterval
        self.maxBatchSize = maxBatchSize
        self.maxQueueSize = maxQueueSize
        self.persistQueue = persistQueue
        self.sampleRate = sampleRate
        self.mask = mask
        self.exclude = exclude
        self.stripQuery = stripQuery
        self.defaultProps = defaultProps
        self.debug = debug
        self.enabled = enabled
        self.trackLifecycle = trackLifecycle
        self.flags = flags
        self.transport = transport
    }
}

/// Which query parameters to drop from a screen path before it is sent.
public enum StripQuery: Sendable, Equatable {
    /// Keep the query as it is.
    case none
    /// Drop the whole query string.
    case all
    /// Drop only these parameters.
    case parameters([String])
}

/// Feature-flag settings.
///
/// Flags answer synchronously, in this order: the set the server evaluated (once it has
/// arrived), a ruleset you handed the client (decided on the device), the `bootstrap`
/// values, and finally a deterministic `fallback` rule. The two local steps run the shared
/// evaluator, so a fallback rule that mirrors the flag's server settings puts each user in
/// the bucket the server would choose.
public struct FlagOptions: Sendable {
    /// The environment the app runs in (`production`, `staging`, ...); flags may override
    /// per environment.
    public var environment: String?

    /// Attributes the flags' conditions and target groups test, such as `["plan": "pro"]`.
    public var context: [String: JSONValue]

    /// Values to answer with before the network does, keyed by flag key.
    public var bootstrap: [String: FlagRecord]

    /// Deterministic local rules, keyed by flag key, for when the network never answers.
    public var fallback: [String: FlagFallback]

    /// A ruleset to decide flags on the device, as `GET /api/flags/ruleset` returns its
    /// `ruleset` member (fetched by your own server, which holds the API key, or bundled with
    /// the app). Answers before `bootstrap` and `fallback`.
    public var rules: JSONValue?

    /// A stable bucketing subject for people who have not been identified, such as your
    /// own install id. Without one, an anonymous person is bucketed by a random id held in
    /// memory for this launch only: the SDK stores no identifier of its own. After
    /// `identify`, the identified id is the subject.
    public var subject: String?

    /// How long an evaluated set is reused before `load()` asks again, in seconds.
    /// Default 60.
    public var cacheTTL: TimeInterval

    /// Send evaluation and exposure counts to the flag's stats. Default true.
    public var stats: Bool

    public init(
        environment: String? = nil,
        context: [String: JSONValue] = [:],
        bootstrap: [String: FlagRecord] = [:],
        fallback: [String: FlagFallback] = [:],
        rules: JSONValue? = nil,
        subject: String? = nil,
        cacheTTL: TimeInterval = 60,
        stats: Bool = true
    ) {
        self.environment = environment
        self.context = context
        self.bootstrap = bootstrap
        self.fallback = fallback
        self.rules = rules
        self.subject = subject
        self.cacheTTL = cacheTTL
        self.stats = stats
    }
}

/// A locally evaluated rule, used before the network answers and when it never does.
public struct FlagFallback: Sendable, Equatable {
    /// 0-100. The user is in when their server bucket is inside it.
    public var rolloutPct: Double
    /// Weighted variants; weights are relative, they need not sum to 100.
    public var variants: [(key: String, weight: Double)]
    /// The value when the user does not bucket in. Default false.
    public var off: Bool

    public init(rolloutPct: Double = 100, variants: [(key: String, weight: Double)] = [], off: Bool = false) {
        self.rolloutPct = rolloutPct
        self.variants = variants
        self.off = off
    }

    public static func == (a: FlagFallback, b: FlagFallback) -> Bool {
        a.rolloutPct == b.rolloutPct && a.off == b.off && a.variants.map(\.key) == b.variants.map(\.key)
            && a.variants.map(\.weight) == b.variants.map(\.weight)
    }
}

/// One evaluated flag.
public struct FlagRecord: Sendable, Equatable, Codable {
    public var key: String
    public var enabled: Bool
    /// The variant, for a multivariate flag.
    public var variant: String?
    /// Arbitrary JSON attached to the flag.
    public var payload: JSONValue?
    /// Why the server or the evaluator decided this way, when one of them did.
    public var reason: String?

    public init(key: String, enabled: Bool, variant: String? = nil, payload: JSONValue? = nil, reason: String? = nil) {
        self.key = key
        self.enabled = enabled
        self.variant = variant
        self.payload = payload
        self.reason = reason
    }
}
