//! Object-graph negotiation helpers shared by server and client.
//!
//! Two primitives:
//! * `closure` — the full reachable set of roots (via verify::reachable,
//!   which follows every typed link + chain `extras.prev`);
//! * `post_order` — reachable(roots) minus an exclude set, emitted
//!   DEPENDENCIES FIRST, never descending into excluded oids (so excluded
//!   objects need not even exist locally — the peer vouches for their
//!   closure via the have-invariant in PROTOCOL.md).

use std::collections::{BTreeSet, HashSet};

use crate::error::{Error, Result};
use crate::object::ObjectId;
use crate::ops::verify;
use crate::repo::Repo;

/// Full reachable set from `roots` (missing/unreadable links are ignored —
/// callers decide whether that is an error).
pub fn closure(repo: &Repo, roots: &[ObjectId]) -> BTreeSet<ObjectId> {
    verify::reachable(repo, roots).0
}

/// Reachable(roots) \ exclude, dependencies before dependents.
/// Excluded oids are skipped WITHOUT being read (the peer holds their
/// closure). Objects under roots must be readable locally.
pub fn post_order(
    repo: &Repo,
    roots: &[ObjectId],
    exclude: &HashSet<ObjectId>,
) -> Result<Vec<ObjectId>> {
    let mut out = Vec::new();
    let mut done: HashSet<ObjectId> = HashSet::new();
    // (oid, children_pushed) — iterative post-order DFS
    let mut stack: Vec<(ObjectId, bool)> = roots.iter().map(|r| (*r, false)).collect();
    while let Some((oid, expanded)) = stack.pop() {
        if !done.insert(oid) {
            continue;
        }
        if exclude.contains(&oid) {
            continue; // peer vouches for this closure; do not descend or send
        }
        let obj = repo.objects.get(&oid).map_err(|_| {
            Error::Protocol(format!(
                "object {oid} is required for transfer but unreadable locally"
            ))
        })?;
        if expanded {
            out.push(oid);
        } else {
            // Re-push self AFTER children so it is emitted last (post-order).
            done.remove(&oid);
            stack.push((oid, true));
            for link in verify::object_links(&obj) {
                if !done.contains(&link) {
                    stack.push((link, false));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::types::{Actor, ActorKind, EntryMode, Object, Snapshot, Tree, TreeEntry};
    use std::collections::BTreeMap;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!(
                "ngneg-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn put_blob(repo: &Repo, data: &[u8]) -> ObjectId {
        repo.objects.put_blob(data).unwrap()
    }

    fn put_actor(repo: &Repo) -> ObjectId {
        let a = Actor {
            kind: ActorKind::Agent,
            id: "agent:test".into(),
            display_name: "Test Agent".into(),
            tool: String::new(),
            tool_version: String::new(),
            pubkey: None,
            extras: BTreeMap::new(),
        };
        repo.put(&Object::Actor(a)).unwrap()
    }

    fn put_snapshot(
        repo: &Repo,
        author: ObjectId,
        root: ObjectId,
        parents: Vec<ObjectId>,
        msg: &str,
    ) -> ObjectId {
        let s = Snapshot {
            parents,
            root,
            author,
            timestamp_ms: 1_700_000_000_000,
            tz_offset_min: 0,
            message: msg.into(),
            workspace: None,
            change: None,
            goal: None,
            extras: BTreeMap::new(),
        };
        repo.put(&Object::Snapshot(s)).unwrap()
    }

    #[test]
    fn post_order_emits_dependencies_first() {
        let tmp = Tmp::new("po");
        let repo = Repo::init(&tmp.0).unwrap();
        let a = put_actor(&repo);
        let b1 = put_blob(&repo, b"one");
        let b2 = put_blob(&repo, b"two");
        let tree = Tree {
            entries: vec![
                TreeEntry {
                    name: "a".into(),
                    mode: EntryMode::File,
                    oid: b1,
                },
                TreeEntry {
                    name: "b".into(),
                    mode: EntryMode::File,
                    oid: b2,
                },
            ],
        };
        let t = repo.put(&Object::Tree(tree)).unwrap();
        let child = put_snapshot(&repo, a, t, vec![], "child");
        let parent = put_snapshot(&repo, a, t, vec![child], "parent");

        let all = post_order(&repo, &[parent], &HashSet::new()).unwrap();
        let pos = |o: ObjectId| all.iter().position(|x| *x == o).unwrap();
        assert!(pos(b1) < pos(t), "blobs before tree");
        assert!(pos(b2) < pos(t));
        assert!(pos(t) < pos(child), "tree before snapshot");
        assert!(pos(a) < pos(child), "author before snapshot");
        assert!(pos(child) < pos(parent), "parent snapshot last");
        assert_eq!(all.len(), closure(&repo, &[parent]).len());
        assert_eq!(all.last().copied(), Some(parent));
    }

    #[test]
    fn post_order_excludes_without_reading_them() {
        let tmp = Tmp::new("po-ex");
        let repo = Repo::init(&tmp.0).unwrap();
        let a = put_actor(&repo);
        let b1 = put_blob(&repo, b"one");
        let t = repo
            .put(&Object::Tree(Tree {
                entries: vec![TreeEntry {
                    name: "a".into(),
                    mode: EntryMode::File,
                    oid: b1,
                }],
            }))
            .unwrap();
        let s = put_snapshot(&repo, a, t, vec![], "s");

        // Excluding the tree prunes its subtree (b1) even though b1 is only
        // reachable THROUGH the tree.
        let mut ex = HashSet::new();
        ex.insert(t);
        let pruned = post_order(&repo, &[s], &ex).unwrap();
        assert!(!pruned.contains(&t));
        assert!(!pruned.contains(&b1));
        assert!(pruned.contains(&s) && pruned.contains(&a));

        // An excluded root that does not exist locally is never read — the
        // peer vouches for its closure. This is what makes push/pull
        // negotiation work across divergent histories.
        let foreign =
            ObjectId::from_hex("1111111111111111111111111111111111111111111111111111111111111111")
                .unwrap();
        let mut ex2 = HashSet::new();
        ex2.insert(foreign);
        assert!(post_order(&repo, &[foreign], &ex2).unwrap().is_empty());
        // ...and a NON-excluded missing object is a loud Protocol error,
        // never a silent gap in the transfer set.
        assert!(matches!(
            post_order(&repo, &[foreign], &HashSet::new()),
            Err(Error::Protocol(_))
        ));
    }
}
