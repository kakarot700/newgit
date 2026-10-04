//! Black-box smart-HTTP interoperability with the installed Git client.
//!
//! The HTTP peer is the real NewGit server. Git clone/fetch/pull/push/ls-remote
//! run as child processes over loopback TCP; assertions inspect actual Git
//! refs, commits, trees, and blob bytes rather than canned protocol replies.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};

use newgit::object::types::{Actor, ActorKind, EntryMode, Object, Snapshot};
use newgit::object::ObjectId;
use newgit::remote::auth::{self, Role, TokenFile};
use newgit::remote::server::{self, ServerConfig};
use newgit::repo::txn::{Cas, RefLogEntry};
use newgit::repo::{Head, Repo};
use tempfile::TempDir;

const READ_TOKEN: &str = "git-http-read-test-token";
const WRITE_TOKEN: &str = "git-http-write-test-token";

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
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
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
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let headers = String::from_utf8_lossy(&response);
    headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
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
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap()
}

fn raw_git_receive_advertisement(addr: SocketAddr, authorization: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET /info/refs?service=git-receive-pack HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {authorization}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let headers_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    assert!(response[..headers_end].starts_with(b"HTTP/1.1 200"));
    response[headers_end + 4..].to_vec()
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
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    String::from_utf8_lossy(&response)
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
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
    let hook_path = hooks_dir.join("post-checkout");
    std::fs::write(
        &hook_path,
        format!("#!/bin/sh\nprintf ran > '{}'\n", hook_marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let child = Command::new(std::env::current_exe().unwrap())
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

    // Malformed receive-pack packets fail before any canonical NewGit mutation.
    let clean_objects = newgit.objects.iter().unwrap();
    let clean_refs = newgit.refs.list(None).unwrap();
    assert_eq!(
        raw_git_receive_post_status(server.addr(), &write_auth, b"000x"),
        400
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
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "origin",
        "main:refs/heads/published",
    ]);
    assert_eq!(newgit.refs.read("refs/published").unwrap(), pushed_tip);

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

    // Annotated tag objects and forced non-fast-forward branch updates remain
    // refused. A branch whose deletion is combined with a failing update must
    // not be partially deleted.
    git(&[
        "-C",
        clone_path_str,
        "tag",
        "-a",
        "forbidden-tag",
        "-m",
        "annotated tags are unsupported",
    ]);
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
    let non_ff = git_fails(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--force",
        "origin",
        "divergent:main",
    ]);
    assert!(!String::from_utf8_lossy(&non_ff.stderr).is_empty());
    assert_eq!(newgit.refs.read("refs/main").unwrap(), multi_ref_tip);
    assert!(newgit.refs.read_opt("refs/published").unwrap().is_none());

    // Git accepts deletion in its disposable projection but refuses the
    // forced non-fast-forward update. The adapter rejects the whole request
    // instead of reporting partial success or deleting the canonical branch.
    let partial_multi_push = git_fails(&[
        "-c",
        &write_auth_config,
        "-C",
        clone_path_str,
        "push",
        "--force",
        "origin",
        "divergent:refs/heads/main",
        ":refs/heads/delete-rejected",
    ]);
    let partial_multi_stderr = String::from_utf8_lossy(&partial_multi_push.stderr);
    assert!(
        partial_multi_stderr.contains("409"),
        "mixed-result multi-ref push should fail at the atomic HTTP boundary: {partial_multi_stderr}"
    );
    assert_eq!(newgit.refs.read("refs/main").unwrap(), multi_ref_tip);
    assert!(newgit.refs.read_opt("refs/published").unwrap().is_none());
    assert_eq!(
        newgit.refs.read("refs/delete-rejected").unwrap(),
        multi_ref_tip
    );
    assert_eq!(newgit.objects.iter().unwrap(), objects_before_rejections);
    assert_eq!(newgit.refs.list(None).unwrap(), refs_before_rejections);

    // With atomic advertised, the same policy failure rejects the entire set
    // in Git's projection. Neither the branch deletion nor any object is
    // promoted into canonical NewGit storage.
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
    ]);
    let atomic_multi_stderr = String::from_utf8_lossy(&atomic_multi_push.stderr);
    assert!(
        !atomic_multi_stderr.is_empty(),
        "Git should report the rejected atomic multi-ref push"
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
