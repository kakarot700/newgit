//! Diff engine: tree-level differences (with rename detection) and
//! file-level content differences (Myers line diff, binary handling).
//!
//! Determinism contract: identical inputs ⇒ byte-identical output. Rename
//! pairing iterates candidates in sorted order and tie-breaks on
//! (score DESC, old path, new path).

pub mod myers;
pub mod render;

use std::collections::BTreeMap;

use myers::{diff_lines, is_binary, split_lines, Opcode, Tag};
use serde::Serialize;

use crate::error::{Error, Result};
use crate::object::types::{EntryMode, Object};
use crate::object::ObjectId;
use crate::ops::tree::flatten_tree;
use crate::repo::Repo;

#[derive(Clone, Debug)]
pub struct DiffOpts {
    pub detect_renames: bool,
    /// When deleted×added pairs exceed this, only exact-content renames are
    /// detected (similarity scoring loads blobs per candidate).
    pub rename_pair_cap: usize,
    /// Context lines in unified output.
    pub context: usize,
    /// Myers edit-distance cap per file.
    pub max_edit_distance: usize,
    /// Skip similarity scoring for blobs larger than this.
    pub max_similarity_blob: u64,
}

impl Default for DiffOpts {
    fn default() -> Self {
        DiffOpts {
            detect_renames: true,
            rename_pair_cap: 1000,
            context: 3,
            max_edit_distance: 1024,
            max_similarity_blob: 2 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDiffKind {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileDiff {
    pub kind: FileDiffKind,
    pub path: String,
    /// For renames: the old path.
    pub old_path: Option<String>,
    pub old_mode: Option<EntryMode>,
    pub new_mode: Option<EntryMode>,
    pub old_oid: Option<ObjectId>,
    pub new_oid: Option<ObjectId>,
    pub binary: bool,
    /// Rename similarity 0–100 (100 = identical content).
    pub similarity: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TreeDiff {
    pub files: Vec<FileDiff>,
    /// "similarity" | "exact" | "off" — which rename stage ran.
    pub rename_detection: String,
}

impl TreeDiff {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

type Entry3 = (String, EntryMode, ObjectId);

pub fn diff_trees(
    repo: &Repo,
    old_root: ObjectId,
    new_root: ObjectId,
    opts: &DiffOpts,
) -> Result<TreeDiff> {
    if old_root == new_root {
        return Ok(TreeDiff {
            files: Vec::new(),
            rename_detection: if opts.detect_renames {
                "exact".into()
            } else {
                "off".into()
            },
        });
    }
    let a: BTreeMap<String, (EntryMode, ObjectId)> = flatten_tree(repo, old_root)?
        .into_iter()
        .map(|(p, m, o)| (p, (m, o)))
        .collect();
    let b: BTreeMap<String, (EntryMode, ObjectId)> = flatten_tree(repo, new_root)?
        .into_iter()
        .map(|(p, m, o)| (p, (m, o)))
        .collect();

    let mut files: Vec<FileDiff> = Vec::new();
    let mut dels: Vec<Entry3> = Vec::new();
    let mut adds: Vec<Entry3> = Vec::new();

    let mut all_paths: Vec<&String> = a.keys().collect();
    for k in b.keys() {
        if !a.contains_key(k) {
            all_paths.push(k);
        }
    }
    all_paths.sort();
    for p in all_paths {
        match (a.get(p), b.get(p)) {
            (Some((am, ao)), Some((bm, bo))) => {
                if am == bm && ao == bo {
                    continue; // unchanged
                }
                files.push(FileDiff {
                    kind: FileDiffKind::Modified,
                    path: p.clone(),
                    old_path: None,
                    old_mode: Some(*am),
                    new_mode: Some(*bm),
                    old_oid: Some(*ao),
                    new_oid: Some(*bo),
                    binary: is_blob_binary(repo, ao)? || is_blob_binary(repo, bo)?,
                    similarity: None,
                });
            }
            (Some((am, ao)), None) => dels.push((p.clone(), *am, *ao)),
            (None, Some((bm, bo))) => adds.push((p.clone(), *bm, *bo)),
            (None, None) => unreachable!(),
        }
    }

    let mut renames: Vec<FileDiff> = Vec::new();
    let mut matched_dels: Vec<bool> = vec![false; dels.len()];
    let mut used_adds: Vec<bool> = vec![false; adds.len()];
    let mut stage = if opts.detect_renames { "exact" } else { "off" };

    if opts.detect_renames && !dels.is_empty() && !adds.is_empty() {
        // Stage 1: exact content renames (same mode + same oid).
        let mut add_idx: BTreeMap<(EntryMode, ObjectId), Vec<usize>> = BTreeMap::new();
        for (i, (_, m, o)) in adds.iter().enumerate() {
            add_idx.entry((*m, *o)).or_default().push(i);
        }
        for (di, (_, dm, do_)) in dels.iter().enumerate() {
            if let Some(cands) = add_idx.get(&(*dm, *do_)) {
                if let Some(&ai) = cands.iter().find(|&&i| !used_adds[i]) {
                    used_adds[ai] = true;
                    matched_dels[di] = true;
                    renames.push(FileDiff {
                        kind: FileDiffKind::Renamed,
                        path: adds[ai].0.clone(),
                        old_path: Some(dels[di].0.clone()),
                        old_mode: Some(*dm),
                        new_mode: Some(*dm),
                        old_oid: Some(*do_),
                        new_oid: Some(*do_),
                        binary: false,
                        similarity: Some(100),
                    });
                }
            }
        }
        // Stage 2: similarity renames (bounded work, deterministic greedy).
        let rem_dels: Vec<usize> = (0..dels.len()).filter(|i| !matched_dels[*i]).collect();
        let rem_adds: Vec<usize> = (0..adds.len()).filter(|i| !used_adds[*i]).collect();
        if !rem_dels.is_empty()
            && !rem_adds.is_empty()
            && rem_dels.len() * rem_adds.len() <= opts.rename_pair_cap
        {
            stage = "similarity";
            let del_lines: Vec<Option<Vec<String>>> = rem_dels
                .iter()
                .map(|&di| load_text_lines(repo, &dels[di], opts))
                .collect::<Result<Vec<_>>>()?;
            let add_lines: Vec<Option<Vec<String>>> = rem_adds
                .iter()
                .map(|&ai| load_text_lines(repo, &adds[ai], opts))
                .collect::<Result<Vec<_>>>()?;
            let mut cands: Vec<(u8, usize, usize)> = Vec::new(); // score, dx, ax
            for (dx, dl) in del_lines.iter().enumerate() {
                let Some(dl) = dl else { continue };
                for (ax, al) in add_lines.iter().enumerate() {
                    let Some(al) = al else { continue };
                    if adds[rem_adds[ax]].1 != dels[rem_dels[dx]].1 {
                        continue; // mode must match for a rename
                    }
                    let score = similarity(dl, al);
                    if score >= 50 {
                        cands.push((score, dx, ax));
                    }
                }
            }
            // deterministic greedy: best score, then old path, then new path
            cands.sort_by(|x, y| {
                y.0.cmp(&x.0)
                    .then_with(|| dels[rem_dels[x.1]].0.cmp(&dels[rem_dels[y.1]].0))
                    .then_with(|| adds[rem_adds[x.2]].0.cmp(&adds[rem_adds[y.2]].0))
            });
            let mut dused = vec![false; rem_dels.len()];
            let mut aused = vec![false; rem_adds.len()];
            for (score, dx, ax) in cands {
                if dused[dx] || aused[ax] {
                    continue;
                }
                dused[dx] = true;
                aused[ax] = true;
                let di = rem_dels[dx];
                let ai = rem_adds[ax];
                matched_dels[di] = true;
                used_adds[ai] = true;
                renames.push(FileDiff {
                    kind: FileDiffKind::Renamed,
                    path: adds[ai].0.clone(),
                    old_path: Some(dels[di].0.clone()),
                    old_mode: Some(dels[di].1),
                    new_mode: Some(adds[ai].1),
                    old_oid: Some(dels[di].2),
                    new_oid: Some(adds[ai].2),
                    binary: false,
                    similarity: Some(score),
                });
            }
        }
    }

    for (di, (p, m, o)) in dels.iter().enumerate() {
        if matched_dels[di] {
            continue;
        }
        files.push(FileDiff {
            kind: FileDiffKind::Deleted,
            path: p.clone(),
            old_path: None,
            old_mode: Some(*m),
            new_mode: None,
            old_oid: Some(*o),
            new_oid: None,
            binary: is_blob_binary(repo, o)?,
            similarity: None,
        });
    }
    for (ai, (p, m, o)) in adds.iter().enumerate() {
        if used_adds[ai] {
            continue;
        }
        files.push(FileDiff {
            kind: FileDiffKind::Added,
            path: p.clone(),
            old_path: None,
            old_mode: None,
            new_mode: Some(*m),
            old_oid: None,
            new_oid: Some(*o),
            binary: is_blob_binary(repo, o)?,
            similarity: None,
        });
    }
    files.extend(renames);
    // deterministic order: by (path, old_path)
    files.sort_by(|x, y| {
        x.path
            .cmp(&y.path)
            .then_with(|| x.old_path.cmp(&y.old_path))
    });
    Ok(TreeDiff {
        files,
        rename_detection: stage.to_string(),
    })
}

fn is_blob_binary(repo: &Repo, oid: &ObjectId) -> Result<bool> {
    let obj = repo.objects.get(oid)?;
    let data = obj.as_blob()?;
    Ok(is_binary(data))
}

/// Owned lines for similarity scoring; None for symlinks, binaries, and
/// oversized blobs (never scored for renames).
fn load_text_lines(repo: &Repo, entry: &Entry3, opts: &DiffOpts) -> Result<Option<Vec<String>>> {
    if entry.1 == EntryMode::Symlink {
        return Ok(None);
    }
    let obj = repo.objects.get(&entry.2)?;
    let data = obj.as_blob()?.to_vec();
    if data.len() as u64 > opts.max_similarity_blob || is_binary(&data) {
        return Ok(None);
    }
    Ok(Some(
        split_lines(&data)
            .into_iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect(),
    ))
}

/// Cheap deterministic similarity: common prefix + common suffix lines over
/// max(len). (Full Myers per candidate pair is too expensive for the search;
/// the chosen rename's actual diff is computed on demand by the renderer.)
fn similarity(a: &[String], b: &[String]) -> u8 {
    let n = a.len();
    let m = b.len();
    if n == 0 && m == 0 {
        return 100;
    }
    let mut p = 0;
    while p < n && p < m && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && s < m - p && a[n - 1 - s] == b[m - 1 - s] {
        s += 1;
    }
    let score = (p + s) * 100 / n.max(m);
    score.min(100) as u8
}

/// Content diff of two blobs (text): opcodes + owned lines for rendering.
#[derive(Debug)]
pub struct ContentDiff {
    pub binary: bool,
    /// None when edit distance exceeded the cap (coarse fallback in ops2).
    pub ops: Option<Vec<Opcode>>,
    pub ops2: Vec<Opcode>,
    pub a_lines: Vec<String>,
    pub b_lines: Vec<String>,
}

pub fn diff_blob_content(
    repo: &Repo,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    opts: &DiffOpts,
) -> Result<ContentDiff> {
    let a_data = load_blob(repo, old)?;
    let b_data = load_blob(repo, new)?;
    if is_binary(&a_data) || is_binary(&b_data) {
        return Ok(ContentDiff {
            binary: true,
            ops: None,
            ops2: Vec::new(),
            a_lines: Vec::new(),
            b_lines: Vec::new(),
        });
    }
    let a_lines = split_lines(&a_data);
    let b_lines = split_lines(&b_data);
    let ops = diff_lines(&a_lines, &b_lines, opts.max_edit_distance);
    let ops2 = ops.clone().unwrap_or_else(|| {
        // coarse fallback: replace whole content — still exact, not minimal
        vec![Opcode {
            tag: Tag::Replace,
            a1: 0,
            a2: a_lines.len(),
            b1: 0,
            b2: b_lines.len(),
        }]
    });
    Ok(ContentDiff {
        binary: false,
        ops,
        ops2,
        a_lines: a_lines
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect(),
        b_lines: b_lines
            .iter()
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .collect(),
    })
}

fn load_blob(repo: &Repo, oid: Option<ObjectId>) -> Result<Vec<u8>> {
    match oid {
        None => Ok(Vec::new()),
        Some(o) => {
            let obj = repo.objects.get(&o)?;
            Ok(obj.as_blob()?.to_vec())
        }
    }
}

/// Resolve any user spec (ref, oid, ws:name, snapshot-or-tree) to a tree oid.
pub fn resolve_tree(repo: &Repo, spec: &str) -> Result<ObjectId> {
    let oid = crate::repo::workspace::resolve_base(repo, Some(spec))?
        .ok_or_else(|| Error::Invalid(format!("cannot resolve {spec:?} to a snapshot")))?;
    snapshot_or_tree_to_root(repo, oid)
}

pub fn snapshot_or_tree_to_root(repo: &Repo, oid: ObjectId) -> Result<ObjectId> {
    let obj = repo.objects.get(&oid)?;
    Ok(match obj {
        Object::Snapshot(s) => s.root,
        Object::Tree(_) => oid,
        other => {
            return Err(Error::Invalid(format!(
                "expected snapshot or tree, got {}",
                other.type_tag().name()
            )))
        }
    })
}
