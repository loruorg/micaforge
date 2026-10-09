import Foundation

/// The pieces of the outside world the engine talks to, gathered so a test can replace
/// every one of them.
struct EngineDependencies: Sendable {
    var transport: any MicaforgeTransport
    var store: any EventStore
    var optOut: any OptOutStore
    var clock: MonotonicClock = systemClock
    var retry = RetryPolicy()
    /// The device, read once at start. Hops to the main actor in the app.
    var device: @Sendable () async -> DeviceInfo
    /// Whether the app is in the foreground at start. A background launch (a push, a
    /// background fetch) must not start the engagement clock.
    var startsInForeground: @Sendable () async -> Bool = { true }
    /// Today's local date, `yyyy-MM-dd`: the day part of the sampling seed.
    var today: @Sendable () -> String = {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: Date())
    }
}

/// What the engine is told to do. Commands arrive in the order the app made the calls,
/// through one stream, so `identify` then `track` is never reordered into `track` then
/// `identify`.
enum Command: Sendable {
    case start
    case track(name: String, props: Props)
    case screen(name: String, path: String?, props: Props, onlyIfChanged: Bool)
    case identify(id: String, traits: Props)
    case setTraits(Props)
    case setProps(Props)
    case reset
    case optOut(Bool)
    case background
    case foreground
    case flush
}

/// The tracker's state and its queue, in one actor.
///
/// # Why an actor, and why a stream in front of it
///
/// Everything mutable lives here: the identity, the default properties, the current
/// screen, the engagement clock and the queue. An actor gives that state one owner with no
/// locks to forget. But actor calls made from separate tasks have no defined order, and
/// analytics depends on order (the screen a custom event happened on is the screen
/// reported before it), so the client does not call the actor directly. It yields commands
/// into an `AsyncStream`, whose `yield` is synchronous and ordered, and a single task
/// drains the stream into ``handle(_:)`` one command at a time.
///
/// # Delivery
///
/// Events are sent in batches to `POST /api/track/batch`, when ``MicaforgeOptions/flushAt``
/// are waiting, ``MicaforgeOptions/flushInterval`` seconds after the first of them, when the
/// app goes to the background, and when the app calls `flush()`. A batch is removed from the
/// queue only once the server has answered it. A failure keeps it and tries again on an
/// exponential backoff with jitter (``RetryPolicy``); the queue is mirrored to disk so
/// events recorded offline survive a relaunch, and it is bounded, oldest dropped first.
actor Engine {
    let site: Int
    let endpoint: URL
    let domain: String
    let options: MicaforgeOptions
    let deps: EngineDependencies
    let log: Log

    private(set) var queue: [QueuedEvent] = []
    private var nextSeq: UInt64 = 1
    private var device: DeviceInfo?

    private var identity = ""
    private var defaults: Props
    private var lastTraits = ""

    private var currentURL: String?
    private var currentExcluded = false
    private var sentScreen = false

    private var foreground = true
    private var engagedMs = 0
    private var visibleSince: TimeInterval?

    private var isFlushing = false
    private var flushWaiters: [CheckedContinuation<Void, Never>] = []
    private var flushTask: Task<Void, Never>?
    private var flushDue: TimeInterval?
    private(set) var attempt = 0
    private(set) var retryNotBefore: TimeInterval = 0
    private var saveTask: Task<Void, Never>?

    init(site: Int, host: URL, domain: String, options: MicaforgeOptions, deps: EngineDependencies) {
        self.site = site
        let base = host.absoluteString.hasSuffix("/") ? String(host.absoluteString.dropLast()) : host.absoluteString
        self.endpoint = URL(string: base + "/api/track/batch") ?? host
        self.domain = domain
        self.options = options
        self.deps = deps
        self.defaults = options.defaultProps
        self.log = Log(enabled: options.debug)
    }

    /// The SDK version, written to `sdk_version` on every row so a bad release can be
    /// found later.
    static let version = "0.1.0"

    // MARK: - Commands

    func handle(_ command: Command) async {
        switch command {
        case .start:
            await start()
        case .track(let name, let props):
            let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else {
                log.warning("track() needs an event name; the event was not recorded")
                return
            }
            emit(kind: "custom", name: trimmed, props: defaults.merging(props) { $1 })
        case .screen(let name, let path, let props, let onlyIfChanged):
            screen(name: name, path: path, props: props, onlyIfChanged: onlyIfChanged)
        case .identify(let id, let traits):
            identity = id
            defaults.merge(traits) { $1 }
            sendTraits(traits)
        case .setTraits(let traits):
            sendTraits(traits)
        case .setProps(let props):
            defaults.merge(props) { $1 }
        case .reset:
            identity = ""
            defaults = options.defaultProps
            lastTraits = ""
        case .optOut(let out):
            deps.optOut.set(out)
            if out {
                queue.removeAll()
                flushTask?.cancel()
                flushTask = nil
                flushDue = nil
                persistNow()
            }
        case .background:
            await enterBackground()
        case .foreground:
            foreground = true
            startClock()
            if !queue.isEmpty { scheduleFlush(after: 1) }
        case .flush:
            await performFlush(force: true)
        }
    }

    // MARK: - Lifecycle

    private func start() async {
        device = await deps.device()
        foreground = await deps.startsInForeground()
        queue = Array(deps.store.load().suffix(max(1, options.maxQueueSize)))
        if deps.optOut.isOptedOut { queue.removeAll() }
        nextSeq = (queue.map(\.seq).max() ?? 0) + 1
        startClock()
        if !queue.isEmpty {
            log.debug("\(queue.count) events waiting from an earlier launch")
            scheduleFlush(after: 1)
        }
    }

    /// The app left the screen: report the engagement it gathered, save the queue, and
    /// try to send it while the system still lets the app run.
    private func enterBackground() async {
        stopClock()
        foreground = false
        if engagedMs > 0 {
            emit(kind: "engagement", url: currentURL, d: engagedMs)
            engagedMs = 0
        }
        persistNow()
        await performFlush(force: true)
    }

    private func startClock() {
        if options.trackLifecycle && foreground && visibleSince == nil { visibleSince = deps.clock() }
    }

    private func stopClock() {
        if let since = visibleSince {
            engagedMs += max(0, Int(((deps.clock() - since) * 1000).rounded()))
            visibleSince = nil
        }
    }

    // MARK: - Recording

    private func screen(name: String, path: String?, props: Props, onlyIfChanged: Bool) {
        let rawPath = path ?? Paths.slug(name)
        let url = Paths.screenURL(domain: domain, prefix: options.pathPrefix, path: rawPath, mask: options.mask, exclude: options.exclude, strip: options.stripQuery)
        if onlyIfChanged && sentScreen && url == currentURL && (url != nil || currentExcluded) { return }
        stopClock()
        // `d` describes the screen being LEFT, so the first screen of a launch has nothing
        // to report, exactly as the first pageview of a page has nothing in the browser.
        let d = sentScreen ? engagedMs : 0
        sentScreen = true
        engagedMs = 0
        currentURL = url
        currentExcluded = url == nil
        startClock()
        guard let url else {
            log.debug("screen \(rawPath) is excluded; not sent")
            return
        }
        emit(kind: "pageview", url: url, props: defaults.merging(props) { $1 }, d: d, title: name)
    }

    /// One `identify` event per change of id or traits, so the server can link today's
    /// visitor digest to the id and keep the trait history. A repeat of the same call (a
    /// view's `onAppear` running again) sends nothing.
    private func sendTraits(_ traits: Props) {
        let key = identity + "\u{0}" + (traits.isEmpty ? "0" : JSONValue.object(traits).jsonString())
        guard (!identity.isEmpty || !traits.isEmpty) && key != lastTraits else { return }
        lastTraits = key
        emit(kind: "identify", props: traits)
    }

    /// Why an event is not recorded, or `nil` when it is.
    private func gate() -> String? {
        if !options.enabled { return "the client is disabled" }
        if site <= 0 { return "the site key is not a site id" }
        if deps.optOut.isOptedOut { return "this device opted out" }
        let ua = device?.userAgent ?? ""
        if !Paths.inSample(rate: options.sampleRate, seed: deps.today() + ua) { return "this device is outside today's sample" }
        return nil
    }

    /// Build one payload and queue it. The shape is CONTRACT.md section 4 plus `title`.
    private func emit(kind: String, name: String? = nil, url: String? = nil, props: Props = [:], d: Int = 0, title: String? = nil) {
        if let reason = gate() {
            log.debug("\(kind) \(name ?? "") not recorded: \(reason)")
            return
        }
        if currentExcluded && url == nil {
            log.debug("\(kind) \(name ?? "") not recorded: the current screen is excluded")
            return
        }
        let target = url ?? currentURL
            ?? Paths.screenURL(domain: domain, prefix: options.pathPrefix, path: "/", mask: options.mask, exclude: options.exclude, strip: options.stripQuery)
        guard let target else { return }

        var payload = TrackPayload(s: site, t: kind, u: target, v: Self.version)
        payload.n = name
        payload.w = device?.screenWidth
        payload.h = device?.screenHeight
        payload.l = device?.language
        payload.tz = device?.timeZone
        payload.d = d > 0 ? d : nil
        payload.id = identity.isEmpty ? nil : identity
        payload.title = title
        if !props.isEmpty {
            payload.p = props
            let size = JSONValue.object(props).jsonString().utf8.count
            if size > 8 * 1024 {
                log.warning("\(kind) \(name ?? ""): properties are \(size) bytes; the server drops a property set over 8 KB")
            }
        }
        if let rate = options.sampleRate, rate < 100 { payload.sr = min(99, max(1, Int(rate.rounded()))) }

        queue.append(QueuedEvent(seq: nextSeq, payload: payload))
        nextSeq += 1
        if queue.count > options.maxQueueSize {
            let over = queue.count - options.maxQueueSize
            queue.removeFirst(over)
            log.warning("the queue is full; dropped the \(over) oldest events")
        }
        log.debug("queued \(kind) \(name ?? "") \(target)")
        persistSoon()

        if queue.count >= max(1, options.flushAt) && deps.clock() >= retryNotBefore {
            Task { await self.performFlush(force: false) }
        } else {
            scheduleFlush(after: options.flushInterval)
        }
    }

    // MARK: - Delivery

    /// Send everything that is waiting, a batch at a time, until the queue is empty or a
    /// batch fails. With `force` the backoff is ignored (the app asked, or is going away).
    func performFlush(force: Bool) async {
        while isFlushing {
            await withCheckedContinuation { flushWaiters.append($0) }
        }
        guard !queue.isEmpty, force || deps.clock() >= retryNotBefore else { return }
        isFlushing = true
        flushTask?.cancel()
        flushTask = nil
        flushDue = nil
        defer {
            isFlushing = false
            let waiters = flushWaiters
            flushWaiters.removeAll()
            for w in waiters { w.resume() }
        }

        while !queue.isEmpty {
            let batch = Array(queue.prefix(max(1, min(options.maxBatchSize, 200))))
            switch await deliver(batch.map(\.payload)) {
            case .done(let dropped):
                let sent = Set(batch.map(\.seq))
                queue.removeAll { sent.contains($0.seq) }
                attempt = 0
                retryNotBefore = 0
                if dropped {
                    log.warning("the server refused \(batch.count) events; they were dropped. A 403 means the site id is unknown or the screen hostname \(domain) is not one of the site's domains (set options.domain)")
                } else {
                    log.debug("sent \(batch.count) events")
                }
            case .retry(let after):
                attempt += 1
                let delay = deps.retry.delay(attempt: attempt, retryAfter: after)
                retryNotBefore = deps.clock() + delay
                log.debug("send failed; \(queue.count) events kept, next try in \(Int(delay.rounded())) s")
                scheduleFlush(after: delay)
                persistSoon()
                return
            }
        }
        persistSoon()
    }

    private func deliver(_ payloads: [TrackPayload]) async -> SendOutcome {
        var request = URLRequest(url: endpoint)
        request.httpMethod = "POST"
        request.httpBody = try? JSONEncoder().encode(payloads)
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        for (k, v) in device?.headers ?? [:] { request.setValue(v, forHTTPHeaderField: k) }
        do {
            let (_, response) = try await deps.transport.send(request)
            let retryAfter = response.value(forHTTPHeaderField: "Retry-After").flatMap(TimeInterval.init)
            return SendOutcome.of(status: response.statusCode, retryAfter: retryAfter)
        } catch {
            return .retry(after: nil)
        }
    }

    private func scheduleFlush(after delay: TimeInterval) {
        let due = deps.clock() + max(0, delay)
        if flushTask != nil, let existing = flushDue, existing <= due { return }
        flushTask?.cancel()
        flushDue = due
        flushTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(max(0, delay) * 1_000_000_000))
            guard !Task.isCancelled else { return }
            await self?.timerFired()
        }
    }

    private func timerFired() async {
        flushTask = nil
        flushDue = nil
        await performFlush(force: false)
    }

    // MARK: - Persistence

    /// Mirror the queue to disk soon, coalescing a burst of events into one write.
    private func persistSoon() {
        guard options.persistQueue, saveTask == nil else { return }
        saveTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 500_000_000)
            await self?.persistNow()
        }
    }

    func persistNow() {
        saveTask?.cancel()
        saveTask = nil
        guard options.persistQueue else { return }
        deps.store.save(queue)
    }
}
