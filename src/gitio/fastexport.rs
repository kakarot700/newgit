//! Parser for `git fast-export` streams (hand-rolled, no dependencies —
//! D-007: git interop via the system git's stream formats).
//!
//! Supported grammar (git 2.x fast-export output):
//! ```text
//! stream     := event*
//! event      := blob | commit | tag | reset | feature | progress | done
//! blob       := "blob" NL mark? original-oid? data
//! commit     := "commit" SP ref NL mark? original-oid?
//!               author? NL committer? NL encoding? NL data
//!               ("from" SP committish NL)? ("merge" SP committish NL)*
//!               fileop*  (NL-terminated block)
//! tag        := "tag" SP ref NL "from" SP committish NL
//!               ("tagger" NL)? data
//! reset      := "reset" SP ref NL ("from" SP committish NL)?
//! data       := "data" SP len NL <len bytes> NL?
//!             | "data" SP "<<" delim NL <lines> delim NL
//! fileop     := "M" SP mode SP (mark | "inline" data) SP path NL
//!             | "D" SP path NL | "R" SP path SP path NL
//! committish := ":" mark | 40- or 64-hex Git object ID | "refs/..."
//! ```
//! Paths are C-quoted by git when they contain special characters; the
//! unquoter below handles the documented escapes.
//!
//! The parser is total: malformed input yields `Err`, never a panic (fuzzed
//! in tests/fuzz_parsers.rs via the gitio feature surface).

use std::io::{BufRead, BufReader};

use crate::error::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FxPerson {
    pub name: String,
    pub email: String,
    /// Unix seconds.
    pub ts: i64,
    /// Timezone offset in minutes (east-positive).
    pub tz_min: i16,
}

#[derive(Clone, Debug)]
pub struct FxBlob {
    pub mark: Option<u64>,
    pub git_sha: Option<String>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum FileOp {
    /// `deleteall` — clear the accumulated tree before subsequent ops
    /// (git emits this with --full-tree for every commit).
    DeleteAll,
    Modify {
        /// Raw git mode string: "100644" | "100755" | "120000" | "160000".
        mode: String,
        path: String,
        /// Either a mark reference or inline data.
        mark: Option<u64>,
        inline: Option<Vec<u8>>,
    },
    Delete {
        path: String,
    },
    /// `M 160000 <40- or 64-hex-sha> <path>` — git submodule (gitlink). NewGit
    /// does not support submodules; the importer turns this into a loud,
    /// actionable error.
    Gitlink {
        sha: String,
        path: String,
    },
    Rename {
        from: String,
        to: String,
    },
}

#[derive(Clone, Debug)]
pub struct FxCommit {
    pub mark: Option<u64>,
    pub git_sha: Option<String>,
    pub ref_name: String,
    pub author: Option<FxPerson>,
    pub committer: Option<FxPerson>,
    pub message: Vec<u8>,
    /// Parent committishes in stream order: first `from`, then `merge`s.
    pub parents: Vec<Committish>,
    pub ops: Vec<FileOp>,
}

#[derive(Clone, Debug)]
pub struct FxTag {
    pub ref_name: String,
    pub from: Committish,
    pub tagger: Option<FxPerson>,
    pub message: Vec<u8>,
    pub git_sha: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FxReset {
    pub ref_name: String,
    pub from: Option<Committish>,
}

#[derive(Clone, Debug)]
pub enum Committish {
    Mark(u64),
    Sha(String),
    Ref(String),
}

#[derive(Clone, Debug)]
pub enum Event {
    Blob(FxBlob),
    Commit(FxCommit),
    Tag(FxTag),
    Reset(FxReset),
    /// feature/progress/done/option lines — recorded for diagnostics only.
    Meta(String),
}

pub struct Parser<R: BufRead> {
    r: R,
    /// One-line pushback (fileop lookahead).
    pending: Option<String>,
    done: bool,
}

impl<R: BufRead> std::fmt::Debug for Parser<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parser")
            .field("pending", &self.pending)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl Parser<BufReader<std::process::ChildStdout>> {
    pub fn from_child_stdout(out: std::process::ChildStdout) -> Self {
        Parser {
            r: BufReader::new(out),
            pending: None,
            done: false,
        }
    }
}

impl<R: BufRead> Parser<R> {
    pub fn new(r: R) -> Self {
        Parser {
            r,
            pending: None,
            done: false,
        }
    }

    /// Next event, or None at end of stream.
    pub fn next_event(&mut self) -> Result<Option<Event>> {
        if self.done {
            return Ok(None);
        }
        let Some(line) = self.readline()? else {
            self.done = true;
            return Ok(None);
        };
        let line = line.trim_end();
        if line.is_empty() {
            return self.next_event();
        }
        let mut it = line.splitn(2, ' ');
        let cmd = it.next().unwrap_or("");
        let arg = it.next().unwrap_or("");
        match cmd {
            "blob" => Ok(Some(Event::Blob(self.blob()?))),
            "commit" => {
                if arg.is_empty() {
                    return Err(bad("commit without ref"));
                }
                Ok(Some(Event::Commit(self.commit(arg)?)))
            }
            "tag" => {
                if arg.is_empty() {
                    return Err(bad("tag without ref"));
                }
                Ok(Some(Event::Tag(self.tag(arg)?)))
            }
            "reset" => {
                if arg.is_empty() {
                    return Err(bad("reset without ref"));
                }
                Ok(Some(Event::Reset(self.reset(arg)?)))
            }
            "feature" | "option" | "progress" => Ok(Some(Event::Meta(format!("{cmd} {arg}")))),
            "done" => {
                self.done = true;
                Ok(None)
            }
            other => Err(bad(format!("unknown fast-export command {other:?}"))),
        }
    }

    // ── primitives ──

    fn readline(&mut self) -> Result<Option<String>> {
        if let Some(p) = self.pending.take() {
            return Ok(Some(p));
        }
        let mut s = String::new();
        let n = self.r.read_line(&mut s)?;
        if n == 0 {
            Ok(None)
        } else {
            // strip exactly one trailing newline
            if s.ends_with('\n') {
                s.pop();
            }
            Ok(Some(s))
        }
    }

    fn pushback(&mut self, line: String) {
        assert!(self.pending.is_none(), "single-line pushback only");
        self.pending = Some(line);
    }

    /// `data <len>` or `data <<DELIM`; returns raw bytes.
    fn data(&mut self) -> Result<Vec<u8>> {
        let Some(line) = self.readline()? else {
            return Err(bad("expected data line at EOF"));
        };
        let rest = line
            .strip_prefix("data ")
            .ok_or_else(|| bad(format!("expected data, got {line:?}")))?;
        if let Some(delim) = rest.strip_prefix("<<") {
            let delim = delim.trim_end();
            if delim.is_empty() {
                return Err(bad("empty heredoc delimiter"));
            }
            let mut out: Vec<u8> = Vec::new();
            loop {
                let mut buf = String::new();
                let n = self.r.read_line(&mut buf)?;
                if n == 0 {
                    return Err(bad("EOF inside heredoc data"));
                }
                if buf.trim_end_matches(['\n', '\r']) == delim {
                    break;
                }
                out.extend_from_slice(buf.as_bytes());
                if out.len() > MAX_DATA {
                    return Err(bad("heredoc data exceeds cap"));
                }
            }
            Ok(out)
        } else {
            let len: usize = rest
                .trim()
                .parse()
                .map_err(|_| bad(format!("bad data length {rest:?}")))?;
            if len > MAX_DATA {
                return Err(bad(format!("data length {len} exceeds cap {MAX_DATA}")));
            }
            let mut buf = vec![0u8; len];
            self.r.read_exact(&mut buf)?;
            // The payload length is exact. git appends an LF only when the
            // payload does not already end with one — so: consume a newline
            // if present, otherwise the next line starts immediately.
            let head = self.r.fill_buf()?;
            if head.first() == Some(&b'\n') {
                self.r.consume(1);
            }
            Ok(buf)
        }
    }

    fn mark_line(&mut self) -> Result<Option<u64>> {
        // Peek: marks appear right after blob/commit; we read and push back
        // anything else.
        let Some(line) = self.readline()? else {
            return Ok(None);
        };
        if let Some(m) = line.strip_prefix("mark :") {
            let m: u64 = m
                .trim()
                .parse()
                .map_err(|_| bad(format!("bad mark {m:?}")))?;
            Ok(Some(m))
        } else {
            self.pushback(line);
            Ok(None)
        }
    }

    fn original_oid_line(&mut self) -> Result<Option<String>> {
        let Some(line) = self.readline()? else {
            return Ok(None);
        };
        if let Some(sha) = line.strip_prefix("original-oid ") {
            let sha = sha.trim().to_string();
            if !is_git_object_id(&sha) {
                return Err(bad(format!("bad original-oid {sha:?}")));
            }
            Ok(Some(sha))
        } else {
            self.pushback(line);
            Ok(None)
        }
    }

    // ── events ──

    fn blob(&mut self) -> Result<FxBlob> {
        let mark = self.mark_line()?;
        let git_sha = self.original_oid_line()?;
        let data = self.data()?;
        Ok(FxBlob {
            mark,
            git_sha,
            data,
        })
    }

    fn person(&mut self, line: &str) -> Result<FxPerson> {
        // "<role> Name <email> <ts> <tz>"
        let body = line
            .split_once(' ')
            .map(|(_, b)| b)
            .ok_or_else(|| bad(format!("bad person line {line:?}")))?;
        parse_person(body)
    }

    fn commit(&mut self, ref_name: &str) -> Result<FxCommit> {
        let mark = self.mark_line()?;
        let git_sha = self.original_oid_line()?;
        let mut author = None;
        let mut committer = None;
        // author/committer/encoding lines in any order before data
        loop {
            let Some(line) = self.readline()? else {
                return Err(bad("EOF in commit header"));
            };
            if line.starts_with("author ") {
                author = Some(self.person(&line)?);
            } else if line.starts_with("committer ") {
                committer = Some(self.person(&line)?);
            } else if line.starts_with("encoding ") {
                // recorded implicitly: message bytes stay as-is
            } else {
                self.pushback(line);
                break;
            }
        }
        let message = self.data()?;
        let mut parents = Vec::new();
        let mut ops = Vec::new();
        while let Some(line) = self.readline()? {
            let t = line.trim_end();
            if t.is_empty() {
                break; // blank line terminates the commit
            }
            if let Some(c) = t.strip_prefix("from ") {
                parents.push(parse_committish(c)?);
                continue;
            }
            if let Some(c) = t.strip_prefix("merge ") {
                parents.push(parse_committish(c)?);
                continue;
            }
            if t == "deleteall" {
                ops.push(FileOp::DeleteAll);
                continue;
            }
            if let Some(rest) = t.strip_prefix("M ") {
                ops.push(self.modify(rest)?);
                continue;
            }
            if let Some(rest) = t.strip_prefix("D ") {
                ops.push(FileOp::Delete {
                    path: unquote_path(rest.trim_end())?,
                });
                continue;
            }
            if let Some(rest) = t.strip_prefix("R ") {
                let (a, b) = split_two_paths(rest.trim_end())?;
                ops.push(FileOp::Rename { from: a, to: b });
                continue;
            }
            // anything else (NUL-terminated variants we never request,
            // feature lines inside commits) is rejected loudly
            return Err(bad(format!("unexpected line in commit: {t:?}")));
        }
        Ok(FxCommit {
            mark,
            git_sha,
            ref_name: ref_name.to_string(),
            author,
            committer,
            message,
            parents,
            ops,
        })
    }

    fn modify(&mut self, rest: &str) -> Result<FileOp> {
        // "M <mode> <mark|inline> <path>" — path may be C-quoted (spaces ok)
        let mut it = rest.splitn(3, ' ');
        let mode = it.next().ok_or_else(|| bad("M without mode"))?.to_string();
        let src = it
            .next()
            .ok_or_else(|| bad("M without source"))?
            .to_string();
        let path = match it.next() {
            Some(p) => unquote_path(p.trim_end())?,
            None => String::new(), // inline: path follows the data block
        };
        if !src.starts_with(':') && src != "inline" {
            // raw sha source: git emits this only for gitlinks (submodules)
            if is_git_object_id(&src) {
                return Ok(FileOp::Gitlink { sha: src, path });
            }
            return Err(bad(format!(
                "M source must be :mark, inline, or a 40-/64-hex gitlink object ID; got {src:?}"
            )));
        }
        if src == "inline" {
            // inline form: `M <mode> inline` LF data LF path LF — the path
            // follows the data block on its own line (`path` here is empty).
            if !path.is_empty() {
                return Err(bad(format!(
                    "inline M must not carry a path on the command line: {path:?}"
                )));
            }
            let data = self.data()?;
            let Some(line) = self.readline()? else {
                return Err(bad("EOF: expected path after inline data"));
            };
            let path = unquote_path(line.trim_end())?;
            if path.is_empty() {
                return Err(bad("empty path after inline data"));
            }
            return Ok(FileOp::Modify {
                mode,
                path,
                mark: None,
                inline: Some(data),
            });
        }
        let mark = src
            .strip_prefix(':')
            .ok_or_else(|| bad(format!("M source must be :mark or inline, got {src:?}")))?
            .parse::<u64>()
            .map_err(|_| bad(format!("bad mark in M: {src:?}")))?;
        Ok(FileOp::Modify {
            mode,
            path,
            mark: Some(mark),
            inline: None,
        })
    }

    fn tag(&mut self, name: &str) -> Result<FxTag> {
        // git emits the tag name WITHOUT the refs/tags/ prefix; restore it
        // (but accept a full ref name if a producer sends one).
        let ref_name = if name.starts_with("refs/") {
            name.to_string()
        } else {
            format!("refs/tags/{name}")
        };
        // Real git order: from → original-oid → tagger? → data.
        let Some(line) = self.readline()? else {
            return Err(bad("EOF in tag"));
        };
        let from = line
            .strip_prefix("from ")
            .ok_or_else(|| bad(format!("tag without from: {line:?}")))?;
        let from = parse_committish(from)?;
        let git_sha = self.original_oid_line()?;
        let mut tagger = None;
        let mut line = self.readline()?.ok_or_else(|| bad("EOF in tag"))?;
        if line.starts_with("tagger ") {
            tagger = Some(self.person(&line)?);
            line = self.readline()?.ok_or_else(|| bad("EOF after tagger"))?;
        }
        self.pushback(line);
        let message = self.data()?;
        Ok(FxTag {
            ref_name,
            from,
            tagger,
            message,
            git_sha,
        })
    }

    fn reset(&mut self, ref_name: &str) -> Result<FxReset> {
        // optional "from" line
        let Some(line) = self.readline()? else {
            return Ok(FxReset {
                ref_name: ref_name.to_string(),
                from: None,
            });
        };
        if let Some(c) = line.strip_prefix("from ") {
            Ok(FxReset {
                ref_name: ref_name.to_string(),
                from: Some(parse_committish(c)?),
            })
        } else {
            self.pushback(line);
            Ok(FxReset {
                ref_name: ref_name.to_string(),
                from: None,
            })
        }
    }
}

/// Per-object data cap (defense against hostile streams). 2 GiB.
const MAX_DATA: usize = 2 << 30;

fn bad(msg: impl Into<String>) -> Error {
    Error::Malformed(format!("fast-export: {}", msg.into()))
}

fn is_git_object_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn parse_person(body: &str) -> Result<FxPerson> {
    // "Name <email> <ts> <tz>"
    let (name_email, tail) = body
        .rsplit_once(' ')
        .ok_or_else(|| bad(format!("bad person {body:?}")))?;
    let (name_email, ts_s) = name_email
        .rsplit_once(' ')
        .ok_or_else(|| bad(format!("bad person {body:?}")))?;
    let (name, email) = name_email
        .rsplit_once(" <")
        .ok_or_else(|| bad(format!("bad person name/email {name_email:?}")))?;
    let email = email
        .strip_suffix('>')
        .ok_or_else(|| bad(format!("unterminated email in {body:?}")))?;
    let ts: i64 = ts_s
        .parse()
        .map_err(|_| bad(format!("bad person ts {ts_s:?}")))?;
    let tz_min = parse_tz(tail.trim())?;
    Ok(FxPerson {
        name: name.to_string(),
        email: email.to_string(),
        ts,
        tz_min,
    })
}

/// "+0530" / "-0800" / "UTC" → minutes east.
pub fn parse_tz(s: &str) -> Result<i16> {
    if s == "UTC" || s == "Z" {
        return Ok(0);
    }
    if s.len() != 5 {
        return Err(bad(format!("bad tz {s:?}")));
    }
    let sign = match &s[..1] {
        "+" => 1i32,
        "-" => -1,
        _ => return Err(bad(format!("bad tz sign {s:?}"))),
    };
    let hh: i32 = s[1..3]
        .parse()
        .map_err(|_| bad(format!("bad tz hours {s:?}")))?;
    let mm: i32 = s[3..5]
        .parse()
        .map_err(|_| bad(format!("bad tz minutes {s:?}")))?;
    if hh > 23 || mm > 59 {
        return Err(bad(format!("tz out of range {s:?}")));
    }
    let total = sign * (hh * 60 + mm);
    i16::try_from(total).map_err(|_| bad(format!("tz overflow {s:?}")))
}

pub fn parse_committish(s: &str) -> Result<Committish> {
    let s = s.trim();
    if let Some(m) = s.strip_prefix(':') {
        let m: u64 = m
            .parse()
            .map_err(|_| bad(format!("bad mark committish {s:?}")))?;
        return Ok(Committish::Mark(m));
    }
    if is_git_object_id(s) {
        return Ok(Committish::Sha(s.to_string()));
    }
    if s.starts_with("refs/") || s == "HEAD" {
        return Ok(Committish::Ref(s.to_string()));
    }
    Err(bad(format!("unrecognized committish {s:?}")))
}

/// Undo git's C-quoting for paths ("a\"b\\c\nd", octal \NNN for UTF-8 bytes).
pub fn unquote_path(s: &str) -> Result<String> {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('"') else {
        // bare path: git quotes only when needed; spaces are allowed bare
        return Ok(s.to_string());
    };
    let inner = inner
        .strip_suffix('"')
        .ok_or_else(|| bad(format!("unterminated quoted path {s:?}")))?;
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            return Err(bad("trailing backslash in quoted path"));
        }
        match bytes[i] {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0C),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0B),
            b'\\' => out.push(b'\\'),
            b'"' => out.push(b'"'),
            c @ b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                for _ in 0..2 {
                    if i + 1 < bytes.len() && bytes[i + 1] >= b'0' && bytes[i + 1] <= b'7' {
                        i += 1;
                        v = v * 8 + (bytes[i] - b'0') as u32;
                    } else {
                        break;
                    }
                }
                out.push(v as u8);
            }
            other => {
                return Err(bad(format!(
                    "unknown escape \\{} in quoted path",
                    other as char
                )))
            }
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| bad("quoted path is not valid utf-8 after unescape"))
}

/// Split "R <from> <to>" remainder into two paths, honoring C-quoting.
fn split_two_paths(s: &str) -> Result<(String, String)> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        // quoted first path: find the closing quote honoring escapes
        let bytes = rest.as_bytes();
        let mut i = 0;
        loop {
            if i >= bytes.len() {
                return Err(bad("unterminated quote in R paths"));
            }
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                break;
            }
            i += 1;
        }
        let first = &s[..i + 2]; // incl. both quotes
        let second = rest[i + 1..].trim();
        Ok((unquote_path(first)?, unquote_path(second)?))
    } else {
        let (a, b) = s
            .split_once(' ')
            .ok_or_else(|| bad(format!("R needs two paths: {s:?}")))?;
        Ok((unquote_path(a)?, unquote_path(b)?))
    }
}

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(s: &str) -> Result<Vec<Event>> {
        let mut p = Parser::new(s.as_bytes());
        let mut out = Vec::new();
        while let Some(e) = p.next_event()? {
            out.push(e);
        }
        Ok(out)
    }

    #[test]
    fn parses_minimal_stream() {
        let s = "blob\nmark :1\ndata 2\na\n\ncommit refs/heads/master\nmark :2\nauthor A <a@x> 100 +0000\ncommitter A <a@x> 100 +0000\ndata 4\none\nM 100644 :1 a.txt\n\n";
        let evs = parse_all(s).unwrap();
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            Event::Blob(b) => {
                assert_eq!(b.mark, Some(1));
                assert_eq!(b.data, b"a\n");
            }
            _ => panic!(),
        }
        match &evs[1] {
            Event::Commit(c) => {
                assert_eq!(c.ref_name, "refs/heads/master");
                assert_eq!(c.message, b"one\n");
                assert_eq!(c.parents.len(), 0);
                assert_eq!(c.ops.len(), 1);
                match &c.ops[0] {
                    FileOp::Modify {
                        mode, path, mark, ..
                    } => {
                        assert_eq!(mode, "100644");
                        assert_eq!(path, "a.txt");
                        assert_eq!(*mark, Some(1));
                    }
                    _ => panic!(),
                }
                let a = c.author.as_ref().unwrap();
                assert_eq!(a.name, "A");
                assert_eq!(a.email, "a@x");
                assert_eq!(a.ts, 100);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_heredoc_inline_and_merges() {
        let s = "commit refs/heads/x\nmark :5\nauthor A B <a@b.c> 5 -0800\ndata <<EOM\nmerge msg\nsecond line\nEOM\nfrom :2\nmerge :3\ndeleteall\nM 100755 inline\ndata 4\n#!/x\nexec.sh\nD gone.txt\nR old.txt new.txt\n\n";
        let evs = parse_all(s).unwrap();
        match &evs[0] {
            Event::Commit(c) => {
                assert_eq!(c.message, b"merge msg\nsecond line\n");
                assert_eq!(c.parents.len(), 2);
                assert!(matches!(c.parents[0], Committish::Mark(2)));
                assert!(matches!(c.parents[1], Committish::Mark(3)));
                assert_eq!(c.author.as_ref().unwrap().tz_min, -480);
                assert!(matches!(c.ops[0], FileOp::DeleteAll));
                match &c.ops[1] {
                    FileOp::Modify {
                        inline, mode, path, ..
                    } => {
                        assert_eq!(inline.as_deref(), Some(&b"#!/x"[..]));
                        assert_eq!(mode, "100755");
                        assert_eq!(path, "exec.sh");
                    }
                    _ => panic!(),
                }
                assert!(matches!(c.ops[2], FileOp::Delete { .. }));
                match &c.ops[3] {
                    FileOp::Rename { from, to } => {
                        assert_eq!(from, "old.txt");
                        assert_eq!(to, "new.txt");
                    }
                    _ => panic!(),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_tags_and_resets() {
        let s = "tag v1\nfrom :2\noriginal-oid 0000000000000000000000000000000000000000\ntagger T <t@x> 200 +0530\ndata 7\nrel v1\n\nreset refs/heads/gone\n\nreset refs/heads/moved\nfrom :2\n\n";
        let evs = parse_all(s).unwrap();
        match &evs[0] {
            Event::Tag(t) => {
                assert_eq!(t.ref_name, "refs/tags/v1");
                assert_eq!(t.tagger.as_ref().unwrap().tz_min, 330);
                assert_eq!(t.message, b"rel v1\n");
                assert!(t.git_sha.is_some());
            }
            _ => panic!(),
        }
        match &evs[1] {
            Event::Reset(r) => {
                assert_eq!(r.ref_name, "refs/heads/gone");
                assert!(r.from.is_none());
            }
            _ => panic!(),
        }
        match &evs[2] {
            Event::Reset(r) => assert!(matches!(r.from, Some(Committish::Mark(2)))),
            _ => panic!(),
        }
    }

    #[test]
    fn parses_sha256_object_ids_and_gitlinks() {
        let oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let stream = format!(
            "blob\noriginal-oid {oid}\ndata 1\nx\n\ncommit refs/heads/master\nmark :1\nauthor A <a@x> 100 +0000\ncommitter A <a@x> 100 +0000\ndata 0\nM 160000 {oid} sub\n\n"
        );
        let events = parse_all(&stream).unwrap();
        match &events[0] {
            Event::Blob(blob) => assert_eq!(blob.git_sha.as_deref(), Some(oid)),
            _ => panic!("expected blob"),
        }
        match &events[1] {
            Event::Commit(commit) => match &commit.ops[0] {
                FileOp::Gitlink { sha, path } => {
                    assert_eq!(sha, oid);
                    assert_eq!(path, "sub");
                }
                _ => panic!("expected gitlink"),
            },
            _ => panic!("expected commit"),
        }
    }

    #[test]
    fn unquotes_paths() {
        assert_eq!(unquote_path("plain path.txt").unwrap(), "plain path.txt");
        assert_eq!(unquote_path("\"a\\\"b\\\\c\\td\"").unwrap(), "a\"b\\c\td");
        // UTF-8 via octal escapes: "é" = \303\251
        assert_eq!(unquote_path("\"\\303\\251.txt\"").unwrap(), "é.txt");
        assert!(unquote_path("\"unterminated").is_err());
        assert!(unquote_path("\"bad \\q escape\"").is_err());
    }

    #[test]
    fn person_and_tz_edge_cases() {
        assert_eq!(parse_tz("+0530").unwrap(), 330);
        assert_eq!(parse_tz("-0800").unwrap(), -480);
        assert_eq!(parse_tz("UTC").unwrap(), 0);
        assert!(parse_tz("+2400").is_err());
        assert!(parse_tz("nope").is_err());
        let p = parse_person("Multi Word Name <mw@x.io> 1700000000 +0100").unwrap();
        assert_eq!(p.name, "Multi Word Name");
        assert_eq!(p.email, "mw@x.io");
        assert_eq!(p.ts, 1700000000);
        assert_eq!(p.tz_min, 60);
        assert!(parse_person("broken").is_err());
        assert!(parse_person("Name <unclosed 1 +0000").is_err());
    }

    #[test]
    fn rejects_garbage_cleanly() {
        for bad_stream in [
            "wat\n",
            "commit\n",
            "blob\ndata notanumber\n",
            "blob\ndata 99999\nshort",
            "commit refs/x\ndata 2\nok\nM 100644 notmark p\n",
            "tag refs/t\nnfrom :1\ndata 0\n\n",
        ] {
            let r = parse_all(bad_stream);
            assert!(r.is_err(), "stream should be rejected: {bad_stream:?}");
        }
    }

    #[test]
    fn data_cap_rejects_absurd_lengths() {
        let s = "blob\ndata 99999999999999\n";
        assert!(parse_all(s).is_err());
    }
}
