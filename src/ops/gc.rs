//! `newgit gc` — safe mark-and-sweep garbage collection.
//!
//! Safety model:
//! * The global txn LOCK is held for the whole run (after a `recover()`
//!   pass), so no ref can move between root collection and the sweep.
//! * Roots = HEAD, every ref value (refs/, workspaces/, chains/), every
//!   reflog OLD/NEW oid (audit history is never collected), and every
//!   workspace `base_oid`.
//! * Mark follows every object link, including `extras.prev` chain links.
//! * Unreachable objects younger than the grace window (mtime) are kept:
//!   a concurrent process may have written the object and be about to
//!   reference it (object writes do not take the txn lock). `--force-now`
//!   disables the grace window — intended for tests and single-user use.
//! * Quarantine (`*.corrupt`) is NEVER touched — forensic evidence stays
//!   until a human deletes it.
//! * Deletion of unreachable objects needs no journaling: it can only ever
//!   remove data that no ref, reflog, or chain can reach.
//! * gc never deletes data it cannot fully decode: corrupt or misfiled
//!   files are kept and counted (`kept_corrupt`) for `verify` and human
//!   forensics — the same policy as the quarantine.
//! * A damaged repo does not block gc: unreadable/missing links are
//!   counted and reported, and everything reachable from the remaining
//!   roots is kept.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::error::Result;
use crate::object::envelope;
use crate::object::ObjectId;
use crate::repo::{txn, Repo};
use crate::util::fsx;

use super::verify;

/// Unreachable objects younger than this are kept (concurrent-writer guard).
pub const GRACE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug)]
pub struct GcOpts {
    /// Report what would be deleted; delete nothing.
    pub dry_run: bool,
    /// Ignore the mtime grace window (tests / single-user repos).
    pub force_now: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GcReport {
    pub live_objects: usize,
    pub deleted_objects: usize,
    pub freed_bytes: u64,
    /// Unreachable but younger than the grace window.
    pub kept_young: usize,
    /// Quarantine files seen (never touched).
    pub quarantined: usize,
    /// Unreachable but unreadable (corrupt/misfiled): kept for forensics —
    /// gc never deletes data it cannot fully decode and identify.
    pub kept_corrupt: usize,
    /// Root/link oids that could not be read (verify reports the damage).
    pub missing_links: usize,
    pub dry_run: bool,
    pub deleted_oids: Vec<String>,
}

pub fn gc(repo: &Repo, opts: &GcOpts) -> Result<GcReport> {
    // Apply any committed-but-unapplied journals first, so their NEW oids
    // are visible in refs when we collect roots.
    let (_recovery, _applied) = repo.recover()?;

    let ng = repo.ng().to_path_buf();
    fsx::ensure_dir(&ng.join("txn"))?;
    let (wait_ms, stale_s) = (repo.limits().lock_wait_ms, repo.limits().lock_stale_s);
    let _lock = fsx::FileLock::acquire(
        &ng.join("txn").join("LOCK"),
        Duration::from_millis(wait_ms),
        Duration::from_secs(stale_s),
    )?;

    let max_object_bytes = repo.limits().max_object_bytes;
    let roots = collect_roots(repo)?;
    let (live, missing) = verify::reachable(repo, &roots);

    let mut rep = GcReport {
        live_objects: live.len(),
        missing_links: missing.len(),
        dry_run: opts.dry_run,
        ..Default::default()
    };

    let objects_dir = ng.join("objects");
    let now = SystemTime::now();
    let mut touched_shards: BTreeSet<PathBuf> = BTreeSet::new();

    for (shard, file) in object_files(&objects_dir) {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.ends_with(".corrupt") {
            rep.quarantined += 1;
            continue; // quarantine is forensic evidence — never collected
        }
        // Object files are <shard:2 hex>/<rest:62 hex>; only delete files
        // that match the layout exactly — anything else is debris that
        // `verify` reports and humans remove.
        let shard_name = shard
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if shard_name.len() != 2 || name.len() != 62 {
            continue;
        }
        let Ok(oid) = ObjectId::from_hex(&format!("{shard_name}{name}")) else {
            continue;
        };
        if repo.objects.path_for(&oid) != file {
            rep.kept_corrupt += 1;
            continue;
        }
        if live.contains(&oid) {
            continue;
        }
        // Strictly non-destructive: collect only objects that decode
        // cleanly AND match their file name. Anything unreadable stays
        // for verify/forensics (quarantine policy).
        let raw = match std::fs::read(&file) {
            Ok(r) => r,
            Err(_) => {
                rep.kept_corrupt += 1;
                continue;
            }
        };
        match envelope::decode(&raw, max_object_bytes) {
            Ok((_, id)) if id == oid => {}
            _ => {
                rep.kept_corrupt += 1;
                continue;
            }
        }
        // Grace window: protect in-flight writes by concurrent processes.
        if !opts.force_now {
            if let Ok(md) = std::fs::metadata(&file) {
                if let Ok(mtime) = md.modified() {
                    if now.duration_since(mtime).unwrap_or(Duration::ZERO) < GRACE {
                        rep.kept_young += 1;
                        continue;
                    }
                }
            }
        }
        let size = raw.len() as u64;
        if !opts.dry_run {
            repo.objects.remove(&oid)?;
            touched_shards.insert(shard);
        }
        rep.deleted_objects += 1;
        rep.freed_bytes += size;
        rep.deleted_oids.push(oid.to_hex());
    }

    // Clean up empty shard directories, then make deletions durable.
    if !opts.dry_run {
        for shard in &touched_shards {
            if let Ok(mut rd) = std::fs::read_dir(shard) {
                if rd.next().is_none() {
                    let _ = std::fs::remove_dir(shard);
                }
            }
            let _ = fsx::fsync_dir(shard);
        }
        if !touched_shards.is_empty() {
            let _ = fsx::fsync_dir(&objects_dir);
        }
    }
    Ok(rep)
}

/// Every oid the repo can still reach through refs, reflogs, HEAD, or
/// workspace metadata.
fn collect_roots(repo: &Repo) -> Result<Vec<ObjectId>> {
    let ng = repo.ng();
    let mut roots: Vec<ObjectId> = Vec::new();

    // HEAD (symbolic → ref value; detached → oid).
    match repo.read_head()? {
        crate::repo::Head::Symbolic(name) => {
            if let Ok(Some(oid)) = txn::read_ref_raw(ng, &name) {
                roots.push(oid);
            }
        }
        crate::repo::Head::Detached(oid) => roots.push(oid),
    }

    // All refs (refs/, workspaces/, chains/) by value.
    let mut names: Vec<String> = Vec::new();
    verify::collect_ref_files(&ng.join("refs"), "", &mut names);
    for name in &names {
        if let Ok(Some(oid)) = txn::read_ref_raw(ng, name) {
            roots.push(oid);
        }
    }

    // Reflogs: OLD and NEW of every line — audit history is a root, so
    // gc can never rewrite what the reflog claims happened. Also scan logs
    // of refs that no longer exist (deleted positions keep their history).
    let mut log_names: Vec<String> = Vec::new();
    verify::collect_ref_files(&ng.join("logs").join("refs"), "", &mut log_names);
    for name in log_names.iter().chain(names.iter()) {
        let log = ng.join("logs").join("refs").join(name);
        if let Ok(text) = std::fs::read_to_string(&log) {
            for line in text.lines() {
                let f: Vec<&str> = line.split_whitespace().collect();
                for hexish in f.iter().take(2) {
                    if *hexish != "ZERO" {
                        if let Ok(oid) = ObjectId::from_hex(hexish) {
                            roots.push(oid);
                        }
                    }
                }
            }
        }
    }

    // Workspace base_oid (a fresh workspace's checkout target must survive
    // even before its first snapshot).
    let ws_dir = ng.join("workspaces");
    if let Ok(rd) = std::fs::read_dir(&ws_dir) {
        for e in rd.flatten() {
            let meta = e.path().join("meta");
            if let Ok(text) = std::fs::read_to_string(&meta) {
                for line in text.lines() {
                    if let Some(v) = line.trim().strip_prefix("base_oid=") {
                        if let Ok(oid) = ObjectId::from_hex(v.trim()) {
                            roots.push(oid);
                        }
                    }
                }
            }
        }
    }
    Ok(roots)
}

/// Sorted list of (shard dir, object file) under the object store.
fn object_files(objects_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(objects_dir) else {
        return out;
    };
    let mut shards: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    shards.sort();
    for shard in shards {
        let Ok(rd2) = std::fs::read_dir(&shard) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd2.flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files {
            out.push((shard.clone(), f));
        }
    }
    out
}
