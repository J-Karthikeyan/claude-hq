//! Local state files: atomic writes (temp file in the same folder, 0600 from
//! birth, fsync, rename) and corruption-tolerant reads that quarantine a file
//! that does not parse -- the same rules as dashboard._atomic_write_text and
//! _load_json_guarded.

use crate::pyjson;
use serde_json::Value;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn temp_name(dir: &Path, prefix: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{}{}{}", prefix, crate::py::token_hex(4), suffix))
}

/// Create a fresh temp file next to the target (O_EXCL, like mkstemp).
fn mkstemp(dir: &Path, prefix: &str, suffix: &str) -> io::Result<(fs::File, PathBuf)> {
    for _ in 0..100 {
        let p = temp_name(dir, prefix, suffix);
        match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p) {
            Ok(f) => return Ok((f, p)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no temp name"))
}

/// os.path.realpath for a path that may not exist yet (resolves what exists).
pub fn realpath(p: &Path) -> PathBuf {
    if let Ok(c) = fs::canonicalize(p) {
        return c;
    }
    match (p.parent(), p.file_name()) {
        (Some(d), Some(n)) if !d.as_os_str().is_empty() => realpath(d).join(n),
        _ => p.to_path_buf(),
    }
}

/// Write `text` atomically with permissions `mode`. A symlinked path is
/// resolved first, so the real file is replaced and the link is kept.
pub fn atomic_write_text(path: &Path, text: &str, mode: u32) -> io::Result<()> {
    let path = realpath(path);
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let base = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (mut f, tmp) = mkstemp(&dir, &format!(".{}-", base), ".tmp")?;
    let res = (|| {
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, &path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// json.dumps(data, indent=2) written atomically.
pub fn atomic_write_json(path: &Path, data: &Value, mode: u32) -> io::Result<()> {
    atomic_write_text(path, &pyjson::dumps_with(data, pyjson::INDENT2), mode)
}

/// Write bytes atomically through a named temp file in `dir` with the given
/// prefix (meals.json / games-save.json use their own temp names).
pub fn atomic_write_prefixed(path: &Path, dir: &Path, prefix: &str, text: &str) -> io::Result<()> {
    let (mut f, tmp) = mkstemp(dir, prefix, ".tmp")?;
    let res = (|| {
        f.write_all(text.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
        drop(f);
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Parsed JSON from `path`, or None if missing/unreadable/corrupt. A file that
/// exists but does not parse is renamed to <name>.corrupt-<ms>, unless it was
/// swapped for another between the read and the check.
pub fn load_json_guarded(path: &Path) -> Option<Value> {
    let mut f = fs::File::open(path).ok()?;
    let mut raw = Vec::new();
    use std::io::Read;
    f.read_to_end(&mut raw).ok()?;
    let seen = f.metadata().ok()?;
    drop(f);
    let parsed = std::str::from_utf8(&raw).ok().and_then(|s| serde_json::from_str::<Value>(s).ok());
    if parsed.is_some() {
        return parsed;
    }
    let now = fs::metadata(path).ok()?;
    let key = |m: &fs::Metadata| (m.ino(), m.dev(), m.size(), m.mtime(), m.mtime_nsec());
    if key(&now) != key(&seen) {
        return None;
    }
    let ms = (crate::py::now_ts() * 1000.0) as i64;
    let dest = format!("{}.corrupt-{}", path.display(), ms);
    let _ = fs::rename(path, dest);
    None
}

/// json.load of a plain file (no quarantine), None on any failure.
pub fn load_json_plain(path: &Path) -> Option<Value> {
    let s = fs::read(path).ok()?;
    serde_json::from_slice(&s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mode(p: &Path) -> u32 {
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn writes_0600_and_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        atomic_write_json(&p, &json!({"a": 1}), 0o600).unwrap();
        assert_eq!(load_json_guarded(&p), Some(json!({"a": 1})));
        assert_eq!(mode(&p), 0o600);
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
        assert_eq!(fs::read_to_string(&p).unwrap(), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn replaces_a_loose_file_and_tightens_mode() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.json");
        fs::write(&p, "{\"old\": true}").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        atomic_write_json(&p, &json!({"new": true}), 0o600).unwrap();
        assert_eq!(mode(&p), 0o600);
        atomic_write_json(&p, &json!([]), 0o640).unwrap();
        assert_eq!(mode(&p), 0o640);
    }

    #[test]
    fn symlink_is_written_through() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real.json");
        let link = d.path().join("link.json");
        fs::write(&real, "{}").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write_json(&link, &json!({"via": "link"}), 0o600).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(load_json_guarded(&real), Some(json!({"via": "link"})));
    }

    #[test]
    fn missing_directory_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(atomic_write_json(&d.path().join("nope/x.json"), &json!({}), 0o600).is_err());
    }

    #[test]
    fn corrupt_file_is_quarantined() {
        let d = tempfile::tempdir().unwrap();
        for junk in [&b"{\"half\": "[..], &b"\xff\xfe not utf8"[..]] {
            let p = d.path().join("bad.json");
            fs::write(&p, junk).unwrap();
            assert_eq!(load_json_guarded(&p), None);
            assert!(!p.exists());
            let q: Vec<_> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().path()).collect();
            assert_eq!(q.len(), 1);
            assert!(q[0].to_string_lossy().contains("bad.json.corrupt-"));
            assert_eq!(fs::read(&q[0]).unwrap(), junk);
            fs::remove_file(&q[0]).unwrap();
        }
        assert_eq!(load_json_guarded(&d.path().join("absent.json")), None);
    }

    #[test]
    fn permission_error_never_quarantines() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("locked.json");
        fs::write(&p, "{not json").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o0)).unwrap();
        assert_eq!(load_json_guarded(&p), None);
        assert!(p.exists());
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
