//! `.newgitignore` — a deterministic, documented subset of gitignore.
//!
//! Grammar (per line):
//! * blank lines and `#` comments are ignored;
//! * `!` prefix negates (re-includes);
//! * trailing `/` restricts the rule to directories;
//! * a pattern containing `/` (beyond a trailing one) is anchored to the
//!   workspace root; otherwise it matches the basename at any depth;
//! * wildcards: `*` (within a segment), `?` (one byte), `[a-z]`/`[!abc]`
//!   classes; `**` as a full segment crosses directory boundaries;
//! * the LAST matching rule decides (git semantics).
//!
//! Directory pruning optimization: directories are pruned only when the set
//! contains no negated rules (otherwise every file is evaluated
//! individually — conservative and correct).

use std::path::Path;

use crate::error::{Error, Result};

#[derive(Clone, Debug)]
enum Seg {
    Lit(String),
    Wild(String),
    DoubleStar,
}

#[derive(Clone, Debug)]
struct Rule {
    neg: bool,
    dir_only: bool,
    anchored: bool,
    segs: Vec<Seg>,
}

#[derive(Clone, Debug, Default)]
pub struct IgnoreSet {
    rules: Vec<Rule>,
    has_neg: bool,
}

impl IgnoreSet {
    pub fn empty() -> IgnoreSet {
        IgnoreSet::default()
    }

    pub fn parse(text: &str) -> Result<IgnoreSet> {
        let mut rules = Vec::new();
        let mut has_neg = false;
        for (lineno, line) in text.lines().enumerate() {
            let line = line.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (neg, pat) = match trimmed.strip_prefix('!') {
                Some(p) => {
                    has_neg = true;
                    (true, p)
                }
                None => (false, trimmed),
            };
            if pat.is_empty() {
                return Err(Error::Invalid(format!(
                    ".newgitignore line {}: empty pattern after '!'",
                    lineno + 1
                )));
            }
            let (dir_only, pat) = match pat.strip_suffix('/') {
                Some(p) => (true, p),
                None => (false, pat),
            };
            if pat.is_empty() || pat.contains("//") {
                return Err(Error::Invalid(format!(
                    ".newgitignore line {}: bad pattern {line:?}",
                    lineno + 1
                )));
            }
            let anchored = pat.starts_with('/') || pat.contains('/');
            let pat = pat.strip_prefix('/').unwrap_or(pat);
            let mut segs = Vec::new();
            for s in pat.split('/') {
                if s.is_empty() {
                    return Err(Error::Invalid(format!(
                        ".newgitignore line {}: empty segment in {pat:?}",
                        lineno + 1
                    )));
                }
                if s == "**" {
                    segs.push(Seg::DoubleStar);
                } else if s.contains('*') || s.contains('?') || s.contains('[') {
                    segs.push(Seg::Wild(s.to_string()));
                } else {
                    segs.push(Seg::Lit(s.to_string()));
                }
            }
            if segs.len() > 64 {
                return Err(Error::Limit(format!(
                    ".newgitignore line {}: pattern too deep",
                    lineno + 1
                )));
            }
            rules.push(Rule {
                neg,
                dir_only,
                anchored,
                segs,
            });
        }
        if rules.len() > 4096 {
            return Err(Error::Limit(".newgitignore has too many rules".into()));
        }
        Ok(IgnoreSet { rules, has_neg })
    }

    pub fn load(path: &Path) -> Result<IgnoreSet> {
        match std::fs::read_to_string(path) {
            Ok(text) => IgnoreSet::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(IgnoreSet::empty()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Decide whether `rel` (slash-separated, relative to workspace root) is
    /// ignored. `is_dir` selects directory-only rules.
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let path: Vec<&str> = rel.split('/').collect();
        let mut decision = false;
        for rule in &self.rules {
            if rule.dir_only && !is_dir {
                continue;
            }
            if rule_matches(rule, &path) {
                decision = !rule.neg;
            }
        }
        decision
    }

    /// May a matching directory be pruned from traversal entirely?
    /// Only when no negation rules exist (see module docs).
    pub fn can_prune_dir(&self, rel: &str) -> bool {
        !self.has_neg && self.is_ignored(rel, true)
    }
}

fn rule_matches(rule: &Rule, path: &[&str]) -> bool {
    if rule.anchored {
        match_segs(&rule.segs, path)
    } else {
        // basename match (single segment guaranteed: not anchored ⇒ no '/')
        debug_assert_eq!(rule.segs.len(), 1);
        match (&rule.segs[0], path.last()) {
            (Seg::Lit(p), Some(last)) => p == last,
            (Seg::Wild(p), Some(last)) => glob_match(p.as_bytes(), last.as_bytes()),
            (Seg::DoubleStar, _) => true,
            (_, None) => false,
        }
    }
}

/// Segment-wise match with `**` crossing zero or more segments.
fn match_segs(segs: &[Seg], path: &[&str]) -> bool {
    if segs.is_empty() {
        return path.is_empty();
    }
    match &segs[0] {
        Seg::DoubleStar => {
            // `**` consumes 0..n segments; also `**/x` == x at any depth,
            // and a trailing `/**` matches everything inside.
            if segs.len() == 1 {
                return true; // trailing ** matches any remainder (incl. empty)
            }
            for k in 0..=path.len() {
                if match_segs(&segs[1..], &path[k..]) {
                    return true;
                }
            }
            false
        }
        Seg::Lit(p) => !path.is_empty() && *p == path[0] && match_segs(&segs[1..], &path[1..]),
        Seg::Wild(p) => {
            !path.is_empty()
                && glob_match(p.as_bytes(), path[0].as_bytes())
                && match_segs(&segs[1..], &path[1..])
        }
    }
}

/// Classic iterative glob with `*` backtracking, `?`, and `[...]` classes.
/// Operates on bytes (documented: `?`/classes are byte-oriented like git).
pub fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (usize::MAX, 0usize);
    while ti < text.len() {
        if pi < pat.len() {
            match pat[pi] {
                b'*' => {
                    star_pi = pi;
                    pi += 1;
                    star_ti = ti;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                b'[' => {
                    if let Some((matched, next_pi)) = match_class(pat, pi, text[ti]) {
                        if matched {
                            pi = next_pi;
                            ti += 1;
                            continue;
                        }
                        // fall through to backtracking
                    } else {
                        // malformed class: treat '[' as literal
                        if text[ti] == b'[' {
                            pi += 1;
                            ti += 1;
                            continue;
                        }
                    }
                }
                c if c == text[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        // mismatch: backtrack to last '*' if any
        if star_pi != usize::MAX {
            star_ti += 1;
            ti = star_ti;
            pi = star_pi + 1;
        } else {
            return false;
        }
    }
    // consume trailing pattern
    while pi < pat.len() && pat[pi] == b'*' {
        pi += 1;
    }
    pi == pat.len()
}

/// Returns (matched, next_pi) or None if the class is malformed.
fn match_class(pat: &[u8], start: usize, ch: u8) -> Option<(bool, usize)> {
    debug_assert_eq!(pat[start], b'[');
    let mut i = start + 1;
    let neg = if i < pat.len() && (pat[i] == b'!' || pat[i] == b'^') {
        i += 1;
        true
    } else {
        false
    };
    let mut matched = false;
    let mut first = true;
    loop {
        if i >= pat.len() {
            return None; // unterminated
        }
        if pat[i] == b']' && !first {
            i += 1;
            break;
        }
        first = false;
        // range?
        if i + 2 < pat.len() && pat[i + 1] == b'-' && pat[i + 2] != b']' {
            let (lo, hi) = (pat[i], pat[i + 2]);
            if ch >= lo && ch <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if pat[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    Some((matched != neg, i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(lines: &[&str]) -> IgnoreSet {
        IgnoreSet::parse(&lines.join("\n")).unwrap()
    }

    #[test]
    fn basics() {
        let s = set(&[
            "*.log",
            "build/",
            "/target",
            "docs/**/tmp",
            "**/node_modules",
        ]);
        assert!(s.is_ignored("a.log", false));
        assert!(s.is_ignored("x/y/a.log", false));
        assert!(!s.is_ignored("a.logx", false));
        assert!(s.is_ignored("build", true));
        assert!(!s.is_ignored("build", false)); // dir-only
        assert!(s.is_ignored("target", true));
        assert!(s.is_ignored("target", false)); // anchored, no trailing slash
        assert!(!s.is_ignored("src/target", false)); // anchored to root
        assert!(s.is_ignored("docs/a/b/tmp", true));
        assert!(s.is_ignored("node_modules", true));
        assert!(s.is_ignored("a/b/node_modules", true));
    }

    #[test]
    fn negation_last_rule_wins() {
        let s = set(&["*.log", "!keep.log"]);
        assert!(s.is_ignored("x.log", false));
        assert!(!s.is_ignored("keep.log", false));
        assert!(s.has_neg);
        assert!(!s.can_prune_dir("whatever")); // never prune with negations
    }

    #[test]
    fn pruning_only_without_negation() {
        let s = set(&["bigdir/"]);
        assert!(s.can_prune_dir("bigdir"));
        assert!(s.can_prune_dir("a/bigdir"));
        assert!(!s.can_prune_dir("bigdir2"));
    }

    #[test]
    fn glob_engine() {
        assert!(glob_match(b"*.rs", b"main.rs"));
        assert!(!glob_match(b"*.rs", b"main.rs.bak"));
        assert!(glob_match(b"a?c", b"abc"));
        assert!(!glob_match(b"a?c", b"ac"));
        assert!(glob_match(b"[a-c]x", b"bx"));
        assert!(!glob_match(b"[a-c]x", b"dx"));
        assert!(glob_match(b"[!a-c]x", b"dx"));
        assert!(glob_match(b"*", b""));
        assert!(glob_match(b"a*b*c", b"axxbyyc"));
        assert!(!glob_match(b"a*b", b"axxbxa"));
        // malformed class behaves as literal '['
        assert!(glob_match(b"[abc", b"[abc"));
    }

    #[test]
    fn rejects_bad_patterns() {
        assert!(IgnoreSet::parse("a//b").is_err());
        assert!(IgnoreSet::parse("!").is_err());
        assert!(IgnoreSet::parse(&"x/".repeat(70)).is_err());
    }
}
