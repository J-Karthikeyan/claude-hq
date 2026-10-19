//! HQ Notch, from HQ (ext_notch.py): is it on this Mac, and install it with
//! one click.
//!
//! GET  /api/notch/status   -> {mac, installed, running, tools, available, job}
//! POST /api/notch/install  -> runs notch/tools/install.py in the background (macOS only)
//!
//! The install is the same as running `python3 notch/tools/install.py` by hand
//! (cwd = the HQ folder). It only ever runs because the user clicked Install in
//! HQ (a POST through the CSRF + Origin + Host gates). Nothing here talks to the
//! network.
//!
//! Interpreter: Python runs the installer with `sys.executable`. The Rust
//! server has no interpreter of its own, so it uses `/usr/bin/python3` when that
//! exists (the Command Line Tools one: the install already requires them for
//! `swift`, the installer pins its hooks to it, and launchd's PATH would not
//! find a Homebrew python anyway), else `python3` from PATH.

use crate::actions::{is_executable, which};
use crate::app::App;
use crate::ext::Resp;
use crate::payload::{run_timeout, RunErr};
use crate::py;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, RwLock};

/// How much of the installer's output the page may show (characters).
pub const LOG_MAX: usize = 2000;
/// A first build takes a minute or two; never hang forever.
pub const TIMEOUT_SECS: u64 = 900;

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub state: &'static str,
    pub started: Option<f64>,
    pub rc: Option<i64>,
    pub log: String,
}

impl Default for Job {
    fn default() -> Job {
        Job { state: "idle", started: None, rc: None, log: String::new() }
    }
}

#[derive(Default)]
pub struct NotchState {
    pub job: Mutex<Job>,
    /// Test seams: sys.platform == "darwin" and shutil.which("swift").
    pub mac_hook: RwLock<Option<bool>>,
    pub swift_hook: RwLock<Option<bool>>,
}

/// text=True output: universal newlines.
fn text_mode(b: &str) -> String {
    b.replace("\r\n", "\n").replace('\r', "\n")
}

fn interpreter() -> String {
    if is_executable(Path::new("/usr/bin/python3")) {
        "/usr/bin/python3".into()
    } else {
        "python3".into()
    }
}

impl App {
    fn notch_mac(&self) -> bool {
        self.notch.mac_hook.read().unwrap().unwrap_or(cfg!(target_os = "macos"))
    }

    fn notch_tools(&self) -> bool {
        if let Some(v) = *self.notch.swift_hook.read().unwrap() {
            return v;
        }
        let path = std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".into());
        which("swift", &path).is_some()
    }

    fn notch_running(&self) -> bool {
        if !self.notch_mac() {
            return false;
        }
        matches!(run_timeout(Command::new("pgrep").args(["-x", "HQNotch"]), 3), Ok((0, _, _)))
    }

    pub fn notch_job_view(&self) -> Value {
        let j = self.notch.job.lock().unwrap().clone();
        json!({"state": j.state, "started": j.started, "rc": j.rc, "log": py::last_chars(&j.log, LOG_MAX)})
    }

    /// ext_notch.status
    pub fn notch_status(&self) -> Value {
        let mac = self.notch_mac();
        json!({
            "mac": mac,
            "installed": mac && self.paths.notch_app.is_dir(),
            "running": self.notch_running(),
            "tools": self.notch_tools(),
            "available": self.paths.notch_installer.is_file(),
            "job": self.notch_job_view(),
        })
    }

    /// ext_notch.install: start the installer once. 202 started, 409 already
    /// running, 400 when it cannot run here.
    pub fn notch_install(self: &std::sync::Arc<Self>) -> Resp {
        if !self.notch_mac() {
            return (400, json!({"error": "HQ Notch is for macOS"}));
        }
        if !self.paths.notch_installer.is_file() {
            return (400, json!({"error": "notch/tools/install.py is missing from this HQ"}));
        }
        if !self.notch_tools() {
            return (400, json!({"error": "needs the Xcode Command Line Tools: run xcode-select --install, then try again"}));
        }
        {
            let mut j = self.notch.job.lock().unwrap();
            if j.state == "running" {
                return (409, json!({"error": "already installing"}));
            }
            *j = Job { state: "running", started: Some(py::now_ts()), rc: None, log: String::new() };
        }
        let app = self.clone();
        let spawned = std::thread::Builder::new().name("hq-notch-install".into()).spawn(move || app.notch_run());
        if let Err(e) = spawned {
            *self.notch.job.lock().unwrap() = Job::default();
            return (500, json!({"error": format!("could not start the install: {}", e)}));
        }
        (202, json!({"started": true}))
    }

    fn notch_run(&self) {
        let mut cmd = Command::new(interpreter());
        cmd.arg(&self.paths.notch_installer).current_dir(&self.paths.here);
        let (out, rc) = match run_timeout(&mut cmd, TIMEOUT_SECS) {
            Ok((rc, so, se)) => (text_mode(&so) + &text_mode(&se), rc as i64),
            Err(RunErr::Timeout) => (format!("the install took longer than {} s and was stopped", TIMEOUT_SECS), -1),
            Err(RunErr::NotFound) => ("could not run the installer: python3 not found".to_string(), -1),
            Err(RunErr::Other(e)) => (format!("could not run the installer: {}", e), -1),
        };
        let mut j = self.notch.job.lock().unwrap();
        j.state = if rc == 0 { "done" } else { "failed" };
        j.rc = Some(rc);
        j.log = py::last_chars(&out, LOG_MAX).to_string();
    }
}

/// GET /api/notch/status (registered in ext::GET, like ext_notch.GET).
pub fn get_status(app: &App, _arg: &dyn Fn(&str) -> String) -> Resp {
    (200, app.notch_status())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testutil::app;
    use std::sync::Arc;

    fn wait(app: &App) -> Value {
        for _ in 0..200 {
            if app.notch.job.lock().unwrap().state != "running" {
                return app.notch_job_view();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("installer never finished");
    }

    /// An App whose installer is a stand-in script, on a pretend Mac with swift.
    fn fake(body: &str) -> (tempfile::TempDir, Arc<App>) {
        let (d, base) = app();
        let mut p = base.paths.clone();
        p.notch_installer = d.path().join("install.py");
        std::fs::write(&p.notch_installer, body).unwrap();
        let a = Arc::new(App::new(p));
        *a.notch.mac_hook.write().unwrap() = Some(true);
        *a.notch.swift_hook.write().unwrap() = Some(true);
        (d, a)
    }

    #[test]
    fn status_shape() {
        let (_d, a) = app();
        let s = a.notch_status();
        let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["mac", "installed", "running", "tools", "available", "job"]);
        assert_eq!(s["available"], false); // the temp HQ has no notch/
        assert_eq!(s["job"], json!({"state": "idle", "started": null, "rc": null, "log": ""}));
        // the real repo ships the installer
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let real = crate::app::Paths::new(repo, Path::new("/nonexistent-home"));
        assert!(real.notch_installer.is_file());
        assert_eq!(real.notch_app, Path::new("/nonexistent-home/Applications/HQ Notch.app"));
    }

    #[test]
    fn not_a_mac() {
        let (_d, a) = fake("print('x')\n");
        *a.notch.mac_hook.write().unwrap() = Some(false);
        std::fs::create_dir_all(&a.paths.notch_app).unwrap();
        assert_eq!(a.notch_install(), (400, json!({"error": "HQ Notch is for macOS"})));
        let s = a.notch_status();
        assert_eq!((s["installed"].clone(), s["running"].clone()), (json!(false), json!(false)));
        *a.notch.mac_hook.write().unwrap() = Some(true);
        assert_eq!(a.notch_status()["installed"], true);
    }

    #[test]
    fn needs_the_command_line_tools_and_the_installer() {
        let (_d, a) = fake("print('x')\n");
        *a.notch.swift_hook.write().unwrap() = Some(false);
        let (code, resp) = a.notch_install();
        assert_eq!(code, 400);
        assert!(resp["error"].as_str().unwrap().contains("xcode-select --install"));
        std::fs::remove_file(&a.paths.notch_installer).unwrap();
        assert_eq!(a.notch_install(), (400, json!({"error": "notch/tools/install.py is missing from this HQ"})));
        assert_eq!(a.notch.job.lock().unwrap().state, "idle");
    }

    #[test]
    fn success() {
        let (_d, a) = fake("import os; print('done. HQ Notch is running in', os.path.basename(os.getcwd()))\n");
        assert_eq!(a.notch_install(), (202, json!({"started": true})));
        let j = wait(&a);
        assert_eq!((j["state"].clone(), j["rc"].clone()), (json!("done"), json!(0)));
        let here = a.paths.here.file_name().unwrap().to_string_lossy().into_owned();
        assert!(j["log"].as_str().unwrap().contains(&format!("HQ Notch is running in {}", here)), "{}", j["log"]);
        assert!(j["started"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn failure_is_reported() {
        let (_d, a) = fake("import sys; print('swift build failed'); sys.stderr.write('boom\\r\\n'); sys.exit(3)\n");
        a.notch_install();
        let j = wait(&a);
        assert_eq!((j["state"].clone(), j["rc"].clone()), (json!("failed"), json!(3)));
        assert_eq!(j["log"], "swift build failed\nboom\n"); // stdout + stderr, universal newlines
    }

    #[test]
    fn never_twice_at_once() {
        let (_d, a) = fake("import time; time.sleep(1)\n");
        assert_eq!(a.notch_install().0, 202);
        assert_eq!(a.notch_install(), (409, json!({"error": "already installing"})));
        wait(&a);
        assert_eq!(a.notch_install().0, 202); // again once it finished
        wait(&a);
    }

    #[test]
    fn log_is_capped() {
        let (_d, a) = fake("print('x' * 50000 + 'é' * 10)\n");
        a.notch_install();
        let log = wait(&a)["log"].as_str().unwrap().to_string();
        assert_eq!(log.chars().count(), LOG_MAX);
        assert!(log.ends_with("ééé\n"));
    }
}
