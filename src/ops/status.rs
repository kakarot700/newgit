//! Workspace status: compare live files against the workspace position.
//! Read-only: never writes the store or the index (the index is consulted as
//! a cache; mismatches fall back to hashing content in memory).

use std::collections::BTreeMap;

use crate::error::Result;
use crate::object::types::{EntryMode, Object};
use crate::object::ObjectId;
use crate::ops::tree::flatten_tree;
use crate::repo::ignore::IgnoreSet;
use crate::repo::index::Index;
use crate::repo::workspace;
use crate::repo::Repo;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct StatusReport {
    pub workspace: String,
    pub head: Option<ObjectId>,
    pub root_tree: Option<ObjectId>,
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub deleted: Vec<String>,
    pub ignored: usize,
    pub clean: bool,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

pub fn status(repo: &Repo, ws_name: &str, list_limit: usize) -> Result<StatusReport> {
    let ws = workspace::info(repo, ws_name)?;
    let mut report = StatusReport {
        workspace: ws_name.to_string(),
        head: ws.head_oid,
        ..Default::default()
    };

    // tracked side: flatten current position tree
    let mut tracked: BTreeMap<String, (EntryMode, ObjectId)> = BTreeMap::new();
    if let Some(head) = ws.head_oid {
        let obj = repo.objects.get(&head)?;
        let snap = obj.as_snapshot()?;
        report.root_tree = Some(snap.root);
        for (p, m, o) in flatten_tree(repo, snap.root)? {
            tracked.insert(p, (m, o));
        }
    }

    // live side: walk with index cache
    let ignore = IgnoreSet::load(&ws.dir.join(".newgitignore"))?;
    let walk = crate::repo::walk::walk(&ws.dir, &ignore, repo.limits())?;
    report.ignored = walk.ignored;
    report.warnings = walk.warnings;
    let (index, idx_mtime) = Index::load_with_mtime(&workspace::index_path(repo, ws_name));

    let mut live: BTreeMap<String, (EntryMode, ObjectId)> = BTreeMap::new();
    for e in &walk.entries {
        // index fast path (cache-only: if it disagrees — or is racily clean
        // — we hash for real)
        if let Some(c) = index.get(&e.rel) {
            if c.mode == e.mode
                && crate::repo::index::cache_trustworthy(
                    c,
                    (e.mtime_sec, e.mtime_nsec, e.size),
                    idx_mtime,
                )
            {
                live.insert(e.rel.clone(), (e.mode, c.oid));
                continue;
            }
        }
        let oid = match (&e.symlink_target, e.mode) {
            (Some(target), EntryMode::Symlink) => Object::Blob(target.as_bytes().to_vec()).id(),
            _ => {
                let p = ws.dir.join(&e.rel);
                match std::fs::read(&p) {
                    Ok(data) => Object::Blob(data).id(),
                    Err(err) => {
                        report
                            .warnings
                            .push(format!("cannot read {}: {err}", e.rel));
                        continue;
                    }
                }
            }
        };
        live.insert(e.rel.clone(), (e.mode, oid));
    }

    // classify
    let mut added_n = 0usize;
    let mut mod_n = 0usize;
    let mut del_n = 0usize;
    for (p, (m, o)) in &live {
        match tracked.get(p) {
            None => {
                added_n += 1;
                if report.added.len() < list_limit {
                    report.added.push(p.clone());
                }
            }
            Some((tm, to)) => {
                if tm != m || to != o {
                    mod_n += 1;
                    if report.modified.len() < list_limit {
                        report.modified.push(p.clone());
                    }
                }
            }
        }
    }
    for p in tracked.keys() {
        if !live.contains_key(p) {
            del_n += 1;
            if report.deleted.len() < list_limit {
                report.deleted.push(p.clone());
            }
        }
    }
    report.truncated = added_n > report.added.len()
        || mod_n > report.modified.len()
        || del_n > report.deleted.len();
    report.clean = added_n == 0 && mod_n == 0 && del_n == 0;
    Ok(report)
}
