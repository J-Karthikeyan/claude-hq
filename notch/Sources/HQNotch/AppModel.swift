import AppKit
import HQNotchCore
import Security
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

enum SpriteSetting: String, CaseIterable, Identifiable {
    case twoD = "2d", threeD = "3d", matchHQ = "hq"
    var id: String { rawValue }
    var label: String {
        switch self { case .twoD: "2D"; case .threeD: "3D"; case .matchHQ: "Match HQ" }
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
    /// Settings speaks in "on" terms.
    var soundsOn: Bool { get { !muted } set { muted = !newValue } }
    /// Where the pill sits (Placement.auto steps beside the notch while another notch app runs).
    @Published var placement = Placement(rawValue: UserDefaults.standard.string(forKey: "hqnotch.placement") ?? "") ?? .auto {
        didSet { UserDefaults.standard.set(placement.rawValue, forKey: "hqnotch.placement") }
    }
    /// The other notch app running now (its name), if any.
    @Published var otherNotchApp: String?
    var placed: Placement { placement.resolved(otherNotchApp: otherNotchApp != nil) }
    func scanNotchApps() {
        let apps = NSWorkspace.shared.runningApplications.map { (bundleId: $0.bundleIdentifier, name: $0.localizedName) }
        let found = NotchNeighbours.running(apps, own: Bundle.main.bundleIdentifier)
        if found != otherNotchApp { otherNotchApp = found }
    }
    /// Which sprites to draw: 2D, 3D, or whatever pack HQ itself uses.
    @Published var spriteSetting = SpriteSetting(rawValue: UserDefaults.standard.string(forKey: "hqnotch.sprites") ?? "") ?? .twoD {
        didSet { UserDefaults.standard.set(spriteSetting.rawValue, forKey: "hqnotch.sprites") }
    }
    /// HQ's own pack (its `creaturePack`), for "Match HQ".
    @Published private(set) var hqPack: SpritePack = .gen5
    /// The settings view is showing in place of the session list.
    @Published var showSettings = false
    var pack: SpritePack {
        switch spriteSetting { case .twoD: .gen5; case .threeD: .threeD; case .matchHQ: hqPack }
    }
    /// HQ's address; only 127.0.0.1 / localhost is ever used.
    let hqURL = UserDefaults.standard.string(forKey: "hqnotch.hqURL") ?? "http://127.0.0.1:8765"

    // Phone alerts (opt-in, off by default): content-free pushes to an ntfy topic.
    @Published var phoneOn = UserDefaults.standard.bool(forKey: "hqnotch.phone") {
        didSet { UserDefaults.standard.set(phoneOn, forKey: "hqnotch.phone") }
    }
    @Published var phoneServer = UserDefaults.standard.string(forKey: "hqnotch.phoneServer") ?? "https://ntfy.sh" {
        didSet { UserDefaults.standard.set(phoneServer, forKey: "hqnotch.phoneServer") }
    }
    @Published var phoneAwayOnly = UserDefaults.standard.bool(forKey: "hqnotch.phoneAway") {
        didSet { UserDefaults.standard.set(phoneAwayOnly, forKey: "hqnotch.phoneAway") }
    }
    /// Made once per Mac (PhoneTopic), then kept.
    @Published private(set) var phoneTopic: String = AppModel.loadTopic()
    /// The last test push's outcome, for the settings view.
    @Published var phoneStatus: String?
    private var phoneDebounce = PhoneDebounce()

    /// The diff view is showing this ticker item's change in place of the session list.
    @Published var showDiff: (item: TickerItem, folder: String)?

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

    private var configFetchedAt: Date?
    private func pollConfig(_ base: URL) async {
        if let t = configFetchedAt, Date().timeIntervalSince(t) < 30 { return }
        var req = URLRequest(url: base.appendingPathComponent("api/config"), timeoutInterval: 3)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        guard let (data, resp) = try? await URLSession.shared.data(for: req),
              (resp as? HTTPURLResponse)?.statusCode == 200,
              let o = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return }
        configFetchedAt = Date()
        let p = SpritePack.fromHQ(o["creaturePack"] as? String)
        if p != hqPack { hqPack = p }
    }

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
        phone(PhoneAlert.from(e))
        switch e {
        case .needsYou:
            showSettings = false          // a request must never hide behind the settings view
            showDiff = nil                // ...or behind a diff
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
    /// A plan (ExitPlanMode) sent back: Claude stays in plan mode.
    func keepPlanning(_ r: PendingRequest) { send(r, ["decision": "deny", "message": "Keep planning — not approved yet"]) }
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

    // MARK: phone alerts

    /// Push one content-free alert, if phone alerts are on (and you are away, when asked).
    private func phone(_ a: PhoneAlert) {
        guard phoneOn, !phoneAwayOnly || Self.awaySeconds() > 120, phoneDebounce.allow(a) else { return }
        Task { _ = await push(a.title, a.body, tags: a.tags, priority: a.priority) }
    }

    func sendTestPush() {
        phoneStatus = "Sending…"
        Task {
            let ok = await push("Claude HQ", "Test from HQ Notch: phone alerts work", tags: "tada", priority: "default")
            phoneStatus = ok ? "Sent. Check your phone." : "Could not reach \(phoneServer). Check the server and your connection."
        }
    }

    /// POST to the ntfy topic. The body and headers are fixed strings (PhoneAlert): nothing
    /// from a session ever goes in.
    private func push(_ title: String, _ body: String, tags: String, priority: String) async -> Bool {
        guard let url = PhoneTopic.url(server: phoneServer, topic: phoneTopic) else { return false }
        var req = URLRequest(url: url, timeoutInterval: 8)
        req.httpMethod = "POST"
        req.httpBody = Data(body.utf8)
        req.setValue(title, forHTTPHeaderField: "Title")
        req.setValue(tags, forHTTPHeaderField: "Tags")
        req.setValue(priority, forHTTPHeaderField: "Priority")
        guard let (_, resp) = try? await URLSession.shared.data(for: req) else { return false }
        return ((resp as? HTTPURLResponse)?.statusCode ?? 0) / 100 == 2
    }

    /// A new random topic (anyone with the old one can no longer follow you).
    func newPhoneTopic() {
        phoneTopic = Self.makeTopic()
        UserDefaults.standard.set(phoneTopic, forKey: "hqnotch.phoneTopic")
    }

    static func loadTopic() -> String {
        if let t = UserDefaults.standard.string(forKey: "hqnotch.phoneTopic"), PhoneTopic.isValid(t) { return t }
        let t = makeTopic()
        UserDefaults.standard.set(t, forKey: "hqnotch.phoneTopic")
        return t
    }

    static func makeTopic() -> String {
        var bytes = [UInt8](repeating: 0, count: 20)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        return PhoneTopic.make(bytes) ?? "hq-" + String(UUID().uuidString.lowercased().filter { $0.isLetter }.prefix(20))
    }

    /// Seconds since your last key press, click, scroll or mouse move on this Mac.
    static func awaySeconds() -> Double {
        let kinds: [CGEventType] = [.keyDown, .leftMouseDown, .rightMouseDown, .mouseMoved, .scrollWheel]
        return kinds.map { CGEventSource.secondsSinceLastEventType(.combinedSessionState, eventType: $0) }.min() ?? 0
    }

    // MARK: actions

    func jump(_ s: Session) { TerminalJump.focus(s) }

    /// Open a ticker item's change (it has one: an edit) in the diff view.
    func openDiff(_ item: TickerItem, in s: Session) {
        guard item.diff != nil else { return }
        showSettings = false
        showDiff = (item, s.label)
    }

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
        await pollConfig(base)
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
