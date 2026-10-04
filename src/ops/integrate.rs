//! Integration: atomic merge of another snapshot into a workspace position,
//! plus rollback and workspace re-checkout.
//!
//! Atomicity contract:
//! * conflicts ⇒ error, NOTHING is written (no ref move, no file changes);
//! * the durable commit point is the journal fsync inside the txn engine —
//!   crash before it ⇒ position unchanged (merge objects may remain as
//!   unreachable GC fodder); crash after it ⇒ recovery completes the ref
//!   move. The post-commit workspace checkout is NOT journaled: a crash
//!   between txn and checkout leaves position ahead of files — detectable by
//!   `status` and repaired by `newgit checkout` (documented, tested).

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::merge::base::{is_ancestor, merge_base};
use crate::merge::{merge_trees, MergeOpts, MergeOutcome};
use crate::object::types::{Object, Snapshot, Tree};
use crate::object::ObjectId;
use crate::ops::checkout::{checkout_tree, CheckoutMode, CheckoutReport};
use crate::ops::snapshot::capture_tree;
use crate::repo::index::Index;
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::workspace;
use crate::repo::Repo;
use crate::util::fault;

#[derive(Clone, Debug)]
pub struct IntegrateRequest {
    pub workspace: String,
    /// Snapshot to integrate (typically another workspace's position).
    pub other: ObjectId,
    pub message: Option<String>,
    pub author: ObjectId,
    pub timestamp_ms: Option<i64>,
    pub merge_opts: MergeOpts,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum IntegrateOutcome {
    /// `other` is already contained in the position (or equal to it).
    UpToDate { position: ObjectId },
    /// Position moved to `other` without a merge commit.
    FastForward {
        from: Option<ObjectId>,
        to: ObjectId,
    },
    /// A merge snapshot was created.
    Merged {
        oid: ObjectId,
        root: ObjectId,
        parents: Vec<ObjectId>,
        entries: usize,
        renames: Vec<(String, String)>,
    },
}

pub fn integrate(repo: &Repo, req: &IntegrateRequest) -> Result<IntegrateOutcome> {
    // lock FIRST, then read the position — serializes concurrent integrates
    // on the same workspace; CAS remains the final guard.
    let _lock = workspace::lock(repo, &req.workspace)?;
    let ws = workspace::info(repo, &req.workspace)?;
    let other_obj = repo.objects.get(&req.other)?;
    let other_snap = other_obj
        .as_snapshot()
        .map_err(|e| Error::Invalid(format!("integrate target must be a snapshot: {e}")))?;
    let ours = ws.head_oid;

    // unborn position ⇒ fast-forward to other
    if ours.is_none() {
        return ff_or_merge_unborn(repo, req, &ws.ref_name, other_snap.root);
    }
    let ours = ours.unwrap();
    if ours == req.other {
        return Ok(IntegrateOutcome::UpToDate { position: ours });
    }
    if is_ancestor(repo, req.other, ours)? {
        return Ok(IntegrateOutcome::UpToDate { position: ours });
    }
    if is_ancestor(repo, ours, req.other)? {
        commit_ref_move(repo, &ws.ref_name, ours, req.other, req, "fast-forward")?;
        checkout_position(repo, &req.workspace)?;
        return Ok(IntegrateOutcome::FastForward {
            from: Some(ours),
            to: req.other,
        });
    }

    // 3-way merge
    let base_oid = merge_base(repo, ours, req.other)?;
    let empty_tree = repo.objects.put(&Object::Tree(Tree::empty()))?;
    let base_root = match base_oid {
        Some(b) => repo.objects.get(&b)?.as_snapshot()?.root,
        None => empty_tree,
    };
    let our_root = repo.objects.get(&ours)?.as_snapshot()?.root;
    let outcome: MergeOutcome =
        merge_trees(repo, base_root, our_root, other_snap.root, &req.merge_opts)?;
    if !outcome.clean {
        return Err(conflict_error(&outcome));
    }

    let ts = req.timestamp_ms.unwrap_or_else(txn::now_ms);
    let message = req
        .message
        .clone()
        .unwrap_or_else(|| format!("integrate {} into {}", req.other.short(), req.workspace));
    let snap = Snapshot {
        parents: [ours, req.other].into_iter().collect(),
        root: outcome.root,
        author: req.author,
        timestamp_ms: ts,
        tz_offset_min: 0,
        message,
        workspace: Some(req.workspace.clone()),
        change: None,
        goal: None,
        extras: {
            // parents is a canonical *sorted set* (D-004) — merge roles live
            // in extras so rollback can find "ours" unambiguously.
            let mut m = BTreeMap::new();
            m.insert("op".to_string(), "integrate".to_string());
            m.insert("merge_ours".to_string(), ours.to_hex());
            m.insert("merge_theirs".to_string(), req.other.to_hex());
            m
        },
    };
    snap.validate()?;
    let oid = repo.objects.put(&Object::Snapshot(snap))?;
    fault::fault("integ:before_txn")?;
    commit_ref_move(repo, &ws.ref_name, ours, oid, req, "merge")?;
    let _ = fault::fault_action("integ:after_txn");
    checkout_position(repo, &req.workspace)?;
    let _ = fault::fault_action("integ:after_checkout");
    Ok(IntegrateOutcome::Merged {
        oid,
        root: outcome.root,
        parents: vec![ours, req.other],
        entries: outcome.entries,
        renames: outcome.renames,
    })
}

fn ff_or_merge_unborn(
    repo: &Repo,
    req: &IntegrateRequest,
    ref_name: &str,
    other_root: ObjectId,
) -> Result<IntegrateOutcome> {
    let _ = other_root;
    fault::fault("integ:before_txn")?;
    txn::execute(
        repo.ng(),
        vec![TxnOp::Ref {
            name: ref_name.to_string(),
            cas: Cas::Exactly(None),
            new: Some(req.other),
            log: RefLogEntry {
                actor: Some(req.author),
                ts_ms: req.timestamp_ms.unwrap_or_else(txn::now_ms),
                message: "integrate: fast-forward (unborn)".into(),
            },
        }],
        repo.limits(),
    )?;
    let _ = fault::fault_action("integ:after_txn");
    checkout_position(repo, &req.workspace)?;
    Ok(IntegrateOutcome::FastForward {
        from: None,
        to: req.other,
    })
}

fn commit_ref_move(
    repo: &Repo,
    ref_name: &str,
    expect: ObjectId,
    new: ObjectId,
    req: &IntegrateRequest,
    kind: &str,
) -> Result<()> {
    fault::fault("integ:before_txn")?;
    txn::execute(
        repo.ng(),
        vec![TxnOp::Ref {
            name: ref_name.to_string(),
            cas: Cas::Exactly(Some(expect)),
            new: Some(new),
            log: RefLogEntry {
                actor: Some(req.author),
                ts_ms: req.timestamp_ms.unwrap_or_else(txn::now_ms),
                message: format!("integrate: {kind} to {}", new.short()),
            },
        }],
        repo.limits(),
    )?;
    Ok(())
}

/// Build the Conflict error carrying a deterministic summary.
pub fn conflict_error(outcome: &MergeOutcome) -> Error {
    let mut msg = format!("merge produced {} conflict(s):", outcome.conflicts.len());
    for c in outcome.conflicts.iter().take(20) {
        msg.push_str(&format!("\n  [{}] {}", conflict_kind(c), c.path()));
    }
    if outcome.conflicts.len() > 20 {
        msg.push_str(&format!("\n  … and {} more", outcome.conflicts.len() - 20));
    }
    msg.push_str(
        "\ninspect with `newgit merge-tree`; resolve and snapshot, or integrate a fixed snapshot",
    );
    Error::Conflict(msg)
}

fn conflict_kind(c: &crate::merge::Conflict) -> &'static str {
    match c {
        crate::merge::Conflict::Content { .. } => "content",
        crate::merge::Conflict::Opaque { .. } => "opaque",
        crate::merge::Conflict::ModifyDelete { .. } => "modify/delete",
        crate::merge::Conflict::RenameRename { .. } => "rename/rename",
        crate::merge::Conflict::RenameDelete { .. } => "rename/delete",
        crate::merge::Conflict::Mode { .. } => "mode",
        crate::merge::Conflict::DirectoryFile { .. } => "dir/file",
    }
}

/// Resynchronize a workspace's files with its position (used after
/// integrate/rollback and by `newgit checkout`).
pub fn checkout_position(repo: &Repo, ws_name: &str) -> Result<CheckoutReport> {
    let ws = workspace::info(repo, ws_name)?;
    let Some(head) = ws.head_oid else {
        return Err(Error::Invalid(format!(
            "workspace {ws_name:?} is unborn — nothing to check out"
        )));
    };
    let root = repo.objects.get(&head)?.as_snapshot()?.root;
    // The OLD index lists what NewGit itself materialized last time. Files in
    // it that the target tree no longer contains are removed (git removes
    // tracked files on switch). Files NewGit never tracked are never touched.
    let old_index = Index::load(&workspace::index_path(repo, ws_name));
    let mut index = Index::new();
    let mut report = checkout_tree(
        repo,
        root,
        &ws.dir,
        CheckoutMode::Overwrite,
        Some(&mut index),
    )?;
    for (p, _) in old_index.iter() {
        if index.get(p).is_none() {
            let victim = ws.dir.join(p);
            match std::fs::remove_file(&victim) {
                Ok(()) => report.files += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::io(&victim, e)),
            }
            // prune now-empty parent dirs (bottom-up, stop at workspace root)
            let mut cur = victim.parent().map(|p| p.to_path_buf());
            while let Some(dir) = cur {
                if dir == ws.dir {
                    break;
                }
                let empty = std::fs::read_dir(&dir)
                    .map(|mut it| it.next().is_none())
                    .unwrap_or(false);
                if !empty {
                    break;
                }
                if std::fs::remove_dir(&dir).is_err() {
                    break;
                }
                cur = dir.parent().map(|p| p.to_path_buf());
            }
        }
    }
    index.save(&workspace::index_path(repo, ws_name))?;
    Ok(report)
}

#[derive(Clone, Debug)]
pub struct RollbackRequest {
    pub workspace: String,
    /// Snapshot whose TREE becomes the new content. Default: first parent of
    /// the current position (i.e. "undo the last snapshot/merge").
    pub target: Option<ObjectId>,
    pub message: Option<String>,
    pub author: ObjectId,
    pub timestamp_ms: Option<i64>,
}

/// Rollback = a NEW snapshot with an old tree. History is never rewritten;
/// the rollback itself is auditable (parents + message + extras).
pub fn rollback(repo: &Repo, req: &RollbackRequest) -> Result<ObjectId> {
    let _lock = workspace::lock(repo, &req.workspace)?;
    let ws = workspace::info(repo, &req.workspace)?;
    let Some(cur) = ws.head_oid else {
        return Err(Error::Invalid(
            "position is unborn — nothing to roll back".into(),
        ));
    };
    let cur_snap = repo.objects.get(&cur)?.as_snapshot()?.clone();
    let target = match req.target {
        Some(t) => t,
        None => {
            // merge snapshots: roll back to the "ours" parent (role recorded
            // in extras because parents is a sorted set); otherwise the sole
            // parent.
            if let Some(hex) = cur_snap.extras.get("merge_ours") {
                ObjectId::from_hex(hex)?
            } else {
                *cur_snap.parents.first().ok_or_else(|| {
                    Error::Invalid(
                        "no parent to roll back to; pass --to <snapshot> explicitly".into(),
                    )
                })?
            }
        }
    };
    let target_obj = repo.objects.get(&target)?;
    let target_snap = target_obj
        .as_snapshot()
        .map_err(|e| Error::Invalid(format!("rollback target must be a snapshot: {e}")))?;
    let ts = req.timestamp_ms.unwrap_or_else(txn::now_ms);
    let message = req
        .message
        .clone()
        .unwrap_or_else(|| format!("rollback to {}", target.short()));
    let snap = Snapshot {
        parents: [cur].into_iter().collect(),
        root: target_snap.root,
        author: req.author,
        timestamp_ms: ts,
        tz_offset_min: 0,
        message,
        workspace: Some(req.workspace.clone()),
        change: None,
        goal: None,
        extras: {
            let mut m = BTreeMap::new();
            m.insert("op".to_string(), "rollback".to_string());
            m.insert("target".to_string(), target.to_hex());
            m
        },
    };
    snap.validate()?;
    let oid = repo.objects.put(&Object::Snapshot(snap))?;
    fault::fault("rollback:before_txn")?;
    txn::execute(
        repo.ng(),
        vec![TxnOp::Ref {
            name: ws.ref_name.clone(),
            cas: Cas::Exactly(Some(cur)),
            new: Some(oid),
            log: RefLogEntry {
                actor: Some(req.author),
                ts_ms: ts,
                message: format!("rollback: to {}", target.short()),
            },
        }],
        repo.limits(),
    )?;
    checkout_position(repo, &req.workspace)?;
    Ok(oid)
}

/// Dry-run merge for inspection (`newgit merge-tree`): never writes refs or
/// workspace files; conflict blobs ARE stored (harmless, GC-able) so they can
/// be inspected with `cat`.
pub fn merge_tree_dry(
    repo: &Repo,
    ours_spec: &str,
    theirs_spec: &str,
    base_spec: Option<&str>,
    opts: &MergeOpts,
) -> Result<MergeOutcome> {
    let ours = crate::diff::resolve_tree(repo, ours_spec)?;
    let theirs = crate::diff::resolve_tree(repo, theirs_spec)?;
    let base = match base_spec {
        Some(s) => crate::diff::resolve_tree(repo, s)?,
        None => {
            // try snapshot-level LCA when both specs are snapshots
            let o = crate::repo::workspace::resolve_base(repo, Some(ours_spec))?;
            let t = crate::repo::workspace::resolve_base(repo, Some(theirs_spec))?;
            match (o, t) {
                (Some(a), Some(b)) => match merge_base(repo, a, b)? {
                    Some(m) => repo.objects.get(&m)?.as_snapshot()?.root,
                    None => repo.objects.put(&Object::Tree(Tree::empty()))?,
                },
                _ => repo.objects.put(&Object::Tree(Tree::empty()))?,
            }
        }
    };
    merge_trees(repo, base, ours, theirs, opts)
}

/// Unused-but-exported helper for the CLI: capture the live worktree tree.
pub fn worktree_root(repo: &Repo, ws_name: &str) -> Result<ObjectId> {
    Ok(capture_tree(repo, ws_name, false)?.root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_error_message_is_bounded() {
        let outcome = MergeOutcome {
            clean: false,
            root: ObjectId::from_bytes([0; 32]),
            entries: 0,
            conflicts: (0..30)
                .map(|i| crate::merge::Conflict::ModifyDelete {
                    path: format!("f{i}"),
                    modified_by: "ours".into(),
                    kept_oid: ObjectId::from_bytes([i; 32]),
                })
                .collect(),
            renames: vec![],
        };
        let e = conflict_error(&outcome);
        let msg = e.to_string();
        assert!(msg.contains("30 conflict(s)"));
        assert!(msg.contains("and 10 more"));
        assert!(msg.len() < 4096);
    }
}
