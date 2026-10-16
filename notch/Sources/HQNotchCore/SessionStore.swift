import Foundation

public enum SessionStatus: String, Sendable {
    case working, needs, done, idle, error
}

/// One line of the live ticker: a tool call, with +N/-M for edits.
public struct TickerItem: Equatable, Sendable, Identifiable {
    public var id: String
    public var tool: String
    public var target: String
    public var plus: Int?
    public var minus: Int?
    public var failed: Bool
    public var at: Date
}

/// A permission or AskUserQuestion request waiting on the person (one open hook socket).
public struct PendingRequest: Equatable, Sendable, Identifiable {
    public var id: Int
    public var sessionId: String
    public var tool: String
    public var target: String
    public var toolInput: JSONValue?
    public var canAlways: Bool
    public var questions: [Question]
    public var at: Date
    public var isQuestion: Bool { !questions.isEmpty }
}

public struct Session: Equatable, Sendable, Identifiable {
    public var id: String
    public var cwd: String
    public var status: SessionStatus = .idle
    public var now: String = ""
    public var lastPrompt: String = ""
    public var promptCount = 0
    public var subagents = 0
    public var ticker: [TickerItem] = []
    public var lastEvent: Date
    public var doneAt: Date?
    public var ppid = 0
    public var env: [String: String] = [:]
    public var creature: Creature?        // from HQ when known; nil = compute locally
    public var title = ""                 // HQ's session title, when HQ is running

    public var folder: String {
        let name = (cwd as NSString).lastPathComponent
        return name.isEmpty ? "session" : name
    }

    /// Card heading: HQ's title, else the folder.
    public var label: String { title.isEmpty ? folder : title }
}

/// Things the UI should do once (sounds, popping the panel open).
public enum StoreEffect: Equatable, Sendable {
    case needsYou(sessionId: String)
    case finished(sessionId: String)
    case failed(sessionId: String)
}

/// All live state, driven only by hook messages and replies. Pure and deterministic:
/// `now` is passed in so the self-test can replay a session.
public struct SessionStore: Sendable {
    public private(set) var sessions: [String: Session] = [:]
    public private(set) var pending: [PendingRequest] = []
    public static let tickerMax = 6
    /// A finished session shows its celebration this long, then settles to idle.
    public static let doneSecs: TimeInterval = 90

    public init() {}

    public var ordered: [Session] {
        sessions.values.sorted { a, b in
            let ra = rank(a.status), rb = rank(b.status)
            return ra != rb ? ra < rb : a.lastEvent > b.lastEvent
        }
    }

    private func rank(_ s: SessionStatus) -> Int {
        switch s { case .needs: 0; case .error: 1; case .working: 2; case .done: 3; case .idle: 4 }
    }

    @discardableResult
    public mutating func apply(_ m: HookMessage, now: Date = Date()) -> [StoreEffect] {
        guard !m.sessionId.isEmpty else { return [] }
        if m.event == "SessionEnd" {
            sessions[m.sessionId] = nil
            pending.removeAll { $0.sessionId == m.sessionId }
            return []
        }
        var s = sessions[m.sessionId] ?? Session(id: m.sessionId, cwd: m.cwd, lastEvent: now)
        if !m.cwd.isEmpty { s.cwd = m.cwd }
        if m.ppid > 0 { s.ppid = m.ppid }
        if !m.env.isEmpty { s.env = m.env }
        s.lastEvent = now
        var fx: [StoreEffect] = []
        let label = m.tool.map { $0.target.isEmpty ? $0.tool : "\($0.tool) · \($0.target)" } ?? ""

        switch m.event {
        case "SessionStart":
            if s.status != .working { s.status = .idle }
        case "UserPromptSubmit":
            s.status = .working
            s.promptCount += 1
            s.lastPrompt = m.prompt ?? s.lastPrompt
            s.now = "Thinking"
            s.doneAt = nil
        case "PreToolUse":
            s.status = hasPending(m.sessionId) ? .needs : .working
            s.now = label
        case "PostToolUse", "PostToolUseFailure":
            // A tool that ran was answered somewhere (maybe the terminal): drop its card.
            if let t = m.tool { pending.removeAll { $0.sessionId == m.sessionId && $0.tool == t.tool } }
            s.status = hasPending(m.sessionId) ? .needs : .working
            if let t = m.tool {
                let item = TickerItem(id: (m.toolUseId ?? "").isEmpty ? UUID().uuidString : m.toolUseId!,
                                      tool: t.tool, target: t.target, plus: m.plus, minus: m.minus,
                                      failed: m.event == "PostToolUseFailure", at: now)
                s.ticker.removeAll { $0.id == item.id }
                s.ticker.insert(item, at: 0)
                if s.ticker.count > Self.tickerMax { s.ticker.removeLast(s.ticker.count - Self.tickerMax) }
            }
        case "Notification":
            if m.notificationType == "permission_prompt" || m.notificationType == "elicitation_dialog" {
                if s.status != .needs { fx.append(.needsYou(sessionId: s.id)) }
                s.status = .needs
                s.now = m.message ?? "Needs you"
            } else if m.notificationType == "idle_prompt" && s.status != .needs {
                s.status = .idle
                s.now = "Waiting for your next prompt"
            }
        case "Stop":
            pending.removeAll { $0.sessionId == m.sessionId }
            s.status = .done
            s.now = "Done"
            s.doneAt = now
            fx.append(.finished(sessionId: s.id))
        case "StopFailure":
            pending.removeAll { $0.sessionId == m.sessionId }
            s.status = .error
            s.now = (m.error ?? "").isEmpty ? "Stopped with an error" : m.error!
            fx.append(.failed(sessionId: s.id))
        case "SubagentStart":
            s.subagents += 1
        case "SubagentStop":
            s.subagents = max(0, s.subagents - 1)
        default:
            break
        }
        sessions[s.id] = s
        return fx
    }

    /// A PermissionRequest arrived on connection `id` (the hook is now waiting).
    @discardableResult
    public mutating func addPending(id: Int, _ m: HookMessage, now: Date = Date()) -> [StoreEffect] {
        apply(m, now: now)
        guard !m.sessionId.isEmpty else { return [] }
        let t = m.tool ?? .init(tool: "Tool", target: "")
        let qs = (m.question ?? false) ? Question.parse(m.toolInput) : []
        pending.append(PendingRequest(id: id, sessionId: m.sessionId, tool: t.tool, target: t.target,
                                      toolInput: m.toolInput, canAlways: m.canAlways ?? false,
                                      questions: qs, at: now))
        sessions[m.sessionId]?.status = .needs
        sessions[m.sessionId]?.now = qs.isEmpty ? "Wants to run \(t.tool)" : "Has a question"
        return [.needsYou(sessionId: m.sessionId)]
    }

    /// The request was answered (notch) or its hook went away (timeout / terminal).
    public mutating func resolvePending(id: Int, now: Date = Date()) {
        guard let i = pending.firstIndex(where: { $0.id == id }) else { return }
        let sid = pending.remove(at: i).sessionId
        if !hasPending(sid), sessions[sid]?.status == .needs {
            sessions[sid]?.status = .working
            sessions[sid]?.now = "Working"
            sessions[sid]?.lastEvent = now
        }
    }

    public func hasPending(_ sid: String) -> Bool { pending.contains { $0.sessionId == sid } }

    /// Time-based settling: done -> idle after `doneSecs`; drop sessions silent for `staleSecs`.
    public mutating func tick(now: Date = Date(), staleSecs: TimeInterval = 6 * 3600) {
        for (id, s) in sessions {
            if s.status == .done, let d = s.doneAt, now.timeIntervalSince(d) > Self.doneSecs {
                sessions[id]?.status = .idle
                sessions[id]?.now = "Waiting for your next prompt"
            }
            if now.timeIntervalSince(s.lastEvent) > staleSecs, !hasPending(id) { sessions[id] = nil }
        }
    }

    /// Merge the creature HQ assigned (and seed sessions HQ sees that started before the notch).
    public mutating func mergeHQ(_ items: [HQSession], now: Date = Date()) {
        for h in items {
            if var s = sessions[h.sessionId] {
                // HQ says the terminal is gone and the notch has heard nothing for a while.
                if !h.live, s.status == .idle || s.status == .done, !hasPending(s.id),
                   now.timeIntervalSince(s.lastEvent) > 120 {
                    sessions[h.sessionId] = nil
                    continue
                }
                s.creature = h.creature
                if !h.title.isEmpty { s.title = h.title }
                if s.promptCount < h.promptCount { s.promptCount = h.promptCount }
                sessions[h.sessionId] = s
            } else if h.live {
                var s = Session(id: h.sessionId, cwd: h.cwd, lastEvent: now)
                s.creature = h.creature
                s.title = h.title
                s.promptCount = h.promptCount
                s.status = h.status
                s.now = h.now
                s.lastPrompt = h.lastPrompt
                sessions[h.sessionId] = s
            }
        }
    }
}

/// The parts of an HQ `/api/sessions` entry the notch uses.
public struct HQSession: Sendable, Equatable {
    public var sessionId: String
    public var cwd: String
    public var status: SessionStatus
    public var live: Bool
    public var now: String
    public var lastPrompt: String
    public var promptCount: Int
    public var creature: Creature?
    public var title: String

    public init(sessionId: String, cwd: String, status: SessionStatus, live: Bool, now: String,
                lastPrompt: String, promptCount: Int, creature: Creature?, title: String = "") {
        self.sessionId = sessionId; self.cwd = cwd; self.status = status; self.live = live
        self.now = now; self.lastPrompt = lastPrompt; self.promptCount = promptCount; self.creature = creature
        self.title = title
    }

    /// Parse HQ's payload. Only Claude Code sessions HQ reports as live (an interactive
    /// session with a running pid, working / needs / idle, not stale) seed new cards.
    public static func parse(_ data: Data) -> [HQSession] {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let list = root["sessions"] as? [[String: Any]] else { return [] }
        return list.compactMap { o in
            guard let sid = o["sessionId"] as? String, !sid.isEmpty else { return nil }
            let raw = (o["status"] as? String) ?? ""
            let status: SessionStatus = raw == "working" ? .working : raw == "needs" ? .needs : .idle
            var cr: Creature?
            if let c = o["creature"] as? [String: Any], let sp = c["species"] as? Int {
                cr = Creature(species: sp, stage: (c["stage"] as? Int) ?? 0, shiny: (c["shiny"] as? Bool) ?? false)
            }
            let live = ["working", "needs", "idle"].contains(raw) && (o["source"] as? String ?? "claude") == "claude"
                && (o["stale"] as? Bool) != true && (o["kind"] as? String) == "interactive" && o["pid"] is Int
            return HQSession(sessionId: sid, cwd: (o["cwd"] as? String) ?? "", status: status, live: live,
                             now: (o["now"] as? String) ?? "", lastPrompt: (o["lastPrompt"] as? String) ?? "",
                             promptCount: (o["promptCount"] as? Int) ?? 0, creature: cr,
                             title: (o["title"] as? String) ?? "")
        }
    }
}
