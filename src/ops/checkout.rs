//! Materialize a tree into a directory (workspace creation, rollback).
//!
//! Safety rules (THREAT_MODEL §B):
//! * every path re-validated before use (`check_rel_path`),
//! * refuse to write *through* any symlinked path component,
//! * never write outside `dest` (defense in depth via component checks),
//! * blob content comes from verified objects only.

use std::path::Path;

use crate::error::{Error, Result};
use crate::object::types::EntryMode;
use crate::object::ObjectId;
use crate::ops::tree::flatten_tree;
use crate::repo::index::{Index, IndexEntry};
use crate::repo::Repo;
use crate::util::fsx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckoutMode {
    /// Destination must be an empty directory (fresh workspace).
    FreshWorkspace,
    /// Replace tracked paths in place; untracked files are left alone.
    Overwrite,
}

#[derive(Clone, Debug, Default)]
pub struct CheckoutReport {
    pub files: usize,
    pub symlinks: usize,
    pub bytes: u64,
}

fn mtime_of_file(p: &Path) -> (u64, u32) {
    std::fs::metadata(p)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0))
}

/// Refuse to traverse symlinked components between `dest` and `rel`.
fn check_no_symlink_components(dest: &Path, rel: &str) -> Result<()> {
    let mut cur = dest.to_path_buf();
    for seg in rel.split('/') {
        // check the component BEFORE descending (the final component may be
        // replaced; intermediates must be real directories)
        match std::fs::symlink_metadata(&cur) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(Error::Invalid(format!(
                        "refusing to write through symlink component: {}",
                        cur.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::io(&cur, e)),
        }
        cur = cur.join(seg);
    }
    Ok(())
}

pub fn checkout_tree(
    repo: &Repo,
    root: ObjectId,
    dest: &Path,
    mode: CheckoutMode,
    mut index: Option<&mut Index>,
) -> Result<CheckoutReport> {
    let mut report = CheckoutReport::default();
    if !dest.exists() {
        fsx::ensure_dir(dest)?;
    }
    if !dest.is_dir() {
        return Err(Error::Invalid(format!(
            "checkout destination is not a directory: {}",
            dest.display()
        )));
    }
    if mode == CheckoutMode::FreshWorkspace {
        let mut it = std::fs::read_dir(dest).map_err(|e| Error::io(dest, e))?;
        if it.next().is_some() {
            return Err(Error::Invalid(format!(
                "fresh checkout requires an empty directory: {}",
                dest.display()
            )));
        }
    }
    let entries = flatten_tree(repo, root)?;
    let limits = repo.limits().clone();
    for (rel, emode, oid) in entries {
        fsx::check_rel_path(&rel, limits.max_path_component)?;
        check_no_symlink_components(dest, &rel)?;
        ensure_dirs_safe(dest, &rel)?;
        let target = dest.join(&rel);
        // existing entry handling
        match std::fs::symlink_metadata(&target) {
            Ok(md) => {
                let is_link = md.file_type().is_symlink();
                let is_dir = md.is_dir();
                if is_dir && !emode.is_tree() {
                    return Err(Error::Conflict(format!(
                        "cannot write file {rel}: a directory exists there"
                    )));
                }
                if mode == CheckoutMode::FreshWorkspace {
                    return Err(Error::Bug("fresh checkout found existing file".into()));
                }
                if is_link || !md.is_dir() {
                    std::fs::remove_file(&target).map_err(|e| Error::io(&target, e))?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::io(&target, e)),
        }
        match emode {
            EntryMode::Symlink => {
                let obj = repo.objects.get(&oid)?;
                let content = obj.as_blob()?;
                let target_str = std::str::from_utf8(content).map_err(|_| Error::Corrupt {
                    oid,
                    reason: "symlink blob is not utf-8".into(),
                })?;
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(target_str, &target)
                        .map_err(|e| Error::io(&target, e))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = target_str;
                    return Err(Error::Invalid(
                        "symlink checkout is only supported on unix".into(),
                    ));
                }
                report.symlinks += 1;
                if let Some(idx) = index.as_deref_mut() {
                    let (sec, nsec) = mtime_of_file(&target);
                    idx.insert(
                        rel.clone(),
                        IndexEntry {
                            oid,
                            size: content.len() as u64,
                            mtime_sec: sec,
                            mtime_nsec: nsec,
                            mode: EntryMode::Symlink,
                        },
                    );
                }
            }
            EntryMode::File | EntryMode::Executable => {
                let obj = repo.objects.get(&oid)?;
                let content = obj.as_blob()?;
                fsx::atomic_write(&target, content)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let bits = if emode == EntryMode::Executable {
                        0o755
                    } else {
                        0o644
                    };
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(bits))
                        .map_err(|e| Error::io(&target, e))?;
                }
                report.files += 1;
                report.bytes += content.len() as u64;
                if let Some(idx) = index.as_deref_mut() {
                    let (sec, nsec) = mtime_of_file(&target);
                    idx.insert(
                        rel.clone(),
                        IndexEntry {
                            oid,
                            size: content.len() as u64,
                            mtime_sec: sec,
                            mtime_nsec: nsec,
                            mode: emode,
                        },
                    );
                }
            }
            EntryMode::Tree => return Err(Error::Bug("flatten_tree returned a tree entry".into())),
        }
    }
    // fsync the destination tree (best effort, parents included)
    fsx::fsync_dir(dest)?;
    Ok(report)
}

/// Create parent directories of `rel` under `dest` one component at a time,
/// refusing to traverse anything that is not a real directory.
fn ensure_dirs_safe(dest: &Path, rel: &str) -> Result<()> {
    let mut cur = dest.to_path_buf();
    let comps: Vec<&str> = rel.split('/').collect();
    for seg in &comps[..comps.len() - 1] {
        cur = cur.join(seg);
        match std::fs::symlink_metadata(&cur) {
            Ok(md) => {
                if md.file_type().is_symlink() {
                    return Err(Error::Invalid(format!(
                        "refusing to create directory through symlink: {}",
                        cur.display()
                    )));
                }
                if !md.is_dir() {
                    return Err(Error::Conflict(format!(
                        "cannot create directory {}: a file exists",
                        cur.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&cur).map_err(|e| Error::io(&cur, e))?;
            }
            Err(e) => return Err(Error::io(&cur, e)),
        }
    }
    Ok(())
}
