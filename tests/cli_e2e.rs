//! End-to-end CLI tests: drive the real binary in a subprocess, assert on
//! stdout/stderr/exit codes and on-disk state (TEST_MATRIX "E2E").

use std::path::{Path, PathBuf};
use std::process::Command;

fn newgit_bin() -> &'static str {
    env!("CARGO_BIN_EXE_newgit")
}

struct Run {
    out: String,
    err: String,
    code: i32,
}

fn ng(cwd: &Path, args: &[&str]) -> Run {
    let o = Command::new(newgit_bin())
        .current_dir(cwd)
        .args(args)
        .env_remove("NEWGIT_FAULTS")
        .output()
        .expect("spawn newgit");
    Run {
        out: String::from_utf8_lossy(&o.stdout).to_string(),
        err: String::from_utf8_lossy(&o.stderr).to_string(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn ok(cwd: &Path, args: &[&str]) -> String {
    let r = ng(cwd, args);
    assert_eq!(r.code, 0, "newgit {args:?} failed: {}{}", r.out, r.err);
    r.out
}

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().to_path_buf();
    (d, p)
}

fn write(dir: &Path, rel: &str, content: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, content).unwrap();
}

#[test]
fn full_workflow_e2e() {
    let (_d, dir) = tmp();
    let proj = dir.join("proj");
    std::fs::create_dir(&proj).unwrap();

    // init
    let out = ok(&proj, &["init"]);
    assert!(out.contains("initialized newgit repository"));
    assert!(proj.join(".newgit/config").exists());

    // unborn status is clean
    let out = ok(&proj, &["status"]);
    assert!(out.contains("unborn"));
    assert!(out.contains("clean"));

    // create files → status shows added
    write(&proj, "src/main.rs", b"fn main() {}\n");
    write(&proj, "README.md", b"# hi\n");
    let out = ok(&proj, &["status"]);
    assert!(out.contains("README.md"));
    assert!(out.contains("src/main.rs"));
    assert!(out.contains("added"));

    // snapshot
    let out = ok(&proj, &["snapshot", "-m", "initial import"]);
    assert!(out.contains("snapshot"));
    assert!(out.contains("files: 2"));

    // status clean again
    let out = ok(&proj, &["status"]);
    assert!(out.contains("clean"));

    // modify + snapshot again
    write(&proj, "src/main.rs", b"fn main() { println!(\"hi\"); }\n");
    let out = ok(&proj, &["status"]);
    assert!(out.contains("modified"));
    ok(&proj, &["snapshot", "-m", "add greeting"]);

    // history shows both, newest first
    let out = ok(&proj, &["history"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("add greeting"));
    assert!(lines[1].contains("initial import"));

    // hash-object + cat round trip
    let oid = ok(&proj, &["hash-object", "README.md", "--write"])
        .trim()
        .to_string();
    assert_eq!(oid.len(), 64);
    let raw = Command::new(newgit_bin())
        .current_dir(&proj)
        .args(["cat", &oid, "--raw"])
        .output()
        .unwrap();
    assert_eq!(raw.stdout, b"# hi\n");

    // cat structured
    let out = ok(&proj, &["cat", &oid[..12], "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["type"], "blob");
    assert_eq!(v["data"]["size"], 5);

    // workspace flow via CLI
    ok(&proj, &["workspace", "create", "agent-a"]);
    let wsdir = proj.join(".newgit/workspaces/agent-a/files");
    assert!(wsdir.join("README.md").exists());
    write(&wsdir, "feature.txt", b"agent work");
    let out = ok(&proj, &["status", "-w", "agent-a"]);
    assert!(out.contains("feature.txt"));
    ok(
        &proj,
        &[
            "snapshot",
            "-w",
            "agent-a",
            "-m",
            "agent change",
            "--author",
            "agent:test-1",
        ],
    );
    let out = ok(&proj, &["workspace", "list"]);
    assert!(out.contains("agent-a"));
    assert!(out.contains("*main"));
    // main is untouched
    let out = ok(&proj, &["status"]);
    assert!(out.contains("clean"));
    // history of the workspace: agent snapshot + the two main snapshots it
    // was based on
    let out = ok(&proj, &["history", "-w", "agent-a", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let entries = v["data"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["snapshot"]["message"], "agent change");
    assert_eq!(entries[1]["snapshot"]["message"], "add greeting");
    assert_eq!(entries[2]["snapshot"]["message"], "initial import");
    // discard dirty workspace refused, forced ok
    write(&wsdir, "dirty.txt", b"x");
    let r = ng(&proj, &["workspace", "discard", "agent-a"]);
    assert_eq!(r.code, 5, "conflict exit code; stderr={}", r.err);
    let r = ng(&proj, &["workspace", "discard", "agent-a", "--force"]);
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(!wsdir.exists());
}

#[test]
fn json_envelope_and_errors() {
    let (_d, dir) = tmp();
    std::fs::create_dir(dir.join("p")).unwrap();
    let proj = dir.join("p");
    ok(&proj, &["init"]);

    // success envelope
    let out = ok(&proj, &["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["clean"], true);

    // error envelope: unknown command
    let r = ng(&proj, &["frobnicate"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("unknown command"));

    // error envelope json
    let r = ng(&proj, &["--json", "frobnicate"]);
    assert_eq!(r.code, 2);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["category"], "invalid");

    // missing required flag
    let r = ng(&proj, &["snapshot"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("--message"));

    // not a repository
    let other = dir.join("notrepo");
    std::fs::create_dir(&other).unwrap();
    let r = ng(&other, &["status"]);
    assert_eq!(r.code, 3);
    assert!(r.err.contains("not a newgit repository"));

    // missing object
    let r = ng(&proj, &["cat", &"ab".repeat(32)]);
    assert_eq!(r.code, 3);

    // ambiguous/short prefix
    let r = ng(&proj, &["cat", "ab"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("at least 4"));
}

#[test]
fn version_and_help() {
    let (_d, dir) = tmp();
    let out = ok(&dir, &["version"]);
    assert!(out.starts_with("newgit "));
    let out = ok(&dir, &["version", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["data"]["version"].is_string());
    let out = ok(&dir, &["help"]);
    assert!(out.contains("snapshot"));
    assert!(out.contains("workspace"));
    let out = ok(&dir, &["help", "workspace"]);
    assert!(out.contains("discard"));
    // no args → usage error
    let r = ng(&dir, &[]);
    assert_eq!(r.code, 2);
}

#[test]
fn actor_config_flow() {
    let (_d, dir) = tmp();
    let proj = dir.join("p2");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    ok(
        &proj,
        &[
            "actor",
            "set-default",
            "--id",
            "human:alice",
            "--name",
            "Alice L",
        ],
    );
    let out = ok(&proj, &["actor", "show", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["actor"]["id"], "human:alice");
    assert_eq!(v["data"]["actor"]["kind"], "human");
    write(&proj, "x.txt", b"x");
    ok(&proj, &["snapshot", "-m", "by alice"]);
    let out = ok(&proj, &["history", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let snap = &v["data"][0]["snapshot"];
    assert_eq!(snap["message"], "by alice");
    // author resolves to the configured actor
    let author_oid = snap["author"].as_str().unwrap();
    let out = ok(&proj, &["cat", author_oid, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["data"]["id"], "human:alice");
}

#[test]
fn repo_flag_and_discovery() {
    let (_d, dir) = tmp();
    let proj = dir.join("p3");
    std::fs::create_dir_all(proj.join("deep/deeper")).unwrap();
    ok(&proj, &["init"]);
    // -C from anywhere
    let out = ok(&dir, &["-C", proj.to_str().unwrap(), "status"]);
    assert!(out.contains("workspace: main"));
    // discovery from a deep subdirectory
    let out = ok(&proj.join("deep/deeper"), &["status"]);
    assert!(out.contains("workspace: main"));
    // --repo pointing at a non-repo
    let r = ng(
        &dir,
        &["--repo", dir.join("nope").to_str().unwrap(), "status"],
    );
    assert_eq!(r.code, 3);
}

#[test]
fn deterministic_snapshot_via_cli() {
    let (_d, dir) = tmp();
    let proj = dir.join("p4");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    write(&proj, "f.txt", b"stable");
    ok(&proj, &["actor", "set-default", "--id", "process:ci"]);
    let out = ok(
        &proj,
        &[
            "snapshot",
            "-m",
            "det",
            "--time",
            "1700000000000",
            "--tz",
            "330",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let oid1 = v["data"]["oid"].as_str().unwrap().to_string();
    // reset the ref and redo identically → same object id
    // (uses the API-level reset through a second init in a fresh repo with
    // identical inputs)
    let proj2 = dir.join("p5");
    std::fs::create_dir(&proj2).unwrap();
    ok(&proj2, &["init"]);
    write(&proj2, "f.txt", b"stable");
    ok(&proj2, &["actor", "set-default", "--id", "process:ci"]);
    let out = ok(
        &proj2,
        &[
            "snapshot",
            "-m",
            "det",
            "--time",
            "1700000000000",
            "--tz",
            "330",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let oid2 = v["data"]["oid"].as_str().unwrap().to_string();
    assert_eq!(
        oid1, oid2,
        "identical inputs must produce identical snapshots"
    );
}

#[test]
fn diff_cli_flows() {
    let (_d, dir) = tmp();
    let proj = dir.join("pd");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    write(&proj, "a.txt", b"one\ntwo\nthree\n");
    write(&proj, "keep.txt", b"k\n");
    let snap1 = {
        let out = ok(&proj, &["snapshot", "-m", "s1", "--time", "1000", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["data"]["oid"].as_str().unwrap().to_string()
    };
    // live changes vs position
    write(&proj, "a.txt", b"one\nTWO\nthree\nfour\n");
    write(&proj, "b.txt", b"new file\n");
    let out = ok(&proj, &["diff"]);
    assert!(out.contains("diff --newgit a/a.txt b/a.txt"), "{out}");
    assert!(out.contains("-two"));
    assert!(out.contains("+TWO"));
    assert!(out.contains("new file"), "added file shown: {out}");
    // name-only
    let out = ok(&proj, &["diff", "--name-only"]);
    assert!(out.contains("a.txt"));
    assert!(out.contains("b.txt"));
    assert!(!out.contains("keep.txt"));
    // json with hunks
    let out = ok(&proj, &["diff", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    let files = v["data"]["files"].as_array().unwrap();
    let fa = files.iter().find(|f| f["path"] == "a.txt").unwrap();
    assert_eq!(fa["kind"], "modified");
    let hunks = fa["hunks"].as_array().unwrap();
    assert!(!hunks.is_empty());
    assert!(hunks[0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().starts_with("+TWO")));
    // snapshot after changes; diff snapshot..snapshot
    let snap2 = {
        let out = ok(&proj, &["snapshot", "-m", "s2", "--time", "2000", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        v["data"]["oid"].as_str().unwrap().to_string()
    };
    let out = ok(&proj, &["diff", &snap1, &snap2, "--name-only"]);
    assert_eq!(out.lines().collect::<Vec<_>>(), vec!["a.txt", "b.txt"]);
    // ref spec works too
    let out = ok(&proj, &["diff", "refs/main", "--name-only"])
        .lines()
        .count();
    assert_eq!(out, 0, "position vs worktree: clean after snapshot");
    // clean diff prints nothing, exit 0
    let r = ng(&proj, &["diff"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.out.trim(), "");
    // --exit-code with differences
    write(&proj, "a.txt", b"one\n");
    let r = ng(&proj, &["diff", "--exit-code"]);
    assert_eq!(r.code, 1);
    // rename detection through CLI
    ok(&proj, &["snapshot", "-m", "s3", "--time", "3000"]);
    std::fs::rename(proj.join("keep.txt"), proj.join("moved.txt")).unwrap();
    let out = ok(&proj, &["diff", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let files = v["data"]["files"].as_array().unwrap();
    let mv = files.iter().find(|f| f["path"] == "moved.txt").unwrap();
    assert_eq!(mv["kind"], "renamed");
    assert_eq!(mv["old_path"], "keep.txt");
    assert_eq!(mv["similarity"], 100);
    let _ = snap2;
}

#[test]
fn debug_logging_goes_to_stderr_jsonl() {
    let (_d, dir) = tmp();
    let proj = dir.join("p6");
    std::fs::create_dir(&proj).unwrap();
    let r = ng(&proj, &["--debug", "init"]);
    assert_eq!(r.code, 0);
    assert!(r.err.contains("\"event\":\"cli_start\"") || r.err.contains("cli_start"));
    assert!(r.err.contains("op_id"));
    // stdout stays clean for scripts
    assert!(r.out.contains("initialized"));
}
