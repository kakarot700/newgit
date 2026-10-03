//! Workspace filesystem walk: enumerate trackable entries with limits,
//! ignore rules, and a strict symlink policy (symlinks are *recorded*,
//! never followed; THREAT_MODEL §B).

use std::fs::FileType;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::object::types::EntryMode;
use crate::repo::config::Limits;
use crate::repo::ignore::IgnoreSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FsEntry {
    /// Workspace-relative path, '/'-separated, validated.
    pub rel: String,
    pub mode: EntryMode, // File | Executable | Symlink (never Tree)
    pub size: u64,
    pub mtime_sec: u64,
    pub mtime_nsec: u32,
    /// For symlinks: the target string (blob content).
    pub symlink_target: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WalkReport {
    pub entries: Vec<FsEntry>,
    pub ignored: usize,
    /// Files skipped as unreadable etc. (walk still succeeds; surfaced to
    /// callers so nothing is silently dropped).
    pub warnings: Vec<String>,
}

/// Name that is always excluded, at any depth.
const ALWAYS_SKIP: [&str; 2] = [".newgit", ".git"];

pub fn walk(root: &Path, ignore: &IgnoreSet, limits: &Limits) -> Result<WalkReport> {
    let mut report = WalkReport::default();
    let mut stack: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
    let mut count: usize = 0;
    const MAX_ENTRIES: usize = 5_000_000;

    while let Some((dir, rel_prefix, depth)) = stack.pop() {
        if depth > limits.max_depth {
            return Err(Error::Limit(format!(
                "directory depth exceeds max_depth {} at {}",
                limits.max_depth,
                dir.display()
            )));
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                report
                    .warnings
                    .push(format!("cannot read {}: {e}", dir.display()));
                continue;
            }
        };
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    report
                        .warnings
                        .push(format!("cannot read entry in {}: {e}", dir.display()));
                    continue;
                }
            };
            let name_os = entry.file_name();
            let name = match name_os.to_str() {
                Some(n) => n,
                None => {
                    report.warnings.push(format!(
                        "skipping non-utf-8 name under {} (NewGit paths are utf-8)",
                        dir.display()
                    ));
                    continue;
                }
            };
            if ALWAYS_SKIP.contains(&name) && depth == 0 {
                continue; // repo metadata lives at the root
            }
            if ALWAYS_SKIP.contains(&name) {
                report.warnings.push(format!(
                    "skipping nested {} directory at {}/{}",
                    name, rel_prefix, name
                ));
                continue;
            }
            if name.len() > limits.max_path_component {
                return Err(Error::Limit(format!(
                    "path component longer than {} bytes: {}/{}",
                    limits.max_path_component, rel_prefix, name
                )));
            }
            // component grammar (control chars, dots) — reject loudly
            if name == "." || name == ".." || name.as_bytes().contains(&0) {
                return Err(Error::Invalid(format!("illegal filename {name:?}")));
            }
            if name.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f) {
                return Err(Error::Invalid(format!(
                    "filename contains control characters: {name:?}"
                )));
            }
            let rel = if rel_prefix.is_empty() {
                name.to_string()
            } else {
                format!("{rel_prefix}/{name}")
            };

            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    report
                        .warnings
                        .push(format!("cannot stat {}: {e}", entry.path().display()));
                    continue;
                }
            };
            if ft.is_dir() {
                if ignore.can_prune_dir(&rel) {
                    report.ignored += 1;
                    continue;
                }
                if ignore.is_ignored(&rel, true) && !ignore.is_empty() {
                    // has negations: descend and evaluate files individually
                    report.ignored += 1;
                }
                stack.push((entry.path(), rel, depth + 1));
                continue;
            }
            if ignore.is_ignored(&rel, false) {
                report.ignored += 1;
                continue;
            }
            if ft.is_symlink() {
                let target =
                    std::fs::read_link(entry.path()).map_err(|e| Error::io(entry.path(), e))?;
                let target = target.into_os_string().into_string().map_err(|_| {
                    Error::Invalid(format!(
                        "symlink target is not utf-8: {} (NewGit paths are utf-8)",
                        rel
                    ))
                })?;
                if target.len() as u64 > limits.max_blob_bytes {
                    return Err(Error::Limit(format!("symlink target too long: {rel}")));
                }
                let md = std::fs::symlink_metadata(entry.path())
                    .map_err(|e| Error::io(entry.path(), e))?;
                let (sec, nsec) = mtime_of(&md);
                report.entries.push(FsEntry {
                    rel,
                    mode: EntryMode::Symlink,
                    size: target.len() as u64,
                    mtime_sec: sec,
                    mtime_nsec: nsec,
                    symlink_target: Some(target),
                });
                count += 1;
                continue;
            }
            if !ft.is_file() {
                report
                    .warnings
                    .push(format!("skipping special file (socket/fifo/device): {rel}"));
                continue;
            }
            // regular file
            let md = match entry.metadata() {
                Ok(md) => md,
                Err(e) => {
                    report.warnings.push(format!("cannot stat {rel}: {e}"));
                    continue;
                }
            };
            if md.len() > limits.max_blob_bytes {
                return Err(Error::Limit(format!(
                    "file {rel} is {} bytes, exceeds max_blob_bytes {}",
                    md.len(),
                    limits.max_blob_bytes
                )));
            }
            let mode = if is_executable(&md) {
                EntryMode::Executable
            } else {
                EntryMode::File
            };
            let (sec, nsec) = mtime_of(&md);
            report.entries.push(FsEntry {
                rel,
                mode,
                size: md.len(),
                mtime_sec: sec,
                mtime_nsec: nsec,
                symlink_target: None,
            });
            count += 1;
            if count > MAX_ENTRIES {
                return Err(Error::Limit(format!(
                    "workspace has more than {MAX_ENTRIES} trackable entries"
                )));
            }
        }
    }
    report.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    // stable order requires uniqueness: duplicate rels would be a bug
    debug_assert!(report.entries.windows(2).all(|w| w[0].rel != w[1].rel));
    Ok(report)
}

#[cfg(unix)]
fn is_executable(md: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_md: &std::fs::Metadata) -> bool {
    false
}

fn mtime_of(md: &std::fs::Metadata) -> (u64, u32) {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0))
}

pub fn file_type_to_mode(ft: &FileType, md: &std::fs::Metadata) -> EntryMode {
    if ft.is_symlink() {
        EntryMode::Symlink
    } else if ft.is_dir() {
        EntryMode::Tree
    } else if is_executable(md) {
        EntryMode::Executable
    } else {
        EntryMode::File
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(dir: &Path, rel: &str, content: &[u8]) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
    }

    #[test]
    fn walk_basics() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        w(root, "a.txt", b"a");
        w(root, "sub/b.txt", b"b");
        w(root, "sub/deep/c.txt", b"c");
        std::fs::create_dir(root.join(".newgit")).unwrap();
        std::fs::write(root.join(".newgit/config"), b"skip me").unwrap();
        let rep = walk(root, &IgnoreSet::empty(), &Limits::default()).unwrap();
        let rels: Vec<&str> = rep.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["a.txt", "sub/b.txt", "sub/deep/c.txt"]);
    }

    #[test]
    fn walk_exec_and_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        w(root, "script.sh", b"#!/bin/sh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(root.join("script.sh"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(root.join("script.sh"), perms).unwrap();
            std::os::unix::fs::symlink("script.sh", root.join("link")).unwrap();
            std::os::unix::fs::symlink("../escape", root.join("link2")).unwrap();
        }
        let rep = walk(root, &IgnoreSet::empty(), &Limits::default()).unwrap();
        #[cfg(unix)]
        {
            let by: std::collections::HashMap<_, _> = rep
                .entries
                .iter()
                .map(|e| (e.rel.clone(), e.clone()))
                .collect();
            assert_eq!(by["script.sh"].mode, EntryMode::Executable);
            assert_eq!(by["link"].mode, EntryMode::Symlink);
            assert_eq!(by["link"].symlink_target.as_deref(), Some("script.sh"));
            // symlink targets are recorded verbatim, never followed
            assert_eq!(by["link2"].symlink_target.as_deref(), Some("../escape"));
        }
        assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    }

    #[test]
    fn walk_applies_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        w(root, "keep.txt", b"k");
        w(root, "drop.log", b"d");
        w(root, "build/x.o", b"o");
        let ig = IgnoreSet::parse("*.log\nbuild/\n").unwrap();
        let rep = walk(root, &ig, &Limits::default()).unwrap();
        let rels: Vec<&str> = rep.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["keep.txt"]);
        assert!(rep.ignored >= 2);
    }

    #[test]
    fn walk_limits_depth() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = dir.path().to_path_buf();
        for i in 0..10 {
            p = p.join(format!("d{i}"));
        }
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("f.txt"), b"x").unwrap();
        let limits = Limits {
            max_depth: 5,
            ..Limits::default()
        };
        let r = walk(dir.path(), &IgnoreSet::empty(), &limits);
        assert!(matches!(r, Err(Error::Limit(_))));
        assert!(walk(dir.path(), &IgnoreSet::empty(), &Limits::default()).is_ok());
    }

    #[test]
    fn walk_rejects_oversized_component() {
        // filesystems cap names at 255 bytes; exercise the *configured*
        // limit below that so the check (not the OS) rejects.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        w(root, "longname.txt", b"x");
        let limits = Limits {
            max_path_component: 8,
            ..Limits::default()
        };
        let r = walk(root, &IgnoreSet::empty(), &limits);
        assert!(matches!(r, Err(Error::Limit(_))));
        assert!(walk(root, &IgnoreSet::empty(), &Limits::default()).is_ok());
    }

    #[test]
    fn walk_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("empty")).unwrap();
        let rep = walk(dir.path(), &IgnoreSet::empty(), &Limits::default()).unwrap();
        // empty directories are not tracked (documented, git-like)
        assert!(rep.entries.is_empty());
    }
}
