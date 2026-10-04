//! Chaos suite: randomized crash-injection over real repository operations.
//!
//! For a fixed set of seeds, a pseudo-random sequence of operations
//! (snapshot, workspace create, integrate, blob put, ref set) is executed
//! in child processes, each possibly killed at a random fault point
//! (`NEWGIT_FAULT_MODE=abort` = power loss at the worst moment).
//!
//! After EVERY step — killed or not — the repository must satisfy:
//! 1. `Repo::open` auto-recovers (no manual repair, no panic),
//! 2. deep `verify` reports ZERO errors (crash debris may only ever be
//!    warnings: orphan workspace dirs, stale locks, quarantined objects),
//! 3. every ref value resolves to a readable object,
//! 4. `status` computes for main and every live workspace.
//!
//! At the end of each seed: `gc --force-now` must clean debris without
//! breaking anything — history walk from refs/main and all pooled oids
//! must still resolve afterwards.
//!
//! Determinism: the RNG is a seeded xorshift64*; a failure prints the seed
//! and the full step log so the sequence can be replayed.

mod common;

use std::path::Path;
use std::process::Output;

use common::*;
use newgit::object::types::Object;
use newgit::object::ObjectId;
use newgit::ops::gc::{gc, GcOpts};
use newgit::ops::status::status;
use newgit::ops::verify::{verify, VerifyOpts};
use newgit::repo::Repo;

// ─────────────────────────── rng ───────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ─────────────────────────── fault menus ───────────────────────────

/// "" = no fault (the operation completes).
const SNAP_FAULTS: &[&str] = &[
    "",
    "snap:before_index_save",
    "snap:after_tree",
    "snap:before_txn",
    "snap:after_txn",
    "txn:before_journal",
    "txn:after_journal",
    "txn:before_complete",
    "txn:apply#1",
    "txn:apply#2",
    "txn:ref_write_err",
];
const WS_FAULTS: &[&str] = &[
    "",
    "txn:before_journal",
    "txn:after_journal",
    "txn:apply#1",
    "txn:apply#2",
    "txn:apply#3",
    "txn:before_complete",
    "txn:ref_write_err",
    "txn:file_write_err",
];
const INTEG_FAULTS: &[&str] = &[
    "",
    "integ:before_txn",
    "integ:after_txn",
    "integ:after_checkout",
    "txn:after_journal",
    "txn:apply#1",
    "txn:before_complete",
];
const BLOB_FAULTS: &[&str] = &[
    "",
    "ostore:before_write",
    "ostore:after_tmp_write",
    "ostore:before_rename",
    "ostore:after_rename",
];

// ─────────────────────────── helpers ───────────────────────────

fn stdout_of(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn parse_ok_oid(o: &Output) -> Option<ObjectId> {
    let s = stdout_of(o);
    s.strip_prefix("OK ")
        .and_then(|r| r.split_whitespace().next())
        .and_then(|h| ObjectId::from_hex(h).ok())
}

fn write(dir: &Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

fn ws_files_dir(root: &Path, ws: &str) -> std::path::PathBuf {
    root.join(".newgit")
        .join("workspaces")
        .join(ws)
        .join("files")
}

/// All invariants that must hold after every step (killed or not).
fn assert_repo_healthy(root: &Path, ctx: &str, live_ws: &[String]) {
    // (1) open auto-recovers
    let repo = match Repo::open(root) {
        Ok(r) => r,
        Err(e) => panic!("{ctx}: Repo::open failed after crash: {e}"),
    };
    // (2) deep verify: zero errors
    let rep = verify(&repo, &VerifyOpts { deep: true });
    if !rep.ok() {
        let errs: Vec<String> = rep
            .issues
            .iter()
            .map(|i| format!("[{:?}] {} — {}", i.severity, i.code, i.detail))
            .collect();
        panic!(
            "{ctx}: verify found {} error(s):\n{}",
            rep.errors(),
            errs.join("\n")
        );
    }
    // (3) every ref value resolves
    let mut names = Vec::new();
    newgit::ops::verify::collect_ref_files(&repo.ng().join("refs"), "", &mut names);
    for name in &names {
        if let Ok(Some(oid)) = newgit::repo::txn::read_ref_raw(repo.ng(), name) {
            assert!(
                repo.objects.get(&oid).is_ok(),
                "{ctx}: ref {name} → {oid} unreadable after crash"
            );
        }
    }
    // (4) status computes everywhere
    assert!(
        status(&repo, "main", 100).is_ok(),
        "{ctx}: status(main) failed"
    );
    for ws in live_ws {
        assert!(status(&repo, ws, 100).is_ok(), "{ctx}: status({ws}) failed");
    }
}

fn run_seed(seed: u64, steps: usize) {
    let mut rng = Rng(seed | 1); // xorshift must not start at 0
    let (d, repo) = temp_repo();
    let root = repo.root().to_path_buf();
    drop(repo);

    let mut log: Vec<String> = Vec::new();
    let mut pool: Vec<ObjectId> = Vec::new(); // known snapshot oids
    let mut live_ws: Vec<String> = Vec::new();
    let mut ws_counter = 0;

    // bootstrap: one clean snapshot so refs/main exists
    write(&root, "base.txt", b"base content");
    let out = run_faultlab(&root, &["snapshot", "bootstrap"], None);
    assert!(
        out.status.success(),
        "bootstrap failed: {}",
        stdout_of(&out)
    );
    if let Some(oid) = parse_ok_oid(&out) {
        pool.push(oid);
    }
    log.push("bootstrap snapshot".into());

    for step in 0..steps {
        // choose an operation
        let op = rng.below(10);
        match op {
            0..=2 => {
                // snapshot on main (fresh or edited file)
                let f = format!("f{}.txt", rng.below(6));
                write(
                    &root,
                    &f,
                    format!("step {step} seed {seed} r{}", rng.next()).as_bytes(),
                );
                let fault = *rng.pick(SNAP_FAULTS);
                let faults = if fault.is_empty() { None } else { Some(fault) };
                let out = run_faultlab(&root, &["snapshot", &format!("s{step}")], faults);
                log.push(format!(
                    "snapshot main {f} fault={fault:?} ok={}",
                    out.status.success()
                ));
                if let Some(oid) = parse_ok_oid(&out) {
                    pool.push(oid);
                }
            }
            3 => {
                // create a workspace
                ws_counter += 1;
                let name = format!("ws{ws_counter}");
                let fault = *rng.pick(WS_FAULTS);
                let faults = if fault.is_empty() { None } else { Some(fault) };
                let out = run_faultlab(&root, &["workspace-create", &name], faults);
                let ok = out.status.success();
                log.push(format!("workspace-create {name} fault={fault:?} ok={ok}"));
                if ok {
                    live_ws.push(name);
                }
            }
            4..=5 => {
                // edit + snapshot inside a live workspace
                if live_ws.is_empty() {
                    log.push("skip ws-snapshot (no ws)".into());
                    continue;
                }
                let ws = rng.pick(&live_ws.clone()).clone();
                let f = format!("g{}.txt", rng.below(4));
                write(
                    &ws_files_dir(&root, &ws),
                    &f,
                    format!("ws {ws} step {step} r{}", rng.next()).as_bytes(),
                );
                let fault = *rng.pick(SNAP_FAULTS);
                let faults = if fault.is_empty() { None } else { Some(fault) };
                let out = run_faultlab(&root, &["snapshot", &format!("w{step}"), &ws], faults);
                log.push(format!(
                    "snapshot {ws} {f} fault={fault:?} ok={}",
                    out.status.success()
                ));
                if let Some(oid) = parse_ok_oid(&out) {
                    pool.push(oid);
                }
            }
            6..=7 => {
                // integrate a known snapshot into main or a workspace
                if pool.is_empty() {
                    log.push("skip integrate (empty pool)".into());
                    continue;
                }
                let target = pool[rng.below(pool.len())];
                let into = if live_ws.is_empty() || rng.below(2) == 0 {
                    "main".to_string()
                } else {
                    rng.pick(&live_ws.clone()).clone()
                };
                let fault = *rng.pick(INTEG_FAULTS);
                let faults = if fault.is_empty() { None } else { Some(fault) };
                let out = run_faultlab(&root, &["integrate", &into, &target.to_hex()], faults);
                log.push(format!(
                    "integrate {} into {into} fault={fault:?} exit={:?}",
                    &target.to_hex()[..8],
                    out.status.code()
                ));
            }
            8 => {
                // loose blob (possibly orphaned by crash)
                let fault = *rng.pick(BLOB_FAULTS);
                let faults = if fault.is_empty() { None } else { Some(fault) };
                let text = format!("blob {step} r{}", rng.next());
                let out = run_faultlab(&root, &["put-blob", &text], faults);
                log.push(format!(
                    "put-blob fault={fault:?} ok={}",
                    out.status.success()
                ));
            }
            _ => {
                // ref update via txn engine (tag-like side ref)
                if pool.is_empty() {
                    log.push("skip txn-set (empty pool)".into());
                    continue;
                }
                let target = pool[rng.below(pool.len())];
                let name = format!("refs/t{}", rng.below(3));
                let fault = *rng.pick(WS_FAULTS);
                let faults = if fault.is_empty() { None } else { Some(fault) };
                let out = run_faultlab(&root, &["txn-set", &name, &target.to_hex()], faults);
                log.push(format!(
                    "txn-set {name} fault={fault:?} exit={:?}",
                    out.status.code()
                ));
            }
        }
        // ── invariants after EVERY step ──
        let ctx = format!("seed {seed:#x} step {step}\n  log: {log:?}");
        assert_repo_healthy(&root, &ctx, &live_ws);
        // refresh pool with everything refs can see (merge results etc.)
        let repo = Repo::open(&root).unwrap();
        let mut names = Vec::new();
        newgit::ops::verify::collect_ref_files(&repo.ng().join("refs"), "", &mut names);
        for n in &names {
            if let Ok(Some(o)) = newgit::repo::txn::read_ref_raw(repo.ng(), n) {
                if !pool.contains(&o) {
                    pool.push(o);
                }
            }
        }
    }

    // ── end of seed: gc must clean debris and break nothing ──
    let repo = Repo::open(&root).unwrap();
    let before = count_objects(&root);
    let rep = gc(
        &repo,
        &GcOpts {
            dry_run: false,
            force_now: true,
        },
    )
    .unwrap();
    let ctx = format!("seed {seed:#x} final gc\n  log: {log:?}");
    assert_eq!(rep.missing_links, 0, "{ctx}: gc saw unreadable links");
    assert_eq!(rep.kept_corrupt, 0, "{ctx}: gc saw corrupt objects");
    assert!(count_objects(&root) <= before, "{ctx}");

    // verify still clean after gc
    let v = verify(&repo, &VerifyOpts { deep: true });
    assert!(v.ok(), "{ctx}: verify after gc: {:?}", v.issues);

    // every pooled oid + full history from refs/main survives gc
    for o in &pool {
        assert!(
            repo.objects.get(o).is_ok(),
            "{ctx}: pooled {o} collected by gc"
        );
    }
    if let Ok(Some(head)) = newgit::repo::txn::read_ref_raw(repo.ng(), "refs/main") {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![head];
        while let Some(s) = stack.pop() {
            if !seen.insert(s) {
                continue;
            }
            match repo.objects.get(&s) {
                Ok(Object::Snapshot(sn)) => {
                    assert!(repo.objects.get(&sn.root).is_ok(), "{ctx}: tree collected");
                    stack.extend(sn.parents.iter().copied());
                }
                Ok(_) => panic!("{ctx}: refs/main is not a snapshot"),
                Err(e) => panic!("{ctx}: history broken at {s}: {e}"),
            }
            assert!(seen.len() < 5000, "{ctx}: history walk exploded");
        }
    }
    // and the repo remains fully usable
    let author = repo.default_actor().unwrap();
    write(&root, "after-gc.txt", b"still works");
    let req = newgit::ops::snapshot::SnapshotRequest {
        workspace: "main".into(),
        message: "after gc".into(),
        author,
        timestamp_ms: Some(1_700_000_999_000),
        tz_offset_min: 0,
        goal: None,
        change: None,
        extras: Default::default(),
    };
    let out = newgit::ops::snapshot::snapshot(&repo, &req).unwrap();
    assert!(repo.objects.get(&out.oid).is_ok());
    assert!(verify(&repo, &VerifyOpts { deep: true }).ok());
    drop(d);
}

fn count_objects(root: &Path) -> usize {
    let dir = root.join(".newgit").join("objects");
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for shard in rd.flatten() {
            if let Ok(rd2) = std::fs::read_dir(shard.path()) {
                n += rd2.flatten().count();
            }
        }
    }
    n
}

// ─────────────────────────── seeds ───────────────────────────

#[test]
fn chaos_seed_1() {
    run_seed(0x5EED_0001, 14);
}
#[test]
fn chaos_seed_2() {
    run_seed(0x5EED_0002, 14);
}
#[test]
fn chaos_seed_3() {
    run_seed(0xC0FF_EE03, 14);
}
#[test]
fn chaos_seed_4() {
    run_seed(0xDEAD_BEEF, 14);
}
#[test]
fn chaos_seed_5() {
    run_seed(0x1234_5678, 14);
}
#[test]
fn chaos_seed_6_long() {
    run_seed(0x0BAD_F00D, 25);
}
