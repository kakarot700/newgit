//! verify (fsck) + gc tests (TEST_MATRIX: integrity / maintenance).
//!
//! Invariants under test:
//! * verify detects every injected corruption class by stable code,
//! * verify NEVER modifies the repository,
//! * gc deletes exactly the unreachable objects, keeps reflog roots,
//!   chain history, and quarantine, honors dry-run and the grace window,
//! * after a crash leaves orphan objects, gc cleans them and verify passes.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use newgit::object::types::GoalStatus;
use newgit::object::ObjectId;
use newgit::ops::gc::{gc, GcOpts};
use newgit::ops::snapshot::{snapshot, SnapshotRequest};
use newgit::ops::verify::{verify, verify_ok, VerifyOpts};
use newgit::ops::workflow::{goal_create, goal_set_status};
use newgit::repo::Repo;
use newgit::util::base64;

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

fn write(dir: &Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

/// Storage path of an object: objects/<2 hex>/<62 hex>.
fn obj_path(repo: &Repo, oid: ObjectId) -> PathBuf {
    let hex = oid.to_hex();
    repo.ng().join("objects").join(&hex[..2]).join(&hex[2..])
}

fn deep(repo: &Repo) -> newgit::ops::verify::VerifyReport {
    verify(repo, &VerifyOpts { deep: true })
}

fn codes(repo: &Repo) -> Vec<String> {
    deep(repo).issues.iter().map(|i| i.code.clone()).collect()
}

fn seeded() -> (tempfile::TempDir, Repo, ObjectId) {
    let (d, repo) = temp_repo();
    let author = repo.default_actor().unwrap();
    write(repo.root(), "a.txt", b"hello");
    write(repo.root(), "dir/b.txt", b"world");
    let out = snapshot(&repo, &req("main", "first", author, 1000)).unwrap();
    (d, repo, out.oid)
}

// ─────────────────────────── verify: clean ───────────────────────────

#[test]
fn clean_repo_verifies_ok() {
    let (_d, repo, snap) = seeded();
    let rep = deep(&repo);
    assert!(rep.ok(), "issues: {:?}", rep.issues);
    assert!(rep.objects_checked >= 5);
    assert_eq!(rep.refs_checked, 1);
    assert_eq!(rep.workspaces_checked, 1);
    assert_eq!(rep.quarantined, 0);
    assert!(verify_ok(&repo).is_ok());
    // counts add up: every reachable object was checked
    let live = newgit::ops::verify::reachable(&repo, &[snap]).0;
    assert!(rep.objects_checked >= live.len());
}

// ─────────────────────────── verify: corruption ───────────────────────────

#[test]
fn corrupt_object_detected() {
    let (_d, repo, _) = seeded();
    // flip a byte in a blob's stored envelope
    let blob_oid = {
        // find any blob: hash the known content
        repo.objects.put_blob(b"hello").unwrap()
    };
    let p = obj_path(&repo, blob_oid);
    let mut bytes = std::fs::read(&p).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&p, &bytes).unwrap();

    let rep = deep(&repo);
    assert!(!rep.ok());
    assert!(codes(&repo).contains(&"object.corrupt".to_string()));
    assert!(verify_ok(&repo).is_err());
}

#[test]
fn truncated_object_detected() {
    let (_d, repo, _) = seeded();
    let blob_oid = repo.objects.put_blob(b"world").unwrap();
    let p = obj_path(&repo, blob_oid);
    let bytes = std::fs::read(&p).unwrap();
    std::fs::write(&p, &bytes[..10]).unwrap();
    assert!(codes(&repo).contains(&"object.corrupt".to_string()));
}

#[test]
fn misfiled_object_detected() {
    let (_d, repo, _) = seeded();
    let blob_oid = repo.objects.put_blob(b"hello").unwrap();
    let raw = std::fs::read(obj_path(&repo, blob_oid)).unwrap();
    // same bytes, different (valid) name → digest no longer matches path
    let bogus = oid(0xab);
    let p = obj_path(&repo, bogus);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, &raw).unwrap();
    let rep = deep(&repo);
    assert!(rep
        .issues
        .iter()
        .any(|i| i.code == "object.misfiled" && i.oid.as_deref() == Some(&bogus.to_hex())));
}

#[test]
fn bad_layout_detected() {
    let (_d, repo, _) = seeded();
    std::fs::write(repo.ng().join("objects").join("junk.txt"), b"x").unwrap();
    let shard = repo.ng().join("objects").join("zz");
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(shard.join("short"), b"x").unwrap();
    let c = codes(&repo);
    assert_eq!(
        c.iter().filter(|x| *x == "object.layout").count(),
        2,
        "{c:?}"
    );
}

#[test]
fn quarantined_object_is_warning_not_error() {
    let (_d, repo, _) = seeded();
    // quarantine files end in .corrupt and must never fail the repo
    let hex = oid(0xcd).to_hex();
    let shard = repo.ng().join("objects").join(&hex[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(shard.join(format!("{}.corrupt", &hex[2..])), b"junk").unwrap();
    let rep = deep(&repo);
    assert!(
        rep.ok(),
        "quarantine must not be an error: {:?}",
        rep.issues
    );
    assert_eq!(rep.quarantined, 1);
    assert!(rep
        .issues
        .iter()
        .any(|i| i.code == "object.quarantined"
            && i.severity == newgit::ops::verify::Severity::Warning));
}

#[test]
fn reflog_malformed_detected() {
    let (_d, repo, _) = seeded();
    let log = repo
        .ng()
        .join("logs")
        .join("refs")
        .join("refs")
        .join("main");
    assert!(log.exists(), "reflog should exist at {}", log.display());
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    std::io::Write::write_all(&mut f, b"garbage line\n").unwrap();
    assert!(codes(&repo).contains(&"reflog.line".to_string()));
}

#[test]
fn ref_target_missing_detected() {
    let (_d, repo, _) = seeded();
    // point refs/main at a syntactically valid, absent object
    let p = repo.ng().join("refs").join("refs").join("main");
    std::fs::write(&p, format!("{}\n", oid(0x99).to_hex())).unwrap();
    let c = codes(&repo);
    assert!(c.contains(&"ref.target_missing".to_string()), "{c:?}");
    // HEAD resolves through the ref → the snapshot chain is broken too
    assert!(verify_ok(&repo).is_err());
}

#[test]
fn chain_tampering_detected() {
    let (_d, repo, _) = seeded();
    let actor = repo.default_actor().unwrap();
    let g = goal_create(&repo, "goal", "", actor, Some(2000)).unwrap();
    goal_set_status(&repo, g, GoalStatus::InProgress, actor, Some(2001)).unwrap();
    assert!(deep(&repo).ok());

    let chain_ref = repo.ng().join("refs").join("chains").join(g.to_hex());
    // (a) head points at a missing object
    std::fs::write(&chain_ref, format!("{}\n", oid(0x77).to_hex())).unwrap();
    assert!(codes(&repo).contains(&"chain.version_missing".to_string()));

    // (b) head points at the wrong object type (a snapshot)
    let snap = repo.refs.read("refs/main").unwrap();
    std::fs::write(&chain_ref, format!("{}\n", snap.to_hex())).unwrap();
    assert!(codes(&repo).contains(&"chain.type".to_string()));
}

#[test]
fn workspace_debris_is_warning() {
    let (_d, repo, _) = seeded();
    // orphan workspace dir without meta (crash debris)
    let ws = repo.ng().join("workspaces").join("ghost");
    std::fs::create_dir_all(&ws).unwrap();
    let rep = deep(&repo);
    assert!(rep.ok(), "debris must not be an error: {:?}", rep.issues);
    assert!(rep.issues.iter().any(|i| i.code == "workspace.orphan_dir"
        && i.severity == newgit::ops::verify::Severity::Warning));
    // corrupt index cache is a warning too (it is only a cache)
    let (d2, repo2) = temp_repo();
    let _ = d2;
    let ws2 = repo2.ng().join("workspaces").join("main");
    std::fs::create_dir_all(&ws2).unwrap();
    std::fs::write(ws2.join("index"), b"junk").unwrap();
    let rep2 = deep(&repo2);
    assert!(rep2.ok(), "{:?}", rep2.issues);
    assert!(rep2
        .issues
        .iter()
        .any(|i| i.code == "workspace.index_corrupt"));
}

#[test]
fn verify_never_modifies_the_repository() {
    let (_d, repo, _) = seeded();
    let actor = repo.default_actor().unwrap();
    goal_create(&repo, "goal", "", actor, Some(2000)).unwrap();
    fn census(repo: &Repo) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![repo.ng().to_path_buf()];
        while let Some(p) = stack.pop() {
            for e in std::fs::read_dir(&p).unwrap().flatten() {
                if e.path().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push((e.path(), std::fs::read(e.path()).unwrap()));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
    let before = census(&repo);
    let rep = deep(&repo);
    assert!(rep.ok(), "{:?}", rep.issues);
    let after = census(&repo);
    assert_eq!(before, after, "verify must be strictly read-only");
}

// ─────────────────────────── gc ───────────────────────────

fn force() -> GcOpts {
    GcOpts {
        dry_run: false,
        force_now: true,
    }
}

#[test]
fn gc_removes_unreachable_keeps_reachable() {
    let (_d, repo, snap) = seeded();
    let orphan = repo.objects.put_blob(b"orphan payload").unwrap();
    assert!(obj_path(&repo, orphan).exists());

    let rep = gc(&repo, &force()).unwrap();
    assert_eq!(rep.deleted_objects, 1);
    assert_eq!(rep.deleted_oids, vec![orphan.to_hex()]);
    assert!(rep.freed_bytes > 0);
    assert!(!obj_path(&repo, orphan).exists());

    // everything reachable survived and still works
    assert!(repo.objects.get(&snap).is_ok());
    assert!(deep(&repo).ok());
    let st = newgit::ops::status::status(&repo, "main", 50).unwrap();
    assert!(st.clean);

    // second gc is a no-op
    let rep2 = gc(&repo, &force()).unwrap();
    assert_eq!(rep2.deleted_objects, 0);
}

#[test]
fn gc_dry_run_deletes_nothing() {
    let (_d, repo, _) = seeded();
    let orphan = repo.objects.put_blob(b"orphan").unwrap();
    let rep = gc(
        &repo,
        &GcOpts {
            dry_run: true,
            force_now: true,
        },
    )
    .unwrap();
    assert!(rep.dry_run);
    assert_eq!(rep.deleted_objects, 1);
    assert!(obj_path(&repo, orphan).exists(), "dry run must not delete");
}

#[test]
fn gc_grace_window_keeps_young_objects() {
    let (_d, repo, _) = seeded();
    let orphan = repo.objects.put_blob(b"fresh orphan").unwrap();
    // without --force-now the fresh object is protected
    let rep = gc(
        &repo,
        &GcOpts {
            dry_run: false,
            force_now: false,
        },
    )
    .unwrap();
    assert_eq!(rep.deleted_objects, 0);
    assert_eq!(rep.kept_young, 1);
    assert!(obj_path(&repo, orphan).exists());
    // with force-now it goes
    let rep = gc(&repo, &force()).unwrap();
    assert_eq!(rep.deleted_objects, 1);
    assert!(!obj_path(&repo, orphan).exists());
}

#[test]
fn gc_keeps_reflog_referenced_objects() {
    let (_d, repo, _) = seeded();
    let orphan = repo.objects.put_blob(b"audit trail").unwrap();
    // a valid reflog line whose NEW is the orphan ⇒ it is an audit root
    let log = repo
        .ng()
        .join("logs")
        .join("refs")
        .join("refs")
        .join("main");
    let line = format!(
        "ZERO {} 1700000000000 ZERO {} {}\n",
        orphan.to_hex(),
        base64::encode(b"txn-id-for-test"),
        base64::encode(b"synthetic audit line"),
    );
    let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    std::io::Write::write_all(&mut f, line.as_bytes()).unwrap();

    assert!(deep(&repo).ok(), "line must parse: {:?}", codes(&repo));
    let rep = gc(&repo, &force()).unwrap();
    assert_eq!(rep.deleted_objects, 0, "reflog NEW is a gc root");
    assert!(obj_path(&repo, orphan).exists());
}

#[test]
fn gc_preserves_chain_history() {
    let (_d, repo, _) = seeded();
    let actor = repo.default_actor().unwrap();
    let g = goal_create(&repo, "goal", "", actor, Some(2000)).unwrap();
    let v2 = goal_set_status(&repo, g, GoalStatus::InProgress, actor, Some(2001)).unwrap();
    let v3 = goal_set_status(&repo, g, GoalStatus::Achieved, actor, Some(2002)).unwrap();
    // plus an orphan that must die
    let orphan = repo.objects.put_blob(b"doomed").unwrap();

    let rep = gc(&repo, &force()).unwrap();
    assert_eq!(rep.deleted_objects, 1);
    assert_eq!(rep.deleted_oids, vec![orphan.to_hex()]);

    // every chain version survives (reachable via extras.prev)
    for v in [g, v2, v3] {
        assert!(repo.objects.get(&v).is_ok(), "chain version {v} collected!");
    }
    assert!(deep(&repo).ok());
}

#[test]
fn gc_never_touches_quarantine() {
    let (_d, repo, _) = seeded();
    let hex = oid(0xef).to_hex();
    let shard = repo.ng().join("objects").join(&hex[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    let q = shard.join(format!("{}.corrupt", &hex[2..]));
    std::fs::write(&q, b"forensic evidence").unwrap();

    let rep = gc(&repo, &force()).unwrap();
    assert_eq!(rep.quarantined, 1);
    assert_eq!(rep.deleted_objects, 0);
    assert!(q.exists(), "quarantine must survive gc");
}

#[test]
fn gc_cleans_crash_debris_then_verify_passes() {
    // A snapshot that aborts before its txn leaves tree/blob objects
    // behind with no ref pointing at them — classic crash debris.
    let (d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    write(&root, "a.txt", b"debris");
    drop(repo);
    let out = run_faultlab(&root, &["snapshot", "crash me"], Some("snap:before_txn"));
    assert!(!out.status.success(), "faultlab should abort");

    let repo = Repo::open(&root).unwrap();
    assert!(repo.refs.read_opt("refs/main").unwrap().is_none());
    let before = deep(&repo);
    assert!(
        before.ok(),
        "debris alone is not corruption: {:?}",
        before.issues
    );

    let rep = gc(&repo, &force()).unwrap();
    assert!(
        rep.deleted_objects >= 2,
        "expected tree+blob debris, got {:?}",
        rep
    );
    assert!(deep(&repo).ok());
    // repo still fully usable afterwards
    let author = repo.default_actor().unwrap();
    let out2 = snapshot(&repo, &req("main", "after gc", author, 5000)).unwrap();
    assert!(repo.objects.get(&out2.oid).is_ok());
    drop(d);
}

#[test]
fn gc_keeps_unreadable_objects_and_reports_missing_links() {
    // One object corrupted ⇒ mark cannot walk through it. gc must report
    // the damage (missing_links), keep the unreadable file for forensics,
    // and never fail outright.
    let (_d, repo, _) = seeded();
    let blob_oid = repo.objects.put_blob(b"world").unwrap();
    let p = obj_path(&repo, blob_oid);
    let mut bytes = std::fs::read(&p).unwrap();
    bytes[5] ^= 0xff;
    std::fs::write(&p, &bytes).unwrap();

    let rep = gc(&repo, &force()).unwrap();
    assert_eq!(rep.missing_links, 1);
    assert_eq!(rep.deleted_objects, 0);
    assert_eq!(rep.kept_corrupt, 1);
    assert!(p.exists(), "corrupt object must survive gc for forensics");
    // verify still reports the corruption
    assert!(codes(&repo).contains(&"object.corrupt".to_string()));
}

#[test]
fn temp_debris_is_warning_and_survives_gc() {
    let (_d, repo, _) = seeded();
    // simulate a killed put_blob: temp sibling left in a shard
    let hex = oid(0x11).to_hex();
    let shard = repo.ng().join("objects").join(&hex[..2]);
    std::fs::create_dir_all(&shard).unwrap();
    let debris = shard.join(format!("{}.tmp.4242.1700000000000000000.0", &hex[2..]));
    std::fs::write(&debris, b"half written envelope").unwrap();

    let rep = deep(&repo);
    assert!(rep.ok(), "temp debris must be a warning: {:?}", rep.issues);
    assert!(rep
        .issues
        .iter()
        .any(|i| i.code == "object.temp_debris"
            && i.severity == newgit::ops::verify::Severity::Warning));

    // gc never touches temp files (sweep_temp_files owns them, by age)
    let g = gc(&repo, &force()).unwrap();
    assert_eq!(g.deleted_objects, 0);
    assert!(debris.exists());

    // stale sweep removes it (age 0 ⇒ everything is stale)
    let swept = repo.objects.sweep_temp_files(0).unwrap();
    assert_eq!(swept, 1);
    assert!(!debris.exists());
    assert!(deep(&repo).ok());
}
