//! Snapshot creation: workspace walk → content hashing (index-accelerated)
//! → tree build → Snapshot object → journaled ref update.
//!
//! `capture_tree` is the read-only core (also used by `diff`): it never
//! touches refs and only writes the index cache when asked.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::object::types::{EntryMode, Object, Snapshot};
use crate::object::ObjectId;
use crate::ops::tree::build_tree;
use crate::repo::ignore::IgnoreSet;
use crate::repo::index::{Index, IndexEntry};
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::workspace;
use crate::repo::Repo;
use crate::util::{fault, fsx};

#[derive(Clone, Debug, serde::Serialize)]
pub struct SnapshotRequest {
    pub workspace: String,
    pub message: String,
    pub author: ObjectId,
    /// None ⇒ current wall-clock time (UTC millis). Tests pass fixed values
    /// for determinism.
    pub timestamp_ms: Option<i64>,
    pub tz_offset_min: i16,
    pub goal: Option<ObjectId>,
    pub change: Option<ObjectId>,
    pub extras: BTreeMap<String, String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct SnapshotOutcome {
    pub oid: ObjectId,
    pub snapshot: Snapshot,
    pub root: ObjectId,
    pub ref_name: String,
    pub parent: Option<ObjectId>,
    pub entries: usize,
    pub hashed: usize,
    pub reused: usize,
    pub warnings: Vec<String>,
}

/// Result of capturing a workspace's live state as a tree.
#[derive(Clone, Debug, serde::Serialize)]
pub struct CaptureStats {
    pub root: ObjectId,
    pub entries: usize,
    pub hashed: usize,
    pub reused: usize,
    pub warnings: Vec<String>,
}

/// Walk + hash + build the tree for a workspace WITHOUT touching refs.
/// When `save_index` is false the index cache is only read, never written
/// (read-only callers like diff/status).
pub fn capture_tree(repo: &Repo, ws_name: &str, save_index: bool) -> Result<CaptureStats> {
    let ws = workspace::info(repo, ws_name)?;
    let _lock = workspace::lock(repo, ws_name)?;
    let ignore = IgnoreSet::load(&ws.dir.join(".newgitignore"))?;
    let report = workspace_walk(repo, &ws.dir, &ignore)?;

    let (mut index, idx_mtime) = Index::load_with_mtime(&workspace::index_path(repo, ws_name));
    let mut items: Vec<(String, ObjectId, EntryMode)> = Vec::with_capacity(report.entries.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut hashed = 0usize;
    let mut reused = 0usize;

    for e in &report.entries {
        seen.insert(e.rel.clone());
        fsx::check_rel_path(&e.rel, repo.limits().max_path_component)?;
        // index fast path: same size+mtime+mode ⇒ same content oid, unless
        // the entry is racily clean (mtime ≥ index mtime) — then re-hash.
        if let Some(cached) = index.get(&e.rel) {
            if cached.mode == e.mode
                && crate::repo::index::cache_trustworthy(
                    cached,
                    (e.mtime_sec, e.mtime_nsec, e.size),
                    idx_mtime,
                )
            {
                items.push((e.rel.clone(), cached.oid, e.mode));
                reused += 1;
                continue;
            }
        }
        let oid = match (&e.symlink_target, e.mode) {
            (Some(target), EntryMode::Symlink) => repo.objects.put_blob(target.as_bytes())?,
            _ => {
                let p = ws.dir.join(&e.rel);
                repo.objects.put_blob_from_file(&p)?
            }
        };
        if save_index {
            index.insert(
                e.rel.clone(),
                IndexEntry {
                    oid,
                    size: e.size,
                    mtime_sec: e.mtime_sec,
                    mtime_nsec: e.mtime_nsec,
                    mode: e.mode,
                },
            );
        }
        items.push((e.rel.clone(), oid, e.mode));
        hashed += 1;
    }
    if save_index {
        // prune index entries for files that disappeared
        let stale: Vec<String> = index
            .iter()
            .filter(|(p, _)| !seen.contains(p.as_str()))
            .map(|(p, _)| p.clone())
            .collect();
        for p in stale {
            index.remove(&p);
        }
        fault::fault("snap:before_index_save")?;
        index.save(&workspace::index_path(repo, ws_name))?;
    }
    let root = build_tree(repo, &items)?;
    Ok(CaptureStats {
        root,
        entries: items.len(),
        hashed,
        reused,
        warnings: report.warnings,
    })
}

pub fn snapshot(repo: &Repo, req: &SnapshotRequest) -> Result<SnapshotOutcome> {
    let ws = workspace::info(repo, &req.workspace)?;
    let cap = capture_tree(repo, &req.workspace, true)?;
    let _ = fault::fault_action("snap:after_tree");
    let root = cap.root;

    let parent = repo.refs.read_opt(&ws.ref_name)?;
    let ts = req.timestamp_ms.unwrap_or_else(txn::now_ms);
    let snap = Snapshot {
        parents: parent.into_iter().collect(),
        root,
        author: req.author,
        timestamp_ms: ts,
        tz_offset_min: req.tz_offset_min,
        message: req.message.clone(),
        workspace: Some(req.workspace.clone()),
        change: req.change,
        goal: req.goal,
        extras: req.extras.clone(),
    };
    snap.validate()?;
    let obj = Object::Snapshot(snap.clone());
    let oid = repo.objects.put(&obj)?;
    let _ = fault::fault_action("snap:before_txn");

    let first_line = req.message.lines().next().unwrap_or("").to_string();
    txn::execute(
        repo.ng(),
        vec![TxnOp::Ref {
            name: ws.ref_name.clone(),
            cas: Cas::Exactly(parent),
            new: Some(oid),
            log: RefLogEntry {
                actor: Some(req.author),
                ts_ms: ts,
                message: format!("snapshot: {first_line}"),
            },
        }],
        repo.limits(),
    )?;
    let _ = fault::fault_action("snap:after_txn");

    Ok(SnapshotOutcome {
        oid,
        snapshot: snap,
        root,
        ref_name: ws.ref_name.clone(),
        parent,
        entries: cap.entries,
        hashed: cap.hashed,
        reused: cap.reused,
        warnings: cap.warnings,
    })
}

/// Walk helper shared with status: enumerate entries honoring ignores.
pub fn workspace_walk(
    repo: &Repo,
    dir: &std::path::Path,
    ignore: &IgnoreSet,
) -> Result<crate::repo::walk::WalkReport> {
    crate::repo::walk::walk(dir, ignore, repo.limits())
}

/// Convenience: snapshot the main workspace with defaults.
pub fn snapshot_main(repo: &Repo, message: &str) -> Result<SnapshotOutcome> {
    let author = repo.default_actor()?;
    snapshot(
        repo,
        &SnapshotRequest {
            workspace: workspace::MAIN.to_string(),
            message: message.to_string(),
            author,
            timestamp_ms: None,
            tz_offset_min: 0,
            goal: None,
            change: None,
            extras: BTreeMap::new(),
        },
    )
}

/// Error helper for CLI messages when HEAD is detached etc.
pub fn ensure_symbolic_head(repo: &Repo) -> Result<String> {
    match repo.read_head()? {
        crate::repo::Head::Symbolic(r) => Ok(r),
        crate::repo::Head::Detached(_) => Err(Error::Invalid(
            "HEAD is detached; snapshots need a ref. Use `newgit workspace create` \
             or re-attach HEAD."
                .into(),
        )),
    }
}
