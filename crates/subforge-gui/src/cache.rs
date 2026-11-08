//! Small persistent disk cache for per-video derived data (waveform peaks, keyframes, fonts).
//! Keyed by path + size + mtime, so re-encoded or replaced files are recomputed automatically.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).map(|p| p.join("SubForge").join("cache"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(PathBuf::from).map(|p| p.join("Library/Caches/SubForge"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|p| p.join("subforge"))
    };
    base.unwrap_or_else(|| std::env::temp_dir().join("subforge-cache"))
}

fn fnv1a(data: &[u8], mut h: u64) -> u64 {
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Stable key for a media file, or None if it can't be stat'ed.
pub fn key(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mtime = meta.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    h = fnv1a(abs.to_string_lossy().as_bytes(), h);
    h = fnv1a(&meta.len().to_le_bytes(), h);
    h = fnv1a(&mtime.to_le_bytes(), h);
    Some(format!("{h:016x}"))
}

fn file(key: &str, ext: &str) -> PathBuf {
    dir().join(format!("{key}.{ext}"))
}

pub fn load(key: &str, ext: &str, magic: &[u8; 4]) -> Option<Vec<u8>> {
    let data = std::fs::read(file(key, ext)).ok()?;
    if data.len() < 4 || &data[..4] != magic {
        return None;
    }
    // touch so pruning keeps recently used entries
    let _ = std::fs::File::options().append(true).open(file(key, ext)).and_then(|f| f.set_modified(SystemTime::now()));
    Some(data[4..].to_vec())
}

pub fn save(key: &str, ext: &str, magic: &[u8; 4], payload: &[u8]) {
    let d = dir();
    if std::fs::create_dir_all(&d).is_err() {
        return;
    }
    let tmp = d.join(format!("{key}.{ext}.tmp"));
    let mut buf = Vec::with_capacity(payload.len() + 4);
    buf.extend_from_slice(magic);
    buf.extend_from_slice(payload);
    if std::fs::write(&tmp, buf).is_ok() {
        let _ = std::fs::rename(&tmp, file(key, ext));
    }
}

/// Per-video working directory (used for extracted fonts and the libass preview script).
pub fn work_dir(key: &str) -> PathBuf {
    dir().join("work").join(key)
}

/// Remove cache entries not used for `max_age` (runs in the background at startup).
pub fn prune(max_age: Duration) {
    std::thread::spawn(move || {
        let now = SystemTime::now();
        let old = |p: &Path| {
            std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).map(|a| a > max_age).unwrap_or(false)
        };
        if let Ok(rd) = std::fs::read_dir(dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() && old(&p) {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(dir().join("work")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && old(&p) {
                    let _ = std::fs::remove_dir_all(p);
                }
            }
        }
    });
}
