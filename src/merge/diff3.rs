//! Three-way line merge (diff3 style) built on the Myers opcodes.
//!
//! Algorithm: lines of `base` that both sides kept (Equal-mapped in both
//! base→ours and base→theirs diffs) are *anchors*. Between consecutive
//! anchors the three sides form segments; a segment triple resolves as:
//! * ours == theirs            → clean (either)
//! * ours == base              → clean (theirs changed)
//! * theirs == base            → clean (ours changed)
//! * otherwise                 → conflict {ours, theirs, base}
//!
//! Deterministic: identical inputs ⇒ identical chunks. When either diff
//! exceeds the edit-distance cap, the whole file becomes one coarse conflict
//! (`capped = true`) — never a silently wrong merge.

use serde::Serialize;

use crate::diff::myers::{diff_lines, Opcode, Tag};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Chunk<'a> {
    Clean {
        lines: Vec<&'a [u8]>,
    },
    Conflict {
        ours: Vec<&'a [u8]>,
        theirs: Vec<&'a [u8]>,
        base: Vec<&'a [u8]>,
    },
}

#[derive(Clone, Debug)]
pub struct Diff3Result<'a> {
    pub chunks: Vec<Chunk<'a>>,
    pub conflicted: bool,
    /// True when the edit-distance cap forced a whole-file conflict.
    pub capped: bool,
}

/// Map each base-line index to its counterpart index in `side` when the
/// opcodes mark it Equal (1:1 inside Equal runs), else None.
fn equal_map(base_len: usize, ops: &[Opcode]) -> Vec<Option<usize>> {
    let mut m = vec![None; base_len];
    for op in ops {
        if op.tag == Tag::Equal {
            for i in 0..(op.a2 - op.a1) {
                m[op.a1 + i] = Some(op.b1 + i);
            }
        }
    }
    m
}

pub fn merge_lines<'a>(
    base: &'a [&'a [u8]],
    ours: &'a [&'a [u8]],
    theirs: &'a [&'a [u8]],
    cap: usize,
) -> Diff3Result<'a> {
    let bo = diff_lines(base, ours, cap);
    let bt = diff_lines(base, theirs, cap);
    let (Some(bo), Some(bt)) = (bo, bt) else {
        return Diff3Result {
            chunks: vec![Chunk::Conflict {
                ours: ours.to_vec(),
                theirs: theirs.to_vec(),
                base: base.to_vec(),
            }],
            conflicted: true,
            capped: true,
        };
    };
    let mo = equal_map(base.len(), &bo);
    let mt = equal_map(base.len(), &bt);

    let anchors: Vec<usize> = (0..base.len())
        .filter(|i| mo[*i].is_some() && mt[*i].is_some())
        .collect();

    let mut chunks: Vec<Chunk<'a>> = Vec::new();
    let push = |chunks: &mut Vec<Chunk<'a>>, c: Chunk<'a>| {
        // merge adjacent Clean chunks (cosmetic canonicalization)
        if let (Some(Chunk::Clean { lines }), Chunk::Clean { lines: l2 }) = (chunks.last_mut(), &c)
        {
            lines.extend_from_slice(l2);
        } else {
            chunks.push(c);
        }
    };

    let mut prev_b = 0usize; // base cursor (exclusive end of last segment)
    let mut prev_o = 0usize; // ours cursor
    let mut prev_t = 0usize; // theirs cursor
    for &ai in anchors.iter().chain(std::iter::once(&base.len())) {
        let is_sentinel = ai == base.len() && anchors.last() != Some(&ai);
        let (b_end, o_end, t_end) = if is_sentinel {
            (base.len(), ours.len(), theirs.len())
        } else {
            (ai, mo[ai].unwrap(), mt[ai].unwrap())
        };
        // segment strictly between cursors and this anchor
        let base_seg = &base[prev_b..b_end];
        let ours_seg = &ours[prev_o..o_end];
        let theirs_seg = &theirs[prev_t..t_end];
        push(&mut chunks, resolve_segment(base_seg, ours_seg, theirs_seg));
        if !is_sentinel {
            push(
                &mut chunks,
                Chunk::Clean {
                    lines: vec![base[ai]],
                },
            );
            prev_b = ai + 1;
            prev_o = o_end + 1;
            prev_t = t_end + 1;
        }
    }
    let conflicted = chunks.iter().any(|c| matches!(c, Chunk::Conflict { .. }));
    Diff3Result {
        chunks,
        conflicted,
        capped: false,
    }
}

fn resolve_segment<'a>(
    base_seg: &[&'a [u8]],
    ours_seg: &[&'a [u8]],
    theirs_seg: &[&'a [u8]],
) -> Chunk<'a> {
    if ours_seg == theirs_seg {
        Chunk::Clean {
            lines: ours_seg.to_vec(),
        }
    } else if ours_seg == base_seg {
        Chunk::Clean {
            lines: theirs_seg.to_vec(),
        }
    } else if theirs_seg == base_seg {
        Chunk::Clean {
            lines: ours_seg.to_vec(),
        }
    } else {
        Chunk::Conflict {
            ours: ours_seg.to_vec(),
            theirs: theirs_seg.to_vec(),
            base: base_seg.to_vec(),
        }
    }
}

pub const MARKER_OURS: &[u8] = b"<<<<<<< ours";
pub const MARKER_SEP: &[u8] = b"=======";
pub const MARKER_BASE: &[u8] = b"||||||| base";
pub const MARKER_THEIRS: &[u8] = b">>>>>>> theirs";

/// Render chunks into merged file bytes. Conflicts get diff3-style markers
/// (ours | base | theirs). A side whose last line lacks '\n' gets one added
/// before its closing marker so markers are always on their own line
/// (documented deviation from byte-exact git marker placement).
pub fn render_merged(chunks: &[Chunk<'_>], include_base: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let push_lines = |out: &mut Vec<u8>, lines: &[&[u8]]| {
        for l in lines {
            out.extend_from_slice(l);
            if !l.ends_with(b"\n") {
                out.push(b'\n');
            }
        }
    };
    for c in chunks {
        match c {
            Chunk::Clean { lines } => {
                for l in lines {
                    out.extend_from_slice(l);
                }
            }
            Chunk::Conflict { ours, theirs, base } => {
                out.extend_from_slice(MARKER_OURS);
                out.push(b'\n');
                push_lines(&mut out, ours);
                if include_base {
                    out.extend_from_slice(MARKER_BASE);
                    out.push(b'\n');
                    push_lines(&mut out, base);
                }
                out.extend_from_slice(MARKER_SEP);
                out.push(b'\n');
                push_lines(&mut out, theirs);
                out.extend_from_slice(MARKER_THEIRS);
                out.push(b'\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::myers::split_lines;

    fn m(base: &'static str, ours: &'static str, theirs: &'static str) -> Diff3Result<'static> {
        // leak-test helper: fine for unit tests
        let b: &'static [&'static [u8]] = Box::leak(
            split_lines(base.as_bytes())
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let o: &'static [&'static [u8]] = Box::leak(
            split_lines(ours.as_bytes())
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        let t: &'static [&'static [u8]] = Box::leak(
            split_lines(theirs.as_bytes())
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        merge_lines(b, o, t, 1024)
    }

    fn rendered(r: &Diff3Result) -> String {
        String::from_utf8_lossy(&render_merged(&r.chunks, false)).to_string()
    }

    #[test]
    fn disjoint_changes_merge_clean() {
        let r = m("a\nb\nc\nd\ne\n", "A\nb\nc\nd\ne\n", "a\nb\nc\nd\nE\n");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "A\nb\nc\nd\nE\n");
    }

    #[test]
    fn overlapping_changes_conflict_with_markers() {
        let r = m("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        assert!(r.conflicted);
        let out = rendered(&r);
        assert!(out.contains("<<<<<<< ours\nOURS\n=======\nTHEIRS\n>>>>>>> theirs\n"));
        assert!(out.starts_with("a\n") && out.ends_with("c\n"));
    }

    #[test]
    fn identical_changes_merge_clean() {
        let r = m("a\nb\n", "a\nSAME\n", "a\nSAME\n");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "a\nSAME\n");
    }

    #[test]
    fn one_side_unchanged_takes_other() {
        let r = m("a\nb\n", "a\nb\n", "a\nB2\n");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "a\nB2\n");
        let r = m("a\nb\n", "a\nB1\n", "a\nb\n");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "a\nB1\n");
    }

    #[test]
    fn insertions_both_sides_same_place_conflict_unless_equal() {
        let r = m("a\nb\n", "a\nX\nb\n", "a\nY\nb\n");
        assert!(r.conflicted);
        let r = m("a\nb\n", "a\nX\nb\n", "a\nX\nb\n");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "a\nX\nb\n");
    }

    #[test]
    fn deletion_vs_modification_conflicts() {
        // ours deleted line b, theirs modified it
        let r = m("a\nb\nc\n", "a\nc\n", "a\nB2\nc\n");
        assert!(r.conflicted, "{:?}", rendered(&r));
    }

    #[test]
    fn empty_sides() {
        let r = m("", "new\n", "");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "new\n");
        let r = m("", "", "new\n");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "new\n");
        let r = m("x\n", "", "");
        assert!(!r.conflicted);
        assert_eq!(rendered(&r), "");
    }

    #[test]
    fn cap_fallback_is_whole_file_conflict() {
        let b = split_lines(b"b\n");
        let o: Vec<&[u8]> = (0..20).flat_map(|_| vec![b"o\n" as &[u8]]).collect();
        let t: Vec<&[u8]> = (0..20).flat_map(|_| vec![b"t\n" as &[u8]]).collect();
        let r = merge_lines(&b, &o, &t, 5);
        assert!(r.capped && r.conflicted);
        assert_eq!(r.chunks.len(), 1);
    }

    #[test]
    fn no_newline_sides_get_marker_newline() {
        let r = m("a", "ours", "theirs");
        assert!(r.conflicted);
        let out = rendered(&r);
        assert!(out.contains("ours\n=======\ntheirs\n>>>>>>>"));
    }
}
