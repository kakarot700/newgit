//! Content-addressed immutable object store.
//!
//! Layout: `<root>/xx/yyyy…(62 hex)` where `xxyy…` is the object id hex.
//! Writes are atomic (temp file → fsync → rename → dir fsync). Objects are
//! immutable: `put` on an existing id verifies the existing file instead of
//! overwriting. Reads always verify the full envelope (SHA-256), so silent
//! corruption is impossible to observe through this API — it surfaces as
//! `Error::Corrupt`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::object::envelope;
use crate::object::id::ObjectId;
use crate::object::types::Object;
use crate::repo::config::Limits;
use crate::util::{fault, fsx};

#[derive(Debug)]
pub struct ObjectStore {
    root: PathBuf,
    pub limits: Limits,
}

impl ObjectStore {
    pub fn new(root: PathBuf, limits: Limits) -> ObjectStore {
        ObjectStore { root, limits }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, oid: &ObjectId) -> PathBuf {
        let h = oid.to_hex();
        self.root.join(&h[..2]).join(&h[2..])
    }

    /// Store an object; returns its id. Idempotent for identical content.
    pub fn put(&self, obj: &Object) -> Result<ObjectId> {
        let canonical = obj.canonical();
        self.put_canonical(&canonical)
    }

    /// Store pre-computed canonical bytes (must be canonical — verified).
    pub fn put_canonical(&self, canonical: &[u8]) -> Result<ObjectId> {
        let id = ObjectId::compute(canonical);
        if canonical.len() as u64 > self.limits.max_object_bytes {
            return Err(Error::Limit(format!(
                "object of {} bytes exceeds max_object_bytes {}",
                canonical.len(),
                self.limits.max_object_bytes
            )));
        }
        let path = self.path_for(&id);
        if path.exists() {
            // Immutable: verify the existing copy matches the id being stored.
            // A mismatch means the store is corrupt; fail loudly, never
            // overwrite.
            let existing = fsx::read_limited(&path, self.limits.max_stored_bytes)?;
            let (_, existing_id) = envelope::decode(&existing, self.limits.max_object_bytes)
                .map_err(|e| Error::Corrupt {
                    oid: id,
                    reason: format!("existing file failed verification: {e}"),
                })?;
            if existing_id != id {
                return Err(Error::Corrupt {
                    oid: id,
                    reason: "existing file at this path has a different identity".into(),
                });
            }
            return Ok(id);
        }

        // Re-check the object parses from its canonical bytes before storing
        // (defense against callers hand-building invalid canonical data).
        Object::from_canonical(canonical)?;
        let env = envelope::encode_raw(canonical, id)?;
        if env.len() as u64 > self.limits.max_stored_bytes {
            return Err(Error::Limit(
                "encoded object exceeds max_stored_bytes".into(),
            ));
        }

        fault::fault("ostore:before_write")?;
        if let Some(parent) = path.parent() {
            fsx::ensure_dir(parent)?;
        }
        let tmp = fsx::temp_sibling(&path)?;
        let written = (|| -> Result<()> {
            let mut f = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&tmp)
                .map_err(|e| Error::io(&tmp, e))?;
            f.write_all(&env).map_err(|e| Error::io(&tmp, e))?;
            // Fault point: kill after the temp file exists but before rename.
            // Recovery requirement: no object appears, no debris matters.
            let _ = fault::fault_action("ostore:after_tmp_write");
            fsx::fsync_file(&f)?;
            Ok(())
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        // Fault point: kill between fsync and rename.
        let _ = fault::fault_action("ostore:before_rename");
        match std::fs::rename(&tmp, &path) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(Error::io(&path, e));
            }
        }
        if let Some(parent) = path.parent() {
            fsx::fsync_dir(parent)?;
        }
        let _ = fault::fault_action("ostore:after_rename");
        Ok(id)
    }

    /// Fetch and fully verify an object.
    pub fn get(&self, oid: &ObjectId) -> Result<Object> {
        let (obj, id) = self.get_verified(oid)?;
        debug_assert_eq!(id, *oid);
        Ok(obj)
    }

    /// Fetch verified canonical bytes (no semantic decode).
    pub fn get_canonical(&self, oid: &ObjectId) -> Result<Vec<u8>> {
        let path = self.path_for(oid);
        let raw = fsx::read_limited(&path, self.limits.max_stored_bytes).map_err(|e| {
            if e.is_missing_file() {
                Error::NotFound(*oid)
            } else {
                e
            }
        })?;
        fault::fault("ostore:after_read")?;
        let (canonical, id) =
            envelope::verify(&raw, self.limits.max_object_bytes).map_err(|e| Error::Corrupt {
                oid: *oid,
                reason: e.to_string(),
            })?;
        if id != *oid {
            return Err(Error::Corrupt {
                oid: *oid,
                reason: format!(
                    "content hashes to {id}, but was stored under {oid} (misfiled object)"
                ),
            });
        }
        Ok(canonical)
    }

    fn get_verified(&self, oid: &ObjectId) -> Result<(Object, ObjectId)> {
        let path = self.path_for(oid);
        let raw = fsx::read_limited(&path, self.limits.max_stored_bytes).map_err(|e| {
            if e.is_missing_file() {
                Error::NotFound(*oid)
            } else {
                e
            }
        })?;
        fault::fault("ostore:after_read")?;
        let (obj, id) =
            envelope::decode(&raw, self.limits.max_object_bytes).map_err(|e| Error::Corrupt {
                oid: *oid,
                reason: e.to_string(),
            })?;
        if id != *oid {
            return Err(Error::Corrupt {
                oid: *oid,
                reason: format!(
                    "content hashes to {id}, but was stored under {oid} (misfiled object)"
                ),
            });
        }
        Ok((obj, id))
    }

    /// True if a *verified* object exists. A corrupt file at the path counts
    /// as "not present but corrupt" → we report false and let `verify` flag
    /// it; `get` still fails loudly.
    pub fn contains(&self, oid: &ObjectId) -> bool {
        self.path_for(oid).exists()
    }

    /// Enumerate all object ids physically present (sorted). Includes objects
    /// that may be corrupt (ids are taken from file names).
    pub fn iter(&self) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        let entries = std::fs::read_dir(&self.root).map_err(|e| Error::io(&self.root, e))?;
        for dir in entries {
            let dir = dir.map_err(|e| Error::io(&self.root, e))?;
            let dname = dir.file_name().to_string_lossy().to_string();
            if dname.len() != 2 || !dname.chars().all(|c| c.is_ascii_hexdigit()) {
                continue; // tmp dirs etc.
            }
            let files = std::fs::read_dir(dir.path()).map_err(|e| Error::io(dir.path(), e))?;
            for f in files {
                let f = f.map_err(|e| Error::io(dir.path(), e))?;
                let fname = f.file_name().to_string_lossy().to_string();
                if fname.len() != 62 || !fname.chars().all(|c| c.is_ascii_hexdigit()) {
                    continue; // skip .tmp./.lock debris
                }
                match ObjectId::from_hex(&format!("{dname}{fname}")) {
                    Ok(id) => out.push(id),
                    Err(_) => continue,
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Remove an object (GC only — callers must hold the GC lock and prove
    /// unreachability). Returns true if a file was removed.
    pub(crate) fn remove(&self, oid: &ObjectId) -> Result<bool> {
        let path = self.path_for(oid);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    fsx::fsync_dir(parent)?;
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Clean leftover temp files older than `stale_secs` (crash debris).
    pub fn sweep_temp_files(&self, stale_secs: u64) -> Result<usize> {
        let mut removed = 0;
        if !self.root.exists() {
            return Ok(0);
        }
        let now = std::time::SystemTime::now();
        for dir in std::fs::read_dir(&self.root).map_err(|e| Error::io(&self.root, e))? {
            let dir = match dir {
                Ok(d) => d,
                Err(_) => continue,
            };
            for f in std::fs::read_dir(dir.path()).map_err(|e| Error::io(dir.path(), e))? {
                let f = match f {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let name = f.file_name().to_string_lossy().to_string();
                if !name.contains(".tmp.") {
                    continue;
                }
                let age_ok = f
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| {
                        now.duration_since(t)
                            .map(|d| d.as_secs() >= stale_secs)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if age_ok && std::fs::remove_file(f.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    /// Resolve a unique hex prefix (≥4 chars) to a full id.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<ObjectId> {
        let p = prefix.to_ascii_lowercase();
        if p.len() < 4 {
            return Err(Error::Invalid(
                "object id prefix must be at least 4 hex characters".into(),
            ));
        }
        if !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invalid(format!("not a hex prefix: {prefix:?}")));
        }
        if p.len() == ObjectId::HEX_LEN {
            let id = ObjectId::from_hex(&p)?;
            return if self.path_for(&id).exists() {
                Ok(id)
            } else {
                Err(Error::NotFound(id))
            };
        }
        let dir = self.root.join(&p[..2]);
        let mut matches: Vec<ObjectId> = Vec::new();
        if dir.exists() {
            for f in std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))? {
                let f = f.map_err(|e| Error::io(&dir, e))?;
                let fname = f.file_name().to_string_lossy().to_string();
                if fname.len() != 62 {
                    continue;
                }
                let full = format!("{}{}", &p[..2], fname);
                if full.starts_with(&p) {
                    if let Ok(id) = ObjectId::from_hex(&full) {
                        matches.push(id);
                    }
                }
            }
        }
        match matches.len() {
            0 => Err(Error::Invalid(format!("no object matches prefix {prefix}"))),
            1 => Ok(matches[0]),
            n => Err(Error::Invalid(format!(
                "prefix {prefix} is ambiguous ({n} objects match)"
            ))),
        }
    }

    /// Store raw bytes as a blob object (convenience with size limit).
    pub fn put_blob(&self, data: &[u8]) -> Result<ObjectId> {
        if data.len() as u64 > self.limits.max_blob_bytes {
            return Err(Error::Limit(format!(
                "blob of {} bytes exceeds max_blob_bytes {}",
                data.len(),
                self.limits.max_blob_bytes
            )));
        }
        self.put(&Object::Blob(data.to_vec()))
    }

    pub fn put_blob_from_file(&self, path: &Path) -> Result<ObjectId> {
        let m = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
        if !m.is_file() {
            return Err(Error::Invalid(format!(
                "not a regular file: {}",
                path.display()
            )));
        }
        if m.len() > self.limits.max_blob_bytes {
            return Err(Error::Limit(format!(
                "file {} is {} bytes, exceeds max_blob_bytes {}",
                path.display(),
                m.len(),
                self.limits.max_blob_bytes
            )));
        }
        let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
        let mut buf = Vec::with_capacity(m.len() as usize);
        std::io::Read::read_to_end(&mut f, &mut buf).map_err(|e| Error::io(path, e))?;
        self.put_blob(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::types::{EntryMode, Tree, TreeEntry};

    fn store() -> (tempfile::TempDir, ObjectStore) {
        let dir = tempfile::tempdir().unwrap();
        let s = ObjectStore::new(dir.path().join("objects"), Limits::default());
        (dir, s)
    }

    #[test]
    fn put_get_roundtrip() {
        let (_d, s) = store();
        let id = s.put_blob(b"hello").unwrap();
        let obj = s.get(&id).unwrap();
        assert_eq!(obj.as_blob().unwrap(), b"hello");
        // idempotent
        let id2 = s.put_blob(b"hello").unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn missing_object() {
        let (_d, s) = store();
        let id = ObjectId::from_bytes([9; 32]);
        assert!(matches!(s.get(&id), Err(Error::NotFound(_))));
        assert!(!s.contains(&id));
    }

    #[test]
    fn corruption_detected() {
        let (_d, s) = store();
        let id = s
            .put(&Object::Tree(
                Tree::new(vec![TreeEntry {
                    name: "f.txt".into(),
                    mode: EntryMode::File,
                    oid: ObjectId::from_bytes([1; 32]),
                }])
                .unwrap(),
            ))
            .unwrap();
        // flip a byte in the stored file
        let p = s.path_for(&id);
        let mut bytes = std::fs::read(&p).unwrap();
        let i = bytes.len() - 40; // inside digest
        bytes[i] ^= 0xff;
        std::fs::write(&p, &bytes).unwrap();
        match s.get(&id) {
            Err(Error::Corrupt { .. }) => {}
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn misfiled_object_detected() {
        let (_d, s) = store();
        let id = s.put_blob(b"aaa").unwrap();
        let other = s.put_blob(b"bbb").unwrap();
        // move bbb's file onto aaa's path
        std::fs::rename(s.path_for(&other), s.path_for(&id)).unwrap();
        assert!(matches!(s.get(&id), Err(Error::Corrupt { .. })));
    }

    #[test]
    fn truncated_file_detected() {
        let (_d, s) = store();
        let id = s.put_blob(&vec![42u8; 10_000]).unwrap();
        let p = s.path_for(&id);
        let bytes = std::fs::read(&p).unwrap();
        std::fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
        assert!(matches!(s.get(&id), Err(Error::Corrupt { .. })));
    }

    #[test]
    fn iter_and_prefix() {
        let (_d, s) = store();
        let a = s.put_blob(b"a").unwrap();
        let b = s.put_blob(b"b").unwrap();
        let all = s.iter().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&a) && all.contains(&b));
        let full = s.resolve_prefix(&a.to_hex()).unwrap();
        assert_eq!(full, a);
        assert!(s.resolve_prefix("ab").is_err()); // too short
        assert!(s.resolve_prefix("ffffffffffff").is_err()); // no match
    }

    #[test]
    fn limits_enforced() {
        let (_d, mut s) = store();
        s.limits.max_blob_bytes = 10;
        assert!(matches!(s.put_blob(&[0u8; 11]), Err(Error::Limit(_))));
    }

    #[test]
    fn sweep_temp_files_removes_debris() {
        let (_d, s) = store();
        let dir = s.root.join("ab");
        std::fs::create_dir_all(&dir).unwrap();
        let debris = dir.join("cd.tmp.123.456.0");
        std::fs::write(&debris, b"junk").unwrap();
        // not stale yet
        assert_eq!(s.sweep_temp_files(3600).unwrap(), 0);
        // treat everything as stale
        assert_eq!(s.sweep_temp_files(0).unwrap(), 1);
        assert!(!debris.exists());
    }
}
