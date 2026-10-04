//! Ancestry queries: `is_ancestor` and `merge_base` (LCA).
//!
//! Determinism: BFS/priority traversal with visited sets and (timestamp DESC,
//! oid DESC) tiebreaks. Criss-cross histories can have multiple merge bases;
//! v1 picks the single maximal one by (timestamp, oid) and documents this
//! (KNOWN_LIMITATIONS #15) — git's "recursive" strategy would merge the
//! bases first. Traversal is bounded (`max_visit`) so hostile histories
//! cannot exhaust memory.

use std::collections::{BinaryHeap, HashSet};

use crate::error::{Error, Result};
use crate::object::ObjectId;
use crate::repo::Repo;

const MAX_VISIT: usize = 1_000_000;

fn parents_of(repo: &Repo, oid: ObjectId) -> Result<(i64, Vec<ObjectId>)> {
    let obj = repo.objects.get(&oid)?;
    let snap = obj.as_snapshot().map_err(|e| Error::Corrupt {
        oid,
        reason: format!("history edge into non-snapshot: {e}"),
    })?;
    Ok((snap.timestamp_ms, snap.parents.clone()))
}

/// Inclusive ancestor test: true when `a == b` or `a` is reachable from `b`
/// by following parents.
pub fn is_ancestor(repo: &Repo, a: ObjectId, b: ObjectId) -> Result<bool> {
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let mut stack = vec![b];
    while let Some(cur) = stack.pop() {
        if cur == a {
            return Ok(true);
        }
        if !visited.insert(cur) {
            continue;
        }
        if visited.len() > MAX_VISIT {
            return Err(Error::Limit(
                "history traversal exceeded bound in is_ancestor".into(),
            ));
        }
        let (_, parents) = parents_of(repo, cur)?;
        stack.extend(parents);
    }
    Ok(false)
}

/// Collect all ancestors of `oid` (inclusive), bounded.
fn ancestors(repo: &Repo, oid: ObjectId) -> Result<HashSet<ObjectId>> {
    let mut set: HashSet<ObjectId> = HashSet::new();
    let mut stack = vec![oid];
    while let Some(cur) = stack.pop() {
        if !set.insert(cur) {
            continue;
        }
        if set.len() > MAX_VISIT {
            return Err(Error::Limit(
                "history traversal exceeded bound in ancestors".into(),
            ));
        }
        let (_, parents) = parents_of(repo, cur)?;
        stack.extend(parents);
    }
    Ok(set)
}

/// Best common ancestor of `a` and `b` (None when histories are unrelated).
/// Among common ancestors, the maximal one by (timestamp DESC, oid DESC) is
/// returned; traversal from `b` uses the same order so the result does not
/// depend on hash-set iteration order.
pub fn merge_base(repo: &Repo, a: ObjectId, b: ObjectId) -> Result<Option<ObjectId>> {
    let anc_a = ancestors(repo, a)?;
    #[derive(PartialEq, Eq)]
    struct Item(i64, ObjectId);
    impl PartialOrd for Item {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Item {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            (self.0, self.1).cmp(&(other.0, other.1))
        }
    }
    let mut heap = BinaryHeap::new();
    let mut visited: HashSet<ObjectId> = HashSet::new();
    let (ts, _) = parents_of(repo, b)?;
    heap.push(Item(ts, b));
    while let Some(Item(_, cur)) = heap.pop() {
        if !visited.insert(cur) {
            continue;
        }
        if visited.len() > MAX_VISIT {
            return Err(Error::Limit(
                "history traversal exceeded bound in merge_base".into(),
            ));
        }
        if anc_a.contains(&cur) {
            return Ok(Some(cur));
        }
        let (ts, parents) = parents_of(repo, cur)?;
        for p in parents {
            if !visited.contains(&p) {
                let (pts, _) = parents_of(repo, p)?;
                let _ = ts;
                heap.push(Item(pts, p));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::types::{Actor, ActorKind, Object, Snapshot, Tree};
    use std::collections::BTreeMap;

    fn snap(repo: &Repo, parents: Vec<ObjectId>, ts: i64, tag: u8) -> ObjectId {
        let root = repo.objects.put(&Object::Tree(Tree::empty())).unwrap();
        let author = {
            let a = Actor {
                kind: ActorKind::Process,
                id: format!("process:t{tag}"),
                display_name: "t".into(),
                tool: "test".into(),
                tool_version: "0".into(),
                pubkey: None,
                extras: BTreeMap::new(),
            };
            repo.objects.put(&Object::Actor(a)).unwrap()
        };
        let s = Snapshot {
            parents: parents.into_iter().collect(),
            root,
            author,
            timestamp_ms: ts,
            tz_offset_min: 0,
            message: format!("s{tag}"),
            workspace: None,
            change: None,
            goal: None,
            extras: BTreeMap::new(),
        };
        repo.objects.put(&Object::Snapshot(s)).unwrap()
    }

    fn repo() -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path().join("r");
        std::fs::create_dir(&r).unwrap();
        let repo = Repo::init(&r).unwrap();
        (dir, repo)
    }

    #[test]
    fn linear_ancestry() {
        let (_d, repo) = repo();
        let a = snap(&repo, vec![], 1, 1);
        let b = snap(&repo, vec![a], 2, 2);
        let c = snap(&repo, vec![b], 3, 3);
        assert!(is_ancestor(&repo, a, a).unwrap());
        assert!(is_ancestor(&repo, a, c).unwrap());
        assert!(is_ancestor(&repo, b, c).unwrap());
        assert!(!is_ancestor(&repo, c, a).unwrap());
        assert_eq!(merge_base(&repo, a, c).unwrap(), Some(a));
        assert_eq!(merge_base(&repo, c, c).unwrap(), Some(c));
    }

    #[test]
    fn diamond_lca() {
        let (_d, repo) = repo();
        let base = snap(&repo, vec![], 1, 0);
        let l = snap(&repo, vec![base], 2, 1);
        let r = snap(&repo, vec![base], 3, 2);
        assert_eq!(merge_base(&repo, l, r).unwrap(), Some(base));
        assert!(!is_ancestor(&repo, l, r).unwrap());
        let tip = snap(&repo, vec![l, r], 4, 3);
        assert!(is_ancestor(&repo, l, tip).unwrap());
        assert_eq!(merge_base(&repo, tip, l).unwrap(), Some(l));
    }

    #[test]
    fn unrelated_histories() {
        let (_d, repo) = repo();
        let a = snap(&repo, vec![], 1, 1);
        let b = snap(&repo, vec![], 1, 2);
        assert_eq!(merge_base(&repo, a, b).unwrap(), None);
        assert!(!is_ancestor(&repo, a, b).unwrap());
    }

    #[test]
    fn criss_cross_deterministic() {
        //     x
        //    / \
        //   a   b     a = x+m1, b = x+m2
        //   |\ /|
        //   | X |
        //   |/ \|
        //   p   q     p = a+b (ts 10), q = a+b (ts 9) — two merge bases
        let (_d, repo) = repo();
        let x = snap(&repo, vec![], 1, 0);
        let a = snap(&repo, vec![x], 2, 1);
        let b = snap(&repo, vec![x], 3, 2);
        let p = snap(&repo, vec![a, b], 10, 3);
        let q = snap(&repo, vec![a, b], 9, 4);
        // common ancestors of p,q = {p?,q? no — p not ancestor of q} = {a,b,x}
        // maximal by (ts, oid): a(ts2) vs b(ts3) → b wins deterministically
        assert_eq!(merge_base(&repo, p, q).unwrap(), Some(b));
        assert_eq!(merge_base(&repo, q, p).unwrap(), Some(b));
    }
}
