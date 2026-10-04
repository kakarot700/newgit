//! Three-way tree merge.
//!
//! v1 semantics (deterministic, honest about scope — KNOWN_LIMITATIONS #14):
//! * per-path 3-way resolution on flattened trees,
//! * exact-content rename tracking per side (same mode+oid at a new path);
//!   rename+edit merges cleanly; rename/rename to different paths and
//!   rename/delete are conflicts,
//! * text files with distinct changes on both sides merge via diff3;
//!   overlapping edits produce a conflict *and* a merged-with-markers blob
//!   stored in the object store (its oid is reported for inspection),
//! * symlinks and binaries never content-merge: divergent targets/bytes on
//!   both sides ⇒ conflict,
//! * mode changes combine when unambiguous (one side changed mode, other
//!   changed content ⇒ both apply; different mode changes ⇒ conflict),
//! * modify/delete ⇒ conflict (never silently resurrect or drop),
//! * add/add merges like git with an empty virtual base (identical ⇒ clean,
//!   otherwise conflict),
//! * directory/file collisions ⇒ conflict.
//!
//! The merge NEVER writes refs or workspace files — `ops::integrate` owns
//! the atomic commit + checkout.

pub mod base;
pub mod diff3;

use std::collections::BTreeMap;

use diff3::{merge_lines, render_merged};
use serde::Serialize;

use crate::diff::myers::{is_binary, split_lines};
use crate::error::Result;
use crate::object::types::{EntryMode, Object, Tree};
use crate::object::ObjectId;
use crate::ops::tree::{build_tree, flatten_tree};
use crate::repo::Repo;

#[derive(Clone, Debug)]
pub struct MergeOpts {
    pub track_renames: bool,
    pub max_edit_distance: usize,
    /// Include the `||||||| base` section in conflict markers.
    pub conflict_style_diff3: bool,
}

impl Default for MergeOpts {
    fn default() -> Self {
        MergeOpts {
            track_renames: true,
            max_edit_distance: 1024,
            conflict_style_diff3: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Conflict {
    /// Both sides edited text with overlapping changes. `merged_oid` is a
    /// stored blob containing the marker-annotated merge for inspection.
    Content {
        path: String,
        merged_oid: ObjectId,
        capped: bool,
    },
    /// Non-textual divergence (binaries, or differing symlink targets).
    Opaque {
        path: String,
        base_oid: Option<ObjectId>,
        ours_oid: Option<ObjectId>,
        theirs_oid: Option<ObjectId>,
        reason: String,
    },
    ModifyDelete {
        path: String,
        /// "ours" or "theirs": the side that modified; the other deleted.
        modified_by: String,
        kept_oid: ObjectId,
    },
    RenameRename {
        base_path: String,
        ours_path: String,
        theirs_path: String,
    },
    RenameDelete {
        base_path: String,
        new_path: String,
        renamed_by: String,
        kept_oid: ObjectId,
    },
    Mode {
        path: String,
        ours_mode: EntryMode,
        theirs_mode: EntryMode,
    },
    DirectoryFile {
        file_path: String,
        dir_path: String,
    },
}

impl Conflict {
    pub fn path(&self) -> &str {
        match self {
            Conflict::Content { path, .. } => path,
            Conflict::Opaque { path, .. } => path,
            Conflict::ModifyDelete { path, .. } => path,
            Conflict::RenameRename { ours_path, .. } => ours_path,
            Conflict::RenameDelete { new_path, .. } => new_path,
            Conflict::Mode { path, .. } => path,
            Conflict::DirectoryFile { file_path, .. } => file_path,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MergeOutcome {
    pub clean: bool,
    /// Root tree of the merge — present even when conflicts exist (with
    /// best-effort ours-side resolution) so callers can inspect; `integrate`
    /// refuses to commit unless `clean`.
    pub root: ObjectId,
    pub entries: usize,
    pub conflicts: Vec<Conflict>,
    /// Informational: renames applied during the merge (base → new).
    pub renames: Vec<(String, String)>,
}

type Flat = BTreeMap<String, (EntryMode, ObjectId)>;

struct Ctx<'a> {
    repo: &'a Repo,
    opts: &'a MergeOpts,
    conflicts: Vec<Conflict>,
}

pub fn merge_trees(
    repo: &Repo,
    base_root: ObjectId,
    ours_root: ObjectId,
    theirs_root: ObjectId,
    opts: &MergeOpts,
) -> Result<MergeOutcome> {
    let mut ctx = Ctx {
        repo,
        opts,
        conflicts: Vec::new(),
    };
    if ours_root == theirs_root {
        return Ok(MergeOutcome {
            clean: true,
            root: ours_root,
            entries: flatten_tree(repo, ours_root)?.len(),
            conflicts: Vec::new(),
            renames: Vec::new(),
        });
    }
    let mut base: Flat = flatten_tree(repo, base_root)?
        .into_iter()
        .map(|(p, m, o)| (p, (m, o)))
        .collect();
    let mut ours: Flat = flatten_tree(repo, ours_root)?
        .into_iter()
        .map(|(p, m, o)| (p, (m, o)))
        .collect();
    let mut theirs: Flat = flatten_tree(repo, theirs_root)?
        .into_iter()
        .map(|(p, m, o)| (p, (m, o)))
        .collect();
    let mut renames: Vec<(String, String)> = Vec::new();

    if opts.track_renames {
        let ro = detect_exact_renames(&base, &ours);
        let rt = detect_exact_renames(&base, &theirs);
        apply_renames(
            &mut ctx,
            &mut base,
            &mut ours,
            &mut theirs,
            &ro,
            &rt,
            &mut renames,
        )?;
    }

    let mut merged: Flat = BTreeMap::new();
    let mut all: Vec<&String> = ours.keys().collect();
    for k in theirs.keys() {
        if !ours.contains_key(k) {
            all.push(k);
        }
    }
    all.sort();
    all.dedup();
    for p in all {
        let b = base.get(p);
        let o = ours.get(p);
        let t = theirs.get(p);
        match (o, t) {
            (Some((om, oo)), Some((tm, to))) => {
                if om == tm && oo == to {
                    merged.insert(p.clone(), (*om, *oo));
                    continue;
                }
                let base_e = match b {
                    // real base entry, or empty virtual base for add/add
                    Some(e) => *e,
                    None => (*om, EMPTY_OID),
                };
                if let Some(entry) =
                    resolve_both_changed(&mut ctx, p, base_e, (*om, *oo), (*tm, *to))?
                {
                    merged.insert(p.clone(), entry);
                } else {
                    // conflict recorded; keep ours-side bytes so the tree is
                    // inspectable (integrate refuses to commit it)
                    merged.insert(p.clone(), (*om, *oo));
                }
            }
            (Some((om, oo)), None) => {
                match b {
                    Some(be) if be == &(*om, *oo) => continue, // untouched ⇒ deletion wins
                    Some(_) => {
                        ctx.conflicts.push(Conflict::ModifyDelete {
                            path: p.clone(),
                            modified_by: "ours".into(),
                            kept_oid: *oo,
                        });
                        merged.insert(p.clone(), (*om, *oo));
                    }
                    None => {
                        merged.insert(p.clone(), (*om, *oo)); // ours-only add
                    }
                }
            }
            (None, Some((tm, to))) => {
                match b {
                    Some(be) if be == &(*tm, *to) => continue,
                    Some(_) => {
                        ctx.conflicts.push(Conflict::ModifyDelete {
                            path: p.clone(),
                            modified_by: "theirs".into(),
                            kept_oid: *to,
                        });
                        merged.insert(p.clone(), (*tm, *to));
                    }
                    None => {
                        merged.insert(p.clone(), (*tm, *to)); // theirs-only add
                    }
                }
            }
            (None, None) => continue, // deleted on both sides (or never existed)
        }
    }

    // directory/file collisions
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    let paths: Vec<String> = merged.keys().cloned().collect();
    for p in &paths {
        let mut prefix = String::new();
        for comp in p.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(comp);
            if &prefix != p
                && merged.contains_key(&prefix)
                && seen.insert((p.clone(), prefix.clone()))
            {
                ctx.conflicts.push(Conflict::DirectoryFile {
                    file_path: p.clone(),
                    dir_path: prefix.clone(),
                });
            }
        }
    }

    let items: Vec<(String, ObjectId, EntryMode)> =
        merged.into_iter().map(|(p, (m, o))| (p, o, m)).collect();
    let entries = items.len();
    let root = build_tree(repo, &items)?;

    ctx.conflicts.sort_by(|a, b| {
        a.path()
            .cmp(b.path())
            .then_with(|| format!("{a:?}").cmp(&format!("{b:?}")))
    });
    renames.sort();
    renames.dedup();
    Ok(MergeOutcome {
        clean: ctx.conflicts.is_empty(),
        root,
        entries,
        conflicts: ctx.conflicts,
        renames,
    })
}

const EMPTY_OID: ObjectId = ObjectId::from_bytes([0; 32]);

/// Both sides have the path with differing (mode, oid). Decide.
/// Returns None when a conflict was recorded.
fn resolve_both_changed(
    ctx: &mut Ctx,
    path: &str,
    base_e: (EntryMode, ObjectId),
    ours_e: (EntryMode, ObjectId),
    theirs_e: (EntryMode, ObjectId),
) -> Result<Option<(EntryMode, ObjectId)>> {
    let (bm, bo) = base_e;
    let (om, oo) = ours_e;
    let (tm, to) = theirs_e;

    // one side identical to base ⇒ take the other (covers add/add where one
    // side matches the empty virtual base only when content is empty — fine)
    if (om, oo) == (bm, bo) {
        return Ok(Some((tm, to)));
    }
    if (tm, to) == (bm, bo) {
        return Ok(Some((om, oo)));
    }
    // mode resolution
    let mode = if om == tm {
        om
    } else if om == bm {
        tm
    } else if tm == bm {
        om
    } else {
        ctx.conflicts.push(Conflict::Mode {
            path: path.to_string(),
            ours_mode: om,
            theirs_mode: tm,
        });
        return Ok(None);
    };
    // content resolution
    if oo == to {
        return Ok(Some((mode, oo)));
    }
    if om == EntryMode::Symlink || tm == EntryMode::Symlink {
        ctx.conflicts.push(Conflict::Opaque {
            path: path.to_string(),
            base_oid: some_if_real(bo),
            ours_oid: Some(oo),
            theirs_oid: Some(to),
            reason: "symlink targets diverged".into(),
        });
        return Ok(None);
    }
    let base_data = blob_or_empty(ctx.repo, bo)?;
    let ours_data = ctx.repo.objects.get(&oo)?.as_blob()?.to_vec();
    let theirs_data = ctx.repo.objects.get(&to)?.as_blob()?.to_vec();
    if is_binary(&base_data) || is_binary(&ours_data) || is_binary(&theirs_data) {
        ctx.conflicts.push(Conflict::Opaque {
            path: path.to_string(),
            base_oid: some_if_real(bo),
            ours_oid: Some(oo),
            theirs_oid: Some(to),
            reason: "binary content diverged".into(),
        });
        return Ok(None);
    }
    let bl = split_lines(&base_data);
    let ol = split_lines(&ours_data);
    let tl = split_lines(&theirs_data);
    let r = merge_lines(&bl, &ol, &tl, ctx.opts.max_edit_distance);
    if r.conflicted {
        let merged_bytes = render_merged(&r.chunks, ctx.opts.conflict_style_diff3);
        let merged_oid = ctx.repo.objects.put_blob(&merged_bytes)?;
        ctx.conflicts.push(Conflict::Content {
            path: path.to_string(),
            merged_oid,
            capped: r.capped,
        });
        return Ok(None);
    }
    // clean merge: reassemble exact bytes (Clean chunks carry original lines)
    let mut out = Vec::new();
    for c in &r.chunks {
        if let diff3::Chunk::Clean { lines } = c {
            for l in lines {
                out.extend_from_slice(l);
            }
        }
    }
    let oid = ctx.repo.objects.put_blob(&out)?;
    Ok(Some((mode, oid)))
}

fn some_if_real(oid: ObjectId) -> Option<ObjectId> {
    if oid == EMPTY_OID {
        None
    } else {
        Some(oid)
    }
}

fn blob_or_empty(repo: &Repo, oid: ObjectId) -> Result<Vec<u8>> {
    if oid == EMPTY_OID {
        return Ok(Vec::new());
    }
    Ok(repo.objects.get(&oid)?.as_blob()?.to_vec())
}

/// Detect exact renames: base paths missing from `side` whose (mode, oid)
/// reappears at a new path. Deterministic pairing (sorted iteration, first
/// unused match).
fn detect_exact_renames(base: &Flat, side: &Flat) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let added: Vec<(&String, &(EntryMode, ObjectId))> = side
        .iter()
        .filter(|(p, _)| !base.contains_key(*p))
        .collect();
    let mut used = vec![false; added.len()];
    for (bp, be) in base.iter().filter(|(p, _)| !side.contains_key(*p)) {
        for (i, (_, ae)) in added.iter().enumerate() {
            if !used[i] && *ae == be {
                used[i] = true;
                out.insert(bp.clone(), added[i].0.clone());
                break;
            }
        }
    }
    out
}

/// Relocate renamed paths so the per-path 3-way sees both sides under the
/// same key. `base` gains a virtual entry at the new path (the pre-rename
/// content), making rename+edit resolve cleanly.
fn apply_renames(
    ctx: &mut Ctx,
    base: &mut Flat,
    ours: &mut Flat,
    theirs: &mut Flat,
    ro: &BTreeMap<String, String>,
    rt: &BTreeMap<String, String>,
    renames: &mut Vec<(String, String)>,
) -> Result<()> {
    let tables = &mut Tables {
        base,
        ours,
        theirs,
        renames,
    };
    for (bp, op) in ro {
        if let Some(tp) = rt.get(bp) {
            if tp != op {
                ctx.conflicts.push(Conflict::RenameRename {
                    base_path: bp.clone(),
                    ours_path: op.clone(),
                    theirs_path: tp.clone(),
                });
                continue; // both new paths survive as side-specific adds
            }
        }
        relocate_one(ctx, tables, bp, op, "ours")?;
    }
    for (bp, tp) in rt {
        if !ro.contains_key(bp) {
            relocate_one(ctx, tables, bp, tp, "theirs")?;
        }
    }
    Ok(())
}

/// Mutable view of the three flattened tables during relocation.
struct Tables<'a> {
    base: &'a mut Flat,
    ours: &'a mut Flat,
    theirs: &'a mut Flat,
    renames: &'a mut Vec<(String, String)>,
}

fn relocate_one(
    ctx: &mut Ctx,
    tables: &mut Tables,
    bp: &str,
    np: &str,
    renamed_by: &str,
) -> Result<()> {
    let Tables {
        base,
        ours,
        theirs,
        renames,
    } = tables;
    let Some(be) = base.get(bp).copied() else {
        return Ok(());
    };
    let kept = ours
        .get(np)
        .or_else(|| theirs.get(np))
        .map(|(_, o)| *o)
        .unwrap_or(be.1);
    // The non-renaming side: move whatever it has at bp (possibly edited) to
    // np. If it has neither bp nor np, it deleted the file while the other
    // side renamed it ⇒ rename/delete conflict.
    if renamed_by == "ours" {
        relocate_side(theirs, bp, np, kept, renamed_by, ctx)?;
    } else {
        relocate_side(ours, bp, np, kept, renamed_by, ctx)?;
    }
    if ctx.conflicts.last().map(|c| c.path()) == Some(np) {
        return Ok(()); // rename/delete recorded; skip virtual base
    }
    // virtual base at the new path
    base.insert(np.to_string(), be);
    renames.push((bp.to_string(), np.to_string()));
    Ok(())
}

fn relocate_side(
    side: &mut Flat,
    bp: &str,
    np: &str,
    kept: ObjectId,
    renamed_by: &str,
    ctx: &mut Ctx,
) -> Result<()> {
    if side.contains_key(np) {
        side.remove(bp);
        return Ok(());
    }
    match side.remove(bp) {
        Some(e) => {
            side.insert(np.to_string(), e);
        }
        None => {
            ctx.conflicts.push(Conflict::RenameDelete {
                base_path: bp.to_string(),
                new_path: np.to_string(),
                renamed_by: renamed_by.to_string(),
                kept_oid: kept,
            });
        }
    }
    Ok(())
}

/// Convenience: merge two snapshots with an automatically located base.
pub fn merge_snapshots(
    repo: &Repo,
    ours_snap: ObjectId,
    theirs_snap: ObjectId,
    opts: &MergeOpts,
) -> Result<MergeOutcome> {
    let base = base::merge_base(repo, ours_snap, theirs_snap)?;
    let empty = repo.objects.put(&Object::Tree(Tree::empty()))?;
    let base_root = match base {
        Some(b) => repo.objects.get(&b)?.as_snapshot()?.root,
        None => empty,
    };
    let our_root = repo.objects.get(&ours_snap)?.as_snapshot()?.root;
    let their_root = repo.objects.get(&theirs_snap)?.as_snapshot()?.root;
    merge_trees(repo, base_root, our_root, their_root, opts)
}
