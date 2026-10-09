import Foundation

/// Screen paths: building them, matching them against globs, masking them.
///
/// The rules are the browser tracker's (`packages/sdk/src/core.ts`): `*` matches one path
/// segment, `**` any depth, `?` one character, and a masked path is reported as the glob
/// itself. Keeping one vocabulary means a site's `data-mask="/users/*"` and an app's
/// `mask: ["/users/*"]` say the same thing and land on the same row.
enum Paths {
    /// One piece of a compiled glob.
    enum Token: Equatable {
        case anyDepth
        case segment
        case one
        case literal(Unicode.Scalar)
    }

    static func compile(_ glob: String) -> [Token] {
        var out: [Token] = []
        let scalars = Array(glob.unicodeScalars)
        var i = 0
        while i < scalars.count {
            let c = scalars[i]
            if c == "*" {
                if i + 1 < scalars.count && scalars[i + 1] == "*" {
                    out.append(.anyDepth)
                    i += 2
                    continue
                }
                out.append(.segment)
            } else if c == "?" {
                out.append(.one)
            } else {
                out.append(.literal(c))
            }
            i += 1
        }
        return out
    }

    /// Does `path` match `glob`, whole? A backtracking match over Unicode scalars; globs
    /// and paths are short, so the worst case does not matter.
    static func matches(_ path: String, glob: String) -> Bool {
        let tokens = compile(glob)
        let text = Array(path.unicodeScalars)
        func match(_ ti: Int, _ pi: Int) -> Bool {
            if ti == tokens.count { return pi == text.count }
            switch tokens[ti] {
            case .anyDepth:
                var p = pi
                while true {
                    if match(ti + 1, p) { return true }
                    if p == text.count { return false }
                    p += 1
                }
            case .segment:
                var p = pi
                while true {
                    if match(ti + 1, p) { return true }
                    if p == text.count || text[p] == "/" { return false }
                    p += 1
                }
            case .one:
                return pi < text.count && text[pi] != "/" && match(ti + 1, pi + 1)
            case .literal(let c):
                return pi < text.count && text[pi] == c && match(ti + 1, pi + 1)
            }
        }
        return match(0, 0)
    }

    /// The first glob in `globs` that matches `path`.
    static func firstMatch(_ path: String, in globs: [String]) -> String? {
        globs.first { matches(path, glob: $0) }
    }

    /// A screen name as a path: `"Order history"` becomes `/order-history`. Letters and
    /// digits are kept (lower-cased), every run of anything else becomes one hyphen.
    static func slug(_ name: String) -> String {
        var out = ""
        var pendingHyphen = false
        for scalar in name.lowercased().unicodeScalars {
            if CharacterSet.alphanumerics.contains(scalar) {
                if pendingHyphen && !out.isEmpty { out.append("-") }
                pendingHyphen = false
                out.unicodeScalars.append(scalar)
            } else {
                pendingHyphen = true
            }
        }
        return "/" + out
    }

    /// The address one screen is reported under, after masking and query stripping, or
    /// `nil` when the path is excluded. `path` may carry a query string.
    static func screenURL(domain: String, prefix: String, path rawPath: String, mask: [String], exclude: [String], strip: StripQuery) -> String? {
        var path = rawPath.hasPrefix("/") ? rawPath : "/" + rawPath
        var query = ""
        if let q = path.firstIndex(of: "?") {
            query = String(path[path.index(after: q)...])
            path = String(path[..<q])
        }
        let trimmedPrefix = prefix.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if !trimmedPrefix.isEmpty { path = "/" + trimmedPrefix + path }
        if firstMatch(path, in: exclude) != nil { return nil }
        if let glob = firstMatch(path, in: mask) { path = glob }

        var components = URLComponents()
        components.scheme = "https"
        components.host = domain
        components.percentEncodedPath = encodePath(path)
        if !query.isEmpty {
            var items = URLComponents(string: "https://x/?" + query)?.queryItems ?? []
            switch strip {
            case .none: break
            case .all: items = []
            case .parameters(let names): items.removeAll { names.contains($0.name) }
            }
            if !items.isEmpty { components.queryItems = items }
        }
        return components.string
    }

    /// Percent-encode a path without touching the slashes or what is already encoded.
    static func encodePath(_ path: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.insert("%")
        return path.addingPercentEncoding(withAllowedCharacters: allowed) ?? path
    }

    /// `true` when `rate` keeps the device today. The rule is the tracker's: an FNV-1a
    /// bucket of today's date and the device's user agent, so every event a device records
    /// today agrees, tomorrow is drawn again, and nothing is stored.
    static func inSample(rate: Double?, seed: String) -> Bool {
        guard let rate, rate < 100 else { return true }
        return Double(fnvBucket(seed)) < rate
    }

    /// A 0-99 bucket: FNV-1a, the browser tracker's `bucket`. The tracker walks the seed by
    /// code point and hashes `charCodeAt(0)` of each, which for a character above U+FFFF is
    /// its high surrogate alone; this does the same, so the two agree on every seed.
    static func fnvBucket(_ seed: String) -> Int {
        var h: UInt32 = 0x811c9dc5
        for scalar in seed.unicodeScalars {
            let v = scalar.value
            let unit = v > 0xffff ? 0xd800 + ((v - 0x10000) >> 10) : v
            h = (h ^ unit) &* 0x01000193
        }
        return Int(h % 100)
    }
}
