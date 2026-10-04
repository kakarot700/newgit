//! Myers O(ND) line diff with bounded edit distance.
//!
//! Deterministic and exact for edit distance ≤ `max_d` (default 1024, see
//! DiffOpts). Beyond the cap, `diff_lines` returns None and callers fall
//! back to a coarse whole-file replace — output stays correct and
//! reconstructible, just not minimal (KNOWN_LIMITATIONS #13).
//!
//! Memory: the greedy algorithm stores one V array per round; with prefix/
//! suffix trimming and the cap, worst case ≈ max_d² · 8 bytes (≤ 8 MiB).

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tag {
    Equal,
    Delete,
    Insert,
    Replace,
}

/// A contiguous block of one tag over ranges [a1,a2) × [b1,b2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Opcode {
    pub tag: Tag,
    pub a1: usize,
    pub a2: usize,
    pub b1: usize,
    pub b2: usize,
}

/// Split blob bytes into lines, keeping '\n' terminators (a trailing partial
/// line stays its own element — "no newline at EOF" is representable).
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in data.iter().enumerate() {
        if b == b'\n' {
            out.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        out.push(&data[start..]);
    }
    out
}

/// Git-style binary detection: NUL in the first 8000 bytes.
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8000)].contains(&0)
}

/// Compute opcodes transforming `a` into `b`. Returns None when the minimal
/// edit distance exceeds `max_d` (caller decides the fallback).
pub fn diff_lines(a: &[&[u8]], b: &[&[u8]], max_d: usize) -> Option<Vec<Opcode>> {
    // common prefix / suffix trimming
    let n = a.len();
    let m = b.len();
    let mut p = 0;
    while p < n && p < m && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && s < m - p && a[n - 1 - s] == b[m - 1 - s] {
        s += 1;
    }
    let am = &a[p..n - s];
    let bm = &b[p..m - s];

    let mut ops = Vec::new();
    if p > 0 {
        ops.push(Opcode {
            tag: Tag::Equal,
            a1: 0,
            a2: p,
            b1: 0,
            b2: p,
        });
    }
    if !am.is_empty() || !bm.is_empty() {
        let edits = myers_core(am, bm, max_d)?;
        ops.extend(group_edits(&edits, p, p));
    }
    if s > 0 {
        ops.push(Opcode {
            tag: Tag::Equal,
            a1: n - s,
            a2: n,
            b1: m - s,
            b2: m,
        });
    }
    Some(merge_replace(ops))
}

#[derive(Clone, Copy)]
enum Edit {
    Eq,
    Del,
    Ins,
}

/// Greedy Myers with full trace, capped at max_d rounds.
fn myers_core(a: &[&[u8]], b: &[&[u8]], max_d: usize) -> Option<Vec<Edit>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = (n + m) as usize;
    if max == 0 {
        return Some(Vec::new());
    }
    let cap = max_d.min(max);
    let off = max as isize;
    let mut v = vec![0isize; 2 * max + 1];
    let mut trace: Vec<Vec<isize>> = Vec::with_capacity(cap + 1);

    let idx = |k: isize| (k + off) as usize;
    let mut found_d = None;
    'outer: for d in 0..=cap as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                found_d = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let d_found = found_d?;

    // backtrack
    let mut edits = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..=d_found).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[idx(prev_k)];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            edits.push(Edit::Eq);
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                edits.push(Edit::Ins);
            } else {
                edits.push(Edit::Del);
            }
            x = prev_x;
            y = prev_y;
        }
    }
    edits.reverse();
    Some(edits)
}

/// Group single edits into opcodes with absolute coordinates.
fn group_edits(edits: &[Edit], a_off: usize, b_off: usize) -> Vec<Opcode> {
    let mut ops: Vec<Opcode> = Vec::new();
    let (mut ai, mut bi) = (a_off, b_off);
    for e in edits {
        let (tag, da, db) = match e {
            Edit::Eq => (Tag::Equal, 1, 1),
            Edit::Del => (Tag::Delete, 1, 0),
            Edit::Ins => (Tag::Insert, 0, 1),
        };
        if let Some(last) = ops.last_mut() {
            if last.tag == tag {
                last.a2 += da;
                last.b2 += db;
                ai += da;
                bi += db;
                continue;
            }
        }
        ops.push(Opcode {
            tag,
            a1: ai,
            a2: ai + da,
            b1: bi,
            b2: bi + db,
        });
        ai += da;
        bi += db;
    }
    ops
}

/// Merge adjacent Delete+Insert (either order) into Replace blocks so
/// renderers see canonical change hunks.
fn merge_replace(ops: Vec<Opcode>) -> Vec<Opcode> {
    let mut out: Vec<Opcode> = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if i + 1 < ops.len() {
            let (x, y) = (&ops[i], &ops[i + 1]);
            let is_del_ins = x.tag == Tag::Delete && y.tag == Tag::Insert && x.a2 == y.a1;
            let is_ins_del = x.tag == Tag::Insert && y.tag == Tag::Delete && x.b2 == y.b1;
            if is_del_ins || is_ins_del {
                out.push(Opcode {
                    tag: Tag::Replace,
                    a1: x.a1.min(y.a1),
                    a2: x.a2.max(y.a2),
                    b1: x.b1.min(y.b1),
                    b2: x.b2.max(y.b2),
                });
                i += 2;
                continue;
            }
        }
        out.push(ops[i]);
        i += 1;
    }
    out
}

/// Full reconstruction (needs both sides): property tests use this to verify
/// opcodes describe an exact transformation.
pub fn reconstruct<'x>(a: &[&'x [u8]], b: &[&'x [u8]], ops: &[Opcode]) -> Vec<&'x [u8]> {
    let mut out = Vec::new();
    for op in ops {
        match op.tag {
            Tag::Equal => {
                debug_assert_eq!(&a[op.a1..op.a2], &b[op.b1..op.b2]);
                out.extend_from_slice(&a[op.a1..op.a2]);
            }
            Tag::Delete => {}
            Tag::Insert | Tag::Replace => out.extend_from_slice(&b[op.b1..op.b2]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(a: &str, b: &str) -> Vec<Opcode> {
        let al: Vec<&[u8]> = split_lines(a.as_bytes());
        let bl: Vec<&[u8]> = split_lines(b.as_bytes());
        let ops = diff_lines(&al, &bl, 1024).unwrap();
        // reconstruction invariant on every unit case
        assert_eq!(reconstruct(&al, &bl, &ops), bl);
        ops
    }

    #[test]
    fn identical() {
        let ops = d("a\nb\n", "a\nb\n");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].tag, Tag::Equal);
    }

    #[test]
    fn classic_cases() {
        // pure insert
        let ops = d("a\nc\n", "a\nb\nc\n");
        assert!(ops.iter().any(|o| o.tag == Tag::Insert));
        // pure delete
        let ops = d("a\nb\nc\n", "a\nc\n");
        assert!(ops.iter().any(|o| o.tag == Tag::Delete));
        // replace
        let ops = d("a\nb\nc\n", "a\nX\nc\n");
        assert!(ops.iter().any(|o| o.tag == Tag::Replace));
        // from empty
        let ops = d("", "x\ny\n");
        let expect: Vec<&[u8]> = split_lines(b"x\ny");
        assert_eq!(reconstruct(&[], &expect, &ops), expect);
        // to empty
        let ops = d("x\n", "");
        assert!(ops.iter().all(|o| o.tag == Tag::Delete));
    }

    #[test]
    fn minimal_edit_distance() {
        // ABCABBA → CBABAC (Myers' paper example, edit distance 5)
        let a: Vec<&[u8]> = b"A\nB\nC\nA\nB\nB\nA\n"
            .split(|&c| c == b'\n')
            .filter(|s| !s.is_empty())
            .map(|s| s as &[u8])
            .collect();
        let b: Vec<&[u8]> = b"C\nB\nA\nB\nA\nC\n"
            .split(|&c| c == b'\n')
            .filter(|s| !s.is_empty())
            .map(|s| s as &[u8])
            .collect();
        let ops = diff_lines(&a, &b, 1024).unwrap();
        let dels: usize = ops
            .iter()
            .map(|o| match o.tag {
                Tag::Delete | Tag::Replace => o.a2 - o.a1,
                _ => 0,
            })
            .sum();
        let ins: usize = ops
            .iter()
            .map(|o| match o.tag {
                Tag::Insert | Tag::Replace => o.b2 - o.b1,
                _ => 0,
            })
            .sum();
        assert_eq!(dels + ins, 5, "must find the minimal script");
        assert_eq!(reconstruct(&a, &b, &ops), b);
    }

    #[test]
    fn cap_returns_none_beyond() {
        let a: Vec<&[u8]> = split_lines(b"a\na\na\na\na\na\na\na\n");
        let b: Vec<&[u8]> = split_lines(b"b\nb\nb\nb\nb\nb\nb\nb\n");
        // 8 dels + 8 ins = distance 16 > cap 4
        assert!(diff_lines(&a, &b, 4).is_none());
        assert!(diff_lines(&a, &b, 16).is_some());
    }

    #[test]
    fn no_newline_at_eof() {
        let ops = d("a\nb", "a\nb\n");
        assert!(ops.iter().any(|o| o.tag == Tag::Replace));
    }

    #[test]
    fn binary_detection() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text\n"));
        let mut big = vec![b'x'; 9000];
        big[8500] = 0; // beyond window → text
        assert!(!is_binary(&big));
    }
}
