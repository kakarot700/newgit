//! Shared helpers for integration tests.

#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Output};

use newgit::object::ObjectId;
use newgit::repo::Repo;

pub fn temp_repo() -> (tempfile::TempDir, Repo) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let repo = Repo::init(&root).unwrap();
    (dir, repo)
}

pub fn faultlab_bin() -> String {
    std::env::var("CARGO_BIN_EXE_newgit-faultlab")
        .expect("Cargo must provide CARGO_BIN_EXE_newgit-faultlab for integration tests")
}

/// Run faultlab in a child process; `faults` sets NEWGIT_FAULTS.
pub fn run_faultlab(repo_root: &Path, args: &[&str], faults: Option<&str>) -> Output {
    let mut cmd = Command::new(faultlab_bin());
    cmd.arg(args[0]).arg(repo_root);
    for a in &args[1..] {
        cmd.arg(a);
    }
    if let Some(f) = faults {
        cmd.env("NEWGIT_FAULTS", f);
        cmd.env("NEWGIT_FAULT_MODE", "abort");
    }
    cmd.output().expect("failed to spawn faultlab")
}

pub fn oid(b: u8) -> ObjectId {
    ObjectId::from_bytes([b; 32])
}

pub fn read_ref_str(repo: &Repo, name: &str) -> Option<String> {
    repo.refs.read_opt(name).ok().flatten().map(|o| o.to_hex())
}

pub fn journal_states(repo: &Repo) -> Vec<(String, String)> {
    let dir = repo.ng().join("txn");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().to_string();
        if !name.ends_with(".journal") {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).unwrap();
        let state = text
            .lines()
            .find(|l| l.starts_with("state="))
            .map(|l| l["state=".len()..].to_string())
            .unwrap_or_default();
        out.push((name, state));
    }
    out.sort();
    out
}
