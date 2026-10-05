//! Concurrency/race tests for refs and transactions (THREAT_MODEL §C).

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use newgit::object::ObjectId;
use newgit::repo::txn::{Cas, RefLogEntry};

fn counter_oid(round: u64) -> ObjectId {
    let mut b = [0u8; 32];
    b[..8].copy_from_slice(&round.to_be_bytes());
    ObjectId::from_bytes(b)
}

#[test]
fn cas_races_exactly_one_winner_per_version() {
    let (_d, repo) = temp_repo();
    let ng = repo.ng().to_path_buf();
    let limits = repo.limits().clone();
    const THREADS: u64 = 8;
    const ATTEMPTS: u64 = 40;

    let wins = Arc::new(AtomicU64::new(0));
    let cas_failures = Arc::new(AtomicU64::new(0));
    let mut handles = vec![];
    for t in 0..THREADS {
        let ng = ng.clone();
        let limits = limits.clone();
        let wins = wins.clone();
        let cas_failures = cas_failures.clone();
        handles.push(std::thread::spawn(move || {
            let store = newgit::repo::refs::RefStore::new(ng, limits);
            for a in 0..ATTEMPTS {
                let current = store.read_opt("races/counter").unwrap();
                let next = counter_oid(t * 10_000 + a);
                let r = store.update(
                    "races/counter",
                    Cas::Exactly(current),
                    Some(next),
                    RefLogEntry::system(format!("t{t}a{a}")),
                );
                match r {
                    Ok(_) => {
                        wins.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(newgit::Error::CasFailed(_)) => {
                        cas_failures.fetch_add(1, Ordering::Relaxed);
                    }
                    // LockBusy is legal under contention but must be rare and
                    // retryable; anything else is a bug.
                    Err(newgit::Error::LockBusy(_)) => {}
                    Err(e) => panic!("unexpected error: {e:?}"),
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let w = wins.load(Ordering::Relaxed);
    assert!(w >= THREADS, "expected real progress, got {w} wins");
    // reflog length must equal the number of successful updates exactly
    let repo2 = newgit::repo::Repo::open(ng.parent().unwrap()).unwrap();
    let log = repo2.refs.reflog("races/counter").unwrap();
    assert_eq!(
        log.len() as u64,
        w,
        "reflog must contain exactly one entry per win"
    );
    // final ref value must be one of the successful writes and parse cleanly
    let final_oid = repo2.refs.read("races/counter").unwrap();
    assert_eq!(log.last().unwrap().new, Some(final_oid));
    println!(
        "cas_races: wins={w} cas_failures={} (of {} attempts)",
        cas_failures.load(Ordering::Relaxed),
        THREADS * ATTEMPTS
    );
}

#[test]
fn parallel_multiref_transactions_all_succeed() {
    let (_d, repo) = temp_repo();
    let ng = repo.ng().to_path_buf();
    let limits = repo.limits().clone();
    let mut handles = vec![];
    for t in 0..4u64 {
        let ng = ng.clone();
        let limits = limits.clone();
        handles.push(std::thread::spawn(move || {
            let store = newgit::repo::refs::RefStore::new(ng.clone(), limits.clone());
            for a in 0..10u64 {
                let oid = counter_oid(t * 100 + a);
                store
                    .update(
                        &format!("t{t}/r{a}"),
                        Cas::Any,
                        Some(oid),
                        RefLogEntry::system("bulk"),
                    )
                    .unwrap();
                // two-ref atomic txn on shared namespace
                newgit::repo::txn::execute(
                    &ng,
                    vec![
                        newgit::repo::txn::TxnOp::Ref {
                            name: format!("shared/a{t}"),
                            cas: Cas::Any,
                            new: Some(oid),
                            log: RefLogEntry::system("shared"),
                        },
                        newgit::repo::txn::TxnOp::Ref {
                            name: format!("shared/b{t}"),
                            cas: Cas::Any,
                            new: Some(oid),
                            log: RefLogEntry::system("shared"),
                        },
                    ],
                    &limits,
                )
                .unwrap();
            }
            drop(store);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let all = repo.refs.list(None).unwrap();
    // 4 threads × 10 single refs + 4 × 2 shared refs = 48
    assert_eq!(all.len(), 48, "got {}", all.len());
    for (name, oid) in &all {
        assert_eq!(repo.refs.read(name).unwrap(), *oid);
    }
    // every paired shared ref is consistent (same txn wrote both)
    for t in 0..4u64 {
        assert_eq!(
            repo.refs.read(&format!("shared/a{t}")).unwrap(),
            repo.refs.read(&format!("shared/b{t}")).unwrap()
        );
    }
}

#[test]
fn concurrent_object_writes_are_safe() {
    let (_d, repo) = temp_repo();
    let objects_dir = repo.ng().join("objects");
    let limits = repo.limits().clone();
    let mut handles = vec![];
    for t in 0..4u64 {
        let objects_dir = objects_dir.clone();
        let limits = limits.clone();
        handles.push(std::thread::spawn(move || {
            let store = newgit::repo::ostore::ObjectStore::new(objects_dir, limits);
            for i in 0..50u64 {
                let data = format!("thread {t} blob {i}");
                let oid = store.put_blob(data.as_bytes()).unwrap();
                let got = store.get(&oid).unwrap();
                assert_eq!(got.as_blob().unwrap(), data.as_bytes());
                // also write the SAME content from every thread (idempotent put)
                let shared = store.put_blob(b"shared content").unwrap();
                assert!(store.contains(&shared));
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let all = repo.objects.iter().unwrap();
    assert_eq!(all.len(), 4 * 50 + 1);
    for oid in &all {
        // every stored object verifies
        repo.objects.get(oid).unwrap();
    }
}

#[test]
fn concurrent_recovery_and_writes() {
    // Writers hammer refs while another thread repeatedly opens the repo
    // (which runs recovery). No corruption, no lost updates, no panics.
    let (_d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    let ng = repo.ng().to_path_buf();
    let limits = repo.limits().clone();
    drop(repo);
    let stop = Arc::new(AtomicU64::new(0));
    let mut handles = vec![];
    for t in 0..3u64 {
        let ng = ng.clone();
        let limits = limits.clone();
        let stop = stop.clone();
        handles.push(std::thread::spawn(move || {
            let store = newgit::repo::refs::RefStore::new(ng, limits);
            let mut a = 0u64;
            while stop.load(Ordering::Relaxed) == 0 && a < 60 {
                let _ = store.update(
                    &format!("hammer/h{t}"),
                    Cas::Any,
                    Some(counter_oid(a)),
                    RefLogEntry::system("hammer"),
                );
                a += 1;
            }
        }));
    }
    let root2 = root.clone();
    let stop2 = stop.clone();
    let opener = std::thread::spawn(move || {
        let mut n = 0;
        while n < 20 && (stop2.load(Ordering::Relaxed) == 0 || n == 0) {
            let r = match newgit::repo::Repo::open(&root2) {
                Ok(repo) => repo,
                Err(newgit::Error::LockBusy(_)) => {
                    // Lock acquisition is bounded and LockBusy is explicitly
                    // retryable; sustained writer contention is not corruption.
                    std::thread::yield_now();
                    continue;
                }
                Err(error) => panic!("repo open failed during concurrent writes: {error:?}"),
            };
            // every read must be consistent (valid oid or absent)
            let _ = r.refs.read_opt("hammer/h0").unwrap();
            n += 1;
        }
        n
    });
    for h in handles {
        h.join().unwrap();
    }
    stop.store(1, Ordering::Relaxed);
    let successful_opens = opener.join().unwrap();
    assert!(
        successful_opens > 0,
        "recovery reader must complete at least one consistent open"
    );
    let repo = newgit::repo::Repo::open(&root).unwrap();
    for t in 0..3u64 {
        let oid = repo.refs.read(&format!("hammer/h{t}")).unwrap();
        // final value must be the last written counter (Any-CAS ⇒ monotonic
        // overwrite; at minimum it must parse and be present)
        assert_eq!(oid, counter_oid(59));
    }
}
