import CryptoKit
import Foundation

/// The Pokémon a Claude session wears: the same one Claude HQ shows on its live card.
///
/// Source of truth is HQ's `/api/sessions` (`creature.species/stage/shiny`). When HQ is not
/// running, `Creature.local` reproduces `dashboard.py`'s `creature_for()` (sha256 species,
/// prompt-count stage, per-install salted shiny). The sprite form follows the evolution line
/// exactly like `pokeDexFor()` in `ui/app/06-trainer-packs.js`, including the hashed
/// branch default (Eevee/Ralts/Slowpoke). A branch or Mega you picked in HQ comes back from
/// `/api/sessions` as `creature.evo` (`branchDex`, `megaSlug`) and wins over the default; the
/// species floor comes from `/api/pokedex` (`floored`).
public struct Creature: Equatable, Sendable {
    public var species: Int      // 0..47
    public var stage: Int        // 0..4
    public var shiny: Bool
    /// Picked in HQ (`creature.evo`): the final form to become, and which Mega. Nil = HQ's
    /// hashed default; a value that is not valid for this species is ignored.
    public var branchDex: Int?
    public var megaSlug: String?

    public init(species: Int, stage: Int, shiny: Bool, branchDex: Int? = nil, megaSlug: String? = nil) {
        self.species = ((species % 48) + 48) % 48
        self.stage = max(0, min(4, stage))
        self.shiny = shiny
        self.branchDex = branchDex
        self.megaSlug = megaSlug
    }

    /// HQ's evolution floor: a species never shows an earlier form than the highest stage
    /// any of your sessions reached with it (HQ's `/api/pokedex` `maxStage`, which the
    /// browser also floors live cards to). `floor` maps species index -> stage.
    public func floored(_ floor: [Int: Int]) -> Creature {
        Creature(species: species, stage: max(stage, floor[species] ?? 0), shiny: shiny,
                 branchDex: branchDex, megaSlug: megaSlug)
    }

    /// Parse HQ's `/api/pokedex` into species -> maxStage.
    public static func parseFloor(_ data: Data) -> [Int: Int] {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let list = root["species"] as? [[String: Any]] else { return [:] }
        var out: [Int: Int] = [:]
        for o in list {
            if let sp = o["species"] as? Int, let m = o["maxStage"] as? Int, m > 0 {
                out[((sp % 48) + 48) % 48] = max(0, min(4, m))
            }
        }
        return out
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
            if let b = branchDex, opts.contains(b) { return b }          // picked in HQ
            return opts[Int(Creature.hashStr(sessionId) % UInt32(opts.count))]
        }
        return line[line.count - 1]
    }

    public func name(sessionId: String, pack: SpritePack = .gen5) -> String {
        if let m = megaForm(sessionId: sessionId, pack: pack) { return m.name }
        return PokeData.names[dex(sessionId: sessionId)] ?? PokeData.real[species]
    }

    /// JS `pokeMegaForm`: 3D pack only, Apex only, on the (branch-aware) final form; with
    /// two Megas (Charizard X/Y) the session id picks one, as HQ does by default.
    public func megaForm(sessionId: String, pack: SpritePack) -> MegaForm? {
        guard pack == .threeD, stage >= 4, let fs = PokeData.mega[dex(sessionId: sessionId)], !fs.isEmpty else { return nil }
        if let m = megaSlug, let f = fs.first(where: { $0.slug == m }) { return f }   // picked in HQ
        return fs.count > 1 ? fs[Int(Creature.hashStr(sessionId) % UInt32(fs.count))] : fs[0]
    }

    public static let animatedBase =
        "https://cdn.jsdelivr.net/gh/PokeAPI/sprites@master/sprites/pokemon/versions/generation-v/black-white/animated/"
    public static let staticBase = "https://cdn.jsdelivr.net/gh/PokeAPI/sprites@master/sprites/pokemon/"

    static let xyBase = "https://www.pkparaiso.com/imagenes/xy/sprites/animados"
    static let orasBase = "https://www.pkparaiso.com/imagenes/rubi-omega-zafiro-alfa/sprites/animados"

    /// Sprite URLs in fallback order: the same hotlinks HQ uses (JS `pokeImgFor` / `pokeErr`).
    /// They carry only a Dex number or a species slug, never session data.
    /// gen5: animated gif -> static png. 3D: X/Y (or ORAS for its Megas) animated gif -> HOME
    /// still png; the 2D gif after that, so a blocked 3D host still shows a Pokémon.
    public func spriteURLs(sessionId: String, pack: SpritePack = .gen5) -> [URL] {
        let d = dex(sessionId: sessionId), sh = shiny ? "shiny/" : ""
        let gen5 = ["\(Creature.animatedBase)\(sh)\(d).gif", "\(Creature.staticBase)\(sh)\(d).png"]
        guard pack == .threeD else { return gen5.compactMap(URL.init(string:)) }
        let mega = megaForm(sessionId: sessionId, pack: pack)
        let slug = mega?.slug ?? (PokeData.names[d] ?? PokeData.real[species]).lowercased()
            .filter { ("a"..."z").contains($0) || ("0"..."9").contains($0) }
        let dir = (mega?.oras ?? false) ? Creature.orasBase : Creature.xyBase
        return ["\(dir)\(shiny ? "-shiny" : "")/\(slug).gif", "\(Creature.staticBase)other/home/\(sh)\(d).png",
                gen5[0]].compactMap(URL.init(string:))
    }
}

/// Which sprites the notch draws. `.matchHQ` resolves to HQ's own `creaturePack`.
public enum SpritePack: String, CaseIterable, Sendable {
    case gen5 = "2d", threeD = "3d"

    /// HQ's `creaturePack` setting -> a pack the notch can draw (only pokemon3d is 3D).
    public static func fromHQ(_ creaturePack: String?) -> SpritePack {
        creaturePack == "pokemon3d" ? .threeD : .gen5
    }
}
