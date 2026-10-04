//! `newgit import-git <git-repo>` — stream a real git repository into a
//! NewGit repository via `git fast-export` (D-007).
//!
//! Guarantees:
//! * **Atomic ref switch**: objects are written first (content-addressed,
//!   idempotent), then ALL imported refs + HEAD move in ONE transaction. A
//!   crash mid-import leaves orphan objects (gc fodder) and unmoved refs —
//!   never a half-imported history.
//! * **Deterministic**: importing the same git repo into two fresh NewGit
//!   repos yields identical object ids (git timestamps, timezones, messages
//!   and identities are preserved; committer is carried in extras when it
//!   differs from author).
//! * **Loud about losses/refusals**: submodules (gitlinks) abort the import
//!   with a clear error; annotated-tag messages are stripped (refs are kept)
//!   and listed in the report; commit-message control characters that the
//!   NewGit text model cannot represent are refused before refs move;
//!   refs/remotes/*, refs/stash, refs/notes/*, refs/replace/*, and symbolic
//!   refs outside HEAD are skipped and listed.
//!
//! Memory: commit tree states are cached per commit mark so incremental
//! (non-full-tree) streams and parent inheritance work; with the default
//! `--full-tree` request each commit is self-contained. Very large histories
//! are bounded by RAM — a documented limitation (KNOWN_LIMITATIONS #20).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::gitio::fastexport::{Committish, Event, FileOp, FxPerson, Parser};
use crate::object::types::{Actor, ActorKind, EntryMode, Object, Snapshot};
use crate::object::ObjectId;
use crate::ops::tree::build_tree;
use crate::repo::txn::{Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;
use crate::util::fsx;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ImportReport {
    pub commits: usize,
    pub blobs: usize,
    pub trees: usize,
    pub actors: usize,
    /// (ref name, snapshot oid hex) actually moved by the import txn.
    pub refs_imported: Vec<(String, String)>,
    /// Source refs deliberately not imported, including refs omitted by Git's
    /// fast-export stream (such as symbolic refs outside HEAD).
    pub refs_skipped: Vec<String>,
    /// Annotated tags whose message/tagger metadata was stripped (the ref
    /// itself IS imported, pointing at its target snapshot).
    pub annotated_tags_stripped: Vec<String>,
    /// HEAD after import, if it could be mapped.
    pub head: Option<String>,
}

/// Refs we never import (git-internal namespaces with no NewGit meaning).
fn skip_ref(name: &str) -> bool {
    name.starts_with("refs/remotes/")
        || name.starts_with("refs/notes/")
        || name.starts_with("refs/replace/")
        || name == "refs/stash"
        || name.starts_with("refs/bisect/")
        || name.starts_with("refs/worktree/")
}

/// `git fast-export --all` omits symbolic refs outside HEAD. Discover them
/// separately so import reports the loss and never reconstructs one as an
/// ordinary direct ref if a Git version happens to emit it in the stream.
fn list_symbolic_refs(git_dir: &Path) -> Result<HashSet<String>> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(git_dir)
        .args(["for-each-ref", "--format=%(refname)%00%(symref)"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(git_dir, e))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "git for-each-ref failed while checking symbolic refs: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    let mut refs = HashSet::new();
    for record in output.stdout.split(|byte| *byte == b'\n') {
        if record.is_empty() {
            continue;
        }
        let Some(separator) = record.iter().position(|byte| *byte == 0) else {
            return Err(Error::Invalid(
                "git for-each-ref returned a malformed ref listing".into(),
            ));
        };
        let (name, target) = record.split_at(separator);
        if target.len() > 1 {
            let name = String::from_utf8(name.to_vec()).map_err(|_| {
                Error::Invalid(
                    "Git symbolic ref name is not valid UTF-8; import is refused before refs move"
                        .into(),
                )
            })?;
            refs.insert(name);
        }
    }
    Ok(refs)
}

pub fn import_git(repo: &Repo, git_dir: &Path) -> Result<ImportReport> {
    // ── validate source ──
    let probe = Command::new("git")
        .args(["-C"])
        .arg(git_dir)
        .args(["rev-parse", "--git-dir"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(git_dir, e))?;
    if !probe.status.success() {
        return Err(Error::Invalid(format!(
            "not a git repository: {} ({})",
            git_dir.display(),
            String::from_utf8_lossy(&probe.stderr).trim()
        )));
    }
    // git's HEAD, so we can mirror it (symbolic or detached).
    let head_out = Command::new("git")
        .args(["-C"])
        .arg(git_dir)
        .args(["symbolic-ref", "-q", "HEAD"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::io(git_dir, e))?;
    let git_head_ref = if head_out.status.success() {
        Some(String::from_utf8_lossy(&head_out.stdout).trim().to_string())
    } else {
        None
    };
    let detached_head_sha = if git_head_ref.is_none() {
        let o = Command::new("git")
            .args(["-C"])
            .arg(git_dir)
            .args(["rev-parse", "-q", "--verify", "HEAD"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Error::io(git_dir, e))?;
        if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        }
    } else {
        None
    };
    let symbolic_ref_names = list_symbolic_refs(git_dir)?;

    // ── stream fast-export ──
    let mut child = Command::new("git")
        .args(["-C"])
        .arg(git_dir)
        .args([
            "fast-export",
            "--all",
            "--full-tree",
            "--show-original-ids",
            "--signed-tags=strip",
            "--tag-of-filtered-object=drop",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::io(git_dir, e))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Bug("fast-export child spawned without stdout".into()))?;

    let mut skipped_symbolic_refs: Vec<String> = symbolic_ref_names.iter().cloned().collect();
    skipped_symbolic_refs.sort();
    let mut rep = ImportReport {
        refs_skipped: skipped_symbolic_refs,
        ..Default::default()
    };
    let mut marks: HashMap<u64, ObjectId> = HashMap::new();
    let mut sha_to_oid: HashMap<String, ObjectId> = HashMap::new();
    let mut tips: BTreeMap<String, ObjectId> = BTreeMap::new();
    let mut trees: HashMap<u64, BTreeMap<String, (EntryMode, ObjectId)>> = HashMap::new();
    let mut actors: HashMap<(String, String), ObjectId> = HashMap::new();

    let mut parser = Parser::from_child_stdout(stdout);
    while let Some(ev) = parser.next_event()? {
        match ev {
            Event::Blob(b) => {
                let oid = repo.objects.put_blob(&b.data)?;
                if let Some(m) = b.mark {
                    marks.insert(m, oid);
                }
                if let Some(sha) = b.git_sha {
                    sha_to_oid.insert(sha, oid);
                }
                rep.blobs += 1;
            }
            Event::Commit(c) => {
                // tree state: deleteall clears; otherwise inherit first parent
                let mut map: BTreeMap<String, (EntryMode, ObjectId)> = BTreeMap::new();
                let mut started = false;
                for op in &c.ops {
                    match op {
                        FileOp::DeleteAll => {
                            map.clear();
                            started = true;
                        }
                        FileOp::Modify {
                            mode,
                            path,
                            mark,
                            inline,
                        } => {
                            if !started {
                                // inherit the first parent's tree lazily
                                if let Some(first_parent_mark) = first_parent_mark(&c.parents) {
                                    if let Some(pm) = trees.get(&first_parent_mark) {
                                        map = pm.clone();
                                    }
                                }
                                started = true;
                            }
                            let em = git_mode(mode, path, c.git_sha.as_deref())?;
                            fsx::check_rel_path(path, 255).map_err(|e| {
                                Error::Invalid(format!(
                                    "git path rejected by NewGit path grammar: {e}"
                                ))
                            })?;
                            let oid = match (mark, inline) {
                                (Some(m), _) => *marks.get(m).ok_or_else(|| {
                                    bad_stream(format!("M references unknown mark :{m}"))
                                })?,
                                (None, Some(data)) => repo.objects.put_blob(data)?,
                                (None, None) => {
                                    return Err(bad_stream("M without mark or inline data"))
                                }
                            };
                            map.insert(path.clone(), (em, oid));
                        }
                        FileOp::Delete { path } => {
                            if !started {
                                if let Some(pmark) = first_parent_mark(&c.parents) {
                                    if let Some(pm) = trees.get(&pmark) {
                                        map = pm.clone();
                                    }
                                }
                                started = true;
                            }
                            map.remove(path);
                        }
                        FileOp::Gitlink { sha, path } => {
                            return Err(Error::Invalid(format!(
                                "git submodule (gitlink) at {path:?} (git object {sha}){} — submodules are \
                                 NOT supported by NewGit (documented limitation); remove the submodule \
                                 from the git repo or import a subdirectory instead",
                                c.git_sha
                                    .as_deref()
                                    .map(|s| format!(" in commit {s}"))
                                    .unwrap_or_default()
                            )))
                        }
                        FileOp::Rename { from, to } => {
                            if !started {
                                if let Some(pmark) = first_parent_mark(&c.parents) {
                                    if let Some(pm) = trees.get(&pmark) {
                                        map = pm.clone();
                                    }
                                }
                                started = true;
                            }
                            let ent = map.remove(from).ok_or_else(|| {
                                bad_stream(format!("R source {from:?} not in tree"))
                            })?;
                            fsx::check_rel_path(to, 255).map_err(|e| {
                                Error::Invalid(format!(
                                    "git path rejected by NewGit path grammar: {e}"
                                ))
                            })?;
                            map.insert(to.clone(), ent);
                        }
                    }
                }
                let items: Vec<(String, ObjectId, EntryMode)> =
                    map.iter().map(|(p, (m, o))| (p.clone(), *o, *m)).collect();
                let root = build_tree(repo, &items)?;
                rep.trees += 1;

                // parents
                let mut parent_oids: Vec<ObjectId> = Vec::new();
                for p in &c.parents {
                    parent_oids.push(resolve_committish(p, &marks, &sha_to_oid, &tips)?);
                }
                parent_oids.sort();
                parent_oids.dedup();

                // author/committer → actors
                let fallback = FxPerson {
                    name: "Git Import".into(),
                    email: "import@newgit.local".into(),
                    ts: 0,
                    tz_min: 0,
                };
                let author_p = c
                    .author
                    .clone()
                    .or_else(|| c.committer.clone())
                    .unwrap_or(fallback);
                let author = get_actor(repo, &mut actors, &author_p, &mut rep)?;

                let mut extras: BTreeMap<String, String> = BTreeMap::new();
                if let Some(sha) = &c.git_sha {
                    extras.insert("git_sha1".into(), sha.clone());
                }
                if let Some(cm) = &c.committer {
                    if cm.name != author_p.name
                        || cm.email != author_p.email
                        || cm.ts != author_p.ts
                        || cm.tz_min != author_p.tz_min
                    {
                        extras.insert("git_committer_name".into(), cm.name.clone());
                        extras.insert("git_committer_email".into(), cm.email.clone());
                        extras.insert(
                            "git_committer_ts_ms".into(),
                            (cm.ts.saturating_mul(1000)).to_string(),
                        );
                        extras.insert("git_committer_tz".into(), cm.tz_min.to_string());
                    }
                }
                if c.parents.len() > 1 {
                    // NewGit parents are a sorted set; preserve git's order
                    // (first-parent lineage matters for history display).
                    let ordered: Vec<String> = c
                        .parents
                        .iter()
                        .map(
                            |p| match resolve_committish(p, &marks, &sha_to_oid, &tips) {
                                Ok(o) => o.to_hex(),
                                Err(_) => String::from("?"),
                            },
                        )
                        .collect();
                    extras.insert("git_parents_ordered".into(), ordered.join(" "));
                }
                let message = match String::from_utf8(c.message.clone()) {
                    Ok(m) => m,
                    Err(_) => {
                        extras.insert("git_message_lossy".into(), "1".into());
                        String::from_utf8_lossy(&c.message).into_owned()
                    }
                };
                if let Some(control) = message.chars().find(|ch| {
                    let code = *ch as u32;
                    (code < 0x20 && !matches!(*ch, '\n' | '\r' | '\t')) || code == 0x7f
                }) {
                    let commit = c
                        .git_sha
                        .as_deref()
                        .map(|sha| format!(" {sha}"))
                        .unwrap_or_default();
                    return Err(Error::Invalid(format!(
                        "Git commit{commit} message contains unsupported control character U+{:04X}; NewGit messages allow LF, CR, and TAB only, so import is refused before updating refs",
                        control as u32
                    )));
                }

                let snap = Snapshot {
                    parents: parent_oids,
                    root,
                    author,
                    timestamp_ms: author_p.ts.saturating_mul(1000),
                    tz_offset_min: author_p.tz_min,
                    message,
                    workspace: None,
                    change: None,
                    goal: None,
                    extras,
                };
                let obj = Object::Snapshot(snap);
                obj.validate()?;
                let oid = repo.objects.put(&obj)?;
                rep.commits += 1;
                if let Some(m) = c.mark {
                    marks.insert(m, oid);
                    trees.insert(m, map);
                }
                if let Some(sha) = c.git_sha {
                    sha_to_oid.insert(sha, oid);
                }
                if skip_ref(&c.ref_name) || symbolic_ref_names.contains(&c.ref_name) {
                    if !rep.refs_skipped.contains(&c.ref_name) {
                        rep.refs_skipped.push(c.ref_name.clone());
                    }
                } else {
                    tips.insert(c.ref_name, oid);
                }
            }
            Event::Tag(t) => {
                if skip_ref(&t.ref_name) || symbolic_ref_names.contains(&t.ref_name) {
                    if !rep.refs_skipped.contains(&t.ref_name) {
                        rep.refs_skipped.push(t.ref_name.clone());
                    }
                    continue;
                }
                let oid = resolve_committish(&t.from, &marks, &sha_to_oid, &tips)?;
                if t.tagger.is_some() {
                    // NewGit has no tag object: the ref is imported pointing
                    // at its target; tagger/message metadata is reported as
                    // stripped (documented limitation).
                    rep.annotated_tags_stripped.push(t.ref_name.clone());
                }
                tips.insert(t.ref_name, oid);
            }
            Event::Reset(r) => {
                if skip_ref(&r.ref_name) || symbolic_ref_names.contains(&r.ref_name) {
                    if !rep.refs_skipped.contains(&r.ref_name) {
                        rep.refs_skipped.push(r.ref_name.clone());
                    }
                    continue;
                }
                match r.from {
                    Some(c) => {
                        let oid = resolve_committish(&c, &marks, &sha_to_oid, &tips)?;
                        tips.insert(r.ref_name, oid);
                    }
                    None => {
                        tips.remove(&r.ref_name);
                    }
                }
            }
            Event::Meta(_) => {}
        }
    }

    // fast-export must have exited cleanly — a truncated stream is an error,
    // never a partial import.
    let status = child.wait().map_err(|e| Error::io(git_dir, e))?;
    if !status.success() {
        let mut err = String::new();
        if let Some(e) = child.stderr.take() {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = std::io::BufReader::new(e).read_to_end(&mut buf);
            err = String::from_utf8_lossy(&buf).trim().to_string();
        }
        return Err(Error::Invalid(format!(
            "git fast-export failed ({status}): {err}"
        )));
    }

    // ── atomic ref switch (+ HEAD in the same txn) ──
    let mut ops: Vec<TxnOp> = Vec::new();
    for (name, oid) in &tips {
        // `git fast-export --all` emits a pseudo-ref named exactly `HEAD`
        // when the source repository is detached. It is a stream label for
        // the detached tip, not a Git ref (real refs are fully qualified).
        if name == "HEAD" {
            continue;
        }
        if crate::repo::refs::check_ref_name(name).is_err() {
            rep.refs_skipped.push(name.clone());
            continue;
        }
        ops.push(TxnOp::Ref {
            name: name.clone(),
            cas: Cas::Any,
            new: Some(*oid),
            log: RefLogEntry::system(format!("import-git from {}", git_dir.display())),
        });
        rep.refs_imported.push((name.clone(), oid.to_hex()));
    }
    // HEAD mapping: symbolic git HEAD if its ref was imported; else detached
    // at the mapped commit; else leave NewGit's HEAD alone.
    let mut head_desc: Option<String> = None;
    if let Some(hr) = &git_head_ref {
        if !skip_ref(hr) && tips.contains_key(hr) {
            ops.push(TxnOp::File {
                rel: "HEAD".into(),
                data: format!("ref: {hr}\n").into_bytes(),
            });
            head_desc = Some(format!("ref: {hr}"));
        }
    } else if let Some(sha) = &detached_head_sha {
        if let Some(oid) = sha_to_oid.get(sha) {
            ops.push(TxnOp::File {
                rel: "HEAD".into(),
                data: format!("{}\n", oid.to_hex()).into_bytes(),
            });
            head_desc = Some(oid.to_hex());
        }
    }
    if !ops.is_empty() {
        crate::repo::txn::execute(repo.ng(), ops, repo.limits())?;
    }
    rep.head = head_desc;
    Ok(rep)
}

fn first_parent_mark(parents: &[Committish]) -> Option<u64> {
    parents.iter().find_map(|p| match p {
        Committish::Mark(m) => Some(*m),
        _ => None,
    })
}

fn resolve_committish(
    c: &Committish,
    marks: &HashMap<u64, ObjectId>,
    shas: &HashMap<String, ObjectId>,
    tips: &BTreeMap<String, ObjectId>,
) -> Result<ObjectId> {
    match c {
        Committish::Mark(m) => marks
            .get(m)
            .copied()
            .ok_or_else(|| bad_stream(format!("unknown mark :{m}"))),
        Committish::Sha(s) => shas
            .get(s)
            .copied()
            .ok_or_else(|| bad_stream(format!("unknown original-oid {s}"))),
        Committish::Ref(r) => tips
            .get(r)
            .copied()
            .ok_or_else(|| bad_stream(format!("unknown ref committish {r}"))),
    }
}

fn git_mode(mode: &str, path: &str, git_sha: Option<&str>) -> Result<EntryMode> {
    Ok(match mode {
        "100644" => EntryMode::File,
        "100755" => EntryMode::Executable,
        "120000" => EntryMode::Symlink,
        "040000" => EntryMode::Tree,
        "160000" => {
            return Err(Error::Invalid(format!(
                "git submodule (gitlink) at {path:?}{} — submodules are NOT supported by NewGit \
                 (documented limitation); remove the submodule from the git repo or import a \
                 subdirectory instead",
                git_sha
                    .map(|s| format!(" in commit {s}"))
                    .unwrap_or_default()
            )))
        }
        other => {
            return Err(bad_stream(format!(
                "unsupported git mode {other:?} at {path:?}"
            )))
        }
    })
}

fn get_actor(
    repo: &Repo,
    cache: &mut HashMap<(String, String), ObjectId>,
    p: &FxPerson,
    rep: &mut ImportReport,
) -> Result<ObjectId> {
    let key = (p.name.clone(), p.email.clone());
    if let Some(oid) = cache.get(&key) {
        return Ok(*oid);
    }
    let mut extras = BTreeMap::new();
    extras.insert("email".into(), p.email.clone());
    extras.insert("source".into(), "git-import".into());
    let id = if p.email.is_empty() {
        format!("git:{}", p.name)
    } else {
        format!("git:{}", p.email)
    };
    let actor = Actor {
        kind: ActorKind::Human,
        id,
        display_name: p.name.clone(),
        tool: "git-import".into(),
        tool_version: String::new(),
        pubkey: None,
        extras,
    };
    let obj = Object::Actor(actor);
    obj.validate()?;
    let oid = repo.objects.put(&obj)?;
    cache.insert(key, oid);
    rep.actors += 1;
    Ok(oid)
}

fn bad_stream(msg: impl Into<String>) -> Error {
    Error::Malformed(format!("fast-export stream: {}", msg.into()))
}
