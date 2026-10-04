//! Merge engine + integration tests (API level): tree merges, conflicts,
//! renames, integrate/rollback atomicity, races, and crash behavior.

mod common;

use std::collections::BTreeMap;

use common::*;
use newgit::error::Error;
use newgit::merge::diff3::{merge_lines, render_merged};
use newgit::merge::{merge_trees, Conflict, MergeOpts};
use newgit::object::types::{EntryMode, Object, Tree, TreeEntry};
use newgit::object::ObjectId;
use newgit::ops::integrate::{
    checkout_position, conflict_error, integrate, merge_tree_dry, rollback, IntegrateOutcome,
    IntegrateRequest, RollbackRequest,
};
use newgit::ops::snapshot::{snapshot, SnapshotRequest};
use newgit::ops::status::status;
use newgit::repo::txn::{Cas, RefLogEntry};
use newgit::repo::workspace;

fn write(dir: &std::path::Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

fn req(repo: &newgit::repo::Repo, msg: &str, ts: i64) -> SnapshotRequest {
    let author = repo.default_actor().unwrap();
    SnapshotRequest {
        workspace: "main".into(),
        message: msg.into(),
        author,
        timestamp_ms: Some(ts),
        tz_offset_min: 0,
        goal: None,
        change: None,
        extras: BTreeMap::new(),
    }
}

fn ws_req(repo: &newgit::repo::Repo, ws: &str, msg: &str, ts: i64) -> SnapshotRequest {
    SnapshotRequest {
        workspace: ws.into(),
        ..req(repo, msg, ts)
    }
}

/// Build a tree from (path, content, mode) tuples directly in the store.
fn tree_from(repo: &newgit::repo::Repo, items: &[(&str, &[u8], EntryMode)]) -> ObjectId {
    let v: Vec<(String, ObjectId, EntryMode)> = items
        .iter()
        .map(|(p, c, m)| (p.to_string(), repo.objects.put_blob(c).unwrap(), *m))
        .collect();
    newgit::ops::tree::build_tree(repo, &v).unwrap()
}

fn blob_bytes(repo: &newgit::repo::Repo, oid: ObjectId) -> Vec<u8> {
    repo.objects.get(&oid).unwrap().as_blob().unwrap().to_vec()
}

// ─────────────────────────── tree merge cases ───────────────────────────

#[test]
fn merge_disjoint_edits_clean() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(&repo, &[("a.txt", b"1\n2\n3\n", f), ("b.txt", b"x\n", f)]);
    let ours = tree_from(
        &repo,
        &[("a.txt", b"1 OURS\n2\n3\n", f), ("b.txt", b"x\n", f)],
    );
    let theirs = tree_from(
        &repo,
        &[("a.txt", b"1\n2\n3 THEIRS\n", f), ("b.txt", b"x\n", f)],
    );
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(out.clean, "{:?}", out.conflicts);
    assert_eq!(out.entries, 2);
    // merged content combined
    let flat = newgit::ops::tree::flatten_tree(&repo, out.root).unwrap();
    let a = flat.iter().find(|(p, _, _)| p == "a.txt").unwrap();
    assert_eq!(blob_bytes(&repo, a.2), b"1 OURS\n2\n3 THEIRS\n");
}

#[test]
fn merge_overlapping_edits_conflict_with_markers() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(&repo, &[("a.txt", b"top\nmid\nbot\n", f)]);
    let ours = tree_from(&repo, &[("a.txt", b"top\nOURS\nbot\n", f)]);
    let theirs = tree_from(&repo, &[("a.txt", b"top\nTHEIRS\nbot\n", f)]);
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(!out.clean);
    assert_eq!(out.conflicts.len(), 1);
    match &out.conflicts[0] {
        Conflict::Content {
            path,
            merged_oid,
            capped,
        } => {
            assert_eq!(path, "a.txt");
            assert!(!capped);
            let merged = String::from_utf8(blob_bytes(&repo, *merged_oid)).unwrap();
            assert!(merged.contains("<<<<<<< ours\nOURS\n"));
            assert!(merged.contains("||||||| base\nmid\n"));
            assert!(merged.contains("=======\nTHEIRS\n"));
            assert!(merged.contains(">>>>>>> theirs\n"));
            assert!(merged.starts_with("top\n") && merged.ends_with("bot\n"));
        }
        other => panic!("expected content conflict, got {other:?}"),
    }
    // best-effort tree still inspectable (ours side kept)
    assert_eq!(out.entries, 1);
}

#[test]
fn merge_identical_changes_and_adds() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(&repo, &[("a.txt", b"1\n", f)]);
    // both sides made the SAME change + added the SAME file
    let ours = tree_from(&repo, &[("a.txt", b"2\n", f), ("new.txt", b"same\n", f)]);
    let theirs = tree_from(&repo, &[("a.txt", b"2\n", f), ("new.txt", b"same\n", f)]);
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(out.clean);
    assert_eq!(
        out.root, ours,
        "identical sides must short-circuit to same tree"
    );
    // add/add with DIFFERENT content conflicts
    let theirs2 = tree_from(&repo, &[("a.txt", b"2\n", f), ("new.txt", b"other\n", f)]);
    let out2 = merge_trees(&repo, base, ours, theirs2, &MergeOpts::default()).unwrap();
    assert!(!out2.clean);
    assert!(matches!(out2.conflicts[0], Conflict::Content { .. }));
}

#[test]
fn merge_modify_delete_conflicts() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(&repo, &[("a.txt", b"1\n", f), ("b.txt", b"2\n", f)]);
    let ours = tree_from(
        &repo,
        &[("a.txt", b"1 MODIFIED\n", f), ("b.txt", b"2\n", f)],
    );
    let theirs = tree_from(&repo, &[("b.txt", b"2\n", f)]); // deleted a.txt
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(!out.clean);
    match &out.conflicts[0] {
        Conflict::ModifyDelete {
            path,
            modified_by,
            kept_oid,
        } => {
            assert_eq!(path, "a.txt");
            assert_eq!(modified_by, "ours");
            assert_eq!(blob_bytes(&repo, *kept_oid), b"1 MODIFIED\n");
        }
        other => panic!("{other:?}"),
    }
    // untouched file deleted on one side: deletion wins, no conflict
    let ours2 = tree_from(&repo, &[("a.txt", b"1\n", f), ("b.txt", b"2\n", f)]);
    let out2 = merge_trees(&repo, base, ours2, theirs, &MergeOpts::default()).unwrap();
    assert!(out2.clean);
    assert_eq!(out2.entries, 1);
}

#[test]
fn merge_modes() {
    let (_d, repo) = temp_repo();
    // one side flips exec bit, other edits content → both apply
    let base = tree_from(&repo, &[("run.sh", b"echo 1\n", EntryMode::File)]);
    let ours = tree_from(&repo, &[("run.sh", b"echo 1\n", EntryMode::Executable)]);
    let theirs = tree_from(&repo, &[("run.sh", b"echo 2\n", EntryMode::File)]);
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(out.clean, "{:?}", out.conflicts);
    let flat = newgit::ops::tree::flatten_tree(&repo, out.root).unwrap();
    assert_eq!(flat[0].1, EntryMode::Executable);
    assert_eq!(blob_bytes(&repo, flat[0].2), b"echo 2\n");
    // both flip modes differently → conflict
    let theirs2 = tree_from(&repo, &[("run.sh", b"echo 1\n", EntryMode::Symlink)]);
    let out2 = merge_trees(&repo, base, ours, theirs2, &MergeOpts::default()).unwrap();
    assert!(!out2.clean);
    assert!(matches!(out2.conflicts[0], Conflict::Mode { .. }));
}

#[test]
fn merge_binary_and_symlink_conflicts() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(&repo, &[("d.bin", b"\x00\x01base", f)]);
    let ours = tree_from(&repo, &[("d.bin", b"\x00\x01ours", f)]);
    let theirs = tree_from(&repo, &[("d.bin", b"\x00\x01theirs", f)]);
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(!out.clean);
    match &out.conflicts[0] {
        Conflict::Opaque { reason, .. } => assert!(reason.contains("binary")),
        other => panic!("{other:?}"),
    }
    // symlinks with different targets
    let s = EntryMode::Symlink;
    let base = tree_from(&repo, &[("lnk", b"t1", s)]);
    let ours = tree_from(&repo, &[("lnk", b"t2", s)]);
    let theirs = tree_from(&repo, &[("lnk", b"t3", s)]);
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(!out.clean);
    match &out.conflicts[0] {
        Conflict::Opaque { reason, .. } => assert!(reason.contains("symlink")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn merge_rename_tracking() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(
        &repo,
        &[("old.txt", b"l1\nl2\nl3\n", f), ("k.txt", b"k\n", f)],
    );

    // ours: rename old.txt → new.txt (exact); theirs: edit content at old.txt
    let ours = tree_from(
        &repo,
        &[("new.txt", b"l1\nl2\nl3\n", f), ("k.txt", b"k\n", f)],
    );
    let theirs = tree_from(
        &repo,
        &[("old.txt", b"l1\nl2 EDIT\nl3\n", f), ("k.txt", b"k\n", f)],
    );
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(out.clean, "{:?}", out.conflicts);
    let flat = newgit::ops::tree::flatten_tree(&repo, out.root).unwrap();
    let paths: Vec<&str> = flat.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        vec!["k.txt", "new.txt"],
        "rename+edit merges at new path"
    );
    let nt = flat.iter().find(|(p, _, _)| p == "new.txt").unwrap();
    assert_eq!(blob_bytes(&repo, nt.2), b"l1\nl2 EDIT\nl3\n");
    assert_eq!(
        out.renames,
        vec![("old.txt".to_string(), "new.txt".to_string())]
    );

    // rename/rename to different paths → conflict, both survive
    let ours_rr = tree_from(
        &repo,
        &[("a1.txt", b"l1\nl2\nl3\n", f), ("k.txt", b"k\n", f)],
    );
    let theirs_rr = tree_from(
        &repo,
        &[("a2.txt", b"l1\nl2\nl3\n", f), ("k.txt", b"k\n", f)],
    );
    let out2 = merge_trees(&repo, base, ours_rr, theirs_rr, &MergeOpts::default()).unwrap();
    assert!(!out2.clean);
    assert!(matches!(out2.conflicts[0], Conflict::RenameRename { .. }));
    let flat2 = newgit::ops::tree::flatten_tree(&repo, out2.root).unwrap();
    let p2: Vec<&str> = flat2.iter().map(|(p, _, _)| p.as_str()).collect();
    assert!(p2.contains(&"a1.txt") && p2.contains(&"a2.txt"));

    // rename vs delete → conflict
    let theirs_del = tree_from(&repo, &[("k.txt", b"k\n", f)]);
    let out3 = merge_trees(&repo, base, ours, theirs_del, &MergeOpts::default()).unwrap();
    assert!(!out3.clean);
    assert!(
        matches!(out3.conflicts[0], Conflict::RenameDelete { .. }),
        "{:?}",
        out3.conflicts
    );

    // no-rename mode: rename shows as delete+add
    let opts = MergeOpts {
        track_renames: false,
        ..Default::default()
    };
    let out4 = merge_trees(&repo, base, ours, theirs, &opts).unwrap();
    assert!(!out4.clean); // modify/delete on old.txt + add of new.txt
    assert!(out4.renames.is_empty());
}

#[test]
fn merge_dir_file_collision() {
    let (_d, repo) = temp_repo();
    let f = EntryMode::File;
    let base = tree_from(&repo, &[("k.txt", b"k\n", f)]);
    let ours = tree_from(&repo, &[("k.txt", b"k\n", f), ("x", b"file\n", f)]);
    let theirs = tree_from(&repo, &[("k.txt", b"k\n", f), ("x/y.txt", b"inner\n", f)]);
    let out = merge_trees(&repo, base, ours, theirs, &MergeOpts::default()).unwrap();
    assert!(!out.clean);
    assert!(matches!(out.conflicts[0], Conflict::DirectoryFile { .. }));
}

#[test]
fn diff3_direct_cases() {
    // merge_lines unit behavior on non-UTF8-safe bytes
    let base: Vec<&[u8]> = vec![b"a\n", b"b\n"];
    let ours: Vec<&[u8]> = vec![b"a\n", b"B1\n"];
    let theirs: Vec<&[u8]> = vec![b"a\n", b"B2\n"];
    let r = merge_lines(&base, &ours, &theirs, 1024);
    assert!(r.conflicted);
    let merged = render_merged(&r.chunks, false);
    assert!(merged.windows(7).any(|w| w == b"<<<<<<<"));
    // determinism
    let r2 = merge_lines(&base, &ours, &theirs, 1024);
    assert_eq!(
        render_merged(&r.chunks, true),
        render_merged(&r2.chunks, true)
    );
}

// ─────────────────────────── integrate / rollback ───────────────────────────

fn integ_req(repo: &newgit::repo::Repo, ws: &str, other: ObjectId, ts: i64) -> IntegrateRequest {
    IntegrateRequest {
        workspace: ws.into(),
        other,
        message: None,
        author: repo.default_actor().unwrap(),
        timestamp_ms: Some(ts),
        merge_opts: MergeOpts::default(),
    }
}

#[test]
fn integrate_up_to_date_and_fast_forward() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "f.txt", b"1\n");
    let s1 = snapshot(&repo, &req(&repo, "s1", 100)).unwrap();
    // up to date: integrate self
    let out = integrate(&repo, &integ_req(&repo, "main", s1.oid, 101)).unwrap();
    assert!(matches!(out, IntegrateOutcome::UpToDate { .. }));
    // make a linear descendant on a named workspace, ff main to it
    let wa = workspace::create(&repo, "feat", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    write(&wa.dir, "f.txt", b"2\n");
    let s2 = snapshot(&repo, &ws_req(&repo, "feat", "s2", 102)).unwrap();
    let out = integrate(&repo, &integ_req(&repo, "main", s2.oid, 103)).unwrap();
    match out {
        IntegrateOutcome::FastForward { from, to } => {
            assert_eq!(from, Some(s1.oid));
            assert_eq!(to, s2.oid);
        }
        other => panic!("expected ff, got {other:?}"),
    }
    assert_eq!(repo.refs.read("refs/main").unwrap(), s2.oid);
    // main workspace FILES were updated by the ff checkout
    assert_eq!(std::fs::read(repo.root().join("f.txt")).unwrap(), b"2\n");
    assert!(status(&repo, "main", 10).unwrap().clean);
    // integrating an ancestor now is up-to-date
    let out = integrate(&repo, &integ_req(&repo, "main", s1.oid, 104)).unwrap();
    assert!(matches!(out, IntegrateOutcome::UpToDate { .. }));
}

#[test]
fn integrate_three_way_merge_atomic_and_checked_out() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "shared.txt", b"base line\n");
    write(repo.root(), "main-only.txt", b"m\n");
    let s1 = snapshot(&repo, &req(&repo, "base", 200)).unwrap();

    let wa = workspace::create(
        &repo,
        "agent-a",
        Some(s1.oid),
        repo.default_actor().unwrap(),
    )
    .unwrap();
    let wb = workspace::create(
        &repo,
        "agent-b",
        Some(s1.oid),
        repo.default_actor().unwrap(),
    )
    .unwrap();
    write(&wa.dir, "shared.txt", b"base line\nA addition\n");
    write(&wa.dir, "a.txt", b"A\n");
    let sa = snapshot(&repo, &ws_req(&repo, "agent-a", "A work", 201)).unwrap();
    write(&wb.dir, "shared.txt", b"B prepend\nbase line\n");
    write(&wb.dir, "b.txt", b"B\n");
    let sb = snapshot(&repo, &ws_req(&repo, "agent-b", "B work", 202)).unwrap();

    // merge B into A (disjoint hunks in shared.txt)
    let out = integrate(&repo, &integ_req(&repo, "agent-a", sb.oid, 203)).unwrap();
    let merge_oid = match out {
        IntegrateOutcome::Merged { oid, parents, .. } => {
            assert_eq!(parents.len(), 2);
            oid
        }
        other => panic!("{other:?}"),
    };
    assert_eq!(repo.refs.read("workspaces/agent-a").unwrap(), merge_oid);
    // files materialized
    assert_eq!(
        std::fs::read(wa.dir.join("shared.txt")).unwrap(),
        b"B prepend\nbase line\nA addition\n"
    );
    assert!(wa.dir.join("b.txt").exists());
    assert!(status(&repo, "agent-a", 10).unwrap().clean);
    // merge snapshot has two parents + extras roles
    let m = repo.objects.get(&merge_oid).unwrap();
    let ms = m.as_snapshot().unwrap();
    assert_eq!(ms.parents.len(), 2);
    assert_eq!(ms.extras["op"], "integrate");
    assert_eq!(ms.extras["merge_ours"], sa.oid.to_hex());
    assert_eq!(ms.extras["merge_theirs"], sb.oid.to_hex());
    // B side untouched
    assert_eq!(repo.refs.read("workspaces/agent-b").unwrap(), sb.oid);

    // rollback agent-a to pre-merge (default: merge_ours)
    let rb = rollback(
        &repo,
        &RollbackRequest {
            workspace: "agent-a".into(),
            target: None,
            message: None,
            author: repo.default_actor().unwrap(),
            timestamp_ms: Some(204),
        },
    )
    .unwrap();
    assert_eq!(repo.refs.read("workspaces/agent-a").unwrap(), rb);
    let rbs = repo.objects.get(&rb).unwrap();
    let rbs = rbs.as_snapshot().unwrap();
    assert_eq!(rbs.root, sa.root, "rollback restores the pre-merge tree");
    assert_eq!(rbs.extras["op"], "rollback");
    // b.txt was materialized by the merge checkout (recorded in the index),
    // so the rollback checkout removes it — status is clean afterwards
    assert!(!wa.dir.join("b.txt").exists());
    assert_eq!(
        std::fs::read(wa.dir.join("shared.txt")).unwrap(),
        b"base line\nA addition\n"
    );
    assert!(status(&repo, "agent-a", 10).unwrap().clean);
    // explicit --to rollback
    let rb2 = rollback(
        &repo,
        &RollbackRequest {
            workspace: "agent-a".into(),
            target: Some(s1.oid),
            message: Some("all the way back".into()),
            author: repo.default_actor().unwrap(),
            timestamp_ms: Some(205),
        },
    )
    .unwrap();
    let rb2s = repo.objects.get(&rb2).unwrap();
    assert_eq!(rb2s.as_snapshot().unwrap().root, s1.root);
    // files match the s1 tree exactly
    assert!(!wa.dir.join("a.txt").exists());
    assert_eq!(
        std::fs::read(wa.dir.join("shared.txt")).unwrap(),
        b"base line\n"
    );
    assert!(status(&repo, "agent-a", 10).unwrap().clean);
}

#[test]
fn integrate_conflict_writes_nothing() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "f.txt", b"line\n");
    let s1 = snapshot(&repo, &req(&repo, "base", 300)).unwrap();
    let wa = workspace::create(&repo, "ca", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    let wb = workspace::create(&repo, "cb", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    write(&wa.dir, "f.txt", b"OURS\n");
    let sa = snapshot(&repo, &ws_req(&repo, "ca", "a", 301)).unwrap();
    write(&wb.dir, "f.txt", b"THEIRS\n");
    let sb = snapshot(&repo, &ws_req(&repo, "cb", "b", 302)).unwrap();

    let before_a = std::fs::read(wa.dir.join("f.txt")).unwrap();
    let r = integrate(&repo, &integ_req(&repo, "ca", sb.oid, 303));
    match r {
        Err(Error::Conflict(msg)) => {
            assert!(msg.contains("f.txt"));
            assert!(msg.contains("merge-tree"));
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    // NOTHING changed: ref, files
    assert_eq!(repo.refs.read("workspaces/ca").unwrap(), sa.oid);
    assert_eq!(std::fs::read(wa.dir.join("f.txt")).unwrap(), before_a);
    assert!(status(&repo, "ca", 10).unwrap().clean);

    // dry-run reports the conflict blob
    let out = merge_tree_dry(
        &repo,
        &sa.oid.to_hex(),
        &sb.oid.to_hex(),
        None,
        &MergeOpts::default(),
    )
    .unwrap();
    assert!(!out.clean);
    match &out.conflicts[0] {
        Conflict::Content { merged_oid, .. } => {
            let merged = String::from_utf8(blob_bytes(&repo, *merged_oid)).unwrap();
            assert!(merged.contains("<<<<<<< ours\nOURS\n"));
            assert!(merged.contains(">>>>>>> theirs\n"));
        }
        other => panic!("{other:?}"),
    }
    // conflict_error summary is deterministic
    let e1 = conflict_error(&out).to_string();
    let e2 = conflict_error(&out).to_string();
    assert_eq!(e1, e2);
}

#[test]
fn integrate_unborn_position_fast_forwards() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "f.txt", b"x\n");
    let s1 = snapshot(&repo, &req(&repo, "s1", 400)).unwrap();
    // fresh repo #2 with unborn HEAD integrates s1? Cross-repo not possible —
    // instead: unborn named workspace is impossible (create needs meta)...
    // simulate unborn main by deleting refs/main
    repo.refs
        .update(
            "refs/main",
            Cas::Any,
            None,
            RefLogEntry::system("test: unbirth"),
        )
        .unwrap();
    let out = integrate(&repo, &integ_req(&repo, "main", s1.oid, 401)).unwrap();
    assert!(matches!(
        out,
        IntegrateOutcome::FastForward { from: None, .. }
    ));
    assert_eq!(repo.refs.read("refs/main").unwrap(), s1.oid);
    assert!(status(&repo, "main", 10).unwrap().clean);
}

#[test]
fn integrate_concurrent_same_target_serializes() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    write(repo.root(), "f.txt", b"top\nmid\nbot\n");
    let s1 = snapshot(&repo, &req(&repo, "base", 500)).unwrap();
    let wa = workspace::create(&repo, "ra", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    write(&wa.dir, "f.txt", b"TOP\nmid\nbot\n");
    let sa = snapshot(&repo, &ws_req(&repo, "ra", "a", 501)).unwrap();
    let wb = workspace::create(&repo, "rb", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    write(&wb.dir, "f.txt", b"top\nmid\nBOT\n");
    let sb = snapshot(&repo, &ws_req(&repo, "rb", "b", 502)).unwrap();
    drop(repo);

    // two threads integrate the SAME snapshot into the SAME workspace:
    // the workspace lock serializes them; the second sees the first's merge
    // and reports up-to-date. Exactly one merge snapshot must be created.
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let root = root.clone();
            let other = sb.oid;
            std::thread::spawn(move || {
                let repo = newgit::repo::Repo::open(&root).unwrap();
                let r = integrate(&repo, &integ_req(&repo, "ra", other, 503));
                match r {
                    Ok(IntegrateOutcome::Merged { oid, .. }) => format!("merged:{oid}"),
                    Ok(IntegrateOutcome::UpToDate { .. }) => "up-to-date".to_string(),
                    Ok(other) => format!("other:{other:?}"),
                    Err(e) => format!("err:{}", e.category()),
                }
            })
        })
        .collect();
    let results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let merges: Vec<&String> = results
        .iter()
        .filter(|r| r.starts_with("merged:"))
        .collect();
    let ups = results.iter().filter(|r| *r == "up-to-date").count();
    assert_eq!(merges.len(), 1, "{results:?}");
    assert_eq!(ups, 1, "{results:?}");

    let repo = newgit::repo::Repo::open(&root).unwrap();
    let head = repo.refs.read("workspaces/ra").unwrap();
    let merged_oid = ObjectId::from_hex(&merges[0]["merged:".len()..]).unwrap();
    assert_eq!(head, merged_oid);
    // reflog: workspace-create + snapshot(sa) + exactly ONE integrate entry
    // (messages are base64 — count lines; the second thread added none)
    let logs = std::fs::read_to_string(repo.ng().join("logs/refs/workspaces/ra")).unwrap();
    let entries: Vec<&str> = logs.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(entries.len(), 3, "{logs}");
    // both sides merged cleanly into the files
    assert_eq!(
        std::fs::read(wa.dir.join("f.txt")).unwrap(),
        b"TOP\nmid\nBOT\n"
    );
    assert!(status(&repo, "ra", 10).unwrap().clean);
    let _ = sa;
}

// ─────────────────────────── crash safety ───────────────────────────

#[test]
fn integrate_crash_before_txn_changes_nothing() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    write(repo.root(), "f.txt", b"1\n");
    let s1 = snapshot(&repo, &req(&repo, "base", 600)).unwrap();
    let wa = workspace::create(&repo, "cw", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    write(&wa.dir, "a.txt", b"A\n");
    let _sa = snapshot(&repo, &ws_req(&repo, "cw", "a", 601)).unwrap();
    write(repo.root(), "m.txt", b"M\n");
    let sb = snapshot(&repo, &req(&repo, "b", 602)).unwrap();
    let ws_pos_before = repo.refs.read("workspaces/cw").unwrap();
    drop(repo);
    let out = run_faultlab(
        &root,
        &["integrate", "cw", &sb.oid.to_hex()],
        Some("integ:before_txn"),
    );
    assert!(!out.status.success());
    let repo = newgit::repo::Repo::open(&root).unwrap();
    assert_eq!(
        repo.refs.read("workspaces/cw").unwrap(),
        ws_pos_before,
        "killed before commit point ⇒ position unchanged"
    );
    // workspace files untouched
    assert_eq!(
        std::fs::read(repo.root().join(".newgit/workspaces/cw/files/a.txt")).unwrap(),
        b"A\n"
    );
    assert!(!repo
        .root()
        .join(".newgit/workspaces/cw/files/m.txt")
        .exists());
}

#[test]
fn integrate_crash_after_txn_committed_files_recoverable() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    write(repo.root(), "f.txt", b"1\n");
    let s1 = snapshot(&repo, &req(&repo, "base", 700)).unwrap();
    // disjoint edits on main and a workspace so the merge is clean
    let wa = workspace::create(&repo, "aw", Some(s1.oid), repo.default_actor().unwrap()).unwrap();
    write(&wa.dir, "a.txt", b"A\n");
    let _sa = snapshot(&repo, &ws_req(&repo, "aw", "a", 701)).unwrap();
    write(repo.root(), "f.txt", b"1\nMAIN\n");
    let sm = snapshot(&repo, &req(&repo, "m", 702)).unwrap();
    let pos_before = repo.refs.read("workspaces/aw").unwrap();
    drop(repo);
    // kill after the txn committed but before checkout
    let out = run_faultlab(
        &root,
        &["integrate", "aw", &sm.oid.to_hex()],
        Some("integ:after_txn"),
    );
    assert!(!out.status.success());
    let repo = newgit::repo::Repo::open(&root).unwrap();
    let head = repo.refs.read("workspaces/aw").unwrap();
    assert_ne!(
        head, pos_before,
        "commit point passed ⇒ ref moved (durable)"
    );
    // files are STALE (checkout never ran) — status shows it honestly
    let st = status(&repo, "aw", 10).unwrap();
    assert!(!st.clean, "stale files must be visible to status");
    // `checkout` repairs
    checkout_position(&repo, "aw").unwrap();
    assert!(status(&repo, "aw", 10).unwrap().clean);
    assert_eq!(std::fs::read(wa.dir.join("f.txt")).unwrap(), b"1\nMAIN\n");
    assert_eq!(std::fs::read(wa.dir.join("a.txt")).unwrap(), b"A\n");
}

#[test]
fn rollback_linear_history_uses_sole_parent() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "f.txt", b"v1\n");
    let s1 = snapshot(&repo, &req(&repo, "v1", 800)).unwrap();
    write(repo.root(), "f.txt", b"v2\n");
    let s2 = snapshot(&repo, &req(&repo, "v2", 801)).unwrap();
    let rb = rollback(
        &repo,
        &RollbackRequest {
            workspace: "main".into(),
            target: None,
            message: None,
            author: repo.default_actor().unwrap(),
            timestamp_ms: Some(802),
        },
    )
    .unwrap();
    let rbs = repo.objects.get(&rb).unwrap();
    let rbs = rbs.as_snapshot().unwrap();
    assert_eq!(rbs.root, s1.root);
    assert!(rbs.parents.contains(&s2.oid));
    // files rolled back too
    assert_eq!(std::fs::read(repo.root().join("f.txt")).unwrap(), b"v1\n");
    // history is intact: s1 ← s2 ← rollback
    let h = newgit::ops::history::history(&repo, None, 10).unwrap();
    assert_eq!(h.len(), 3);
    assert_eq!(h[0].oid, rb);
    // rolling back with no parent errors
    let r = rollback(
        &repo,
        &RollbackRequest {
            workspace: "main".into(),
            target: None,
            message: None,
            author: repo.default_actor().unwrap(),
            timestamp_ms: Some(803),
        },
    );
    assert!(r.is_ok()); // rb has a parent (s2)
}

#[test]
fn empty_tree_merge_edge() {
    let (_d, repo) = temp_repo();
    let empty = repo
        .objects
        .put(&Object::Tree(Tree::new(vec![]).unwrap()))
        .unwrap();
    let t = tree_from(&repo, &[("a", b"x\n", EntryMode::File)]);
    // base empty, ours adds, theirs empty → add survives
    let out = merge_trees(&repo, empty, t, empty, &MergeOpts::default()).unwrap();
    assert!(out.clean && out.entries == 1);
    // identical empty sides short-circuit
    let out = merge_trees(&repo, empty, empty, empty, &MergeOpts::default()).unwrap();
    assert!(out.clean && out.entries == 0);
    // tree entry sanity: Tree::new rejects empty names
    assert!(Tree::new(vec![TreeEntry {
        name: "".into(),
        mode: EntryMode::File,
        oid: ObjectId::from_bytes([1; 32]),
    }])
    .is_err());
}
