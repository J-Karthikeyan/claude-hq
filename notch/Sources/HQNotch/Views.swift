import AppKit
import HQNotchCore
import SwiftUI

/// Notch outline: flush with the top edge, rounded at the bottom.
struct NotchShape: Shape {
    var radius: CGFloat
    var animatableData: CGFloat { get { radius } set { radius = newValue } }
    func path(in r: CGRect) -> Path {
        var p = Path()
        let rad = min(radius, r.height / 2, r.width / 2)
        p.move(to: CGPoint(x: r.minX, y: r.minY))
        p.addLine(to: CGPoint(x: r.maxX, y: r.minY))
        p.addLine(to: CGPoint(x: r.maxX, y: r.maxY - rad))
        p.addQuadCurve(to: CGPoint(x: r.maxX - rad, y: r.maxY), control: CGPoint(x: r.maxX, y: r.maxY))
        p.addLine(to: CGPoint(x: r.minX + rad, y: r.maxY))
        p.addQuadCurve(to: CGPoint(x: r.minX, y: r.maxY - rad), control: CGPoint(x: r.minX, y: r.maxY))
        p.closeSubpath()
        return p
    }
}

struct NotchRoot: View {
    @ObservedObject var model: AppModel
    let notch: CGSize          // the hardware notch (or a menu-bar sized stand-in)
    static let wing: CGFloat = 64
    static let expandedWidth: CGFloat = 500

    var body: some View {
        VStack(spacing: 0) {
            Group {
                if model.expanded { expanded } else { closed }
            }
            .background(NotchShape(radius: model.expanded ? 22 : 12).fill(Theme.bg))
            .clipShape(NotchShape(radius: model.expanded ? 22 : 12))
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .animation(.spring(response: 0.32, dampingFraction: 0.82), value: model.expanded)
    }

    // MARK: closed

    private var closed: some View {
        let live = model.visibleSessions
        let needs = model.store.pending.count + live.filter { $0.status == .needs && !model.store.hasPending($0.id) }.count
        return HStack(spacing: 0) {
            HStack(spacing: -6) {
                ForEach(live.prefix(3)) { s in
                    CreatureSprite(sessionId: s.id, creature: model.creature(s), status: s.status,
                                   size: max(18, notch.height - 6), pack: model.pack)
                }
            }
            .frame(width: Self.wing, alignment: .leading)
            .padding(.leading, 10)
            Color.clear.frame(width: notch.width)
            HStack(spacing: 4) {
                if needs > 0 {
                    Text("\(needs)").font(.system(size: 11, weight: .heavy, design: .rounded))
                        .foregroundStyle(Theme.alertText)
                        .padding(.horizontal, 6).padding(.vertical, 1)
                        .background(Capsule().fill(Theme.alert))
                } else if let s = live.first {
                    Circle().fill(Theme.color(s.status)).frame(width: 7, height: 7)
                    Text("\(live.count)").font(.system(size: 11, weight: .semibold, design: .rounded))
                        .foregroundStyle(Theme.subtle)
                }
            }
            .frame(width: Self.wing, alignment: .trailing)
            .padding(.trailing, 10)
        }
        .frame(height: notch.height)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(live.count) Claude sessions, \(needs) need you")
    }

    // MARK: expanded

    private var expanded: some View {
        VStack(alignment: .leading, spacing: 8) {
            header
            if let d = model.showDiff {
                DiffView(model: model, item: d.item, folder: d.folder)
            } else if model.showSettings {
                SettingsView(model: model)
            } else {
                // Oldest two requests as cards; the rest wait their turn.
                ForEach(model.store.pending.prefix(2)) { r in
                    RequestCard(model: model, request: r)
                }
                if model.store.pending.count > 2 {
                    Text("+\(model.store.pending.count - 2) more waiting").font(.system(size: 11, weight: .medium))
                        .foregroundStyle(Theme.alert)
                }
                let list = model.visibleSessions
                if list.isEmpty {
                    Text(model.hqReachable ? "No live Claude sessions." : "No live Claude sessions. Start one and it shows up here.")
                        .font(.system(size: 12)).foregroundStyle(Theme.subtle)
                        .frame(maxWidth: .infinity, alignment: .center).padding(.vertical, 18)
                } else {
                    // Up to four cards at natural height; more scroll inside 380 pt.
                    if list.count <= 4 && model.store.pending.isEmpty {
                        cards(list)
                    } else {
                        ScrollView(.vertical, showsIndicators: false) { cards(list) }
                            .frame(height: model.store.pending.isEmpty ? 380 : 220)
                    }
                }
            }
        }
        .padding(.top, notch.height + 4)
        .padding(.horizontal, 14)
        .padding(.bottom, 14)
        .frame(width: Self.expandedWidth)
        .background(GeometryReader { g in
            Color.clear
                .onAppear { model.expandedSize = g.size }
                .onChange(of: g.size) { _, new in model.expandedSize = new }
        })
    }

    private func cards(_ list: [Session]) -> some View {
        VStack(spacing: 6) {
            ForEach(list) { s in SessionCard(model: model, session: s) }
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            Text("Claude HQ").font(.system(size: 13, weight: .bold, design: .rounded)).foregroundStyle(Theme.text)
            Circle().fill(model.hqReachable ? Theme.ok : Theme.faint).frame(width: 6, height: 6)
                .help(model.hqReachable ? "Connected to Claude HQ" : "Claude HQ is not running: using local creatures")
            Spacer()
            IconButton(symbol: "square.grid.2x2.fill", help: "Open Claude HQ") { model.openHQ() }
            IconButton(symbol: model.showSettings ? "xmark" : "gearshape.fill",
                       help: model.showSettings ? "Close settings" : "Settings") { model.showSettings.toggle() }
        }
    }
}

struct IconButton: View {
    let symbol: String
    let help: String
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Theme.subtle).frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain).help(help).accessibilityLabel(help)
    }
}

// MARK: session card

struct SessionCard: View {
    @ObservedObject var model: AppModel
    let session: Session
    @State private var hover = false

    var body: some View {
        let cr = model.creature(session)
        HStack(alignment: .top, spacing: 10) {
            CreatureSprite(sessionId: session.id, creature: cr, status: session.status, size: 46, pack: model.pack)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(session.label).font(.system(size: 12.5, weight: .semibold)).foregroundStyle(Theme.text)
                        .lineLimit(1)
                    Text(session.title.isEmpty ? cr.name(sessionId: session.id, pack: model.pack)
                                               : "\(session.folder) · \(cr.name(sessionId: session.id, pack: model.pack))")
                        .font(.system(size: 10.5)).foregroundStyle(Theme.faint).lineLimit(1).layoutPriority(-1)
                    if session.subagents > 0 {
                        Text("+\(session.subagents) agents").font(.system(size: 10, weight: .medium))
                            .foregroundStyle(Theme.subtle)
                    }
                    Spacer(minLength: 4)
                    StatusChip(status: session.status)
                }
                if !session.now.isEmpty {
                    Text(session.now).font(.system(size: 11.5)).foregroundStyle(Theme.subtle).lineLimit(1)
                        .truncationMode(.middle)
                }
                if !session.lastPrompt.isEmpty {
                    Text("“\(session.lastPrompt)”").font(.system(size: 11)).foregroundStyle(Theme.faint).lineLimit(1)
                }
                ForEach(session.ticker.prefix(3)) { t in
                    TickerRow(item: t, onOpen: t.diff == nil ? nil : { model.openDiff(t, in: session) })
                }
            }
        }
        .padding(8)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(hover ? Theme.cardHover : Theme.card))
        .contentShape(Rectangle())
        .onHover { hover = $0 }
        .onTapGesture { model.jump(session) }
        .help("Jump to this session's terminal")
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }
}

struct StatusChip: View {
    let status: SessionStatus
    var body: some View {
        let label: String = switch status {
        case .working: "Working"
        case .needs: "Needs you"
        case .done: "Done"
        case .error: "Error"
        case .idle: "Idle"
        }
        Text(label).font(.system(size: 9.5, weight: .bold, design: .rounded))
            .foregroundStyle(status == .needs ? Theme.alertText : Theme.color(status))
            .padding(.horizontal, 6).padding(.vertical, 2)
            .background(Capsule().fill(status == .needs ? Theme.alert : Theme.color(status).opacity(0.16)))
    }
}

struct TickerRow: View {
    let item: TickerItem
    /// Set for an edit: a click opens its change in the diff view.
    var onOpen: (() -> Void)?
    @State private var hover = false
    var body: some View {
        let row = HStack(spacing: 5) {
            Text(item.tool).font(.system(size: 10.5, weight: .semibold, design: .monospaced))
                .foregroundStyle(item.failed ? Theme.danger : Theme.subtle)
            Text(item.target).font(.system(size: 10.5, design: .monospaced)).foregroundStyle(Theme.faint)
                .lineLimit(1).truncationMode(.middle)
            if let p = item.plus, p > 0 {
                Text("+\(p)").font(.system(size: 10.5, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.ok)
            }
            if let m = item.minus, m > 0 {
                Text("−\(m)").font(.system(size: 10.5, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.danger)
            }
            if onOpen != nil {
                Image(systemName: "chevron.right").font(.system(size: 8.5, weight: .bold))
                    .foregroundStyle(hover ? Theme.text : Theme.faint)
            }
        }
        if let onOpen {
            // a click here opens the diff instead of jumping to the terminal (the card's tap)
            row.contentShape(Rectangle()).onHover { hover = $0 }.onTapGesture(perform: onOpen)
                .help("Show this change").accessibilityAddTraits(.isButton)
        } else {
            row
        }
    }
}

// MARK: diff view

/// What an edit changed: removed lines tinted red, added green, a few unchanged lines around.
struct DiffView: View {
    @ObservedObject var model: AppModel
    let item: TickerItem
    let folder: String

    var body: some View {
        let lines = item.diff.map(DiffBuilder.lines) ?? []
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Button { model.showDiff = nil } label: {
                    Label("Back", systemImage: "chevron.left").font(.system(size: 11, weight: .semibold))
                }
                .buttonStyle(LinkStyle()).keyboardShortcut(.escape, modifiers: [])
                VStack(alignment: .leading, spacing: 1) {
                    Text(item.target.isEmpty ? item.tool : item.target)
                        .font(.system(size: 12, weight: .semibold, design: .monospaced)).foregroundStyle(Theme.text).lineLimit(1)
                    Text("\(item.tool) · \(folder)").font(.system(size: 10.5)).foregroundStyle(Theme.faint).lineLimit(1)
                }
                Spacer()
                if let p = item.plus, p > 0 { Text("+\(p)").font(.system(size: 11, weight: .bold, design: .monospaced)).foregroundStyle(Theme.ok) }
                if let m = item.minus, m > 0 { Text("−\(m)").font(.system(size: 11, weight: .bold, design: .monospaced)).foregroundStyle(Theme.danger) }
            }
            ScrollView(.vertical) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, l in DiffRow(line: l) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
            }
            .frame(height: min(380, max(60, CGFloat(lines.count)*15 + 12)))
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.card))
        }
    }
}

struct DiffRow: View {
    let line: DiffLine
    var body: some View {
        switch line {
        case .context(let s): text(" " + s, Theme.subtle, nil)
        case .removed(let s): text("−" + s, Theme.text, Theme.danger.opacity(0.22))
        case .added(let s): text("+" + s, Theme.text, Theme.ok.opacity(0.2))
        case .gap(let n):
            Text(n > 0 ? "⋯ \(n) unchanged line\(n == 1 ? "" : "s")" : "⋯")
                .font(.system(size: 10, design: .monospaced)).foregroundStyle(Theme.faint).padding(.vertical, 3).padding(.horizontal, 6)
        case .more:
            Text("… more than the notch shows: see the terminal").font(.system(size: 10)).foregroundStyle(Theme.faint)
                .padding(.vertical, 3).padding(.horizontal, 6)
        }
    }

    private func text(_ s: String, _ fg: Color, _ bg: Color?) -> some View {
        Text(s.isEmpty ? " " : s).font(.system(size: 10.5, design: .monospaced)).foregroundStyle(fg)
            .lineLimit(1).truncationMode(.tail)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 6).padding(.vertical, 1)
            .background(bg ?? .clear)
    }
}

// MARK: requests

struct RequestCard: View {
    @ObservedObject var model: AppModel
    let request: PendingRequest

    var body: some View {
        let s = model.store.sessions[request.sessionId]
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                if let s {
                    CreatureSprite(sessionId: s.id, creature: model.creature(s), status: .needs, size: 34, pack: model.pack)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text(s?.label ?? "Claude").font(.system(size: 12, weight: .semibold)).foregroundStyle(Theme.text)
                    Text(request.isPlan ? "Has a plan for you" : request.isQuestion ? "Has a question" : "Wants to run \(request.tool)")
                        .font(.system(size: 11)).foregroundStyle(Theme.alert)
                }
                Spacer()
            }
            if request.isPlan {
                PlanBody(model: model, request: request)
            } else if request.isQuestion {
                QuestionForm(model: model, request: request)
            } else {
                Text(RequestDetail.text(request)).font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Theme.text).lineLimit(6).truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .background(RoundedRectangle(cornerRadius: 8).fill(Theme.card))
                    .textSelection(.enabled)
                HStack(spacing: 8) {
                    Button("Terminal") { model.passToTerminal(request) }.buttonStyle(LinkStyle())
                    Spacer()
                    Button("Deny") { model.deny(request) }.buttonStyle(PillStyle(fill: Theme.danger))
                        .keyboardShortcut(.escape, modifiers: [])
                    if request.canAlways {
                        Button("Always") { model.allow(request, always: true) }.buttonStyle(PillStyle(fill: Theme.working))
                    }
                    Button("Allow") { model.allow(request) }.buttonStyle(PillStyle(fill: Theme.ok))
                        .keyboardShortcut(.return, modifiers: [])
                }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Theme.alert.opacity(0.7), lineWidth: 1))
    }
}

/// A plan waiting for approval (ExitPlanMode): read it, then approve it, send it back, or answer
/// in the terminal.
struct PlanBody: View {
    @ObservedObject var model: AppModel
    let request: PendingRequest

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ScrollView(.vertical) {
                Text(PlanText.render(request.plan ?? ""))
                    .font(.system(size: 11.5)).foregroundStyle(Theme.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .textSelection(.enabled)
            }
            .frame(maxHeight: 220)
            .fixedSize(horizontal: false, vertical: (request.plan ?? "").count < 400)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.card))
            HStack(spacing: 8) {
                Button("Terminal") { model.passToTerminal(request) }.buttonStyle(LinkStyle())
                Spacer()
                Button("Keep planning") { model.keepPlanning(request) }.buttonStyle(PillStyle(fill: Theme.working))
                    .keyboardShortcut(.escape, modifiers: [])
                Button("Approve") { model.allow(request) }.buttonStyle(PillStyle(fill: Theme.ok))
                    .keyboardShortcut(.return, modifiers: [])
            }
        }
    }
}

/// A plan's markdown as text: inline styles (bold, code, links) rendered, line breaks kept; plain
/// text if it does not parse. Headings and bullets stay as written, which reads fine.
enum PlanText {
    static func render(_ md: String) -> AttributedString {
        let opts = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: md, options: opts)) ?? AttributedString(md)
    }
}

/// What the permission card shows for a tool input.
enum RequestDetail {
    static func text(_ r: PendingRequest) -> String {
        let ti = r.toolInput
        switch r.tool {
        case "Bash": return "$ " + (ti?["command"]?.string ?? r.target)
        case "Edit", "MultiEdit", "Write", "NotebookEdit":
            var out = ti?["file_path"]?.string ?? ti?["notebook_path"]?.string ?? r.target
            if let old = ti?["old_string"]?.string, let new = ti?["new_string"]?.string {
                out += "\n" + diff(old, "-") + "\n" + diff(new, "+")
            } else if let c = ti?["content"]?.string {
                out += "\n" + diff(c, "+")
            }
            return out
        case "WebFetch": return ti?["url"]?.string ?? r.target
        default:
            if let ti, let d = try? JSONEncoder().encode(ti), let s = String(data: d, encoding: .utf8) {
                return s.count > 600 ? String(s.prefix(600)) + "…" : s
            }
            return r.target
        }
    }

    private static func diff(_ s: String, _ mark: String) -> String {
        let lines = s.split(separator: "\n", omittingEmptySubsequences: false).prefix(3)
        return lines.map { "\(mark) \($0)" }.joined(separator: "\n")
    }
}

struct QuestionForm: View {
    @ObservedObject var model: AppModel
    let request: PendingRequest
    @State private var picks: [String: [String]] = [:]

    private var complete: Bool { request.questions.allSatisfy { !(picks[$0.text] ?? []).isEmpty } }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(request.questions) { q in
                VStack(alignment: .leading, spacing: 5) {
                    if !q.header.isEmpty {
                        Text(q.header.uppercased()).font(.system(size: 9.5, weight: .bold)).foregroundStyle(Theme.faint)
                    }
                    Text(q.text).font(.system(size: 12, weight: .medium)).foregroundStyle(Theme.text)
                        .fixedSize(horizontal: false, vertical: true)
                    ForEach(Array(q.options.enumerated()), id: \.offset) { _, o in
                        let on = (picks[q.text] ?? []).contains(o.label)
                        Button { toggle(q, o.label) } label: {
                            HStack(alignment: .top, spacing: 6) {
                                Image(systemName: q.multiSelect ? (on ? "checkmark.square.fill" : "square")
                                                                : (on ? "largecircle.fill.circle" : "circle"))
                                    .font(.system(size: 11)).foregroundStyle(on ? Theme.ok : Theme.subtle)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(o.label).font(.system(size: 11.5, weight: .semibold)).foregroundStyle(Theme.text)
                                    if !o.detail.isEmpty {
                                        Text(o.detail).font(.system(size: 10.5)).foregroundStyle(Theme.subtle)
                                            .fixedSize(horizontal: false, vertical: true)
                                    }
                                }
                                Spacer(minLength: 0)
                            }
                            .padding(6)
                            .background(RoundedRectangle(cornerRadius: 8).fill(on ? Theme.cardHover : Theme.card))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityAddTraits(on ? .isSelected : [])
                    }
                }
            }
            HStack {
                Button("Answer in terminal") { model.passToTerminal(request) }.buttonStyle(LinkStyle())
                Spacer()
                Button("Send") { submit() }.buttonStyle(PillStyle(fill: complete ? Theme.ok : Theme.faint))
                    .disabled(!complete)
            }
        }
    }

    private func toggle(_ q: Question, _ label: String) {
        var cur = picks[q.text] ?? []
        if q.multiSelect {
            if let i = cur.firstIndex(of: label) { cur.remove(at: i) } else { cur.append(label) }
        } else {
            cur = [label]
        }
        picks[q.text] = cur
    }

    private func submit() {
        var answers: [String: Any] = [:]
        for q in request.questions {
            let p = picks[q.text] ?? []
            answers[q.text] = q.multiSelect ? p : (p.first ?? "")
        }
        model.answer(request, answers)
    }
}

struct PillStyle: ButtonStyle {
    let fill: Color
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.system(size: 11.5, weight: .bold, design: .rounded))
            .foregroundStyle(Theme.buttonText)
            .padding(.horizontal, 12).padding(.vertical, 5)
            .background(Capsule().fill(fill.opacity(configuration.isPressed ? 0.7 : 1)))
    }
}

struct LinkStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label.font(.system(size: 11, weight: .medium))
            .foregroundStyle(configuration.isPressed ? Theme.text : Theme.subtle)
            .underline()
    }
}

// MARK: settings

/// Phone alerts: a content-free push to an ntfy topic when a session needs you, finishes or
/// fails. Off until you turn it on.
struct PhoneSettings: View {
    @ObservedObject var model: AppModel
    @State private var copied = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SettingToggle(title: "Phone alerts",
                          detail: "A short push (\u{201C}A Claude session needs you\u{201D}, \u{201C}…finished\u{201D}) through ntfy. Never your prompts, files or session names.",
                          isOn: $model.phoneOn)
            if model.phoneOn {
                HStack(spacing: 6) {
                    Text("Server").font(.system(size: 10.5)).foregroundStyle(Theme.faint).frame(width: 46, alignment: .leading)
                    TextField("https://ntfy.sh", text: $model.phoneServer)
                        .textFieldStyle(.roundedBorder).font(.system(size: 11, design: .monospaced))
                        .disableAutocorrection(true)
                }
                HStack(spacing: 6) {
                    Text("Topic").font(.system(size: 10.5)).foregroundStyle(Theme.faint).frame(width: 46, alignment: .leading)
                    Text(model.phoneTopic).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.text)
                        .textSelection(.enabled)
                    Spacer(minLength: 4)
                    Button(copied ? "Copied" : "Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(model.phoneTopic, forType: .string)
                        copied = true
                        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { copied = false }
                    }.buttonStyle(LinkStyle())
                    Button("New") { model.newPhoneTopic() }.buttonStyle(LinkStyle())
                        .help("Make a new random topic: anyone following the old one stops getting your alerts")
                }
                Text("Install the ntfy app on your phone and subscribe to this topic. Anyone who knows the topic can read the alerts, so keep it to yourself.")
                    .font(.system(size: 10.5)).foregroundStyle(Theme.faint).fixedSize(horizontal: false, vertical: true)
                if PhoneTopic.url(server: model.phoneServer, topic: model.phoneTopic) == nil {
                    Text("The server must be an http(s) address, like https://ntfy.sh.").font(.system(size: 10.5)).foregroundStyle(Theme.danger)
                }
                SettingToggle(title: "Only when I'm away from the Mac",
                              detail: "Skip the push if you used the keyboard or mouse in the last 2 minutes.",
                              isOn: $model.phoneAwayOnly)
                HStack(spacing: 8) {
                    Button("Send a test") { model.sendTestPush() }.buttonStyle(PillStyle(fill: Theme.working))
                    if let st = model.phoneStatus {
                        Text(st).font(.system(size: 10.5)).foregroundStyle(Theme.subtle).lineLimit(2)
                    }
                }
            }
        }
    }
}

/// The notch's own settings (the gear in the header). Every choice is saved at once.
struct SettingsView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Sprites").font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.subtle)
                Picker("Sprites", selection: $model.spriteSetting) {
                    ForEach(SpriteSetting.allCases) { Text($0.label).tag($0) }
                }
                .pickerStyle(.segmented).labelsHidden()
                Text(spriteNote).font(.system(size: 10.5)).foregroundStyle(Theme.faint)
                    .fixedSize(horizontal: false, vertical: true)
                previewRow
            }
            Divider().overlay(Theme.card)
            SettingToggle(title: "Sounds", detail: "A chime when a session needs you or finishes.", isOn: $model.soundsOn)
            SettingToggle(title: "Show idle sessions", detail: "Sessions waiting for your next prompt.", isOn: $model.showIdle)
            SettingToggle(title: "Approve from the notch",
                          detail: "Off: every permission prompt and question goes straight to the terminal.",
                          isOn: $model.approvals)
            Divider().overlay(Theme.card)
            PhoneSettings(model: model)
            Divider().overlay(Theme.card)
            HStack {
                Circle().fill(model.hqReachable ? Theme.ok : Theme.faint).frame(width: 6, height: 6)
                Text(model.hqReachable ? "Connected to Claude HQ" : "Claude HQ isn't running: creatures are computed here")
                    .font(.system(size: 10.5)).foregroundStyle(Theme.subtle)
                Spacer()
                Button("Quit") { NSApp.terminate(nil) }.buttonStyle(LinkStyle())
            }
        }
        .padding(.top, 2)
    }

    private var spriteNote: String {
        switch model.spriteSetting {
        case .twoD: return "Animated 2D sprites. Apex is the line's final form."
        case .threeD: return "Animated 3D sprites, as in HQ's 3D pack. Apex Pokémon with a Mega show their Mega form."
        case .matchHQ: return "Whatever HQ uses: now \(model.hqPack == .threeD ? "3D" : "2D")."
        }
    }

    /// The live sessions in the chosen style, so the switch shows what it does.
    private var previewRow: some View {
        HStack(spacing: 6) {
            ForEach(model.visibleSessions.prefix(6)) { s in
                CreatureSprite(sessionId: s.id, creature: model.creature(s), status: .working, size: 34, pack: model.pack)
            }
        }
    }
}

struct SettingToggle: View {
    let title: String
    let detail: String
    @Binding var isOn: Bool
    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.system(size: 12, weight: .medium)).foregroundStyle(Theme.text)
                Text(detail).font(.system(size: 10.5)).foregroundStyle(Theme.faint)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Toggle(title, isOn: $isOn).labelsHidden()
                .toggleStyle(.switch).tint(Theme.ok).controlSize(.small)
        }
        .contentShape(Rectangle())
        .onTapGesture { isOn.toggle() }
    }
}
