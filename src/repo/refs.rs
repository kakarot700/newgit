//! Named references to snapshots (and registry pointers for goals/changes/
//! proposals). Ref files are `refs/<name>` containing 64-hex + newline.
//! All mutations go through the transaction engine (CAS + reflog + atomicity).

use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::object::ObjectId;
use crate::repo::config::Limits;
use crate::repo::txn::{self, Cas, RecoveryReport, RefLogEntry, ReflogLine, TxnOp, TxnReport};

/// Validate a user-facing ref name (STORAGE_FORMAT.md §5).
pub fn check_ref_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidRef("empty ref name".into()));
    }
    if name.len() > 255 {
        return Err(Error::InvalidRef(format!("ref name too long: {name:?}")));
    }
    if name.as_bytes().contains(&0) {
        return Err(Error::InvalidRef("ref name contains NUL".into()));
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Err(Error::InvalidRef(format!(
            "ref name has empty segment or slash at edge: {name:?}"
        )));
    }
    if name.contains("@{") {
        return Err(Error::InvalidRef(format!(
            "ref name contains '@{{': {name:?}"
        )));
    }
    for c in ['~', '^', ':', '?', '*', '[', '\\'] {
        if name.contains(c) {
            return Err(Error::InvalidRef(format!(
                "ref name contains forbidden character {c:?}: {name:?}"
            )));
        }
    }
    // reserved system namespaces
    for reserved in ["logs", "txn", "workspaces", "meta"] {
        if name == reserved || name.starts_with(&format!("{reserved}/")) {
            return Err(Error::InvalidRef(format!(
                "ref namespace {reserved:?} is reserved: {name:?}"
            )));
        }
    }
    for seg in name.split('/') {
        if seg.is_empty() {
            return Err(Error::InvalidRef(format!("empty segment in {name:?}")));
        }
        if seg == "." || seg == ".." {
            return Err(Error::InvalidRef(format!("illegal segment in {name:?}")));
        }
        if seg.ends_with(".lock") {
            return Err(Error::InvalidRef(format!(
                "segment ends with .lock in {name:?}"
            )));
        }
        if seg.ends_with('.') {
            return Err(Error::InvalidRef(format!(
                "segment ends with '.' in {name:?}"
            )));
        }
        for b in seg.bytes() {
            let ok = b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-');
            if !ok {
                return Err(Error::InvalidRef(format!(
                    "segment contains forbidden byte {:?} in {name:?}",
                    b as char
                )));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct RefStore {
    ng: PathBuf,
    limits: Limits,
}

impl RefStore {
    pub fn new(ng: PathBuf, limits: Limits) -> RefStore {
        RefStore { ng, limits }
    }

    pub fn read(&self, name: &str) -> Result<ObjectId> {
        txn::read_ref_raw(&self.ng, name)?.ok_or_else(|| Error::RefNotFound(name.to_string()))
    }

    pub fn read_opt(&self, name: &str) -> Result<Option<ObjectId>> {
        txn::read_ref_raw(&self.ng, name)
    }

    /// CAS update. `new = None` deletes the ref.
    pub fn update(
        &self,
        name: &str,
        cas: Cas,
        new: Option<ObjectId>,
        log: RefLogEntry,
    ) -> Result<TxnReport> {
        txn::execute(
            &self.ng,
            vec![TxnOp::Ref {
                name: name.to_string(),
                cas,
                new,
                log,
            }],
            &self.limits,
        )
    }

    /// List refs (optionally under a prefix like "goals/"), sorted by name.
    pub fn list(&self, prefix: Option<&str>) -> Result<Vec<(String, ObjectId)>> {
        let dir = self.ng.join("refs");
        let mut out = Vec::new();
        walk_refs(&dir, &dir, prefix, &mut out)?;
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    pub fn reflog(&self, name: &str) -> Result<Vec<ReflogLine>> {
        txn::read_reflog(&self.ng, name)
    }

    pub fn recover(&self) -> Result<RecoveryReport> {
        txn::recover(&self.ng, &self.limits)
    }
}

fn walk_refs(
    base: &std::path::Path,
    dir: &std::path::Path,
    prefix: Option<&str>,
    out: &mut Vec<(String, ObjectId)>,
) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for e in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let e = e.map_err(|err| Error::io(dir, err))?;
        let path = e.path();
        let rel = path
            .strip_prefix(base)
            .map_err(|_| Error::Bug("ref walk prefix mismatch".into()))?
            .to_string_lossy()
            .to_string();
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            // Only descend when the directory can contain the prefix.
            if let Some(p) = prefix {
                let could_match = rel == p
                    || rel.starts_with(&format!("{p}/"))
                    || p.starts_with(&format!("{rel}/"));
                if !could_match {
                    continue;
                }
            }
            walk_refs(base, &path, prefix, out)?;
        } else {
            if let Some(p) = prefix {
                if !(rel == p || rel.starts_with(&format!("{p}/"))) {
                    continue;
                }
            }
            if rel.ends_with(".lock") || rel.contains(".tmp.") {
                continue;
            }
            if check_ref_name(&rel).is_err() {
                // foreign debris in refs/ — surface via verify, skip here
                continue;
            }
            match txn::read_ref_raw(base.parent().unwrap_or(base), &rel) {
                Ok(Some(oid)) => out.push((rel, oid)),
                // malformed ref files: report through verify; skip in listings
                _ => continue,
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names() {
        for n in [
            "main",
            "feature/oauth",
            "goals/g-123",
            "changes/c-1",
            "a.b-c_d/e",
            "remotes/origin/main",
        ] {
            assert!(check_ref_name(n).is_ok(), "{n} should be valid");
        }
    }

    #[test]
    fn invalid_names() {
        for n in [
            "",
            "/main",
            "main/",
            "a//b",
            ".",
            "..",
            "a/..",
            "a/.",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
            "a@{b",
            "main.lock",
            "a/b.lock",
            "logs/x",
            "txn/x",
            "workspaces/x",
            "meta/x",
            "a ",
            "a\tb",
            "main.",
            &"x".repeat(256),
        ] {
            assert!(check_ref_name(n).is_err(), "{n:?} should be invalid");
        }
    }
}
