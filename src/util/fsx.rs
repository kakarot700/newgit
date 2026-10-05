//! Crash-safe filesystem primitives.
//!
//! Rules enforced here (see docs/STORAGE_FORMAT.md):
//! * Files are written via temp-file + fsync + rename + dir-fsync, so a reader
//!   never observes a partial file and a crash never loses a completed rename.
//! * Mutual exclusion uses `O_CREAT|O_EXCL` lock files containing pid+time,
//!   with an explicit stale-lock timeout (locks are never silently stolen
//!   before the timeout).
//! * Path safety helpers reject traversal, absolute paths, and NUL bytes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

pub fn ensure_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(|e| Error::io(path, e))
}

pub fn fsync_file(f: &File) -> Result<()> {
    f.sync_all().map_err(Error::from)
}

pub fn fsync_dir(path: &Path) -> Result<()> {
    match File::open(path) {
        Ok(dir) => {
            let _ = dir.sync_all(); // best effort: not all platforms support it
            Ok(())
        }
        Err(_) => Ok(()), // directory fsync unsupported → skip (documented)
    }
}

/// Atomically write `data` to `path` (create parent dirs as needed).
/// Durability order: write tmp → fsync tmp → rename → fsync dir.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let tmp = temp_sibling(path)?;
    let write_result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|e| Error::io(&tmp, e))?;
        f.write_all(data).map_err(|e| Error::io(&tmp, e))?;
        fsync_file(&f)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return write_result;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(path, e)
    })?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Create a unique temporary path next to `path`.
pub fn temp_sibling(path: &Path) -> Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    // Per-process counter keeps sibling temps unique within a process.
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let name = format!(
        "{}.tmp.{}.{}.{}",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        pid,
        nanos,
        n
    );
    Ok(path.with_file_name(name))
}

/// An advisory exclusive lock backed by an O_EXCL file.
///
/// The lock file records `pid`, creation time, and an optional label so that
/// operators (and `newgit verify`) can diagnose stuck locks. Stale locks older
/// than `stale_after` may be reclaimed; this is logged by callers.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// Try to acquire `<path>.lock`. Returns Ok(lock) or Err(LockBusy).
    /// `wait`: total time to keep retrying; `stale_after`: reclaim threshold.
    pub fn acquire(path: &Path, wait: Duration, stale_after: Duration) -> Result<FileLock> {
        let lock_path = lock_path_for(path);
        let deadline = SystemTime::now() + wait;
        loop {
            match try_create_lock(&lock_path) {
                Ok(()) => {
                    if let Some(parent) = lock_path.parent() {
                        fsync_dir(parent)?;
                    }
                    return Ok(FileLock { path: lock_path });
                }
                Err(e) => {
                    // Inspect the existing lock: reclaim it when the holder
                    // is provably dead (recorded pid not alive) or when it
                    // is older than the stale timeout. Aborted/killed
                    // processes never run Drop, so this reclamation is the
                    // crash-recovery path for locks.
                    let age = lock_age(&lock_path);
                    let holder = read_lock_info(&lock_path);
                    let timed_out = age.map(|a| a > stale_after).unwrap_or(false);
                    let reclaimable = match holder {
                        Some((pid, _)) => {
                            #[cfg(target_os = "linux")]
                            {
                                // On Linux, a live PID is authoritative: an
                                // old lock may still protect a long transaction
                                // or projection and must not be stolen by age.
                                pid != std::process::id() && !pid_alive(pid)
                            }
                            #[cfg(not(target_os = "linux"))]
                            {
                                let _ = pid;
                                timed_out
                            }
                        }
                        None => timed_out,
                    };
                    if reclaimable {
                        // Reclaim: remove and retry once.
                        let _ = std::fs::remove_file(&lock_path);
                        if try_create_lock(&lock_path).is_ok() {
                            return Ok(FileLock { path: lock_path });
                        }
                    }
                    if SystemTime::now() >= deadline {
                        return Err(Error::LockBusy(format!(
                            "{} (held for {:?}, holder pid {:?})",
                            lock_path.display(),
                            age.unwrap_or(Duration::ZERO),
                            holder.map(|(pid, _)| pid)
                        )));
                    }
                    drop(e);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(parent) = self.path.parent() {
            let _ = fsync_dir(parent);
        }
    }
}

pub fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

fn try_create_lock(lock_path: &Path) -> std::io::Result<()> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(lock_path)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    writeln!(f, "pid={} time={}", std::process::id(), now)?;
    f.sync_all()?;
    Ok(())
}

fn lock_age(lock_path: &Path) -> Option<Duration> {
    let m = std::fs::metadata(lock_path).ok()?;
    let t = m.modified().ok()?;
    SystemTime::now().duration_since(t).ok()
}

/// Parse `pid=<n> time=<secs>` from a lock file.
fn read_lock_info(lock_path: &Path) -> Option<(u32, u64)> {
    let text = std::fs::read_to_string(lock_path).ok()?;
    let mut pid = None;
    let mut time = None;
    for part in text.split_whitespace() {
        if let Some(v) = part.strip_prefix("pid=") {
            pid = v.parse::<u32>().ok();
        } else if let Some(v) = part.strip_prefix("time=") {
            time = v.parse::<u64>().ok();
        }
    }
    Some((pid?, time.unwrap_or(0)))
}

/// Is a process with this pid currently alive?
/// Linux: /proc probe. Elsewhere: conservatively assume alive (reclamation
/// then relies on the stale timeout — documented in STORAGE_FORMAT.md).
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Read a file fully, rejecting files larger than `max` before reading.
pub fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>> {
    let m = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    if m.len() > max {
        return Err(Error::Limit(format!(
            "file {} is {} bytes, exceeds limit {max}",
            path.display(),
            m.len()
        )));
    }
    std::fs::read(path).map_err(|e| Error::io(path, e))
}

/// Validate a repository-relative path component-wise.
///
/// Rejects: absolute paths, `..` traversal, empty names, NUL bytes,
/// Windows-style drive prefixes, and components longer than `max_component`.
pub fn check_rel_path(p: &str, max_component: usize) -> Result<()> {
    if p.is_empty() {
        return Err(Error::Invalid("empty path".into()));
    }
    if p.as_bytes().contains(&0) {
        return Err(Error::Invalid(format!("path contains NUL byte: {p:?}")));
    }
    let path = Path::new(p);
    if path.is_absolute() {
        return Err(Error::Invalid(format!("absolute path not allowed: {p:?}")));
    }
    let mut depth = 0usize;
    for c in path.components() {
        match c {
            Component::Normal(name) => {
                let s = name.to_str().ok_or_else(|| {
                    Error::Invalid(format!("path component is not valid utf-8: {p:?}"))
                })?;
                if s.is_empty() || s == "." || s == ".." {
                    return Err(Error::Invalid(format!("illegal path component in {p:?}")));
                }
                // Reject drive-letter-style components (cross-platform safety
                // for git export / Windows checkouts): "c:", "C:x", ...
                let b = s.as_bytes();
                if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
                    return Err(Error::Invalid(format!(
                        "drive-letter path component: {p:?}"
                    )));
                }
                if s.len() > max_component {
                    return Err(Error::Limit(format!(
                        "path component longer than {max_component} bytes in {p:?}"
                    )));
                }
                depth += 1;
            }
            Component::ParentDir => {
                return Err(Error::Invalid(format!("`..` traversal not allowed: {p:?}")))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::Invalid(format!("absolute path not allowed: {p:?}")))
            }
            Component::CurDir => {
                return Err(Error::Invalid(format!("`.` component not allowed: {p:?}")))
            }
        }
    }
    if depth == 0 {
        return Err(Error::Invalid(format!("empty path: {p:?}")));
    }
    Ok(())
}

/// Resolve `rel` under `root`, then verify the result stays inside `root`
/// (defense in depth against symlink escapes: we canonicalize the *parent*
/// dir, which must exist, and re-check containment).
pub fn safe_join(root: &Path, rel: &str, max_component: usize) -> Result<PathBuf> {
    check_rel_path(rel, max_component)?;
    let joined = root.join(rel);
    // Containment check on the lexical level; parent canonicalization adds a
    // symlink-escape check for the directory portion.
    if let Some(parent) = joined.parent() {
        if parent.exists() {
            let canon_parent = parent.canonicalize().map_err(|e| Error::io(parent, e))?;
            let canon_root = root.canonicalize().map_err(|e| Error::io(root, e))?;
            if !canon_parent.starts_with(&canon_root) {
                return Err(Error::Invalid(format!(
                    "path {rel:?} escapes repository root (symlink?)"
                )));
            }
        }
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_is_durable_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b.txt");
        atomic_write(&p, b"one").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"one");
        atomic_write(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        // no temp files left behind
        let left: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(left.is_empty(), "temp files left: {left:?}");
    }

    #[test]
    fn lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("res");
        let _l1 = FileLock::acquire(&p, Duration::ZERO, Duration::from_secs(60)).unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(20), Duration::from_secs(60));
        assert!(matches!(r, Err(Error::LockBusy(_))));
        drop(_l1);
        let _l2 =
            FileLock::acquire(&p, Duration::from_millis(100), Duration::from_secs(60)).unwrap();
    }

    #[test]
    fn dead_holder_lock_is_reclaimed() {
        // Simulates a lock left behind by an aborted/killed process:
        // the recorded pid is beyond any possible Linux pid (pid_max ≤ 2^22),
        // so the holder is provably dead and the lock must be reclaimed
        // *without* waiting for the stale timeout.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("res");
        let lp = lock_path_for(&p);
        std::fs::write(&lp, "pid=99999999 time=1").unwrap();
        let start = std::time::Instant::now();
        let _l =
            FileLock::acquire(&p, Duration::from_millis(500), Duration::from_secs(3600)).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "dead-holder lock must be reclaimed immediately"
        );
    }

    #[test]
    fn live_holder_lock_is_not_stolen() {
        // Our own pid is provably alive: the lock must NOT be reclaimed by
        // the pid rule; acquisition times out with LockBusy.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("res");
        let lp = lock_path_for(&p);
        std::fs::write(&lp, format!("pid={} time=1", std::process::id())).unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(50), Duration::from_secs(3600));
        assert!(matches!(r, Err(Error::LockBusy(_))));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stale_age_does_not_steal_lock_from_live_holder() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("res");
        let lp = lock_path_for(&p);
        std::fs::write(&lp, format!("pid={} time=1", std::process::id())).unwrap();
        let file = File::open(&lp).unwrap();
        file.set_modified(filetime_past()).unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(50), Duration::from_millis(1));
        assert!(matches!(r, Err(Error::LockBusy(_))));
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("res");
        let lp = lock_path_for(&p);
        std::fs::write(&lp, b"pid=999999 time=0").unwrap();
        // backdate
        let old = filetime_past();
        let f = File::open(&lp).unwrap();
        f.set_modified(old).ok();
        drop(f);
        let _l =
            FileLock::acquire(&p, Duration::from_millis(100), Duration::from_millis(1)).unwrap();
    }

    fn filetime_past() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1)
    }

    #[test]
    fn path_checks() {
        assert!(check_rel_path("a/b/c.txt", 255).is_ok());
        assert!(check_rel_path("../x", 255).is_err());
        assert!(check_rel_path("/x", 255).is_err());
        assert!(check_rel_path("a/../../b", 255).is_err());
        assert!(check_rel_path("", 255).is_err());
        assert!(check_rel_path("a\0b", 255).is_err());
        assert!(check_rel_path("./a", 255).is_err());
        assert!(check_rel_path("c:x", 255).is_err());
        assert!(check_rel_path(&"x".repeat(300), 255).is_err());
        // safe_join containment
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        assert!(safe_join(&root, "ok.txt", 255).is_ok());
        assert!(safe_join(&root, "../evil.txt", 255).is_err());
    }

    #[test]
    fn symlink_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        #[cfg(unix)]
        {
            let r = safe_join(&root, "link/evil.txt", 255);
            assert!(r.is_err(), "symlink escape must be rejected");
        }
    }
}
