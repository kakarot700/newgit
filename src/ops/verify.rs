//! `newgit verify` — filesystem-level integrity checker (fsck).
//!
//! Checks (each produces a coded Issue; errors ⇒ exit 3):
//! * every stored object: name/layout, envelope digest, parse, struct
//!   validate; `--deep` additionally re-encodes and compares bytes and
//!   walks every link (existence + type),
//! * refs: values parse, targets exist; HEAD grammar; reflog line format,
//! * chains: head resolvable, prev-links walk to the root without cycles,
//! * workspaces: meta parse, files dir present, orphan debris (warning),
//!   corrupt index cache (warning — it is only a cache),
//! * txn dir: leftover journals (warning → auto-recovery hint), locks
//!   (warning), quarantine inventory (warning),
//! * config parses (error).
//!
//! Verify NEVER modifies the repository (no auto-repair); messages say what
//! to run next.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::object::envelope;
use crate::object::types::{Object, ObjectType};
use crate::object::ObjectId;
use crate::repo::config::RepoConfig;
use crate::repo::index::Index;
use crate::repo::txn;
use crate::repo::Repo;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Issue {
    /// stable machine code, e.g. "object.digest", "ref.target_missing"
    pub code: String,
    pub severity: Severity,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct VerifyReport {
    pub objects_checked: usize,
    pub refs_checked: usize,
    pub chains_checked: usize,
    pub workspaces_checked: usize,
    pub quarantined: usize,
    pub issues: Vec<Issue>,
}

impl VerifyReport {
    pub fn errors(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count()
    }
    pub fn warnings(&self) -> usize {
        self.issues.len() - self.errors()
    }
    pub fn ok(&self) -> bool {
        self.errors() == 0
    }
}

fn issue(code: &str, sev: Severity, detail: String) -> Issue {
    Issue {
        code: code.into(),
        severity: sev,
        detail,
        oid: None,
        path: None,
    }
}
fn err(code: &str, detail: String) -> Issue {
    issue(code, Severity::Error, detail)
}
fn warn(code: &str, detail: String) -> Issue {
    issue(code, Severity::Warning, detail)
}

#[derive(Clone, Copy, Debug)]
pub struct VerifyOpts {
    /// Re-encode parsed objects and compare bytes; walk all object links.
    pub deep: bool,
}

pub fn verify(repo: &Repo, opts: &VerifyOpts) -> VerifyReport {
    let mut rep = VerifyReport::default();
    verify_objects(repo, opts, &mut rep);
    verify_refs(repo, &mut rep);
    verify_chains(repo, &mut rep);
    verify_workspaces(repo, &mut rep);
    verify_txn_dir(repo, &mut rep);
    verify_config(repo, &mut rep);
    rep
}

// ─────────────────────────── objects ───────────────────────────

fn verify_objects(repo: &Repo, opts: &VerifyOpts, rep: &mut VerifyReport) {
    let objects_dir = repo.ng().join("objects");
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![objects_dir.clone()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                files.push(e.path());
            }
        }
    }
    files.sort();
    for f in files {
        rep.objects_checked += 1;
        let rel = f
            .strip_prefix(&objects_dir)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.ends_with(".corrupt") {
            rep.quarantined += 1;
            rep.issues.push(Issue {
                path: Some(rel.clone()),
                ..warn(
                    "object.quarantined",
                    format!("quarantined corrupt object: {rel} (kept for forensics; delete manually when investigated)"),
                )
            });
            continue;
        }
        // Temp-file debris from a killed writer (`<62hex>.tmp.<pid>.<ns>.<n>`):
        // a warning, not corruption — `sweep_temp_files` removes it once
        // stale (runs on every recover / gc).
        if let Some((shard, f)) = rel.split_once('/') {
            if shard.len() == 2
                && f.contains(".tmp.")
                && f.split(".tmp.")
                    .next()
                    .is_some_and(|p| p.len() == 62 && p.chars().all(|c| c.is_ascii_hexdigit()))
            {
                rep.issues.push(Issue {
                    path: Some(rel.clone()),
                    ..warn(
                        "object.temp_debris",
                        format!(
                            "crash debris (temp object file): {rel} — swept automatically once stale, or delete manually"
                        ),
                    )
                });
                continue;
            }
        }
        if rel.len() != 65 || !rel.contains('/') {
            rep.issues.push(Issue {
                path: Some(rel.clone()),
                ..err(
                    "object.layout",
                    format!("unexpected file in object store: {rel}"),
                )
            });
            continue;
        }
        let hex = rel.replace('/', "");
        let Ok(expected) = ObjectId::from_hex(&hex) else {
            rep.issues.push(Issue {
                path: Some(rel.clone()),
                ..err("object.name", format!("non-hex object name: {rel}"))
            });
            continue;
        };
        let raw = match std::fs::read(&f) {
            Ok(b) => b,
            Err(e) => {
                rep.issues.push(Issue {
                    oid: Some(hex),
                    path: Some(rel.clone()),
                    ..err("object.unreadable", format!("cannot read: {e}"))
                });
                continue;
            }
        };
        match envelope::decode(&raw, repo.limits().max_object_bytes) {
            Ok((obj, id)) => {
                if id != expected {
                    rep.issues.push(Issue {
                        oid: Some(hex),
                        path: Some(rel.clone()),
                        ..err(
                            "object.misfiled",
                            format!("file contains object {id} (expected {expected})"),
                        )
                    });
                    continue;
                }
                if let Err(e) = obj.validate() {
                    rep.issues.push(Issue {
                        oid: Some(hex.clone()),
                        path: Some(rel.clone()),
                        ..err("object.invalid", format!("{e}"))
                    });
                }
                if opts.deep {
                    match envelope::encode(&obj) {
                        Ok(re) if re == raw => {}
                        Ok(_) => {
                            rep.issues.push(Issue {
                                oid: Some(hex.clone()),
                                path: Some(rel.clone()),
                                ..err(
                                    "object.noncanonical",
                                    "re-encoding differs from stored bytes".into(),
                                )
                            });
                        }
                        Err(e) => {
                            rep.issues.push(Issue {
                                oid: Some(hex.clone()),
                                path: Some(rel.clone()),
                                ..err("object.noncanonical", format!("re-encode failed: {e}"))
                            });
                        }
                    }
                    verify_links(repo, &obj, expected, rep);
                }
            }
            Err(e) => {
                // digest mismatch / envelope corruption / parse failure
                rep.issues.push(Issue {
                    oid: Some(hex),
                    path: Some(rel.clone()),
                    ..err("object.corrupt", format!("{e}"))
                });
            }
        }
    }
}

/// Deep-mode link checks (existence + type of referenced objects).
fn verify_links(repo: &Repo, obj: &Object, oid: ObjectId, rep: &mut VerifyReport) {
    let exists = |o: ObjectId, want: ObjectType, what: &str, rep: &mut VerifyReport| match repo
        .objects
        .get(&o)
    {
        Ok(child) => {
            if child.type_tag() != want {
                rep.issues.push(Issue {
                    oid: Some(oid.to_hex()),
                    ..err(
                        "link.type",
                        format!(
                            "{what} {o} is {} (expected {})",
                            child.type_tag().name(),
                            want.name()
                        ),
                    )
                });
            }
        }
        Err(_) => {
            rep.issues.push(Issue {
                oid: Some(oid.to_hex()),
                ..err("link.missing", format!("{what} {o} is missing"))
            });
        }
    };
    match obj {
        Object::Tree(t) => {
            for e in &t.entries {
                if e.mode.is_tree() {
                    exists(e.oid, ObjectType::Tree, "subtree", rep);
                } else {
                    exists(e.oid, ObjectType::Blob, "blob", rep);
                }
            }
        }
        Object::Snapshot(s) => {
            exists(s.root, ObjectType::Tree, "root tree", rep);
            exists(s.author, ObjectType::Actor, "author", rep);
            for p in &s.parents {
                exists(*p, ObjectType::Snapshot, "parent", rep);
            }
            if let Some(g) = s.goal {
                exists(g, ObjectType::Goal, "goal", rep);
            }
            if let Some(c) = s.change {
                exists(c, ObjectType::Change, "change", rep);
            }
        }
        Object::Goal(g) => exists(g.creator, ObjectType::Actor, "creator", rep),
        Object::Change(c) => {
            exists(c.base, ObjectType::Snapshot, "base", rep);
            exists(c.result, ObjectType::Snapshot, "result", rep);
            exists(c.author, ObjectType::Actor, "author", rep);
            for e in &c.evidence {
                exists(*e, ObjectType::Evidence, "evidence", rep);
            }
        }
        Object::Evidence(e) => {
            exists(e.producer, ObjectType::Actor, "producer", rep);
            if let Some(o) = e.output {
                exists(o, ObjectType::Blob, "output blob", rep);
            }
        }
        Object::Evaluation(v) => {
            exists(v.evaluator, ObjectType::Actor, "evaluator", rep);
        }
        Object::Proposal(p) => {
            exists(p.change, ObjectType::Change, "change", rep);
            exists(p.base, ObjectType::Snapshot, "base", rep);
            exists(p.author, ObjectType::Actor, "author", rep);
            for e in &p.evidence {
                exists(*e, ObjectType::Evidence, "evidence", rep);
            }
            for (a, _) in &p.approvals {
                exists(*a, ObjectType::Actor, "approver", rep);
            }
        }
        Object::Blob(_) | Object::Actor(_) => {}
    }
}

// ─────────────────────────── refs ───────────────────────────

fn verify_refs(repo: &Repo, rep: &mut VerifyReport) {
    match repo.read_head() {
        Ok(crate::repo::Head::Symbolic(r)) => {
            if crate::repo::refs::check_ref_name(&r).is_err() {
                rep.issues.push(err(
                    "head.target_invalid",
                    format!("HEAD points at invalid ref name {r:?}"),
                ));
            }
        }
        Ok(crate::repo::Head::Detached(oid)) => {
            if repo.objects.get(&oid).is_err() {
                rep.issues.push(Issue {
                    oid: Some(oid.to_hex()),
                    ..err(
                        "head.target_missing",
                        format!("detached HEAD {oid} missing from store"),
                    )
                });
            }
        }
        Err(e) => rep
            .issues
            .push(err("head.unreadable", format!("HEAD: {e}"))),
    }
    let refs_dir = repo.ng().join("refs");
    let mut names: Vec<String> = Vec::new();
    collect_ref_files(&refs_dir, "", &mut names);
    for name in &names {
        rep.refs_checked += 1;
        let path = match txn::ref_path(repo.ng(), name) {
            Ok(p) => p,
            Err(e) => {
                rep.issues
                    .push(err("ref.name_invalid", format!("{name}: {e}")));
                continue;
            }
        };
        match txn::read_ref_raw(repo.ng(), name) {
            Ok(Some(oid)) => {
                if repo.objects.get(&oid).is_err() {
                    rep.issues.push(Issue {
                        oid: Some(oid.to_hex()),
                        path: Some(path.to_string_lossy().to_string()),
                        ..err(
                            "ref.target_missing",
                            format!("ref {name} → {oid} missing from store"),
                        )
                    });
                }
            }
            Ok(None) => {}
            Err(e) => {
                rep.issues.push(Issue {
                    path: Some(path.to_string_lossy().to_string()),
                    ..err("ref.unreadable", format!("ref {name}: {e}"))
                });
            }
        }
        // reflog line format: OLD NEW ACTOR B64(txn-id) B64(message)
        let log = repo.ng().join("logs").join("refs").join(name);
        if let Ok(text) = std::fs::read_to_string(&log) {
            for (i, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                if parse_reflog_line(line).is_err() {
                    rep.issues.push(Issue {
                        path: Some(log.to_string_lossy().to_string()),
                        ..err(
                            "reflog.line",
                            format!("reflog {name} line {} is malformed", i + 1),
                        )
                    });
                }
            }
        }
    }
}

/// Minimal structural reflog-line parser for verification.
/// Line format (written by the txn engine):
/// `OLD NEW TS_MS ACTOR B64(txn-id) B64(message)` — OLD/NEW/ACTOR are
/// 64-hex or the literal `ZERO`; TS_MS is a non-negative integer.
fn parse_reflog_line(line: &str) -> Result<()> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() != 6 {
        return Err(Error::Malformed("reflog line must have 6 fields".into()));
    }
    for hexish in [&f[0], &f[1], &f[3]] {
        if *hexish != "ZERO" && ObjectId::from_hex(hexish).is_err() {
            return Err(Error::Malformed("reflog hex field invalid".into()));
        }
    }
    if f[2].parse::<i64>().is_err() {
        return Err(Error::Malformed("reflog ts field invalid".into()));
    }
    crate::util::base64::decode(f[4])?;
    crate::util::base64::decode(f[5])?;
    Ok(())
}

pub fn collect_ref_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(".lock") {
            continue; // locks are not refs
        }
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if e.path().is_dir() {
            collect_ref_files(&e.path(), &full, out);
        } else {
            out.push(full);
        }
    }
}

// ─────────────────────────── chains ───────────────────────────

fn verify_chains(repo: &Repo, rep: &mut VerifyReport) {
    let dir = repo.ng().join("refs").join("chains");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut names: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    for name in names {
        rep.chains_checked += 1;
        let Ok(root) = ObjectId::from_hex(&name) else {
            rep.issues.push(err(
                "chain.name",
                format!("chain ref {name:?} is not 64-hex"),
            ));
            continue;
        };
        let head = match txn::read_ref_raw(repo.ng(), &format!("chains/{name}")) {
            Ok(Some(h)) => h,
            Ok(None) => {
                rep.issues
                    .push(err("chain.empty", format!("chain {name} has no value")));
                continue;
            }
            Err(e) => {
                rep.issues
                    .push(err("chain.unreadable", format!("chain {name}: {e}")));
                continue;
            }
        };
        let mut cur = head;
        let mut seen: HashSet<ObjectId> = HashSet::new();
        loop {
            if !seen.insert(cur) {
                rep.issues.push(err(
                    "chain.cycle",
                    format!("chain {name}: prev-link cycle at {cur}"),
                ));
                break;
            }
            let obj = match repo.objects.get(&cur) {
                Ok(o) => o,
                Err(_) => {
                    rep.issues.push(Issue {
                        oid: Some(cur.to_hex()),
                        ..err(
                            "chain.version_missing",
                            format!("chain {name}: version {cur} missing"),
                        )
                    });
                    break;
                }
            };
            let extras = match &obj {
                Object::Goal(g) => &g.extras,
                Object::Change(c) => &c.extras,
                Object::Proposal(p) => &p.extras,
                other => {
                    rep.issues.push(err(
                        "chain.type",
                        format!("chain {name}: version {cur} is {}", other.type_tag().name()),
                    ));
                    break;
                }
            };
            if cur == root {
                if extras.contains_key("prev") {
                    rep.issues.push(err(
                        "chain.root_prev",
                        format!("chain {name}: root version has a prev link"),
                    ));
                }
                break;
            }
            match extras.get("prev") {
                Some(hex) => match ObjectId::from_hex(hex) {
                    Ok(p) => cur = p,
                    Err(_) => {
                        rep.issues.push(err(
                            "chain.prev_invalid",
                            format!("chain {name}: version {cur} has non-hex prev"),
                        ));
                        break;
                    }
                },
                None => {
                    rep.issues.push(err(
                        "chain.prev_missing",
                        format!("chain {name}: version {cur} lacks prev link before root"),
                    ));
                    break;
                }
            }
            if seen.len() > 1_000_000 {
                rep.issues.push(err(
                    "chain.too_long",
                    format!("chain {name}: over 1M versions"),
                ));
                break;
            }
        }
    }
}

// ─────────────────────────── workspaces ───────────────────────────

fn verify_workspaces(repo: &Repo, rep: &mut VerifyReport) {
    let dir = repo.ng().join("workspaces");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            rep.workspaces_checked += 1;
            let is_main = name == crate::repo::workspace::MAIN;
            // Index cache check runs first: it applies even when meta is
            // absent (crash debris) — a corrupt cache is only a warning.
            let idx_path = e.path().join("index");
            if idx_path.exists() {
                if let Ok(bytes) = std::fs::read(&idx_path) {
                    if Index::decode(&bytes).is_err() {
                        rep.issues.push(Issue {
                            path: Some(idx_path.to_string_lossy().to_string()),
                            ..warn(
                                "workspace.index_corrupt",
                                format!(
                                    "workspace {name}: index cache corrupt (safe to delete; rebuilds)"
                                ),
                            )
                        });
                    }
                }
            }
            let meta = e.path().join("meta");
            if !meta.exists() {
                if !is_main {
                    rep.issues.push(Issue {
                        path: Some(e.path().to_string_lossy().to_string()),
                        ..warn(
                            "workspace.orphan_dir",
                            format!(
                                "workspace dir {name} has no meta (crash debris?); safe to delete"
                            ),
                        )
                    });
                }
                continue;
            }
            match std::fs::read_to_string(&meta) {
                Ok(text) => {
                    let mut base_empty = true;
                    for line in text.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with('#') {
                            continue;
                        }
                        let Some((k, v)) = line.split_once('=') else {
                            rep.issues.push(Issue {
                                path: Some(meta.to_string_lossy().to_string()),
                                ..err(
                                    "workspace.meta",
                                    format!("workspace {name}: bad meta line {line:?}"),
                                )
                            });
                            continue;
                        };
                        let (k, v) = (k.trim(), v.trim());
                        if k == "base_oid" {
                            base_empty = v.is_empty();
                            if !v.is_empty() && ObjectId::from_hex(v).is_err() {
                                rep.issues.push(Issue {
                                    path: Some(meta.to_string_lossy().to_string()),
                                    ..err(
                                        "workspace.meta",
                                        format!("workspace {name}: base_oid not hex"),
                                    )
                                });
                            }
                        }
                    }
                    if !is_main {
                        if !e.path().join("files").is_dir() {
                            // Crash between the create-txn commit and the
                            // (unjournaled) checkout, or user deletion: the
                            // workspace is logically intact and repairable —
                            // a warning, not corruption (same policy as
                            // integrate's post-commit checkout, D-012).
                            rep.issues.push(Issue {
                                path: Some(e.path().to_string_lossy().to_string()),
                                ..warn(
                                    "workspace.files_missing",
                                    format!(
                                        "workspace {name}: files/ directory missing — run `newgit checkout -w {name}` to rematerialize"
                                    ),
                                )
                            });
                        }
                        let has_ref = txn::read_ref_raw(repo.ng(), &format!("workspaces/{name}"))
                            .ok()
                            .flatten()
                            .is_some();
                        if !has_ref && !base_empty {
                            rep.issues.push(Issue {
                                path: Some(meta.to_string_lossy().to_string()),
                                ..err(
                                    "workspace.ref_missing",
                                    format!("workspace {name} has a base_oid but no position ref"),
                                )
                            });
                        }
                    }
                }
                Err(e) => {
                    rep.issues.push(Issue {
                        path: Some(meta.to_string_lossy().to_string()),
                        ..err(
                            "workspace.meta",
                            format!("workspace {name}: meta unreadable: {e}"),
                        )
                    });
                }
            }
        }
    }
    // position refs whose workspace dir vanished
    let mut names = Vec::new();
    collect_ref_files(
        &repo.ng().join("refs").join("workspaces"),
        "workspaces",
        &mut names,
    );
    for n in names {
        let ws = n.strip_prefix("workspaces/").unwrap_or(&n);
        if !repo.ng().join("workspaces").join(ws).join("meta").exists() {
            rep.issues.push(err(
                "workspace.meta_missing",
                format!("position ref {n} exists but workspace meta is missing"),
            ));
        }
    }
}

// ─────────────────────────── txn dir / config ───────────────────────────

fn verify_txn_dir(repo: &Repo, rep: &mut VerifyReport) {
    let txn_dir = repo.ng().join("txn");
    if let Ok(rd) = std::fs::read_dir(&txn_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".lock") {
                rep.issues.push(Issue {
                    path: Some(e.path().to_string_lossy().to_string()),
                    ..warn(
                        "txn.lock_present",
                        format!(
                            "lock file present: {name} (held or stale; stale locks auto-reclaim by pid/age)"
                        ),
                    )
                });
            } else if name.ends_with(".journal") {
                rep.issues.push(Issue {
                    path: Some(e.path().to_string_lossy().to_string()),
                    ..warn(
                        "txn.journal_pending",
                        format!(
                            "unrecovered journal {name} — run any newgit command (auto-recovery) or `newgit recover`"
                        ),
                    )
                });
            }
        }
    }
}

fn verify_config(repo: &Repo, rep: &mut VerifyReport) {
    let path = repo.ng().join("config");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            if RepoConfig::parse(&text).is_err() {
                rep.issues.push(Issue {
                    path: Some(path.to_string_lossy().to_string()),
                    ..err("config.parse", "config file does not parse".into())
                });
            }
        }
        Err(e) => {
            rep.issues.push(Issue {
                path: Some(path.to_string_lossy().to_string()),
                ..err("config.unreadable", format!("config unreadable: {e}"))
            });
        }
    }
}

/// Convenience: run a deep verify and require zero errors.
pub fn verify_ok(repo: &Repo) -> Result<VerifyReport> {
    let rep = verify(repo, &VerifyOpts { deep: true });
    if !rep.ok() {
        let msgs: Vec<String> = rep
            .issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .map(|i| format!("[{}] {}", i.code, i.detail))
            .collect();
        return Err(Error::Corrupt {
            oid: ObjectId::from_bytes([0; 32]),
            reason: format!("verify found {} error(s): {}", msgs.len(), msgs.join("; ")),
        });
    }
    Ok(rep)
}

/// Full reachability closure from a set of roots (used by gc).
///
/// Lenient by design: objects that cannot be read (missing, corrupt,
/// quarantined) are collected in the returned `missing` list instead of
/// failing the walk — gc must stay usable on a damaged repo, and `verify`
/// is the tool that reports the damage.
///
/// Follows every link type, including `extras.prev` chain links (older
/// Goal/Change/Proposal versions are reachable through the chain audit
/// trail and must never be collected).
/// Every object id `obj` depends on (its links), including chain-audit
/// `extras.prev` links. Single source of truth for reachability walks
/// (gc, verify, remote negotiation).
pub fn object_links(obj: &Object) -> Vec<ObjectId> {
    let mut out = Vec::new();
    let extras = match obj {
        Object::Goal(g) => Some(&g.extras),
        Object::Change(c) => Some(&c.extras),
        Object::Proposal(p) => Some(&p.extras),
        _ => None,
    };
    if let Some(ex) = extras {
        if let Some(hex) = ex.get("prev") {
            if let Ok(p) = ObjectId::from_hex(hex) {
                out.push(p);
            }
        }
    }
    match obj {
        Object::Tree(t) => out.extend(t.entries.iter().map(|e| e.oid)),
        Object::Snapshot(s) => {
            out.push(s.root);
            out.push(s.author);
            out.extend(s.parents.iter().copied());
            out.extend(s.goal);
            out.extend(s.change);
        }
        Object::Goal(g) => out.push(g.creator),
        Object::Change(c) => {
            out.push(c.base);
            out.push(c.result);
            out.push(c.author);
            out.extend(c.evidence.iter().copied());
        }
        Object::Evidence(e) => {
            out.push(e.producer);
            out.extend(e.target);
            out.extend(e.output);
        }
        Object::Evaluation(v) => {
            out.push(v.target);
            out.push(v.evaluator);
        }
        Object::Proposal(p) => {
            out.push(p.change);
            out.push(p.base);
            out.push(p.author);
            out.extend(p.evidence.iter().copied());
            out.extend(p.depends_on.iter().copied());
            out.extend(p.approvals.iter().map(|(a, _)| *a));
        }
        Object::Actor(_) | Object::Blob(_) => {}
    }
    out
}

pub fn reachable(repo: &Repo, roots: &[ObjectId]) -> (BTreeSet<ObjectId>, Vec<ObjectId>) {
    let mut seen = BTreeSet::new();
    let mut missing = Vec::new();
    let mut stack: Vec<ObjectId> = roots.to_vec();
    while let Some(oid) = stack.pop() {
        if !seen.insert(oid) {
            continue;
        }
        let obj = match repo.objects.get(&oid) {
            Ok(o) => o,
            Err(_) => {
                // Unreadable ⇒ not "live": gc must still see the file in
                // the sweep to classify it (kept_corrupt), and verify
                // reports the damage separately.
                seen.remove(&oid);
                missing.push(oid);
                continue;
            }
        };
        stack.extend(object_links(&obj));
    }
    (seen, missing)
}
