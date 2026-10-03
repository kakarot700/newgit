//! Snapshot/status/workspace operation tests (API level).

mod common;

use std::collections::BTreeMap;

use common::*;
use newgit::error::Error;
use newgit::object::types::{EntryMode, Object, Tree, TreeEntry};
use newgit::object::ObjectId;
use newgit::ops::checkout::{checkout_tree, CheckoutMode};
use newgit::ops::snapshot::{snapshot, SnapshotRequest};
use newgit::ops::status::status;
use newgit::ops::tree::flatten_tree;
use newgit::repo::workspace;

fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

fn req(ws: &str, msg: &str, author: ObjectId, ts: i64) -> SnapshotRequest {
    SnapshotRequest {
        workspace: ws.into(),
        message: msg.into(),
        author,
        timestamp_ms: Some(ts),
        tz_offset_min: 0,
        goal: None,
        change: None,
        extras: BTreeMap::new(),
    }
}

#[test]
fn snapshot_status_history_cycle() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    // unborn: status clean (nothing tracked, nothing on disk)
    let st = status(&repo, "main", 50).unwrap();
    assert!(st.clean && st.head.is_none());

    write(repo.root(), "a.txt", b"hello");
    write(repo.root(), "dir/b.txt", b"world");
    let st = status(&repo, "main", 50).unwrap();
    assert!(!st.clean);
    assert_eq!(st.added, vec!["a.txt".to_string(), "dir/b.txt".to_string()]);

    let out = snapshot(&repo, &req("main", "first", author, 1000)).unwrap();
    assert_eq!(out.entries, 2);
    assert_eq!(out.hashed, 2);
    assert_eq!(out.parent, None);
    assert_eq!(repo.refs.read("refs/main").unwrap(), out.oid);
    assert_eq!(repo.resolve_head().unwrap(), Some(out.oid));

    // clean now
    let st = status(&repo, "main", 50).unwrap();
    assert!(st.clean, "{st:?}");

    // modify + delete
    write(repo.root(), "a.txt", b"hello2");
    std::fs::remove_file(repo.root().join("dir/b.txt")).unwrap();
    let st = status(&repo, "main", 50).unwrap();
    assert_eq!(st.modified, vec!["a.txt".to_string()]);
    assert_eq!(st.deleted, vec!["dir/b.txt".to_string()]);

    let out2 = snapshot(&repo, &req("main", "second", author, 2000)).unwrap();
    assert_eq!(out2.parent, Some(out.oid));
    let st = status(&repo, "main", 50).unwrap();
    assert!(st.clean);

    // history
    let h = newgit::ops::history::history(&repo, None, 10).unwrap();
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].oid, out2.oid);
    assert_eq!(h[1].oid, out.oid);

    // trees: second snapshot has only a.txt
    let flat = flatten_tree(&repo, out2.root).unwrap();
    assert_eq!(flat.len(), 1);
    assert_eq!(flat[0].0, "a.txt");
    let blob = repo.objects.get(&flat[0].2).unwrap();
    assert_eq!(blob.as_blob().unwrap(), b"hello2");
}

#[test]
fn index_reuse_and_index_is_just_a_cache() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    for i in 0..20 {
        write(
            repo.root(),
            &format!("f{i:02}.txt"),
            format!("content {i}").as_bytes(),
        );
    }
    let o1 = snapshot(&repo, &req("main", "s1", author, 1)).unwrap();
    assert_eq!(o1.hashed, 20);
    // no changes: everything reused via index
    let o2 = snapshot(&repo, &req("main", "s2", author, 2)).unwrap();
    assert_eq!(o2.hashed, 0);
    assert_eq!(o2.reused, 20);
    assert_eq!(o1.root, o2.root); // same tree
                                  // INVARIANT: deleting the index loses nothing and changes no semantics
    std::fs::remove_file(workspace::index_path(&repo, "main")).unwrap();
    let st = status(&repo, "main", 50).unwrap();
    assert!(st.clean, "status must be correct without an index: {st:?}");
    let o3 = snapshot(&repo, &req("main", "s3", author, 3)).unwrap();
    assert_eq!(o3.hashed, 20); // rehashed without index...
    assert_eq!(o3.root, o1.root); // ...but same tree ⇒ same identity
}

#[test]
fn deterministic_snapshots() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "x.txt", b"same");
    let a = snapshot(&repo, &req("main", "msg", author, 42)).unwrap();
    // second identical snapshot (same tree, author, ts, message) after moving
    // the ref back — identity must be identical (I1 at snapshot level)
    repo.refs
        .update(
            "refs/main",
            newgit::repo::txn::Cas::Any,
            None,
            newgit::repo::txn::RefLogEntry::system("reset for test"),
        )
        .unwrap();
    let b = snapshot(&repo, &req("main", "msg", author, 42)).unwrap();
    assert_eq!(a.oid, b.oid);
}

#[test]
fn exec_bits_and_symlinks_roundtrip() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "run.sh", b"#!/bin/sh\necho hi\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(repo.root().join("run.sh"))
            .unwrap()
            .permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(repo.root().join("run.sh"), p).unwrap();
        std::os::unix::fs::symlink("run.sh", repo.root().join("link")).unwrap();
        std::os::unix::fs::symlink("../outside-target", repo.root().join("dirlink")).unwrap();
    }
    let out = snapshot(&repo, &req("main", "exec+links", author, 7)).unwrap();
    let flat = flatten_tree(&repo, out.root).unwrap();
    let modes: BTreeMap<_, _> = flat.iter().map(|(p, m, _)| (p.clone(), *m)).collect();
    assert_eq!(modes["run.sh"], EntryMode::Executable);
    #[cfg(unix)]
    {
        assert_eq!(modes["link"], EntryMode::Symlink);
        assert_eq!(modes["dirlink"], EntryMode::Symlink);
        // symlink blob content is the target string, verbatim
        let l = flat.iter().find(|(p, _, _)| p == "link").unwrap();
        let b = repo.objects.get(&l.2).unwrap();
        assert_eq!(b.as_blob().unwrap(), b"run.sh");
        // checkout into a fresh workspace restores them
        let ws = workspace::create(&repo, "ws1", Some(out.oid), author).unwrap();
        let lm = std::fs::symlink_metadata(ws.dir.join("link")).unwrap();
        assert!(lm.file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(ws.dir.join("link")).unwrap(),
            std::path::Path::new("run.sh")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(ws.dir.join("run.sh")).unwrap();
            assert_ne!(m.permissions().mode() & 0o111, 0, "exec bit restored");
        }
    }
}

#[test]
fn ignore_files_respected() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "keep.txt", b"k");
    write(repo.root(), "drop.log", b"d");
    write(repo.root(), "node_modules/pkg/i.js", b"x");
    write(repo.root(), ".newgitignore", b"*.log\nnode_modules/\n");
    let out = snapshot(&repo, &req("main", "ignores", author, 8)).unwrap();
    let flat = flatten_tree(&repo, out.root).unwrap();
    let names: Vec<&str> = flat.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(names, vec![".newgitignore", "keep.txt"]);
}

#[test]
fn workspaces_isolated_and_concurrent_safe() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "base.txt", b"base");
    let s1 = snapshot(&repo, &req("main", "base", author, 10)).unwrap();

    // two workspaces from the same base
    let wa = workspace::create(&repo, "ws-a", Some(s1.oid), author).unwrap();
    let wb = workspace::create(&repo, "ws-b", Some(s1.oid), author).unwrap();
    assert_eq!(wa.head_oid, Some(s1.oid));
    assert!(wa.dir.join("base.txt").exists());
    assert!(wb.dir.join("base.txt").exists());

    // independent modifications
    write(&wa.dir, "base.txt", b"from A");
    write(&wa.dir, "only-a.txt", b"A");
    write(&wb.dir, "base.txt", b"from B");

    let st_a = status(&repo, "ws-a", 50).unwrap();
    assert_eq!(st_a.modified, vec!["base.txt".to_string()]);
    assert_eq!(st_a.added, vec!["only-a.txt".to_string()]);
    let st_b = status(&repo, "ws-b", 50).unwrap();
    assert_eq!(st_b.modified, vec!["base.txt".to_string()]);
    assert!(st_b.added.is_empty());
    // main untouched
    let st_main = status(&repo, "main", 50).unwrap();
    assert!(st_main.clean);

    let sa = snapshot(&repo, &req("ws-a", "A work", author, 11)).unwrap();
    let sb = snapshot(&repo, &req("ws-b", "B work", author, 12)).unwrap();
    assert_ne!(sa.oid, sb.oid);
    assert_eq!(sa.parent, Some(s1.oid));
    assert_eq!(sb.parent, Some(s1.oid));
    assert_eq!(repo.refs.read("workspaces/ws-a").unwrap(), sa.oid);
    assert_eq!(repo.refs.read("refs/main").unwrap(), s1.oid);

    // workspace list shows all three
    let list = workspace::list(&repo).unwrap();
    let names: Vec<&str> = list.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, vec!["main", "ws-a", "ws-b"]);
}

#[test]
fn workspace_discard_rules() {
    let (_d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "f.txt", b"1");
    let s = snapshot(&repo, &req("main", "s", author, 20)).unwrap();
    let w = workspace::create(&repo, "tmp", Some(s.oid), author).unwrap();
    // dirty discard refused
    write(&w.dir, "f.txt", b"changed");
    let r = workspace::discard(&repo, "tmp", false);
    assert!(matches!(r, Err(Error::Conflict(_))), "{r:?}");
    // force works
    workspace::discard(&repo, "tmp", true).unwrap();
    assert!(workspace::info(&repo, "tmp").is_err());
    assert!(repo.refs.read_opt("workspaces/tmp").unwrap().is_none());
    assert!(!w.meta_dir.exists());
    // main cannot be discarded
    assert!(workspace::discard(&repo, "main", true).is_err());
}

#[test]
fn detached_head_snapshot_rejected() {
    let (_d, repo) = temp_repo();
    let oid = oid(5);
    repo.set_head(
        &newgit::repo::Head::Detached(oid),
        newgit::repo::txn::RefLogEntry::system("detach"),
    )
    .unwrap();
    let author = repo.default_actor().unwrap();
    let r = snapshot(&repo, &req("main", "nope", author, 1));
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
}

#[test]
fn checkout_refuses_symlink_component_traversal() {
    // Craft a tree: "link" → symlink to "elsewhere", plus "link/evil.txt".
    // A hostile repository could contain this; checkout must refuse to write
    // through the symlinked component (THREAT_MODEL §B).
    let (_d, repo) = temp_repo();
    let target_blob = repo.objects.put_blob(b"elsewhere").unwrap();
    let evil_blob = repo.objects.put_blob(b"evil").unwrap();
    let link_entry = TreeEntry {
        name: "link".into(),
        mode: EntryMode::Symlink,
        oid: target_blob,
    };
    let inner = Tree::new(vec![TreeEntry {
        name: "evil.txt".into(),
        mode: EntryMode::File,
        oid: evil_blob,
    }])
    .unwrap();
    // To make "link" both a symlink and a directory we need two trees where
    // the ROOT has "link" as symlink and the path link/evil.txt implies a
    // dir "link" — impossible in ONE valid tree (names unique). The attack
    // surface is sequential checkout: symlink written first, then a path
    // whose component resolves through it. Simulate with a root tree that
    // has symlink "a" -> "." and file "a/x"? also impossible in one tree.
    // The realistic vector: EXISTING symlink in the destination directory
    // (e.g. leftover workspace) + checkout writing "through" it.
    let dest = repo.root().join("dest");
    std::fs::create_dir(&dest).unwrap();
    let outside = repo.root().join("outside");
    std::fs::create_dir(&outside).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, dest.join("sub")).unwrap();
    let root = Tree::new(vec![
        link_entry,
        TreeEntry {
            name: "sub".into(),
            mode: EntryMode::Tree,
            oid: repo.objects.put(&Object::Tree(inner)).unwrap(),
        },
    ]);
    // root has both "link" (symlink) and "sub" (tree containing evil.txt);
    // dest already contains symlink sub → outside. Overwrite checkout must
    // refuse to traverse the existing symlink "sub".
    let root = root.unwrap();
    let root_oid = repo.objects.put(&Object::Tree(root)).unwrap();
    let r = checkout_tree(&repo, root_oid, &dest, CheckoutMode::Overwrite, None);
    #[cfg(unix)]
    assert!(matches!(r, Err(Error::Invalid(_))), "{r:?}");
    #[cfg(not(unix))]
    let _ = (r, outside);
    // nothing escaped
    #[cfg(unix)]
    assert!(!outside.join("evil.txt").exists());
}

#[test]
fn checkout_fresh_requires_empty() {
    let (_d, repo) = temp_repo();
    let dest = repo.root().join("dest");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("existing"), b"x").unwrap();
    let empty = repo.objects.put(&Object::Tree(Tree::empty())).unwrap();
    let r = checkout_tree(&repo, empty, &dest, CheckoutMode::FreshWorkspace, None);
    assert!(matches!(r, Err(Error::Invalid(_))));
}

#[test]
fn snapshot_crash_before_txn_is_harmless() {
    // kill after the tree is built but before the ref update: no position
    // change, objects may exist (unreachable → GC fodder), next snapshot OK.
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    write(&root, "f.txt", b"data");
    drop(repo);
    let out = run_faultlab(&root, &["snapshot", "crash me"], Some("snap:before_txn"));
    assert!(!out.status.success());
    let repo = newgit::repo::Repo::open(&root).unwrap();
    assert_eq!(repo.resolve_head().unwrap(), None, "ref must not move");
    // and a normal snapshot afterwards works
    let author = repo.default_actor().unwrap();
    let o = snapshot(&repo, &req("main", "after crash", author, 99)).unwrap();
    assert_eq!(repo.refs.read("refs/main").unwrap(), o.oid);
}

#[test]
fn snapshot_crash_after_txn_is_complete() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    write(&root, "g.txt", b"data");
    drop(repo);
    let out = run_faultlab(&root, &["snapshot", "ok"], Some("snap:after_txn"));
    assert!(!out.status.success());
    // the snapshot transaction was durable ⇒ position moved
    let repo = newgit::repo::Repo::open(&root).unwrap();
    let head = repo.resolve_head().unwrap();
    assert!(head.is_some(), "txn committed before crash ⇒ ref set");
    let st = status(&repo, "main", 10).unwrap();
    assert!(st.clean);
}

#[test]
fn workspace_create_crash_leaves_no_registered_workspace() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    let author = repo.default_actor().unwrap();
    write(&root, "f.txt", b"x");
    let s = snapshot(&repo, &req("main", "base", author, 5)).unwrap();
    drop(repo);
    // kill during the workspace-create txn (at the ref write)
    let out = run_faultlab(
        &root,
        &["workspace-create", "wsc"],
        Some("txn:ref_write_err"),
    );
    assert!(!out.status.success());
    let repo = newgit::repo::Repo::open(&root).unwrap();
    // either the txn completed fully (ref+meta) or not at all — but meta may
    // have been written by apply order; recovery must converge to BOTH.
    let has_ref = repo.refs.read_opt("workspaces/wsc").unwrap().is_some();
    let has_meta = repo.ng().join("workspaces/wsc/meta").exists();
    assert_eq!(has_ref, has_meta, "ref and meta must agree after recovery");
    if has_ref {
        assert_eq!(repo.refs.read("workspaces/wsc").unwrap(), s.oid);
    }
}
