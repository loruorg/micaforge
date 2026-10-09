import Foundation

/// Feature flags in an app.
///
/// Reads `GET /api/flags/evaluate?site=&visitor=&identity=` and keeps the answer in memory
/// for ``FlagOptions/cacheTTL``. Reads are synchronous, so a SwiftUI `body` can ask, and
/// answer in this order:
///
/// 1. the set the server evaluated, once it has arrived;
/// 2. a ruleset handed to the client (``FlagOptions/rules``), decided on the device;
/// 3. the ``FlagOptions/bootstrap`` values;
/// 4. a deterministic ``FlagOptions/fallback`` rule, then off.
///
/// Steps 2 and 4 run ``FlagEvaluator``, the same rule the server, the browser SDK and the
/// Node SDK run, held to it by one shared file of test vectors. So a flag holds the same
/// value before, during and after the network call, and a fallback that mirrors the flag's
/// server settings lands each user in the bucket the server would choose.
///
/// Evaluations and first reads (exposures) are counted and sent in batches to
/// `POST /api/flags/events`, so the flag's screen in the dashboard shows what each variant
/// reached. Turn that off with ``FlagOptions/stats``.
///
/// Nothing is stored on the device. The bucketing subject is the identified id after
/// `identify`, else ``FlagOptions/subject`` when you pass one, else a random id that lives
/// for this launch.
public final class FlagsClient: Sendable {
    /// One count waiting to be sent.
    struct Tally: Sendable, Equatable {
        var key: String
        var variant: String?
        var enabled: Bool
        var kind: String
        var count: Int
    }

    struct State {
        var identity: String?
        var cache: [String: FlagRecord]?
        var cachedAt: TimeInterval = 0
        var ruled: [String: FlagRecord]?
        var exposed: Set<String> = []
        var tallies: [String: Tally] = [:]
        var tallyOrder: [String] = []
        var listeners: [UUID: @Sendable ([String: FlagRecord]) -> Void] = [:]
        var statsTimer: Task<Void, Never>?
        var generation = 0
    }

    let site: Int
    let host: String
    let options: FlagOptions
    let transport: any MicaforgeTransport
    let clock: MonotonicClock
    let anonymous: String
    let rules: CompiledRuleset?
    let log: Log
    private let state = Locked(State())

    /// How long counts wait before they are sent, in seconds.
    static let statsDelay: TimeInterval = 10

    /// The most entries one stats batch carries; the server reads no more.
    static let statsBatchMax = 200

    init(site: Int, host: URL, options: FlagOptions, transport: any MicaforgeTransport, clock: @escaping MonotonicClock = systemClock, log: Log = Log(enabled: false)) {
        self.site = site
        let base = host.absoluteString
        self.host = base.hasSuffix("/") ? String(base.dropLast()) : base
        self.options = options
        self.transport = transport
        self.clock = clock
        self.anonymous = "anon-" + UUID().uuidString.lowercased()
        self.rules = options.rules.map(FlagEvaluator.compile)
        self.log = log
    }

    /// The subject flags bucket on: the identified id, the app's own subject, or this
    /// launch's random id.
    var subject: String {
        state.withLock { $0.identity } ?? options.subject ?? anonymous
    }

    private var input: FlagInput {
        FlagInput(subject: subject, context: options.context, environment: options.environment)
    }

    // MARK: - Reading

    /// One flag, answered from the best source available right now. The first read of each
    /// key is counted as an exposure.
    public func flag(_ key: String) -> FlagRecord {
        // The lock is never held across `local` or `fromRules`, which read the subject
        // through the same lock.
        let ruled = fromRules()
        let cached = state.withLock { $0.cache?[key] }
        let record = cached ?? ruled?[key] ?? options.bootstrap[key] ?? local(key)
        if state.withLock({ $0.exposed.insert(key).inserted }) { count(record, kind: "exposure") }
        return record
    }

    /// Whether a flag is on for this user.
    public func isEnabled(_ key: String) -> Bool { flag(key).enabled }

    /// The variant this user gets, for a multivariate flag that is on.
    public func variant(_ key: String) -> String? {
        let rec = flag(key)
        return rec.enabled ? rec.variant : nil
    }

    /// The JSON attached to a flag, when the source that answered carries one.
    public func payload(_ key: String) -> JSONValue? { flag(key).payload }

    /// Every flag known right now, from every source, without counting exposures.
    public func all() -> [String: FlagRecord] {
        let ruled = fromRules() ?? [:]
        return state.withLock { s in
            options.bootstrap.merging(ruled) { $1 }.merging(s.cache ?? [:]) { $1 }
        }
    }

    /// This user's 0-99 bucket for a key: the server's bucket, in whole percent. No network.
    public func bucket(_ key: String) -> Int {
        FlagEvaluator.bucket(salt: key, subject: subject) / 100
    }

    /// Be told whenever the server's evaluated set changes. Keep the returned subscription;
    /// releasing it does not cancel, ``FlagSubscription/cancel()`` does.
    @discardableResult
    public func onChange(_ handler: @escaping @Sendable ([String: FlagRecord]) -> Void) -> FlagSubscription {
        let id = UUID()
        state.withLock { $0.listeners[id] = handler }
        return FlagSubscription { [weak self] in
            _ = self?.state.withLock { $0.listeners.removeValue(forKey: id) }
        }
    }

    // MARK: - Loading

    /// Fetch and cache the evaluated set. Never throws: on any failure it answers with what
    /// it already had, so a flag never flips to off because the network did.
    @discardableResult
    public func load(force: Bool = false) async -> [String: FlagRecord] {
        let now = clock()
        let (fresh, generation) = state.withLock { s -> ([String: FlagRecord]?, Int) in
            if !force, let cache = s.cache, now - s.cachedAt < options.cacheTTL { return (cache, s.generation) }
            return (nil, s.generation)
        }
        if let fresh { return fresh }
        guard site > 0, var components = URLComponents(string: host + "/api/flags/evaluate") else { return all() }
        var items = [URLQueryItem(name: "site", value: String(site)), URLQueryItem(name: "visitor", value: options.subject ?? anonymous)]
        if let identity = state.withLock({ $0.identity }) { items.append(URLQueryItem(name: "identity", value: identity)) }
        if let env = options.environment, !env.isEmpty { items.append(URLQueryItem(name: "environment", value: env)) }
        if !options.context.isEmpty { items.append(URLQueryItem(name: "context", value: JSONValue.object(options.context).jsonString())) }
        components.queryItems = items
        guard let url = components.url else { return all() }
        var request = URLRequest(url: url)
        request.setValue("application/json", forHTTPHeaderField: "Accept")

        do {
            let (data, response) = try await transport.send(request)
            guard (200..<300).contains(response.statusCode), let body = JSONValue.parse(data) else {
                log.debug("flags: the server answered \(response.statusCode); keeping the current values")
                return all()
            }
            let records = Self.records(from: body)
            let listeners = state.withLock { s -> [@Sendable ([String: FlagRecord]) -> Void]? in
                // An identify or reset while the request was in flight makes this answer
                // someone else's: drop it rather than show it.
                guard s.generation == generation else { return nil }
                s.cache = records
                s.cachedAt = clock()
                return Array(s.listeners.values)
            }
            guard let listeners else { return all() }
            for rec in records.values { count(rec, kind: "evaluation") }
            for listener in listeners { listener(records) }
            return records
        } catch {
            log.debug("flags: \(error.localizedDescription); keeping the current values")
            return all()
        }
    }

    /// The flag set in an evaluate answer: `{ data: { flags: { key: decision } } }`, with the
    /// envelope or the `flags` level optional, and an array of records accepted too.
    static func records(from body: JSONValue) -> [String: FlagRecord] {
        let data = body["data"] ?? body
        let raw = data["flags"] ?? data
        var out: [String: FlagRecord] = [:]
        switch raw {
        case .object(let map):
            for (key, value) in map { out[key] = record(key, value) }
        case .array(let list):
            for value in list { if let key = value["key"]?.stringValue { out[key] = record(key, value) } }
        default:
            break
        }
        return out
    }

    private static func record(_ key: String, _ value: JSONValue) -> FlagRecord {
        switch value {
        case .bool(let on): return FlagRecord(key: key, enabled: on)
        case .string(let variant): return FlagRecord(key: key, enabled: true, variant: variant)
        default:
            return FlagRecord(
                key: key,
                enabled: value["enabled"]?.boolValue ?? false,
                variant: value["variant"]?.stringValue,
                payload: value["payload"].flatMap { $0 == .null ? nil : $0 },
                reason: value["reason"]?.stringValue
            )
        }
    }

    // MARK: - Subject

    /// Change the identified id (or clear it with `nil`). Everything cached was decided for
    /// the previous subject, so it is dropped.
    func setIdentity(_ id: String?) {
        let next = (id?.isEmpty ?? true) ? nil : id
        state.withLock { s in
            guard s.identity != next else { return }
            s.identity = next
            s.cache = nil
            s.cachedAt = 0
            s.ruled = nil
            s.exposed.removeAll()
            s.generation += 1
        }
    }

    // MARK: - Local answers

    /// Every flag of the ruleset, decided once per subject and counted once.
    private func fromRules() -> [String: FlagRecord]? {
        guard let rules else { return nil }
        if let ruled = state.withLock({ $0.ruled }) { return ruled }
        let decided = rules.evaluateAll(input)
        var next: [String: FlagRecord] = [:]
        for key in rules.keys {
            guard let d = decided[key] else { continue }
            next[key] = FlagRecord(key: key, enabled: d.enabled, variant: d.variant, reason: d.reason.rawValue)
        }
        state.withLock { $0.ruled = next }
        for rec in next.values { count(rec, kind: "evaluation") }
        return next
    }

    /// The deterministic answer of a fallback rule, or off.
    private func local(_ key: String) -> FlagRecord {
        guard let rule = options.fallback[key] else { return FlagRecord(key: key, enabled: false) }
        let ruleset: JSONValue = [
            "flags": [[
                "key": .string(key),
                "enabled": true,
                "rollout_pct": .number(rule.rolloutPct),
                "variants": .array(rule.variants.map { ["key": .string($0.key), "weight": .number($0.weight)] }),
            ]],
        ]
        let d = FlagEvaluator.evaluate(ruleset, key: key, input: input)
        return d.enabled
            ? FlagRecord(key: key, enabled: true, variant: d.variant, reason: d.reason.rawValue)
            : FlagRecord(key: key, enabled: rule.off, reason: d.reason.rawValue)
    }

    // MARK: - Stats

    private func count(_ rec: FlagRecord, kind: String) {
        guard options.stats, site > 0 else { return }
        let id = "\(kind)\u{0}\(rec.key)\u{0}\(rec.enabled ? 1 : 0)\u{0}\(rec.variant ?? "")"
        let needsTimer = state.withLock { s -> Bool in
            if s.tallies[id] != nil {
                s.tallies[id]?.count += 1
            } else {
                s.tallies[id] = Tally(key: rec.key, variant: rec.variant, enabled: rec.enabled, kind: kind, count: 1)
                s.tallyOrder.append(id)
            }
            return s.statsTimer == nil
        }
        guard needsTimer else { return }
        let timer = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(Self.statsDelay * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await self?.flushStats()
        }
        state.withLock { s in
            if s.statsTimer == nil { s.statsTimer = timer } else { timer.cancel() }
        }
    }

    /// The counts waiting to be sent, in the order they were first counted.
    var pendingTallies: [Tally] {
        state.withLock { s in s.tallyOrder.compactMap { s.tallies[$0] } }
    }

    /// Send the counted evaluations and exposures now. Never throws; counts that fail to
    /// send are not retried, because a stats count is not worth a queue on disk.
    public func flushStats() async {
        let events = state.withLock { s -> [Tally] in
            let out = s.tallyOrder.compactMap { s.tallies[$0] }
            s.tallies.removeAll()
            s.tallyOrder.removeAll()
            s.statsTimer?.cancel()
            s.statsTimer = nil
            return out
        }
        guard !events.isEmpty, let url = URL(string: host + "/api/flags/events") else { return }
        var start = 0
        while start < events.count {
            let slice = events[start..<min(events.count, start + Self.statsBatchMax)]
            start += Self.statsBatchMax
            var body: [String: JSONValue] = [
                "site": .number(Double(site)),
                // The server files anything but a server SDK's counts under `browser`,
                // which is the client side of the flag's screen; an app is client side.
                "source": "browser",
                "events": .array(slice.map { t in
                    [
                        "key": .string(t.key),
                        "variant": t.variant.map(JSONValue.string) ?? .null,
                        "enabled": .bool(t.enabled),
                        "kind": .string(t.kind),
                        "count": .number(Double(t.count)),
                    ]
                }),
            ]
            if let env = options.environment, !env.isEmpty { body["environment"] = .string(env) }
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.httpBody = Data(JSONValue.object(body).jsonString().utf8)
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            _ = try? await transport.send(request)
        }
    }
}

/// A handle on an ``FlagsClient/onChange(_:)`` subscription.
public final class FlagSubscription: Sendable {
    private let cancelAction: @Sendable () -> Void

    init(_ cancel: @escaping @Sendable () -> Void) { cancelAction = cancel }

    /// Stop being told about changes.
    public func cancel() { cancelAction() }
}
