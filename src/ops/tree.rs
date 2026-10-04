//! Tree construction and flattening.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use crate::error::{Error, Result};
use crate::object::types::{EntryMode, Object, Tree, TreeEntry};
use crate::object::ObjectId;
use crate::repo::Repo;

/// Build (and store) the tree hierarchy for sorted flat entries.
/// `items`: (relative '/'-separated path, content oid, leaf mode).
/// Returns the root tree oid. Empty input ⇒ empty tree.
pub fn build_tree(repo: &Repo, items: &[(String, ObjectId, EntryMode)]) -> Result<ObjectId> {
    // group by parent directory
    let mut by_dir: BTreeMap<String, Vec<TreeEntry>> = BTreeMap::new();
    for (rel, oid, mode) in items {
        if mode.is_tree() {
            return Err(Error::Bug("build_tree called with a tree-mode leaf".into()));
        }
        let (parent, name) = match rel.rsplit_once('/') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (String::new(), rel.clone()),
        };
        by_dir.entry(parent).or_default().push(TreeEntry {
            name,
            mode: *mode,
            oid: *oid,
        });
    }
    if by_dir.is_empty() {
        let t = Tree::empty();
        return repo.objects.put(&Object::Tree(t));
    }
    // process deepest directories first so children oids exist
    let mut dirs: Vec<String> = by_dir.keys().cloned().collect();
    dirs.sort_by(|a, b| {
        let da = a.matches('/').count();
        let db = b.matches('/').count();
        db.cmp(&da)
            .then_with(|| b.len().cmp(&a.len()))
            .then_with(|| a.cmp(b))
    });
    // ensure implicit parent dirs exist in the map
    for d in dirs.clone() {
        let mut cur = d.clone();
        while let Some((parent, seg)) = split_dir(&cur) {
            let entry_name = seg;
            if !by_dir.contains_key(&parent) {
                by_dir.insert(parent.clone(), Vec::new());
                dirs.push(parent.clone());
            }
            // register a placeholder; real oid patched after child is built
            let _ = entry_name;
            cur = parent;
        }
    }
    dirs.sort_by(|a, b| {
        let da = a.matches('/').count();
        let db = b.matches('/').count();
        db.cmp(&da)
            .then_with(|| b.len().cmp(&a.len()))
            .then_with(|| a.cmp(b))
    });
    dirs.dedup();

    let mut built: BTreeMap<String, ObjectId> = BTreeMap::new();
    for d in &dirs {
        let mut entries = by_dir.get(d).cloned().unwrap_or_default();
        // attach child directories
        let prefix = if d.is_empty() {
            String::new()
        } else {
            format!("{d}/")
        };
        for (child, oid) in &built {
            if let Some(rest) = child.strip_prefix(&prefix) {
                if !rest.contains('/') && !child.is_empty() && !rest.is_empty() {
                    entries.push(TreeEntry {
                        name: rest.to_string(),
                        mode: EntryMode::Tree,
                        oid: *oid,
                    });
                }
            }
        }
        let tree = Tree::new(entries)?;
        let oid = repo.objects.put(&Object::Tree(tree))?;
        built.insert(d.clone(), oid);
    }
    built
        .get("")
        .copied()
        .ok_or_else(|| Error::Bug("root tree missing after build".into()))
}

fn split_dir(d: &str) -> Option<(String, String)> {
    match d.rsplit_once('/') {
        Some((p, s)) => Some((p.to_string(), s.to_string())),
        None => {
            if d.is_empty() {
                None
            } else {
                Some((String::new(), d.to_string()))
            }
        }
    }
}

/// Flatten a tree into sorted (path, mode, oid) leaf entries.
/// Cycle-safe: trees are content-addressed, but a hostile store could contain
/// a self-referencing tree; we track the oid stack and error on cycles.
pub fn flatten_tree(repo: &Repo, root: ObjectId) -> Result<Vec<(String, EntryMode, ObjectId)>> {
    flatten_tree_with_deadline(repo, root, None)
}

/// Flatten a tree while honoring an optional absolute deadline used by the
/// Git smart-HTTP request path.
pub(crate) fn flatten_tree_with_deadline(
    repo: &Repo,
    root: ObjectId,
    deadline: Option<Instant>,
) -> Result<Vec<(String, EntryMode, ObjectId)>> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, ObjectId, Vec<ObjectId>)> = vec![(String::new(), root, Vec::new())];
    let mut depth_guard = 0usize;
    while let Some((prefix, oid, ancestors)) = stack.pop() {
        check_tree_deadline(deadline)?;
        depth_guard += 1;
        if depth_guard > 10_000_000 {
            return Err(Error::Limit("flatten_tree visited too many trees".into()));
        }
        let obj = repo.objects.get(&oid)?;
        let tree = obj.as_tree().map_err(|e| Error::Corrupt {
            oid,
            reason: format!("expected tree in hierarchy: {e}"),
        })?;
        if ancestors.contains(&oid) {
            return Err(Error::Corrupt {
                oid,
                reason: "tree cycle detected".into(),
            });
        }
        let mut child_ancestors = ancestors;
        child_ancestors.push(oid);
        if child_ancestors.len() > 4096 {
            return Err(Error::Limit("tree nesting too deep".into()));
        }
        for (index, e) in tree.entries.iter().rev().enumerate() {
            if index % 1024 == 0 {
                check_tree_deadline(deadline)?;
            }
            let path = if prefix.is_empty() {
                e.name.clone()
            } else {
                format!("{prefix}/{}", e.name)
            };
            if e.mode.is_tree() {
                stack.push((path, e.oid, child_ancestors.clone()));
            } else {
                out.push((path, e.mode, e.oid));
            }
        }
    }
    check_tree_deadline(deadline)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    check_tree_deadline(deadline)?;
    Ok(out)
}

fn check_tree_deadline(deadline: Option<Instant>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(Error::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Git smart-HTTP tree traversal exceeded its operation deadline",
        )));
    }
    Ok(())
}

/// Detect a tree cycle explicitly (used by verify).
pub fn check_tree_acyclic(repo: &Repo, root: ObjectId) -> Result<()> {
    let mut visited_path: HashSet<ObjectId> = HashSet::new();
    fn rec(
        repo: &Repo,
        oid: ObjectId,
        visited_path: &mut HashSet<ObjectId>,
        depth: usize,
    ) -> Result<()> {
        if depth > 4096 {
            return Err(Error::Limit("tree nesting too deep".into()));
        }
        if !visited_path.insert(oid) {
            return Err(Error::Corrupt {
                oid,
                reason: "tree cycle detected".into(),
            });
        }
        let obj = repo.objects.get(&oid)?;
        let tree = obj.as_tree()?;
        for e in &tree.entries {
            if e.mode.is_tree() {
                rec(repo, e.oid, visited_path, depth + 1)?;
            }
        }
        visited_path.remove(&oid);
        Ok(())
    }
    rec(repo, root, &mut visited_path, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::types::Object;

    fn repo() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path().join("r");
        std::fs::create_dir(&r).unwrap();
        let repo = Repo::init(&r).unwrap();
        (dir, repo)
    }

    fn blob(repo: &Repo, s: &str) -> ObjectId {
        repo.objects
            .put(&Object::Blob(s.as_bytes().to_vec()))
            .unwrap()
    }

    #[test]
    fn build_and_flatten() {
        let (_d, repo) = repo();
        let items = vec![
            ("a.txt".to_string(), blob(&repo, "a"), EntryMode::File),
            ("dir/b.txt".to_string(), blob(&repo, "b"), EntryMode::File),
            (
                "dir/sub/c.sh".to_string(),
                blob(&repo, "c"),
                EntryMode::Executable,
            ),
            ("zz".to_string(), blob(&repo, "z"), EntryMode::Symlink),
        ];
        let root = build_tree(&repo, &items).unwrap();
        let flat = flatten_tree(&repo, root).unwrap();
        let mut expect: Vec<(String, EntryMode, ObjectId)> =
            items.iter().map(|(p, o, m)| (p.clone(), *m, *o)).collect();
        expect.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(flat, expect);
        // deterministic: rebuilding yields the same root
        let root2 = build_tree(&repo, &items).unwrap();
        assert_eq!(root, root2);
        // empty tree
        let e = build_tree(&repo, &[]).unwrap();
        assert!(flatten_tree(&repo, e).unwrap().is_empty());
    }

    #[test]
    fn flatten_rejects_blob_root() {
        let (_d, repo) = repo();
        let b = blob(&repo, "x");
        assert!(flatten_tree(&repo, b).is_err());
    }

    #[test]
    fn cycle_detection() {
        // craft a tree that references itself by storing it under a forged
        // path (bypasses put's id check to simulate a hostile store)
        let (_d, repo) = repo();
        let self_ref = ObjectId::from_bytes([0xaa; 32]);
        let t = Tree::new(vec![TreeEntry {
            name: "loop".into(),
            mode: EntryMode::Tree,
            oid: self_ref,
        }])
        .unwrap();
        // store the crafted tree *at* the self_ref path: envelope digest must
        // match the path id, which is impossible for a true cycle — so the
        // realistic hostile case is a↔b mutual references, also impossible to
        // forge with valid digests. What we CAN test: missing child +
        // deep-nesting guard behavior.
        let _ = t;
        // missing child → error, not hang
        let fake = self_ref;
        let r = flatten_tree(&repo, fake);
        assert!(matches!(r, Err(Error::NotFound(_))));
    }
}
