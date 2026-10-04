//! Black-box smart-HTTP interoperability with the installed Git client.
//!
//! The HTTP peer is the real NewGit server. Git clone/fetch/pull/ls-remote
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
fn real_git_clone_fetch_pull_and_ls_remote_over_smart_http() {
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

    // The adapter does not pretend to implement receive-pack/push. It rejects
    // service discovery before any NewGit refs or objects can be mutated.
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
    let push = git_fails(&[
        "-c",
        &auth_config,
        "-C",
        clone_path_str,
        "push",
        "origin",
        "main",
    ]);
    assert_eq!(newgit.refs.read("refs/main").unwrap(), s2);
    assert_eq!(newgit.refs.read("refs/feature").unwrap(), s1);
    assert_eq!(newgit.refs.read("refs/tags/v1.0").unwrap(), s1);
    assert!(
        !String::from_utf8_lossy(&push.stderr).is_empty(),
        "push refusal should be reported by Git"
    );

    server.shutdown();
}
