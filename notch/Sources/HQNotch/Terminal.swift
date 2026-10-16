import AppKit
import Foundation
import HQNotchCore

/// Bring the session's own terminal window to the front.
/// kitty: remote-control `focus-window` (via KITTY_LISTEN_ON, or the escape code written
/// to the session's tty with no_response). Terminal.app / iTerm2: AppleScript by tty /
/// session id. tmux: select the pane. Anything else: activate the terminal app.
enum TerminalJump {
    static func focus(_ s: Session) {
        let env = s.env
        let tty = Self.tty(from: s.ppid)
        if let pane = env["TMUX_PANE"] {
            run("/usr/bin/env", ["tmux", "select-window", "-t", pane])
            run("/usr/bin/env", ["tmux", "select-pane", "-t", pane])
        }
        if let wid = env["KITTY_WINDOW_ID"], Int(wid) != nil {
            if let sock = env["KITTY_LISTEN_ON"], !sock.isEmpty {
                run(kittyBinary, ["@", "--to", sock, "focus-window", "--match", "id:\(wid)"])
            } else if let tty {
                kittyEscape(tty: tty, windowId: Int(wid)!)
            }
            activate(bundleId: "net.kovidgoyal.kitty")
            return
        }
        switch env["TERM_PROGRAM"] ?? "" {
        case "Apple_Terminal":
            if let tty { osa(terminalScript(tty: tty)) } else { activate(bundleId: "com.apple.Terminal") }
        case "iTerm.app":
            if let sid = env["ITERM_SESSION_ID"]?.split(separator: ":").last { osa(itermScript(uid: String(sid))) }
            else { activate(bundleId: "com.googlecode.iterm2") }
        default:
            if let b = env["__CFBundleIdentifier"] { activate(bundleId: b) }
        }
    }

    static var kittyBinary: String {
        let app = "/Applications/kitty.app/Contents/MacOS/kitty"
        return FileManager.default.isExecutableFile(atPath: app) ? app : "/opt/homebrew/bin/kitty"
    }

    /// Walk up from the hook's parent until a process has a controlling tty.
    static func tty(from pid: Int) -> String? {
        var p = pid
        for _ in 0..<8 where p > 1 {
            guard let out = capture("/bin/ps", ["-o", "ppid=,tty=", "-p", "\(p)"]) else { return nil }
            let f = out.split(whereSeparator: \.isWhitespace)
            guard f.count >= 2 else { return nil }
            let t = String(f[1])
            if t != "??", !t.isEmpty { return t.hasPrefix("/dev/") ? t : "/dev/" + t }
            p = Int(f[0]) ?? 0
        }
        return nil
    }

    /// kitty remote-control DCS on the window's own tty. Needs `allow_remote_control yes`.
    /// no_response keeps kitty from typing a reply into Claude Code's input.
    static func kittyEscape(tty: String, windowId: Int) {
        guard tty.range(of: #"^/dev/ttys\d+$"#, options: .regularExpression) != nil,
              let fh = FileHandle(forWritingAtPath: tty) else { return }
        let cmd = #"{"cmd":"focus-window","version":[0,30,0],"no_response":true,"payload":{"match":"id:\#(windowId)"}}"#
        fh.write(Data("\u{1B}P@kitty-cmd\(cmd)\u{1B}\\".utf8))
        try? fh.close()
    }

    static func terminalScript(tty: String) -> String {
        """
        tell application "Terminal"
          repeat with w in windows
            repeat with t in tabs of w
              if tty of t is "\(tty)" then
                set selected of t to true
                set index of w to 1
                activate
                return
              end if
            end repeat
          end repeat
          activate
        end tell
        """
    }

    static func itermScript(uid: String) -> String {
        let safe = uid.filter { $0.isHexDigit || $0 == "-" }
        return """
        tell application "iTerm2"
          repeat with w in windows
            repeat with t in tabs of w
              repeat with s in sessions of t
                if unique id of s is "\(safe)" then
                  select w
                  select t
                  select s
                  activate
                  return
                end if
              end repeat
            end repeat
          end repeat
          activate
        end tell
        """
    }

    static func activate(bundleId: String) {
        NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first?.activate()
    }

    static func osa(_ script: String) {
        DispatchQueue.global().async {
            var err: NSDictionary?
            NSAppleScript(source: script)?.executeAndReturnError(&err)
        }
    }

    @discardableResult
    static func run(_ exe: String, _ args: [String]) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = args
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        do { try p.run() } catch { return false }
        return true
    }

    static func capture(_ exe: String, _ args: [String]) -> String? {
        let p = Process(), pipe = Pipe()
        p.executableURL = URL(fileURLWithPath: exe)
        p.arguments = args
        p.standardOutput = pipe
        p.standardError = FileHandle.nullDevice
        do { try p.run() } catch { return nil }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        return p.terminationStatus == 0 ? String(data: data, encoding: .utf8) : nil
    }
}
