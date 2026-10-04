//! Diff engine integration tests: tree diffs, rename detection, binary
//! handling, content diffs, determinism, and the worktree-vs-position flow.

mod common;

use std::collections::BTreeMap;

use common::*;
use newgit::diff::myers::{split_lines, Tag};
use newgit::diff::render::{file_header, render_content};
use newgit::diff::{diff_blob_content, diff_trees, resolve_tree, DiffOpts, FileDiffKind};
use newgit::object::types::{EntryMode, Object, Tree, TreeEntry};
use newgit::object::ObjectId;
use newgit::ops::snapshot::{capture_tree, snapshot, SnapshotRequest};

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

fn tree_of(repo: &newgit::repo::Repo, oid: ObjectId) -> ObjectId {
    let obj = repo.objects.get(&oid).unwrap();
    obj.as_snapshot().unwrap().root
}

#[test]
fn tree_diff_kinds() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "same.txt", b"unchanged\n");
    write(repo.root(), "mod.txt", b"line1\nline2\n");
    write(repo.root(), "gone.txt", b"delete me\n");
    write(repo.root(), "run.sh", b"echo 1\n");
    let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();

    write(repo.root(), "mod.txt", b"line1\nline2 changed\n");
    std::fs::remove_file(repo.root().join("gone.txt")).unwrap();
    write(repo.root(), "new.txt", b"added\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(repo.root().join("run.sh"))
            .unwrap()
            .permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(repo.root().join("run.sh"), p).unwrap();
    }
    let s2 = snapshot(&repo, &req(&repo, "s2", 2)).unwrap();

    let td = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
    let by_path: BTreeMap<&str, &newgit::diff::FileDiff> =
        td.files.iter().map(|f| (f.path.as_str(), f)).collect();
    assert!(
        !by_path.contains_key("same.txt"),
        "unchanged must not appear"
    );
    assert_eq!(by_path["mod.txt"].kind, FileDiffKind::Modified);
    assert_eq!(by_path["new.txt"].kind, FileDiffKind::Added);
    assert_eq!(by_path["gone.txt"].kind, FileDiffKind::Deleted);
    #[cfg(unix)]
    {
        let r = &by_path["run.sh"];
        assert_eq!(r.kind, FileDiffKind::Modified);
        assert_eq!(r.old_mode, Some(EntryMode::File));
        assert_eq!(r.new_mode, Some(EntryMode::Executable));
        assert_eq!(r.old_oid, r.new_oid, "mode-only change keeps content oid");
    }

    // identical trees → empty diff
    let td0 = diff_trees(&repo, s1.root, s1.root, &DiffOpts::default()).unwrap();
    assert!(td0.is_empty());

    // determinism
    let td_again = diff_trees(
        &repo,
        tree_of(&repo, s1.oid),
        tree_of(&repo, s2.oid),
        &DiffOpts::default(),
    )
    .unwrap();
    assert_eq!(td_again.files.len(), td.files.len());
}

#[test]
fn rename_detection_exact_and_similarity() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "old/name.txt", b"content stays identical\n");
    write(
        repo.root(),
        "src/util.rs",
        b"fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
    );
    let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();

    // pure rename (identical content) + rename-with-small-edit
    std::fs::create_dir_all(repo.root().join("new")).unwrap();
    std::fs::rename(
        repo.root().join("old/name.txt"),
        repo.root().join("new/name.txt"),
    )
    .unwrap();
    std::fs::remove_file(repo.root().join("src/util.rs")).unwrap();
    write(
        repo.root(),
        "src/helpers.rs",
        b"fn a() {}\nfn b() {}\nfn c() {}\nfn d() { /* tweaked */ }\n",
    );
    let s2 = snapshot(&repo, &req(&repo, "s2", 2)).unwrap();

    let td = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
    let renames: Vec<&newgit::diff::FileDiff> = td
        .files
        .iter()
        .filter(|f| f.kind == FileDiffKind::Renamed)
        .collect();
    assert_eq!(renames.len(), 2, "{td:#?}");
    let exact = renames.iter().find(|f| f.path == "new/name.txt").unwrap();
    assert_eq!(exact.similarity, Some(100));
    assert_eq!(exact.old_path.as_deref(), Some("old/name.txt"));
    let sim = renames.iter().find(|f| f.path == "src/helpers.rs").unwrap();
    assert_eq!(sim.old_path.as_deref(), Some("src/util.rs"));
    assert!(sim.similarity.unwrap() >= 50 && sim.similarity.unwrap() < 100);
    assert_eq!(td.rename_detection, "similarity");

    // with rename detection off: plain delete+add
    let opts = DiffOpts {
        detect_renames: false,
        ..Default::default()
    };
    let td = diff_trees(&repo, s1.root, s2.root, &opts).unwrap();
    assert!(td.files.iter().all(|f| f.kind != FileDiffKind::Renamed));
    assert!(td
        .files
        .iter()
        .any(|f| f.kind == FileDiffKind::Deleted && f.path == "old/name.txt"));
    assert_eq!(td.rename_detection, "off");
}

#[test]
fn binary_files_flagged_without_content() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "img.bin", b"\x89PNG\0\0fake");
    let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();
    write(repo.root(), "img.bin", b"\x89PNG\0\0fake2");
    let s2 = snapshot(&repo, &req(&repo, "s2", 2)).unwrap();
    let td = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
    assert_eq!(td.files.len(), 1);
    assert!(td.files[0].binary);
    let cd = diff_blob_content(
        &repo,
        td.files[0].old_oid,
        td.files[0].new_oid,
        &DiffOpts::default(),
    )
    .unwrap();
    assert!(cd.binary && cd.ops.is_none() && cd.ops2.is_empty());
    let rendered = render_content(&td.files[0], &cd, 3);
    assert!(rendered.contains("Binary files"));
}

#[test]
fn content_diff_and_unified_render() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "f.txt", b"one\ntwo\nthree\nfour\n");
    let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();
    write(repo.root(), "f.txt", b"one\nTWO\nthree\nfour\nfive\n");
    let s2 = snapshot(&repo, &req(&repo, "s2", 2)).unwrap();
    let td = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
    let f = &td.files[0];
    let cd = diff_blob_content(&repo, f.old_oid, f.new_oid, &DiffOpts::default()).unwrap();
    assert!(!cd.binary);
    assert!(cd.ops.is_some(), "within cap ⇒ exact ops");
    let out = format!("{}{}", file_header(f), render_content(f, &cd, 3));
    assert!(out.contains("diff --newgit a/f.txt b/f.txt"));
    assert!(out.contains("-two"));
    assert!(out.contains("+TWO"));
    assert!(out.contains("+five"));
    assert!(out.contains("@@"));
    // reconstruct invariant via ops
    let al: Vec<&[u8]> = split_lines(b"one\ntwo\nthree\nfour\n");
    let bl: Vec<&[u8]> = split_lines(b"one\nTWO\nthree\nfour\nfive\n");
    assert_eq!(
        newgit::diff::myers::reconstruct(&al, &bl, cd.ops.as_ref().unwrap()),
        bl
    );
}

#[test]
fn edit_distance_cap_falls_back_coarse() {
    let (_d, repo) = temp_repo();
    let a: Vec<u8> = (0..50)
        .flat_map(|i| format!("a{i}\n").into_bytes())
        .collect();
    let b: Vec<u8> = (0..50)
        .flat_map(|i| format!("b{i}\n").into_bytes())
        .collect();
    let oa = repo.objects.put_blob(&a).unwrap();
    let ob = repo.objects.put_blob(&b).unwrap();
    // distance = 100 > cap 10
    let opts = DiffOpts {
        max_edit_distance: 10,
        ..Default::default()
    };
    let cd = diff_blob_content(&repo, Some(oa), Some(ob), &opts).unwrap();
    assert!(cd.ops.is_none(), "cap exceeded ⇒ no exact ops");
    assert_eq!(cd.ops2.len(), 1);
    assert_eq!(cd.ops2[0].tag, Tag::Replace);
    assert_eq!(cd.ops2[0].a2, 50);
    assert_eq!(cd.ops2[0].b2, 50);
    // coarse output still reconstructs
    let al: Vec<&[u8]> = split_lines(&a);
    let bl: Vec<&[u8]> = split_lines(&b);
    assert_eq!(newgit::diff::myers::reconstruct(&al, &bl, &cd.ops2), bl);
}

#[test]
fn worktree_vs_position_diff() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "a.txt", b"1\n");
    write(repo.root(), "b.txt", b"2\n");
    let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();
    // live modifications without snapshotting
    write(repo.root(), "a.txt", b"1\nadded line\n");
    std::fs::remove_file(repo.root().join("b.txt")).unwrap();
    write(repo.root(), "c.txt", b"brand new\n");
    let cap = capture_tree(&repo, "main", false).unwrap();
    let td = diff_trees(&repo, s1.root, cap.root, &DiffOpts::default()).unwrap();
    let kinds: BTreeMap<&str, FileDiffKind> =
        td.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
    assert_eq!(kinds["a.txt"], FileDiffKind::Modified);
    assert_eq!(kinds["b.txt"], FileDiffKind::Deleted);
    assert_eq!(kinds["c.txt"], FileDiffKind::Added);
    // capture_tree(save_index=false) must NOT modify the index file
    let ip = newgit::repo::workspace::index_path(&repo, "main");
    let before = std::fs::read(&ip).unwrap_or_default();
    capture_tree(&repo, "main", false).unwrap();
    let after = std::fs::read(&ip).unwrap_or_default();
    assert_eq!(
        before, after,
        "read-only capture must not rewrite the index"
    );
    // resolve_tree accepts refs and snapshot oids
    assert_eq!(resolve_tree(&repo, "refs/main").unwrap(), s1.root);
    assert_eq!(resolve_tree(&repo, &s1.oid.to_hex()).unwrap(), s1.root);
    // ...and trees directly
    let entry = TreeEntry {
        name: "x".into(),
        mode: EntryMode::File,
        oid: repo.objects.put_blob(b"y").unwrap(),
    };
    let t = repo
        .objects
        .put(&Object::Tree(Tree::new(vec![entry]).unwrap()))
        .unwrap();
    assert_eq!(resolve_tree(&repo, &t.to_hex()).unwrap(), t);
}

#[test]
fn diff_is_deterministic_across_runs() {
    let (_d, repo) = temp_repo();
    write(repo.root(), "p/q.txt", b"one\ntwo\n");
    write(repo.root(), "z.txt", b"zz\n");
    let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();
    std::fs::remove_file(repo.root().join("p/q.txt")).unwrap();
    write(repo.root(), "p/r.txt", b"one\ntwo!\n");
    std::fs::remove_file(repo.root().join("z.txt")).unwrap();
    write(repo.root(), "y.txt", b"zz\n");
    let s2 = snapshot(&repo, &req(&repo, "s2", 2)).unwrap();
    let r1 = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
    let r2 = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
    let j1 = serde_json::to_string(&r1).unwrap();
    let j2 = serde_json::to_string(&r2).unwrap();
    assert_eq!(j1, j2, "diff output must be byte-identical across runs");
}

#[test]
fn symlink_diff_reports_target_change() {
    let (_d, repo) = temp_repo();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("target1", repo.root().join("lnk")).unwrap();
        let s1 = snapshot(&repo, &req(&repo, "s1", 1)).unwrap();
        std::fs::remove_file(repo.root().join("lnk")).unwrap();
        std::os::unix::fs::symlink("target2", repo.root().join("lnk")).unwrap();
        let s2 = snapshot(&repo, &req(&repo, "s2", 2)).unwrap();
        let td = diff_trees(&repo, s1.root, s2.root, &DiffOpts::default()).unwrap();
        assert_eq!(td.files.len(), 1);
        let f = &td.files[0];
        assert_eq!(f.kind, FileDiffKind::Modified);
        assert!(!f.binary);
        let cd = diff_blob_content(&repo, f.old_oid, f.new_oid, &DiffOpts::default()).unwrap();
        let out = render_content(f, &cd, 3);
        assert!(out.contains("-target1"));
        assert!(out.contains("+target2"));
    }
    let _ = &repo;
}
