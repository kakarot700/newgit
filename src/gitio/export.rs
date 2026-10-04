//! `newgit export-git <target-dir>` — stream NewGit history into a real git
//! repository via `git fast-import` (D-007).
//!
//! Mapping:
//! * Snapshots → git commits (author/committer identities reconstructed from
//!   Actor objects and `git_committer_*` extras when present; timestamps in
//!   whole seconds — sub-second precision is lost, documented),
//! * Trees/blobs → git trees/blobs byte-exact (modes File/Executable/Symlink
//!   → 100644/100755/120000),
//! * Merge parents: git first-parent order is restored from the
//!   `git_parents_ordered` extra when present, else sorted order,
//! * Ref mapping: `refs/heads/*` and `refs/tags/*` pass through; any other
//!   `refs/X` maps to `refs/heads/X`; bare names map to `refs/heads/<name>`;
//!   Git namespace refs (`refs/namespaces/*`) and NewGit-internal namespaces
//!   (`workspaces/*`, `chains/*`) are skipped and reported,
//! * Annotated git tags imported earlier become lightweight tags (their
//!   messages were stripped at import — documented round-trip loss).
//!
//! The stream is deterministic: same repository ⇒ same marks, same bytes.
//! `feature done`/`done` framing makes truncated streams a loud error inside
//! git fast-import.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::object::types::{EntryMode, Object};
use crate::object::ObjectId;
use crate::ops::tree::flatten_tree;
use crate::repo::Repo;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ExportReport {
    pub commits: usize,
    pub blobs: usize,
    /// (newgit ref name, git ref name)
    pub refs_exported: Vec<(String, String)>,
    pub refs_skipped: Vec<String>,
    /// git-side HEAD after export (branch name or detached sha).
    pub head: Option<String>,
    pub target: String,
}

type Flat = BTreeMap<String, (EntryMode, ObjectId)>;

pub fn export_git(repo: &Repo, target: &Path) -> Result<ExportReport> {
    // ── target must be absent or empty ──
    if target.exists() {
        let empty = std::fs::read_dir(target)
            .map_err(|e| Error::io(target, e))?
            .next()
            .is_none();
        if !empty {
            return Err(Error::Invalid(format!(
                "export target {} exists and is not empty",
                target.display()
            )));
        }
    }

    let mut rep = ExportReport {
        target: target.display().to_string(),
        ..Default::default()
    };

    // ── collect refs ──
    let mut names: Vec<String> = Vec::new();
    crate::ops::verify::collect_ref_files(&repo.ng().join("refs"), "", &mut names);
    let mut tips: BTreeMap<String, ObjectId> = BTreeMap::new(); // git-name → oid
    let mut git_to_newgit: BTreeMap<String, String> = BTreeMap::new();
    for n in &names {
        if n.starts_with("refs/namespaces/")
            || n.starts_with("workspaces/")
            || n.starts_with("chains/")
        {
            rep.refs_skipped.push(n.clone());
            continue;
        }
        let Some(oid) = repo.refs.read_opt(n)? else {
            continue;
        };
        // every exported ref must point at a snapshot
        match repo.objects.get(&oid)? {
            Object::Snapshot(_) => {}
            other => {
                return Err(Error::Invalid(format!(
                    "ref {n} points at {} — only snapshot histories can be exported to git",
                    other.type_tag().name()
                )))
            }
        }
        let g = map_ref_name(n);
        if let Some(previous) = git_to_newgit.get(&g) {
            return Err(Error::Invalid(format!(
                "NewGit refs {previous:?} and {n:?} both map to Git ref {g:?}; refusing lossy export"
            )));
        }
        tips.insert(g.clone(), oid);
        git_to_newgit.insert(g.clone(), n.clone());
        rep.refs_exported.push((n.clone(), g));
    }
    let export_head = repo.read_head()?;
    // Git fast-import needs a ref on which to emit the detached history. Use
    // a temporary ref, then detach HEAD and delete it after import; it must
    // never leak into the exported repository's visible refs.
    let detached_export_ref = if let crate::repo::Head::Detached(oid) = &export_head {
        match repo.objects.get(oid)? {
            Object::Snapshot(_) => {}
            other => {
                return Err(Error::Invalid(format!(
                    "detached HEAD points at {} — only snapshot histories can be exported to git",
                    other.type_tag().name()
                )))
            }
        }
        let base = "refs/heads/newgit-export-detached-head";
        let mut candidate = base.to_string();
        let mut suffix = 0usize;
        while tips.keys().any(|name| ref_names_conflict(name, &candidate)) {
            suffix += 1;
            candidate = format!("{base}-{suffix}");
        }
        tips.insert(candidate.clone(), *oid);
        Some(candidate)
    } else {
        None
    };
    if tips.is_empty() {
        return Err(Error::Invalid(
            "nothing to export: repository has no snapshot refs (workspaces/chains are internal)"
                .into(),
        ));
    }

    // ── collect commits (topological, parents before children) ──
    let order = topo_order(repo, &tips)?;

    // ── blob discovery in deterministic order → marks ──
    let mut flats: HashMap<ObjectId, Flat> = HashMap::new();
    let mut blob_marks: BTreeMap<ObjectId, u64> = BTreeMap::new();
    for c in &order {
        let flat = flat_of(repo, c, &mut flats)?;
        for (_, (_, oid)) in flat {
            if !blob_marks.contains_key(&oid) {
                let m = blob_marks.len() as u64 + 1;
                blob_marks.insert(oid, m);
            }
        }
    }
    let commit_marks: HashMap<ObjectId, u64> = order
        .iter()
        .enumerate()
        .map(|(i, o)| (*o, blob_marks.len() as u64 + 1 + i as u64))
        .collect();

    // ── ref ownership for commit lines (first ref in sorted order owns) ──
    let mut owner: HashMap<ObjectId, String> = HashMap::new();
    for (gname, tip) in &tips {
        let mut stack = vec![*tip];
        let mut seen = HashSet::new();
        while let Some(o) = stack.pop() {
            if !seen.insert(o) {
                continue;
            }
            owner.entry(o).or_insert_with(|| gname.clone());
            if let Ok(Object::Snapshot(s)) = repo.objects.get(&o) {
                stack.extend(s.parents.iter().copied());
            }
        }
    }

    // ── run git init + fast-import ──
    let init = Command::new("git")
        .args(["init", "--quiet"])
        .arg(target)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(target, e))?;
    if !init.status.success() {
        return Err(Error::Invalid(format!(
            "git init failed: {}",
            String::from_utf8_lossy(&init.stderr).trim()
        )));
    }
    let marks_file = target.join(".git").join("newgit-export-marks");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(target)
        .arg("fast-import")
        .arg("--quiet")
        .arg("--done")
        .arg(format!("--export-marks={}", marks_file.display()))
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::io(target, e))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Bug("fast-import without stdin".into()))?;
    let mut w = std::io::BufWriter::new(stdin);

    writeln!(w, "feature done").map_err(|e| Error::io(target, e))?;
    // blobs
    for (oid, mark) in &blob_marks {
        let data = match repo.objects.get(oid)? {
            Object::Blob(b) => b,
            _ => return Err(Error::Bug(format!("{oid} is not a blob during export"))),
        };
        writeln!(w, "blob\nmark :{mark}\ndata {}", data.len()).map_err(|e| Error::io(target, e))?;
        w.write_all(&data).map_err(|e| Error::io(target, e))?;
        w.write_all(b"\n").map_err(|e| Error::io(target, e))?;
        rep.blobs += 1;
    }
    // commits in topo order
    for c in &order {
        let gname = owner
            .get(c)
            .ok_or_else(|| Error::Bug(format!("commit {c} without owning ref")))?;
        let ec = EmitCtx {
            blob_marks: &blob_marks,
            commit_marks: &commit_marks,
            flats: &flats,
            target,
        };
        emit_commit(repo, &mut w, *c, gname, &ec)?;
        rep.commits += 1;
    }
    // final ref positions (guarantees tips even for fully-shared branches)
    for (gname, tip) in &tips {
        let m = commit_marks[tip];
        writeln!(w, "reset {gname}\nfrom :{m}").map_err(|e| Error::io(target, e))?;
    }
    writeln!(w, "done").map_err(|e| Error::io(target, e))?;
    w.flush().map_err(|e| Error::io(target, e))?;
    drop(w); // close stdin → fast-import finishes

    let status = child.wait().map_err(|e| Error::io(target, e))?;
    if !status.success() {
        let mut err = String::new();
        if let Some(e) = child.stderr.take() {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = std::io::BufReader::new(e).read_to_end(&mut buf);
            err = String::from_utf8_lossy(&buf).trim().to_string();
        }
        return Err(Error::Invalid(format!(
            "git fast-import failed ({status}): {err}"
        )));
    }

    // ── HEAD + working tree materialization ──
    let marks_by_mark = read_marks_file(&marks_file)?;
    let _ = std::fs::remove_file(&marks_file);
    match export_head {
        crate::repo::Head::Symbolic(name) => {
            let g = if tips.contains_key(&map_ref_name(&name)) {
                map_ref_name(&name)
            } else {
                // HEAD ref not exported (unborn/internal): fall back to the
                // first exported branch so the worktree is usable.
                tips.keys().next().cloned().unwrap_or_default()
            };
            run_git(target, &["symbolic-ref", "HEAD", &g])?;
            run_git(target, &["reset", "--hard", "--quiet"])?;
            rep.head = Some(g);
        }
        crate::repo::Head::Detached(oid) => {
            let mark = *commit_marks
                .get(&oid)
                .ok_or_else(|| Error::Bug(format!("detached HEAD {oid} not in export set")))?;
            let sha = marks_by_mark
                .get(&mark)
                .cloned()
                .ok_or_else(|| Error::Bug(format!("mark :{mark} missing from git export-marks")))?;
            run_git(target, &["checkout", "--detach", "--quiet", &sha])?;
            if let Some(temp_ref) = &detached_export_ref {
                run_git(target, &["update-ref", "-d", temp_ref])?;
            }
            rep.head = Some(format!("detached:{sha}"));
        }
    }
    // map report refs back for clarity
    rep.refs_exported.sort();
    Ok(rep)
}

/// Everything `emit_commit` needs besides the commit itself (keeps the
/// argument count sane — clippy too_many_arguments).
struct EmitCtx<'a> {
    blob_marks: &'a BTreeMap<ObjectId, u64>,
    commit_marks: &'a HashMap<ObjectId, u64>,
    flats: &'a HashMap<ObjectId, Flat>,
    target: &'a Path,
}

fn emit_commit<W: Write>(
    repo: &Repo,
    w: &mut W,
    oid: ObjectId,
    gname: &str,
    ec: &EmitCtx<'_>,
) -> Result<()> {
    let EmitCtx {
        blob_marks,
        commit_marks,
        flats,
        target,
    } = ec;
    let snap = match repo.objects.get(&oid)? {
        Object::Snapshot(s) => s,
        _ => return Err(Error::Bug(format!("{oid} is not a snapshot during export"))),
    };
    // parents in git order: extras.git_parents_ordered when present
    let parents: Vec<ObjectId> = match snap.extras.get("git_parents_ordered") {
        Some(list) => {
            let v: Vec<ObjectId> = list
                .split_whitespace()
                .filter_map(|h| ObjectId::from_hex(h).ok())
                .collect();
            if v.len() == snap.parents.len() {
                v
            } else {
                snap.parents.clone()
            }
        }
        None => snap.parents.clone(),
    };
    writeln!(w, "commit {gname}").map_err(|e| Error::io(target, e))?;
    writeln!(w, "mark :{}", commit_marks[&oid]).map_err(|e| Error::io(target, e))?;
    // identities
    let (aname, aemail) = actor_identity(repo, &snap.author)?;
    let ats = snap.timestamp_ms.div_euclid(1000);
    let atz = tz_string(snap.tz_offset_min);
    writeln!(w, "author {aname} <{aemail}> {ats} {atz}").map_err(|e| Error::io(target, e))?;
    let (cname, cemail, cts, ctz) = match snap.extras.get("git_committer_name") {
        Some(cn) => (
            cn.clone(),
            snap.extras
                .get("git_committer_email")
                .cloned()
                .unwrap_or_else(|| aemail.clone()),
            snap.extras
                .get("git_committer_ts_ms")
                .and_then(|s| s.parse::<i64>().ok())
                .map(|ms| ms.div_euclid(1000))
                .unwrap_or(ats),
            snap.extras
                .get("git_committer_tz")
                .and_then(|s| s.parse::<i16>().ok())
                .map(tz_string)
                .unwrap_or_else(|| atz.clone()),
        ),
        None => (aname.clone(), aemail.clone(), ats, atz.clone()),
    };
    writeln!(w, "committer {cname} <{cemail}> {cts} {ctz}").map_err(|e| Error::io(target, e))?;
    // message
    let msg = snap.message.as_bytes();
    writeln!(w, "data {}", msg.len()).map_err(|e| Error::io(target, e))?;
    w.write_all(msg).map_err(|e| Error::io(target, e))?;
    w.write_all(b"\n").map_err(|e| Error::io(target, e))?;
    // lineage
    if let Some((first, rest)) = parents.split_first() {
        writeln!(w, "from :{}", commit_marks[first]).map_err(|e| Error::io(target, e))?;
        for p in rest {
            writeln!(w, "merge :{}", commit_marks[p]).map_err(|e| Error::io(target, e))?;
        }
    }
    // file ops: diff against first parent (full list for roots)
    let flat = flats
        .get(&oid)
        .ok_or_else(|| Error::Bug(format!("missing flat tree for {oid}")))?;
    let base: &Flat = match parents.first() {
        Some(p) => flats
            .get(p)
            .ok_or_else(|| Error::Bug(format!("missing flat tree for parent {p}")))?,
        None => &BTreeMap::new(),
    };
    for path in base.keys() {
        if !flat.contains_key(path) {
            writeln!(w, "D {}", quote_path(path)).map_err(|e| Error::io(target, e))?;
        }
    }
    for (path, (mode, boid)) in flat {
        let changed = match base.get(path) {
            Some((bm, bo)) => bm != mode || bo != boid,
            None => true,
        };
        if changed {
            writeln!(
                w,
                "M {} :{} {}",
                git_mode_str(*mode)?,
                blob_marks[boid],
                quote_path(path)
            )
            .map_err(|e| Error::io(target, e))?;
        }
    }
    writeln!(w).map_err(|e| Error::io(target, e))?;
    Ok(())
}

/// Topological order over all snapshots reachable from tips (parents first).
fn topo_order(repo: &Repo, tips: &BTreeMap<String, ObjectId>) -> Result<Vec<ObjectId>> {
    let mut order: Vec<ObjectId> = Vec::new();
    let mut entered: HashSet<ObjectId> = HashSet::new();
    let mut done: HashSet<ObjectId> = HashSet::new();
    for tip in tips.values() {
        let mut stack: Vec<(ObjectId, bool)> = vec![(*tip, false)];
        while let Some((o, post)) = stack.pop() {
            if done.contains(&o) {
                continue;
            }
            if post {
                done.insert(o);
                order.push(o);
                continue;
            }
            if !entered.insert(o) {
                continue; // expansion already scheduled
            }
            let snap = match repo.objects.get(&o)? {
                Object::Snapshot(s) => s,
                other => {
                    return Err(Error::Invalid(format!(
                        "history contains {} where a snapshot was expected ({o})",
                        other.type_tag().name()
                    )))
                }
            };
            stack.push((o, true));
            for p in snap.parents.iter().rev() {
                stack.push((*p, false));
            }
        }
    }
    Ok(order)
}

fn flat_of(repo: &Repo, oid: &ObjectId, cache: &mut HashMap<ObjectId, Flat>) -> Result<Flat> {
    if let Some(f) = cache.get(oid) {
        return Ok(f.clone());
    }
    let snap = match repo.objects.get(oid)? {
        Object::Snapshot(s) => s,
        _ => return Err(Error::Bug(format!("{oid} not a snapshot"))),
    };
    let mut flat = Flat::new();
    for (path, mode, boid) in flatten_tree(repo, snap.root)? {
        flat.insert(path, (mode, boid));
    }
    cache.insert(*oid, flat.clone());
    Ok(flat)
}

fn actor_identity(repo: &Repo, actor: &ObjectId) -> Result<(String, String)> {
    match repo.objects.get(actor)? {
        Object::Actor(a) => {
            let name = if a.display_name.is_empty() {
                "NewGit User".to_string()
            } else {
                a.display_name.clone()
            };
            let email = a
                .extras
                .get("email")
                .filter(|e| !e.is_empty())
                .cloned()
                .unwrap_or_else(|| "exported@newgit.local".to_string());
            Ok((name, email))
        }
        _ => Err(Error::Bug(format!("{actor} is not an actor"))),
    }
}

fn git_mode_str(m: EntryMode) -> Result<&'static str> {
    Ok(match m {
        EntryMode::File => "100644",
        EntryMode::Executable => "100755",
        EntryMode::Symlink => "120000",
        EntryMode::Tree => return Err(Error::Bug("tree entry in flattened file list".into())),
    })
}

fn tz_string(min: i16) -> String {
    let sign = if min < 0 { '-' } else { '+' };
    let m = min.unsigned_abs();
    format!("{sign}{:02}{:02}", m / 60, m % 60)
}

/// C-quote only when git would (control chars, quote, backslash).
fn quote_path(p: &str) -> String {
    let needs = p
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_ascii_control());
    if !needs {
        return p.to_string();
    }
    let mut out = String::with_capacity(p.len() + 2);
    out.push('"');
    for c in p.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => out.push_str(&format!("\\{:03o}", c as u8)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// NewGit ref name → git ref name. Branch/tag namespaces pass through;
/// everything else lands under refs/heads/ (git tooling expects branches
/// there): `refs/main` → `refs/heads/main`, `main` → `refs/heads/main`.
/// Namespace-shaped refs are excluded and reported by `export_git` before this
/// mapping, since flattening them would change their Git meaning.
pub fn map_ref_name(name: &str) -> String {
    if name.starts_with("refs/heads/") || name.starts_with("refs/tags/") {
        name.to_string()
    } else if let Some(rest) = name.strip_prefix("refs/") {
        format!("refs/heads/{rest}")
    } else {
        format!("refs/heads/{name}")
    }
}

/// Git cannot store both a ref and another ref below it (for example,
/// `refs/heads/topic` and `refs/heads/topic/child`).
fn ref_names_conflict(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with('/'))
}

/// Parse git's `--export-marks` output: `:<mark> <git-sha>` per line.
fn read_marks_file(path: &Path) -> Result<HashMap<u64, String>> {
    let mut out = HashMap::new();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(Error::io(path, e)),
    };
    for line in text.lines() {
        let Some(rest) = line.strip_prefix(':') else {
            continue;
        };
        let Some((mark, sha)) = rest.split_once(' ') else {
            continue;
        };
        if let Ok(m) = mark.parse::<u64>() {
            out.insert(m, sha.trim().to_string());
        }
    }
    Ok(out)
}

fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(dir, e))?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}
