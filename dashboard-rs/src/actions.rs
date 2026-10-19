//! Session actions (resume / reveal / close), the self-update, the launchd
//! LaunchAgent, OS notifications and process stats.

use crate::app::{is_uuid, is_uuid_match, App, APP_VERSION};
use crate::payload::run_timeout;
use crate::py;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const LAUNCH_LABEL: &str = "com.claudehq.dashboard";
const UPDATE_CHECK_SECS: f64 = 600.0;

pub fn is_executable(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// shutil.which(name, path=path)
pub fn which(name: &str, path: &str) -> Option<String> {
    for d in path.split(':').filter(|d| !d.is_empty()) {
        let c = Path::new(d).join(name);
        if is_executable(&c) {
            return Some(c.to_string_lossy().into_owned());
        }
    }
    None
}

/// The path this process was started from, read once: after a rebuild has
/// replaced the file, Linux would report the old inode as "... (deleted)".
pub fn boot_exe() -> Option<PathBuf> {
    static EXE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    EXE.get_or_init(|| std::env::current_exe().ok()).clone()
}

fn mtime_ns(p: &Path) -> Option<i128> {
    let m = std::fs::metadata(p).ok()?;
    use std::os::unix::fs::MetadataExt;
    Some(m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128)
}

/// The running binary's mtime: when the code on disk moves on, the server
/// reports itself as "stale" until it restarts.
pub fn code_sig() -> Option<i128> {
    mtime_ns(&boot_exe()?)
}

/// Seconds the self-update gives `cargo build` (a cold release build of
/// claudehq takes a few minutes on a laptop).
pub const REBUILD_TIMEOUT_SECS: u64 = 900;

/// The cargo profile when `exe` is this checkout's own build
/// (<here>/dashboard-rs/target/<profile>/claudehq), else None: a binary that
/// lives anywhere else is never rebuilt behind the user's back.
pub fn checkout_profile(here: &Path, exe: &Path) -> Option<&'static str> {
    let exe = crate::state::realpath(exe);
    let target = crate::state::realpath(&here.join("dashboard-rs").join("target"));
    let rel = exe.strip_prefix(&target).ok()?;
    let parts: Vec<String> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
    match parts.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["release", "claudehq"] => Some("release"),
        ["debug", "claudehq"] => Some("debug"),
        _ => None,
    }
}

/// The newest mtime among the files the claudehq build reads
/// (Cargo.toml, Cargo.lock, src/**, assets/**).
pub fn rs_sources_mtime(here: &Path) -> Option<i128> {
    fn walk(d: &Path, best: &mut Option<i128>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, best);
            } else if let Some(t) = mtime_ns(&p) {
                *best = Some(best.map_or(t, |b| b.max(t)));
            }
        }
    }
    let root = here.join("dashboard-rs");
    let mut best = None;
    for f in ["Cargo.toml", "Cargo.lock"] {
        if let Some(t) = mtime_ns(&root.join(f)) {
            best = Some(best.map_or(t, |b: i128| b.max(t)));
        }
    }
    walk(&root.join("src"), &mut best);
    walk(&root.join("assets"), &mut best);
    best
}

/// The last few lines of a build log, for an error message.
fn log_tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(6)..].join("\n");
    py::last_chars(&tail, 600).to_string()
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

fn find_kitty() -> Option<String> {
    let h = home();
    for p in ["/Applications/kitty.app/Contents/MacOS/kitty".to_string(), format!("{}/.local/bin/kitty", h), "/opt/homebrew/bin/kitty".into(), "/usr/local/bin/kitty".into()] {
        if Path::new(&p).exists() {
            return Some(p);
        }
    }
    which("kitty", &std::env::var("PATH").unwrap_or_default())
}

/// shlex.quote
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "@%+=:,./-_".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

impl App {
    fn cwd_for_session(&self, sid: &str) -> String {
        self.payload_session(sid).and_then(|s| s.get("cwd").and_then(|c| c.as_str()).map(String::from)).unwrap_or_default()
    }

    /// Open a terminal running `claude --resume <sid>` in the session's cwd.
    pub fn action_resume(&self, sid: Option<&Value>) -> (u16, Value) {
        let sid = sid.and_then(|v| v.as_str()).unwrap_or("");
        let path = self.find_transcript(sid);
        if sid.is_empty() || !is_uuid_match(sid) || path.is_none() {
            return (400, json!({"error": "invalid or unknown sessionId"}));
        }
        if self.is_cursor(path.as_ref().unwrap()) {
            return (400, json!({"error": "Cursor sessions open in Cursor"}));
        }
        let mut cwd = self.cwd_for_session(sid);
        if cwd.is_empty() || !Path::new(&cwd).is_dir() {
            cwd = home();
        }
        if let Some(kitty) = find_kitty() {
            let spawned = Command::new(kitty)
                .args(["--single-instance", "--directory", &cwd, "zsh", "-lc", &format!("claude --resume {}; exec zsh -l", sid)])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            if let Err(e) = spawned {
                return (500, json!({"error": format!("resume failed: {}", e)}));
            }
            let _ = Command::new("open").args(["-a", "kitty"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            return (200, json!({"ok": true, "action": "resume", "sessionId": sid, "terminal": "kitty"}));
        }
        if cwd.contains('"') {
            return (400, json!({"error": "unsupported cwd"}));
        }
        let inner = format!("cd {} && claude --resume {}", shlex_quote(&cwd), sid);
        let script = format!("tell application \"Terminal\" to do script \"{}\"", inner.replace('\\', "\\\\"));
        let _ = run_timeout(Command::new("osascript").args(["-e", &script, "-e", "tell application \"Terminal\" to activate"]), 15);
        (200, json!({"ok": true, "action": "resume", "sessionId": sid, "terminal": "Terminal"}))
    }

    /// Open the session's working directory in Finder.
    pub fn action_reveal(&self, sid: Option<&Value>) -> (u16, Value) {
        let sid = sid.and_then(|v| v.as_str()).unwrap_or("");
        if sid.is_empty() || !is_uuid_match(sid) || self.find_transcript(sid).is_none() {
            return (400, json!({"error": "invalid or unknown sessionId"}));
        }
        let cwd = self.cwd_for_session(sid);
        if cwd.is_empty() || !Path::new(&cwd).is_dir() {
            return (400, json!({"error": "no folder for this session"}));
        }
        if let Err(crate::payload::RunErr::Other(e)) = run_timeout(Command::new("open").arg(&cwd), 10) {
            return (500, json!({"error": format!("reveal failed: {}", e)}));
        }
        (200, json!({"ok": true, "action": "reveal", "sessionId": sid}))
    }

    /// Terminate a live interactive Claude session by pid (never this server,
    /// init, or a process that isn't claude).
    pub fn action_close(&self, pid: Option<&Value>) -> (u16, Value) {
        let pid = match pid {
            Some(Value::Bool(_)) | None => return (400, json!({"error": "pid must be an integer"})),
            Some(v) => match py::py_int(v) {
                Some(p) => p,
                None => return (400, json!({"error": "pid must be an integer"})),
            },
        };
        if pid <= 1 || pid == std::process::id() as i64 {
            return (400, json!({"error": "refusing that pid"}));
        }
        let (agents, err) = self.get_live_agents();
        if let Some(e) = err {
            return (503, json!({"error": format!("cannot verify live sessions: {}", e)}));
        }
        let live: Vec<i64> = agents
            .iter()
            .filter_map(|a| a.as_object())
            .filter(|a| a.get("kind").filter(|v| py::truthy(v)).and_then(|v| v.as_str()).unwrap_or("interactive") == "interactive")
            .filter_map(|a| a.get("pid").and_then(py::strict_int))
            .collect();
        if !live.contains(&pid) {
            return (400, json!({"error": format!("pid {} is not a live interactive Claude session", pid)}));
        }
        let cmd = match run_timeout(Command::new("ps").args(["-p", &pid.to_string(), "-o", "command="]), 10) {
            Ok((_, out, _)) => py::strip(&out).to_lowercase(),
            Err(_) => return (500, json!({"error": "ps failed"})),
        };
        if cmd.is_empty() || !cmd.contains("claude") {
            return (400, json!({"error": format!("pid {} is not a Claude process", pid)}));
        }
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if rc != 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ESRCH) {
                return (400, json!({"error": "no such process"}));
            }
            return (500, json!({"error": format!("close failed: {}", e)}));
        }
        (200, json!({"ok": true, "action": "close", "pid": pid}))
    }

    // ---- self-update -------------------------------------------------------

    fn git(&self, args: &[&str], timeout: u64) -> Result<(i32, String, String), String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.paths.here).args(args).env("GIT_TERMINAL_PROMPT", "0");
        run_timeout(&mut cmd, timeout).map_err(|e| match e {
            crate::payload::RunErr::NotFound => "git not found".to_string(),
            crate::payload::RunErr::Timeout => "git timed out".to_string(),
            crate::payload::RunErr::Other(s) => s,
        })
    }

    fn update_status_git(&self, force: bool) -> Value {
        let mut g = self.update_cache.lock().unwrap();
        let now = py::now_ts();
        if let (false, Some(d)) = (force, &g.1) {
            if now - g.0 < UPDATE_CHECK_SECS {
                return d.clone();
            }
        }
        if !self.paths.here.join(".git").exists() {
            return json!({"ok": false, "error": "this copy of Claude HQ is not a git checkout"});
        }
        let res = (|| -> Result<Value, String> {
            let fetched = self.git(&["fetch", "--quiet", "origin"], 60)?.0 == 0;
            let up = self.git(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], 30)?;
            if up.0 != 0 {
                return Ok(json!({"ok": false, "error": "the current branch has no upstream to pull from"}));
            }
            let counts: Vec<i64> = py::split_ws(&self.git(&["rev-list", "--left-right", "--count", "HEAD...@{u}"], 30)?.1).iter().filter_map(|s| s.parse().ok()).collect();
            let (ahead, behind) = if counts.len() == 2 { (counts[0], counts[1]) } else { (0, 0) };
            let commits: Vec<Value> = py::splitlines(&self.git(&["log", "--format=%h %s", "-n", "10", "HEAD..@{u}"], 30)?.1).into_iter().map(Value::from).collect();
            Ok(json!({
                "ok": true, "fetched": fetched,
                "branch": py::strip(&self.git(&["rev-parse", "--abbrev-ref", "HEAD"], 30)?.1),
                "upstream": py::strip(&up.1),
                "head": py::strip(&self.git(&["rev-parse", "--short", "HEAD"], 30)?.1),
                "ahead": ahead, "behind": behind,
                "dirty": !py::strip(&self.git(&["status", "--porcelain", "--untracked-files=no"], 30)?.1).is_empty(),
                "commits": commits,
                "version": APP_VERSION,
            }))
        })();
        match res {
            Ok(d) => {
                if d["ok"] == true {
                    *g = (now, Some(d.clone()));
                }
                d
            }
            Err(e) => json!({"ok": false, "error": format!("git failed: {}", e)}),
        }
    }

    /// How far this checkout is behind its upstream, plus whether this process
    /// still runs the code on disk (its binary was replaced, or this checkout's
    /// Rust sources are newer than the build it runs).
    pub fn update_status(&self, force: bool) -> Value {
        let mut d = self.update_status_git(force);
        d["stale"] = Value::Bool(code_sig() != self.boot_code_sig || self.rs_binary_outdated());
        d
    }

    fn running_exe(&self) -> Option<PathBuf> {
        self.exe_hook.read().unwrap().clone().or_else(boot_exe)
    }

    /// The running binary is this checkout's build and older than its sources.
    pub fn rs_binary_outdated(&self) -> bool {
        let Some(exe) = self.running_exe() else { return false };
        if checkout_profile(&self.paths.here, &exe).is_none() {
            return false;
        }
        matches!((rs_sources_mtime(&self.paths.here), mtime_ns(&exe)), (Some(s), Some(b)) if s > b)
    }

    /// Did anything under dashboard-rs/ change between two commits? A failing
    /// `git diff` counts as a change (rebuilding is the safe side).
    pub fn rs_changed_between(&self, old: &str, new: &str) -> bool {
        !matches!(self.git(&["diff", "--quiet", old, new, "--", "dashboard-rs"], 30), Ok((0, _, _)))
    }

    /// cargo on PATH, else rustup's ~/.cargo/bin/cargo (launchd's PATH has no ~/.cargo/bin).
    fn cargo_path(&self) -> Option<PathBuf> {
        if let Some(c) = self.cargo_hook.read().unwrap().clone() {
            return Some(c);
        }
        if let Some(c) = which("cargo", &std::env::var("PATH").unwrap_or_default()) {
            return Some(PathBuf::from(c));
        }
        let c = PathBuf::from(home()).join(".cargo/bin/cargo");
        is_executable(&c).then_some(c)
    }

    /// Rebuild the running claudehq in place with cargo, so the restart that
    /// follows runs the new code. Err(why + how to fix it by hand).
    pub fn rebuild_rs(&self) -> Result<(), String> {
        let exe = self.running_exe();
        let profile = exe.as_deref().and_then(|e| checkout_profile(&self.paths.here, e));
        let hint = |p: Option<&str>| {
            format!("; run cargo build{} --manifest-path dashboard-rs/Cargo.toml", if p == Some("debug") { "" } else { " --release" })
        };
        let Some(profile) = profile else {
            let at = exe.map(|e| e.display().to_string()).unwrap_or_else(|| "an unknown path".into());
            return Err(format!("it runs from {}, not from this checkout's dashboard-rs/target{} and restart it", at, hint(None)));
        };
        let Some(cargo) = self.cargo_path() else {
            return Err(format!("cargo was not found (install Rust from rustup.rs){}", hint(Some(profile))));
        };
        let mut cmd = Command::new(cargo);
        cmd.arg("build");
        if profile == "release" {
            cmd.arg("--release");
        }
        cmd.arg("--locked").arg("--manifest-path").arg(self.paths.here.join("dashboard-rs").join("Cargo.toml")).current_dir(&self.paths.here);
        match run_timeout(&mut cmd, REBUILD_TIMEOUT_SECS) {
            Ok((0, _, _)) => {
                // cargo leaves the binary alone when nothing it tracks changed;
                // touch it so "stale" (sources newer than the binary) settles.
                if let Some(e) = self.running_exe() {
                    if let Ok(f) = std::fs::File::open(&e) {
                        let _ = f.set_modified(std::time::SystemTime::now());
                    }
                }
                Ok(())
            }
            Ok((rc, out, err)) => {
                let tail = log_tail(&format!("{}\n{}", out, err));
                Err(format!("{}{}", if tail.is_empty() { format!("cargo build exited with {}", rc) } else { tail }, hint(Some(profile))))
            }
            Err(crate::payload::RunErr::Timeout) => Err(format!("cargo build took longer than {} s{}", REBUILD_TIMEOUT_SECS, hint(Some(profile)))),
            Err(crate::payload::RunErr::NotFound) => Err(format!("cargo was not found{}", hint(Some(profile)))),
            Err(crate::payload::RunErr::Other(e)) => Err(format!("{}{}", e, hint(Some(profile)))),
        }
    }

    fn schedule_restart(&self) {
        let hook = self.restart_hook.read().unwrap().clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            match hook {
                Some(h) => h(),
                None => restart_self(),
            }
        });
    }

    /// Fast-forward to the upstream and restart; refuses local edits and
    /// diverged history.
    pub fn update_and_restart(&self) -> (u16, Value) {
        let st = self.update_status(true);
        let with = |mut v: Value, extra: Value| {
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            v
        };
        // Rust only: a restart must run the code on disk, so a build that is
        // older than this checkout's sources is rebuilt first.
        let rebuild_failed = |st: Value, why: String| (409, with(st, json!({"ok": false, "restarting": false, "error": format!("could not rebuild claudehq: {}", why)})));
        if st["ok"] != true {
            if st["stale"] == true {
                if self.rs_binary_outdated() {
                    if let Err(why) = self.rebuild_rs() {
                        return rebuild_failed(json!({"updated": false}), why);
                    }
                }
                self.schedule_restart();
                return (200, json!({"ok": true, "updated": false, "restarting": true}));
            }
            return (409, st);
        }
        let behind = st["behind"].as_i64().unwrap_or(0);
        let ahead = st["ahead"].as_i64().unwrap_or(0);
        if behind == 0 && st["stale"] == true {
            if self.rs_binary_outdated() {
                if let Err(why) = self.rebuild_rs() {
                    return rebuild_failed(with(st, json!({"updated": false})), why);
                }
            }
            *self.update_cache.lock().unwrap() = (0.0, None);
            self.schedule_restart();
            return (200, with(st, json!({"updated": false, "restarting": true})));
        }
        if st["dirty"] == true {
            return (409, with(st, json!({"error": "you have local changes in the Claude HQ folder; commit or stash them first"})));
        }
        if ahead > 0 && behind > 0 {
            let up = st["upstream"].as_str().unwrap_or("").to_string();
            return (409, with(st, json!({"error": format!("your branch and {} have diverged; pull by hand", up)})));
        }
        if behind == 0 {
            return (200, with(st, json!({"updated": false})));
        }
        let old = st["head"].clone();
        match self.git(&["merge", "--ff-only", "@{u}"], 60) {
            Ok((0, _, _)) => {}
            Ok((_, _, err)) => {
                let e = py::take_chars(py::strip(&err), 300);
                return (409, with(st, json!({"error": format!("pull failed: {}", if e.is_empty() { "unknown error".into() } else { e })})));
            }
            Err(e) => return (409, with(st, json!({"error": format!("pull failed: {}", e)}))),
        }
        let new = self.git(&["rev-parse", "--short", "HEAD"], 30).map(|r| py::strip(&r.1).to_string()).unwrap_or_default();
        *self.update_cache.lock().unwrap() = (0.0, None);
        let done = json!({"ok": true, "updated": true, "from": old, "to": new, "commits": st["commits"], "count": behind, "restarting": true});
        // The pull brought new Rust code (or the build was already behind):
        // rebuild before the restart, or keep running and say how to finish.
        if self.rs_changed_between(old.as_str().unwrap_or("HEAD"), &new) || self.rs_binary_outdated() {
            if let Err(why) = self.rebuild_rs() {
                return (409, with(done, json!({"ok": false, "restarting": false, "error": format!("updated the code but could not rebuild claudehq: {}", why)})));
            }
        }
        self.schedule_restart();
        (200, done)
    }

    // ---- stats ---------------------------------------------------------------

    /// This server's own numbers, for Settings.
    pub fn server_stats(&self) -> Value {
        let pid = std::process::id();
        let (cpu, rss) = match run_timeout(Command::new("ps").args(["-o", "%cpu=,rss=", "-p", &pid.to_string()]), 3) {
            Ok((_, out, _)) => {
                let parts = py::split_ws(&out);
                match (parts.first().and_then(|s| s.parse::<f64>().ok()), parts.get(1).and_then(|s| s.parse::<f64>().ok())) {
                    (Some(c), Some(r)) => (json!(c), json!(py::round(r / 1024.0, 1))),
                    _ => (Value::Null, Value::Null),
                }
            }
            Err(_) => (Value::Null, Value::Null),
        };
        let ram = if cfg!(target_os = "macos") {
            run_timeout(Command::new("sysctl").args(["-n", "hw.memsize"]), 3)
                .ok()
                .and_then(|(_, o, _)| py::strip(&o).parse::<f64>().ok())
                .map(|b| json!(py::round_int(b / 1048576.0)))
                .unwrap_or(Value::Null)
        } else {
            std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|s| s.lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| py::split_ws(l).get(1).and_then(|x| x.parse::<f64>().ok())))
                .map(|kb| json!(py::round_int(kb / 1024.0)))
                .unwrap_or(Value::Null)
        };
        let mut load = [0f64; 3];
        let load = if unsafe { libc::getloadavg(load.as_mut_ptr(), 3) } == 3 { json!(load.iter().map(|x| py::round(*x, 2)).collect::<Vec<_>>()) } else { Value::Null };
        let threads = run_timeout(Command::new("ps").args(["-M", "-p", &pid.to_string()]), 3)
            .ok()
            .map(|(_, o, _)| o.lines().count().saturating_sub(1))
            .filter(|n| *n > 0)
            .map(|n| json!(n))
            .unwrap_or(Value::Null);
        json!({
            "version": APP_VERSION, "python": format!("n/a (Rust {})", env!("CARGO_PKG_VERSION")), "pid": pid, "boot": self.boot_id,
            "uptimeSecs": (py::now_ts() - self.proc_started) as i64, "cpuPct": cpu, "rssMb": rss,
            "machineRamMb": ram, "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1), "load": load,
            "threads": threads, "platform": if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS },
            "impl": "rust",
        })
    }
}

/// Replace this process with a fresh one: under launchd ask it to restart us
/// (KeepAlive brings it back), otherwise re-exec.
pub fn restart_self() {
    if unsafe { libc::getppid() } == 1 && cfg!(target_os = "macos") {
        let uid = unsafe { libc::getuid() };
        if Command::new("launchctl")
            .args(["kickstart", "-k", &format!("gui/{}/{}", uid, LAUNCH_LABEL)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    use std::os::unix::process::CommandExt;
    if let Some(exe) = boot_exe() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let e = Command::new(exe).args(args).exec();
        eprintln!("restart failed: {}", e);
    }
}

pub fn launch_plist_path() -> PathBuf {
    PathBuf::from(home()).join(format!("Library/LaunchAgents/{}.plist", LAUNCH_LABEL))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The LaunchAgent plist, pointing at this Rust binary.
pub fn render_plist(port: u16, here: &Path) -> String {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "claudehq".into());
    let log = here.join("claude-hq.log");
    let err = here.join("claude-hq.err.log");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
  <key>Label</key>\n  <string>{label}</string>\n\
  <key>ProgramArguments</key>\n  <array>\n\
    <string>{exe}</string>\n\
    <string>--root</string>\n    <string>{root}</string>\n\
    <string>--port</string>\n    <string>{port}</string>\n\
    <string>--no-open</string>\n  </array>\n\
  <key>EnvironmentVariables</key>\n  <dict>\n\
    <key>PATH</key>\n    <string>{path}</string>\n  </dict>\n\
  <key>RunAtLoad</key>\n  <true/>\n\
  <key>KeepAlive</key>\n  <true/>\n\
  <key>StandardOutPath</key>\n  <string>{log}</string>\n\
  <key>StandardErrorPath</key>\n  <string>{err}</string>\n\
</dict>\n</plist>\n",
        label = LAUNCH_LABEL,
        exe = xml_escape(&exe),
        root = xml_escape(&here.to_string_lossy()),
        port = port,
        path = xml_escape(&format!("{}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin", home())),
        log = xml_escape(&log.to_string_lossy()),
        err = xml_escape(&err.to_string_lossy()),
    )
}

pub fn install_launchagent(port: u16, here: &Path) -> std::io::Result<()> {
    let plist = launch_plist_path();
    std::fs::create_dir_all(plist.parent().unwrap())?;
    std::fs::write(&plist, render_plist(port, here))?;
    let p = plist.to_string_lossy().into_owned();
    let _ = Command::new("launchctl").args(["unload", &p]).output();
    let r = Command::new("launchctl").args(["load", &p]).output();
    println!("Installed LaunchAgent: {}", p);
    println!("Claude HQ will now start at login on http://127.0.0.1:{}", port);
    if let Ok(r) = r {
        let e = String::from_utf8_lossy(&r.stderr);
        if !r.status.success() && !py::strip(&e).is_empty() {
            println!("launchctl load said: {}", py::strip(&e));
        }
    }
    Ok(())
}

pub fn uninstall_launchagent() {
    let plist = launch_plist_path();
    let p = plist.to_string_lossy().into_owned();
    if plist.exists() {
        let _ = Command::new("launchctl").args(["unload", &p]).output();
        let _ = std::fs::remove_file(&plist);
        println!("Removed LaunchAgent: {}", p);
    } else {
        println!("No LaunchAgent installed ({} not found).", p);
    }
}

/// An AppleScript string literal: " and \ escaped, non-ASCII kept, control
/// characters dropped.
pub fn applescript_str(s: &str) -> String {
    let kept = py::printable_only(s);
    crate::pyjson::dumps_with(&Value::from(kept), crate::pyjson::Style { indent: None, compact: false, ensure_ascii: false })
}

/// Native macOS notification: nudges ping, gifts arrive silently.
pub fn notify(title: &str, body: &str, sound: bool) -> Result<(), String> {
    let script = format!(
        "display notification {} with title {}{}",
        applescript_str(body),
        applescript_str(title),
        if sound { " sound name \"Ping\"" } else { "" }
    );
    run_timeout(Command::new("osascript").args(["-e", &script]), 10).map(|_| ()).map_err(|_| "osascript failed".into())
}

/// The session id is a validated UUID before anything is spawned.
pub fn valid_sid(s: &str) -> bool {
    is_uuid(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::*;

    #[test]
    fn applescript_quotes() {
        assert_eq!(applescript_str("a \"quoted\" \\ note\u{7}\n"), "\"a \\\"quoted\\\" \\\\ note\"");
        assert!(!applescript_str("\u{1F381} Gary").contains("\\u"));
    }

    #[test]
    fn close_rejects_bad_pids() {
        let (_d, app) = app();
        for bad in [json!(null), json!("x"), json!(true), json!(0), json!(1), json!(-5)] {
            assert_eq!(app.action_close(Some(&bad)).0, 400, "{}", bad);
        }
        let me = std::process::id() as i64;
        *app.agents_hook.write().unwrap() = Some(std::sync::Arc::new(move || (vec![json!({"kind": "interactive", "pid": me})], None)));
        let (code, resp) = app.action_close(Some(&json!(me)));
        assert_eq!(code, 400);
        assert!(resp["error"].as_str().unwrap().contains("refusing"));
        *app.agents_hook.write().unwrap() = Some(std::sync::Arc::new(|| (vec![], Some("claude CLI not found on PATH".into()))));
        let (code, resp) = app.action_close(Some(&json!(99999)));
        assert_eq!(code, 503);
        assert!(resp["error"].as_str().unwrap().contains("claude CLI not found"));
    }

    #[test]
    fn close_checks_the_list_and_the_command() {
        let (_d, app) = app();
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id() as i64;
        *app.agents_hook.write().unwrap() = Some(std::sync::Arc::new(move || (vec![json!({"kind": "background", "pid": pid})], None)));
        assert!(app.action_close(Some(&json!(pid))).1["error"].as_str().unwrap().contains("not a live interactive"));
        *app.agents_hook.write().unwrap() = Some(std::sync::Arc::new(move || (vec![json!({"pid": pid})], None)));
        assert!(app.action_close(Some(&json!(pid))).1["error"].as_str().unwrap().contains("not a Claude process"));
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn plist_points_at_this_binary() {
        let p = render_plist(8765, Path::new("/repo"));
        assert!(p.contains("<string>--port</string>\n    <string>8765</string>"));
        assert!(p.contains("<string>/repo</string>"));
        assert!(p.contains(LAUNCH_LABEL));
    }

    fn git(cwd: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(ok.status.success(), "{:?}", ok);
    }

    #[test]
    fn self_update_fast_forwards_and_refuses_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let other = tmp.path().join("other");
        let mine = tmp.path().join("mine");
        assert!(Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&origin).status().unwrap().success());
        assert!(Command::new("git").args(["clone", "-q"]).arg(&origin).arg(&other).output().unwrap().status.success());
        std::fs::write(other.join("a.txt"), "one").unwrap();
        git(&other, &["add", "a.txt"]);
        git(&other, &["commit", "-q", "-m", "first"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        assert!(Command::new("git").args(["clone", "-q"]).arg(&origin).arg(&mine).output().unwrap().status.success());
        let (_d, base) = app();
        let mut paths = base.paths.clone();
        paths.here = mine.clone();
        let app = App::new(paths);
        let restarts = std::sync::Arc::new(std::sync::Mutex::new(0));
        let r2 = restarts.clone();
        *app.restart_hook.write().unwrap() = Some(std::sync::Arc::new(move || *r2.lock().unwrap() += 1));
        let st = app.update_status(true);
        assert_eq!((st["behind"].clone(), st["ahead"].clone(), st["dirty"].clone(), st["stale"].clone()), (json!(0), json!(0), json!(false), json!(false)));
        assert_eq!(app.update_and_restart().1["updated"], false);
        std::fs::write(other.join("a.txt"), "second").unwrap();
        git(&other, &["commit", "-qam", "second"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        assert_eq!(app.update_status(false)["behind"], 0); // cached
        let st = app.update_status(true);
        assert_eq!(st["behind"], 1);
        assert!(st["commits"][0].as_str().unwrap().ends_with(" second"));
        std::fs::write(mine.join("a.txt"), "my edit").unwrap();
        let (code, resp) = app.update_and_restart();
        assert_eq!(code, 409);
        assert!(resp["error"].as_str().unwrap().contains("local changes"));
        git(&mine, &["checkout", "--", "a.txt"]);
        let (code, resp) = app.update_and_restart();
        assert_eq!(code, 200);
        assert_eq!((resp["updated"].clone(), resp["restarting"].clone()), (json!(true), json!(true)));
        assert_eq!(std::fs::read_to_string(mine.join("a.txt")).unwrap(), "second");
        std::thread::sleep(std::time::Duration::from_millis(1300));
        assert_eq!(*restarts.lock().unwrap(), 1);
    }

    #[test]
    fn only_this_checkouts_build_is_rebuilt() {
        let here = Path::new("/hq");
        assert_eq!(checkout_profile(here, Path::new("/hq/dashboard-rs/target/release/claudehq")), Some("release"));
        assert_eq!(checkout_profile(here, Path::new("/hq/dashboard-rs/target/debug/claudehq")), Some("debug"));
        assert_eq!(checkout_profile(here, Path::new("/hq/dashboard-rs/target/debug/deps/claudehq-0123abcd")), None);
        assert_eq!(checkout_profile(here, Path::new("/usr/local/bin/claudehq")), None);
        assert_eq!(checkout_profile(here, Path::new("/other/dashboard-rs/target/release/claudehq")), None);
        assert_eq!(log_tail("a\n\nb\nc\nd\ne\nf\ng\n"), "b\nc\nd\ne\nf\ng");
    }

    fn set_mtime(p: &Path, t: std::time::SystemTime) {
        std::fs::File::open(p).unwrap().set_modified(t).unwrap();
    }

    /// A stand-in cargo: logs its arguments, then exits with `rc` after printing `msg`.
    fn fake_cargo(dir: &Path, rc: i32, msg: &str) -> PathBuf {
        let p = dir.join(format!("cargo-{}", rc));
        let log = dir.join("cargo.log");
        std::fs::write(&p, format!("#!/bin/sh\necho \"$@\" >> '{}'\necho '{}' >&2\nexit {}\n", log.display(), msg, rc)).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn self_update_rebuilds_new_rust_code_before_restarting() {
        let tmp = tempfile::tempdir().unwrap();
        let (origin, other, mine) = (tmp.path().join("origin.git"), tmp.path().join("other"), tmp.path().join("mine"));
        assert!(Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&origin).status().unwrap().success());
        assert!(Command::new("git").args(["clone", "-q"]).arg(&origin).arg(&other).output().unwrap().status.success());
        std::fs::create_dir_all(other.join("dashboard-rs/src")).unwrap();
        std::fs::write(other.join("dashboard-rs/src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(other.join("dashboard-rs/Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(other.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(other.join("a.txt"), "one").unwrap();
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "first"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        assert!(Command::new("git").args(["clone", "-q"]).arg(&origin).arg(&mine).output().unwrap().status.success());

        let (_d, base) = app();
        let mut paths = base.paths.clone();
        paths.here = mine.clone();
        let app = App::new(paths);
        let exe = mine.join("dashboard-rs/target/release/claudehq");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "old binary").unwrap();
        let now = std::time::SystemTime::now;
        set_mtime(&exe, now() + std::time::Duration::from_secs(5)); // built after the checkout
        *app.exe_hook.write().unwrap() = Some(exe.clone());
        let restarts = std::sync::Arc::new(std::sync::Mutex::new(0));
        let r2 = restarts.clone();
        *app.restart_hook.write().unwrap() = Some(std::sync::Arc::new(move || *r2.lock().unwrap() += 1));
        let log = tmp.path().join("cargo.log");
        *app.cargo_hook.write().unwrap() = Some(fake_cargo(tmp.path(), 0, "Finished"));
        assert_eq!(app.update_status(true)["stale"], false);

        // 1. A pull that does not touch dashboard-rs/: no build, straight to the restart.
        std::fs::write(other.join("a.txt"), "two").unwrap();
        git(&other, &["commit", "-qam", "page only"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        let (code, resp) = app.update_and_restart();
        assert_eq!((code, resp["restarting"].clone()), (200, json!(true)));
        assert!(!log.exists(), "cargo ran for a page-only pull");

        // 2. New Rust code and a build that fails: the code stays pulled, no restart, the fix is named.
        std::fs::write(other.join("dashboard-rs/src/main.rs"), "fn main() { new() }\n").unwrap();
        git(&other, &["commit", "-qam", "rust change"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        *app.cargo_hook.write().unwrap() = Some(fake_cargo(tmp.path(), 101, "error[E0425]: cannot find function `new`"));
        let (code, resp) = app.update_and_restart();
        assert_eq!(code, 409);
        assert_eq!((resp["updated"].clone(), resp["restarting"].clone(), resp["ok"].clone()), (json!(true), json!(false), json!(false)));
        let err = resp["error"].as_str().unwrap();
        assert!(err.starts_with("updated the code but could not rebuild claudehq: "), "{}", err);
        assert!(err.contains("error[E0425]") && err.ends_with("run cargo build --release --manifest-path dashboard-rs/Cargo.toml"), "{}", err);
        assert_eq!(std::fs::read_to_string(mine.join("dashboard-rs/src/main.rs")).unwrap(), "fn main() { new() }\n");
        assert!(std::fs::read_to_string(&log).unwrap().contains("build --release --locked --manifest-path"));

        // 3. Up to date, but the build is older than the sources: "stale"; the restart rebuilds first.
        set_mtime(&exe, now() - std::time::Duration::from_secs(60));
        let st = app.update_status(true);
        assert_eq!((st["behind"].clone(), st["stale"].clone()), (json!(0), json!(true)));
        let (code, resp) = app.update_and_restart();
        assert_eq!(code, 409);
        assert!(resp["error"].as_str().unwrap().starts_with("could not rebuild claudehq: error[E0425]"), "{}", resp);
        *app.cargo_hook.write().unwrap() = Some(fake_cargo(tmp.path(), 0, "Finished"));
        let (code, resp) = app.update_and_restart();
        assert_eq!((code, resp["restarting"].clone()), (200, json!(true)));
        assert!(!app.rs_binary_outdated()); // the build (here: the touch) settled it
        *app.cargo_hook.write().unwrap() = Some(tmp.path().join("no-such-cargo"));
        set_mtime(&exe, now() - std::time::Duration::from_secs(60));
        assert!(app.update_and_restart().1["error"].as_str().unwrap().contains("cargo was not found"));

        // 4. A claudehq that is not this checkout's build is never rebuilt behind the user's back.
        set_mtime(&exe, now() + std::time::Duration::from_secs(5));
        *app.exe_hook.write().unwrap() = Some(PathBuf::from("/usr/local/bin/claudehq"));
        std::fs::write(other.join("dashboard-rs/src/main.rs"), "fn main() {}\n").unwrap();
        git(&other, &["commit", "-qam", "rust fix"]);
        git(&other, &["push", "-q", "origin", "HEAD:main"]);
        let (code, resp) = app.update_and_restart();
        assert_eq!(code, 409);
        assert!(resp["error"].as_str().unwrap().contains("it runs from /usr/local/bin/claudehq, not from this checkout's dashboard-rs/target"), "{}", resp);
        std::thread::sleep(std::time::Duration::from_millis(1300));
        assert_eq!(*restarts.lock().unwrap(), 2);
    }
}
