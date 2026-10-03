//! NewGit object model and canonical binary encoding.
//!
//! IDENTITY RULE (invariant, tested):
//!   id(object) = SHA-256(canonical(object))
//!   canonical(object) = type_tag || varint(body_len) || body
//!
//! The encodings below are a *protocol*: they must never change in a
//! breaking way without a new type tag / format version. See
//! docs/STORAGE_FORMAT.md for the normative specification.
//!
//! Determinism rules enforced by `validate()` at decode time:
//! * set-like fields (evidence lists, extras, metrics, approvals, dimensions)
//!   are strictly ascending and duplicate-free;
//! * tree entries are strictly ascending by name;
//! * strings are UTF-8 within documented length limits;
//! * no trailing bytes anywhere.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::object::id::ObjectId;
use crate::util::varint::{self, Reader};

pub mod limits {
    pub const MAX_NAME_LEN: usize = 255;
    pub const MAX_MESSAGE_LEN: usize = 65536;
    pub const MAX_TITLE_LEN: usize = 512;
    pub const MAX_DESC_LEN: usize = 65536;
    pub const MAX_ID_STR_LEN: usize = 512;
    pub const MAX_TOOL_LEN: usize = 256;
    pub const MAX_COMMAND_LEN: usize = 8192;
    pub const MAX_EXTRAS: usize = 256;
    pub const MAX_EXTRA_KEY: usize = 256;
    pub const MAX_EXTRA_VALUE: usize = 8192;
    pub const MAX_METRICS: usize = 256;
    pub const MAX_DIMENSIONS: usize = 256;
    pub const MAX_LIST: usize = 1 << 20;
    pub const MAX_PARENTS: usize = 64;
    pub const MAX_PUBKEY: usize = 4096;
    pub const MAX_WORKSPACE_NAME: usize = 255;
}

// ---------------------------------------------------------------------------
// Type tags
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ObjectType {
    Blob = 1,
    Tree = 2,
    Snapshot = 3,
    Actor = 4,
    Goal = 5,
    Change = 6,
    Evidence = 7,
    Evaluation = 8,
    Proposal = 9,
}

impl ObjectType {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            1 => ObjectType::Blob,
            2 => ObjectType::Tree,
            3 => ObjectType::Snapshot,
            4 => ObjectType::Actor,
            5 => ObjectType::Goal,
            6 => ObjectType::Change,
            7 => ObjectType::Evidence,
            8 => ObjectType::Evaluation,
            9 => ObjectType::Proposal,
            other => return Err(Error::Malformed(format!("unknown object type tag {other}"))),
        })
    }

    pub fn name(&self) -> &'static str {
        match self {
            ObjectType::Blob => "blob",
            ObjectType::Tree => "tree",
            ObjectType::Snapshot => "snapshot",
            ObjectType::Actor => "actor",
            ObjectType::Goal => "goal",
            ObjectType::Change => "change",
            ObjectType::Evidence => "evidence",
            ObjectType::Evaluation => "evaluation",
            ObjectType::Proposal => "proposal",
        }
    }
}

// ---------------------------------------------------------------------------
// Field-level codec helpers
// ---------------------------------------------------------------------------

fn w_u8(v: &mut Vec<u8>, b: u8) {
    v.push(b);
}
fn w_bool(v: &mut Vec<u8>, b: bool) {
    v.push(u8::from(b));
}
fn w_i64(v: &mut Vec<u8>, n: i64) {
    varint::write_i64(v, n);
}
fn w_u64(v: &mut Vec<u8>, n: u64) {
    varint::write_u64(v, n);
}
fn w_bytes(v: &mut Vec<u8>, b: &[u8]) {
    w_u64(v, b.len() as u64);
    v.extend_from_slice(b);
}
fn w_str(v: &mut Vec<u8>, s: &str) {
    w_bytes(v, s.as_bytes());
}
fn w_oid(v: &mut Vec<u8>, id: &ObjectId) {
    v.extend_from_slice(id.as_bytes());
}
fn w_opt_oid(v: &mut Vec<u8>, id: &Option<ObjectId>) {
    match id {
        Some(x) => {
            w_bool(v, true);
            w_oid(v, x);
        }
        None => w_bool(v, false),
    }
}
fn w_opt_str(v: &mut Vec<u8>, s: &Option<String>) {
    match s {
        Some(x) => {
            w_bool(v, true);
            w_str(v, x);
        }
        None => w_bool(v, false),
    }
}
fn w_oid_list(v: &mut Vec<u8>, l: &[ObjectId]) {
    w_u64(v, l.len() as u64);
    for id in l {
        w_oid(v, id);
    }
}
fn w_map(v: &mut Vec<u8>, m: &BTreeMap<String, String>) {
    w_u64(v, m.len() as u64);
    for (k, val) in m {
        w_str(v, k);
        w_str(v, val);
    }
}

fn r_str_checked<'a>(r: &mut Reader<'a>, max: usize, what: &str) -> Result<String> {
    let n = r.read_u64()? as usize;
    if n > max {
        return Err(Error::Limit(format!("{what} length {n} exceeds {max}")));
    }
    let b = r.read_bytes(n)?;
    std::str::from_utf8(b)
        .map(|s| s.to_string())
        .map_err(|e| Error::Malformed(format!("{what}: invalid utf-8: {e}")))
}

fn r_oid(r: &mut Reader<'_>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(r.read_fixed::<32>()?))
}

fn r_opt_oid(r: &mut Reader<'_>) -> Result<Option<ObjectId>> {
    Ok(if r.read_bool()? {
        Some(r_oid(r)?)
    } else {
        None
    })
}

fn r_opt_str(r: &mut Reader<'_>, max: usize, what: &str) -> Result<Option<String>> {
    Ok(if r.read_bool()? {
        Some(r_str_checked(r, max, what)?)
    } else {
        None
    })
}

fn r_oid_list(r: &mut Reader<'_>, max: usize, what: &str) -> Result<Vec<ObjectId>> {
    let n = r.read_u64()? as usize;
    if n > max {
        return Err(Error::Limit(format!("{what} count {n} exceeds {max}")));
    }
    if n.checked_mul(32)
        .map(|sz| sz > r.remaining())
        .unwrap_or(true)
    {
        return Err(Error::Malformed(format!(
            "{what} length prefix exceeds remaining input"
        )));
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(r_oid(r)?);
    }
    Ok(out)
}

fn r_map(
    r: &mut Reader<'_>,
    max: usize,
    kmax: usize,
    vmax: usize,
    what: &str,
) -> Result<BTreeMap<String, String>> {
    let n = r.read_u64()? as usize;
    if n > max {
        return Err(Error::Limit(format!("{what} count {n} exceeds {max}")));
    }
    let mut m = BTreeMap::new();
    let mut last: Option<String> = None;
    for _ in 0..n {
        let k = r_str_checked(r, kmax, &format!("{what} key"))?;
        let v = r_str_checked(r, vmax, &format!("{what} value"))?;
        if let Some(l) = &last {
            if k <= *l {
                return Err(Error::Malformed(format!(
                    "{what} keys not strictly ascending: {l:?} >= {k:?}"
                )));
            }
        }
        if m.insert(k.clone(), v).is_some() {
            return Err(Error::Malformed(format!("{what} duplicate key {k:?}")));
        }
        last = Some(k);
    }
    Ok(m)
}

/// Check a string field for control characters (newline/tab allowed only in
/// free-text fields, flagged by `multiline`).
fn check_text(s: &str, multiline: bool, what: &str) -> Result<()> {
    for c in s.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            if !multiline {
                return Err(Error::Invalid(format!(
                    "{what} contains line break/tab: {s:?}"
                )));
            }
            continue;
        }
        if (c as u32) < 0x20 || c as u32 == 0x7f {
            return Err(Error::Invalid(format!(
                "{what} contains control character U+{:04X}",
                c as u32
            )));
        }
    }
    Ok(())
}

fn check_name(s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(Error::Invalid("entry name is empty".into()));
    }
    if s.len() > limits::MAX_NAME_LEN {
        return Err(Error::Limit(format!(
            "entry name longer than {} bytes",
            limits::MAX_NAME_LEN
        )));
    }
    if s == "." || s == ".." {
        return Err(Error::Invalid(format!("illegal entry name {s:?}")));
    }
    if s.contains('/') || s.as_bytes().contains(&0) {
        return Err(Error::Invalid(format!(
            "entry name contains '/' or NUL: {s:?}"
        )));
    }
    check_text(s, false, "entry name")?;
    Ok(())
}

fn check_set_sorted(list: &[ObjectId], what: &str) -> Result<()> {
    for w in list.windows(2) {
        if w[0] >= w[1] {
            return Err(Error::Malformed(format!(
                "{what} must be strictly ascending and unique"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum EntryMode {
    File = 0,
    Executable = 1,
    Symlink = 2,
    Tree = 3,
}

impl EntryMode {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => EntryMode::File,
            1 => EntryMode::Executable,
            2 => EntryMode::Symlink,
            3 => EntryMode::Tree,
            other => return Err(Error::Malformed(format!("unknown entry mode {other}"))),
        })
    }
    pub fn is_tree(&self) -> bool {
        matches!(self, EntryMode::Tree)
    }
    /// Unix permission bits equivalent (for export/checkout).
    pub fn unix_bits(&self) -> u32 {
        match self {
            EntryMode::File => 0o100644,
            EntryMode::Executable => 0o100755,
            EntryMode::Symlink => 0o120000,
            EntryMode::Tree => 0o040000,
        }
    }
    pub fn from_unix_bits(m: u32) -> Result<Self> {
        Ok(match m & 0o170000 {
            0o040000 => EntryMode::Tree,
            0o100000 => {
                if m & 0o111 != 0 {
                    EntryMode::Executable
                } else {
                    EntryMode::File
                }
            }
            0o120000 => EntryMode::Symlink,
            other => {
                return Err(Error::Git(format!(
                    "unsupported git file mode {other:o} (newgit supports regular, exec, symlink, tree)"
                )))
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    pub mode: EntryMode,
    pub oid: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Tree> {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        entries.dedup_by(|a, b| a.name == b.name);
        let t = Tree { entries };
        t.validate()?;
        Ok(t)
    }
    pub fn empty() -> Tree {
        Tree { entries: vec![] }
    }
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_str().cmp(name))
            .ok()
            .map(|i| &self.entries[i])
    }
    pub fn validate(&self) -> Result<()> {
        if self.entries.len() > limits::MAX_LIST {
            return Err(Error::Limit("tree has too many entries".into()));
        }
        let mut last: Option<&str> = None;
        for e in &self.entries {
            check_name(&e.name)?;
            if let Some(l) = last {
                if e.name.as_str() <= l {
                    return Err(Error::Malformed(format!(
                        "tree entries not strictly ascending: {l:?} >= {:?}",
                        e.name
                    )));
                }
            }
            last = Some(&e.name);
        }
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_u64(v, self.entries.len() as u64);
        for e in &self.entries {
            w_u8(v, e.mode as u8);
            w_str(v, &e.name);
            w_oid(v, &e.oid);
        }
    }
    fn parse(r: &mut Reader<'_>) -> Result<Tree> {
        let n = r.read_u64()? as usize;
        if n > limits::MAX_LIST {
            return Err(Error::Limit(format!("tree entry count {n} exceeds limit")));
        }
        // each entry is at least 1(mode)+1(len)+1(name)+32(oid) bytes
        if n.checked_mul(35)
            .map(|sz| sz > r.remaining())
            .unwrap_or(true)
        {
            return Err(Error::Malformed(
                "tree entry count exceeds remaining input".into(),
            ));
        }
        let mut entries = Vec::with_capacity(n);
        for _ in 0..n {
            let mode = EntryMode::from_u8(r.read_u8()?)?;
            let name = r_str_checked(r, limits::MAX_NAME_LEN, "tree entry name")?;
            let oid = r_oid(r)?;
            entries.push(TreeEntry { name, mode, oid });
        }
        let t = Tree { entries };
        t.validate()?;
        Ok(t)
    }
}

// ---------------------------------------------------------------------------
// Actor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ActorKind {
    Human = 0,
    Agent = 1,
    Process = 2,
    Anonymous = 3,
}

impl ActorKind {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => ActorKind::Human,
            1 => ActorKind::Agent,
            2 => ActorKind::Process,
            3 => ActorKind::Anonymous,
            other => return Err(Error::Malformed(format!("unknown actor kind {other}"))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            ActorKind::Human => "human",
            ActorKind::Agent => "agent",
            ActorKind::Process => "process",
            ActorKind::Anonymous => "anonymous",
        }
    }
}

/// An actor is *display + provenance metadata*. It is NOT an authenticated
/// identity by itself: `pubkey`, when present, is the anchor for signatures
/// (see SECURITY_MODEL.md — unverified claims are marked as such everywhere).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    /// Stable logical id, e.g. "human:alice" or "agent:acme-coder-v2".
    pub id: String,
    pub display_name: String,
    /// Runtime/tool info, e.g. "claude-code", "ci-runner", "cli".
    pub tool: String,
    pub tool_version: String,
    /// Optional public key (algo, bytes). Presence != verification.
    pub pubkey: Option<(String, Vec<u8>)>,
    pub extras: BTreeMap<String, String>,
}

impl Actor {
    pub fn validate(&self) -> Result<()> {
        check_text(&self.id, false, "actor id")?;
        if self.id.is_empty() || self.id.len() > limits::MAX_ID_STR_LEN {
            return Err(Error::Invalid(format!(
                "actor id length must be 1..={} (got {})",
                limits::MAX_ID_STR_LEN,
                self.id.len()
            )));
        }
        check_text(&self.display_name, false, "actor display_name")?;
        if self.display_name.len() > limits::MAX_ID_STR_LEN {
            return Err(Error::Limit("actor display_name too long".into()));
        }
        check_text(&self.tool, false, "actor tool")?;
        check_text(&self.tool_version, false, "actor tool_version")?;
        if self.tool.len() > limits::MAX_TOOL_LEN || self.tool_version.len() > limits::MAX_TOOL_LEN
        {
            return Err(Error::Limit("actor tool field too long".into()));
        }
        if let Some((algo, key)) = &self.pubkey {
            check_text(algo, false, "pubkey algo")?;
            if algo.len() > 64 || key.len() > limits::MAX_PUBKEY {
                return Err(Error::Limit("pubkey too large".into()));
            }
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_u8(v, self.kind as u8);
        w_str(v, &self.id);
        w_str(v, &self.display_name);
        w_str(v, &self.tool);
        w_str(v, &self.tool_version);
        match &self.pubkey {
            Some((algo, key)) => {
                w_bool(v, true);
                w_str(v, algo);
                w_bytes(v, key);
            }
            None => w_bool(v, false),
        }
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Actor> {
        let kind = ActorKind::from_u8(r.read_u8()?)?;
        let id = r_str_checked(r, limits::MAX_ID_STR_LEN, "actor id")?;
        let display_name = r_str_checked(r, limits::MAX_ID_STR_LEN, "actor display_name")?;
        let tool = r_str_checked(r, limits::MAX_TOOL_LEN, "actor tool")?;
        let tool_version = r_str_checked(r, limits::MAX_TOOL_LEN, "actor tool_version")?;
        let pubkey = if r.read_bool()? {
            let algo = r_str_checked(r, 64, "pubkey algo")?;
            let n = r.read_u64()? as usize;
            if n > limits::MAX_PUBKEY {
                return Err(Error::Limit("pubkey too large".into()));
            }
            Some((algo, r.read_bytes(n)?.to_vec()))
        } else {
            None
        };
        let extras = r_map(
            r,
            limits::MAX_EXTRAS,
            limits::MAX_EXTRA_KEY,
            limits::MAX_EXTRA_VALUE,
            "actor extras",
        )?;
        let a = Actor {
            kind,
            id,
            display_name,
            tool,
            tool_version,
            pubkey,
            extras,
        };
        a.validate()?;
        Ok(a)
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Ordered parents; parents[0] is the primary lineage parent.
    pub parents: Vec<ObjectId>,
    /// Root tree of the immutable project state.
    pub root: ObjectId,
    pub author: ObjectId,
    pub timestamp_ms: i64,
    pub tz_offset_min: i16,
    pub message: String,
    pub workspace: Option<String>,
    pub change: Option<ObjectId>,
    pub goal: Option<ObjectId>,
    pub extras: BTreeMap<String, String>,
}

impl Snapshot {
    pub fn validate(&self) -> Result<()> {
        if self.parents.len() > limits::MAX_PARENTS {
            return Err(Error::Limit(format!(
                "snapshot has {} parents, max {}",
                self.parents.len(),
                limits::MAX_PARENTS
            )));
        }
        for w in self.parents.windows(2) {
            if w[0] == w[1] {
                return Err(Error::Malformed("duplicate snapshot parent".into()));
            }
        }
        if self.message.len() > limits::MAX_MESSAGE_LEN {
            return Err(Error::Limit("snapshot message too long".into()));
        }
        check_text(&self.message, true, "snapshot message")?;
        if let Some(ws) = &self.workspace {
            check_text(ws, false, "workspace name")?;
            if ws.len() > limits::MAX_WORKSPACE_NAME {
                return Err(Error::Limit("workspace name too long".into()));
            }
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_oid_list(v, &self.parents);
        w_oid(v, &self.root);
        w_oid(v, &self.author);
        w_i64(v, self.timestamp_ms);
        w_i64(v, self.tz_offset_min as i64);
        w_str(v, &self.message);
        w_opt_str(v, &self.workspace);
        w_opt_oid(v, &self.change);
        w_opt_oid(v, &self.goal);
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Snapshot> {
        let parents = r_oid_list(r, limits::MAX_PARENTS, "snapshot parents")?;
        let root = r_oid(r)?;
        let author = r_oid(r)?;
        let timestamp_ms = r.read_i64()?;
        let tz = r.read_i64()?;
        if !(-1440..=1440).contains(&tz) {
            return Err(Error::Malformed(format!("tz offset out of range: {tz}")));
        }
        let message = r_str_checked(r, limits::MAX_MESSAGE_LEN, "snapshot message")?;
        let workspace = r_opt_str(r, limits::MAX_WORKSPACE_NAME, "workspace name")?;
        let change = r_opt_oid(r)?;
        let goal = r_opt_oid(r)?;
        let extras = r_map(
            r,
            limits::MAX_EXTRAS,
            limits::MAX_EXTRA_KEY,
            limits::MAX_EXTRA_VALUE,
            "snapshot extras",
        )?;
        let s = Snapshot {
            parents,
            root,
            author,
            timestamp_ms,
            tz_offset_min: tz as i16,
            message,
            workspace,
            change,
            goal,
            extras,
        };
        s.validate()?;
        Ok(s)
    }
}

// ---------------------------------------------------------------------------
// Goal / Change / Evidence / Evaluation / Proposal
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum GoalStatus {
    Open = 0,
    InProgress = 1,
    Achieved = 2,
    Abandoned = 3,
}

impl GoalStatus {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => GoalStatus::Open,
            1 => GoalStatus::InProgress,
            2 => GoalStatus::Achieved,
            3 => GoalStatus::Abandoned,
            other => return Err(Error::Malformed(format!("unknown goal status {other}"))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            GoalStatus::Open => "open",
            GoalStatus::InProgress => "in_progress",
            GoalStatus::Achieved => "achieved",
            GoalStatus::Abandoned => "abandoned",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "open" => GoalStatus::Open,
            "in_progress" => GoalStatus::InProgress,
            "achieved" => GoalStatus::Achieved,
            "abandoned" => GoalStatus::Abandoned,
            other => return Err(Error::Invalid(format!("unknown goal status {other:?}"))),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub title: String,
    pub description: String,
    pub creator: ObjectId,
    pub status: GoalStatus,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub extras: BTreeMap<String, String>,
}

impl Goal {
    pub fn validate(&self) -> Result<()> {
        check_title(&self.title)?;
        check_desc(&self.description)?;
        if self.updated_ms < self.created_ms {
            return Err(Error::Malformed(
                "goal updated_ms earlier than created_ms".into(),
            ));
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_str(v, &self.title);
        w_str(v, &self.description);
        w_oid(v, &self.creator);
        w_u8(v, self.status as u8);
        w_i64(v, self.created_ms);
        w_i64(v, self.updated_ms);
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Goal> {
        let g = Goal {
            title: r_str_checked(r, limits::MAX_TITLE_LEN, "goal title")?,
            description: r_str_checked(r, limits::MAX_DESC_LEN, "goal description")?,
            creator: r_oid(r)?,
            status: GoalStatus::from_u8(r.read_u8()?)?,
            created_ms: r.read_i64()?,
            updated_ms: r.read_i64()?,
            extras: r_map(
                r,
                limits::MAX_EXTRAS,
                limits::MAX_EXTRA_KEY,
                limits::MAX_EXTRA_VALUE,
                "goal extras",
            )?,
        };
        g.validate()?;
        Ok(g)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ChangeStatus {
    Draft = 0,
    Tested = 1,
    Proposed = 2,
    Integrated = 3,
    Abandoned = 4,
}

impl ChangeStatus {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => ChangeStatus::Draft,
            1 => ChangeStatus::Tested,
            2 => ChangeStatus::Proposed,
            3 => ChangeStatus::Integrated,
            4 => ChangeStatus::Abandoned,
            other => return Err(Error::Malformed(format!("unknown change status {other}"))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            ChangeStatus::Draft => "draft",
            ChangeStatus::Tested => "tested",
            ChangeStatus::Proposed => "proposed",
            ChangeStatus::Integrated => "integrated",
            ChangeStatus::Abandoned => "abandoned",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "draft" => ChangeStatus::Draft,
            "tested" => ChangeStatus::Tested,
            "proposed" => ChangeStatus::Proposed,
            "integrated" => ChangeStatus::Integrated,
            "abandoned" => ChangeStatus::Abandoned,
            other => return Err(Error::Invalid(format!("unknown change status {other:?}"))),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Snapshot the change was based on.
    pub base: ObjectId,
    /// Snapshot resulting from applying the change.
    pub result: ObjectId,
    pub author: ObjectId,
    /// Optional goal this change addresses. Multiple changes may share a goal
    /// (alternative implementations).
    pub goal: Option<ObjectId>,
    pub title: String,
    pub description: String,
    pub status: ChangeStatus,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Evidence object ids, ascending + unique.
    pub evidence: Vec<ObjectId>,
    pub extras: BTreeMap<String, String>,
}

impl Change {
    pub fn validate(&self) -> Result<()> {
        check_title(&self.title)?;
        check_desc(&self.description)?;
        if self.base == self.result {
            return Err(Error::Malformed(
                "change base and result must differ".into(),
            ));
        }
        check_set_sorted(&self.evidence, "change evidence list")?;
        if self.updated_ms < self.created_ms {
            return Err(Error::Malformed(
                "change updated_ms earlier than created_ms".into(),
            ));
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_oid(v, &self.base);
        w_oid(v, &self.result);
        w_oid(v, &self.author);
        w_opt_oid(v, &self.goal);
        w_str(v, &self.title);
        w_str(v, &self.description);
        w_u8(v, self.status as u8);
        w_i64(v, self.created_ms);
        w_i64(v, self.updated_ms);
        w_oid_list(v, &self.evidence);
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Change> {
        let c = Change {
            base: r_oid(r)?,
            result: r_oid(r)?,
            author: r_oid(r)?,
            goal: r_opt_oid(r)?,
            title: r_str_checked(r, limits::MAX_TITLE_LEN, "change title")?,
            description: r_str_checked(r, limits::MAX_DESC_LEN, "change description")?,
            status: ChangeStatus::from_u8(r.read_u8()?)?,
            created_ms: r.read_i64()?,
            updated_ms: r.read_i64()?,
            evidence: r_oid_list(r, limits::MAX_LIST, "change evidence")?,
            extras: r_map(
                r,
                limits::MAX_EXTRAS,
                limits::MAX_EXTRA_KEY,
                limits::MAX_EXTRA_VALUE,
                "change extras",
            )?,
        };
        c.validate()?;
        Ok(c)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Verdict {
    Pass = 0,
    Fail = 1,
    Inconclusive = 2,
    NotApplicable = 3,
}

impl Verdict {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => Verdict::Pass,
            1 => Verdict::Fail,
            2 => Verdict::Inconclusive,
            3 => Verdict::NotApplicable,
            other => return Err(Error::Malformed(format!("unknown verdict {other}"))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Inconclusive => "inconclusive",
            Verdict::NotApplicable => "not_applicable",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pass" => Verdict::Pass,
            "fail" => Verdict::Fail,
            "inconclusive" => Verdict::Inconclusive,
            "not_applicable" => Verdict::NotApplicable,
            other => return Err(Error::Invalid(format!("unknown verdict {other:?}"))),
        })
    }
}

/// Evidence: a recorded observation about a change (test results, scans,
/// benchmarks, reviews, agent evaluations...).
///
/// HONESTY RULE (invariant): `deterministic == true` means the evidence was
/// produced by a tool run recorded by the runner itself. Claims that come from
/// a human or an AI *opinion* MUST set `deterministic == false`; UIs and the
/// evaluator layer render them distinctly. The core never treats a
/// user-supplied string as a verified "pass".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub producer: ObjectId,
    /// Target change (optional: evidence may target a snapshot directly).
    pub target: Option<ObjectId>,
    /// Free-form but conventional: unit_test, integration_test, typecheck,
    /// lint, security_scan, benchmark, browser_test, build, review,
    /// agent_evaluation, reproducibility, ...
    pub kind: String,
    pub verdict: Verdict,
    pub deterministic: bool,
    pub tool: String,
    pub tool_version: String,
    pub command: String,
    /// Optional blob with raw output.
    pub output: Option<ObjectId>,
    /// Sorted string metrics (e.g. "duration_ms" -> "1234").
    pub metrics: BTreeMap<String, String>,
    pub created_ms: i64,
    pub extras: BTreeMap<String, String>,
}

impl Evidence {
    pub fn validate(&self) -> Result<()> {
        check_text(&self.kind, false, "evidence kind")?;
        if self.kind.is_empty() || self.kind.len() > limits::MAX_TOOL_LEN {
            return Err(Error::Invalid("evidence kind length invalid".into()));
        }
        check_text(&self.tool, false, "evidence tool")?;
        check_text(&self.tool_version, false, "evidence tool_version")?;
        if self.command.len() > limits::MAX_COMMAND_LEN {
            return Err(Error::Limit("evidence command too long".into()));
        }
        check_text(&self.command, true, "evidence command")?;
        if self.metrics.len() > limits::MAX_METRICS {
            return Err(Error::Limit("too many evidence metrics".into()));
        }
        for (k, v) in &self.metrics {
            if k.len() > limits::MAX_EXTRA_KEY || v.len() > limits::MAX_EXTRA_VALUE {
                return Err(Error::Limit("evidence metric too large".into()));
            }
            check_text(k, false, "metric key")?;
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_oid(v, &self.producer);
        w_opt_oid(v, &self.target);
        w_str(v, &self.kind);
        w_u8(v, self.verdict as u8);
        w_bool(v, self.deterministic);
        w_str(v, &self.tool);
        w_str(v, &self.tool_version);
        w_str(v, &self.command);
        w_opt_oid(v, &self.output);
        w_map(v, &self.metrics);
        w_i64(v, self.created_ms);
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Evidence> {
        let e = Evidence {
            producer: r_oid(r)?,
            target: r_opt_oid(r)?,
            kind: r_str_checked(r, limits::MAX_TOOL_LEN, "evidence kind")?,
            verdict: Verdict::from_u8(r.read_u8()?)?,
            deterministic: r.read_bool()?,
            tool: r_str_checked(r, limits::MAX_TOOL_LEN, "evidence tool")?,
            tool_version: r_str_checked(r, limits::MAX_TOOL_LEN, "evidence tool_version")?,
            command: r_str_checked(r, limits::MAX_COMMAND_LEN, "evidence command")?,
            output: r_opt_oid(r)?,
            metrics: r_map(
                r,
                limits::MAX_METRICS,
                limits::MAX_EXTRA_KEY,
                limits::MAX_EXTRA_VALUE,
                "evidence metrics",
            )?,
            created_ms: r.read_i64()?,
            extras: r_map(
                r,
                limits::MAX_EXTRAS,
                limits::MAX_EXTRA_KEY,
                limits::MAX_EXTRA_VALUE,
                "evidence extras",
            )?,
        };
        e.validate()?;
        Ok(e)
    }
}

/// Evaluation: an aggregate judgement over a change, composed of dimensions.
/// `ai_generated == true` marks AI opinions; the system never mixes them into
/// deterministic verdicts without the flag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evaluation {
    pub target: ObjectId,
    pub evaluator: ObjectId,
    pub ai_generated: bool,
    pub verdict: Verdict,
    /// (dimension, verdict, note), ascending by dimension, unique.
    pub dimensions: Vec<(String, Verdict, String)>,
    pub created_ms: i64,
    pub extras: BTreeMap<String, String>,
}

impl Evaluation {
    pub fn validate(&self) -> Result<()> {
        if self.dimensions.len() > limits::MAX_DIMENSIONS {
            return Err(Error::Limit("too many evaluation dimensions".into()));
        }
        let mut last: Option<&str> = None;
        for (name, _, note) in &self.dimensions {
            check_text(name, false, "dimension name")?;
            check_text(note, true, "dimension note")?;
            if name.is_empty() || name.len() > limits::MAX_EXTRA_KEY {
                return Err(Error::Invalid("dimension name length invalid".into()));
            }
            if note.len() > limits::MAX_EXTRA_VALUE {
                return Err(Error::Limit("dimension note too long".into()));
            }
            if let Some(l) = last {
                if name.as_str() <= l {
                    return Err(Error::Malformed(
                        "evaluation dimensions not strictly ascending".into(),
                    ));
                }
            }
            last = Some(name);
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_oid(v, &self.target);
        w_oid(v, &self.evaluator);
        w_bool(v, self.ai_generated);
        w_u8(v, self.verdict as u8);
        w_u64(v, self.dimensions.len() as u64);
        for (n, verdict, note) in &self.dimensions {
            w_str(v, n);
            w_u8(v, *verdict as u8);
            w_str(v, note);
        }
        w_i64(v, self.created_ms);
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Evaluation> {
        let target = r_oid(r)?;
        let evaluator = r_oid(r)?;
        let ai_generated = r.read_bool()?;
        let verdict = Verdict::from_u8(r.read_u8()?)?;
        let n = r.read_u64()? as usize;
        if n > limits::MAX_DIMENSIONS {
            return Err(Error::Limit("too many evaluation dimensions".into()));
        }
        let mut dimensions = Vec::with_capacity(n);
        for _ in 0..n {
            let name = r_str_checked(r, limits::MAX_EXTRA_KEY, "dimension name")?;
            let dv = Verdict::from_u8(r.read_u8()?)?;
            let note = r_str_checked(r, limits::MAX_EXTRA_VALUE, "dimension note")?;
            dimensions.push((name, dv, note));
        }
        let e = Evaluation {
            target,
            evaluator,
            ai_generated,
            verdict,
            dimensions,
            created_ms: r.read_i64()?,
            extras: r_map(
                r,
                limits::MAX_EXTRAS,
                limits::MAX_EXTRA_KEY,
                limits::MAX_EXTRA_VALUE,
                "evaluation extras",
            )?,
        };
        e.validate()?;
        Ok(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum ProposalState {
    Open = 0,
    Approved = 1,
    Rejected = 2,
    Integrated = 3,
    Closed = 4,
}

impl ProposalState {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0 => ProposalState::Open,
            1 => ProposalState::Approved,
            2 => ProposalState::Rejected,
            3 => ProposalState::Integrated,
            4 => ProposalState::Closed,
            other => return Err(Error::Malformed(format!("unknown proposal state {other}"))),
        })
    }
    pub fn name(&self) -> &'static str {
        match self {
            ProposalState::Open => "open",
            ProposalState::Approved => "approved",
            ProposalState::Rejected => "rejected",
            ProposalState::Integrated => "integrated",
            ProposalState::Closed => "closed",
        }
    }
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "open" => ProposalState::Open,
            "approved" => ProposalState::Approved,
            "rejected" => ProposalState::Rejected,
            "integrated" => ProposalState::Integrated,
            "closed" => ProposalState::Closed,
            other => return Err(Error::Invalid(format!("unknown proposal state {other:?}"))),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub change: ObjectId,
    pub title: String,
    pub rationale: String,
    pub author: ObjectId,
    /// Base snapshot the proposal expects to integrate onto.
    pub base: ObjectId,
    pub evidence: Vec<ObjectId>,
    pub depends_on: Vec<ObjectId>,
    /// (approver actor, timestamp), ascending by actor then time, unique.
    pub approvals: Vec<(ObjectId, i64)>,
    pub state: ProposalState,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub extras: BTreeMap<String, String>,
}

impl Proposal {
    pub fn validate(&self) -> Result<()> {
        check_title(&self.title)?;
        check_desc(&self.rationale)?;
        check_set_sorted(&self.evidence, "proposal evidence list")?;
        check_set_sorted(&self.depends_on, "proposal depends_on list")?;
        if self.approvals.len() > limits::MAX_LIST {
            return Err(Error::Limit("too many approvals".into()));
        }
        for w in self.approvals.windows(2) {
            if (w[0].0, w[0].1) >= (w[1].0, w[1].1) {
                return Err(Error::Malformed(
                    "proposal approvals not strictly ascending".into(),
                ));
            }
        }
        if self.updated_ms < self.created_ms {
            return Err(Error::Malformed(
                "proposal updated_ms earlier than created_ms".into(),
            ));
        }
        check_extras(&self.extras)?;
        Ok(())
    }
    fn body(&self, v: &mut Vec<u8>) {
        w_oid(v, &self.change);
        w_str(v, &self.title);
        w_str(v, &self.rationale);
        w_oid(v, &self.author);
        w_oid(v, &self.base);
        w_oid_list(v, &self.evidence);
        w_oid_list(v, &self.depends_on);
        w_u64(v, self.approvals.len() as u64);
        for (a, t) in &self.approvals {
            w_oid(v, a);
            w_i64(v, *t);
        }
        w_u8(v, self.state as u8);
        w_i64(v, self.created_ms);
        w_i64(v, self.updated_ms);
        w_map(v, &self.extras);
    }
    fn parse(r: &mut Reader<'_>) -> Result<Proposal> {
        let change = r_oid(r)?;
        let title = r_str_checked(r, limits::MAX_TITLE_LEN, "proposal title")?;
        let rationale = r_str_checked(r, limits::MAX_DESC_LEN, "proposal rationale")?;
        let author = r_oid(r)?;
        let base = r_oid(r)?;
        let evidence = r_oid_list(r, limits::MAX_LIST, "proposal evidence")?;
        let depends_on = r_oid_list(r, limits::MAX_LIST, "proposal depends_on")?;
        let na = r.read_u64()? as usize;
        if na > limits::MAX_LIST {
            return Err(Error::Limit("too many approvals".into()));
        }
        // min size per approval = 32-byte oid + 1-byte varint timestamp
        if na
            .checked_mul(33)
            .map(|sz| sz > r.remaining())
            .unwrap_or(true)
        {
            return Err(Error::Malformed(
                "approval count exceeds remaining input".into(),
            ));
        }
        let mut approvals = Vec::with_capacity(na);
        for _ in 0..na {
            approvals.push((r_oid(r)?, r.read_i64()?));
        }
        let p = Proposal {
            change,
            title,
            rationale,
            author,
            base,
            evidence,
            depends_on,
            approvals,
            state: ProposalState::from_u8(r.read_u8()?)?,
            created_ms: r.read_i64()?,
            updated_ms: r.read_i64()?,
            extras: r_map(
                r,
                limits::MAX_EXTRAS,
                limits::MAX_EXTRA_KEY,
                limits::MAX_EXTRA_VALUE,
                "proposal extras",
            )?,
        };
        p.validate()?;
        Ok(p)
    }
}

fn check_title(t: &str) -> Result<()> {
    check_text(t, false, "title")?;
    if t.is_empty() || t.len() > limits::MAX_TITLE_LEN {
        return Err(Error::Invalid(format!(
            "title must be 1..={} bytes (got {})",
            limits::MAX_TITLE_LEN,
            t.len()
        )));
    }
    Ok(())
}

fn check_desc(d: &str) -> Result<()> {
    check_text(d, true, "description")?;
    if d.len() > limits::MAX_DESC_LEN {
        return Err(Error::Limit("description too long".into()));
    }
    Ok(())
}

fn check_extras(m: &BTreeMap<String, String>) -> Result<()> {
    if m.len() > limits::MAX_EXTRAS {
        return Err(Error::Limit("too many extras entries".into()));
    }
    for (k, v) in m {
        if k.is_empty() || k.len() > limits::MAX_EXTRA_KEY {
            return Err(Error::Invalid("extras key length invalid".into()));
        }
        if v.len() > limits::MAX_EXTRA_VALUE {
            return Err(Error::Limit("extras value too long".into()));
        }
        check_text(k, false, "extras key")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Object enum: canonical encoding + identity + decoding
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Object {
    Blob(Vec<u8>),
    Tree(Tree),
    Snapshot(Snapshot),
    Actor(Actor),
    Goal(Goal),
    Change(Change),
    Evidence(Evidence),
    Evaluation(Evaluation),
    Proposal(Proposal),
}

impl Object {
    pub fn type_tag(&self) -> ObjectType {
        match self {
            Object::Blob(_) => ObjectType::Blob,
            Object::Tree(_) => ObjectType::Tree,
            Object::Snapshot(_) => ObjectType::Snapshot,
            Object::Actor(_) => ObjectType::Actor,
            Object::Goal(_) => ObjectType::Goal,
            Object::Change(_) => ObjectType::Change,
            Object::Evidence(_) => ObjectType::Evidence,
            Object::Evaluation(_) => ObjectType::Evaluation,
            Object::Proposal(_) => ObjectType::Proposal,
        }
    }

    /// Canonical bytes: type_tag || varint(body_len) || body.
    pub fn canonical(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            Object::Blob(data) => body.extend_from_slice(data),
            Object::Tree(t) => t.body(&mut body),
            Object::Snapshot(s) => s.body(&mut body),
            Object::Actor(a) => a.body(&mut body),
            Object::Goal(g) => g.body(&mut body),
            Object::Change(c) => c.body(&mut body),
            Object::Evidence(e) => e.body(&mut body),
            Object::Evaluation(e) => e.body(&mut body),
            Object::Proposal(p) => p.body(&mut body),
        }
        let mut out = Vec::with_capacity(body.len() + 11);
        out.push(self.type_tag() as u8);
        varint::write_u64(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
        out
    }

    /// Invariant: identical content ⇒ identical identity.
    pub fn id(&self) -> ObjectId {
        ObjectId::compute(&self.canonical())
    }

    /// Decode canonical bytes; rejects unknown tags, trailing data, and any
    /// structural violation. Never panics on malformed input.
    pub fn from_canonical(bytes: &[u8]) -> Result<Object> {
        let mut r = Reader::new(bytes);
        let tag = ObjectType::from_u8(r.read_u8()?)?;
        let body_len = r.read_u64()? as usize;
        if body_len > r.remaining() {
            return Err(Error::Malformed(format!(
                "declared body length {body_len} exceeds input {}",
                r.remaining()
            )));
        }
        let body = r.read_bytes(body_len)?;
        if !r.is_empty() {
            return Err(Error::Malformed(
                "trailing bytes after canonical object".into(),
            ));
        }
        // Blob bodies are opaque bytes: no sub-structure, no trailing check.
        if tag == ObjectType::Blob {
            return Ok(Object::Blob(body.to_vec()));
        }
        let mut br = Reader::new(body);
        let obj = match tag {
            ObjectType::Tree => Object::Tree(Tree::parse(&mut br)?),
            ObjectType::Snapshot => Object::Snapshot(Snapshot::parse(&mut br)?),
            ObjectType::Actor => Object::Actor(Actor::parse(&mut br)?),
            ObjectType::Goal => Object::Goal(Goal::parse(&mut br)?),
            ObjectType::Change => Object::Change(Change::parse(&mut br)?),
            ObjectType::Evidence => Object::Evidence(Evidence::parse(&mut br)?),
            ObjectType::Evaluation => Object::Evaluation(Evaluation::parse(&mut br)?),
            ObjectType::Proposal => Object::Proposal(Proposal::parse(&mut br)?),
            ObjectType::Blob => unreachable!("handled above"),
        };
        if !br.is_empty() {
            return Err(Error::Malformed(format!(
                "trailing bytes in {} body",
                tag.name()
            )));
        }
        Ok(obj)
    }

    /// Convenience accessors.
    pub fn as_tree(&self) -> Result<&Tree> {
        match self {
            Object::Tree(t) => Ok(t),
            other => Err(Error::Invalid(format!(
                "expected tree, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_snapshot(&self) -> Result<&Snapshot> {
        match self {
            Object::Snapshot(s) => Ok(s),
            other => Err(Error::Invalid(format!(
                "expected snapshot, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_blob(&self) -> Result<&[u8]> {
        match self {
            Object::Blob(b) => Ok(b),
            other => Err(Error::Invalid(format!(
                "expected blob, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_change(&self) -> Result<&Change> {
        match self {
            Object::Change(c) => Ok(c),
            other => Err(Error::Invalid(format!(
                "expected change, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_goal(&self) -> Result<&Goal> {
        match self {
            Object::Goal(g) => Ok(g),
            other => Err(Error::Invalid(format!(
                "expected goal, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_evidence(&self) -> Result<&Evidence> {
        match self {
            Object::Evidence(e) => Ok(e),
            other => Err(Error::Invalid(format!(
                "expected evidence, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_proposal(&self) -> Result<&Proposal> {
        match self {
            Object::Proposal(p) => Ok(p),
            other => Err(Error::Invalid(format!(
                "expected proposal, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_actor(&self) -> Result<&Actor> {
        match self {
            Object::Actor(a) => Ok(a),
            other => Err(Error::Invalid(format!(
                "expected actor, found {}",
                other.type_tag().name()
            ))),
        }
    }
    pub fn as_evaluation(&self) -> Result<&Evaluation> {
        match self {
            Object::Evaluation(e) => Ok(e),
            other => Err(Error::Invalid(format!(
                "expected evaluation, found {}",
                other.type_tag().name()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        Actor {
            kind: ActorKind::Human,
            id: "human:alice".into(),
            display_name: "Alice".into(),
            tool: "cli".into(),
            tool_version: "1.0".into(),
            pubkey: None,
            extras: BTreeMap::new(),
        }
    }

    fn oid(b: u8) -> ObjectId {
        ObjectId::from_bytes([b; 32])
    }

    #[test]
    fn blob_identity_deterministic() {
        let a = Object::Blob(b"hello".to_vec());
        let b = Object::Blob(b"hello".to_vec());
        assert_eq!(a.id(), b.id());
        let decoded = Object::from_canonical(&a.canonical()).unwrap();
        assert_eq!(decoded, a);
        assert_eq!(decoded.id(), a.id());
    }

    #[test]
    fn tree_roundtrip_and_ordering() {
        let t = Tree::new(vec![
            TreeEntry {
                name: "b.txt".into(),
                mode: EntryMode::File,
                oid: oid(2),
            },
            TreeEntry {
                name: "a.txt".into(),
                mode: EntryMode::Executable,
                oid: oid(1),
            },
        ])
        .unwrap();
        assert_eq!(t.entries[0].name, "a.txt");
        let o = Object::Tree(t.clone());
        let d = Object::from_canonical(&o.canonical()).unwrap();
        assert_eq!(d, o);
        // duplicate names collapse
        let t2 = Tree::new(vec![
            TreeEntry {
                name: "x".into(),
                mode: EntryMode::File,
                oid: oid(1),
            },
            TreeEntry {
                name: "x".into(),
                mode: EntryMode::File,
                oid: oid(2),
            },
        ]);
        assert!(t2.is_err() || t2.unwrap().entries.len() == 1);
    }

    #[test]
    fn tree_rejects_bad_names() {
        for name in ["", ".", "..", "a/b", "a\0b"] {
            let t = Tree {
                entries: vec![TreeEntry {
                    name: name.into(),
                    mode: EntryMode::File,
                    oid: oid(1),
                }],
            };
            assert!(t.validate().is_err(), "name {name:?} must be rejected");
        }
    }

    #[test]
    fn snapshot_roundtrip() {
        let s = Snapshot {
            parents: vec![oid(9)],
            root: oid(1),
            author: oid(2),
            timestamp_ms: 1_700_000_000_123,
            tz_offset_min: 330,
            message: "first\nsecond line\n".into(),
            workspace: Some("ws-agent-1".into()),
            change: Some(oid(3)),
            goal: Some(oid(4)),
            extras: BTreeMap::from([("k".to_string(), "v".to_string())]),
        };
        s.validate().unwrap();
        let o = Object::Snapshot(s.clone());
        let d = Object::from_canonical(&o.canonical()).unwrap();
        assert_eq!(d, o);
    }

    #[test]
    fn all_types_roundtrip() {
        let objs = vec![
            Object::Actor(actor()),
            Object::Goal(Goal {
                title: "Add OAuth".into(),
                description: "d".into(),
                creator: oid(1),
                status: GoalStatus::Open,
                created_ms: 5,
                updated_ms: 6,
                extras: BTreeMap::new(),
            }),
            Object::Change(Change {
                base: oid(1),
                result: oid(2),
                author: oid(3),
                goal: Some(oid(4)),
                title: "t".into(),
                description: "d".into(),
                status: ChangeStatus::Draft,
                created_ms: 1,
                updated_ms: 2,
                evidence: vec![oid(5), oid(6)],
                extras: BTreeMap::new(),
            }),
            Object::Evidence(Evidence {
                producer: oid(1),
                target: Some(oid(2)),
                kind: "unit_test".into(),
                verdict: Verdict::Pass,
                deterministic: true,
                tool: "cargo".into(),
                tool_version: "1.80".into(),
                command: "cargo test".into(),
                output: Some(oid(3)),
                metrics: BTreeMap::from([("duration_ms".into(), "42".into())]),
                created_ms: 7,
                extras: BTreeMap::new(),
            }),
            Object::Evaluation(Evaluation {
                target: oid(1),
                evaluator: oid(2),
                ai_generated: true,
                verdict: Verdict::Pass,
                dimensions: vec![("tests".into(), Verdict::Pass, "all green".into())],
                created_ms: 3,
                extras: BTreeMap::new(),
            }),
            Object::Proposal(Proposal {
                change: oid(1),
                title: "t".into(),
                rationale: "r".into(),
                author: oid(2),
                base: oid(3),
                evidence: vec![oid(4)],
                depends_on: vec![],
                approvals: vec![(oid(5), 9)],
                state: ProposalState::Open,
                created_ms: 1,
                updated_ms: 2,
                extras: BTreeMap::new(),
            }),
        ];
        for o in objs {
            let d = Object::from_canonical(&o.canonical()).unwrap();
            assert_eq!(d, o, "roundtrip failed for {}", o.type_tag().name());
            assert_eq!(d.id(), o.id());
        }
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Object::from_canonical(&[]).is_err());
        assert!(Object::from_canonical(&[200]).is_err()); // unknown tag
        assert!(Object::from_canonical(&[1, 5, 1, 2]).is_err()); // short body
        assert!(Object::from_canonical(&[1, 1, b'x', b'y']).is_err()); // trailing
                                                                       // tree with unsorted entries
        let mut o = Object::Tree(Tree {
            entries: vec![
                TreeEntry {
                    name: "z".into(),
                    mode: EntryMode::File,
                    oid: oid(1),
                },
                TreeEntry {
                    name: "a".into(),
                    mode: EntryMode::File,
                    oid: oid(2),
                },
            ],
        });
        let bytes = o.canonical();
        assert!(Object::from_canonical(&bytes).is_err());
        o = Object::Snapshot(Snapshot {
            parents: vec![],
            root: oid(1),
            author: oid(2),
            timestamp_ms: 0,
            tz_offset_min: 5000, // out of range
            message: String::new(),
            workspace: None,
            change: None,
            goal: None,
            extras: BTreeMap::new(),
        });
        assert!(Object::from_canonical(&o.canonical()).is_err());
    }

    #[test]
    fn change_requires_distinct_base_result() {
        let c = Change {
            base: oid(1),
            result: oid(1),
            author: oid(2),
            goal: None,
            title: "t".into(),
            description: String::new(),
            status: ChangeStatus::Draft,
            created_ms: 0,
            updated_ms: 0,
            evidence: vec![],
            extras: BTreeMap::new(),
        };
        assert!(c.validate().is_err());
    }
}
