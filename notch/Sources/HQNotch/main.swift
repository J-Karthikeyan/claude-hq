import AppKit
import Combine
import HQNotchCore
import SwiftUI

/// Borderless panel that floats over the menu bar on every Space without stealing focus.
final class NotchPanel: NSPanel {
    init(frame: NSRect) {
        super.init(contentRect: frame, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        level = .statusBar
        collectionBehavior = [.canJoinAllSpaces, .stationary, .fullScreenAuxiliary, .ignoresCycle]
        isMovable = false
        hidesOnDeactivate = false
        ignoresMouseEvents = true
    }
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

@MainActor
final class NotchController {
    let model: AppModel
    private var panel: NotchPanel!
    private var notch = CGSize(width: 190, height: 32)
    private var mouseTimer: Timer?
    private var leftAt: Date?
    private var enteredAt: Date?
    /// The cursor must rest on the closed pill this long before it opens.
    static let hoverDelay: TimeInterval = 0.5
    private var autoOpenUntil: Date?
    private var bag = Set<AnyCancellable>()
    static let panelSize = CGSize(width: NotchRoot.expandedWidth + 40, height: 900)

    init(model: AppModel) {
        self.model = model
        panel = NotchPanel(frame: NSRect(origin: .zero, size: Self.panelSize))
        layout()
        NotificationCenter.default.addObserver(forName: NSApplication.didChangeScreenParametersNotification,
                                               object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.layout() }
        }
        model.$attention.dropFirst().sink { [weak self] _ in self?.popOpen() }.store(in: &bag)
        // step aside for another notch app (and come back when it quits), or follow the setting
        let ws = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didLaunchApplicationNotification, NSWorkspace.didTerminateApplicationNotification] {
            ws.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.model.scanNotchApps() }
            }
        }
        model.$placement.dropFirst().sink { [weak self] _ in DispatchQueue.main.async { self?.layout() } }.store(in: &bag)
        model.$otherNotchApp.dropFirst().removeDuplicates().sink { [weak self] _ in DispatchQueue.main.async { self?.layout() } }.store(in: &bag)
        model.scanNotchApps()                   // after the sinks, so finding one at launch re-lays out
        mouseTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.trackMouse() }
        }
        panel.orderFrontRegardless()
        if let dir = ProcessInfo.processInfo.environment["HQNOTCH_SNAPSHOT_DIR"] { debugSnapshots(dir) }
    }

    /// Debug aid (needs no Screen Recording permission): render the panel's own views to
    /// PNGs, closed then expanded, so a change can be checked without looking at the screen.
    private func debugSnapshots(_ dir: String) {
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(3))
            snapshot(to: dir + "/notch-closed.png")
            autoOpenUntil = Date().addingTimeInterval(2.5)
            set(expanded: true)
            try? await Task.sleep(for: .seconds(2))
            snapshot(to: dir + "/notch-expanded.png")
            autoOpenUntil = Date().addingTimeInterval(2)
            model.showSettings = true
            try? await Task.sleep(for: .seconds(1.5))
            snapshot(to: dir + "/notch-settings.png")
            autoOpenUntil = nil   // back to normal hover behaviour
            try? await Task.sleep(for: .seconds(1.5))
            snapshot(to: dir + "/notch-after.png")   // collapsed again unless hovered
        }
    }

    private func snapshot(to path: String) {
        guard let v = panel.contentView else { return }
        var r = visibleRect.offsetBy(dx: -panel.frame.minX, dy: -panel.frame.minY)
        if v.isFlipped { r.origin.y = v.bounds.height - r.maxY }
        guard let rep = v.bitmapImageRepForCachingDisplay(in: r) else { return }
        v.cacheDisplay(in: r, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    }

    /// The built-in notched display if there is one, else the main screen.
    private var screen: NSScreen? {
        NSScreen.screens.first { $0.safeAreaInsets.top > 0 } ?? NSScreen.main
    }

    private func layout() {
        guard let sc = screen else { return }
        if sc.safeAreaInsets.top > 0, let l = sc.auxiliaryTopLeftArea, let r = sc.auxiliaryTopRightArea {
            notch = CGSize(width: sc.frame.width - l.width - r.width, height: sc.safeAreaInsets.top)
        } else {
            // No notch: a small pill under the middle of the menu bar.
            notch = CGSize(width: 120, height: max(24, sc.frame.maxY - sc.visibleFrame.maxY))
        }
        let f = sc.frame, place = model.placed, beside = place != .notch
        // Beside the notch, the pill is a capsule of its own (no gap for the notch) just left or
        // right of it; the expanded panel hangs below, centred on it but kept on the screen.
        let pill = Self.pillWidth(notch: notch, beside: beside)
        let anchor: CGFloat = place == .left ? f.midX - notch.width/2 - 8 - pill/2
            : place == .right ? f.midX + notch.width/2 + 8 + pill/2 : f.midX
        let x = min(max(f.minX, anchor - Self.panelSize.width / 2), f.maxX - Self.panelSize.width)
        panel.setFrame(NSRect(x: x, y: f.maxY - Self.panelSize.height,
                              width: Self.panelSize.width, height: Self.panelSize.height), display: true)
        pillCentre = anchor - x
        panel.contentView = NSHostingView(rootView: NotchRoot(model: model, notch: notch, gap: beside ? 0 : notch.width,
                                                              anchor: pillCentre - Self.panelSize.width / 2)
            .environment(\.colorScheme, .dark))      // the notch is always black: dark system controls
    }
    /// The closed pill's width: two wings, and the notch between them when it sits in it.
    static func pillWidth(notch: CGSize, beside: Bool) -> CGFloat { (beside ? 0 : notch.width) + 2 * (NotchRoot.wing + 10) }
    /// The pill's centre, from the panel's left edge.
    private var pillCentre: CGFloat = NotchController.panelSize.width / 2

    /// Screen rect of what is currently drawn (closed pill or expanded panel).
    private var visibleRect: NSRect {
        let pf = panel.frame
        let size = model.expanded ? model.expandedSize
            : CGSize(width: Self.pillWidth(notch: notch, beside: model.placed != .notch), height: notch.height)
        // the expanded panel is centred on the pill too, but never past the panel's own edges
        let cx = model.expanded ? min(max(pillCentre, size.width/2), pf.width - size.width/2) : pillCentre
        return NSRect(x: pf.minX + cx - size.width / 2, y: pf.maxY - size.height, width: size.width, height: size.height)
    }

    private func trackMouse() {
        let p = NSEvent.mouseLocation
        let inside = visibleRect.insetBy(dx: -2, dy: -2).contains(p)
        panel.ignoresMouseEvents = !inside
        if inside {
            leftAt = nil
            if !model.expanded {
                if enteredAt == nil { enteredAt = Date() }
                if let e = enteredAt, Date().timeIntervalSince(e) >= Self.hoverDelay { set(expanded: true) }
            }
            return
        }
        enteredAt = nil
        guard model.expanded else { return }
        if let until = autoOpenUntil, Date() < until { return }
        autoOpenUntil = nil
        if leftAt == nil { leftAt = Date() }
        if let l = leftAt, Date().timeIntervalSince(l) > 0.45 { set(expanded: false) }
    }

    /// Something needs the person: open for a few seconds even without hover.
    private func popOpen() {
        autoOpenUntil = Date().addingTimeInterval(8)
        set(expanded: true)
    }

    private func set(expanded: Bool) {
        model.expanded = expanded
        enteredAt = nil
        if !expanded { leftAt = nil; model.showSettings = false; model.showDiff = nil; panel.resignKey() }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()
    var controller: NotchController?
    var statusItem: NSStatusItem?

    func applicationDidFinishLaunching(_ n: Notification) {
        controller = NotchController(model: model)
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item.button?.image = NSImage(systemSymbolName: "circle.hexagongrid.fill", accessibilityDescription: "HQ Notch")
        item.menu = buildMenu()
        statusItem = item
    }

    func applicationWillTerminate(_ n: Notification) { model.server.stop() }

    private func buildMenu() -> NSMenu {
        let m = NSMenu()
        m.addItem(withTitle: "Open Claude HQ", action: #selector(openHQ), keyEquivalent: "o").target = self
        m.addItem(.separator())
        let mute = m.addItem(withTitle: "Sounds", action: #selector(toggleMute(_:)), keyEquivalent: "")
        mute.target = self; mute.state = model.muted ? .off : .on
        let idle = m.addItem(withTitle: "Show idle sessions", action: #selector(toggleIdle(_:)), keyEquivalent: "")
        idle.target = self; idle.state = model.showIdle ? .on : .off
        let appr = m.addItem(withTitle: "Approve from the notch", action: #selector(toggleApprovals(_:)), keyEquivalent: "")
        appr.target = self; appr.state = model.approvals ? .on : .off
        m.addItem(.separator())
        m.addItem(withTitle: "Quit HQ Notch", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        return m
    }

    @objc func openHQ() { model.openHQ() }
    @objc func toggleMute(_ i: NSMenuItem) { model.muted.toggle(); i.state = model.muted ? .off : .on }
    @objc func toggleIdle(_ i: NSMenuItem) { model.showIdle.toggle(); i.state = model.showIdle ? .on : .off }
    @objc func toggleApprovals(_ i: NSMenuItem) { model.approvals.toggle(); i.state = model.approvals ? .on : .off }
}

MainActor.assumeIsolated {
    let app = NSApplication.shared
    app.setActivationPolicy(.accessory)
    let delegate = AppDelegate()
    app.delegate = delegate
    app.run()
}
