//! Transaction + crash-recovery integration tests (THREAT_MODEL §C).
//!
//! These tests kill a child process at exact fault points and assert the
//! recovery invariants:
//! * before the journal is durable → nothing happened;
//! * after the journal is durable → the transaction completes on recovery
//!   (commit point = journal fsync, forward recovery);
//! * mid-apply → recovery finishes the application (all-or-nothing);
//! * before COMPLETE marker → recovery marks it done; reflog dedupes;
//! * corrupt journals → quarantined, never applied, repo still usable.

mod common;

use common::*;
use newgit::repo::txn::{self, Cas, RefLogEntry};
use std::sync::{Arc, Barrier};

#[test]
fn ref_crud_cas_and_reflog() {
    let (_d, repo) = temp_repo();
    let name = "heads/feature";
    // create with Exactly(None)
    repo.refs
        .update(
            name,
            Cas::Exactly(None),
            Some(oid(1)),
            RefLogEntry::system("create"),
        )
        .unwrap();
    assert_eq!(repo.refs.read(name).unwrap(), oid(1));
    // second create fails CAS
    let r = repo.refs.update(
        name,
        Cas::Exactly(None),
        Some(oid(2)),
        RefLogEntry::system("dup"),
    );
    assert!(matches!(r, Err(newgit::Error::CasFailed(_))), "got {r:?}");
    assert_eq!(repo.refs.read(name).unwrap(), oid(1));
    // wrong expect fails
    let r = repo.refs.update(
        name,
        Cas::Exactly(Some(oid(9))),
        Some(oid(2)),
        RefLogEntry::system("bad-cas"),
    );
    assert!(matches!(r, Err(newgit::Error::CasFailed(_))));
    // right expect succeeds
    repo.refs
        .update(
            name,
            Cas::Exactly(Some(oid(1))),
            Some(oid(2)),
            RefLogEntry::system("move"),
        )
        .unwrap();
    assert_eq!(repo.refs.read(name).unwrap(), oid(2));
    // reflog recorded both successful ops
    let log = repo.refs.reflog(name).unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].message, "create");
    assert_eq!(log[1].message, "move");
    assert_eq!(log[1].new, Some(oid(2)));
    // list + prefix
    repo.refs
        .update("goals/g1", Cas::Any, Some(oid(3)), RefLogEntry::system("g"))
        .unwrap();
    let all = repo.refs.list(None).unwrap();
    assert_eq!(all.len(), 2);
    let goals = repo.refs.list(Some("goals")).unwrap();
    assert_eq!(goals, vec![("goals/g1".to_string(), oid(3))]);
    // delete with CAS
    repo.refs
        .update(
            name,
            Cas::Exactly(Some(oid(2))),
            None,
            RefLogEntry::system("delete"),
        )
        .unwrap();
    assert!(matches!(
        repo.refs.read(name),
        Err(newgit::Error::RefNotFound(_))
    ));
}

#[test]
fn two_ref_transaction_atomic_success() {
    let (_d, repo) = temp_repo();
    txn::execute(
        repo.ng(),
        vec![
            newgit::repo::txn::TxnOp::Ref {
                name: "a".into(),
                cas: Cas::Any,
                new: Some(oid(1)),
                log: RefLogEntry::system("a"),
            },
            newgit::repo::txn::TxnOp::Ref {
                name: "b".into(),
                cas: Cas::Any,
                new: Some(oid(2)),
                log: RefLogEntry::system("b"),
            },
        ],
        repo.limits(),
    )
    .unwrap();
    assert_eq!(repo.refs.read("a").unwrap(), oid(1));
    assert_eq!(repo.refs.read("b").unwrap(), oid(2));
}

#[test]
fn cas_precondition_failure_writes_nothing() {
    let (_d, repo) = temp_repo();
    repo.refs
        .update("x", Cas::Any, Some(oid(1)), RefLogEntry::system("init"))
        .unwrap();
    // second op in the txn has a failing CAS → first op must not be applied
    let r = txn::execute(
        repo.ng(),
        vec![
            newgit::repo::txn::TxnOp::Ref {
                name: "y".into(),
                cas: Cas::Any,
                new: Some(oid(5)),
                log: RefLogEntry::system("y"),
            },
            newgit::repo::txn::TxnOp::Ref {
                name: "x".into(),
                cas: Cas::Exactly(Some(oid(99))), // wrong
                new: Some(oid(6)),
                log: RefLogEntry::system("x"),
            },
        ],
        repo.limits(),
    );
    assert!(matches!(r, Err(newgit::Error::CasFailed(_))));
    assert_eq!(repo.refs.read("x").unwrap(), oid(1));
    assert!(repo.refs.read_opt("y").unwrap().is_none());
    // Checkpointing: successful txns delete their journals, and the failed
    // CAS txn never wrote one — the txn dir must be empty.
    let states = journal_states(&repo);
    assert!(states.is_empty(), "unexpected journals: {states:?}");
}

#[test]
fn precommit_object_staging_is_skipped_on_cas_failure() {
    let (_d, repo) = temp_repo();
    repo.refs
        .update("main", Cas::Any, Some(oid(1)), RefLogEntry::system("init"))
        .unwrap();
    let object = newgit::object::types::Object::Blob(b"staged only after CAS".to_vec());
    let object_id = object.id();
    let result = txn::execute_with_precommit(
        repo.ng(),
        vec![newgit::repo::txn::TxnOp::Ref {
            name: "main".into(),
            cas: Cas::Exactly(Some(oid(99))),
            new: Some(oid(2)),
            log: RefLogEntry::system("stale writer"),
        }],
        repo.limits(),
        || {
            repo.objects.put(&object)?;
            Ok(())
        },
    );
    assert!(matches!(result, Err(newgit::Error::CasFailed(_))));
    assert_eq!(repo.refs.read("main").unwrap(), oid(1));
    assert!(!repo.objects.contains(&object_id));
    assert!(journal_states(&repo).is_empty());
}

#[test]
fn racing_same_ref_pushes_promote_only_the_cas_winner() {
    let (_d, repo) = temp_repo();
    let repo = Arc::new(repo);
    let barrier = Arc::new(Barrier::new(2));
    let candidates = [
        newgit::object::types::Object::Blob(b"first writer".to_vec()),
        newgit::object::types::Object::Blob(b"second writer".to_vec()),
    ];
    let candidates: Vec<_> = candidates
        .into_iter()
        .map(|object| {
            let repo = Arc::clone(&repo);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let object_id = object.id();
                barrier.wait();
                let result = txn::execute_with_precommit(
                    repo.ng(),
                    vec![newgit::repo::txn::TxnOp::Ref {
                        name: "main".into(),
                        cas: Cas::Exactly(None),
                        new: Some(object_id),
                        log: RefLogEntry::system("concurrent Git push"),
                    }],
                    repo.limits(),
                    || {
                        repo.objects.put(&object)?;
                        Ok(())
                    },
                );
                (object_id, result)
            })
        })
        .collect();
    let outcomes: Vec<_> = candidates
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let winners: Vec<_> = outcomes
        .iter()
        .filter(|(_, result)| result.is_ok())
        .collect();
    let losers: Vec<_> = outcomes
        .iter()
        .filter(|(_, result)| matches!(result, Err(newgit::Error::CasFailed(_))))
        .collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(losers.len(), 1);
    let winner = winners[0].0;
    let loser = losers[0].0;
    assert_eq!(repo.refs.read("main").unwrap(), winner);
    assert!(repo.objects.contains(&winner));
    assert!(!repo.objects.contains(&loser));
}

#[test]
fn racing_multi_ref_pushes_publish_only_one_complete_ref_set() {
    let (_d, repo) = temp_repo();
    let main_old = oid(10);
    let side_old = oid(11);
    repo.refs
        .update(
            "main",
            Cas::Any,
            Some(main_old),
            RefLogEntry::system("init main"),
        )
        .unwrap();
    repo.refs
        .update(
            "side",
            Cas::Any,
            Some(side_old),
            RefLogEntry::system("init side"),
        )
        .unwrap();
    let repo = Arc::new(repo);
    let barrier = Arc::new(Barrier::new(2));
    let candidates = [
        (b"writer one main".to_vec(), b"writer one side".to_vec()),
        (b"writer two main".to_vec(), b"writer two side".to_vec()),
    ];
    let threads: Vec<_> = candidates
        .into_iter()
        .map(|(main_bytes, side_bytes)| {
            let repo = Arc::clone(&repo);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let main_object = newgit::object::types::Object::Blob(main_bytes);
                let side_object = newgit::object::types::Object::Blob(side_bytes);
                let main_oid = main_object.id();
                let side_oid = side_object.id();
                barrier.wait();
                let result = txn::execute_with_precommit(
                    repo.ng(),
                    vec![
                        newgit::repo::txn::TxnOp::Ref {
                            name: "main".into(),
                            cas: Cas::Exactly(Some(main_old)),
                            new: Some(main_oid),
                            log: RefLogEntry::system("concurrent multi-ref Git push"),
                        },
                        newgit::repo::txn::TxnOp::Ref {
                            name: "side".into(),
                            cas: Cas::Exactly(Some(side_old)),
                            new: Some(side_oid),
                            log: RefLogEntry::system("concurrent multi-ref Git push"),
                        },
                    ],
                    repo.limits(),
                    || {
                        repo.objects.put(&main_object)?;
                        repo.objects.put(&side_object)?;
                        Ok(())
                    },
                );
                (main_oid, side_oid, result)
            })
        })
        .collect();
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let winners: Vec<_> = outcomes
        .iter()
        .filter(|(_, _, result)| result.is_ok())
        .collect();
    let losers: Vec<_> = outcomes
        .iter()
        .filter(|(_, _, result)| matches!(result, Err(newgit::Error::CasFailed(_))))
        .collect();
    assert_eq!(winners.len(), 1);
    assert_eq!(losers.len(), 1);
    let (main_winner, side_winner, _) = winners[0];
    let (main_loser, side_loser, _) = losers[0];
    assert_eq!(repo.refs.read("main").unwrap(), *main_winner);
    assert_eq!(repo.refs.read("side").unwrap(), *side_winner);
    assert!(repo.objects.contains(main_winner));
    assert!(repo.objects.contains(side_winner));
    assert!(!repo.objects.contains(main_loser));
    assert!(!repo.objects.contains(side_loser));
}

#[test]
fn crash_before_journal_leaves_no_trace() {
    let (d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    drop(repo);
    let out = run_faultlab(
        &root,
        &["txn-two", "c1", &oid(1).to_hex(), "c2", &oid(2).to_hex()],
        Some("txn:before_journal"),
    );
    assert!(!out.status.success(), "child must die at fault point");
    let repo = newgit::repo::Repo::open(&root).unwrap();
    assert!(repo.refs.read_opt("c1").unwrap().is_none());
    assert!(repo.refs.read_opt("c2").unwrap().is_none());
    assert!(journal_states(&repo).is_empty());
    drop(repo);
    drop(d);
}

#[test]
fn crash_after_journal_commits_on_recovery() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    drop(repo);
    let out = run_faultlab(
        &root,
        &["txn-two", "r1", &oid(1).to_hex(), "r2", &oid(2).to_hex()],
        Some("txn:after_journal"),
    );
    assert!(!out.status.success());
    // Before recovery the refs may or may not exist (apply may not have
    // started). The journal must exist in RUNNING state.
    let ng = root.join(".newgit");
    let states: Vec<String> = std::fs::read_dir(ng.join("txn"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| {
            let t = std::fs::read_to_string(e.path()).unwrap_or_default();
            t.lines()
                .find(|l| l.starts_with("state="))
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        states.iter().any(|s| s == "state=RUNNING"),
        "expected a RUNNING journal, got {states:?}"
    );
    // Opening the repo recovers: commit point passed ⇒ forward-recover.
    let repo = newgit::repo::Repo::open(&root).unwrap();
    assert_eq!(repo.refs.read("r1").unwrap(), oid(1));
    assert_eq!(repo.refs.read("r2").unwrap(), oid(2));
    // recovery redoes the txn and checkpoint-deletes the journal
    let st = journal_states(&repo);
    assert!(st.is_empty(), "{st:?}");
    // reflog deduped even though redo re-appended
    assert_eq!(repo.refs.reflog("r1").unwrap().len(), 1);
    assert_eq!(repo.refs.reflog("r2").unwrap().len(), 1);
}

#[test]
fn crash_mid_apply_finishes_on_recovery() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    drop(repo);
    let out = run_faultlab(
        &root,
        &["txn-two", "m1", &oid(1).to_hex(), "m2", &oid(2).to_hex()],
        Some("txn:apply#1"),
    );
    assert!(!out.status.success());
    // partial state observable on disk: m1 applied, m2 missing
    let raw1 = txn::read_ref_raw(&root.join(".newgit"), "m1").unwrap();
    let raw2 = txn::read_ref_raw(&root.join(".newgit"), "m2").unwrap();
    assert_eq!(raw1, Some(oid(1)), "op0 should have applied");
    assert_eq!(raw2, None, "op1 should not have applied yet");
    // recovery completes the transaction
    let repo = newgit::repo::Repo::open(&root).unwrap();
    assert_eq!(repo.refs.read("m1").unwrap(), oid(1));
    assert_eq!(repo.refs.read("m2").unwrap(), oid(2));
}

#[test]
fn crash_before_complete_marker_recovers_and_dedupes_reflog() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    drop(repo);
    let out = run_faultlab(
        &root,
        &["txn-two", "k1", &oid(1).to_hex(), "k2", &oid(2).to_hex()],
        Some("txn:before_complete"),
    );
    assert!(!out.status.success());
    let repo = newgit::repo::Repo::open(&root).unwrap();
    assert_eq!(repo.refs.read("k1").unwrap(), oid(1));
    assert_eq!(repo.refs.read("k2").unwrap(), oid(2));
    // redo re-appended the reflog line; readers must dedupe by txn id
    assert_eq!(repo.refs.reflog("k1").unwrap().len(), 1);
    assert_eq!(repo.refs.reflog("k2").unwrap().len(), 1);
    let st = journal_states(&repo);
    assert!(st.is_empty(), "{st:?}");
    // repeated recovery is a no-op
    let (rep, _) = repo.recover().unwrap();
    assert!(rep.redone.is_empty());
}

#[test]
fn corrupt_journal_is_quarantined_never_applied() {
    let (_d, repo) = temp_repo();
    // garbage journal
    let j = repo.ng().join("txn/123-456-0.journal");
    std::fs::write(&j, b"total garbage \xff\xfe not a journal").unwrap();
    // well-formed journal with a FILE op whose digest does not match
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"something-else");
    let wrong_digest = newgit::util::hex::encode(&h.finalize());
    let j2 = repo.ng().join("txn/123-456-1.journal");
    let content = format!(
        "NEWGIT-TXN v1\ntxn=123-456-1\nstate=RUNNING\nFILE {} {} {}\nEND\n",
        newgit::util::base64::encode(b"HEAD"),
        wrong_digest,
        newgit::util::base64::encode(b"ref: refs/evil\n"),
    );
    std::fs::write(&j2, content).unwrap();

    let root = repo.root().to_path_buf();
    drop(repo);
    // open() runs recovery internally and must quarantine both journals
    let repo = newgit::repo::Repo::open(&root).unwrap();
    // HEAD untouched
    assert_eq!(
        repo.read_head().unwrap(),
        newgit::repo::Head::Symbolic(newgit::repo::DEFAULT_BRANCH.into())
    );
    // explicit recover() is now a no-op (already quarantined)
    let (rep, _) = repo.recover().unwrap();
    assert!(
        rep.redone.is_empty() && rep.quarantined.is_empty(),
        "{rep:?}"
    );
    // quarantined files exist and end with .corrupt
    let quarantined: Vec<String> = std::fs::read_dir(repo.ng().join("txn"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".corrupt"))
        .collect();
    assert_eq!(quarantined.len(), 2);
}

#[test]
fn journal_roundtrip_property_shape() {
    // serialize→parse identity for a rich journal
    let j = txn::Journal {
        id: "1-2-3".into(),
        state: txn::JournalState::Running,
        ops: vec![
            txn::TxnOp::Ref {
                name: "heads/x".into(),
                cas: Cas::Any,
                new: Some(oid(7)),
                log: RefLogEntry {
                    actor: Some(oid(8)),
                    ts_ms: 42,
                    message: "hello world with spaces".into(),
                },
            },
            txn::TxnOp::Ref {
                name: "gone".into(),
                cas: Cas::Any,
                new: None,
                log: RefLogEntry::system("delete me"),
            },
            txn::TxnOp::File {
                rel: "HEAD".into(),
                data: b"ref: refs/main\n".to_vec(),
            },
        ],
    };
    let text = j.serialize();
    let parsed = txn::Journal::parse(&text).unwrap();
    assert_eq!(parsed.id, j.id);
    assert_eq!(parsed.ops.len(), 3);
    match &parsed.ops[0] {
        txn::TxnOp::Ref { name, new, log, .. } => {
            assert_eq!(name, "heads/x");
            assert_eq!(*new, Some(oid(7)));
            assert_eq!(log.message, "hello world with spaces");
            assert_eq!(log.actor, Some(oid(8)));
            assert_eq!(log.ts_ms, 42);
        }
        _ => panic!("expected ref op"),
    }
    // truncated journals rejected
    assert!(txn::Journal::parse(&text[..text.len() - 5]).is_err());
    assert!(txn::Journal::parse("garbage").is_err());
}

#[test]
fn repeated_recovery_is_stable() {
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    drop(repo);
    let out = run_faultlab(
        &root,
        &["txn-two", "s1", &oid(1).to_hex(), "s2", &oid(2).to_hex()],
        Some("txn:after_journal"),
    );
    assert!(!out.status.success());
    for _ in 0..5 {
        let repo = newgit::repo::Repo::open(&root).unwrap();
        assert_eq!(repo.refs.read("s1").unwrap(), oid(1));
        assert_eq!(repo.refs.read("s2").unwrap(), oid(2));
        assert_eq!(repo.refs.reflog("s1").unwrap().len(), 1);
        drop(repo);
    }
}
