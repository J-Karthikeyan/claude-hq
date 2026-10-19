//! The HTTP gates on a real 127.0.0.1 server (ported from the route tests in
//! test_scanner.py, test_atomic_state.py, test_games.py, test_pantry_proxy.py,
//! test_maps_proxy.py, test_cups_proxy.py): Host check, CSRF, Origin, body
//! caps, UUID-only session routes, the games allowlist and the save endpoint;
//! plus test_evo_choice.py and test_notch_invite.py (evo picks, HQ Notch).

use claudehq::app::{App, Paths};
use claudehq::server;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::Arc;

struct Srv {
    port: u16,
    app: Arc<App>,
    _dir: tempfile::TempDir,
    _rt: tokio::runtime::Runtime,
}

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

fn serve() -> Srv {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Paths::new(dir.path(), &dir.path().join("home"));
    p.projects_dir = dir.path().join("projects");
    p.cursor_projects_dir = dir.path().join("cursor");
    // @include paths are relative to the root, like dashboard.py's HERE.
    std::os::unix::fs::symlink(repo().join("ui"), dir.path().join("ui")).unwrap();
    p.index_html = repo().join("index.html");
    p.games_dir = repo().join("games");
    std::fs::create_dir_all(&p.projects_dir).unwrap();
    let app = Arc::new(App::new(p));
    *app.agents_hook.write().unwrap() = Some(Arc::new(|| (Vec::new(), None)));
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = server::router(app.clone());
    rt.spawn(async move { axum::serve(listener, router).await.unwrap() });
    Srv { port, app, _dir: dir, _rt: rt }
}

/// (status, headers lowercased, body)
fn raw(port: u16, req: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut buf = Vec::new();
    // A refused POST is answered without reading its body, so the server's
    // close can arrive as a reset once the whole response is in: keep what came.
    match s.read_to_end(&mut buf) {
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset && buf.windows(4).any(|w| w == b"\r\n\r\n") => {}
        r => {
            r.unwrap();
        }
    }
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&buf[..split]).to_lowercase();
    let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut body = buf[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        body = dechunk(&body);
    }
    (status, head, body)
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(i) = b.windows(2).position(|w| w == b"\r\n") {
        let n = usize::from_str_radix(std::str::from_utf8(&b[..i]).unwrap().trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.extend_from_slice(&b[i + 2..i + 2 + n]);
        b = &b[i + 4 + n..];
    }
    out
}

fn get(port: u16, path: &str, extra: &str) -> (u16, String, Vec<u8>) {
    raw(port, &format!("GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n{}\r\n", path, extra), b"")
}

fn post(srv: &Srv, path: &str, body: &[u8], extra: &str, token: bool) -> (u16, Value) {
    let tok = if token { format!("X-HQ-Token: {}\r\n", srv.app.csrf_token) } else { String::new() };
    let cl = if extra.to_lowercase().contains("content-length") { String::new() } else { format!("Content-Length: {}\r\n", body.len()) };
    let (code, _, b) = raw(
        srv.port,
        &format!("POST {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\n{}{}{}\r\n", path, tok, cl, extra),
        body,
    );
    (code, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

#[test]
fn host_gate_and_index() {
    let srv = serve();
    let (code, _, body) = raw(srv.port, "GET /api/config HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n", b"");
    assert_eq!((code, body.as_slice()), (403, &b"Forbidden: local access only\n"[..]));
    let (code, head, body) = get(srv.port, "/", "");
    assert_eq!(code, 200);
    assert!(head.contains("content-type: text/html; charset=utf-8"));
    let html = String::from_utf8(body).unwrap();
    assert!(html.contains(&srv.app.csrf_token), "len {} head {}", html.len(), &html[..200.min(html.len())]);
    assert!(!html.contains("__HQ_CSRF__"));
    assert!(!html.contains("@include ui/"));
    assert!(html.contains("\"use strict\";\nvar $ = function(id)"));
    let fz = format!("var FZ_SCALE_MINS = {};", (claudehq::fatigue::CERTAIN_SECS / 60.0) as i64);
    assert!(html.lines().any(|l| l.trim() == fz));
    let (code, head, _) = raw(srv.port, "HEAD /api/export.csv HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n", b"");
    assert_eq!(code, 200);
    assert!(head.contains("content-disposition: attachment; filename=\"claude-hq-sessions.csv\""));
}

#[test]
fn includes_cannot_escape_ui() {
    let srv = serve();
    for bad in ["ui/../dashboard.py", "ui/app/../../README.md", "ui/nope.js"] {
        assert!(srv.app.include_path(bad).is_err(), "{}", bad);
    }
}

#[test]
fn session_routes_take_uuids_only() {
    let srv = serve();
    let sid = "22222222-2222-4222-8222-222222222222";
    for bad in ["*", "%2A", "abc", &format!("{}*", &sid[..35]), &format!("{}x", sid)] {
        for route in ["/api/transcript/{}", "/api/session/{}", "/api/session/{}/export.md"] {
            let (code, _, _) = get(srv.port, &route.replace("{}", bad), "");
            assert_eq!(code, 404, "{}", route.replace("{}", bad));
        }
    }
    let p = srv.app.paths.projects_dir.join("proj").join(format!("{}.jsonl", sid));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{\"type\": \"user\", \"timestamp\": \"2026-09-01T10:00:00Z\", \"message\": {\"content\": \"hello there\"}}\n").unwrap();
    let (code, _, body) = get(srv.port, &format!("/api/transcript/{}?q=hello&limit=999&offset=-3", sid), "");
    assert_eq!(code, 200);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!((v["total"].clone(), v["matched"].clone(), v["limit"].clone(), v["offset"].clone()), (json!(1), json!(1), json!(200), json!(0)));
    let (code, head, body) = get(srv.port, &format!("/api/session/{}/export.md", sid), "");
    assert_eq!(code, 200);
    assert!(head.contains("claude-hq-session-22222222.md"));
    assert!(String::from_utf8(body).unwrap().contains("**You:** hello there"));
}

#[test]
fn games_allowlist_etag_and_save() {
    let srv = serve();
    let (code, _, body) = get(srv.port, "/games/core.js", "");
    assert_eq!(code, 200);
    assert!(String::from_utf8_lossy(&body).contains("HQV"));
    for bad in ["/games/..%2fdashboard.py", "/games/Core.js", "/games/golf/LICENSE-kenney.txt", "/games/golf//straight.glb", "/games/vendor/x.css"] {
        assert_eq!(get(srv.port, bad, "").0, 404, "{}", bad);
    }
    for good in ["golf/courses.json", "golf/straight.glb", "vendor/three-core.js", "kart/tracks.json", "fps/map.json", "platformer/levels.json", "strike/map.json"] {
        assert!(srv.app.game_file(good).is_some(), "{}", good);
    }
    let (code, head, _) = get(srv.port, "/games/golf/courses.json", "");
    assert_eq!(code, 200);
    assert!(head.contains("cache-control: no-cache"));
    let tag = head.lines().find(|l| l.starts_with("etag:")).unwrap()[6..].trim().to_string();
    assert_eq!(get(srv.port, "/games/golf/courses.json", &format!("If-None-Match: {}\r\n", tag)).0, 304);
    assert_eq!(server::game_cache_control("golf.js"), "no-store");
    assert_eq!(server::game_cache_control("vendor/../golf.js"), "no-store");

    let (code, _, body) = get(srv.port, "/api/games/state", "");
    assert_eq!((code, serde_json::from_slice::<Value>(&body).unwrap()), (200, json!({"state": {}})));
    let st = json!({"state": {"v": 1, "inv": {"koi": 2}}});
    assert_eq!(post(&srv, "/api/games/state", st.to_string().as_bytes(), "", true).0, 200);
    assert_eq!(serde_json::from_slice::<Value>(&get(srv.port, "/api/games/state", "").2).unwrap(), json!({"state": {"v": 1, "inv": {"koi": 2}}}));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&srv.app.paths.games_save).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(post(&srv, "/api/games/state", b"{\"state\": [1]}", "", true).0, 400);
    assert_eq!(post(&srv, "/api/games/state", b"{\"state\": {}}", "", false).0, 403);
}

#[test]
fn post_gates_and_limits() {
    let srv = serve();
    assert_eq!(post(&srv, "/api/config", b"{}", "", false), (403, json!({"error": "bad or missing CSRF token"})));
    assert_eq!(post(&srv, "/api/config", b"{}", "Origin: https://evil.example\r\n", true).1["error"], "cross-site request rejected");
    assert_eq!(post(&srv, "/api/config", b"{}", "Origin: http://localhost:8765\r\n", true).0, 200);
    assert_eq!(post(&srv, "/api/config", b"{}", "Sec-Fetch-Site: cross-site\r\n", true).0, 403);
    assert_eq!(post(&srv, "/api/nope", b"{}", "", true), (404, json!({"error": "not found"})));
    assert_eq!(post(&srv, "/api/config", b"", "Content-Length: 1048577\r\n", true).0, 413);
    assert_eq!(post(&srv, "/api/arena/sounds", b"", "Content-Length: 8388609\r\n", true).0, 413);
    assert_eq!(post(&srv, "/api/config", b"", "Content-Length: 0\r\n", true).0, 200);
    let (code, _, _) = raw(srv.port, &format!("POST /api/config HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nX-HQ-Token: {}\r\nTransfer-Encoding: chunked\r\n\r\n", srv.app.csrf_token), b"0\r\n\r\n");
    assert_eq!(code, 411);
    let (code, resp) = post(&srv, "/api/config", json!({"dailyBudgetUSD": 30}).to_string().as_bytes(), "", true);
    assert_eq!(code, 200);
    assert_eq!(resp, Value::Object(srv.app.load_config()));
    assert_eq!(post(&srv, "/api/config", b"[1]", "", true).1["error"], "bad JSON body: body must be an object");
    assert_eq!(post(&srv, "/api/meta", json!({"sessionId": "nope"}).to_string().as_bytes(), "", true).0, 400);
}

#[test]
fn arena_routes_stay_local_when_unpaired() {
    let srv = serve();
    for p in ["/api/arena/pantry", "/api/arena/cups", "/api/arena/maps/featured", "/api/arena/boss", "/api/arena/skills"] {
        let (code, _, body) = get(srv.port, p, "");
        assert_eq!((code, serde_json::from_slice::<Value>(&body).unwrap()), (400, json!({"error": "not paired"})), "{}", p);
    }
    let (code, _, body) = get(srv.port, "/api/arena/cups/one?id=cup-2026-W41-td", "");
    assert_eq!(code, 400);
    assert!(String::from_utf8_lossy(&body).contains("bad cup id"));
    for p in ["/api/arena/maps/like", "/api/arena/boss/fight", "/api/arena/pantry/claim"] {
        // No body: a refused POST is answered without reading one.
        assert_eq!(post(&srv, p, b"", "", false).0, 403, "{}", p);
    }
    assert_eq!(post(&srv, "/api/arena/pantry/steal", b"{}", "", true), (404, json!({"error": "not found"})));
    let (code, body) = post(&srv, "/api/arena/pantry/buy", json!({"requestId": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d", "kind": "berry", "qty": 9}).to_string().as_bytes(), "", true);
    assert_eq!((code, body), (400, json!({"error": "qty must be a whole number from 1 to 5"})));
}

#[test]
fn stream_sends_a_data_frame() {
    let srv = serve();
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.write_all(b"GET /api/stream HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 65536];
    while !String::from_utf8_lossy(&buf).contains("\n\n") || !String::from_utf8_lossy(&buf).contains("data: ") {
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0);
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    assert!(text.to_lowercase().contains("content-type: text/event-stream; charset=utf-8"));
    assert!(text.contains(&format!("\"version\": \"{}\"", claudehq::app::APP_VERSION)));
}

/// A session id whose Pokemon is this species (test_evo_choice.py's sid_for).
fn sid_for(species: u64) -> String {
    let mut i: u128 = 0;
    loop {
        let h = format!("{:032x}", i.wrapping_mul(2654435761));
        let sid = format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
        if claudehq::creature::session_hash_mod(&sid, 48) == species {
            return sid;
        }
        i += 1;
    }
}

#[test]
fn evo_choice_saves_and_rides_in_sessions() {
    let srv = serve();
    let (eevee, zard) = (sid_for(4), sid_for(1));
    let p = srv.app.paths.projects_dir.join("proj").join(format!("{}.jsonl", eevee));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{\"type\": \"user\", \"timestamp\": \"2026-09-01T10:00:00Z\", \"message\": {\"content\": \"hello there\"}}\n").unwrap();
    let card = |srv: &Srv| -> Value {
        let v: Value = serde_json::from_slice(&get(srv.port, "/api/sessions", "").2).unwrap();
        v["sessions"].as_array().unwrap().iter().find(|s| s["sessionId"] == eevee.as_str()).cloned().expect("eevee card")
    };
    assert!(card(&srv)["creature"].get("evo").is_none()); // also primes the 1.5 s memo

    let (code, resp) = post(&srv, "/api/evo-choice", json!({"sessionId": eevee, "branchDex": 470}).to_string().as_bytes(), "", true);
    assert_eq!((code, resp), (200, json!({"sessionId": eevee, "evo": {"branchDex": 470}})));
    assert_eq!(Value::Object(srv.app.load_evo_choices()), json!({eevee.clone(): {"branchDex": 470}}));
    assert_eq!(card(&srv)["creature"]["evo"], json!({"branchDex": 470})); // the save dropped the memo

    let (code, resp) = post(&srv, "/api/evo-choice", json!({"sessionId": eevee, "branchDex": null}).to_string().as_bytes(), "", true);
    assert_eq!((code, resp), (200, json!({"sessionId": eevee, "evo": {}})));
    assert!(card(&srv)["creature"].get("evo").is_none());

    let (code, resp) = post(&srv, "/api/evo-choice", json!({"sessionId": zard, "megaSlug": "charizard-megax"}).to_string().as_bytes(), "", true);
    assert_eq!((code, resp), (200, json!({"sessionId": zard, "evo": {"megaSlug": "charizard-megax"}})));
}

#[test]
fn evo_choice_rejects_bad_ids_picks_and_requests() {
    let srv = serve();
    let (eevee, zard) = (sid_for(4), sid_for(1));
    let cases = [
        (json!({"sessionId": "../../etc", "branchDex": 470}), "invalid sessionId"),
        (json!({"branchDex": 470}), "invalid sessionId"),
        (json!({"sessionId": eevee, "branchDex": 6}), "branchDex is not a valid pick for this session's Pokemon"),
        (json!({"sessionId": zard, "megaSlug": "<script>"}), "megaSlug is not a valid pick for this session's Pokemon"),
    ];
    for (body, err) in cases {
        assert_eq!(post(&srv, "/api/evo-choice", body.to_string().as_bytes(), "", true), (400, json!({"error": err})), "{}", body);
    }
    let ok = json!({"sessionId": eevee, "branchDex": 470}).to_string();
    assert_eq!(post(&srv, "/api/evo-choice", ok.as_bytes(), "", false).0, 403);
    assert_eq!(post(&srv, "/api/evo-choice", ok.as_bytes(), "Origin: https://evil.example\r\n", true).0, 403);
    let (code, _, _) = raw(
        srv.port,
        &format!("POST /api/evo-choice HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\nX-HQ-Token: {}\r\nContent-Length: {}\r\n\r\n", srv.app.csrf_token, ok.len()),
        ok.as_bytes(),
    );
    assert_eq!(code, 403);
    assert!(!srv.app.paths.evo.exists());
    assert!(server::post_paths().contains(&"/api/evo-choice"));
}

#[test]
fn notch_status_and_protected_install() {
    let srv = serve();
    let (code, _, body) = get(srv.port, "/api/notch/status", "");
    assert_eq!(code, 200);
    let s: Value = serde_json::from_slice(&body).unwrap();
    let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["mac", "installed", "running", "tools", "available", "job"]);
    assert_eq!(s["available"], false); // the temp HQ has no notch/ yet

    // A stand-in installer under the temp HQ root, on a pretend Mac with swift.
    let inst = srv.app.paths.here.join("notch/tools/install.py");
    std::fs::create_dir_all(inst.parent().unwrap()).unwrap();
    std::fs::write(&inst, "print('done. HQ Notch is running')\n").unwrap();
    *srv.app.notch.mac_hook.write().unwrap() = Some(true);
    *srv.app.notch.swift_hook.write().unwrap() = Some(true);
    assert_eq!(serde_json::from_slice::<Value>(&get(srv.port, "/api/notch/status", "").2).unwrap()["available"], true);

    assert_eq!(post(&srv, "/api/notch/install", b"{}", "", false).0, 403);
    assert_eq!(post(&srv, "/api/notch/install", b"{}", "Origin: https://evil.example\r\n", true).0, 403);
    assert_eq!(srv.app.notch.job.lock().unwrap().state, "idle"); // refused before it ran
    assert_eq!(post(&srv, "/api/notch/install", b"{}", "", true), (202, json!({"started": true})));
    let mut job = Value::Null;
    for _ in 0..200 {
        job = serde_json::from_slice::<Value>(&get(srv.port, "/api/notch/status", "").2).unwrap()["job"].clone();
        if job["state"] != "running" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!((job["state"].clone(), job["rc"].clone()), (json!("done"), json!(0)));
    assert!(job["log"].as_str().unwrap().contains("HQ Notch is running"));

    *srv.app.notch.mac_hook.write().unwrap() = Some(false);
    assert_eq!(post(&srv, "/api/notch/install", b"{}", "", true), (400, json!({"error": "HQ Notch is for macOS"})));
}

#[test]
fn vial_spend_route_and_payload() {
    let srv = serve();
    let sid = "11111111-1111-4111-8111-111111111111";
    assert!(server::post_paths().contains(&"/api/vial/spend"));
    let spend = |body: Value| post(&srv, "/api/vial/spend", body.to_string().as_bytes(), "", true);
    assert_eq!(spend(json!({"sessionId": "../x", "fromStage": 0})), (400, json!({"error": "invalid sessionId"})));
    for stg in [json!(4), json!(-1), json!(true), json!(1.0), json!("1"), json!(null)] {
        assert_eq!(spend(json!({"sessionId": sid, "fromStage": stg})), (400, json!({"error": "invalid fromStage"})), "{}", stg);
    }
    assert_eq!(spend(json!({"sessionId": sid, "fromStage": 0})), (200, json!({"ok": false, "error": "the vial is short", "cost": 30, "balance": 0})));
    srv.app.vial_grant(100, claudehq::py::now_ts());
    assert_eq!(spend(json!({"sessionId": sid, "fromStage": 1})), (200, json!({"ok": true, "cost": 90, "balance": 10})));
    assert_eq!(post(&srv, "/api/vial/spend", json!({"sessionId": sid, "fromStage": 0}).to_string().as_bytes(), "", false).0, 403);

    // A prompt-less transcript is still an archived card (2.5.2), and the payload carries the vial.
    let quiet = "33333333-3333-4333-8333-333333333333";
    let p = srv.app.paths.projects_dir.join("proj").join(format!("{}.jsonl", quiet));
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "{\"type\": \"assistant\", \"timestamp\": \"2026-09-01T10:00:00Z\", \"message\": {\"content\": [{\"type\": \"text\", \"text\": \"hi\"}]}}\n").unwrap();
    let v: Value = serde_json::from_slice(&get(srv.port, "/api/sessions", "").2).unwrap();
    let card = v["sessions"].as_array().unwrap().iter().find(|s| s["sessionId"] == quiet).cloned().expect("prompt-less card");
    assert_eq!((card["promptCount"].clone(), card["vialPoured"].clone()), (json!(0), json!(0)));
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(keys.last().map(|k| k.as_str()), Some("vial"));
    assert_eq!(v["vial"]["xp"], 10);
    assert_eq!(v["vial"]["costs"], json!({"0": 30, "1": 90, "2": 180, "3": 400}));
}
