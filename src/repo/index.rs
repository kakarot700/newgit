//! Workspace index — a *status cache* mapping path → (oid, size, mtime, mode).
//!
//! INVARIANT: the index is never authoritative. Deleting it must never lose
//! data or change semantics; `status`/`snapshot` rebuild what they need from
//! content hashes. Format "NGIX" v1 (STORAGE_FORMAT.md §7).

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{Error, Result};
use crate::object::types::EntryMode;
use crate::object::ObjectId;
use crate::util::fsx;
use crate::util::varint::{self, Reader};

pub const MAGIC: &[u8; 4] = b"NGIX";
pub const VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub oid: ObjectId,
    pub size: u64,
    pub mtime_sec: u64,
    pub mtime_nsec: u32,
    pub mode: EntryMode,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    entries: BTreeMap<String, IndexEntry>,
}

impl Index {
    pub fn new() -> Index {
        Index::default()
    }

    pub fn get(&self, path: &str) -> Option<&IndexEntry> {
        self.entries.get(path)
    }

    pub fn insert(&mut self, path: String, e: IndexEntry) {
        self.entries.insert(path, e);
    }

    pub fn remove(&mut self, path: &str) -> Option<IndexEntry> {
        self.entries.remove(path)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &IndexEntry)> {
        self.entries.iter()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        varint::write_u64(&mut out, self.entries.len() as u64);
        for (path, e) in &self.entries {
            varint::write_u64(&mut out, path.len() as u64);
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(e.oid.as_bytes());
            varint::write_u64(&mut out, e.size);
            varint::write_u64(&mut out, e.mtime_sec);
            varint::write_u64(&mut out, e.mtime_nsec as u64);
            out.push(e.mode as u8);
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Index> {
        let mut r = Reader::new(bytes);
        if bytes.len() < 5 {
            return Err(Error::Malformed("index too short".into()));
        }
        if &bytes[0..4] != MAGIC {
            return Err(Error::Malformed("bad index magic".into()));
        }
        r.read_bytes(4)?;
        let version = r.read_u8()?;
        if version != VERSION {
            return Err(Error::Malformed(format!(
                "unsupported index version {version}"
            )));
        }
        let n = r.read_u64()? as usize;
        if n > (1 << 22) {
            return Err(Error::Limit(format!("index entry count {n} too large")));
        }
        // min entry size = 1(len)+32(oid)+3 varints+1 mode ≥ 36
        if n.checked_mul(36)
            .map(|sz| sz > r.remaining())
            .unwrap_or(true)
        {
            return Err(Error::Malformed(
                "index entry count exceeds remaining input".into(),
            ));
        }
        let mut entries = BTreeMap::new();
        let mut last: Option<String> = None;
        for _ in 0..n {
            let plen = r.read_u64()? as usize;
            if plen == 0 || plen > 8192 {
                return Err(Error::Malformed(format!("bad index path length {plen}")));
            }
            let pbytes = r.read_bytes(plen)?;
            let path = std::str::from_utf8(pbytes)
                .map_err(|e| Error::Malformed(format!("index path not utf-8: {e}")))?;
            if let Some(l) = &last {
                if path <= l.as_str() {
                    return Err(Error::Malformed(
                        "index paths not strictly ascending".into(),
                    ));
                }
            }
            let oid = ObjectId::from_bytes(r.read_fixed::<32>()?);
            let size = r.read_u64()?;
            let mtime_sec = r.read_u64()?;
            let mtime_nsec = r.read_u64()?;
            if mtime_nsec >= 1_000_000_000 {
                return Err(Error::Malformed("index mtime_nsec out of range".into()));
            }
            let mode = EntryMode::from_u8(r.read_u8()?)?;
            if mode.is_tree() {
                return Err(Error::Malformed("index entry with tree mode".into()));
            }
            entries.insert(
                path.to_string(),
                IndexEntry {
                    oid,
                    size,
                    mtime_sec,
                    mtime_nsec: mtime_nsec as u32,
                    mode,
                },
            );
            last = Some(path.to_string());
        }
        if !r.is_empty() {
            return Err(Error::Malformed("trailing bytes in index".into()));
        }
        Ok(Index { entries })
    }

    /// Load from disk; a missing or *corrupt* index yields an empty index
    /// (it is a cache — rebuild is always safe; corruption is surfaced by
    /// `newgit verify`, not by failing status).
    pub fn load(path: &Path) -> Index {
        Index::load_with_mtime(path).0
    }

    /// Load the index together with the index file's own mtime — needed for
    /// the racily-clean guard (see `cache_trustworthy`).
    pub fn load_with_mtime(path: &Path) -> (Index, Option<(u64, u32)>) {
        let mtime = std::fs::metadata(path)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()));
        let idx = match std::fs::read(path) {
            Ok(bytes) => Index::decode(&bytes).unwrap_or_default(),
            Err(_) => Index::new(),
        };
        (idx, mtime)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fsx::atomic_write(path, &self.encode())
    }
}

/// Decide whether a cached entry may be trusted without re-reading content.
///
/// Git's "racily clean" rule: an entry whose recorded mtime is ≥ the index
/// file's own mtime is *suspect* — a modification in the same timestamp tick
/// is otherwise undetectable — so it must be re-hashed. `file` is
/// (mtime_sec, mtime_nsec, size) from the live walk.
pub fn cache_trustworthy(
    cached: &IndexEntry,
    file: (u64, u32, u64),
    idx_mtime: Option<(u64, u32)>,
) -> bool {
    if cached.size != file.2 || cached.mtime_sec != file.0 || cached.mtime_nsec != file.1 {
        return false;
    }
    match idx_mtime {
        Some((s, n)) => (file.0, file.1) < (s, n),
        None => false, // no index on disk ⇒ nothing is trustworthy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(b: u8) -> IndexEntry {
        IndexEntry {
            oid: ObjectId::from_bytes([b; 32]),
            size: 10,
            mtime_sec: 123,
            mtime_nsec: 456,
            mode: EntryMode::File,
        }
    }

    #[test]
    fn roundtrip() {
        let mut idx = Index::new();
        idx.insert("a.txt".into(), entry(1));
        idx.insert("sub/b.txt".into(), entry(2));
        let bytes = idx.encode();
        let back = Index::decode(&bytes).unwrap();
        assert_eq!(back, idx);
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn racily_clean_guard() {
        let e = IndexEntry {
            oid: ObjectId::from_bytes([1; 32]),
            size: 7,
            mtime_sec: 100,
            mtime_nsec: 0,
            mode: EntryMode::File,
        };
        // file mtime older than index mtime ⇒ trusted
        assert!(cache_trustworthy(&e, (100, 0, 7), Some((100, 500))));
        // equal timestamp ⇒ suspect (racily clean)
        assert!(!cache_trustworthy(&e, (100, 0, 7), Some((100, 0))));
        // newer than index ⇒ suspect
        assert!(!cache_trustworthy(&e, (101, 0, 7), Some((100, 0))));
        // size mismatch ⇒ never
        assert!(!cache_trustworthy(&e, (100, 0, 8), Some((200, 0))));
        // no index mtime ⇒ never
        assert!(!cache_trustworthy(&e, (100, 0, 7), None));
    }

    #[test]
    fn corrupt_index_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("index");
        std::fs::write(&p, b"garbage!!").unwrap();
        assert!(Index::load(&p).is_empty());
        assert!(p.exists()); // untouched
                             // missing file
        assert!(Index::load(&dir.path().join("nope")).is_empty());
    }

    #[test]
    fn decode_rejects_malformed() {
        let mut idx = Index::new();
        idx.insert("a".into(), entry(1));
        let mut bytes = idx.encode();
        // truncate
        assert!(Index::decode(&bytes[..bytes.len() - 3]).is_err());
        // craft an index with unsorted paths: count=2 then entries b,a
        let mut craft = Vec::new();
        craft.extend_from_slice(MAGIC);
        craft.push(VERSION);
        varint::write_u64(&mut craft, 2);
        for (p, b) in [("b", 1u8), ("a", 2u8)] {
            varint::write_u64(&mut craft, p.len() as u64);
            craft.extend_from_slice(p.as_bytes());
            craft.extend_from_slice(&[b; 32]);
            varint::write_u64(&mut craft, 0);
            varint::write_u64(&mut craft, 0);
            varint::write_u64(&mut craft, 0);
            craft.push(0);
        }
        assert!(Index::decode(&craft).is_err());
        // bad magic
        bytes[0] = b'X';
        assert!(Index::decode(&bytes).is_err());
    }
}
