import AppKit
import HQNotchCore
import SwiftUI

/// Downloads HQ's sprite hotlinks once and keeps them in ~/Library/Caches/HQNotch/sprites.
/// The request carries only a National Dex number (no session data). Nothing is bundled.
@MainActor
final class SpriteCache {
    static let shared = SpriteCache()
    private let dir: URL = {
        let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        let d = base.appendingPathComponent("HQNotch/sprites", isDirectory: true)
        try? FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }()
    private var memory: [URL: NSImage] = [:]
    private var failedAt: [URL: Date] = [:]
    private var inflight: [URL: Task<NSImage?, Never>] = [:]

    private func file(for url: URL) -> URL {
        // ".../animated/shiny/25.gif" -> "animated-shiny-25.gif"
        let parts = url.pathComponents.suffix(3).joined(separator: "-")
        return dir.appendingPathComponent(parts)
    }

    /// First URL that yields an image, trying each in order; nil if all fail.
    func image(_ urls: [URL]) async -> NSImage? {
        for u in urls { if let img = await image(u) { return img } }
        return nil
    }

    func image(_ url: URL) async -> NSImage? {
        if let m = memory[url] { return m }
        if let f = failedAt[url], Date().timeIntervalSince(f) < 600 { return nil }
        let path = file(for: url)
        if let img = NSImage(contentsOf: path) { memory[url] = img; return img }
        if let t = inflight[url] { return await t.value }
        let t = Task<NSImage?, Never> {
            var req = URLRequest(url: url, cachePolicy: .returnCacheDataElseLoad, timeoutInterval: 8)
            req.setValue("HQNotch", forHTTPHeaderField: "User-Agent")
            guard let (data, resp) = try? await URLSession.shared.data(for: req),
                  (resp as? HTTPURLResponse)?.statusCode == 200, NSImage(data: data) != nil else { return nil }
            try? data.write(to: path, options: .atomic)
            return NSImage(data: data)
        }
        inflight[url] = t
        let img = await t.value
        inflight[url] = nil
        if let img { memory[url] = img } else { failedAt[url] = Date() }
        return img
    }
}

/// An animated GIF that keeps its pixels crisp when scaled.
struct GIFView: NSViewRepresentable {
    let image: NSImage
    var animates: Bool

    func makeNSView(context: Context) -> NSImageView {
        let v = NSImageView()
        v.imageScaling = .scaleProportionallyUpOrDown
        v.wantsLayer = true
        v.layer?.magnificationFilter = .nearest
        v.setContentHuggingPriority(.defaultLow, for: .horizontal)
        v.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        v.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        return v
    }

    func updateNSView(_ v: NSImageView, context: Context) {
        if v.image !== image { v.image = image }
        v.animates = animates
    }
}

/// The session's Pokémon, with its state animation. Falls back to an original drawn blob.
struct CreatureSprite: View {
    let sessionId: String
    let creature: Creature
    let status: SessionStatus
    var size: CGFloat = 44
    @State private var image: NSImage?
    @State private var phase = false
    @State private var poked = 0
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var key: String { "\(creature.species)-\(creature.stage)-\(creature.shiny)-\(sessionId)" }

    var body: some View {
        ZStack(alignment: .topTrailing) {
            Group {
                if let image {
                    GIFView(image: image, animates: !reduceMotion && status != .error)
                } else {
                    FallbackBlob(hue: Double(Creature.hashStr("\(creature.species)") % 360) / 360)
                }
            }
            .frame(width: size, height: size)
            .saturation(status == .error ? 0 : 1)
            .opacity(status == .idle ? 0.75 : 1)
            .rotationEffect(.degrees(status == .error ? 90 : (poked % 2 == 1 ? -12 : 0)))
            .offset(y: offset)
            .animation(motion, value: phase)
            .animation(.spring(response: 0.25, dampingFraction: 0.35), value: poked)
            if status == .needs {
                Text("!").font(.system(size: size * 0.32, weight: .black, design: .rounded))
                    .foregroundStyle(Theme.alertText)
                    .padding(.horizontal, 4).background(Capsule().fill(Theme.alert))
                    .offset(x: 4, y: -2)
            } else if status == .idle {
                Text("z").font(.system(size: size * 0.24, weight: .bold, design: .rounded))
                    .foregroundStyle(Theme.subtle).offset(x: 2, y: -2)
            }
        }
        .contentShape(Rectangle())
        .onTapGesture { poked += 1; Task { try? await Task.sleep(for: .milliseconds(260)); poked += 1 } }
        .task(id: key) {
            image = await SpriteCache.shared.image(creature.spriteURLs(sessionId: sessionId))
        }
        .onAppear { phase.toggle() }
        .accessibilityLabel(creature.name(sessionId: sessionId))
    }

    private var offset: CGFloat {
        guard !reduceMotion else { return 0 }
        switch status {
        case .working: return phase ? -size * 0.06 : 0
        case .needs: return phase ? -size * 0.14 : 0
        case .done: return phase ? -size * 0.2 : 0
        default: return 0
        }
    }

    private var motion: Animation? {
        guard !reduceMotion else { return nil }
        switch status {
        case .working: return .easeInOut(duration: 0.45).repeatForever(autoreverses: true)
        case .needs: return .easeInOut(duration: 0.3).repeatForever(autoreverses: true)
        case .done: return .interpolatingSpring(stiffness: 220, damping: 9).repeatCount(3, autoreverses: true)
        default: return .default
        }
    }
}

/// Original fallback art (no third-party asset): a soft blob with two eyes.
struct FallbackBlob: View {
    let hue: Double
    var body: some View {
        GeometryReader { g in
            let w = g.size.width
            ZStack {
                RoundedRectangle(cornerRadius: w * 0.38, style: .continuous)
                    .fill(Color(hue: hue, saturation: 0.55, brightness: 0.9))
                    .frame(width: w * 0.86, height: w * 0.74)
                    .offset(y: w * 0.08)
                HStack(spacing: w * 0.16) {
                    Circle().fill(.black).frame(width: w * 0.12)
                    Circle().fill(.black).frame(width: w * 0.12)
                }.offset(y: w * 0.02)
            }.frame(width: w, height: w)
        }
    }
}
