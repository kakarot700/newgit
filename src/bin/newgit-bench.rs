//! newgit-bench — benchmark harness.
//!
//! No external benchmark framework (D-002: minimal deps): this binary runs
//! each workload N times with `Instant`, reports min/median/mean/max, and
//! prints a markdown table ready to paste into docs/BENCHMARKS.md.
//!
//! Run:  cargo run --release --bin newgit-bench
//!
//! Numbers are wall-clock on the machine that ran them — always record the
//! environment block (printed first) alongside results.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use newgit::diff::{diff_trees, DiffOpts};
use newgit::object::ObjectId;
use newgit::ops::gc::{gc, GcOpts};
use newgit::ops::history::history;
use newgit::ops::integrate::{integrate, IntegrateRequest};
use newgit::ops::snapshot::{snapshot, SnapshotRequest};
use newgit::ops::status::status;
use newgit::ops::verify::{verify, VerifyOpts};
use newgit::repo::Repo;

use std::collections::BTreeMap;

const BASE_TS: i64 = 1_700_000_000_000;

fn main() {
    let base = std::env::temp_dir().join(format!("newgit-bench-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    print_env();
    println!();
    println!("| benchmark | iters | min ms | med ms | mean ms | max ms |");
    println!("|---|---|---|---|---|---|");

    bench_object_store(&base);
    bench_snapshots(&base);
    bench_status(&base);
    bench_diff(&base);
    bench_history(&base);
    bench_integrate(&base);
    bench_verify(&base);
    bench_gc(&base);

    let _ = std::fs::remove_dir_all(&base);
}

fn print_env() {
    println!("# newgit bench — environment");
    if let Ok(cpu) = std::fs::read_to_string("/proc/cpuinfo") {
        if let Some(model) = cpu.lines().find(|l| l.starts_with("model name")) {
            println!("cpu: {}", model.split(':').nth(1).unwrap_or("?").trim());
        }
    }
    if let Ok(mem) = std::fs::read_to_string("/proc/meminfo") {
        if let Some(t) = mem.lines().find(|l| l.starts_with("MemTotal")) {
            println!("mem: {}", t.split(':').nth(1).unwrap_or("?").trim());
        }
    }
    if let Ok(u) = std::process::Command::new("uname").arg("-sr").output() {
        println!("os: {}", String::from_utf8_lossy(&u.stdout).trim());
    }
    println!("newgit: {} (release, lto=thin)", env!("CARGO_PKG_VERSION"));
    println!("workdir: {}", std::env::temp_dir().display());
}

/// Run `f` `iters` times and print one table row.
fn bench(label: &str, iters: usize, mut f: impl FnMut(usize)) {
    assert!(iters > 0);
    let mut times: Vec<Duration> = Vec::with_capacity(iters);
    for i in 0..iters {
        let t = Instant::now();
        f(i);
        times.push(t.elapsed());
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let min = ms(*times.first().unwrap());
    let max = ms(*times.last().unwrap());
    let med = ms(times[times.len() / 2]);
    let mean = ms(times.iter().sum::<Duration>() / times.len() as u32);
    println!("| {label} | {iters} | {min:.2} | {med:.2} | {mean:.2} | {max:.2} |");
}

fn fresh_repo(dir: &Path, name: &str) -> (PathBuf, Repo) {
    let root = dir.join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    (root.clone(), Repo::init(&root).unwrap())
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

fn fill_files(dir: &Path, n: usize, size: usize, salt: u64) {
    for i in 0..n {
        let mut content = format!("file {i} salt {salt} ");
        while content.len() < size {
            content.push_str(&format!("pad-{}-", i * 7 + salt as usize % 13));
        }
        std::fs::write(dir.join(format!("f{i:05}.txt")), &content[..size]).unwrap();
    }
}

fn root_tree(repo: &Repo, oid: ObjectId) -> ObjectId {
    match repo.objects.get(&oid).unwrap() {
        newgit::object::types::Object::Snapshot(s) => s.root,
        _ => panic!("not a snapshot"),
    }
}

// ─────────────────────────── workloads ───────────────────────────

fn bench_object_store(base: &Path) {
    let (_, repo) = fresh_repo(base, "ostore");
    let mut payload = vec![0x61u8; 1024];
    bench("put_blob 1KiB (unique)", 2000, |i| {
        // unique content per iteration
        payload[..16].copy_from_slice(&i.to_le_bytes().repeat(2));
        let oid = repo.objects.put_blob(&payload).unwrap();
        assert_eq!(repo.objects.get(&oid).unwrap().type_tag().name(), "blob");
    });
}

fn bench_snapshots(base: &Path) {
    // cold: 1000 files × 200 B, fresh repo per iteration
    bench("snapshot 1000×200B cold", 3, |i| {
        let (root, repo) = fresh_repo(base, &format!("snap1k-{i}"));
        fill_files(&root, 1000, 200, i as u64);
        let author = repo.default_actor().unwrap();
        snapshot(&repo, &req("main", "cold", author, BASE_TS + i as i64)).unwrap();
    });
    // warm: unchanged re-snapshot (index reuse) on the last cold repo
    let (root, repo) = fresh_repo(base, "snap-warm");
    fill_files(&root, 1000, 200, 99);
    let author = repo.default_actor().unwrap();
    snapshot(&repo, &req("main", "first", author, BASE_TS)).unwrap();
    bench("snapshot 1000 files warm (no changes)", 5, |i| {
        snapshot(&repo, &req("main", "warm", author, BASE_TS + 10 + i as i64)).unwrap();
    });
    // cold 5000 files × 200 B
    bench("snapshot 5000×200B cold", 1, |_| {
        let (root, repo) = fresh_repo(base, "snap5k");
        fill_files(&root, 5000, 200, 7);
        let author = repo.default_actor().unwrap();
        snapshot(&repo, &req("main", "cold5k", author, BASE_TS)).unwrap();
    });
}

fn bench_status(base: &Path) {
    let (root, repo) = fresh_repo(base, "status");
    fill_files(&root, 1000, 200, 5);
    let author = repo.default_actor().unwrap();
    snapshot(&repo, &req("main", "s", author, BASE_TS)).unwrap();
    bench("status 1000 files clean (cached index)", 10, |_| {
        let st = status(&repo, "main", 2000).unwrap();
        assert!(st.clean);
    });
    bench("status 1000 files clean (index deleted)", 5, |_| {
        let _ = std::fs::remove_file(repo.ng().join("workspaces").join("main").join("index"));
        let st = status(&repo, "main", 2000).unwrap();
        assert!(st.clean);
    });
}

fn bench_diff(base: &Path) {
    let (root, repo) = fresh_repo(base, "diff");
    fill_files(&root, 1000, 200, 3);
    let author = repo.default_actor().unwrap();
    let a = snapshot(&repo, &req("main", "a", author, BASE_TS)).unwrap();
    // one file changed, one added, one renamed
    std::fs::write(root.join("f00007.txt"), b"totally different content now\n").unwrap();
    std::fs::write(root.join("new.txt"), b"brand new file\n").unwrap();
    std::fs::rename(root.join("f00042.txt"), root.join("moved42.txt")).unwrap();
    let b = snapshot(&repo, &req("main", "b", author, BASE_TS + 1)).unwrap();
    let (ra, rb) = (root_tree(&repo, a.oid), root_tree(&repo, b.oid));
    bench(
        "diff_trees 1000 files (1 edit, 1 add, 1 rename)",
        10,
        |_| {
            let d = diff_trees(&repo, ra, rb, &DiffOpts::default()).unwrap();
            assert!(!d.files.is_empty());
        },
    );
}

fn bench_history(base: &Path) {
    let (root, repo) = fresh_repo(base, "hist");
    std::fs::write(root.join("x.txt"), b"0").unwrap();
    let author = repo.default_actor().unwrap();
    for i in 0..500 {
        std::fs::write(root.join("x.txt"), format!("{i}").as_bytes()).unwrap();
        snapshot(
            &repo,
            &req("main", &format!("s{i}"), author, BASE_TS + i as i64),
        )
        .unwrap();
    }
    bench("history walk 500 snapshots (full)", 5, |_| {
        let h = history(&repo, None, 0).unwrap();
        assert_eq!(h.len(), 500);
    });
}

fn bench_integrate(base: &Path) {
    let (root, repo) = fresh_repo(base, "integ");
    fill_files(&root, 200, 200, 11);
    let author = repo.default_actor().unwrap();
    snapshot(&repo, &req("main", "base", author, BASE_TS)).unwrap();
    newgit::repo::workspace::create(&repo, "ws", None, author).unwrap();
    let ws_files = repo.ng().join("workspaces").join("ws").join("files");
    // setup + timed integrate per iteration: ws adds one unique file each
    // round ⇒ a real 3-way merge every time (base ≠ ours ≠ theirs).
    let mut targets: Vec<ObjectId> = Vec::new();
    for i in 0..10 {
        std::fs::write(
            ws_files.join(format!("ws-{i}.txt")),
            format!("ws {i}").as_bytes(),
        )
        .unwrap();
        let s = snapshot(
            &repo,
            &req("ws", &format!("w{i}"), author, BASE_TS + 100 + i as i64),
        )
        .unwrap();
        targets.push(s.oid);
    }
    bench("integrate 3-way merge (200 files + 1 new)", 10, |i| {
        let r = IntegrateRequest {
            workspace: "main".into(),
            other: targets[i],
            message: None,
            author,
            timestamp_ms: Some(BASE_TS + 1000 + i as i64),
            merge_opts: Default::default(),
        };
        integrate(&repo, &r).unwrap();
    });
}

fn bench_verify(base: &Path) {
    // reuse a 1000-file repo shape
    let (root, repo) = fresh_repo(base, "verify");
    fill_files(&root, 1000, 200, 13);
    let author = repo.default_actor().unwrap();
    snapshot(&repo, &req("main", "v", author, BASE_TS)).unwrap();
    bench("verify --deep (1000 files, ~1000 objects)", 3, |_| {
        let rep = verify(&repo, &VerifyOpts { deep: true });
        assert!(rep.ok(), "{:?}", rep.issues);
    });
}

fn bench_gc(base: &Path) {
    let (root, repo) = fresh_repo(base, "gc");
    fill_files(&root, 100, 200, 17);
    let author = repo.default_actor().unwrap();
    snapshot(&repo, &req("main", "g", author, BASE_TS)).unwrap();
    // 1000 orphan blobs (~25% overhead over the live set)
    let mut payload = vec![0x62u8; 512];
    for i in 0..1000u64 {
        payload[..8].copy_from_slice(&i.to_le_bytes());
        repo.objects.put_blob(&payload).unwrap();
    }
    bench("gc --force-now (1000 orphans / ~1100 objects)", 1, |_| {
        let rep = gc(
            &repo,
            &GcOpts {
                dry_run: false,
                force_now: true,
            },
        )
        .unwrap();
        assert_eq!(rep.deleted_objects, 1000, "{rep:?}");
    });
}
