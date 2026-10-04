//! Diff rendering: unified text (human, git-shaped) and structured hunks
//! (machine, JSON). Byte-identical output for identical inputs.

use serde::Serialize;

use super::myers::{Opcode, Tag};
use super::{ContentDiff, FileDiff, FileDiffKind};

#[derive(Clone, Debug, Serialize)]
pub struct Hunk {
    /// 1-based start line in the old file (0 when the hunk has no old lines).
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    /// Lines with ' ', '-', '+' prefixes; "\ No newline at end of file"
    /// markers included where needed.
    pub lines: Vec<String>,
}

fn mode_name(m: crate::object::types::EntryMode) -> &'static str {
    match m {
        crate::object::types::EntryMode::File => "file",
        crate::object::types::EntryMode::Executable => "executable",
        crate::object::types::EntryMode::Symlink => "symlink",
        crate::object::types::EntryMode::Tree => "tree",
    }
}

/// File header block (git-shaped but with explicit NewGit semantics).
pub fn file_header(fd: &FileDiff) -> String {
    let mut out = String::new();
    let a = fd.old_path.as_deref().unwrap_or(&fd.path);
    let b = &fd.path;
    out.push_str(&format!("diff --newgit a/{a} b/{b}\n"));
    match fd.kind {
        FileDiffKind::Added => {
            out.push_str("new file\n");
            if let Some(m) = fd.new_mode {
                out.push_str(&format!("mode {}\n", mode_name(m)));
            }
        }
        FileDiffKind::Deleted => {
            out.push_str("deleted file\n");
            if let Some(m) = fd.old_mode {
                out.push_str(&format!("mode {}\n", mode_name(m)));
            }
        }
        FileDiffKind::Renamed => {
            if let Some(s) = fd.similarity {
                out.push_str(&format!("similarity index {s}%\n"));
            }
            out.push_str(&format!("rename from {a}\nrename to {b}\n"));
            if fd.old_oid != fd.new_oid {
                out.push_str("content changed after rename\n");
            }
            if let (Some(om), Some(nm)) = (fd.old_mode, fd.new_mode) {
                if om != nm {
                    out.push_str(&format!(
                        "old mode {}\nnew mode {}\n",
                        mode_name(om),
                        mode_name(nm)
                    ));
                }
            }
        }
        FileDiffKind::Modified => {
            if let (Some(om), Some(nm)) = (fd.old_mode, fd.new_mode) {
                if om != nm {
                    out.push_str(&format!(
                        "old mode {}\nnew mode {}\n",
                        mode_name(om),
                        mode_name(nm)
                    ));
                }
            }
        }
    }
    out
}

/// Render one file's content diff (unified). `None` content ⇒ header only
/// (renames without content change, mode-only changes).
pub fn render_content(fd: &FileDiff, cd: &ContentDiff, context: usize) -> String {
    let mut out = String::new();
    if cd.binary {
        let a = fd.old_path.as_deref().unwrap_or(&fd.path);
        out.push_str(&format!("Binary files a/{a} and b/{} differ\n", fd.path));
        return out;
    }
    if fd.old_oid == fd.new_oid {
        return out; // pure rename / mode change: no content section
    }
    let a = fd.old_path.as_deref().unwrap_or(&fd.path);
    out.push_str(&format!("--- a/{a}\n+++ b/{}\n", fd.path));
    if cd.ops.is_none() {
        out.push_str("# edit distance exceeds cap; showing whole-file replace\n");
    }
    for h in hunks(&cd.a_lines, &cd.b_lines, &cd.ops2, context) {
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            h.old_start, h.old_lines, h.new_start, h.new_lines
        ));
        for l in &h.lines {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// Expand opcodes into tagged entries and cluster into hunks with context.
pub fn hunks(a: &[String], b: &[String], ops: &[Opcode], context: usize) -> Vec<Hunk> {
    // (tag, a_line_idx, b_line_idx)
    let mut exp: Vec<(char, Option<usize>, Option<usize>)> = Vec::new();
    for op in ops {
        match op.tag {
            Tag::Equal => {
                for i in 0..(op.a2 - op.a1) {
                    exp.push((' ', Some(op.a1 + i), Some(op.b1 + i)));
                }
            }
            Tag::Delete => {
                for i in 0..(op.a2 - op.a1) {
                    exp.push(('-', Some(op.a1 + i), None));
                }
            }
            Tag::Insert => {
                for i in 0..(op.b2 - op.b1) {
                    exp.push(('+', None, Some(op.b1 + i)));
                }
            }
            Tag::Replace => {
                for i in 0..(op.a2 - op.a1) {
                    exp.push(('-', Some(op.a1 + i), None));
                }
                for i in 0..(op.b2 - op.b1) {
                    exp.push(('+', None, Some(op.b1 + i)));
                }
            }
        }
    }
    let changed: Vec<usize> = exp
        .iter()
        .enumerate()
        .filter(|(_, (t, _, _))| *t != ' ')
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    // cluster indices with gaps ≤ 2*context, expand by context
    let mut ranges: Vec<(usize, usize)> = Vec::new(); // inclusive over exp
    let mut start = changed[0].saturating_sub(context);
    let mut end = (changed[0] + context).min(exp.len().saturating_sub(1));
    for &c in &changed[1..] {
        let cs = c.saturating_sub(context);
        let ce = (c + context).min(exp.len().saturating_sub(1));
        if cs <= end + 1 {
            end = end.max(ce);
        } else {
            ranges.push((start, end));
            start = cs;
            end = ce;
        }
    }
    ranges.push((start, end));

    let mut out = Vec::new();
    for (s, e) in ranges {
        let slice = &exp[s..=e];
        let old_lines = slice.iter().filter(|(_, ai, _)| ai.is_some()).count();
        let new_lines = slice.iter().filter(|(_, _, bi)| bi.is_some()).count();
        // positions before the hunk
        let a_before = slice
            .iter()
            .filter(|(_, ai, _)| ai.is_some())
            .map(|(_, ai, _)| ai.unwrap())
            .next()
            .unwrap_or(0);
        let b_before = slice
            .iter()
            .filter(|(_, _, bi)| bi.is_some())
            .map(|(_, _, bi)| bi.unwrap())
            .next()
            .unwrap_or(0);
        let old_start = if old_lines == 0 {
            a_before
        } else {
            a_before + 1
        };
        let new_start = if new_lines == 0 {
            b_before
        } else {
            b_before + 1
        };
        let mut lines = Vec::with_capacity(slice.len());
        for (t, ai, bi) in slice {
            let (src, idx) = match (ai, bi) {
                (Some(i), _) if *t != '+' => (a, *i),
                (_, Some(i)) => (b, *i),
                (Some(i), _) => (a, *i),
                (None, None) => unreachable!(),
            };
            let raw = &src[idx];
            let (text, had_nl) = match raw.strip_suffix('\n') {
                Some(t) => (t.to_string(), true),
                None => (raw.clone(), false),
            };
            lines.push(format!("{t}{text}"));
            if !had_nl {
                lines.push("\\ No newline at end of file".to_string());
            }
        }
        out.push(Hunk {
            old_start,
            old_lines,
            new_start,
            new_lines,
            lines,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::myers::{diff_lines, split_lines};
    use crate::object::types::EntryMode;
    use crate::object::ObjectId;

    fn cd(a: &str, b: &str) -> ContentDiff {
        let al = split_lines(a.as_bytes());
        let bl = split_lines(b.as_bytes());
        let ops = diff_lines(&al, &bl, 1024).unwrap();
        ContentDiff {
            binary: false,
            ops: Some(ops.clone()),
            ops2: ops,
            a_lines: al
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect(),
            b_lines: bl
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect(),
        }
    }

    fn fd(path: &str) -> FileDiff {
        FileDiff {
            kind: FileDiffKind::Modified,
            path: path.into(),
            old_path: None,
            old_mode: Some(EntryMode::File),
            new_mode: Some(EntryMode::File),
            old_oid: Some(ObjectId::from_bytes([1; 32])),
            new_oid: Some(ObjectId::from_bytes([2; 32])),
            binary: false,
            similarity: None,
        }
    }

    #[test]
    fn golden_unified() {
        let a = "l1\nl2\nl3\nl4\nl5\n";
        let b = "l1\nl2\nXX\nl4\nl5\nl6\n";
        let out = render_content(&fd("f.txt"), &cd(a, b), 1);
        // context=1: the gap between the two changes (l4,l5 = 2 equal lines)
        // is ≤ 2·context, so git-compatible rendering merges them into one hunk.
        assert_eq!(
            out,
            "\
--- a/f.txt
+++ b/f.txt
@@ -2,4 +2,5 @@
 l2
-l3
+XX
 l4
 l5
+l6
"
        );
    }

    #[test]
    fn no_newline_marker() {
        let out = render_content(&fd("f.txt"), &cd("a\nb", "a\nc"), 3);
        assert!(out.contains("\\ No newline at end of file"));
        assert!(out.contains("-b"));
        assert!(out.contains("+c"));
    }

    #[test]
    fn multiple_hunks_split_by_distance() {
        let a = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let b = "1\nX\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\nY\n";
        let out = render_content(&fd("f.txt"), &cd(a, b), 1);
        let hunk_headers: Vec<&str> = out.lines().filter(|l| l.starts_with("@@")).collect();
        assert_eq!(hunk_headers.len(), 2, "{out}");
    }

    #[test]
    fn header_forms() {
        let mut f = fd("new.txt");
        f.kind = FileDiffKind::Added;
        f.old_oid = None;
        f.old_mode = None;
        let h = file_header(&f);
        assert!(h.contains("diff --newgit a/new.txt b/new.txt"));
        assert!(h.contains("new file"));

        let mut f = fd("moved.txt");
        f.kind = FileDiffKind::Renamed;
        f.old_path = Some("old.txt".into());
        f.similarity = Some(87);
        let h = file_header(&f);
        assert!(h.contains("similarity index 87%"));
        assert!(h.contains("rename from old.txt"));
        assert!(h.contains("rename to moved.txt"));
    }

    #[test]
    fn pure_rename_has_no_content_section() {
        let mut f = fd("b.txt");
        f.kind = FileDiffKind::Renamed;
        f.old_path = Some("a.txt".into());
        f.new_oid = f.old_oid;
        let mut c = cd("x\n", "x\n");
        c.binary = false;
        let out = render_content(&f, &c, 3);
        assert!(out.is_empty());
    }
}
