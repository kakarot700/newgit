//! Black-box smart-HTTP interoperability with the installed Git client.
//!
//! The HTTP peer is the real NewGit server. Git clone/fetch/pull/push/ls-remote
//! run as child processes over loopback TCP; assertions inspect actual Git
//! refs, commits, trees, and blob bytes rather than canned protocol replies.

mod common;

use common::git_config_file;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::process::{Command, Output};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use newgit::object::types::{Actor, ActorKind, EntryMode, Object, Snapshot};
use newgit::object::ObjectId;
use newgit::remote::auth::{self, Role, TokenFile};
use newgit::remote::server::{self, ServerConfig};
use newgit::repo::txn::{Cas, RefLogEntry};
use newgit::repo::{Head, Repo};
use tempfile::TempDir;

const READ_TOKEN: &str = "git-http-read-test-token";
const WRITE_TOKEN: &str = "git-http-write-test-token";
const ADMIN_TOKEN: &str = "git-http-admin-test-token";

fn temp(tag: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("ng-git-http-{tag}-"))
        .tempdir()
        .unwrap();
    let path = dir.path().to_path_buf();
    (dir, path)
}

fn actor(repo: &Repo) -> ObjectId {
    repo.put(&Object::Actor(Actor {
        kind: ActorKind::Human,
        id: "git-http-test".into(),
        display_name: "Git HTTP Test".into(),
        tool: String::new(),
        tool_version: String::new(),
        pubkey: None,
        extras: [("email".into(), "git-http@example.test".into())]
            .into_iter()
            .collect(),
    }))
    .unwrap()
}

fn commit(
    repo: &Repo,
    ref_name: &str,
    message: &str,
    files: &[(&str, &[u8], EntryMode)],
    parents: Vec<ObjectId>,
) -> ObjectId {
    let author = actor(repo);
    let entries = files
        .iter()
        .map(|(path, bytes, mode)| {
            let blob = repo.objects.put_blob(bytes).unwrap();
            ((*path).to_string(), blob, *mode)
        })
        .collect::<Vec<_>>();
    let root = newgit::ops::tree::build_tree(repo, &entries).unwrap();
    let oid = repo
        .put(&Object::Snapshot(Snapshot {
            parents,
            root,
            author,
            timestamp_ms: 1_735_689_600_000,
            tz_offset_min: 0,
            message: message.into(),
            workspace: None,
            change: None,
            goal: None,
            extras: Default::default(),
        }))
        .unwrap();
    repo.refs
        .update(ref_name, Cas::Any, Some(oid), RefLogEntry::system(message))
        .unwrap();
    oid
}

fn git(args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", git_config_file())
        .env("GIT_CONFIG_SYSTEM", git_config_file())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("could not spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn git_fails(args: &[&str]) -> Output {
    let out = Command::new("git")
        .args(args)
        .env("GIT_CONFIG_GLOBAL", git_config_file())
        .env("GIT_CONFIG_SYSTEM", git_config_file())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("could not spawn git {args:?}: {e}"));
    assert!(!out.status.success(), "git {args:?} unexpectedly succeeded");
    out
}

fn as_text(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn read_http_response(stream: &mut TcpStream, max_body_bytes: usize) -> (u16, Vec<u8>) {
    const MAX_HEADER_BYTES: usize = 64 * 1024;

    let mut response = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let n = stream.read(&mut chunk).unwrap_or_else(|error| {
            panic!(
                "HTTP status response read failed before complete headers ({} bytes received): {error}",
                response.len()
            )
        });
        assert_ne!(n, 0, "HTTP response closed before complete headers");
        response.extend_from_slice(&chunk[..n]);
        if let Some(position) = response.windows(4).position(|window| window == b"\r\n\r\n") {
            assert!(
                position <= MAX_HEADER_BYTES,
                "HTTP status response headers exceeded {MAX_HEADER_BYTES} bytes"
            );
            break position + 4;
        }
        assert!(
            response.len() <= MAX_HEADER_BYTES,
            "HTTP status response headers exceeded {MAX_HEADER_BYTES} bytes"
        );
    };
    let header = std::str::from_utf8(&response[..header_end - 4])
        .expect("HTTP response headers must be ASCII/UTF-8");
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("HTTP response must start with a numeric status");
    let content_length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("valid Content-Length"))
        })
        .expect("HTTP response must include Content-Length");
    assert!(
        content_length <= max_body_bytes,
        "HTTP response body exceeded {max_body_bytes} bytes"
    );
    let response_end = header_end
        .checked_add(content_length)
        .expect("HTTP response length must not overflow");
    while response.len() < response_end {
        let n = stream.read(&mut chunk).unwrap_or_else(|error| {
            panic!(
                "HTTP status response body incomplete ({} of {content_length} body bytes received): {error}",
                response.len().saturating_sub(header_end)
            )
        });
        assert_ne!(
            n, 0,
            "HTTP response closed before its declared {content_length}-byte body completed"
        );
        response.extend_from_slice(&chunk[..n]);
    }
    (status, response[header_end..response_end].to_vec())
}

fn read_http_status(stream: &mut TcpStream) -> u16 {
    read_http_response(stream, 1024 * 1024).0
}

fn raw_git_get_status(addr: SocketAddr, authorization: Option<&str>) -> u16 {
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET /info/refs?service=git-upload-pack HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n"
    )
    .unwrap();
    read_http_status(&mut stream)
}

fn raw_git_receive_get_status(addr: SocketAddr, authorization: Option<&str>) -> u16 {
    let mut stream = TcpStream::connect(addr).unwrap();
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET /info/refs?service=git-receive-pack HTTP/1.1\r\nHost: {addr}\r\n{auth}Connection: close\r\n\r\n"
    )
    .unwrap();
    read_http_status(&mut stream)
}

fn raw_git_receive_advertisement(addr: SocketAddr, authorization: &str) -> Vec<u8> {
    raw_git_receive_advertisement_with_protocol(addr, authorization, None)
}
fn raw_git_receive_advertisement_with_protocol(
    addr: SocketAddr,
    authorization: &str,
    git_protocol: Option<&str>,
) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    let protocol = git_protocol
        .map(|value| format!("Git-Protocol: {value}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "GET /info/refs?service=git-receive-pack HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {authorization}\r\n{protocol}Connection: close\r\n\r\n"
    )
    .unwrap();
    let (status, response) = read_http_response(&mut stream, 1024 * 1024);
    assert_eq!(
        status, 200,
        "receive-pack advertisement must return HTTP 200"
    );
    response
}

fn raw_git_receive_post_status(addr: SocketAddr, authorization: &str, body: &[u8]) -> u16 {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST /git-receive-pack HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {authorization}\r\nContent-Type: application/x-git-receive-pack-request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
    read_http_status(&mut stream)
}

const EMPTY_GIT_PACK_V2: &[u8; 32] = b"PACK\x00\x00\x00\x02\x00\x00\x00\x00\x02\x9d\x08\x82\x3b\xd8\xa8\xea\xb5\x10\xad\x6a\xc7\x5c\x82\x3c\xfd\x3e\xd3\x1e";

fn git_pkt_line(payload: &[u8]) -> Vec<u8> {
    let length = payload.len() + 4;
    assert!(
        length <= 0xffff,
        "test pkt-line must fit the four-byte header"
    );
    let mut packet = format!("{length:04x}").into_bytes();
    packet.extend_from_slice(payload);
    packet
}

fn git_receive_body(first_command: &[u8], trailing: &[u8]) -> Vec<u8> {
    let mut body = git_pkt_line(first_command);
    body.extend_from_slice(b"0000");
    body.extend_from_slice(trailing);
    body
}

#[cfg(target_os = "linux")]
const SNAPSHOT_READER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(130);

#[cfg(target_os = "linux")]
fn raw_git_http_response(addr: SocketAddr, request: &[u8]) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(SNAPSHOT_READER_RESPONSE_TIMEOUT))
        .unwrap();
    stream.write_all(request).unwrap();
    read_http_response(&mut stream, 256 * 1024 * 1024)
}

#[cfg(target_os = "linux")]
fn raw_git_upload_advertisement(addr: SocketAddr) -> (u16, Vec<u8>) {
    raw_git_http_response(
        addr,
        format!(
            "GET /info/refs?service=git-upload-pack HTTP/1.1\r\nHost: {addr}\r\nGit-Protocol: version=1\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
}

#[cfg(target_os = "linux")]
fn raw_git_upload_pack(addr: SocketAddr, request_body: &[u8]) -> (u16, Vec<u8>) {
    let request = format!(
        "POST /git-upload-pack HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        request_body.len()
    );
    let mut bytes = request.into_bytes();
    bytes.extend_from_slice(request_body);
    raw_git_http_response(addr, &bytes)
}

#[cfg(target_os = "linux")]
fn advertised_git_refs(advertisement: &[u8]) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    let mut position = 0;
    while position + 4 <= advertisement.len() {
        let Ok(prefix) = std::str::from_utf8(&advertisement[position..position + 4]) else {
            break;
        };
        let Ok(length) = usize::from_str_radix(prefix, 16) else {
            break;
        };
        position += 4;
        if length == 0 {
            continue;
        }
        if length < 4 || position + length - 4 > advertisement.len() {
            break;
        }
        let packet = &advertisement[position..position + length - 4];
        position += length - 4;
        let line = packet.split(|byte| *byte == 0).next().unwrap_or_default();
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let Some(space) = line.iter().position(|byte| *byte == b' ') else {
            continue;
        };
        let Ok(oid) = std::str::from_utf8(&line[..space]) else {
            continue;
        };
        let Ok(name) = std::str::from_utf8(&line[space + 1..]) else {
            continue;
        };
        if oid.len() == 40 && name.starts_with("refs/heads/") {
            refs.push((name.to_string(), oid.to_string()));
        }
    }
    refs.sort();
    refs
}

#[cfg(target_os = "linux")]
fn upload_pack_want(oid: &str) -> Vec<u8> {
    let line = format!("want {oid}\n");
    let mut request = format!("{:04x}{line}", line.len() + 4).into_bytes();
    request.extend_from_slice(b"0000");
    request.extend_from_slice(b"0009done\n");
    request
}

#[cfg(target_os = "linux")]
fn child_snapshot(repo: &Repo, message: &str, bytes: &[u8], parent: ObjectId) -> ObjectId {
    let author = actor(repo);
    let blob = repo.objects.put_blob(bytes).unwrap();
    let root =
        newgit::ops::tree::build_tree(repo, &[("file.txt".to_string(), blob, EntryMode::File)])
            .unwrap();
    repo.put(&Object::Snapshot(Snapshot {
        parents: vec![parent],
        root,
        author,
        timestamp_ms: 1_735_689_601_000,
        tz_offset_min: 0,
        message: message.into(),
        workspace: None,
        change: None,
        goal: None,
        extras: Default::default(),
    }))
    .unwrap()
}

#[test]
fn ambient_git_environment_cannot_redirect_or_run_template_hooks() {
    const CHILD_MODE: &str = "NEWGIT_GIT_DIR_REGRESSION_CHILD";
    const REPO_PATH: &str = "NEWGIT_GIT_DIR_REGRESSION_REPO";
    if std::env::var_os(CHILD_MODE).is_some() {
        let remote_path = PathBuf::from(std::env::var_os(REPO_PATH).unwrap());
        let root = remote_path.parent().unwrap();
        let token_file = root.join("ambient-test-tokens.json");
        auth::save(&token_file, &TokenFile::default()).unwrap();
        let server = server::spawn(ServerConfig {
            bind: "127.0.0.1:0".into(),
            repo_root: remote_path,
            token_file,
            allow_anonymous_read: true,
            ..Default::default()
        })
        .unwrap();
        let url = format!("http://{}/", server.addr());
        let listing = git(&["-c", "protocol.version=2", "ls-remote", &url]);
        assert!(
            as_text(&listing)
                .lines()
                .any(|line| line.ends_with("\trefs/heads/main")),
            "server did not advertise the NewGit-backed main ref: {}",
            String::from_utf8_lossy(&listing.stdout)
        );
        server.shutdown();
        return;
    }

    let (_dir, root) = temp("ambient-git-environment");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();
    let tip = commit(
        &newgit,
        "refs/main",
        "isolated projection",
        &[("file.txt", b"private to NewGit\n", EntryMode::File)],
        vec![],
    );
    newgit
        .set_head(
            &Head::Detached(tip),
            RefLogEntry::system("set test detached HEAD"),
        )
        .unwrap();
    assert_eq!(newgit.refs.read("refs/main").unwrap(), tip);
    drop(newgit);

    let sentinel = root.join("unrelated.git");
    git(&["init", "--bare", "--quiet", sentinel.to_str().unwrap()]);
    let before = git(&[
        "--git-dir",
        sentinel.to_str().unwrap(),
        "for-each-ref",
        "--format=%(refname)",
    ]);
    let template_dir = root.join("hostile-template");
    let hooks_dir = template_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_marker = root.join("ambient-template-hook-ran");
    let hook_marker_name = hook_marker.file_name().unwrap().to_string_lossy();
    let hook_path = hooks_dir.join("post-checkout");
    std::fs::write(
        &hook_path,
        format!("#!/bin/sh\nprintf ran > '{hook_marker_name}'\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let child = Command::new(std::env::current_exe().unwrap())
        .current_dir(&root)
        .args([
            "--exact",
            "ambient_git_environment_cannot_redirect_or_run_template_hooks",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env(REPO_PATH, &remote_path)
        .env("GIT_DIR", &sentinel)
        .env("GIT_WORK_TREE", root.join("unrelated-worktree"))
        .env("GIT_TEMPLATE_DIR", &template_dir)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "server test with hostile GIT_DIR failed ({}): {}\n{}",
        child.status,
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let after = git(&[
        "--git-dir",
        sentinel.to_str().unwrap(),
        "for-each-ref",
        "--format=%(refname)",
    ]);
    assert_eq!(
        before.stdout, after.stdout,
        "Git smart HTTP mutated refs in an unrelated ambient GIT_DIR"
    );
    assert!(
        as_text(&after).is_empty(),
        "unrelated bare repository gained refs: {}",
        as_text(&after)
    );
    assert!(
        !hook_marker.exists(),
        "ambient Git template hook executed during projection generation"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn smart_http_projection_waits_for_mid_apply_recovery_and_exports_only_committed_refs() {
    let (_dir, root) = temp("snapshot-recovery-race");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let repo = Repo::init(&remote_path).unwrap();
    let old_left = commit(
        &repo,
        "refs/left",
        "old left",
        &[("file.txt", b"old left\n", EntryMode::File)],
        vec![],
    );
    let old_right = commit(
        &repo,
        "refs/right",
        "old right",
        &[("file.txt", b"old right\n", EntryMode::File)],
        vec![],
    );
    let new_left = child_snapshot(&repo, "new left", b"new left\n", old_left);
    let new_right = child_snapshot(&repo, "new right", b"new right\n", old_right);
    repo.set_head(
        &Head::Symbolic("refs/left".into()),
        RefLogEntry::system("set snapshot-race HEAD"),
    )
    .unwrap();

    let prior_advertisement =
        newgit::remote::git_http::advertise(&repo, Some("version=1"), 64 * 1024 * 1024).unwrap();
    let old_refs = advertised_git_refs(&prior_advertisement)
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        old_refs.len(),
        2,
        "unexpected initial advertisement: {old_refs:?}"
    );
    let want_old_left = upload_pack_want(&old_refs["refs/heads/left"]);

    let token_file = root.join("tokens.json");
    auth::save(&token_file, &TokenFile::default()).unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path.clone(),
        token_file,
        allow_anonymous_read: true,
        ..Default::default()
    })
    .unwrap();

    // Pause the live transaction after its first ref apply. It still owns the
    // real transaction lock while the partial on-disk state exists.
    let new_left_hex = new_left.to_hex();
    let new_right_hex = new_right.to_hex();
    let pause_marker = root.join("txn-paused");
    let resume_marker = root.join("txn-resume");
    let mut transaction = Command::new(std::env::var("CARGO_BIN_EXE_newgit-faultlab").unwrap())
        .args([
            "txn-two",
            remote_path.to_str().unwrap(),
            "refs/left",
            &new_left_hex,
            "refs/right",
            &new_right_hex,
        ])
        .env("NEWGIT_FAULTS", "txn:apply#1")
        .env("NEWGIT_FAULT_MODE", "pause")
        .env("NEWGIT_FAULT_READY_FILE", &pause_marker)
        .env("NEWGIT_FAULT_RESUME_FILE", &resume_marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pause_deadline = Instant::now() + Duration::from_secs(10);
    while !pause_marker.exists() {
        if let Some(status) = transaction.try_wait().unwrap() {
            panic!("faultlab exited before pausing after ref 1: {status}");
        }
        assert!(Instant::now() < pause_deadline, "faultlab pause timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(repo.refs.read("refs/left").unwrap(), new_left);
    assert_eq!(repo.refs.read("refs/right").unwrap(), old_right);
    assert!(std::fs::read_dir(repo.ng().join("txn"))
        .unwrap()
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".journal")));

    // Readers using an already-open Repo and the live HTTP server must block
    // while the transaction process itself holds the lock.
    let url = format!("http://{}/", server.addr());
    let (advertisement_tx, advertisement_rx) = mpsc::channel();
    let (projection_pack_tx, projection_pack_rx) = mpsc::channel();
    let (http_advertisement_tx, http_advertisement_rx) = mpsc::channel();
    let (http_pack_tx, http_pack_rx) = mpsc::channel();
    let (git_ls_remote_tx, git_ls_remote_rx) = mpsc::channel();

    std::thread::scope(|scope| {
        scope.spawn(|| {
            advertisement_tx
                .send(newgit::remote::git_http::advertise(
                    &repo,
                    Some("version=1"),
                    64 * 1024 * 1024,
                ))
                .unwrap();
        });
        scope.spawn(|| {
            projection_pack_tx
                .send(newgit::remote::git_http::upload_pack(
                    &repo,
                    None,
                    &want_old_left,
                    64 * 1024 * 1024,
                ))
                .unwrap();
        });
        scope.spawn(|| {
            http_advertisement_tx
                .send(raw_git_upload_advertisement(server.addr()))
                .unwrap();
        });
        scope.spawn(|| {
            http_pack_tx
                .send(raw_git_upload_pack(server.addr(), &want_old_left))
                .unwrap();
        });
        scope.spawn(|| {
            let output = Command::new("git")
                .args([
                    "-c",
                    "protocol.version=1",
                    "ls-remote",
                    &url,
                    "refs/heads/left",
                    "refs/heads/right",
                ])
                .env("GIT_CONFIG_GLOBAL", git_config_file())
                .env("GIT_CONFIG_SYSTEM", git_config_file())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap();
            git_ls_remote_tx.send(output).unwrap();
        });

        assert!(
            matches!(
                advertisement_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "direct advertisement returned before lock release"
        );
        assert!(
            matches!(
                projection_pack_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "direct upload-pack returned before lock release"
        );
        assert!(
            matches!(
                http_advertisement_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "HTTP advertisement returned before lock release"
        );
        assert!(
            matches!(
                http_pack_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "HTTP upload-pack returned before lock release"
        );
        assert!(
            matches!(
                git_ls_remote_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "real Git ls-remote returned before lock release"
        );

        assert!(
            transaction.try_wait().unwrap().is_none(),
            "transaction must still be paused with the lock held"
        );
        transaction.kill().unwrap();
        assert!(
            !transaction.wait().unwrap().success(),
            "killed mid-apply transaction must not report success"
        );

        let direct_advertisement = advertisement_rx
            .recv_timeout(SNAPSHOT_READER_RESPONSE_TIMEOUT)
            .expect("direct advertisement must finish after recovery")
            .unwrap();
        let direct_pack = projection_pack_rx
            .recv_timeout(SNAPSHOT_READER_RESPONSE_TIMEOUT)
            .expect("direct upload-pack must finish after recovery")
            .unwrap();
        let (http_ad_status, http_advertisement) = http_advertisement_rx
            .recv_timeout(SNAPSHOT_READER_RESPONSE_TIMEOUT)
            .expect("HTTP advertisement must finish after recovery");
        let (http_pack_status, http_pack) = http_pack_rx
            .recv_timeout(SNAPSHOT_READER_RESPONSE_TIMEOUT)
            .expect("HTTP upload-pack must finish after recovery");
        let git_ls_remote = git_ls_remote_rx
            .recv_timeout(SNAPSHOT_READER_RESPONSE_TIMEOUT)
            .expect("real Git ls-remote must finish after recovery");

        let direct_refs = advertised_git_refs(&direct_advertisement)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let http_refs = advertised_git_refs(&http_advertisement)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            direct_refs.len(),
            2,
            "direct snapshot refs: {direct_refs:?}"
        );
        assert_eq!(http_ad_status, 200);
        assert_eq!(http_refs.len(), 2, "HTTP snapshot refs: {http_refs:?}");
        assert_eq!(direct_refs, http_refs);
        assert_ne!(direct_refs["refs/heads/left"], old_refs["refs/heads/left"]);
        assert_ne!(
            direct_refs["refs/heads/right"],
            old_refs["refs/heads/right"]
        );
        assert_eq!(http_pack_status, 200);
        assert!(
            direct_pack.windows(4).any(|bytes| bytes == b"PACK"),
            "direct upload-pack response: {:?}",
            String::from_utf8_lossy(&direct_pack)
        );
        assert!(
            http_pack.windows(4).any(|bytes| bytes == b"PACK"),
            "HTTP upload-pack response: {:?}",
            String::from_utf8_lossy(&http_pack)
        );
        assert!(
            git_ls_remote.status.success(),
            "{}",
            String::from_utf8_lossy(&git_ls_remote.stderr)
        );
        let git_refs = String::from_utf8(git_ls_remote.stdout).unwrap();
        assert_eq!(git_refs.lines().count(), 2, "{git_refs:?}");
        assert!(git_refs.contains("refs/heads/left"), "{git_refs:?}");
        assert!(git_refs.contains("refs/heads/right"), "{git_refs:?}");
    });

    assert_eq!(repo.refs.read("refs/left").unwrap(), new_left);
    assert_eq!(repo.refs.read("refs/right").unwrap(), new_right);
    assert!(repo.recover().unwrap().0.redone.is_empty());
    server.shutdown();
}

#[test]
fn real_git_clone_fetch_pull_push_and_ls_remote_over_smart_http() {
    let (_dir, root) = temp("roundtrip");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();
    let binary = [0, 1, 2, 0, 254, 255, 128, 42];
    let s1 = commit(
        &newgit,
        "refs/main",
        "initial remote snapshot",
        &[
            ("README.md", b"from NewGit\n", EntryMode::File),
            ("nested/blob.bin", &binary, EntryMode::File),
        ],
        vec![],
    );
    newgit
        .refs
        .update(
            "refs/feature",
            Cas::Exactly(None),
            Some(s1),
            RefLogEntry::system("feature at initial snapshot"),
        )
        .unwrap();
    newgit
        .refs
        .update(
            "refs/tags/v1.0",
            Cas::Exactly(None),
            Some(s1),
            RefLogEntry::system("test tag"),
        )
        .unwrap();
    newgit
        .set_head(
            &Head::Symbolic("refs/main".into()),
            RefLogEntry::system("set default branch"),
        )
        .unwrap();
    assert!(matches!(
        newgit::remote::git_http::advertise(&newgit, None, 64),
        Err(newgit::error::Error::Limit(_))
    ));

    let tokens_dir = root.join("tokens");
    std::fs::create_dir(&tokens_dir).unwrap();
    let token_file = tokens_dir.join("tokens.json");
    let mut tokens = TokenFile::default();
    tokens.add("git-reader", READ_TOKEN, Role::Read).unwrap();
    tokens.add("git-writer", WRITE_TOKEN, Role::Write).unwrap();
    auth::save(&token_file, &tokens).unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path.clone(),
        token_file,
        allow_anonymous_read: false,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    assert_eq!(raw_git_get_status(server.addr(), None), 401);
    assert_eq!(
        raw_git_get_status(server.addr(), Some("Bearer not-the-token")),
        401
    );
    let write_auth = format!("Bearer {WRITE_TOKEN}");
    assert_eq!(raw_git_receive_get_status(server.addr(), None), 401);
    assert_eq!(
        raw_git_receive_get_status(server.addr(), Some(&format!("Bearer {READ_TOKEN}"))),
        403
    );
    assert_eq!(
        raw_git_receive_get_status(server.addr(), Some(&write_auth)),
        200
    );
    let receive_advertisement = raw_git_receive_advertisement(server.addr(), &write_auth);
    assert!(
        String::from_utf8_lossy(&receive_advertisement).contains(" atomic "),
        "receive-pack must advertise the atomic capability: {:?}",
        String::from_utf8_lossy(&receive_advertisement)
    );
    let receive_v1_advertisement =
        raw_git_receive_advertisement_with_protocol(server.addr(), &write_auth, Some("version=1"));
    let receive_v1_text = String::from_utf8_lossy(&receive_v1_advertisement);
    assert!(
        receive_v1_text.contains("000eversion 1\n"),
        "receive-pack v1 must include its version packet: {receive_v1_text:?}"
    );
    assert!(
        receive_v1_text.contains(" atomic "),
        "receive-pack v1 must retain the atomic capability: {receive_v1_text:?}"
    );
    let receive_v2_fallback =
        raw_git_receive_advertisement_with_protocol(server.addr(), &write_auth, Some("version=2"));
    let receive_v2_text = String::from_utf8_lossy(&receive_v2_fallback);
    assert!(
        !receive_v2_text.contains("version 1") && !receive_v2_text.contains("version 2"),
        "receive-pack must fall back to v0 framing for a v2 request: {receive_v2_text:?}"
    );
    assert!(
        receive_v2_text.contains(" atomic "),
        "the v0 fallback must retain the atomic capability: {receive_v2_text:?}"
    );
    let clone_path = root.join("clone");
    let clone_path_str = clone_path.to_str().unwrap();
    let auth_config = format!("http.extraHeader=Authorization: Bearer {READ_TOKEN}");

    // Force protocol v2 for clone. Git computes and validates the received
    // packfile, refs, checkout, and binary blob over real HTTP/TCP.
    git(&[
        "-c",
        "protocol.version=2",
        "-c",
        &auth_config,
        "clone",
        "--quiet",
        &url,
        clone_path_str,
    ]);
    assert_eq!(
        std::fs::read(clone_path.join("README.md")).unwrap(),
        b"from NewGit\n"
    );
    assert_eq!(
        std::fs::read(clone_path.join("nested/blob.bin")).unwrap(),
        binary
    );
    assert_eq!(
        as_text(&git(&["-C", clone_path_str, "symbolic-ref", "HEAD"])).trim(),
        "refs/heads/main"
    );
    assert_eq!(
        as_text(&git(&["-C", clone_path_str, "rev-parse", "HEAD"])).trim(),
        as_text(&git(&[
            "-C",
            clone_path_str,
            "rev-parse",
            "refs/remotes/origin/main"
        ]))
        .trim()
    );
    let tree_output = git(&["-C", clone_path_str, "ls-tree", "-r", "--name-only", "HEAD"]);
    let tree = as_text(&tree_output);
    assert!(tree.lines().any(|line| line == "README.md"));
    assert!(tree.lines().any(|line| line == "nested/blob.bin"));
    assert!(as_text(&git(&["-C", clone_path_str, "cat-file", "-t", "HEAD"])).trim() == "commit");

    let listing = git(&[
        "-c",
        "protocol.version=2",
        "-c",
        &auth_config,
        "-C",
        clone_path_str,
        "ls-remote",
        "--symref",
        "origin",
    ]);
    let listing = as_text(&listing);
    assert!(
        listing.lines().any(|l| l == "ref: refs/heads/main\tHEAD"),
        "{listing}"
    );
    assert!(
        listing.lines().any(|l| l.ends_with("\trefs/heads/main")),
        "{listing}"
    );
    assert!(
        listing.lines().any(|l| l.ends_with("\trefs/heads/feature")),
        "{listing}"
    );
    assert!(
        listing.lines().any(|l| l.ends_with("\trefs/tags/v1.0")),
        "{listing}"
    );

    // Advance the canonical NewGit ref, then fetch over protocol v0. The
    // fetched commit and blob are checked through Git, not through NewGit's
    // remote client or a synthetic HTTP payload.
    let s2 = commit(
        &newgit,
        "refs/main",
        "second remote snapshot",
        &[
            ("README.md", b"updated through NewGit\n", EntryMode::File),
            ("nested/blob.bin", &binary, EntryMode::File),
            ("new.txt", b"new content\n", EntryMode::File),
        ],
        vec![s1],
    );
    git(&[
        "-c",
        "protocol.version=0",
        "-c",
        &auth_config,
        "-C",
        clone_path_str,
        "fetch",
        "--quiet",
        "origin",
    ]);
    let fetched_tip = as_text(&git(&[
        "-C",
        clone_path_str,
        "rev-parse",
        "refs/remotes/origin/main",
    ]))
    .trim()
    .to_string();
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path_str,
            "show",
            "-s",
            "--format=%s",
            &fetched_tip
        ]))
        .trim(),
        "second remote snapshot"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path_str,
            "cat-file",
            "-p",
            &format!("{fetched_tip}:new.txt")
        ])),
        "new content\n"
    );

    // Git protocol v1 is independently exercised by an ordinary fast-forward
    // pull, which updates the cloned branch and its working tree.
    git(&[
        "-c",
        "protocol.version=1",
        "-c",
        &auth_config,
        "-C",
        clone_path_str,
        "pull",
        "--quiet",
        "--ff-only",
    ]);
    assert_eq!(newgit.refs.read("refs/main").unwrap(), s2);
    assert_eq!(
        as_text(&git(&["-C", clone_path_str, "rev-parse", "HEAD"])).trim(),
        fetched_tip
    );
    assert_eq!(
        std::fs::read(clone_path.join("README.md")).unwrap(),
        b"updated through NewGit\n"
    );
    assert_eq!(
        std::fs::read(clone_path.join("new.txt")).unwrap(),
        b"new content\n"
    );
    assert!(as_text(&git(&["-C", clone_path_str, "status", "--porcelain"])).is_empty());

    // A wrong bearer token is rejected by the same access boundary.
    let wrong_auth = "http.extraHeader=Authorization: Bearer not-the-token";
    let denied = git_fails(&[
        "-c",
        "protocol.version=2",
        "-c",
        wrong_auth,
        "ls-remote",
        &url,
    ]);
    assert!(
        !String::from_utf8_lossy(&denied.stderr).is_empty(),
        "Git should report the denied remote access"
    );

    // Adversarial receive-pack framing fails safely and never changes canonical
    // NewGit refs or objects. Git's pack decoder remains the authority for the
    // pack version/checksum and object contents.
    let clean_objects = newgit.objects.iter().unwrap();
    let clean_refs = newgit.refs.list(None).unwrap();
    assert_eq!(
        raw_git_receive_post_status(server.addr(), &write_auth, b"000x"),
        400
    );
    let zero_oid = "0000000000000000000000000000000000000000";
    let command = format!("{zero_oid} {fetched_tip} refs/heads/parser-probe\0report-status\n");
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(command.as_bytes(), b""),
        ),
        400,
        "create/update without even an empty pack must be rejected before Git projection"
    );
    let no_capability_separator =
        format!("{zero_oid} {fetched_tip} refs/heads/no-capability-separator\n");
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(no_capability_separator.as_bytes(), EMPTY_GIT_PACK_V2),
        ),
        400
    );
    let multiple_nuls =
        format!("{zero_oid} {fetched_tip} refs/heads/multiple-nuls\0report-status\0hidden\n");
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(multiple_nuls.as_bytes(), EMPTY_GIT_PACK_V2),
        ),
        400
    );
    let malformed_ref = format!("{zero_oid} {fetched_tip} refs/heads/../escape\0report-status\n");
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(malformed_ref.as_bytes(), EMPTY_GIT_PACK_V2),
        ),
        400
    );
    let mut invalid_utf8 = format!("{zero_oid} {fetched_tip} refs/heads/").into_bytes();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(b"\0report-status\n");
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(&invalid_utf8, EMPTY_GIT_PACK_V2),
        ),
        400
    );
    for special_length in [b"0001".as_slice(), b"0002", b"0003", b"0004"] {
        let mut malformed = special_length.to_vec();
        malformed.extend_from_slice(b"0000");
        assert_eq!(
            raw_git_receive_post_status(server.addr(), &write_auth, &malformed),
            400,
            "unexpected special/empty pkt-line {special_length:?}"
        );
    }
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(command.as_bytes(), b"PACK"),
        ),
        400,
        "truncated pack header must be rejected before Git projection"
    );

    // A syntactically framed but invalid pack reaches Git's receive-pack
    // validator. Duplicate capability tokens are passed through to Git; an
    // unpack rejection must still leave canonical NewGit state untouched.
    let duplicate_capabilities = format!(
        "{zero_oid} {fetched_tip} refs/heads/duplicate-capabilities\0report-status report-status\n"
    );
    let mut invalid_pack = b"PACK".to_vec();
    invalid_pack.extend_from_slice(&[0; 28]);
    assert_eq!(
        raw_git_receive_post_status(
            server.addr(),
            &write_auth,
            &git_receive_body(duplicate_capabilities.as_bytes(), &invalid_pack),
        ),
        200,
        "Git reports an unpack failure in its receive-pack status response"
    );

    // 257 small commands fit far below the configured 64 MiB request body
    // cap, but exceed the independent per-push ref-work bound.
    let mut too_many_commands = Vec::new();
    for index in 0..257 {
        let capability = if index == 0 { "\0report-status" } else { "" };
        let line = format!("{zero_oid} {fetched_tip} refs/heads/too-many-{index}{capability}\n");
        too_many_commands.extend_from_slice(&git_pkt_line(line.as_bytes()));
    }
    too_many_commands.extend_from_slice(b"0000");
    too_many_commands.extend_from_slice(EMPTY_GIT_PACK_V2);
    assert_eq!(
        raw_git_receive_post_status(server.addr(), &write_auth, &too_many_commands),
        413,
        "command count is bounded independently of Content-Length"
    );

    assert_eq!(newgit.refs.read("refs/main").unwrap(), s2);
    assert_eq!(newgit.objects.iter().unwrap(), clean_objects);
    assert_eq!(newgit.refs.list(None).unwrap(), clean_refs);

    // A read token cannot push. A write token can push one real fast-forward
    // commit through the ordinary Git smart-HTTP receive-pack exchange.
    git(&[
        "-C",
        clone_path_str,
        "config",
        "user.name",
        "Local Git User",
    ]);
    git(&[
        "-C",
        clone_path_str,
        "config",
        "user.email",
        "local@example.test",
    ]);
    std::fs::write(clone_path.join("local-only.txt"), b"not on server\n").unwrap();
    git(&["-C", clone_path_str, "add", "local-only.txt"]);
    git(&[
        "-C",
        clone_path_str,
        "commit",
        "--quiet",
        "-m",
        "local commit",
    ]);
    let objects_before_denied_push = newgit.objects.iter().unwrap();
    let refs_before_denied_push = newgit.refs.list(None).unwrap();
    let push_denied = git_fails(&[
        "-c",
        &auth_config,
        "-C",
        clone_path_str,
        "push",
        "origin",
        "main",
    ]);
    assert_eq!(newgit.refs.read("refs/main").unwrap(), s2);
    assert!(
        !String::from_utf8_lossy(&push_denied.stderr).is_empty(),
        "read-role push denial should be reported by Git"
    );
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_denied_push);
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_denied_push);

    let write_auth_config = format!("http.extraHeader=Authorization: {write_auth}");
    git(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--porcelain",
        "origin",
        "main",
    ]);
    let pushed_tip = newgit.refs.read("refs/main").unwrap();
    assert_ne!(pushed_tip, s2);
    let pushed_snapshot = newgit.objects.get(&pushed_tip).unwrap();
    let pushed_snapshot = pushed_snapshot.as_snapshot().unwrap();
    assert_eq!(pushed_snapshot.parents, vec![s2]);
    let pushed_tree = newgit.objects.get(&pushed_snapshot.root).unwrap();
    let pushed_tree = pushed_tree.as_tree().unwrap();
    let local_blob = pushed_tree.get("local-only.txt").unwrap();
    assert_eq!(
        newgit
            .objects
            .get(&local_blob.oid)
            .unwrap()
            .as_blob()
            .unwrap(),
        b"not on server\n"
    );

    // New Git branch names map to canonical NewGit refs/<name> refs.
    git(&[
        "-c",
        "protocol.version=1",
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--atomic",
        "origin",
        "main:refs/heads/published",
    ]);
    assert_eq!(newgit.refs.read("refs/published").unwrap(), pushed_tip);
    git(&[
        "-c",
        "protocol.version=2",
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--atomic",
        "origin",
        "main:refs/heads/v2-requested-fallback",
    ]);
    assert_eq!(
        newgit.refs.read("refs/v2-requested-fallback").unwrap(),
        pushed_tip
    );

    // One `git push --atomic` updates an existing branch, advances another
    // existing branch, and creates a third branch. Git's projection-side
    // all-or-none checks and NewGit's canonical transaction back the
    // advertised capability.
    std::fs::write(clone_path.join("multi-ref.txt"), b"one multi-ref push\n").unwrap();
    git(&["-C", clone_path_str, "add", "multi-ref.txt"]);
    git(&[
        "-C",
        clone_path_str,
        "commit",
        "--quiet",
        "-m",
        "multi-ref commit",
    ]);
    git(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--atomic",
        "origin",
        "main",
        "main:refs/heads/published",
        "main:refs/heads/multi-created",
    ]);
    let multi_ref_tip = newgit.refs.read("refs/main").unwrap();
    assert_eq!(newgit.refs.read("refs/published").unwrap(), multi_ref_tip);
    assert_eq!(
        newgit.refs.read("refs/multi-created").unwrap(),
        multi_ref_tip
    );
    let multi_ref_snapshot_object = newgit.objects.get(&multi_ref_tip).unwrap();
    let multi_ref_snapshot = multi_ref_snapshot_object.as_snapshot().unwrap();
    assert_eq!(multi_ref_snapshot.parents, vec![pushed_tip]);
    let multi_ref_tree_object = newgit.objects.get(&multi_ref_snapshot.root).unwrap();
    let multi_ref_tree = multi_ref_tree_object.as_tree().unwrap();
    let multi_ref_blob = multi_ref_tree.get("multi-ref.txt").unwrap();
    assert_eq!(
        newgit
            .objects
            .get(&multi_ref_blob.oid)
            .unwrap()
            .as_blob()
            .unwrap(),
        b"one multi-ref push\n"
    );

    // A fresh ordinary Git clone independently observes the canonical pushed
    // commit and both refs (not the disposable receive-pack projection).
    let post_push_clone = root.join("post-push-clone");
    git(&[
        "-c",
        "protocol.version=2",
        "-c",
        &auth_config,
        "clone",
        "--quiet",
        &url,
        post_push_clone.to_str().unwrap(),
    ]);
    git(&[
        "-c",
        "protocol.version=2",
        "-c",
        &auth_config,
        "-C",
        post_push_clone.to_str().unwrap(),
        "fetch",
        "--quiet",
        "origin",
    ]);
    assert_eq!(
        std::fs::read(post_push_clone.join("local-only.txt")).unwrap(),
        b"not on server\n"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            post_push_clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/main",
        ]))
        .trim(),
        as_text(&git(&[
            "-C",
            post_push_clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/published",
        ]))
        .trim()
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            post_push_clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/main",
        ]))
        .trim(),
        as_text(&git(&[
            "-C",
            post_push_clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/multi-created",
        ]))
        .trim()
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            post_push_clone.to_str().unwrap(),
            "cat-file",
            "-p",
            "refs/remotes/origin/multi-created:multi-ref.txt",
        ])),
        "one multi-ref push\n"
    );

    // Ordinary Git branch deletion removes only the requested canonical ref;
    // existing clones observe it with --prune and fresh clones never see it.
    let objects_before_delete = newgit.objects.iter().unwrap();
    git(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--delete",
        "origin",
        "published",
    ]);
    assert!(newgit.refs.read_opt("refs/published").unwrap().is_none());
    assert_eq!(newgit.refs.read("refs/main").unwrap(), multi_ref_tip);
    assert_eq!(
        newgit.refs.read("refs/multi-created").unwrap(),
        multi_ref_tip
    );
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_delete);
    git(&[
        "-c",
        &auth_config,
        "-C",
        post_push_clone.to_str().unwrap(),
        "fetch",
        "--prune",
        "--quiet",
        "origin",
    ]);
    git_fails(&[
        "-C",
        post_push_clone.to_str().unwrap(),
        "show-ref",
        "--verify",
        "--quiet",
        "refs/remotes/origin/published",
    ]);

    // One atomic real-Git request deletes multiple branches together.
    git(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "origin",
        "main:refs/heads/delete-a",
        "main:refs/heads/delete-b",
    ]);
    assert_eq!(newgit.refs.read("refs/delete-a").unwrap(), multi_ref_tip);
    assert_eq!(newgit.refs.read("refs/delete-b").unwrap(), multi_ref_tip);
    git(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--atomic",
        "--delete",
        "origin",
        "delete-a",
        "delete-b",
    ]);
    assert!(newgit.refs.read_opt("refs/delete-a").unwrap().is_none());
    assert!(newgit.refs.read_opt("refs/delete-b").unwrap().is_none());
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_delete);

    let post_delete_clone = root.join("post-delete-clone");
    git(&[
        "-c",
        &auth_config,
        "clone",
        "--quiet",
        &url,
        post_delete_clone.to_str().unwrap(),
    ]);
    git(&[
        "-c",
        &auth_config,
        "-C",
        post_delete_clone.to_str().unwrap(),
        "fetch",
        "--prune",
        "--quiet",
        "origin",
    ]);
    git_fails(&[
        "-C",
        post_delete_clone.to_str().unwrap(),
        "show-ref",
        "--verify",
        "--quiet",
        "refs/remotes/origin/published",
    ]);
    git_fails(&[
        "-C",
        post_delete_clone.to_str().unwrap(),
        "show-ref",
        "--verify",
        "--quiet",
        "refs/remotes/origin/delete-a",
    ]);
    assert_eq!(
        as_text(&git(&[
            "-C",
            post_delete_clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/main",
        ]))
        .trim(),
        as_text(&git(&[
            "-C",
            post_delete_clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/multi-created",
        ]))
        .trim()
    );

    // Annotated tag objects remain refused. Even when a forced branch update
    // and deletion are accepted in Git's projection, an unsupported annotated
    // tag rejects the atomic batch before canonical refs or objects move.
    git(&[
        "-C",
        clone_path_str,
        "tag",
        "-a",
        "forbidden-tag",
        "-m",
        "annotated tags are unsupported",
    ]);
    let refs_before_annotated_push = newgit.refs.list(None).unwrap();
    let objects_before_annotated_push = newgit.objects.iter().unwrap();
    let tag_push = git_fails(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "origin",
        "refs/tags/forbidden-tag",
    ]);
    assert!(!String::from_utf8_lossy(&tag_push.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_annotated_push);
    assert_eq!(
        newgit.objects.iter().unwrap(),
        objects_before_annotated_push
    );
    git(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "origin",
        "main:refs/heads/delete-rejected",
    ]);
    let objects_before_rejections = newgit.objects.iter().unwrap();
    let refs_before_rejections = newgit.refs.list(None).unwrap();
    let base_git_tip = as_text(&git(&["-C", clone_path_str, "rev-parse", "HEAD~1"]))
        .trim()
        .to_string();
    git(&[
        "-C",
        clone_path_str,
        "checkout",
        "-b",
        "divergent",
        &base_git_tip,
    ]);
    std::fs::write(clone_path.join("divergent.txt"), b"divergent\n").unwrap();
    git(&["-C", clone_path_str, "add", "divergent.txt"]);
    git(&[
        "-C",
        clone_path_str,
        "commit",
        "--quiet",
        "-m",
        "divergent commit",
    ]);
    let atomic_multi_push = git_fails(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--atomic",
        "--force",
        "origin",
        ":refs/heads/delete-rejected",
        "divergent:refs/heads/main",
        "refs/tags/forbidden-tag",
    ]);
    let atomic_multi_stderr = String::from_utf8_lossy(&atomic_multi_push.stderr);
    assert!(
        !atomic_multi_stderr.is_empty(),
        "Git should report the rejected atomic push containing an annotated tag"
    );
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_rejections);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_rejections);
    assert_eq!(
        newgit.refs.read("refs/delete-rejected").unwrap(),
        multi_ref_tip
    );

    server.shutdown();
}

#[test]
fn real_git_force_push_modes_use_old_tip_cas_and_reject_invalid_atomic_batch() {
    let (_dir, root) = temp("force-push-cas");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();
    let initial = commit(
        &newgit,
        "refs/main",
        "initial snapshot",
        &[(
            "README.md",
            b"initial\n",
            newgit::object::types::EntryMode::File,
        )],
        vec![],
    );
    let side = commit(
        &newgit,
        "refs/side",
        "side branch",
        &[(
            "side.txt",
            b"side\n",
            newgit::object::types::EntryMode::File,
        )],
        vec![initial],
    );
    assert_ne!(side, initial);
    newgit
        .set_head(
            &Head::Symbolic("refs/main".into()),
            RefLogEntry::system("set default branch"),
        )
        .unwrap();

    let tokens_path = root.join("tokens.json");
    let mut tokens = TokenFile::default();
    tokens.add("git-reader", READ_TOKEN, Role::Read).unwrap();
    tokens.add("git-writer", WRITE_TOKEN, Role::Write).unwrap();
    auth::save(&tokens_path, &tokens).unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path,
        token_file: tokens_path,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    let local = root.join("client");
    let local_str = local.to_str().unwrap();
    let read_auth = format!("http.extraHeader=Authorization: Bearer {READ_TOKEN}");
    let write_auth = format!("http.extraHeader=Authorization: Bearer {WRITE_TOKEN}");
    git(&[
        "-c", &read_auth, "clone", "--quiet", "--branch", "main", &url, local_str,
    ]);
    git(&["-C", local_str, "config", "user.name", "Local Git User"]);
    git(&[
        "-C",
        local_str,
        "config",
        "user.email",
        "local@example.test",
    ]);
    let initial_git_tip = as_text(&git(&["-C", local_str, "rev-parse", "HEAD"]))
        .trim()
        .to_string();

    // Advance the canonical server ref independently after the client's clone,
    // then create a local tip based on the now-stale initial snapshot.
    let server_tip = commit(
        &newgit,
        "refs/main",
        "independent server advance",
        &[(
            "server.txt",
            b"server advance\n",
            newgit::object::types::EntryMode::File,
        )],
        vec![initial],
    );
    let refs_before_rejections = newgit.refs.list(None).unwrap();
    let objects_before_rejections = newgit.objects.iter().unwrap();
    git(&[
        "-C",
        local_str,
        "checkout",
        "-b",
        "lease-candidate",
        &initial_git_tip,
    ]);
    std::fs::write(local.join("lease.txt"), b"force-with-lease content\n").unwrap();
    git(&["-C", local_str, "add", "lease.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "divergent lease candidate",
    ]);

    // Ordinary Git refuses a stale non-fast-forward push; a lease naming the
    // stale initial tip is also rejected without any canonical mutation.
    let ordinary = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "lease-candidate:refs/heads/main",
    ]);
    assert!(!String::from_utf8_lossy(&ordinary.stderr).is_empty());
    let stale_lease = format!("--force-with-lease=refs/heads/main:{initial_git_tip}");
    let lease_mismatch = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        &stale_lease,
        "origin",
        "lease-candidate:refs/heads/main",
    ]);
    assert!(!String::from_utf8_lossy(&lease_mismatch.stderr).is_empty());
    assert_eq!(newgit.refs.read("refs/main").unwrap(), server_tip);
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_rejections);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_rejections);

    // A matching lease permits the non-fast-forward update. The wire old OID
    // maps to the canonical tip, which NewGit rechecks under its txn lock.
    git(&[
        "-c", &read_auth, "-C", local_str, "fetch", "--quiet", "origin",
    ]);
    let advertised_git_tip = as_text(&git(&[
        "-C",
        local_str,
        "rev-parse",
        "refs/remotes/origin/main",
    ]))
    .trim()
    .to_string();
    assert_ne!(advertised_git_tip, initial_git_tip);
    let matching_lease = format!("--force-with-lease=refs/heads/main:{advertised_git_tip}");
    git(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        &matching_lease,
        "origin",
        "lease-candidate:refs/heads/main",
    ]);
    let lease_tip = newgit.refs.read("refs/main").unwrap();
    assert_ne!(lease_tip, server_tip);
    let lease_snapshot = newgit.objects.get(&lease_tip).unwrap();
    let lease_snapshot = lease_snapshot.as_snapshot().unwrap();
    assert_eq!(lease_snapshot.parents, vec![initial]);
    let lease_tree = newgit.objects.get(&lease_snapshot.root).unwrap();
    let lease_tree = lease_tree.as_tree().unwrap();
    let lease_blob = lease_tree.get("lease.txt").unwrap();
    assert_eq!(
        newgit
            .objects
            .get(&lease_blob.oid)
            .unwrap()
            .as_blob()
            .unwrap(),
        b"force-with-lease content\n"
    );

    // Plain --force also accepts a divergent branch update; its old advertised
    // tip is still protected by the same canonical ref CAS.
    git(&[
        "-C",
        local_str,
        "checkout",
        "--force",
        "-b",
        "force-candidate",
        &initial_git_tip,
    ]);
    std::fs::write(local.join("force.txt"), b"explicit force content\n").unwrap();
    git(&["-C", local_str, "add", "force.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "divergent force candidate",
    ]);
    git(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "--force",
        "origin",
        "force-candidate:refs/heads/main",
    ]);
    let force_tip = newgit.refs.read("refs/main").unwrap();
    let force_snapshot = newgit.objects.get(&force_tip).unwrap();
    let force_snapshot = force_snapshot.as_snapshot().unwrap();
    assert_eq!(force_snapshot.parents, vec![initial]);
    let force_tree = newgit.objects.get(&force_snapshot.root).unwrap();
    let force_tree = force_tree.as_tree().unwrap();
    let force_blob = force_tree.get("force.txt").unwrap();
    assert_eq!(
        newgit
            .objects
            .get(&force_blob.oid)
            .unwrap()
            .as_blob()
            .unwrap(),
        b"explicit force content\n"
    );

    // An invalid annotated tag makes a forced, atomic branch update plus branch
    // deletion fail as a whole before canonical refs or objects are promoted.
    git(&[
        "-C",
        local_str,
        "checkout",
        "--force",
        "-b",
        "atomic-candidate",
        &initial_git_tip,
    ]);
    std::fs::write(local.join("atomic.txt"), b"must not publish\n").unwrap();
    git(&["-C", local_str, "add", "atomic.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "atomic rejected candidate",
    ]);
    git(&[
        "-C",
        local_str,
        "tag",
        "-a",
        "atomic-rejected",
        "-m",
        "annotated tags are unsupported",
    ]);
    let refs_before_atomic = newgit.refs.list(None).unwrap();
    let objects_before_atomic = newgit.objects.iter().unwrap();
    let atomic_rejected = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "--atomic",
        "--force",
        "origin",
        "atomic-candidate:refs/heads/main",
        ":refs/heads/side",
        "refs/tags/atomic-rejected",
    ]);
    assert!(!String::from_utf8_lossy(&atomic_rejected.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_atomic);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_atomic);
    assert_eq!(newgit.refs.read("refs/main").unwrap(), force_tip);
    assert_eq!(newgit.refs.read("refs/side").unwrap(), side);

    // Fresh Git clone plus fetch sees the accepted forced ref and its content,
    // not any branch, tag, or object from the rejected atomic batch.
    let post_force_clone = root.join("post-force-clone");
    git(&[
        "-c",
        &read_auth,
        "clone",
        "--quiet",
        "--branch",
        "main",
        &url,
        post_force_clone.to_str().unwrap(),
    ]);
    git(&[
        "-c",
        &read_auth,
        "-C",
        post_force_clone.to_str().unwrap(),
        "fetch",
        "--prune",
        "--quiet",
        "origin",
    ]);
    assert_eq!(
        as_text(&git(&[
            "-C",
            post_force_clone.to_str().unwrap(),
            "show",
            "refs/remotes/origin/main:force.txt",
        ])),
        "explicit force content\n"
    );
    git_fails(&[
        "-C",
        post_force_clone.to_str().unwrap(),
        "show",
        "refs/remotes/origin/main:atomic.txt",
    ]);
    git(&[
        "-C",
        post_force_clone.to_str().unwrap(),
        "show-ref",
        "--verify",
        "--quiet",
        "refs/remotes/origin/side",
    ]);
    git_fails(&[
        "-C",
        post_force_clone.to_str().unwrap(),
        "show-ref",
        "--verify",
        "--quiet",
        "refs/tags/atomic-rejected",
    ]);
    server.shutdown();
}

#[test]
fn real_git_lightweight_tag_pushes_are_transactional_and_bounded() {
    let (_dir, root) = temp("lightweight-tag-push");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();
    let initial = commit(
        &newgit,
        "refs/main",
        "initial snapshot",
        &[(
            "README.md",
            b"before tag push\n",
            newgit::object::types::EntryMode::File,
        )],
        vec![],
    );
    newgit
        .set_head(
            &Head::Symbolic("refs/main".into()),
            RefLogEntry::system("set default branch"),
        )
        .unwrap();

    let tokens_path = root.join("tokens.json");
    let mut tokens = TokenFile::default();
    tokens.add("git-reader", READ_TOKEN, Role::Read).unwrap();
    tokens.add("git-writer", WRITE_TOKEN, Role::Write).unwrap();
    auth::save(&tokens_path, &tokens).unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path,
        token_file: tokens_path,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    let local = root.join("local");
    let read_auth = format!("http.extraHeader=Authorization: Bearer {READ_TOKEN}");
    let write_auth = format!("http.extraHeader=Authorization: Bearer {WRITE_TOKEN}");
    git(&[
        "-c",
        &read_auth,
        "clone",
        "--quiet",
        &url,
        local.to_str().unwrap(),
    ]);
    let initial_git_tip = as_text(&git(&[
        "-C",
        local.to_str().unwrap(),
        "rev-parse",
        "refs/remotes/origin/main",
    ]))
    .trim()
    .to_string();
    git(&[
        "-C",
        local.to_str().unwrap(),
        "config",
        "user.name",
        "Tag Test",
    ]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "config",
        "user.email",
        "tag-test@example.test",
    ]);

    // A read-role token cannot create tags or change canonical refs/objects.
    git(&["-C", local.to_str().unwrap(), "tag", "unauthorized-tag"]);
    let refs_before_denied = newgit.refs.list(None).unwrap();
    let objects_before_denied = newgit.objects.iter().unwrap();
    let denied = git_fails(&[
        "-c",
        &read_auth,
        "-C",
        local.to_str().unwrap(),
        "push",
        "origin",
        "refs/tags/unauthorized-tag",
    ]);
    assert!(!String::from_utf8_lossy(&denied.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_denied);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_denied);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "tag",
        "-d",
        "unauthorized-tag",
    ]);

    // A branch fast-forward and new lightweight tag share one atomic push and
    // map back to the canonical NewGit snapshot ID.
    std::fs::write(local.join("tagged.txt"), b"lightweight tag content\n").unwrap();
    git(&["-C", local.to_str().unwrap(), "add", "tagged.txt"]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "commit",
        "--quiet",
        "-m",
        "release snapshot",
    ]);
    git(&["-C", local.to_str().unwrap(), "tag", "v2.0"]);
    git(&[
        "-c",
        &write_auth,
        "-C",
        local.to_str().unwrap(),
        "push",
        "--atomic",
        "origin",
        "main",
        "refs/tags/v2.0",
    ]);
    let pushed_tip = newgit.refs.read("refs/main").unwrap();
    assert_eq!(newgit.refs.read("refs/tags/v2.0").unwrap(), pushed_tip);
    assert_eq!(
        newgit
            .objects
            .get(&pushed_tip)
            .unwrap()
            .as_snapshot()
            .unwrap()
            .parents,
        vec![initial]
    );
    let pushed_snapshot = newgit.objects.get(&pushed_tip).unwrap();
    let pushed_tree = newgit
        .objects
        .get(&pushed_snapshot.as_snapshot().unwrap().root)
        .unwrap();
    let tagged_blob = pushed_tree.as_tree().unwrap().get("tagged.txt").unwrap();
    assert_eq!(
        newgit
            .objects
            .get(&tagged_blob.oid)
            .unwrap()
            .as_blob()
            .unwrap(),
        b"lightweight tag content\n"
    );

    // An ordinary Git clone/fetch sees the committed tag and its actual tree.
    let clone = root.join("post-tag-push-clone");
    git(&[
        "-c",
        &read_auth,
        "clone",
        "--quiet",
        &url,
        clone.to_str().unwrap(),
    ]);
    git(&[
        "-c",
        &read_auth,
        "-C",
        clone.to_str().unwrap(),
        "fetch",
        "--quiet",
        "origin",
    ]);
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone.to_str().unwrap(),
            "rev-parse",
            "refs/tags/v2.0",
        ]))
        .trim(),
        as_text(&git(&[
            "-C",
            clone.to_str().unwrap(),
            "rev-parse",
            "refs/remotes/origin/main",
        ]))
        .trim()
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone.to_str().unwrap(),
            "cat-file",
            "-t",
            "refs/tags/v2.0",
        ]))
        .trim(),
        "commit"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone.to_str().unwrap(),
            "show",
            "refs/tags/v2.0:tagged.txt",
        ])),
        "lightweight tag content\n"
    );

    // An annotated tag points at a Git tag object, which NewGit cannot
    // represent. Even paired with a valid branch advance, it promotes neither
    // the branch nor the pack's new objects.
    std::fs::write(local.join("unpublished.txt"), b"must stay local\n").unwrap();
    git(&["-C", local.to_str().unwrap(), "add", "unpublished.txt"]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "commit",
        "--quiet",
        "-m",
        "unpublished branch advance",
    ]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "tag",
        "-a",
        "annotated-denied",
        "-m",
        "annotated tags are unsupported",
    ]);
    let refs_before_annotated = newgit.refs.list(None).unwrap();
    let objects_before_annotated = newgit.objects.iter().unwrap();
    let annotated = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local.to_str().unwrap(),
        "push",
        "--atomic",
        "origin",
        "main",
        "refs/tags/annotated-denied",
    ]);
    assert!(!String::from_utf8_lossy(&annotated.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_annotated);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_annotated);
    assert_eq!(newgit.refs.read("refs/main").unwrap(), pushed_tip);

    // Git's receive.denyNonFastForwards setting does not protect forced tag
    // retargets. NewGit refuses all retargeting before projection import.
    git(&[
        "-C",
        local.to_str().unwrap(),
        "tag",
        "--force",
        "v2.0",
        &initial_git_tip,
    ]);
    let refs_before_retarget = newgit.refs.list(None).unwrap();
    let objects_before_retarget = newgit.objects.iter().unwrap();
    let retarget = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local.to_str().unwrap(),
        "push",
        "--force",
        "origin",
        "refs/tags/v2.0",
    ]);
    assert!(!String::from_utf8_lossy(&retarget.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_retarget);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_retarget);
    assert_eq!(newgit.refs.read("refs/tags/v2.0").unwrap(), pushed_tip);

    // Tag deletion is transactional too; clones observe it after pruning.
    let objects_before_delete = newgit.objects.iter().unwrap();
    git(&[
        "-c",
        &write_auth,
        "-C",
        local.to_str().unwrap(),
        "push",
        "--delete",
        "origin",
        "v2.0",
    ]);
    assert!(newgit.refs.read_opt("refs/tags/v2.0").unwrap().is_none());
    assert_eq!(newgit.refs.read("refs/main").unwrap(), pushed_tip);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_delete);
    git(&[
        "-c",
        &read_auth,
        "-C",
        clone.to_str().unwrap(),
        "fetch",
        "--prune",
        "--prune-tags",
        "--quiet",
        "origin",
    ]);
    git_fails(&[
        "-C",
        clone.to_str().unwrap(),
        "show-ref",
        "--verify",
        "--quiet",
        "refs/tags/v2.0",
    ]);
    let remote_tags = git(&["-c", &read_auth, "ls-remote", "--tags", &url]);
    assert!(!as_text(&remote_tags).contains("refs/tags/v2.0"));
    server.shutdown();
}

#[test]
fn real_git_protected_refs_require_admin_and_reject_mixed_pushes_before_promotion() {
    let (_dir, root) = temp("protected-refs");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();
    let initial = commit(
        &newgit,
        "refs/main",
        "initial snapshot",
        &[(
            "README.md",
            b"protected-ref baseline\n",
            newgit::object::types::EntryMode::File,
        )],
        vec![],
    );
    newgit
        .set_head(
            &Head::Symbolic("refs/main".into()),
            RefLogEntry::system("set protected-ref test HEAD"),
        )
        .unwrap();

    let tokens_path = root.join("tokens.json");
    let mut tokens = TokenFile::default();
    tokens.add("git-reader", READ_TOKEN, Role::Read).unwrap();
    tokens.add("git-writer", WRITE_TOKEN, Role::Write).unwrap();
    tokens.add("git-admin", ADMIN_TOKEN, Role::Admin).unwrap();
    auth::save(&tokens_path, &tokens).unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path,
        token_file: tokens_path,
        protected_refs: [
            "refs/heads/main".to_string(),
            "refs/heads/release".to_string(),
            "refs/tags/release".to_string(),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    let local = root.join("local");
    let local_str = local.to_str().unwrap();
    let read_auth = format!("http.extraHeader=Authorization: Bearer {READ_TOKEN}");
    let write_auth = format!("http.extraHeader=Authorization: Bearer {WRITE_TOKEN}");
    let admin_auth = format!("http.extraHeader=Authorization: Bearer {ADMIN_TOKEN}");
    git(&["-c", &read_auth, "clone", "--quiet", &url, local_str]);
    let listing = git(&["-c", &read_auth, "ls-remote", &url]);
    assert!(as_text(&listing).contains("refs/heads/main"));
    git(&["-C", local_str, "config", "user.name", "Protected Ref Test"]);
    git(&[
        "-C",
        local_str,
        "config",
        "user.email",
        "protected-ref@example.test",
    ]);

    // Protection is exact, not a prefix rule: a neighboring branch remains writable.
    git(&["-C", local_str, "checkout", "-b", "mainline"]);
    std::fs::write(local.join("mainline.txt"), b"exact-name neighbor\n").unwrap();
    git(&["-C", local_str, "add", "mainline.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "mainline update",
    ]);
    git(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "mainline:refs/heads/mainline",
    ]);
    let mainline_tip = newgit.refs.read("refs/mainline").unwrap();

    // A valid writer may not update the protected default branch. Rejection
    // precedes projection/import, so even incoming objects are not promoted.
    git(&["-C", local_str, "checkout", "-b", "protected-main", "main"]);
    std::fs::write(local.join("protected.txt"), b"not yet published\n").unwrap();
    git(&["-C", local_str, "add", "protected.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "protected main candidate",
    ]);
    let refs_before_update_denial = newgit.refs.list(None).unwrap();
    let objects_before_update_denial = newgit.objects.iter().unwrap();
    let denied_update = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "protected-main:refs/heads/main",
    ]);
    assert!(!String::from_utf8_lossy(&denied_update.stderr).is_empty());
    assert_eq!(newgit.refs.read("refs/main").unwrap(), initial);
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_update_denial);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_update_denial);
    let command = b"1111111111111111111111111111111111111111 2222222222222222222222222222222222222222 refs/heads/main\0report-status\n";
    let mut raw_update = format!("{:04x}", command.len() + 4).into_bytes();
    raw_update.extend_from_slice(command);
    raw_update.extend_from_slice(b"0000");
    raw_update.extend_from_slice(EMPTY_GIT_PACK_V2);
    assert_eq!(
        raw_git_receive_post_status(server.addr(), &format!("Bearer {WRITE_TOKEN}"), &raw_update),
        403,
        "an authenticated writer's protected-ref change is Forbidden, not Unauthorized"
    );
    // The adapter maps this valid branch name to the same canonical NewGit ref
    // as protected `refs/tags/release`; the alias must not bypass authorization.
    let refs_before_alias_denial = newgit.refs.list(None).unwrap();
    let objects_before_alias_denial = newgit.objects.iter().unwrap();
    let denied_alias = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "main:refs/heads/tags/release",
    ]);
    assert!(!String::from_utf8_lossy(&denied_alias.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_alias_denial);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_alias_denial);

    // Protected creates and deletes receive the same admin gate; admin can
    // create and later delete that exact ref.
    let refs_before_create_denial = newgit.refs.list(None).unwrap();
    let objects_before_create_denial = newgit.objects.iter().unwrap();
    let denied_create = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "main:refs/heads/release",
    ]);
    assert!(!String::from_utf8_lossy(&denied_create.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_create_denial);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_create_denial);
    git(&[
        "-c",
        &admin_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "main:refs/heads/release",
    ]);
    assert_eq!(newgit.refs.read("refs/release").unwrap(), initial);

    let refs_before_delete_denial = newgit.refs.list(None).unwrap();
    let objects_before_delete_denial = newgit.objects.iter().unwrap();
    let denied_delete = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "--delete",
        "origin",
        "release",
    ]);
    assert!(!String::from_utf8_lossy(&denied_delete.stderr).is_empty());
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_delete_denial);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_delete_denial);
    git(&[
        "-c",
        &admin_auth,
        "-C",
        local_str,
        "push",
        "--delete",
        "origin",
        "release",
    ]);
    assert!(newgit.refs.read_opt("refs/release").unwrap().is_none());

    // Exact protected lightweight tags use the same role policy. Denied
    // creation cannot promote the unpushed protected-main commit; admin may
    // create and later delete the tag.
    let refs_before_tag_create_denial = newgit.refs.list(None).unwrap();
    let objects_before_tag_create_denial = newgit.objects.iter().unwrap();
    let denied_tag_create = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "protected-main:refs/tags/release",
    ]);
    assert!(!String::from_utf8_lossy(&denied_tag_create.stderr).is_empty());
    assert_eq!(
        newgit.refs.list(None).unwrap(),
        refs_before_tag_create_denial
    );
    assert_eq!(
        newgit.objects.iter().unwrap(),
        objects_before_tag_create_denial
    );
    git(&[
        "-c",
        &admin_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "protected-main:refs/tags/release",
    ]);
    let protected_tag_tip = newgit.refs.read("refs/tags/release").unwrap();
    assert_eq!(
        newgit
            .objects
            .get(&protected_tag_tip)
            .unwrap()
            .as_snapshot()
            .unwrap()
            .message
            .trim_end(),
        "protected main candidate"
    );

    let refs_before_tag_delete_denial = newgit.refs.list(None).unwrap();
    let objects_before_tag_delete_denial = newgit.objects.iter().unwrap();
    let denied_tag_delete = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "--delete",
        "origin",
        "refs/tags/release",
    ]);
    assert!(!String::from_utf8_lossy(&denied_tag_delete.stderr).is_empty());
    assert_eq!(
        newgit.refs.list(None).unwrap(),
        refs_before_tag_delete_denial
    );
    assert_eq!(
        newgit.objects.iter().unwrap(),
        objects_before_tag_delete_denial
    );
    git(&[
        "-c",
        &admin_auth,
        "-C",
        local_str,
        "push",
        "--delete",
        "origin",
        "refs/tags/release",
    ]);
    assert!(newgit.refs.read_opt("refs/tags/release").unwrap().is_none());

    // Admin may update a protected ref, independent of the writer's refusal.
    git(&[
        "-c",
        &admin_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "protected-main:refs/heads/main",
    ]);
    let accepted_main_tip = newgit.refs.read("refs/main").unwrap();
    assert_ne!(accepted_main_tip, initial);

    // A mixed atomic request containing one protected change is denied before
    // either its protected or ordinary sibling ref/object is committed.
    git(&["-C", local_str, "checkout", "mainline"]);
    std::fs::write(
        local.join("mainline-next.txt"),
        b"atomic sibling candidate\n",
    )
    .unwrap();
    git(&["-C", local_str, "add", "mainline-next.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "mainline atomic candidate",
    ]);
    git(&["-C", local_str, "checkout", "protected-main"]);
    std::fs::write(local.join("protected-next.txt"), b"admin-only follow-up\n").unwrap();
    git(&["-C", local_str, "add", "protected-next.txt"]);
    git(&[
        "-C",
        local_str,
        "commit",
        "--quiet",
        "-m",
        "second protected candidate",
    ]);
    let refs_before_atomic_denial = newgit.refs.list(None).unwrap();
    let objects_before_atomic_denial = newgit.objects.iter().unwrap();
    let denied_atomic = git_fails(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "--atomic",
        "origin",
        "protected-main:refs/heads/main",
        "mainline:refs/heads/mainline",
    ]);
    assert!(!String::from_utf8_lossy(&denied_atomic.stderr).is_empty());
    assert_eq!(newgit.refs.read("refs/main").unwrap(), accepted_main_tip);
    assert_eq!(newgit.refs.read("refs/mainline").unwrap(), mainline_tip);
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_atomic_denial);
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_atomic_denial);

    // The same writer remains authorized to update the exact unprotected sibling.
    git(&[
        "-c",
        &write_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "mainline:refs/heads/mainline",
    ]);
    assert_ne!(newgit.refs.read("refs/mainline").unwrap(), mainline_tip);
    git(&[
        "-c",
        &admin_auth,
        "-C",
        local_str,
        "push",
        "origin",
        "protected-main:refs/heads/main",
    ]);
    let final_main_tip = newgit.refs.read("refs/main").unwrap();
    assert_ne!(final_main_tip, accepted_main_tip);
    assert_eq!(
        newgit
            .objects
            .get(&final_main_tip)
            .unwrap()
            .as_snapshot()
            .unwrap()
            .message
            .trim_end(),
        "second protected candidate"
    );
    server.shutdown();
}

#[test]
fn real_git_initial_push_to_empty_newgit_repo_creates_main() {
    let (_dir, root) = temp("empty-first-push");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();

    let tokens_path = root.join("tokens.json");
    let mut tokens = TokenFile::default();
    tokens.add("git-writer", WRITE_TOKEN, Role::Write).unwrap();
    auth::save(&tokens_path, &tokens).unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path,
        token_file: tokens_path,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    let local = root.join("local");
    git(&[
        "init",
        "--quiet",
        "--initial-branch=main",
        local.to_str().unwrap(),
    ]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "config",
        "user.name",
        "Initial User",
    ]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "config",
        "user.email",
        "initial@example.test",
    ]);
    std::fs::write(local.join("first.txt"), b"first pushed snapshot\n").unwrap();
    git(&["-C", local.to_str().unwrap(), "add", "first.txt"]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "commit",
        "--quiet",
        "-m",
        "initial push",
    ]);
    git(&[
        "-C",
        local.to_str().unwrap(),
        "remote",
        "add",
        "origin",
        &url,
    ]);
    let auth_config = format!("http.extraHeader=Authorization: Bearer {WRITE_TOKEN}");
    git(&[
        "-c",
        &auth_config,
        "-C",
        local.to_str().unwrap(),
        "push",
        "origin",
        "HEAD:refs/heads/main",
    ]);

    let canonical_tip = newgit.refs.read("refs/main").unwrap();
    let snapshot = newgit.objects.get(&canonical_tip).unwrap();
    let snapshot = snapshot.as_snapshot().unwrap();
    assert!(snapshot.parents.is_empty());
    let tree = newgit.objects.get(&snapshot.root).unwrap();
    let tree = tree.as_tree().unwrap();
    let entry = tree.get("first.txt").unwrap();
    assert_eq!(
        newgit.objects.get(&entry.oid).unwrap().as_blob().unwrap(),
        b"first pushed snapshot\n"
    );

    let clone = root.join("clone");
    git(&[
        "-c",
        &auth_config,
        "clone",
        "--quiet",
        &url,
        clone.to_str().unwrap(),
    ]);
    assert_eq!(
        std::fs::read(clone.join("first.txt")).unwrap(),
        b"first pushed snapshot\n"
    );
    server.shutdown();
}

#[test]
fn real_git_shallow_clone_deepen_unshallow_and_pull_over_smart_http() {
    let (_dir, root) = temp("shallow-history");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();

    let first = commit(
        &newgit,
        "refs/main",
        "first shallow-history snapshot",
        &[("history.txt", b"first\n", EntryMode::File)],
        vec![],
    );
    let second = commit(
        &newgit,
        "refs/main",
        "second shallow-history snapshot",
        &[("history.txt", b"second\n", EntryMode::File)],
        vec![first],
    );
    let third = commit(
        &newgit,
        "refs/main",
        "third shallow-history snapshot",
        &[("history.txt", b"third\n", EntryMode::File)],
        vec![second],
    );
    newgit
        .set_head(
            &Head::Symbolic("refs/main".into()),
            RefLogEntry::system("set shallow-test default branch"),
        )
        .unwrap();

    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path,
        allow_anonymous_read: true,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    let clone = root.join("shallow-clone");
    let clone_path = clone.to_str().unwrap();

    // The source is a live loopback HTTP peer, not Git's local/file transport,
    // which refuses shallow clones unless explicitly overridden.
    git(&["clone", "--quiet", "--depth=1", &url, clone_path]);
    let shallow_tip = as_text(&git(&["-C", clone_path, "rev-parse", "HEAD"]))
        .trim()
        .to_string();
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "rev-parse",
            "--is-shallow-repository"
        ]))
        .trim(),
        "true"
    );
    assert_eq!(
        as_text(&git(&["-C", clone_path, "rev-list", "--count", "HEAD"])).trim(),
        "1"
    );
    assert_eq!(
        std::fs::read_to_string(clone.join(".git/shallow")).unwrap(),
        format!("{shallow_tip}\n")
    );
    assert_eq!(
        as_text(&git(&["-C", clone_path, "show", "HEAD:history.txt"])),
        "third\n"
    );
    assert_eq!(newgit.refs.read("refs/main").unwrap(), third);

    // A stateless upload-pack request must honor the client's shallow boundary
    // and extend it by exactly one parent without changing the checked-out tip.
    git(&["-C", clone_path, "fetch", "--quiet", "--deepen=1", "origin"]);
    assert_eq!(
        as_text(&git(&["-C", clone_path, "rev-parse", "HEAD"])).trim(),
        shallow_tip
    );
    assert_eq!(
        as_text(&git(&["-C", clone_path, "rev-list", "--count", "HEAD"])).trim(),
        "2"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "show",
            "-s",
            "--format=%s",
            "HEAD^"
        ]))
        .trim(),
        "second shallow-history snapshot"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "rev-parse",
            "--is-shallow-repository"
        ]))
        .trim(),
        "true"
    );

    // Unshallow retrieves the remaining ancestry and removes the shallow
    // boundary; normal subsequent fetch and pull continue to work.
    git(&[
        "-C",
        clone_path,
        "fetch",
        "--quiet",
        "--unshallow",
        "origin",
    ]);
    assert_eq!(
        as_text(&git(&["-C", clone_path, "rev-list", "--count", "HEAD"])).trim(),
        "3"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "rev-parse",
            "--is-shallow-repository"
        ]))
        .trim(),
        "false"
    );
    assert!(!clone.join(".git/shallow").exists());
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "show",
            "-s",
            "--format=%s",
            "HEAD~2"
        ]))
        .trim(),
        "first shallow-history snapshot"
    );

    let fourth = commit(
        &newgit,
        "refs/main",
        "fourth shallow-history snapshot",
        &[("history.txt", b"fourth\n", EntryMode::File)],
        vec![third],
    );
    git(&["-C", clone_path, "fetch", "--quiet", "origin"]);
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "show",
            "-s",
            "--format=%s",
            "refs/remotes/origin/main"
        ]))
        .trim(),
        "fourth shallow-history snapshot"
    );
    git(&["-C", clone_path, "pull", "--quiet", "--ff-only"]);
    assert_eq!(
        as_text(&git(&["-C", clone_path, "rev-parse", "HEAD"])).trim(),
        as_text(&git(&[
            "-C",
            clone_path,
            "rev-parse",
            "refs/remotes/origin/main"
        ]))
        .trim()
    );
    assert_eq!(
        as_text(&git(&["-C", clone_path, "rev-list", "--count", "HEAD"])).trim(),
        "4"
    );
    assert_eq!(
        std::fs::read(clone.join("history.txt")).unwrap(),
        b"fourth\n"
    );
    assert_eq!(newgit.refs.read("refs/main").unwrap(), fourth);
    server.shutdown();
}

#[test]
fn real_git_partial_clone_omits_blobs_and_lazily_fetches_checkout_content() {
    let (_dir, root) = temp("partial-clone");
    let remote_path = root.join("newgit");
    std::fs::create_dir(&remote_path).unwrap();
    let newgit = Repo::init(&remote_path).unwrap();
    let mut payloads = Vec::new();
    let mut parent = None;
    for (index, seed) in [0x1234_5678_u64, 0x9abc_def0, 0xfeed_beef]
        .into_iter()
        .enumerate()
    {
        let mut state = seed;
        let payload = (0..256 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect::<Vec<_>>();
        let tip = commit(
            &newgit,
            "refs/main",
            &format!("partial-clone snapshot {index}"),
            &[("large.bin", &payload, EntryMode::File)],
            parent.into_iter().collect(),
        );
        parent = Some(tip);
        payloads.push(payload);
    }
    newgit
        .set_head(
            &Head::Symbolic("refs/main".into()),
            RefLogEntry::system("set partial-clone default branch"),
        )
        .unwrap();
    let server = server::spawn(ServerConfig {
        bind: "127.0.0.1:0".into(),
        repo_root: remote_path,
        allow_anonymous_read: true,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}/", server.addr());
    let clone = root.join("partial-clone");
    let clone_path = clone.to_str().unwrap();

    // Avoid checkout during clone so blob:none has an observable missing-blob
    // state before Git's normal checkout path performs promisor hydration.
    git(&[
        "clone",
        "--quiet",
        "--no-checkout",
        "--filter=blob:none",
        &url,
        clone_path,
    ]);
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "config",
            "--get",
            "remote.origin.promisor"
        ]))
        .trim(),
        "true"
    );
    assert_eq!(
        as_text(&git(&[
            "-C",
            clone_path,
            "config",
            "--get",
            "remote.origin.partialCloneFilter"
        ]))
        .trim(),
        "blob:none"
    );
    assert!(std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry
            .path()
            .extension()
            .is_some_and(|ext| ext == "promisor")));

    let blob_oids = ["HEAD~2", "HEAD~1", "HEAD"]
        .into_iter()
        .map(|revision| {
            as_text(&git(&[
                "-C",
                clone_path,
                "ls-tree",
                revision,
                "--",
                "large.bin",
            ]))
            .split_whitespace()
            .nth(2)
            .unwrap()
            .to_string()
        })
        .collect::<Vec<_>>();
    let missing = || {
        as_text(&git(&[
            "-C",
            clone_path,
            "rev-list",
            "--objects",
            "--missing=print",
            "--no-object-names",
            "HEAD",
        ]))
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>()
    };
    let missing_after_clone = missing();
    for oid in &blob_oids {
        assert!(
            missing_after_clone.contains(oid),
            "blob {oid} should be omitted before checkout; missing objects: {missing_after_clone:?}"
        );
    }

    // Checkout is an ordinary Git operation: it asks the promisor remote for
    // the current blob by object ID. Older versions remain absent locally.
    git(&["-C", clone_path, "checkout", "--quiet", "main"]);
    assert_eq!(std::fs::read(clone.join("large.bin")).unwrap(), payloads[2]);
    let missing_after_checkout = missing();
    assert!(!missing_after_checkout.contains(&blob_oids[2]));
    assert!(missing_after_checkout.contains(&blob_oids[0]));
    assert!(missing_after_checkout.contains(&blob_oids[1]));
    server.shutdown();
}
