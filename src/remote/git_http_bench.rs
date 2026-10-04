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
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FILES: usize = 40;
const FILE_BYTES: usize = 12 * 1024;
const COMMITS: usize = 80;

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
    let _ = client.flush();
    let _ = client.shutdown(std::net::Shutdown::Write);
    let mut totals = stats.lock().unwrap();
    totals.responses += 1;
    totals.response_body_bytes += response_len;
    if !status_line.contains(" 200 ") {
        totals.non_200.push(format!(
            "{status_line}: {}",
            String::from_utf8_lossy(&preview)
        ));
    }
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

fn build_fixture(root: &Path) -> Repo {
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
    for revision in 0..COMMITS {
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

fn measure(label: &str, proxy: &CountingProxy, args: &[String]) -> Duration {
    proxy.reset();
    let start = Instant::now();
    let output = run_git(args);
    let elapsed = start.elapsed();
    let transfer = proxy.snapshot();
    println!(
        "transfer {label}: wall_ms={:.2}, http_responses={}, response_body_bytes={}, non_200={:?}",
        elapsed.as_secs_f64() * 1000.0,
        transfer.responses,
        transfer.response_body_bytes,
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
    let newgit_path = root.path().join("canonical-newgit");
    let repo = build_fixture(&newgit_path);
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: newgit_path,
        token_file: root.path().join("missing-tokens.json"),
        allow_anonymous_read: true,
        ..Default::default()
    })
    .unwrap();
    let proxy = CountingProxy::spawn(server.addr());
    let url = format!("http://{}/", proxy.addr);

    println!("benchmark: live Git smart-HTTP via loopback counting proxy");
    println!(
        "git: {}",
        String::from_utf8_lossy(&run_git(&["--version".into()]).stdout).trim()
    );
    println!("fixture: {COMMITS} commits, {FILES} paths/commit, {FILE_BYTES} bytes/version");
    println!("workflows: full clone, depth=1 clone, blob:none clone+one-file lazy hydration, 3 unchanged fetches");

    let mut projection_ms = Vec::new();
    let mut projection_total_bytes = 0u64;
    let mut projection_git_bytes = 0u64;
    let mut projection_objects_bytes = 0u64;
    let mut projection_commits = 0usize;
    let mut projection_blobs = 0usize;
    for _ in 0..3 {
        let start = Instant::now();
        let (view, report) = TempGitView::from_newgit_for_upload_pack_with_export(
            &repo,
            Instant::now() + Duration::from_secs(120),
        )
        .unwrap();
        projection_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        projection_total_bytes = directory_bytes(&view.path);
        projection_git_bytes = directory_bytes(&view.path.join(".git"));
        projection_objects_bytes = directory_bytes(&view.path.join(".git/objects"));
        projection_commits = report.commits;
        projection_blobs = report.blobs;
        drop(view);
    }
    let projection_median = median_ms(&mut projection_ms);
    println!(
        "projection: samples_ms={:?}, median_ms={projection_median:.2}, commits={projection_commits}, exported_unique_blobs={projection_blobs}, total_temp_bytes={projection_total_bytes}, git_dir_bytes={projection_git_bytes}, object_store_bytes={projection_objects_bytes}",
        projection_ms.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>(),
    );

    let full = root.path().join("clone-full");
    let mut clone_full = vec![
        "-c".into(),
        "protocol.version=2".into(),
        "clone".into(),
        "--quiet".into(),
    ];
    clone_full.extend([url.clone(), full.display().to_string()]);
    measure("full clone", &proxy, &clone_full);

    let shallow = root.path().join("clone-shallow");
    let mut clone_shallow = vec![
        "-c".into(),
        "protocol.version=2".into(),
        "clone".into(),
        "--quiet".into(),
        "--depth=1".into(),
    ];
    clone_shallow.extend([url.clone(), shallow.display().to_string()]);
    measure("shallow clone --depth=1", &proxy, &clone_shallow);

    let partial = root.path().join("clone-blob-none");
    let mut clone_partial = vec![
        "-c".into(),
        "protocol.version=2".into(),
        "clone".into(),
        "--quiet".into(),
        "--filter=blob:none".into(),
        "--no-checkout".into(),
    ];
    clone_partial.extend([url.clone(), partial.display().to_string()]);
    measure("blob:none clone --no-checkout", &proxy, &clone_partial);

    measure(
        "blob:none lazy one-file checkout",
        &proxy,
        &[
            "-C".into(),
            partial.display().to_string(),
            "checkout".into(),
            "--quiet".into(),
            "main".into(),
            "--".into(),
            "src/module-039.dat".into(),
        ],
    );

    let mut fetch_ms = Vec::new();
    for index in 1..=3 {
        fetch_ms.push(measure(
            &format!("unchanged fetch {index}/3"),
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
    let fetch_values = fetch_ms
        .iter()
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .collect::<Vec<_>>();
    let fetch_median = median_ms(&mut fetch_values.clone());
    println!(
        "repeated fetch summary: samples_ms={:?}, median_ms={fetch_median:.2}",
        fetch_values
            .iter()
            .map(|x| format!("{x:.2}"))
            .collect::<Vec<_>>(),
    );

    server.shutdown();
    drop(proxy);
    drop(repo);
    drop(root);
}
