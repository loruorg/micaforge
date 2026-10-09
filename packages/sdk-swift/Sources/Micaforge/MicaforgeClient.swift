import Foundation

/// One configured tracker. Most apps never touch this type: ``Micaforge`` holds one and
/// forwards to it. Build your own when you need two sites in one process, or in a test.
///
/// Every recording method returns at once and never throws. Calls are applied in the order
/// they were made, whatever thread made them.
public final class MicaforgeClient: Sendable {
    /// The numeric site id this client reports to.
    public let siteID: Int
    /// The Micaforge server.
    public let host: URL
    /// The hostname screens are reported under.
    public let domain: String
    /// Feature flags for this site.
    public let flags: FlagsClient

    let engine: Engine
    let options: MicaforgeOptions
    private let optOutStore: any OptOutStore
    private let input: AsyncStream<Envelope>.Continuation

    /// A command and, for the calls that wait, the continuation to resume once it is done.
    struct Envelope: Sendable {
        var command: Command
        var done: CheckedContinuation<Void, Never>?
    }

    /// Build a client.
    ///
    /// - Parameters:
    ///   - siteKey: the numeric site id, as in the tracking snippet's `data-site` (`"1"` for
    ///     the first site on an install). It is public. It is not the ingest key, which is a
    ///     server secret and must never ship in an app.
    ///   - host: your Micaforge server, such as `https://analytics.example.com`.
    ///   - options: everything else.
    public convenience init(siteKey: String, host: URL, options: MicaforgeOptions = MicaforgeOptions()) {
        let site = Self.parseSite(siteKey)
        let transport = options.transport ?? URLSessionTransport()
        let store: any EventStore = options.persistQueue ? FileEventStore(site: site) : MemoryEventStore()
        let deps = EngineDependencies(
            transport: transport,
            store: store,
            optOut: DefaultsOptOutStore(suiteName: nil),
            device: { await DeviceInfo.current(sdkVersion: Engine.version) },
            startsInForeground: { await Lifecycle.isForeground() }
        )
        self.init(siteKey: siteKey, host: host, options: options, deps: deps)
    }

    init(siteKey: String, host: URL, options: MicaforgeOptions, deps: EngineDependencies) {
        let site = Self.parseSite(siteKey)
        let log = Log(enabled: options.debug)
        if site == 0 { log.warning("siteKey \"\(siteKey)\" is not a site id (1-65535); nothing will be sent") }
        self.siteID = site
        self.host = host
        self.options = options
        self.optOutStore = deps.optOut
        self.domain = Self.normaliseDomain(options.domain) ?? host.host?.lowercased() ?? ""
        self.engine = Engine(site: site, host: host, domain: domain, options: options, deps: deps)
        self.flags = FlagsClient(site: site, host: host, options: options.flags, transport: deps.transport, clock: deps.clock, log: log)

        let (stream, continuation) = AsyncStream.makeStream(of: Envelope.self)
        self.input = continuation
        let engine = self.engine
        Task {
            for await envelope in stream {
                await engine.handle(envelope.command)
                envelope.done?.resume()
            }
        }
        continuation.yield(Envelope(command: .start))
    }

    deinit {
        input.finish()
    }

    /// `"1"`, `" 1 "` or `"0001"` is site 1; anything outside 1-65535 is 0, which sends
    /// nothing.
    static func parseSite(_ key: String) -> Int {
        guard let n = Int(key.trimmingCharacters(in: .whitespacesAndNewlines)), (1...65535).contains(n) else { return 0 }
        return n
    }

    /// A hostname from whatever the app passed: `example.com`, `https://example.com/`, or
    /// `App.Example.com`.
    static func normaliseDomain(_ raw: String?) -> String? {
        guard var s = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty else { return nil }
        if let url = URL(string: s), let host = url.host, url.scheme != nil { s = host }
        s = s.split(separator: "/").first.map(String.init) ?? s
        return s.lowercased()
    }

    private func send(_ command: Command) {
        input.yield(Envelope(command: command, done: nil))
    }

    private func sendAndWait(_ command: Command) async {
        await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
            if case .terminated = input.yield(Envelope(command: command, done: c)) { c.resume() }
        }
    }

    // MARK: - Recording

    /// A custom event: `track("signup", props: ["plan": "pro"])`. It is recorded against the
    /// current screen and carries the default properties.
    public func track(_ name: String, props: Props = [:]) {
        send(.track(name: name, props: props))
    }

    /// A screen view, reported as a pageview of `https://<domain><pathPrefix><path>` with the
    /// name as its title. Without `path`, the name is turned into one: `"Order history"` is
    /// `/order-history`. Pass a path when the screen shows one item of many
    /// (`/products/42`), and mask it with ``MicaforgeOptions/mask``.
    public func screen(_ name: String, path: String? = nil, props: Props = [:]) {
        send(.screen(name: name, path: path, props: props, onlyIfChanged: false))
    }

    /// The SwiftUI modifier's entry: a view that appears again (a tab switched back to, a
    /// sheet dismissed) does not count as a new screen view of the screen already showing.
    func screenIfChanged(_ name: String, path: String?, props: Props) {
        send(.screen(name: name, path: path, props: props, onlyIfChanged: true))
    }

    /// Attach a stable id to every following event and record `traits` on the profile. The
    /// traits also become default properties, as in the browser tracker. Calling it again
    /// with the same id and traits sends nothing.
    public func identify(id: String, traits: Props = [:]) {
        send(.identify(id: id, traits: traits))
        flags.setIdentity(id)
    }

    /// Record traits on the profile without changing the id or the default properties.
    public func setTraits(_ traits: Props) {
        send(.setTraits(traits))
    }

    /// Merge properties into every following event.
    public func setProps(_ props: Props) {
        send(.setProps(props))
    }

    /// Forget the identified id and the default properties, as when the person signs out.
    /// Flags go back to the anonymous subject.
    public func reset() {
        send(.reset)
        flags.setIdentity(nil)
    }

    /// A purchase, refund or any other money event: a custom event carrying `revenue` and
    /// `currency`, which the server lifts into the revenue columns every goal and report
    /// sums. `amount` is in major units (`19.99`), `currency` an ISO 4217 code.
    public func revenue(_ amount: Decimal, currency: String, event: String = "purchase", props: Props = [:]) {
        var merged = props
        // Through the decimal text rather than `NSDecimalNumber.doubleValue`, which turns
        // 19.99 into 19.990000000000002, and rounded to the four places of the server's
        // Decimal64(4) column, so a `Decimal` built from a float literal (19.99 as a
        // `Double` first) is sent as 19.99 too.
        let value = Double(amount.description) ?? NSDecimalNumber(decimal: amount).doubleValue
        merged["revenue"] = .number((value * 10_000).rounded() / 10_000)
        merged["currency"] = .string(currency.trimmingCharacters(in: .whitespaces).uppercased())
        send(.track(name: event, props: merged))
    }

    // MARK: - Consent

    /// Stop recording on this device, and drop anything waiting to be sent, until
    /// ``optIn()``. Survives relaunches.
    public func optOut() { send(.optOut(true)) }

    /// Undo ``optOut()``.
    public func optIn() { send(.optOut(false)) }

    /// Whether this device opted out.
    public var isOptedOut: Bool { optOutStore.isOptedOut }

    // MARK: - Delivery

    /// Send everything waiting now, and return once the server has answered (or the send
    /// failed and the events were kept for later). Earlier calls are applied first.
    public func flush() async {
        await sendAndWait(.flush)
        await flags.flushStats()
    }

    /// The app went to the background: report the screen's engagement time and send.
    func enterBackground() async {
        await sendAndWait(.background)
        await flags.flushStats()
    }

    /// The app came back to the foreground: restart the engagement clock.
    func enterForeground() {
        send(.foreground)
    }
}
