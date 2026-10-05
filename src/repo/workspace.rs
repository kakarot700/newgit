//! Workspaces: isolated areas where one actor (human, agent, process) works.
//!
//! * `main` — the repository root checkout; its position is HEAD's ref.
//! * `<name>` — lives in `.newgit/workspaces/<name>/files/…`; its position is
//!   the system ref `workspaces/<name>`.
//!
//! Each workspace directory holds `meta` (key=value), `index` (NGIX cache),
//! and `files/` (except `main`, whose files are the repo root itself).
//! Creation is journaled: checkout first, then one transaction creating the
//! ref and the meta file together (crash between ⇒ orphan `files/` dir,
//! swept by `verify`/`discard`; crash mid-txn ⇒ recovery completes it).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::object::types::Snapshot;
use crate::object::ObjectId;
use crate::repo::index::Index;
use crate::repo::refs::check_workspace_segment;
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;
use crate::util::fsx;

pub const MAIN: &str = "main";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WorkspaceInfo {
    pub name: String,
    pub is_main: bool,
    /// Directory holding the workspace's files.
    pub dir: PathBuf,
    /// Directory holding meta/index/locks.
    pub meta_dir: PathBuf,
    /// Ref tracking this workspace's position.
    pub ref_name: String,
    pub base_oid: Option<ObjectId>,
    pub created_ms: i64,
    pub actor: Option<ObjectId>,
    /// Current position (ref value), None when unborn.
    pub head_oid: Option<ObjectId>,
}

pub fn check_workspace_name(name: &str) -> Result<()> {
    check_workspace_segment(name).map_err(|e| match e {
        Error::InvalidRef(m) => Error::Invalid(format!("workspace name: {m}")),
        other => other,
    })
}

fn meta_dir_for(repo: &Repo, name: &str) -> PathBuf {
    repo.ng().join("workspaces").join(name)
}

fn ref_name_for(repo: &Repo, name: &str) -> Result<String> {
    if name == MAIN {
        match repo.read_head()? {
            crate::repo::Head::Symbolic(r) => Ok(r),
            crate::repo::Head::Detached(_) => Err(Error::Invalid(
                "HEAD is detached; the main workspace needs a symbolic HEAD ref. \
                 Run `newgit workspace switch <ref>` or create a named workspace."
                    .into(),
            )),
        }
    } else {
        Ok(format!("workspaces/{name}"))
    }
}

fn dir_for(repo: &Repo, name: &str) -> PathBuf {
    if name == MAIN {
        repo.root().to_path_buf()
    } else {
        meta_dir_for(repo, name).join("files")
    }
}

pub fn meta_path(repo: &Repo, name: &str) -> PathBuf {
    if name == MAIN {
        meta_dir_for(repo, MAIN).join("meta")
    } else {
        meta_dir_for(repo, name).join("meta")
    }
}

pub fn index_path(repo: &Repo, name: &str) -> PathBuf {
    meta_dir_for(repo, name).join("index")
}

/// Acquire the per-workspace operation lock.
pub fn lock(repo: &Repo, name: &str) -> Result<fsx::FileLock> {
    let md = meta_dir_for(repo, name);
    fsx::ensure_dir(&md)?;
    fsx::FileLock::acquire(
        &md.join("OP"),
        Duration::from_millis(repo.limits().lock_wait_ms),
    )
}

fn parse_meta(text: &str) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| Error::Malformed(format!("bad workspace meta line: {line:?}")))?;
        m.insert(k.trim().to_string(), v.trim().to_string());
    }
    Ok(m)
}

pub fn info(repo: &Repo, name: &str) -> Result<WorkspaceInfo> {
    check_workspace_name(name)?;
    let md = meta_dir_for(repo, name);
    let is_main = name == MAIN;
    let mut base_oid = None;
    let mut created_ms = 0i64;
    let mut actor = None;
    let mp = md.join("meta");
    if mp.exists() {
        let text = fsx::read_limited(&mp, 1 << 20)?;
        let text = String::from_utf8(text)
            .map_err(|_| Error::Malformed("workspace meta not utf-8".into()))?;
        let m = parse_meta(&text)?;
        if let Some(v) = m.get("base_oid") {
            if !v.is_empty() {
                base_oid = Some(ObjectId::from_hex(v)?);
            }
        }
        if let Some(v) = m.get("created_ms") {
            created_ms = v
                .parse()
                .map_err(|_| Error::Malformed("bad created_ms in workspace meta".into()))?;
        }
        if let Some(v) = m.get("actor") {
            if !v.is_empty() {
                actor = Some(ObjectId::from_hex(v)?);
            }
        }
    } else if !is_main {
        return Err(Error::Invalid(format!("workspace {name:?} does not exist")));
    }
    let ref_name = ref_name_for(repo, name)?;
    let head_oid = repo.refs.read_opt(&ref_name)?;
    Ok(WorkspaceInfo {
        name: name.to_string(),
        is_main,
        dir: dir_for(repo, name),
        meta_dir: md,
        ref_name,
        base_oid,
        created_ms,
        actor,
        head_oid,
    })
}

pub fn list(repo: &Repo) -> Result<Vec<WorkspaceInfo>> {
    let mut out = vec![info(repo, MAIN)?];
    let dir = repo.ng().join("workspaces");
    if dir.exists() {
        for e in std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))? {
            let e = e.map_err(|err| Error::io(&dir, err))?;
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name == MAIN || check_workspace_name(&name).is_err() {
                continue;
            }
            if !e.path().join("meta").exists() {
                continue; // orphan debris; verify/discard sweeps
            }
            out.push(info(repo, &name)?);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Snapshot of the workspace's current position (None when unborn).
pub fn head_snapshot(repo: &Repo, name: &str) -> Result<Option<(ObjectId, Snapshot)>> {
    let ws = info(repo, name)?;
    match ws.head_oid {
        Some(oid) => {
            let obj = repo.objects.get(&oid)?;
            Ok(Some((oid, obj.as_snapshot()?.clone())))
        }
        None => Ok(None),
    }
}

/// Create a workspace from `base` (defaults to HEAD).
pub fn create(
    repo: &Repo,
    name: &str,
    base: Option<ObjectId>,
    actor: ObjectId,
) -> Result<WorkspaceInfo> {
    check_workspace_name(name)?;
    if name == MAIN {
        return Err(Error::Invalid(
            "workspace 'main' always exists (it is the repository root)".into(),
        ));
    }
    let md = meta_dir_for(repo, name);
    if md.join("meta").exists() {
        return Err(Error::Invalid(format!("workspace {name:?} already exists")));
    }
    let base = match base {
        Some(b) => Some(b),
        None => repo.resolve_head()?,
    };
    // Validate base is a snapshot before doing any work.
    let base_root = match base {
        Some(b) => {
            let obj = repo.objects.get(&b)?;
            Some(obj.as_snapshot()?.clone())
        }
        None => None,
    };

    fsx::ensure_dir(&md)?;
    let _lock = lock(repo, name)?;
    if md.join("meta").exists() {
        return Err(Error::Invalid(format!("workspace {name:?} already exists")));
    }

    // 1. Materialize files (crash here ⇒ orphan dir, no ref/meta: harmless).
    let files_dir = md.join("files");
    fsx::ensure_dir(&files_dir)?;
    let mut index = Index::new();
    if let Some(snap) = &base_root {
        crate::ops::checkout::checkout_tree(
            repo,
            snap.root,
            &files_dir,
            crate::ops::checkout::CheckoutMode::FreshWorkspace,
            Some(&mut index),
        )?;
    }
    index.save(&index_path(repo, name))?;

    // 2. One transaction: create ref + meta (all-or-nothing).
    let created_ms = txn::now_ms();
    let meta = format!(
        "# newgit workspace meta v1\nname = {name}\nbase_oid = {}\ncreated_ms = {created_ms}\nactor = {}\n",
        base.map(|b| b.to_hex()).unwrap_or_default(),
        actor.to_hex(),
    );
    let mut ops = vec![TxnOp::File {
        rel: format!("workspaces/{name}/meta"),
        data: meta.into_bytes(),
    }];
    if let Some(b) = base {
        ops.push(TxnOp::Ref {
            name: format!("workspaces/{name}"),
            cas: Cas::Exactly(None),
            new: Some(b),
            log: RefLogEntry {
                actor: Some(actor),
                ts_ms: created_ms,
                message: format!("workspace create from {}", b.short()),
            },
        });
    }
    txn::execute(repo.ng(), ops, repo.limits())?;
    info(repo, name)
}

/// Discard a workspace: deletes its ref, meta, index, and files.
/// Refuses when the workspace has unsnapshotted changes unless `force`.
pub fn discard(repo: &Repo, name: &str, force: bool) -> Result<()> {
    check_workspace_name(name)?;
    if name == MAIN {
        return Err(Error::Invalid("cannot discard the main workspace".into()));
    }
    let md = meta_dir_for(repo, name);
    if !md.join("meta").exists() {
        return Err(Error::Invalid(format!("workspace {name:?} does not exist")));
    }
    let _lock = lock(repo, name)?;
    if !force {
        let status = crate::ops::status::status(repo, name, 1)?;
        if !status.clean {
            return Err(Error::Conflict(format!(
                "workspace {name:?} has unsnapshotted changes ({} added, {} modified, {} deleted); re-run with --force to discard anyway",
                status.added.len(),
                status.modified.len(),
                status.deleted.len()
            )));
        }
    }
    // Journal: delete ref + meta together; then remove files best-effort.
    let ops = vec![
        TxnOp::Ref {
            name: format!("workspaces/{name}"),
            cas: Cas::Any,
            new: None,
            log: RefLogEntry::system(format!("workspace discard {name}")),
        },
        TxnOp::FileDelete {
            rel: format!("workspaces/{name}/meta"),
        },
    ];
    txn::execute(repo.ng(), ops, repo.limits())?;
    // Files/index are caches+materialization; safe to delete after the txn.
    let _ = std::fs::remove_dir_all(md);
    Ok(())
}

/// Resolve a base argument: ref name, snapshot id (full/prefix), or None.
pub fn resolve_base(repo: &Repo, spec: Option<&str>) -> Result<Option<ObjectId>> {
    match spec {
        None => Ok(repo.resolve_head()?),
        Some(s) => {
            if s.len() == ObjectId::HEX_LEN
                || (s.len() >= 4 && s.chars().all(|c| c.is_ascii_hexdigit()))
            {
                if let Ok(id) = ObjectId::from_hex(s) {
                    return Ok(Some(id));
                }
                return Ok(Some(repo.objects.resolve_prefix(s)?));
            }
            // workspace position shorthand: "ws:<name>"
            if let Some(ws) = s.strip_prefix("ws:") {
                let i = info(repo, ws)?;
                return Ok(i.head_oid);
            }
            match repo.refs.read_opt(s)? {
                Some(oid) => Ok(Some(oid)),
                None => Err(Error::RefNotFound(s.to_string())),
            }
        }
    }
}

/// Snapshot the workspace's HEAD position → used by rollback (iteration 5).
pub fn position(repo: &Repo, name: &str) -> Result<Option<ObjectId>> {
    Ok(info(repo, name)?.head_oid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_names() {
        assert!(check_workspace_name("ws-agent-1").is_ok());
        assert!(check_workspace_name("a.b_c-d").is_ok());
        for bad in ["", ".", "..", "a/b", "a b", "a~", "x.lock", "end."] {
            assert!(check_workspace_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn main_workspace_info() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("r")).unwrap();
        let repo = Repo::init(&dir.path().join("r")).unwrap();
        let i = info(&repo, MAIN).unwrap();
        assert!(i.is_main);
        assert_eq!(i.dir, repo.root());
        assert_eq!(i.ref_name, "refs/main");
        assert_eq!(i.head_oid, None);
    }
}
