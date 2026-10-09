import Foundation

/// Where queued events wait while the device is offline.
///
/// A protocol so tests can keep the queue in memory and assert on it; the default is
/// ``FileEventStore``.
protocol EventStore: Sendable {
    func load() -> [QueuedEvent]
    func save(_ events: [QueuedEvent])
}

/// The queue as one JSON file in Application Support.
///
/// - **Protected.** On iOS, tvOS, watchOS and visionOS the file is written with
///   `completeUntilFirstUserAuthentication`: unreadable after a reboot until the device is
///   first unlocked, readable afterwards, so a flush that runs while the app is in the
///   background (locked phone, app still alive) can still read it.
/// - **Not backed up.** The file is excluded from iCloud and device backups. A queue of
///   analytics events restored onto a new phone months later would be delivered then, and
///   the server would stamp it with that day.
/// - **Per app and per site.** On macOS, Application Support is shared by every app of the
///   user, so the path carries the bundle identifier as well as the site id.
/// - **Atomic.** Written to a temporary file and moved into place, so a crash mid-write
///   leaves the previous queue rather than half a file.
struct FileEventStore: EventStore {
    let url: URL

    init(site: Int, directory: URL? = nil) {
        let base = directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        let app = Bundle.main.bundleIdentifier ?? "app"
        url = base
            .appendingPathComponent(app, isDirectory: true)
            .appendingPathComponent("Micaforge", isDirectory: true)
            .appendingPathComponent("queue-\(site).json")
    }

    func load() -> [QueuedEvent] {
        guard let data = try? Data(contentsOf: url) else { return [] }
        return (try? JSONDecoder().decode([QueuedEvent].self, from: data)) ?? []
    }

    func save(_ events: [QueuedEvent]) {
        let fm = FileManager.default
        if events.isEmpty {
            try? fm.removeItem(at: url)
            return
        }
        guard let data = try? JSONEncoder().encode(events) else { return }
        let dir = url.deletingLastPathComponent()
        try? fm.createDirectory(at: dir, withIntermediateDirectories: true)
        var options: Data.WritingOptions = [.atomic]
        #if os(iOS) || os(tvOS) || os(watchOS) || os(visionOS)
        options.insert(.completeFileProtectionUntilFirstUserAuthentication)
        #endif
        do {
            try data.write(to: url, options: options)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            var target = url
            try? target.setResourceValues(values)
        } catch {
            // A full disk or a sandbox refusal: the events stay in memory and are still
            // sent while the app runs. Losing the offline copy is the right failure.
        }
    }
}

/// The queue in memory only, for `persistQueue: false` and for tests.
final class MemoryEventStore: EventStore, @unchecked Sendable {
    private let lock = NSLock()
    private var events: [QueuedEvent] = []

    init(_ events: [QueuedEvent] = []) { self.events = events }

    func load() -> [QueuedEvent] {
        lock.lock()
        defer { lock.unlock() }
        return events
    }

    func save(_ events: [QueuedEvent]) {
        lock.lock()
        self.events = events
        lock.unlock()
    }
}

/// The one preference the SDK keeps: whether this device opted out. Same key as the
/// browser tracker (`mf-opt-out`), in `UserDefaults`.
protocol OptOutStore: Sendable {
    var isOptedOut: Bool { get }
    func set(_ optedOut: Bool)
}

/// The opt-out in a `UserDefaults` suite. Holds the suite name rather than the defaults
/// object, so the store is a plain value that crosses threads freely.
struct DefaultsOptOutStore: OptOutStore {
    static let key = "mf-opt-out"
    let suiteName: String?

    private var defaults: UserDefaults { suiteName.flatMap { UserDefaults(suiteName: $0) } ?? .standard }

    var isOptedOut: Bool { defaults.bool(forKey: Self.key) }

    func set(_ optedOut: Bool) {
        if optedOut {
            defaults.set(true, forKey: Self.key)
        } else {
            defaults.removeObject(forKey: Self.key)
        }
    }
}
