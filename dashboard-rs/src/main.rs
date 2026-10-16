//! claudehq [--port 8765] [--no-open] [--install] [--uninstall] [--print-plist] [--root DIR]

use claudehq::app::{App, Paths};
use claudehq::{actions, arena, ext, music, py, server};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const USAGE: &str = "usage: claudehq [-h] [--port PORT] [--no-open] [--print-plist] [--install] [--uninstall] [--root DIR]";

struct Args {
    port: u16,
    no_open: bool,
    print_plist: bool,
    install: bool,
    uninstall: bool,
    root: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut a = Args { port: 8765, no_open: false, print_plist: false, install: false, uninstall: false, root: None };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let die = |msg: &str| -> ! {
        eprintln!("{}\nclaudehq: error: {}", USAGE, msg);
        std::process::exit(2)
    };
    while i < argv.len() {
        let (flag, inline) = match argv[i].split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (argv[i].clone(), None),
        };
        let mut value = |name: &str| -> String {
            if let Some(v) = inline.clone() {
                return v;
            }
            i += 1;
            argv.get(i).cloned().unwrap_or_else(|| die(&format!("argument {}: expected one argument", name)))
        };
        match flag.as_str() {
            "-h" | "--help" => {
                println!("{}\n\nLocal Claude sessions dashboard (Rust).\n\noptions:\n  --port PORT    port on 127.0.0.1 (default 8765)\n  --no-open      do not open a browser\n  --print-plist  print the launchd LaunchAgent plist and exit\n  --install      install a launchd LaunchAgent so Claude HQ starts at login\n  --uninstall    remove the launchd LaunchAgent\n  --root DIR     the Claude HQ folder (index.html, ui/, games/, state files)", USAGE);
                std::process::exit(0);
            }
            "--port" => {
                let v = value("--port");
                a.port = v.parse().unwrap_or_else(|_| die(&format!("argument --port: invalid int value: '{}'", v)));
            }
            "--root" => a.root = Some(PathBuf::from(value("--root"))),
            "--no-open" => a.no_open = true,
            "--print-plist" => a.print_plist = true,
            "--install" => a.install = true,
            "--uninstall" => a.uninstall = true,
            other => die(&format!("unrecognized arguments: {}", other)),
        }
        i += 1;
    }
    a
}

fn looks_like_root(p: &Path) -> bool {
    p.join("index.html").is_file() && p.join("ui").is_dir()
}

/// The Claude HQ folder: --root, $CLAUDEHQ_ROOT, the folder above this binary
/// (dashboard-rs/target/<profile>/claudehq), else the crate's parent.
fn resolve_root(arg: Option<PathBuf>) -> PathBuf {
    if let Some(r) = arg.or_else(|| std::env::var_os("CLAUDEHQ_ROOT").map(PathBuf::from)) {
        return std::fs::canonicalize(&r).unwrap_or(r);
    }
    if let Ok(exe) = std::env::current_exe().and_then(std::fs::canonicalize) {
        for anc in exe.ancestors().skip(1) {
            if looks_like_root(anc) {
                return anc.to_path_buf();
            }
        }
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

fn start_music_share(app: Arc<App>) {
    let a1 = app.clone();
    let a2 = app.clone();
    let a3 = app.clone();
    *app.music.share.lock().unwrap() = Some(music::ShareLoop::new(
        Box::new(move || py::opt_truthy(a1.load_config().get("musicShare")) && a1.load_link().get("token").map(py::truthy).unwrap_or(false)),
        Box::new(move |t| a2.arena_music_now_put(t).0),
        Box::new(move || a3.arena_music_now_clear().0),
    ));
    std::thread::Builder::new()
        .name("hq-music-share".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let on = py::opt_truthy(app.load_config().get("musicShare")) && app.is_paired();
            let det = if on { app.music.current(4.0).0 } else { None };
            if let Some(s) = app.music.share.lock().unwrap().as_mut() {
                s.step(py::now_ts(), None, &|| det.clone());
            }
        })
        .ok();
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let here = resolve_root(args.root.clone());
    if args.print_plist {
        print!("{}", actions::render_plist(args.port, &here));
        return;
    }
    if args.uninstall {
        actions::uninstall_launchagent();
        return;
    }
    if args.install {
        if let Err(e) = actions::install_launchagent(args.port, &here) {
            eprintln!("could not install the LaunchAgent: {}", e);
            std::process::exit(1);
        }
        return;
    }

    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
    let app = Arc::new(App::new(Paths::new(&here, &home)));
    app.port.store(args.port, std::sync::atomic::Ordering::Relaxed);
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not listen on 127.0.0.1:{}: {}", args.port, e);
            std::process::exit(1);
        }
    };

    // Arena (multiplayer) stays dormant until the user pairs and enables it.
    arena::start_publisher(app.clone());
    ext::start_skills_reporter(app.clone());
    ext::start_loot_reporter(app.clone());
    start_music_share(app.clone());
    arena::start_nudge_poller(app.clone(), actions::notify);

    // Warm the per-file scan + search caches so the first search is instant.
    let warm = app.clone();
    std::thread::spawn(move || {
        for p in warm.iter_transcript_paths() {
            warm.scan_file(&p);
            warm.get_search_entry(&p);
        }
    });

    let url = format!("http://127.0.0.1:{}", args.port);
    if !args.no_open {
        let _ = std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(&url).spawn();
    }
    println!("Claude Dashboard serving at {}  (127.0.0.1 only — private)", url);
    println!("Root: {}", here.display());
    println!("Press Ctrl+C to stop.");
    let serve = axum::serve(listener, server::router(app)).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        println!("\nShutting down.");
    });
    if let Err(e) = serve.await {
        eprintln!("server error: {}", e);
    }
}
