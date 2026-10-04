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
fn integrate_merge_tree_rollback_cli() {
    let (_d, dir) = tmp();
    let proj = dir.join("pi");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    write(&proj, "doc.md", b"# Title\n\nintro line\n");
    ok(&proj, &["snapshot", "-m", "base", "--time", "1000"]);

    // two agents branch from main
    ok(&proj, &["workspace", "create", "agent-x"]);
    ok(&proj, &["workspace", "create", "agent-y"]);
    let xd = proj.join(".newgit/workspaces/agent-x/files");
    let yd = proj.join(".newgit/workspaces/agent-y/files");
    write(&xd, "doc.md", b"# Title (X edit)\n\nintro line\n");
    write(&xd, "x-notes.md", b"X\n");
    ok(
        &proj,
        &[
            "snapshot", "-w", "agent-x", "-m", "X work", "--time", "2000",
        ],
    );
    write(&yd, "doc.md", b"# Title\n\nintro line\n\nappendix (Y)\n");
    write(&yd, "y-notes.md", b"Y\n");
    ok(
        &proj,
        &[
            "snapshot", "-w", "agent-y", "-m", "Y work", "--time", "2001",
        ],
    );

    // dry-run merge-tree (clean): exit 0
    let out = ok(&proj, &["merge-tree", "ws:agent-x", "ws:agent-y", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["clean"], true);
    assert_eq!(v["data"]["entries"], 3); // doc.md + x-notes.md + y-notes.md

    // real integrate into agent-x
    let out = ok(
        &proj,
        &["integrate", "ws:agent-y", "-w", "agent-x", "--json"],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["result"], "merged");
    // files materialized in agent-x
    let doc = std::fs::read(xd.join("doc.md")).unwrap();
    assert_eq!(doc, b"# Title (X edit)\n\nintro line\n\nappendix (Y)\n");
    assert!(xd.join("y-notes.md").exists());
    // second integrate is up to date
    let out = ok(
        &proj,
        &["integrate", "ws:agent-y", "-w", "agent-x", "--json"],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["result"], "up_to_date");

    // conflict path: make agent-y diverge on the same line
    write(&yd, "doc.md", b"CONFLICT Y\n");
    ok(
        &proj,
        &[
            "snapshot",
            "-w",
            "agent-y",
            "-m",
            "Y conflict",
            "--time",
            "3000",
        ],
    );
    write(&xd, "doc.md", b"CONFLICT X\n");
    ok(
        &proj,
        &[
            "snapshot",
            "-w",
            "agent-x",
            "-m",
            "X conflict",
            "--time",
            "3001",
        ],
    );
    let r = ng(&proj, &["integrate", "ws:agent-y", "-w", "agent-x"]);
    assert_eq!(r.code, 5, "conflict exit code; err={}", r.err);
    assert!(r.err.contains("doc.md"));
    // merge-tree dry run: exit 5 + conflict detail
    let r = ng(&proj, &["merge-tree", "ws:agent-x", "ws:agent-y", "--json"]);
    assert_eq!(r.code, 5);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["data"]["clean"], false);
    let c = &v["data"]["conflicts"][0];
    assert_eq!(c["kind"], "content");
    assert_eq!(c["path"], "doc.md");
    // the merged-with-markers blob is inspectable
    let merged_oid = c["merged_oid"].as_str().unwrap();
    let raw = Command::new(newgit_bin())
        .current_dir(&proj)
        .args(["cat", merged_oid, "--raw"])
        .output()
        .unwrap();
    let merged = String::from_utf8(raw.stdout).unwrap();
    assert!(merged.contains("<<<<<<< ours"));
    assert!(merged.contains(">>>>>>> theirs"));

    // rollback agent-x past its last snapshot
    let out = ok(&proj, &["rollback", "-w", "agent-x", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["data"]["oid"].is_string());
    let doc = std::fs::read(xd.join("doc.md")).unwrap();
    assert!(
        doc.starts_with(b"# Title (X edit)"),
        "rollback restored tree: {doc:?}"
    );

    // checkout resync
    let out = ok(&proj, &["checkout", "-w", "agent-x", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["data"]["files"].as_u64().unwrap() > 0);
    let out = ok(&proj, &["status", "-w", "agent-x"]);
    assert!(out.contains("clean"));
}

#[test]
fn two_agent_goal_workflow_e2e() {
    // The flagship scenario: two agents address the SAME goal with different
    // changes; each collects runner-recorded evidence; evaluations compare
    // them; the better one is proposed, approved, and atomically integrated.
    let (_d, dir) = tmp();
    let proj = dir.join("agents");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    write(&proj, "app.py", b"def solve(x):\n    return None\n");
    ok(&proj, &["snapshot", "-m", "skeleton", "--time", "1000"]);

    // goal
    let out = ok(
        &proj,
        &[
            "goal",
            "create",
            "Implement solve()",
            "--description",
            "return x*2",
            "--json",
        ],
    );
    let goal_id = serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["goal"]
        .as_str()
        .unwrap()
        .to_string();
    ok(&proj, &["goal", "set-status", &goal_id, "in_progress"]);

    // two agent workspaces
    ok(
        &proj,
        &["workspace", "create", "agent-1", "--author", "agent:one"],
    );
    ok(
        &proj,
        &["workspace", "create", "agent-2", "--author", "agent:two"],
    );
    let w1 = proj.join(".newgit/workspaces/agent-1/files");
    let w2 = proj.join(".newgit/workspaces/agent-2/files");
    write(&w1, "app.py", b"def solve(x):\n    return x * 2\n");
    write(&w2, "app.py", b"def solve(x):\n    return x + x\n");
    let out = ok(
        &proj,
        &[
            "snapshot",
            "-w",
            "agent-1",
            "-m",
            "agent-1 impl",
            "--time",
            "2000",
            "--author",
            "agent:one",
            "--goal",
            &goal_id,
            "--json",
        ],
    );
    let s_a1 = serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();
    let out = ok(
        &proj,
        &[
            "snapshot",
            "-w",
            "agent-2",
            "-m",
            "agent-2 impl",
            "--time",
            "2001",
            "--author",
            "agent:two",
            "--goal",
            &goal_id,
            "--json",
        ],
    );
    let s_a2 = serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["oid"]
        .as_str()
        .unwrap()
        .to_string();

    // changes referencing base + result snapshots
    let out = ok(&proj, &["history", "--json", "-n", "10"]);
    let hist = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    let base_snap = hist["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["snapshot"]["message"] == "skeleton")
        .unwrap()["oid"]
        .as_str()
        .unwrap()
        .to_string();
    let out = ok(
        &proj,
        &[
            "change",
            "create",
            "agent-1 solve",
            "--base",
            &base_snap,
            "--result",
            &s_a1,
            "--goal",
            &goal_id,
            "--json",
        ],
    );
    let ch1 = serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["change"]
        .as_str()
        .unwrap()
        .to_string();
    let out = ok(
        &proj,
        &[
            "change",
            "create",
            "agent-2 solve",
            "--base",
            &base_snap,
            "--result",
            &s_a2,
            "--goal",
            &goal_id,
            "--json",
        ],
    );
    let ch2 = serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["change"]
        .as_str()
        .unwrap()
        .to_string();

    // honesty gate: cannot mark tested without evidence
    let r = ng(&proj, &["change", "set-status", &ch1, "tested"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("no evidence"), "{}", r.err);

    // runner-recorded evidence (real commands, deterministic)
    let out = ok(
        &proj,
        &[
            "evidence",
            "record",
            "--kind",
            "unit_test",
            "--target",
            &ch1,
            "-w",
            "agent-1",
            "--",
            "sh",
            "-c",
            "grep -qF 'x * 2' app.py",
        ],
    );
    assert!(out.contains("verdict=pass"));
    let ev1 = out.split_whitespace().nth(1).unwrap().to_string();
    let out = ok(
        &proj,
        &[
            "evidence",
            "record",
            "--kind",
            "unit_test",
            "--target",
            &ch2,
            "-w",
            "agent-2",
            "--",
            "sh",
            "-c",
            "grep -qF 'x + x' app.py",
        ],
    );
    let ev2 = out.split_whitespace().nth(1).unwrap().to_string();
    // evidence show: deterministic flag visible
    let out = ok(&proj, &["evidence", "show", &ev1, "--json"]);
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert_eq!(v["data"]["evidence"]["deterministic"], true);
    assert_eq!(v["data"]["evidence"]["verdict"], "pass");

    ok(&proj, &["change", "attach-evidence", &ch1, &ev1]);
    ok(&proj, &["change", "attach-evidence", &ch2, &ev2]);
    ok(&proj, &["change", "set-status", &ch1, "tested"]);
    ok(&proj, &["change", "set-status", &ch2, "tested"]);

    // deterministic evaluations aggregated from evidence
    let out = ok(&proj, &["evaluation", "from-evidence", &ch1, "--json"]);
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert_eq!(v["data"]["data"]["verdict"], "pass");
    assert_eq!(v["data"]["data"]["ai_generated"], false);
    // AI-style opinion evaluation is flagged, never passes as deterministic
    let out = ok(
        &proj,
        &[
            "evaluation",
            "create",
            "--target",
            &ch2,
            "--verdict",
            "pass",
            "--ai",
            "--dimension",
            "elegance=pass:adds are neat",
            "--json",
        ],
    );
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    let ai_eval = v["data"]["evaluation"].as_str().unwrap().to_string();
    let out = ok(&proj, &["evaluation", "show", &ai_eval, "--json"]);
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert_eq!(v["data"]["evaluation"]["ai_generated"], true);

    // proposal for change 1 (agent-1's implementation wins the comparison)
    let out = ok(
        &proj,
        &[
            "proposal",
            "create",
            "Integrate agent-1 solve",
            "--change",
            &ch1,
            "--rationale",
            "passes deterministic test; simpler",
            "--evidence",
            &ev1,
            "--json",
        ],
    );
    let prop = serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["proposal"]
        .as_str()
        .unwrap()
        .to_string();
    // cannot integrate before approval
    let r = ng(&proj, &["proposal", "integrate", &prop]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("approved"), "{}", r.err);
    ok(
        &proj,
        &[
            "proposal",
            "approve",
            &prop,
            "--author",
            "human:reviewer",
            "--author-name",
            "Rev",
        ],
    );
    let out = ok(&proj, &["proposal", "integrate", &prop, "--json"]);
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert!(v["data"]["integration"]["result"].is_string());
    // main now contains agent-1's implementation
    let app = std::fs::read(proj.join("app.py")).unwrap();
    assert!(
        app.windows(5).any(|w| w == b"x * 2"),
        "{:?}",
        String::from_utf8_lossy(&app)
    );
    // entity states after integration
    let out = ok(&proj, &["proposal", "show", &prop, "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["proposal"]["state"],
        "integrated"
    );
    let out = ok(&proj, &["change", "show", &ch1, "--json"]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["data"]["change"]["status"],
        "integrated"
    );
    // history filtered by goal: the integration was a FAST-FORWARD (main had
    // not diverged), so main's history contains exactly one goal-tagged
    // snapshot — agent-1's. agent-2's snapshot lives on its own workspace
    // ref (visible via `history -w agent-2 --goal ...`), not in main's.
    let out = ok(&proj, &["history", "--goal", &goal_id, "--json"]);
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    let out = ok(
        &proj,
        &["history", "-w", "agent-2", "--goal", &goal_id, "--json"],
    );
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 1);
    // goal can be achieved now
    ok(&proj, &["goal", "set-status", &goal_id, "achieved"]);
    let out = ok(&proj, &["goal", "list"]);
    assert!(out.contains("achieved"));
    // agent-2's change remains tested (alternative implementation preserved)
    let out = ok(&proj, &["change", "list", "--goal", &goal_id, "--json"]);
    let v = serde_json::from_str::<serde_json::Value>(&out).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
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

#[test]
fn verify_gc_recover_cli_contract() {
    let (_d, dir) = tmp();
    let proj = dir.join("p7");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    ok(
        &proj,
        &["actor", "set-default", "--id", "a1", "--name", "A"],
    );
    std::fs::write(proj.join("x.txt"), b"content").unwrap();
    ok(&proj, &["snapshot", "-m", "one"]);

    // clean repo: exit 0; JSON envelope carries the full report
    let r = ng(&proj, &["verify", "--deep", "--json"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(v["data"]["objects_checked"].as_u64().unwrap() > 0);
    assert_eq!(v["data"]["issues"], serde_json::json!([]));

    // orphan object: dry-run reports, real gc deletes, cat then fails 3
    std::fs::write(proj.join("y.txt"), b"orphan payload").unwrap();
    let orphan = ok(&proj, &["hash-object", "y.txt", "--write"]);
    let orphan = orphan.trim();
    assert_eq!(orphan.len(), 64);
    let r = ng(&proj, &["gc", "--dry-run", "--force-now", "--json"]);
    assert_eq!(r.code, 0);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["data"]["deleted_objects"], serde_json::json!(1));
    assert_eq!(v["data"]["dry_run"], serde_json::json!(true));
    ok(&proj, &["gc", "--force-now"]);
    let r = ng(&proj, &["cat", orphan]);
    assert_eq!(r.code, 3, "collected blob must be gone");

    // corrupt an object: verify exits 3 in BOTH formats, JSON still ok=true
    // (the report is the payload; the exit code carries pass/fail)
    let mut victim = PathBuf::new();
    for shard in std::fs::read_dir(proj.join(".newgit").join("objects")).unwrap() {
        for f in std::fs::read_dir(shard.unwrap().path()).unwrap() {
            let f = f.unwrap().path();
            if f.file_name().unwrap().to_string_lossy().len() == 62 {
                victim = f;
                break;
            }
        }
        if !victim.as_os_str().is_empty() {
            break;
        }
    }
    let mut bytes = std::fs::read(&victim).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&victim, &bytes).unwrap();

    let r = ng(&proj, &["verify", "--json"]);
    assert_eq!(r.code, 3);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert!(!v["data"]["issues"].as_array().unwrap().is_empty());
    let r = ng(&proj, &["verify"]);
    assert_eq!(r.code, 3);
    assert!(r.out.contains("object.corrupt"));

    // gc on a damaged repo: keeps the unreadable file (forensics), exits 0
    let r = ng(&proj, &["gc", "--force-now", "--json"]);
    assert_eq!(r.code, 0, "{}{}", r.out, r.err);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["data"]["kept_corrupt"], serde_json::json!(1));
    assert!(victim.exists());

    // recover is clean-running and JSON-shaped
    let r = ng(&proj, &["recover", "--json"]);
    assert_eq!(r.code, 0);
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["data"]["redone"], serde_json::json!([]));
    let r = ng(&proj, &["recover"]);
    assert_eq!(r.code, 0);
    assert!(r.out.contains("recover:"));
}

#[test]
fn import_export_git_cli() {
    use std::process::Command;
    let (_d, dir) = tmp();
    // a small real git repo
    let gdir = dir.join("gsrc");
    std::fs::create_dir_all(&gdir).unwrap();
    let git = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&gdir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    };
    git(&["init", "--quiet", "-b", "master"]);
    git(&["config", "user.name", "CLI Tester"]);
    git(&["config", "user.email", "cli@test"]);
    std::fs::write(gdir.join("hello.txt"), b"hello git\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "git commit one"]);

    // newgit side
    let proj = dir.join("p8");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    let out = ok(&proj, &["import-git", gdir.to_str().unwrap(), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], serde_json::json!(true));
    assert_eq!(v["data"]["commits"], serde_json::json!(1));
    assert_eq!(
        v["data"]["head"],
        serde_json::json!("ref: refs/heads/master")
    );
    ok(&proj, &["verify", "--deep"]);
    let out = ok(&proj, &["history", "--from", "refs/heads/master"]);
    assert!(out.contains("git commit one"), "{out}");
    // cat resolves oids (not ref names): take the tip oid from history
    let out = ok(
        &proj,
        &[
            "history",
            "--from",
            "refs/heads/master",
            "-n",
            "1",
            "--json",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let tip = v["data"][0]["oid"].as_str().unwrap().to_string();
    let out = ok(&proj, &["cat", &tip, "--json"]);
    assert!(
        out.contains("git_sha1"),
        "snapshot must carry the original git sha"
    );

    // export back out
    let outdir = dir.join("gout");
    let out = ok(&proj, &["export-git", outdir.to_str().unwrap(), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["commits"], serde_json::json!(1));
    let o = Command::new("git")
        .arg("-C")
        .arg(&outdir)
        .args(["log", "--format=%s", "refs/heads/master"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "git commit one");
    assert_eq!(
        std::fs::read(outdir.join("hello.txt")).unwrap(),
        b"hello git\n"
    );

    // error contracts: not a git repo / occupied target — both usage errors
    let r = ng(&proj, &["import-git", proj.to_str().unwrap()]);
    assert_eq!(r.code, 2, "{}{}", r.out, r.err);
    assert!(r.err.contains("not a git repository") || r.out.contains("not a git repository"));
    let r = ng(&proj, &["export-git", gdir.to_str().unwrap()]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("not empty") || r.out.contains("not empty"));
    // missing positional
    let r = ng(&proj, &["import-git"]);
    assert_eq!(r.code, 2);
}

// ---------------------------------------------------------------------------
// Remote protocol v1 over the real binary (TEST_MATRIX "E2E remote")
// ---------------------------------------------------------------------------

use std::process::Stdio;

struct Serve {
    child: std::process::Child,
    url: String,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `newgit serve` on an ephemeral port and wait for its listening
/// line — this asserts the port-0 announcement contract at the same time.
fn spawn_serve(repo: &Path) -> Serve {
    use std::io::{BufRead, BufReader};
    let mut child = Command::new(newgit_bin())
        .current_dir(repo)
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn newgit serve");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    let line = lines
        .next()
        .expect("serve produced no output")
        .expect("serve stdout");
    let addr = line
        .split("http://")
        .nth(1)
        .unwrap_or_else(|| panic!("unexpected listening line: {line}"))
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    assert!(line.contains("protocol v1"), "{line}");
    child.stdout = None;
    Serve {
        child,
        url: format!("http://{addr}"),
    }
}

#[test]
fn remote_cli_push_pull_serve_token_audit() {
    let (_d, dir) = tmp();
    // Server repo with one commit.
    let srv = dir.join("srv");
    std::fs::create_dir_all(&srv).unwrap();
    ok(&srv, &["init"]);
    ok(
        &srv,
        &["actor", "set-default", "--id", "agent:srv", "--name", "Srv"],
    );
    write(&srv, "hello.txt", b"server side\n");
    ok(&srv, &["snapshot", "-m", "srv s1"]);

    // Token management: raw token printed once, list never leaks it.
    let out = ok(&srv, &["token", "add", "ci", "--role", "write", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let raw = v["data"]["token"].as_str().unwrap().to_string();
    assert!(raw.len() >= 32, "token must be high-entropy: {raw}");
    let out = ok(
        &srv,
        &[
            "token", "add", "boss", "--role", "admin", "--token", "tok-boss",
        ],
    );
    assert!(out.contains("tok-boss"), "explicit token shown once");
    let out = ok(&srv, &["token", "list", "--json"]);
    assert!(out.contains("ci") && out.contains("admin"));
    assert!(
        !out.contains(&raw) && !out.contains("tok-boss"),
        "list must never leak tokens"
    );

    let serve = spawn_serve(&srv);

    // Client repo: remote add (token masked in list), pull the server state.
    let cli = dir.join("cli");
    std::fs::create_dir_all(&cli).unwrap();
    ok(&cli, &["init"]);
    ok(
        &cli,
        &["actor", "set-default", "--id", "agent:cli", "--name", "Cli"],
    );
    ok(
        &cli,
        &["remote", "add", "origin", &serve.url, "--token", &raw],
    );
    let out = ok(&cli, &["remote", "list"]);
    assert!(out.contains("origin") && out.contains("token=set") && !out.contains(&raw));
    let out = ok(&cli, &["pull", "origin", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["refs_updated"].as_array().unwrap().len(), 1);
    assert!(v["data"]["objects_received"].as_u64().unwrap() > 0);
    let out = ok(&cli, &["history"]);
    assert!(out.contains("srv s1"), "pulled history: {out}");
    assert!(
        !cli.join("hello.txt").exists(),
        "pull moves refs, not worktrees (documented)"
    );

    // Push a new commit back.
    write(&cli, "from-cli.txt", b"client side\n");
    ok(&cli, &["snapshot", "-m", "cli s2"]);
    let out = ok(&cli, &["push", "origin", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["data"]["refs_pushed"], serde_json::json!(["refs/main"]));
    let out = ok(&srv, &["history"]);
    assert!(out.contains("cli s2"), "server received the push: {out}");

    // Auth contracts: no token ⇒ exit 7 (AUTH), read token cannot write.
    ok(&cli, &["remote", "add", "notok", &serve.url]);
    let r = ng(&cli, &["push", "notok", "--json"]);
    assert_eq!(
        r.code, 7,
        "anonymous push must exit AUTH: {}{}",
        r.out, r.err
    );
    let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["category"], "auth");
    ok(
        &srv,
        &["token", "add", "ro", "--role", "read", "--token", "tok-ro"],
    );
    ok(
        &cli,
        &["remote", "add", "ronly", &serve.url, "--token", "tok-ro"],
    );
    let r = ng(&cli, &["push", "ronly", "--json"]);
    assert_eq!(r.code, 7, "read-role push must exit AUTH: {}", r.out);
    // ...but the read token CAN pull
    ok(&cli, &["pull", "ronly"]);

    // Audit: the server recorded who did what — including the failures.
    let out = ok(&srv, &["audit", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let entries = v["data"]["entries"].as_array().unwrap();
    let has = |p: &str, path: &str, status: u64| {
        entries
            .iter()
            .any(|e| e["principal"] == p && e["path"] == path && e["status"] == status)
    };
    assert!(
        has("ci", "/v1/objects/put", 200),
        "push objects audited: {entries:?}"
    );
    assert!(has("ci", "/v1/refs/update", 200), "ref update audited");
    assert!(
        entries.iter().any(|e| e["status"] == 401),
        "auth failures audited: {entries:?}"
    );
    let out = ok(&srv, &["audit"]);
    assert!(
        out.contains("ci") && out.contains("/v1/refs/update"),
        "text audit: {out}"
    );
    let out = ok(&srv, &["--json", "config", "show"]);
    assert!(out.contains("ok"), "config show still works while serving");

    // URL validation errors are usage errors.
    let r = ng(&cli, &["remote", "add", "bad", "https://example/x"]);
    assert_eq!(r.code, 2, "https/path urls must be USAGE: {}", r.err);
    assert!(r.err.contains("plain HTTP") || r.out.contains("plain HTTP"));

    // Unknown remote ⇒ config error, non-zero.
    let r = ng(&cli, &["push", "nosuch"]);
    assert_ne!(r.code, 0);
    assert!(r.err.contains("no remote named") || r.out.contains("no remote named"));

    // remote remove
    ok(&cli, &["remote", "remove", "notok"]);
    let out = ok(&cli, &["remote", "list", "--json"]);
    assert!(!out.contains("notok"));

    // Server repo still verifies deep after all of this (ok() enforces the
    // exit-3-on-errors contract; double-check no error-severity issues).
    let out = ok(&srv, &["verify", "--deep", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["data"]["objects_checked"].as_u64().unwrap() > 0);
    let issues = v["data"]["issues"].as_array().unwrap();
    assert!(issues.iter().all(|i| i["severity"] != "error"), "{out}");
}

#[test]
fn remote_cli_refuses_to_serve_outside_a_repo_and_reports_bind_errors() {
    let (_d, dir) = tmp();
    // not a repo ⇒ exit 3 (REPO)
    let r = ng(&dir, &["serve", "--bind", "127.0.0.1:0"]);
    assert_ne!(r.code, 0);
    // an occupied port is a config error, not a panic
    let srv = dir.join("srv2");
    std::fs::create_dir_all(&srv).unwrap();
    ok(&srv, &["init"]);
    let s1 = spawn_serve(&srv);
    let addr = s1.url.trim_start_matches("http://");
    let r = ng(&srv, &["serve", "--bind", addr]);
    assert_ne!(r.code, 0, "double bind must fail cleanly");
    assert!(
        r.err.contains("cannot bind") || r.err.contains("address"),
        "{}",
        r.err
    );
    // bad --max-body value ⇒ usage error
    let r = ng(&srv, &["serve", "--max-body", "lots"]);
    assert_eq!(r.code, 2, "{}", r.err);
}

// ---------------------------------------------------------------------------
// Iteration 10: `newgit ui` + `newgit mcp` as real processes
// ---------------------------------------------------------------------------

#[test]
fn ui_command_serves_browser_shell() {
    let (_d, dir) = tmp();
    let proj = dir.join("proj");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    write(&proj, "x.txt", b"hello\n");
    ok(&proj, &["snapshot", "-m", "s1"]);

    let mut child = Command::new(newgit_bin())
        .args([
            "--repo",
            proj.to_str().unwrap(),
            "ui",
            "--bind",
            "127.0.0.1:0",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn ui");
    use std::io::BufRead;
    let stdout = child.stdout.take().unwrap();
    let mut lines = std::io::BufReader::new(stdout).lines();
    let line = lines.next().expect("ui must announce its address").unwrap();
    let hi = line.find("http://").expect("announcement contains a URL");
    let addr = line[hi + "http://".len()..]
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let a: std::net::SocketAddr = addr
        .parse()
        .unwrap_or_else(|e| panic!("listening addr {addr:?}: {e}"));
    // second line: the web UI hint
    let hint = lines.next().expect("ui hint line").unwrap();
    assert!(hint.contains("web UI") && hint.contains('/'), "{hint}");
    drop(lines);

    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(a).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    write!(s, "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
    let first = buf.lines().next().unwrap_or("").to_string();
    assert!(buf.starts_with("HTTP/1.1 200"), "status line: {first}");
    assert!(buf.contains("<!DOCTYPE html>"), "html body");
    assert!(buf.contains("Content-Type: text/html"), "content type");
    // repository data must NOT be embedded in the shell
    assert!(!buf.contains("refs/main"), "static shell only");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn mcp_speaks_jsonrpc_over_stdio() {
    let (_d, dir) = tmp();
    let proj = dir.join("proj");
    std::fs::create_dir(&proj).unwrap();
    ok(&proj, &["init"]);
    write(&proj, "f.txt", b"mcp\n");
    ok(&proj, &["snapshot", "-m", "s0"]);

    let mut child = Command::new(newgit_bin())
        .args(["--repo", proj.to_str().unwrap(), "mcp"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mcp");
    use std::io::{BufRead, Write};
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = std::io::BufReader::new(stdout).lines();

    const NOTIF: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

    fn rpc(
        stdin: &mut std::process::ChildStdin,
        reader: &mut std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
        msg: &str,
    ) -> serde_json::Value {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let line = reader.next().expect("mcp response").unwrap();
        serde_json::from_str(&line).expect("json response")
    }

    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
    );
    assert_eq!(r["result"]["serverInfo"]["name"], "newgit", "{r}");
    writeln!(stdin, "{NOTIF}").unwrap();
    stdin.flush().unwrap();

    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13, "tool catalog");

    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"newgit_history","arguments":{"limit":5}}}"#,
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("s0"));

    // snapshot through MCP, then see it in history
    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"newgit_snapshot","arguments":{"message":"mcp-snap"}}}"#,
    );
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"newgit_history","arguments":{}}}"#,
    );
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("mcp-snap"));

    // error path: tool failure is isError content with a category, not a crash
    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"newgit_cat","arguments":{"oid":"zz"}}}"#,
    );
    assert_eq!(r["result"]["isError"], true);
    let text = r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.contains("category"), "{text}");

    // server survives malformed input, then answers ping
    let r = rpc(&mut stdin, &mut reader, "{oops");
    assert_eq!(r["error"]["code"], -32700);
    let r = rpc(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#,
    );
    assert_eq!(r["result"], serde_json::json!({}));

    drop(stdin); // EOF ⇒ clean exit
    let status = child.wait().unwrap();
    assert!(status.success(), "mcp exits 0 on EOF: {status}");
}
