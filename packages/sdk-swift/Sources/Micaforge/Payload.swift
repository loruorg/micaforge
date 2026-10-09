import Foundation

/// One hit, in the shape `POST /api/track/batch` reads: the browser payload of
/// `docs/CONTRACT.md` section 4, with the one extra field the server accepts from SDKs that
/// have room for it (`title`).
///
/// The keys are one or two characters because that is the contract, not because an app
/// is short of bytes. Nothing here may be renamed or added to without the contract changing
/// first. Empty and zero values are left out rather than sent, because every column they
/// map to carries a DEFAULT.
///
/// There is no time field. The collection routes are unauthenticated, so the server stamps
/// every hit with its own receive time rather than trusting a clock it cannot check. An
/// event recorded offline is therefore counted when it is finally delivered.
struct TrackPayload: Codable, Sendable, Equatable {
    /// Site id.
    var s: Int
    /// Event kind: `pageview`, `custom`, `engagement` or `identify` from this SDK.
    var t: String
    /// Event name, custom events only.
    var n: String?
    /// The screen's address: `https://<domain><path>`.
    var u: String
    /// Screen width and height, in points.
    var w: Int?
    var h: Int?
    /// Preferred language, BCP 47 (`en-GB`).
    var l: String?
    /// IANA time zone.
    var tz: String?
    /// Foreground milliseconds on the previous screen; absent on the first screen.
    var d: Int?
    /// Properties.
    var p: Props?
    /// The id from `identify`.
    var id: String?
    /// SDK version.
    var v: String
    /// The sample rate, sent only by a device inside a sample.
    var sr: Int?
    /// The screen's name, stored as the page title.
    var title: String?
}

/// A payload waiting in the queue, with a sequence number so a batch can be removed after
/// it is delivered even if newer events arrived while it was in flight.
struct QueuedEvent: Codable, Sendable, Equatable {
    var seq: UInt64
    var payload: TrackPayload
}
