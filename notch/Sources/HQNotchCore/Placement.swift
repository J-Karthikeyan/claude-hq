import Foundation

/// Where the pill sits. Other notch apps (Sapphire, NotchNook, ...) draw in the notch too and
/// open on hover, so two of them fight over the same spot: `.auto` steps aside, to the left
/// of the notch, while one of them is running.
public enum Placement: String, CaseIterable, Sendable {
    case auto, notch, left, right

    public var label: String {
        switch self { case .auto: "Auto"; case .notch: "In the notch"; case .left: "Left of it"; case .right: "Right of it" }
    }

    /// Where to draw, given whether another notch app is running.
    public func resolved(otherNotchApp: Bool) -> Placement {
        self == .auto ? (otherNotchApp ? .left : .notch) : self
    }
}

/// Apps known to live in the notch. Matched by bundle id, and by name for builds whose id we
/// do not know.
public enum NotchNeighbours {
    public static let bundleIds: Set<String> = [
        "com.cshariq.sapphire",                // Sapphire
        "com.lo.cafe.NotchNook",               // NotchNook
        "theboringteam.boringnotch",           // Boring Notch
        "com.henrikruscon.Alcove",             // Alcove
        "fr.louisraille.NotchBuddy",           // Coucou
    ]
    public static let names: Set<String> = ["sapphire", "notchnook", "boringnotch", "boring notch", "alcove",
                                            "dynamiclake", "dynamic lake", "coucou", "notchdrop", "mediamate"]

    /// The first running app that is another notch app (its name, for Settings), else nil.
    public static func running(_ apps: [(bundleId: String?, name: String?)], own: String?) -> String? {
        for a in apps where a.bundleId == nil || a.bundleId != own {
            if let b = a.bundleId, bundleIds.contains(b) { return a.name ?? b }
            if let n = a.name?.lowercased(), names.contains(n) { return a.name }
        }
        return nil
    }
}
