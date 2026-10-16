import AppKit
import HQNotchCore
import SwiftUI

/// Notch colours. The pill sits in the notch, so it is always dark whatever the system
/// appearance; every colour used by the views comes from here.
enum Theme {
    static let bg = Color.black
    static let card = Color.white.opacity(0.07)
    static let cardHover = Color.white.opacity(0.12)
    static let text = Color.white
    static let subtle = Color.white.opacity(0.58)
    static let faint = Color.white.opacity(0.34)
    static let alert = Color(red: 1.0, green: 0.62, blue: 0.2)
    static let alertText = Color.black
    static let ok = Color(red: 0.35, green: 0.85, blue: 0.5)
    static let danger = Color(red: 1.0, green: 0.38, blue: 0.38)
    static let working = Color(red: 0.42, green: 0.68, blue: 1.0)
    static let buttonText = Color.black

    static func color(_ s: SessionStatus) -> Color {
        switch s {
        case .working: working
        case .needs: alert
        case .done: ok
        case .error: danger
        case .idle: faint
        }
    }
}

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var store = SessionStore()
    @Published var expanded = false
    @Published var hqReachable = false
    /// Measured size of the expanded panel (drives the hover hit area). Not @Published:
    /// layout writes it, and it must not trigger another layout pass.
    var expandedSize = CGSize(width: NotchRoot.expandedWidth, height: 200)
    // Settings: @Published (so views refresh) and persisted to UserDefaults.
    @Published var muted = UserDefaults.standard.bool(forKey: "hqnotch.muted") {
        didSet { UserDefaults.standard.set(muted, forKey: "hqnotch.muted") }
    }
    @Published var showIdle = UserDefaults.standard.object(forKey: "hqnotch.showIdle") as? Bool ?? true {
        didSet { UserDefaults.standard.set(showIdle, forKey: "hqnotch.showIdle") }
    }
    @Published var approvals = UserDefaults.standard.object(forKey: "hqnotch.approvals") as? Bool ?? true {
        didSet { UserDefaults.standard.set(approvals, forKey: "hqnotch.approvals") }
    }
    /// HQ's address; only 127.0.0.1 / localhost is ever used.
    let hqURL = UserDefaults.standard.string(forKey: "hqnotch.hqURL") ?? "http://127.0.0.1:8765"

    let server = HookServer()
    private var salt: String?
    private var pollTask: Task<Void, Never>?
    private var tickTimer: Timer?
    /// Sessions whose question was just handed to the terminal from the `--ask` hook. If
    /// Claude Code then routes the same question through PermissionRequest, pass it on
    /// instead of asking a second time.
    private var passedQuestion: [String: Date] = [:]
    /// Bumped on every needs-you event so the panel can pop open.
    @Published private(set) var attention = 0

    init() {
        salt = Self.readSalt()
        server.onMessage = { [weak self] m, id in self?.receive(m, id) }
        server.onGone = { [weak self] id in self?.store.resolvePending(id: id) }
        do { try server.start() } catch { NSLog("HQNotch: socket failed: \(error)") }
        tickTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.store.tick() }
        }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.pollHQ()
                let secs: Double = (self?.expanded ?? false) ? 4 : 12
                try? await Task.sleep(for: .seconds(secs))
            }
        }
    }

    var visibleSessions: [Session] {
        store.ordered.filter { showIdle || $0.status != .idle }
    }

    func creature(_ s: Session) -> Creature {
        (s.creature ?? Creature.local(sessionId: s.id, prompts: s.promptCount, salt: salt)).floored(stageFloor)
    }

    /// Species -> highest stage reached (HQ's Pokédex). Kept in UserDefaults so the floor
    /// still applies when HQ is not running.
    @Published private(set) var stageFloor: [Int: Int] = {
        let raw = UserDefaults.standard.dictionary(forKey: "hqnotch.stageFloor") as? [String: Int] ?? [:]
        return Dictionary(uniqueKeysWithValues: raw.compactMap { k, v in Int(k).map { ($0, v) } })
    }()
    private var floorFetchedAt: Date?

    private func pollFloor(_ base: URL) async {
        if let t = floorFetchedAt, Date().timeIntervalSince(t) < 60 { return }   // Pokédex scans every transcript
        var req = URLRequest(url: base.appendingPathComponent("api/pokedex"), timeoutInterval: 10)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        guard let (data, resp) = try? await URLSession.shared.data(for: req),
              (resp as? HTTPURLResponse)?.statusCode == 200 else { return }
        floorFetchedAt = Date()
        let fresh = Creature.parseFloor(data)
        guard !fresh.isEmpty else { return }
        // Never lower a floor (HQ's browser floor is monotonic too).
        let merged = stageFloor.merging(fresh) { max($0, $1) }
        if merged != stageFloor {
            stageFloor = merged
            UserDefaults.standard.set(Dictionary(uniqueKeysWithValues: merged.map { (String($0.key), $0.value) }),
                                      forKey: "hqnotch.stageFloor")
        }
    }

    // MARK: hook events

    private func receive(_ m: HookMessage, _ id: Int?) {
        let fx: [StoreEffect]
        if let id {
            guard approvals else { server.reply(id: id, ["decision": "pass"]); store.apply(m); return }
            if m.event == "PermissionRequest", m.question == true,
               let t = passedQuestion.removeValue(forKey: m.sessionId), Date().timeIntervalSince(t) < 60 {
                server.reply(id: id, ["decision": "pass"])
                store.apply(m)
                return
            }
            fx = store.addPending(id: id, m)
            // Debug aid: answer like a click on the card's button would, to test the socket path.
            if let d = ProcessInfo.processInfo.environment["HQNOTCH_DEBUG_REPLY"],
               let r = store.pending.first(where: { $0.id == id }) {
                DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
                    if d == "allow" { self?.allow(r) } else if d == "always" { self?.allow(r, always: true) }
                    else if d == "deny" { self?.deny(r) }
                    else if d == "pass" { self?.passToTerminal(r) }
                    else if d == "answer", !r.questions.isEmpty {
                        self?.answer(r, Dictionary(uniqueKeysWithValues: r.questions.map {
                            ($0.text, $0.multiSelect ? [$0.options.first?.label ?? ""] as Any : ($0.options.first?.label ?? "") as Any)
                        }))
                    }
                }
            }
        } else {
            fx = store.apply(m)
        }
        for e in fx { effect(e) }
        if ProcessInfo.processInfo.environment["HQNOTCH_DEBUG_LOG"] != nil {
            NSLog("HQNotch: %@ %@ -> %@", m.event, m.sessionId,
                  store.ordered.map { "\($0.id.prefix(9)):\($0.status.rawValue)" }.joined(separator: " "))
        }
    }

    private func effect(_ e: StoreEffect) {
        switch e {
        case .needsYou:
            attention += 1
            play("Funk")
        case .finished:
            play("Glass")
        case .failed:
            play("Basso")
        }
    }

    private func play(_ name: String) {
        guard !muted else { return }
        NSSound(named: NSSound.Name(name))?.play()
    }

    // MARK: replies

    func allow(_ r: PendingRequest, always: Bool = false) { send(r, ["decision": always ? "always" : "allow"]) }
    func deny(_ r: PendingRequest) { send(r, ["decision": "deny", "message": "Denied from HQ Notch"]) }
    /// Hand the request back to Claude Code's own terminal prompt.
    func passToTerminal(_ r: PendingRequest) {
        if r.isQuestion { passedQuestion[r.sessionId] = Date() }
        send(r, ["decision": "pass"])
        if let s = store.sessions[r.sessionId] { TerminalJump.focus(s) }
    }
    func answer(_ r: PendingRequest, _ answers: [String: Any]) { send(r, ["decision": "answer", "answers": answers]) }

    private func send(_ r: PendingRequest, _ reply: [String: Any]) {
        server.reply(id: r.id, reply)
        store.resolvePending(id: r.id)
    }

    // MARK: actions

    func jump(_ s: Session) { TerminalJump.focus(s) }

    func openHQ() {
        if let u = URL(string: hqURL), ["127.0.0.1", "localhost"].contains(u.host ?? "") { NSWorkspace.shared.open(u) }
    }

    // MARK: HQ

    private func pollHQ() async {
        guard let base = URL(string: hqURL), ["127.0.0.1", "localhost"].contains(base.host ?? "") else { return }
        var req = URLRequest(url: base.appendingPathComponent("api/sessions"), timeoutInterval: 3)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        guard let (data, resp) = try? await URLSession.shared.data(for: req),
              (resp as? HTTPURLResponse)?.statusCode == 200 else { hqReachable = false; return }
        hqReachable = true
        store.mergeHQ(HQSession.parse(data))
        await pollFloor(base)
    }

    /// HQ's per-install shiny salt (`.dex-seed` next to dashboard.py), so local fallback
    /// shinies match HQ. Optional: without it the fallback is never shiny.
    static func readSalt() -> String? {
        let candidates = [
            ProcessInfo.processInfo.environment["HQ_DIR"],
            UserDefaults.standard.string(forKey: "hqnotch.hqDir"),
            ("~/Documents/Claude/claude-dashboard" as NSString).expandingTildeInPath,
        ].compactMap { $0 }
        for d in candidates {
            if let s = try? String(contentsOfFile: d + "/.dex-seed", encoding: .utf8)
                .trimmingCharacters(in: .whitespacesAndNewlines),
               s.range(of: "^[0-9a-f]{8,64}$", options: .regularExpression) != nil { return s }
        }
        return nil
    }
}
