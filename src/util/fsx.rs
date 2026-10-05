//! Crash-safe filesystem primitives.
//!
//! Rules enforced here (see docs/STORAGE_FORMAT.md):
//! * Files are written via temp-file + fsync + rename + dir-fsync, so a reader
//!   never observes a partial file and a crash never loses a completed rename.
//! * Mutual exclusion uses kernel-managed advisory locks on stable `.lock`
//!   files. The OS releases a lock when its handle closes, including on process
//!   death; the stable path is never unlinked while waiters may have it open.
//! * Path safety helpers reject traversal, absolute paths, and NUL bytes.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// An advisory exclusive lock backed by a stable, kernel-managed lock file.
///
/// The `.lock` path is deliberately persistent: unlinking a locked file could
/// let a later process lock a different inode while existing waiters still
/// reference the old one. The file contents are never read or written; kernel
/// ownership controls exclusion and is released automatically when the process
/// exits.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    file: File,
}

impl FileLock {
    /// Try to acquire `<path>.lock`. Returns Ok(lock) or Err(LockBusy).
    /// `wait` is the total time to keep retrying the kernel lock.
    pub fn acquire(path: &Path, wait: Duration) -> Result<FileLock> {
        let lock_path = lock_path_for(path);
        if let Some(parent) = lock_path.parent() {
            ensure_dir(parent)?;
        }
        ensure_regular_lock_file(&lock_path)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| Error::io(&lock_path, e))?;
        ensure_regular_lock_file(&lock_path)?;
        let deadline = Instant::now() + wait;
        loop {
            match fs4::FileExt::try_lock(&file) {
                Ok(()) => {
                    return Ok(FileLock {
                        path: lock_path,
                        file,
                    });
                }
                Err(fs4::TryLockError::WouldBlock) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(Error::LockBusy(format!(
                            "{} (kernel advisory lock is held)",
                            lock_path.display()
                        )));
                    }
                    std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
                }
                Err(fs4::TryLockError::Error(e)) => return Err(Error::io(&lock_path, e)),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

fn ensure_regular_lock_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(Error::Invalid(format!(
            "lock path is not a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::io(path, error)),
    }
}

pub fn lock_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
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
    #[cfg(windows)]
    if p.contains('\\') {
        return Err(Error::Invalid(format!(
            "backslash is not a portable repository path separator: {p:?}"
        )));
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
                #[cfg(windows)]
                check_windows_component(s, p)?;
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

#[cfg(windows)]
fn check_windows_component(component: &str, path: &str) -> Result<()> {
    if component
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Err(Error::Invalid(format!(
            "Windows-reserved character in repository path: {path:?}"
        )));
    }
    if component.ends_with('.') || component.ends_with(' ') {
        return Err(Error::Invalid(format!(
            "Windows path component ends in a dot or space: {path:?}"
        )));
    }
    let device = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
        device.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered_device
    {
        return Err(Error::Invalid(format!(
            "Windows device name is not a repository path: {path:?}"
        )));
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
        let l1 = FileLock::acquire(&p, Duration::ZERO).unwrap();
        let lock_path = l1.path().to_path_buf();
        assert!(std::fs::read_to_string(&lock_path).unwrap().is_empty());
        let r = FileLock::acquire(&p, Duration::from_millis(20));
        assert!(matches!(r, Err(Error::LockBusy(_))));
        drop(l1);
        assert!(lock_path.exists(), "stable lock inode path must persist");
        assert_eq!(std::fs::read(&lock_path).unwrap(), b"");
        let _l2 = FileLock::acquire(&p, Duration::from_millis(100)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn lock_rejects_symlink_sidecar_without_modifying_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let protected = dir.path().join("protected");
        std::fs::write(&target, b"preserve this data").unwrap();
        std::os::unix::fs::symlink(&target, lock_path_for(&protected)).unwrap();

        let result = FileLock::acquire(&protected, Duration::ZERO);
        assert!(matches!(result, Err(Error::Invalid(_))));
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve this data");
    }

    #[test]
    fn lock_rejects_nonregular_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("directory");
        std::fs::create_dir(lock_path_for(&resource)).unwrap();

        let result = FileLock::acquire(&resource, Duration::ZERO);
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    #[test]
    fn concurrent_lock_waiters_never_overlap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("shared"));
        let active = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(6));
        let workers: Vec<_> = (0..6)
            .map(|_| {
                let path = Arc::clone(&path);
                let active = Arc::clone(&active);
                let start = Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    for _ in 0..25 {
                        let _lock =
                            FileLock::acquire(path.as_ref(), Duration::from_secs(5)).unwrap();
                        assert_eq!(
                            active.fetch_add(1, Ordering::SeqCst),
                            0,
                            "two threads held one kernel lock simultaneously"
                        );
                        std::thread::yield_now();
                        assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
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

    #[cfg(windows)]
    #[test]
    fn windows_unrepresentable_path_names_are_rejected() {
        for path in [
            r"dir\child",
            "file:alternate-stream",
            "CON",
            "CONIN$",
            "CONOUT$.txt",
            "nul.txt",
            "COM1",
            "LPT9.log",
            "COM¹",
            "LPT².txt",
            "name.",
            "name ",
        ] {
            assert!(check_rel_path(path, 255).is_err(), "accepted {path:?}");
        }
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
        #[cfg(windows)]
        match std::os::windows::fs::symlink_dir(&outside, root.join("link")) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("Skipping symlink-escape filesystem assertion; Windows symlink creation unavailable: {error}");
                return;
            }
            Err(error) => panic!("Windows symlink creation failed: {error}"),
        }
        #[cfg(any(unix, windows))]
        {
            let r = safe_join(&root, "link/evil.txt", 255);
            assert!(r.is_err(), "symlink escape must be rejected");
        }
    }
}
