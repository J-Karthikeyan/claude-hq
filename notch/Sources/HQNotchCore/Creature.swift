import CryptoKit
import Foundation

/// The Pokémon a Claude session wears: the same one Claude HQ shows on its live card.
///
/// Source of truth is HQ's `/api/sessions` (`creature.species/stage/shiny`). When HQ is not
/// running, `Creature.local` reproduces `dashboard.py`'s `creature_for()` (sha256 species,
/// prompt-count stage, per-install salted shiny). The sprite form follows the evolution line
/// exactly like `pokeDexFor()` in `ui/app/06-trainer-packs.js`, including the hashed
/// branch default (Eevee/Ralts/Slowpoke). Two browser-only extras are NOT mirrored: manual
/// branch picks and the per-species high-water floor (both live in the browser's
/// localStorage), so a session can show an earlier form here than in a browser that has
/// already seen that species evolve further.
public struct Creature: Equatable, Sendable {
    public var species: Int      // 0..47
    public var stage: Int        // 0..4
    public var shiny: Bool

    public init(species: Int, stage: Int, shiny: Bool) {
        self.species = ((species % 48) + 48) % 48
        self.stage = max(0, min(4, stage))
        self.shiny = shiny
    }

    /// HQ's evolution thresholds on promptCount (dashboard.py STAGE_THRESHOLDS).
    public static let stageThresholds = [0, 3, 12, 30, 70]

    public static func stage(forPrompts count: Int) -> Int {
        var s = 0
        for (i, th) in stageThresholds.enumerated() where count >= th { s = i }
        return s
    }

    /// `int(sha256(text).hexdigest(), 16) % m`, without big integers.
    public static func sha256Mod(_ text: String, _ m: Int) -> Int {
        var r = 0
        for b in SHA256.hash(data: Data(text.utf8)) { r = (r * 256 + Int(b)) % m }
        return r
    }

    /// dashboard.py `shiny_for_species`: 1/6 roll, else a second 1/10 roll (total 1/4).
    public static func shiny(species: Int, salt: String) -> Bool {
        let sp = ((species % 48) + 48) % 48
        if sha256Mod("nymonster:shiny:\(salt):\(sp)", 6) == 0 { return true }
        return sha256Mod("nymonster:shiny2:\(salt):\(sp)", 10) == 0
    }

    /// Fallback when HQ is unreachable. `salt` is HQ's `.dex-seed` (nil = never shiny).
    public static func local(sessionId: String, prompts: Int, salt: String?) -> Creature {
        let sp = sha256Mod(sessionId, 48)
        return Creature(species: sp, stage: stage(forPrompts: prompts),
                        shiny: salt.map { shiny(species: sp, salt: $0) } ?? false)
    }

    /// JS `hashStr`: 32-bit FNV-1a over UTF-16 code units.
    public static func hashStr(_ s: String) -> UInt32 {
        var h: UInt32 = 2_166_136_261
        for u in s.utf16 { h ^= UInt32(u); h = h &* 16_777_619 }
        return h
    }

    public var line: [Int] { PokeData.evo[species] }

    /// JS `pokeEvoPos`: Math.round(stage/4 * (len-1)), clamped.
    public var linePosition: Int {
        let n = line.count
        let p = (Double(stage) / 4.0 * Double(n - 1) + 0.5).rounded(.down)
        return max(0, min(n - 1, Int(p)))
    }

    /// National Dex number of the form to draw (JS `pokeDexFor`, branch default by session).
    public func dex(sessionId: String) -> Int {
        let pos = linePosition
        guard pos == line.count - 1 else { return line[pos] }
        if let opts = PokeData.branch[species], !opts.isEmpty {
            return opts[Int(Creature.hashStr(sessionId) % UInt32(opts.count))]
        }
        return line[line.count - 1]
    }

    public func name(sessionId: String) -> String {
        PokeData.names[dex(sessionId: sessionId)] ?? PokeData.real[species]
    }

    public static let animatedBase =
        "https://cdn.jsdelivr.net/gh/PokeAPI/sprites@master/sprites/pokemon/versions/generation-v/black-white/animated/"
    public static let staticBase = "https://cdn.jsdelivr.net/gh/PokeAPI/sprites@master/sprites/pokemon/"

    /// Sprite URLs in fallback order (same hotlinks HQ uses; only the dex number is sent).
    public func spriteURLs(sessionId: String) -> [URL] {
        let d = dex(sessionId: sessionId), sh = shiny ? "shiny/" : ""
        return ["\(Creature.animatedBase)\(sh)\(d).gif", "\(Creature.staticBase)\(sh)\(d).png"]
            .compactMap(URL.init(string:))
    }
}
