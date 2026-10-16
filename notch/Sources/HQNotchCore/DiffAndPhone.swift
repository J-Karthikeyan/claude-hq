import Foundation

// MARK: the diff view

/// One line of the notch's diff view.
public enum DiffLine: Equatable, Sendable {
    case context(String)
    case removed(String)
    case added(String)
    /// Unchanged lines left out (how many), or a separator between two edits (0).
    case gap(Int)
    /// The hook cut the change short, or there were more lines than the view shows.
    case more
}

public enum DiffBuilder {
    /// Lines of unchanged text kept around a change.
    public static let context = 2
    /// The view never shows more than this many lines.
    public static let maxLines = 400

    static func split(_ s: String) -> [String] {
        if s.isEmpty { return [] }
        var lines = s.components(separatedBy: "\n")
        if lines.last == "" { lines.removeLast() }       // a trailing newline is not an extra line
        return lines
    }

    /// One edit: the lines both sides share at the start and the end stay as context (two each
    /// way at most), the rest is what was removed and what was added. Not a full diff, and it
    /// does not need to be: an Edit's old and new strings are already just the changed region.
    public static func lines(old: String, new: String) -> [DiffLine] {
        let a = split(old), b = split(new)
        var p = 0
        while p < a.count, p < b.count, a[p] == b[p] { p += 1 }
        var q = 0
        while q < a.count - p, q < b.count - p, a[a.count - 1 - q] == b[b.count - 1 - q] { q += 1 }
        var out: [DiffLine] = []
        if p > context { out.append(.gap(p - context)) }
        out += a[max(0, p - context)..<p].map { DiffLine.context($0) }
        out += a[p..<(a.count - q)].map { DiffLine.removed($0) }
        out += b[p..<(b.count - q)].map { DiffLine.added($0) }
        out += a[(a.count - q)..<min(a.count, a.count - q + context)].map { DiffLine.context($0) }
        if q > context { out.append(.gap(q - context)) }
        return out
    }

    /// Every edit of a change, separated, capped at `maxLines`.
    public static func lines(_ diff: EditDiff) -> [DiffLine] {
        var out: [DiffLine] = []
        for (i, e) in diff.edits.enumerated() {
            if i > 0 { out.append(.gap(0)) }
            out += lines(old: e.old, new: e.new)
        }
        if out.count > maxLines { out = Array(out.prefix(maxLines)); out.append(.more) }
        else if diff.truncated { out.append(.more) }
        return out
    }
}

// MARK: phone alerts

/// What can be pushed to your phone. The text is fixed: no prompt, title, folder, command,
/// file path or session id ever goes into it (see notch/README.md, Privacy).
public enum PhoneAlert: String, CaseIterable, Sendable {
    case needsYou, finished, failed

    public var title: String { "Claude HQ" }
    public var body: String {
        switch self {
        case .needsYou: "A Claude session needs you"
        case .finished: "A Claude session finished"
        case .failed: "A Claude session had an error"
        }
    }
    /// ntfy tags (shown as emoji) and priority.
    public var tags: String {
        switch self { case .needsYou: "bell"; case .finished: "white_check_mark"; case .failed: "warning" }
    }
    public var priority: String { self == .needsYou ? "high" : "default" }

    public static func from(_ e: StoreEffect) -> PhoneAlert {
        switch e { case .needsYou: .needsYou; case .finished: .finished; case .failed: .failed }
    }
}

/// At most one push of each kind per `gap` seconds, so six sessions finishing together buzz once.
public struct PhoneDebounce: Sendable {
    public static let gap: TimeInterval = 30
    private var last: [PhoneAlert: Date] = [:]
    public init() {}

    public mutating func allow(_ a: PhoneAlert, now: Date = Date()) -> Bool {
        if let t = last[a], now.timeIntervalSince(t) < Self.gap { return false }
        last[a] = now
        return true
    }
}

/// The ntfy topic: "hq-" and 20 random base32 letters (100 bits), made once per Mac. Anyone
/// who knows a topic can read it, so it is random, not chosen; it is the only thing that ties
/// the pushes to you.
public enum PhoneTopic {
    static let alphabet = Array("abcdefghijklmnopqrstuvwxyz234567")

    /// `bytes`: 20 random bytes (the app uses SecRandomCopyBytes). 256 is a multiple of 32, so
    /// `byte % 32` is unbiased.
    public static func make(_ bytes: [UInt8]) -> String? {
        guard bytes.count >= 20 else { return nil }
        return "hq-" + String(bytes.prefix(20).map { alphabet[Int($0) % 32] })
    }

    public static func isValid(_ s: String) -> Bool {
        s.range(of: "^hq-[a-z2-7]{20}$", options: .regularExpression) != nil
    }

    /// Where a push goes: `server` (https, or http for a server on your own network) + "/" + topic.
    /// nil for anything else (no other scheme, no credentials, query or fragment in the URL).
    public static func url(server: String, topic: String) -> URL? {
        guard isValid(topic), var c = URLComponents(string: server.trimmingCharacters(in: .whitespaces)),
              let scheme = c.scheme?.lowercased(), scheme == "https" || scheme == "http",
              let host = c.host, !host.isEmpty, c.user == nil, c.password == nil, c.query == nil, c.fragment == nil
        else { return nil }
        var path = c.path
        while path.hasSuffix("/") { path.removeLast() }
        c.path = path + "/" + topic
        return c.url
    }
}
