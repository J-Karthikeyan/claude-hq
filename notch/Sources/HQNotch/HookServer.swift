import Darwin
import Foundation
import HQNotchCore

/// Unix-socket server the hook relay talks to. One JSON line per connection.
/// Fire-and-forget events are read and closed; a PermissionRequest keeps its connection
/// open until `reply(id:)` writes the decision, or the hook gives up (EOF -> onGone).
/// Socket: ~/Library/Application Support/HQNotch/hq.sock, dir 0700, socket 0600, and every
/// peer's uid is checked, so only this user's processes can talk to it.
final class HookServer: @unchecked Sendable {
    static let dir = ("~/Library/Application Support/HQNotch" as NSString).expandingTildeInPath
    static let path = dir + "/hq.sock"

    /// Called on the main queue. `id` is non-nil when the hook is waiting for a reply.
    var onMessage: (HookMessage, Int?) -> Void = { _, _ in }
    /// Called on the main queue when a waiting hook disconnects without a reply.
    var onGone: (Int) -> Void = { _ in }

    private let q = DispatchQueue(label: "hqnotch.socket")
    private var listenFD: Int32 = -1
    private var acceptSource: DispatchSourceRead?
    private var waiting: [Int: (fd: Int32, src: DispatchSourceRead)] = [:]
    private var nextId = 1

    func start() throws {
        try FileManager.default.createDirectory(atPath: Self.dir, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        chmod(Self.dir, 0o700)
        unlink(Self.path)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw POSIXError(.EIO) }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let bytes = Array(Self.path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { close(fd); throw POSIXError(.ENAMETOOLONG) }
        withUnsafeMutableBytes(of: &addr.sun_path) { raw in
            raw.copyBytes(from: bytes)
            raw[bytes.count] = 0
        }
        let len = socklen_t(MemoryLayout<sockaddr_un>.size)
        let ok = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, len) }
        }
        guard ok == 0 else { close(fd); throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
        chmod(Self.path, 0o600)
        guard listen(fd, 64) == 0 else { close(fd); throw POSIXError(.EIO) }
        _ = fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK)
        listenFD = fd
        let src = DispatchSource.makeReadSource(fileDescriptor: fd, queue: q)
        src.setEventHandler { [weak self] in self?.acceptAll() }
        src.resume()
        acceptSource = src
    }

    func stop() {
        q.sync {
            acceptSource?.cancel()
            for (_, w) in waiting { w.src.cancel() }   // cancel handler closes the fd
            waiting.removeAll()
            if listenFD >= 0 { close(listenFD); listenFD = -1 }
            unlink(Self.path)
        }
    }

    /// Answer a waiting hook. `reply` is the JSON object the hook's decision_output() reads.
    func reply(id: Int, _ reply: [String: Any]) {
        q.async { [self] in
            guard let w = waiting.removeValue(forKey: id) else { return }
            if var data = try? JSONSerialization.data(withJSONObject: reply) {
                data.append(0x0A)
                data.withUnsafeBytes { p in _ = write(w.fd, p.baseAddress, p.count) }
            }
            w.src.cancel()
        }
    }

    private func acceptAll() {
        while true {
            let cfd = accept(listenFD, nil, nil)
            if cfd < 0 { return }
            var uid: uid_t = 0, gid: gid_t = 0
            guard getpeereid(cfd, &uid, &gid) == 0, uid == getuid() else { close(cfd); continue }
            // BSD accept() inherits O_NONBLOCK from the listener; the reader wants blocking + timeout.
            _ = fcntl(cfd, F_SETFL, fcntl(cfd, F_GETFL) & ~O_NONBLOCK)
            var nosig: Int32 = 1
            setsockopt(cfd, SOL_SOCKET, SO_NOSIGPIPE, &nosig, socklen_t(MemoryLayout<Int32>.size))
            DispatchQueue.global(qos: .userInitiated).async { [weak self] in self?.handle(cfd) }
        }
    }

    /// Read one line (the hook sends it immediately; 2 s cap), then dispatch.
    private func handle(_ fd: Int32) {
        var tv = timeval(tv_sec: 2, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        var buf = Data()
        var chunk = [UInt8](repeating: 0, count: 65536)
        while !buf.contains(0x0A), buf.count < (1 << 20) {
            let n = read(fd, &chunk, chunk.count)
            if n <= 0 { break }
            buf.append(contentsOf: chunk[0..<n])
        }
        let line = buf.firstIndex(of: 0x0A).map { buf.prefix(upTo: $0) } ?? buf
        let msg: HookMessage
        do { msg = try JSONDecoder().decode(HookMessage.self, from: line) } catch {
            NSLog("HQNotch: dropped a hook line (%d bytes): %@", line.count, String(describing: error))
            close(fd)
            return
        }
        guard msg.wantsReply == true else {
            close(fd)
            DispatchQueue.main.async { [weak self] in self?.onMessage(msg, nil) }
            return
        }
        q.async { [self] in
            let id = nextId; nextId += 1
            // Watch for the hook going away (its timeout, or Claude Code cancelling).
            let src = DispatchSource.makeReadSource(fileDescriptor: fd, queue: q)
            src.setEventHandler { [weak self] in
                var b: UInt8 = 0
                if recv(fd, &b, 1, MSG_PEEK | MSG_DONTWAIT) <= 0 { self?.gone(id) }
            }
            src.setCancelHandler { close(fd) }
            waiting[id] = (fd, src)
            src.resume()
            DispatchQueue.main.async { [weak self] in self?.onMessage(msg, id) }
        }
    }

    private func gone(_ id: Int) {
        guard let w = waiting.removeValue(forKey: id) else { return }
        w.src.cancel()
        DispatchQueue.main.async { [weak self] in self?.onGone(id) }
    }
}
