//! Manual real-Git transfer benchmark for the smart-HTTP adapter.
//!
//! Run with:
//! `cargo test --release --locked --lib remote::git_http::benchmark::live_git_transfer_baseline -- --ignored --nocapture`
//!
//! This is intentionally ignored by normal CI tests: it performs real transfer
//! workflows and reports machine-dependent measurements, not assertions about
//! performance. The counting proxy records exact HTTP response-body bytes.

use super::TempGitView;
use crate::object::types::{Actor, ActorKind, EntryMode, Object, Snapshot};
use crate::object::ObjectId;
use crate::remote::server::{self, ServerConfig};
use crate::repo::txn::{Cas, RefLogEntry};
use crate::repo::{Head, Repo};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FILES: usize = 40;
const FILE_BYTES: usize = 12 * 1024;
const HISTORY_SIZES: &[usize] = &[80, 800];
const PROJECTION_SAMPLES: usize = 3;
const CLONE_SAMPLES: usize = 3;
const CONCURRENT_CLONES: usize = 4;
static CLOCK_TICKS_PER_SECOND: OnceLock<Option<f64>> = OnceLock::new();

#[derive(Clone, Debug, Default)]
struct TransferStats {
    responses: u64,
    response_body_bytes: u64,
    non_200: Vec<String>,
}

struct CountingProxy {
    addr: SocketAddr,
    stats: Arc<Mutex<TransferStats>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl CountingProxy {
    fn spawn(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stats = Arc::new(Mutex::new(TransferStats::default()));
        let worker_stats = stats.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let join = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let stats = worker_stats.clone();
                        thread::spawn(move || proxy_connection(client, upstream, stats));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stats,
            stop,
            join: Some(join),
        }
    }

    fn reset(&self) {
        *self.stats.lock().unwrap() = TransferStats::default();
    }

    fn snapshot(&self) -> TransferStats {
        self.stats.lock().unwrap().clone()
    }
}

impl Drop for CountingProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn proxy_connection(
    mut client: TcpStream,
    upstream_addr: SocketAddr,
    stats: Arc<Mutex<TransferStats>>,
) {
    let Ok(mut upstream) = TcpStream::connect(upstream_addr) else {
        return;
    };
    let Ok(request) = read_http_head(&mut client) else {
        return;
    };
    let Some(request_len) = content_length(&request) else {
        return;
    };
    if upstream.write_all(&request).is_err()
        || copy_exact(&mut client, &mut upstream, request_len).is_err()
    {
        return;
    }
    let _ = upstream.shutdown(std::net::Shutdown::Write);

    let Ok(response) = read_http_head(&mut upstream) else {
        return;
    };
    let Some(response_len) = content_length(&response) else {
        return;
    };
    if client.write_all(&response).is_err() {
        return;
    }
    let status_line = std::str::from_utf8(&response)
        .ok()
        .and_then(|head| head.lines().next())
        .unwrap_or("<invalid HTTP response>")
        .to_string();
    let mut remaining = response_len;
    let mut buffer = [0u8; 64 * 1024];
    let mut preview = Vec::new();
    while remaining > 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
        let Ok(n) = upstream.read(&mut buffer[..take]) else {
            return;
        };
        if n == 0 || client.write_all(&buffer[..n]).is_err() {
            return;
        }
        if preview.len() < 512 {
            let count = (512 - preview.len()).min(n);
            preview.extend_from_slice(&buffer[..count]);
        }
        remaining -= n as u64;
    }
    let mut totals = stats.lock().unwrap();
    totals.responses += 1;
    totals.response_body_bytes += response_len;
    if !status_line.contains(" 200 ") {
        totals.non_200.push(format!(
            "{status_line}: {}",
            String::from_utf8_lossy(&preview)
        ));
    }
    drop(totals);
    let _ = client.flush();
    let _ = client.shutdown(std::net::Shutdown::Write);
}

fn read_http_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while head.len() < 64 * 1024 {
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            return Ok(head);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "HTTP header exceeded 64 KiB",
    ))
}

fn content_length(head: &[u8]) -> Option<u64> {
    std::str::from_utf8(head)
        .ok()?
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())
                .flatten()
        })
        .or(Some(0))
}

fn copy_exact(
    input: &mut TcpStream,
    output: &mut TcpStream,
    mut remaining: u64,
) -> std::io::Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64)).unwrap();
        let n = input.read(&mut buffer[..take])?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed before request body finished",
            ));
        }
        output.write_all(&buffer[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}

fn randomish_bytes(file: usize, revision: usize) -> Vec<u8> {
    let mut state = (file as u64 + 1).wrapping_mul(0x9e37_79b9) ^ revision as u64;
    (0..FILE_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

fn build_fixture(root: &Path, commits: usize) -> Repo {
    std::fs::create_dir_all(root).unwrap();
    let repo = Repo::init(root).unwrap();
    let author = repo
        .put(&Object::Actor(Actor {
            kind: ActorKind::Human,
            id: "git-http-benchmark".into(),
            display_name: "Git HTTP benchmark".into(),
            tool: "benchmark".into(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
            pubkey: None,
            extras: [("email".into(), "benchmark@example.test".into())]
                .into_iter()
                .collect(),
        }))
        .unwrap();

    let mut files: Vec<(String, ObjectId, EntryMode)> = Vec::with_capacity(FILES);
    for index in 0..FILES {
        let blob = repo.objects.put_blob(&randomish_bytes(index, 0)).unwrap();
        files.push((format!("src/module-{index:03}.dat"), blob, EntryMode::File));
    }
    let mut parent = None;
    for revision in 0..commits {
        let file_index = revision % FILES;
        let blob = repo
            .objects
            .put_blob(&randomish_bytes(file_index, revision + 1))
            .unwrap();
        files[file_index].1 = blob;
        let entries = files
            .iter()
            .map(|(name, blob, mode)| (name.clone(), *blob, *mode))
            .collect::<Vec<_>>();
        let tree = crate::ops::tree::build_tree(&repo, &entries).unwrap();
        let snapshot = Snapshot {
            parents: parent.into_iter().collect(),
            root: tree,
            author,
            timestamp_ms: 1_735_689_600_000 + revision as i64 * 1000,
            tz_offset_min: 0,
            message: format!("benchmark snapshot {revision}"),
            workspace: None,
            change: None,
            goal: None,
            extras: Default::default(),
        };
        let commit = repo.put(&Object::Snapshot(snapshot)).unwrap();
        repo.refs
            .update(
                "refs/main",
                Cas::Any,
                Some(commit),
                RefLogEntry::system("benchmark advance"),
            )
            .unwrap();
        parent = Some(commit);
    }
    repo.set_head(
        &Head::Symbolic("refs/main".into()),
        RefLogEntry::system("benchmark default branch"),
    )
    .unwrap();
    repo
}

fn build_small_repo(root: &Path) -> (Repo, ObjectId) {
    std::fs::create_dir_all(root).unwrap();
    let repo = Repo::init(root).unwrap();
    let author = repo
        .put(&Object::Actor(Actor {
            kind: ActorKind::Human,
            id: "git-http-projection-test".into(),
            display_name: "Git HTTP projection test".into(),
            tool: "test".into(),
            tool_version: env!("CARGO_PKG_VERSION").into(),
            pubkey: None,
            extras: [("email".into(), "projection@example.test".into())]
                .into_iter()
                .collect(),
        }))
        .unwrap();
    let blob = repo.objects.put_blob(b"projection contents\n").unwrap();
    let tree = crate::ops::tree::build_tree(&repo, &[("README.md".into(), blob, EntryMode::File)])
        .unwrap();
    let tip = repo
        .put(&Object::Snapshot(Snapshot {
            parents: Vec::new(),
            root: tree,
            author,
            timestamp_ms: 1_735_689_600_000,
            tz_offset_min: 0,
            message: "projection test commit".into(),
            workspace: None,
            change: None,
            goal: None,
            extras: Default::default(),
        }))
        .unwrap();
    repo.refs
        .update(
            "refs/main",
            Cas::Exactly(None),
            Some(tip),
            RefLogEntry::system("projection test ref"),
        )
        .unwrap();
    repo.set_head(
        &Head::Symbolic("refs/main".into()),
        RefLogEntry::system("projection test HEAD"),
    )
    .unwrap();
    (repo, tip)
}

fn run_git(args: &[String]) -> Output {
    Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_TEMPLATE_DIR")
        .output()
        .unwrap_or_else(|error| panic!("could not start git {args:?}: {error}"))
}

#[derive(Clone, Copy, Debug, Default)]
struct GitResources {
    user_seconds: Option<f64>,
    system_seconds: Option<f64>,
    peak_rss_kib: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct ProcessCpu {
    user_ticks: u64,
    system_ticks: u64,
}

fn process_cpu() -> Option<ProcessCpu> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    Some(ProcessCpu {
        user_ticks: fields.get(11)?.parse().ok()?,
        system_ticks: fields.get(12)?.parse().ok()?,
    })
}

fn clock_tick_rate() -> Option<f64> {
    *CLOCK_TICKS_PER_SECOND.get_or_init(|| {
        let output = Command::new("getconf").arg("CLK_TCK").output().ok()?;
        String::from_utf8(output.stdout)
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
    })
}

fn process_cpu_seconds(before: Option<ProcessCpu>, after: Option<ProcessCpu>) -> Option<f64> {
    let (before, after, hz) = (before?, after?, clock_tick_rate()?);
    Some(
        (after.user_ticks.saturating_sub(before.user_ticks)
            + after.system_ticks.saturating_sub(before.system_ticks)) as f64
            / hz,
    )
}

fn harness_peak_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix("VmHWM:")?.split_whitespace().next()?;
        value.parse().ok()
    })
}

fn child_process_sample(pid: u32) -> Option<(u64, u64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let user_ticks = fields.get(11)?.parse().ok()?;
    let system_ticks = fields.get(12)?.parse().ok()?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let peak_rss_kib = status.lines().find_map(|line| {
        let value = line.strip_prefix("VmHWM:")?.split_whitespace().next()?;
        value.parse().ok()
    })?;
    Some((user_ticks, system_ticks, peak_rss_kib))
}

fn sample_child_resources(pid: u32) -> GitResources {
    let mut last_cpu = None;
    let mut peak_rss_kib = None;
    while let Some((user_ticks, system_ticks, rss)) = child_process_sample(pid) {
        last_cpu = Some((user_ticks, system_ticks));
        peak_rss_kib = Some(peak_rss_kib.unwrap_or(0u64).max(rss));
        thread::sleep(Duration::from_millis(2));
    }
    let (user_seconds, system_seconds) = match (last_cpu, clock_tick_rate()) {
        (Some((user, system)), Some(hz)) if hz > 0.0 => {
            (Some(user as f64 / hz), Some(system as f64 / hz))
        }
        _ => (None, None),
    };
    GitResources {
        user_seconds,
        system_seconds,
        peak_rss_kib,
    }
}

fn run_timed_git(args: &[String]) -> (Duration, Output, GitResources) {
    let mut command = Command::new("git");
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_TEMPLATE_DIR"] {
        command.env_remove(key);
    }
    let start = Instant::now();
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("could not start git {args:?}: {error}"));
    let pid = child.id();
    let (output, resources) = thread::scope(|scope| {
        let sampler = scope.spawn(move || sample_child_resources(pid));
        let output = child.wait_with_output();
        let resources = sampler.join().unwrap_or_default();
        (output, resources)
    });
    let elapsed = start.elapsed();
    let output = output.unwrap_or_else(|error| panic!("could not wait for git {args:?}: {error}"));
    (elapsed, output, resources)
}

fn optional_seconds(value: Option<f64>) -> String {
    value
        .map(|seconds| format!("{seconds:.3}"))
        .unwrap_or_else(|| "n/a".into())
}

fn measure(label: &str, proxy: &CountingProxy, args: &[String]) -> Duration {
    proxy.reset();
    let process_cpu_before = process_cpu();
    let (elapsed, output, resources) = run_timed_git(args);
    let process_cpu_elapsed = process_cpu_seconds(process_cpu_before, process_cpu());
    let transfer = proxy.snapshot();
    println!(
        "transfer {label}: wall_ms={:.2}, http_responses={}, response_body_bytes={}, client_leader_user_s={}, client_leader_system_s={}, client_leader_peak_rss_kib={}, harness_process_cpu_s={}, non_200={:?}",
        elapsed.as_secs_f64() * 1000.0,
        transfer.responses,
        transfer.response_body_bytes,
        optional_seconds(resources.user_seconds),
        optional_seconds(resources.system_seconds),
        resources.peak_rss_kib.map(|rss| rss.to_string()).unwrap_or_else(|| "n/a".into()),
        optional_seconds(process_cpu_elapsed),
        transfer.non_200,
    );
    assert!(
        output.status.success(),
        "git {args:?} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    elapsed
}

fn measure_concurrent_clones(proxy: &CountingProxy, args: Vec<Vec<String>>) -> Vec<Duration> {
    proxy.reset();
    let process_cpu_before = process_cpu();
    let wall_start = Instant::now();
    let measurements = thread::scope(|scope| {
        let handles = args
            .iter()
            .map(|args| scope.spawn(move || run_timed_git(args)))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("concurrent clone thread panicked"))
            .collect::<Vec<_>>()
    });
    let wall = wall_start.elapsed();
    let process_cpu_elapsed = process_cpu_seconds(process_cpu_before, process_cpu());
    let transfer = proxy.snapshot();
    for (index, (elapsed, output, resources)) in measurements.iter().enumerate() {
        assert!(
            output.status.success(),
            "concurrent git clone {} failed ({}): {}",
            index + 1,
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        println!(
            "concurrent clone client {}: wall_ms={:.2}, client_leader_user_s={}, client_leader_system_s={}, client_leader_peak_rss_kib={}",
            index + 1,
            elapsed.as_secs_f64() * 1000.0,
            optional_seconds(resources.user_seconds),
            optional_seconds(resources.system_seconds),
            resources.peak_rss_kib.map(|rss| rss.to_string()).unwrap_or_else(|| "n/a".into()),
        );
    }
    println!(
        "concurrent clone batch: clients={}, wall_ms={:.2}, http_responses={}, response_body_bytes={}, harness_process_cpu_s={}, non_200={:?}",
        measurements.len(),
        wall.as_secs_f64() * 1000.0,
        transfer.responses,
        transfer.response_body_bytes,
        optional_seconds(process_cpu_elapsed),
        transfer.non_200,
    );
    measurements
        .into_iter()
        .map(|(elapsed, _, _)| elapsed)
        .collect()
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => directory_bytes(&path),
                Ok(kind) if kind.is_file() => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                _ => 0,
            }
        })
        .sum()
}

fn median_ms(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[test]
fn upload_pack_projection_preserves_head_and_objects_without_checkout() {
    let root = tempfile::tempdir().unwrap();
    let (repo, tip) = build_small_repo(&root.path().join("newgit"));

    let (symbolic, symbolic_report) = TempGitView::from_newgit_for_upload_pack_with_export(
        &repo,
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap();
    assert!(
        !symbolic.path.join("README.md").exists(),
        "upload-pack projection must not create a working-tree copy"
    );
    let head_ref = run_git(&[
        "-C".into(),
        symbolic.path.display().to_string(),
        "symbolic-ref".into(),
        "HEAD".into(),
    ]);
    assert!(head_ref.status.success());
    assert_eq!(
        String::from_utf8_lossy(&head_ref.stdout).trim(),
        "refs/heads/main"
    );
    let blob = run_git(&[
        "-C".into(),
        symbolic.path.display().to_string(),
        "cat-file".into(),
        "blob".into(),
        "HEAD:README.md".into(),
    ]);
    assert!(blob.status.success());
    assert_eq!(blob.stdout, b"projection contents\n");
    assert!(symbolic_report
        .git_commit_oids
        .values()
        .any(|canonical| *canonical == tip));

    let (receive_pack, _) =
        TempGitView::from_newgit_with_export(&repo, Instant::now() + Duration::from_secs(120))
            .unwrap();
    assert_eq!(
        std::fs::read(receive_pack.path.join("README.md")).unwrap(),
        b"projection contents\n",
        "receive-pack retains its existing materialized projection"
    );

    repo.set_head(
        &Head::Detached(tip),
        RefLogEntry::system("make projection test HEAD detached"),
    )
    .unwrap();
    let (detached, detached_report) = TempGitView::from_newgit_for_upload_pack_with_export(
        &repo,
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap();
    assert!(
        !detached.path.join("README.md").exists(),
        "detached upload-pack projection must not create a working-tree copy"
    );
    let symbolic_head = run_git(&[
        "-C".into(),
        detached.path.display().to_string(),
        "symbolic-ref".into(),
        "-q".into(),
        "HEAD".into(),
    ]);
    assert!(
        !symbolic_head.status.success(),
        "HEAD should remain detached"
    );
    let head_oid = run_git(&[
        "-C".into(),
        detached.path.display().to_string(),
        "rev-parse".into(),
        "HEAD".into(),
    ]);
    assert!(head_oid.status.success());
    let expected_git_oid = detached_report
        .git_commit_oids
        .iter()
        .find_map(|(git_oid, canonical)| (*canonical == tip).then_some(git_oid))
        .expect("detached tip should map to its exported Git commit");
    assert_eq!(
        String::from_utf8_lossy(&head_oid.stdout).trim(),
        expected_git_oid
    );
}

#[test]
#[ignore = "manual performance experiment; run explicitly with --ignored --nocapture"]
fn live_git_transfer_baseline() {
    let root = tempfile::Builder::new()
        .prefix("newgit-git-http-bench-")
        .tempdir()
        .unwrap();
    let _ = clock_tick_rate();
    println!("benchmark: live Git smart-HTTP via loopback counting proxy");
    println!(
        "git: {}",
        String::from_utf8_lossy(&run_git(&["--version".into()]).stdout).trim()
    );
    println!(
        "context: fixture built once then served; client outputs use fresh directories; no OS page-cache eviction (warm-cache observation); projection temporary directory is fresh per HTTP request; {PROJECTION_SAMPLES} direct projection samples, {CLONE_SAMPLES} serial full-clone samples, {CONCURRENT_CLONES} simultaneous full clones, 3 unchanged fetches"
    );
    println!(
        "resources: /proc/{{pid}}/stat and /proc/{{pid}}/status are sampled every 2ms for each Git leader process (CPU and high-water RSS; excludes helper descendants); /proc/self/stat and VmHWM cover only the harness process, including its server threads but excluding child Git processes; CPU resolution is kernel clock ticks; non-Linux resource fields may be n/a"
    );
    println!(
        "process_clock_ticks_per_second={}",
        clock_tick_rate()
            .map(|hz| format!("{hz:.0}"))
            .unwrap_or_else(|| "n/a".into())
    );

    for &commits in HISTORY_SIZES {
        let case_root = root.path().join(format!("history-{commits}"));
        std::fs::create_dir_all(&case_root).unwrap();
        let newgit_path = case_root.join("canonical-newgit");
        let fixture_start = Instant::now();
        let repo = build_fixture(&newgit_path, commits);
        println!(
            "fixture: commits={commits}, paths_per_commit={FILES}, blob_bytes_per_version={FILE_BYTES}, build_ms={:.2}, canonical_repo_bytes={}",
            fixture_start.elapsed().as_secs_f64() * 1000.0,
            directory_bytes(&newgit_path),
        );

        let mut projection_samples = Vec::with_capacity(PROJECTION_SAMPLES);
        let mut projection_temp_bytes = 0;
        let mut projection_git_bytes = 0;
        let mut projection_objects_bytes = 0;
        let mut exported_blobs = 0;
        for sample in 1..=PROJECTION_SAMPLES {
            let cpu_before = process_cpu();
            let start = Instant::now();
            let (view, report) = TempGitView::from_newgit_for_upload_pack_with_export(
                &repo,
                Instant::now() + Duration::from_secs(120),
            )
            .unwrap();
            let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
            let cpu_seconds = process_cpu_seconds(cpu_before, process_cpu());
            let container = view.path.parent().unwrap_or(&view.path);
            projection_temp_bytes = directory_bytes(container);
            projection_git_bytes = directory_bytes(&view.path.join(".git"));
            projection_objects_bytes = directory_bytes(&view.path.join(".git/objects"));
            exported_blobs = report.blobs;
            println!(
                "projection sample {sample}/{PROJECTION_SAMPLES}: wall_ms={wall_ms:.2}, harness_process_cpu_s={}, temp_bytes={projection_temp_bytes}, git_dir_bytes={projection_git_bytes}, object_store_bytes={projection_objects_bytes}, exported_commits={}, exported_unique_blobs={exported_blobs}",
                optional_seconds(cpu_seconds), report.commits,
            );
            projection_samples.push(wall_ms);
            drop(view);
        }
        let projection_median = median_ms(&mut projection_samples.clone());
        println!(
            "projection summary: commits={commits}, raw_wall_ms={:?}, median_wall_ms={projection_median:.2}, last_sample_temp_bytes={projection_temp_bytes}, git_dir_bytes={projection_git_bytes}, object_store_bytes={projection_objects_bytes}, exported_unique_blobs={exported_blobs}",
            projection_samples.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>(),
        );

        let server = server::spawn(ServerConfig {
            bind: "127.0.0.1:0".into(),
            repo_root: newgit_path,
            token_file: case_root.join("missing-tokens.json"),
            allow_anonymous_read: true,
            ..Default::default()
        })
        .unwrap();
        let proxy = CountingProxy::spawn(server.addr());
        let url = format!("http://{}/", proxy.addr);
        let mut serial_clones = Vec::with_capacity(CLONE_SAMPLES);
        let mut first_clone = None;
        for sample in 1..=CLONE_SAMPLES {
            let destination = case_root.join(format!("clone-serial-{sample}"));
            if sample == 1 {
                first_clone = Some(destination.clone());
            }
            let args = vec![
                "-c".into(),
                "protocol.version=2".into(),
                "clone".into(),
                "--quiet".into(),
                url.clone(),
                destination.display().to_string(),
            ];
            serial_clones.push(measure(
                &format!("{commits}-commit full clone {sample}/{CLONE_SAMPLES}"),
                &proxy,
                &args,
            ));
        }
        let clone_ms = serial_clones
            .iter()
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .collect::<Vec<_>>();
        println!(
            "serial clone summary: commits={commits}, raw_wall_ms={:?}, median_wall_ms={:.2}",
            clone_ms
                .iter()
                .map(|x| format!("{x:.2}"))
                .collect::<Vec<_>>(),
            median_ms(&mut clone_ms.clone()),
        );

        let full = first_clone.expect("at least one clone sample");
        let mut fetch_samples = Vec::with_capacity(3);
        for sample in 1..=3 {
            fetch_samples.push(measure(
                &format!("{commits}-commit unchanged fetch {sample}/3"),
                &proxy,
                &[
                    "-C".into(),
                    full.display().to_string(),
                    "fetch".into(),
                    "--quiet".into(),
                    "origin".into(),
                ],
            ));
        }
        let fetch_ms = fetch_samples
            .iter()
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .collect::<Vec<_>>();
        println!(
            "unchanged fetch summary: commits={commits}, raw_wall_ms={:?}, median_wall_ms={:.2}",
            fetch_ms
                .iter()
                .map(|x| format!("{x:.2}"))
                .collect::<Vec<_>>(),
            median_ms(&mut fetch_ms.clone()),
        );

        let concurrent_args = (1..=CONCURRENT_CLONES)
            .map(|client| {
                let destination = case_root.join(format!("clone-concurrent-{client}"));
                vec![
                    "-c".into(),
                    "protocol.version=2".into(),
                    "clone".into(),
                    "--quiet".into(),
                    url.clone(),
                    destination.display().to_string(),
                ]
            })
            .collect();
        measure_concurrent_clones(&proxy, concurrent_args);
        server.shutdown();
        drop(proxy);
        drop(repo);
    }
    println!(
        "harness_peak_rss_kib={}",
        harness_peak_rss_kib()
            .map(|rss| rss.to_string())
            .unwrap_or_else(|| "n/a".into())
    );
    drop(root);
}
