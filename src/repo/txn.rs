//! Write-ahead transaction engine (STORAGE_FORMAT.md §6).
//!
//! All ref mutations and small metadata-file writes in NewGit go through
//! this engine, even single-ref updates. Protocol:
//!
//! 1. acquire the global txn lock,
//! 2. redo any incomplete journals from previous crashes (recovery first),
//! 3. check every CAS precondition; if any fails, nothing is written,
//! 4. write a journal (RUNNING) atomically and fsync it,
//! 5. apply: ref/file renames (each atomic + idempotent) → reflog appends,
//! 6. rewrite the journal state to COMPLETE, fsync,
//! 7. release the lock.
//!
//! Crash anywhere ⇒ next `recover()` redoes step 5–6 from the journal.
//! Because application is idempotent (renames to fixed contents, reflog
//! lines tagged with the txn id and deduplicated on read), redo is safe
//! and yields all-or-nothing semantics across process death.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::object::ObjectId;
use crate::repo::config::Limits;
use crate::repo::refs::check_ref_name_system;
use crate::util::{base64, fault, fsx};

static TXN_CTR: AtomicU64 = AtomicU64::new(0);

/// Compare-and-swap expectation for a ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cas {
    /// Unconditional write (used by recovery and admin paths).
    Any,
    /// Value must equal `Some(oid)`, or (`None`) the ref must not exist.
    Exactly(Option<ObjectId>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefLogEntry {
    pub actor: Option<ObjectId>,
    pub ts_ms: i64,
    pub message: String,
}

impl RefLogEntry {
    pub fn system(message: impl Into<String>) -> RefLogEntry {
        RefLogEntry {
            actor: None,
            ts_ms: now_ms(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum TxnOp {
    Ref {
        name: String,
        cas: Cas,
        /// `None` deletes the ref.
        new: Option<ObjectId>,
        log: RefLogEntry,
    },
    /// Write a small metadata file relative to `.newgit/` (e.g. `HEAD`).
    File { rel: String, data: Vec<u8> },
    /// Delete a small metadata file relative to `.newgit/` (idempotent).
    FileDelete { rel: String },
}

#[derive(Clone, Debug, Default)]
pub struct TxnReport {
    pub txn_id: String,
    pub refs_updated: Vec<String>,
    pub files_written: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    pub redone: Vec<String>,
    pub quarantined: Vec<String>,
    /// Terminal-state journals checkpoint-deleted during this pass.
    pub cleaned: usize,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn txn_dir(ng: &Path) -> PathBuf {
    ng.join("txn")
}

fn acquire_global_lock(ng: &Path, limits: &Limits) -> Result<fsx::FileLock> {
    fsx::ensure_dir(&txn_dir(ng))?;
    fsx::FileLock::acquire(
        &txn_dir(ng).join("LOCK"),
        Duration::from_millis(limits.lock_wait_ms),
        Duration::from_secs(limits.lock_stale_s),
    )
}

/// An exclusive, recovered view of canonical repository state.
///
/// Hold this guard for the entire traversal of refs, HEAD, and reachable
/// objects. It serializes with ref/metadata transactions and GC; acquiring it
/// first replays any committed-but-partially-applied journal.
#[derive(Debug)]
pub struct SnapshotReadGuard {
    _lock: fsx::FileLock,
}

impl SnapshotReadGuard {
    pub fn acquire(ng: &Path, limits: &Limits) -> Result<Self> {
        let lock = acquire_global_lock(ng, limits)?;
        recover_locked(ng, limits)?;
        Ok(Self { _lock: lock })
    }
}

/// Initialize the default HEAD only if no concurrent initializer or
/// transaction has already created it. The atomic one-file write is protected
/// by the same lock used by repository readers and writers.
pub(crate) fn initialize_head_if_missing(ng: &Path, limits: &Limits) -> Result<()> {
    let _lock = acquire_global_lock(ng, limits)?;
    recover_locked(ng, limits)?;
    let path = ng.join("HEAD");
    if !path.exists() {
        fsx::atomic_write(
            &path,
            format!("ref: {}\n", crate::repo::DEFAULT_BRANCH).as_bytes(),
        )?;
    }
    Ok(())
}

fn journal_path(ng: &Path, id: &str) -> PathBuf {
    txn_dir(ng).join(format!("{id}.journal"))
}

// ---------------------------------------------------------------------------
// Journal serialization (text, total parser)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Journal {
    pub id: String,
    pub state: JournalState,
    pub ops: Vec<TxnOp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalState {
    Running,
    Complete,
    Recovered,
}

impl Journal {
    pub fn new_running(ops: Vec<TxnOp>) -> Journal {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let ctr = TXN_CTR.fetch_add(1, Ordering::Relaxed);
        Journal {
            id: format!("{}-{}-{}", ts, std::process::id(), ctr),
            state: JournalState::Running,
            ops,
        }
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("NEWGIT-TXN v1\n");
        out.push_str(&format!("txn={}\n", self.id));
        out.push_str(&format!(
            "state={}\n",
            match self.state {
                JournalState::Running => "RUNNING",
                JournalState::Complete => "COMPLETE",
                JournalState::Recovered => "RECOVERED",
            }
        ));
        for op in &self.ops {
            match op {
                TxnOp::Ref {
                    name,
                    cas: _,
                    new,
                    log,
                } => {
                    // `old` is not stored: the journal records *final* state
                    // (redo semantics); CAS expectations were validated
                    // before the journal was written.
                    out.push_str(&format!(
                        "REF {} ZERO {} {} {} {}\n",
                        base64::encode(name.as_bytes()),
                        match new {
                            Some(o) => o.to_hex(),
                            None => "ZERO".to_string(),
                        },
                        match log.actor {
                            Some(a) => a.to_hex(),
                            None => "ZERO".to_string(),
                        },
                        log.ts_ms,
                        base64::encode(log.message.as_bytes()),
                    ));
                }
                TxnOp::File { rel, data } => {
                    let mut h = Sha256::new();
                    h.update(data);
                    let digest = crate::util::hex::encode(&h.finalize());
                    out.push_str(&format!(
                        "FILE {} {} {}\n",
                        base64::encode(rel.as_bytes()),
                        digest,
                        base64::encode(data),
                    ));
                }
                TxnOp::FileDelete { rel } => {
                    out.push_str(&format!("FDEL {}\n", base64::encode(rel.as_bytes()),));
                }
            }
        }
        out.push_str("END\n");
        out
    }

    pub fn parse(text: &str) -> Result<Journal> {
        let mut lines = text.lines();
        let header = lines
            .next()
            .ok_or_else(|| Error::Malformed("empty journal".into()))?;
        if header != "NEWGIT-TXN v1" {
            return Err(Error::Malformed(format!("bad journal header: {header:?}")));
        }
        let mut id = None;
        let mut state = None;
        let mut ops = Vec::new();
        let mut saw_end = false;
        for line in lines {
            if line == "END" {
                saw_end = true;
                break;
            }
            if let Some(v) = line.strip_prefix("txn=") {
                if v.is_empty() || v.len() > 128 {
                    return Err(Error::Malformed("bad journal txn id".into()));
                }
                id = Some(v.to_string());
                continue;
            }
            if let Some(v) = line.strip_prefix("state=") {
                state = Some(match v {
                    "RUNNING" => JournalState::Running,
                    "COMPLETE" => JournalState::Complete,
                    "RECOVERED" => JournalState::Recovered,
                    other => return Err(Error::Malformed(format!("bad journal state {other:?}"))),
                });
                continue;
            }
            if let Some(rest) = line.strip_prefix("REF ") {
                // fields: name-b64 old new actor ts_ms msg-b64
                let f: Vec<&str> = rest.split(' ').collect();
                if f.len() != 6 {
                    return Err(Error::Malformed(format!(
                        "bad REF op arity {} (expected 6 fields)",
                        f.len()
                    )));
                }
                let name = String::from_utf8(base64::decode(f[0])?)
                    .map_err(|_| Error::Malformed("REF name not utf-8".into()))?;
                check_ref_name_system(&name)?;
                if f[1] != "ZERO" {
                    return Err(Error::Malformed(
                        "journal REF old field must be ZERO (final-state journals)".into(),
                    ));
                }
                let new = parse_oid_field(f[2])?;
                let actor = parse_oid_field(f[3])?;
                let ts_ms: i64 = f[4]
                    .parse()
                    .map_err(|_| Error::Malformed("bad REF timestamp".into()))?;
                let message = String::from_utf8(base64::decode(f[5])?)
                    .map_err(|_| Error::Malformed("REF message not utf-8".into()))?;
                ops.push(TxnOp::Ref {
                    name,
                    cas: Cas::Any,
                    new,
                    log: RefLogEntry {
                        actor,
                        ts_ms,
                        message,
                    },
                });
                continue;
            }
            if let Some(rest) = line.strip_prefix("FILE ") {
                let f: Vec<&str> = rest.split(' ').collect();
                if f.len() != 3 {
                    return Err(Error::Malformed("bad FILE op arity".into()));
                }
                let rel = String::from_utf8(base64::decode(f[0])?)
                    .map_err(|_| Error::Malformed("FILE rel not utf-8".into()))?;
                fsx::check_rel_path(&rel, 255)?;
                let data = base64::decode(f[2])?;
                let mut h = Sha256::new();
                h.update(&data);
                let digest = crate::util::hex::encode(&h.finalize());
                if digest != f[1] {
                    return Err(Error::Malformed(
                        "FILE op digest mismatch (journal corrupt)".into(),
                    ));
                }
                ops.push(TxnOp::File { rel, data });
                continue;
            }
            if let Some(rest) = line.strip_prefix("FDEL ") {
                let rel = String::from_utf8(base64::decode(rest.trim())?)
                    .map_err(|_| Error::Malformed("FDEL rel not utf-8".into()))?;
                fsx::check_rel_path(&rel, 255)?;
                if rel.starts_with("objects/") || rel.starts_with("txn/") {
                    return Err(Error::Malformed(format!("FDEL may not target {rel:?}")));
                }
                ops.push(TxnOp::FileDelete { rel });
                continue;
            }
            return Err(Error::Malformed(format!("bad journal line: {line:?}")));
        }
        if !saw_end {
            return Err(Error::Malformed("journal missing END".into()));
        }
        let id = id.ok_or_else(|| Error::Malformed("journal missing txn id".into()))?;
        let state = state.ok_or_else(|| Error::Malformed("journal missing state".into()))?;
        Ok(Journal { id, state, ops })
    }
}

fn parse_oid_field(s: &str) -> Result<Option<ObjectId>> {
    if s == "ZERO" {
        Ok(None)
    } else {
        Ok(Some(ObjectId::from_hex(s)?))
    }
}

// ---------------------------------------------------------------------------
// Execution + recovery
// ---------------------------------------------------------------------------

/// Execute ops as one atomic transaction (acquires the global lock).
pub fn execute(ng: &Path, ops: Vec<TxnOp>, limits: &Limits) -> Result<TxnReport> {
    execute_with_precommit(ng, ops, limits, || Ok(()))
}

/// Execute a ref/file transaction, running `before_journal` only after its CAS
/// checks pass while the global transaction lock is held. This lets callers
/// stage immutable objects before making their refs reachable without copying
/// them when a concurrent ref update has already made the operation stale.
pub fn execute_with_precommit<F>(
    ng: &Path,
    ops: Vec<TxnOp>,
    limits: &Limits,
    before_journal: F,
) -> Result<TxnReport>
where
    F: FnOnce() -> Result<()>,
{
    validate_ops(&ops, limits)?;
    let lock = acquire_global_lock(ng, limits)?;
    // Recovery first: a previous crash must not be masked by new work.
    recover_locked(ng, limits)?;

    // CAS preconditions (under the global lock ⇒ stable reads).
    let mut refs_updated = Vec::new();
    let mut files_written = Vec::new();
    for op in &ops {
        if let TxnOp::Ref { name, cas, .. } = op {
            let current = read_ref_raw(ng, name)?;
            match cas {
                Cas::Any => {}
                Cas::Exactly(expected) => {
                    if &current != expected {
                        return Err(Error::CasFailed(format!(
                            "ref {name}: expected {:?}, found {:?}",
                            expected.map(|o| o.short()),
                            current.map(|o| o.short())
                        )));
                    }
                }
            }
            refs_updated.push(name.clone());
        }
        if let TxnOp::File { rel, .. } = op {
            files_written.push(rel.clone());
        }
    }
    before_journal()?;
    fault::fault("txn:before_journal")?;

    let journal = Journal::new_running(ops);
    let jpath = journal_path(ng, &journal.id);
    fsx::atomic_write(&jpath, journal.serialize().as_bytes())?;
    let _ = fault::fault_action("txn:after_journal");

    apply_journal(ng, &journal, limits)?;

    let _ = fault::fault_action("txn:before_complete");
    let mut done = journal.clone();
    done.state = JournalState::Complete;
    fsx::atomic_write(&jpath, done.serialize().as_bytes())?;
    let _ = fault::fault_action("txn:after_complete");

    // Checkpoint: the txn is durable and applied — remove its journal.
    // A crash before this delete is harmless: recovery deletes journals
    // in terminal states. Without checkpointing the txn dir would grow
    // without bound and every open/recover would rescan dead journals.
    let _ = std::fs::remove_file(&jpath);

    drop(lock);
    Ok(TxnReport {
        txn_id: journal.id,
        refs_updated,
        files_written,
    })
}

/// Redo incomplete journals. Called on repo open and before every txn.
pub fn recover(ng: &Path, limits: &Limits) -> Result<RecoveryReport> {
    if !txn_dir(ng).exists() {
        return Ok(RecoveryReport::default());
    }
    let lock = acquire_global_lock(ng, limits)?;
    let r = recover_locked(ng, limits)?;
    drop(lock);
    Ok(r)
}

fn recover_locked(ng: &Path, limits: &Limits) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();
    let dir = txn_dir(ng);
    if !dir.exists() {
        return Ok(report);
    }
    let mut journals: Vec<PathBuf> = Vec::new();
    for e in std::fs::read_dir(&dir).map_err(|e| Error::io(&dir, e))? {
        let e = e.map_err(|err| Error::io(&dir, err))?;
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with(".journal") {
            journals.push(e.path());
        }
    }
    journals.sort();
    for jpath in journals {
        let text = fsx::read_limited(&jpath, 64 << 20)?;
        let text = match String::from_utf8(text) {
            Ok(t) => t,
            Err(_) => {
                quarantine(&jpath, &mut report)?;
                continue;
            }
        };
        let journal = match Journal::parse(&text) {
            Ok(j) => j,
            Err(_) => {
                // Journals are written atomically; an unparsable journal is
                // real corruption — quarantine it, never apply guesses.
                quarantine(&jpath, &mut report)?;
                continue;
            }
        };
        match journal.state {
            JournalState::Complete | JournalState::Recovered => {
                // Terminal state: the txn is durable and applied. Delete
                // (checkpoint cleanup — the writer normally deletes it
                // itself; this covers a crash between commit and delete).
                std::fs::remove_file(&jpath).map_err(|e| Error::io(&jpath, e))?;
                report.cleaned += 1;
            }
            JournalState::Running => {
                // Crash happened before COMPLETE: redo (idempotent).
                apply_journal(ng, &journal, limits)?;
                let mut done = journal.clone();
                done.state = JournalState::Recovered;
                fsx::atomic_write(&jpath, done.serialize().as_bytes())?;
                report.redone.push(journal.id.clone());
                std::fs::remove_file(&jpath).map_err(|e| Error::io(&jpath, e))?;
                report.cleaned += 1;
            }
        }
    }
    Ok(report)
}

fn quarantine(jpath: &Path, report: &mut RecoveryReport) -> Result<()> {
    let mut dest = jpath.as_os_str().to_os_string();
    dest.push(".corrupt");
    std::fs::rename(jpath, PathBuf::from(&dest)).map_err(|e| Error::io(jpath, e))?;
    report.quarantined.push(
        PathBuf::from(dest)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
    );
    Ok(())
}

/// Apply a journal's final state. Idempotent: safe to run any number of
/// times (renames write fixed contents; reflog lines carry the txn id and
/// are deduplicated by readers).
fn apply_journal(ng: &Path, journal: &Journal, limits: &Limits) -> Result<()> {
    // Group reflog appends per ref; write each in one syscall + fsync.
    let mut logs: BTreeMap<&str, Vec<String>> = BTreeMap::new();

    for (i, op) in journal.ops.iter().enumerate() {
        // Optional per-op kill switch for crash tests: txn:apply#<i>
        let _ = fault::fault_action(&format!("txn:apply#{}", i));
        match op {
            TxnOp::Ref {
                name,
                cas: _,
                new,
                log,
            } => {
                let path = ref_path(ng, name)?;
                match new {
                    Some(oid) => {
                        fault::fault("txn:ref_write_err")?;
                        fsx::ensure_dir(path.parent().unwrap_or(ng))?;
                        fsx::atomic_write(&path, format!("{}\n", oid.to_hex()).as_bytes())?;
                    }
                    None => match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(Error::io(&path, e)),
                    },
                }
                let line = format!(
                    "{} {} {} {} {} {}\n",
                    "ZERO",
                    match new {
                        Some(o) => o.to_hex(),
                        None => "ZERO".to_string(),
                    },
                    log.ts_ms,
                    match log.actor {
                        Some(a) => a.to_hex(),
                        None => "ZERO".to_string(),
                    },
                    base64::encode(journal.id.as_bytes()),
                    base64::encode(log.message.as_bytes()),
                );
                logs.entry(name.as_str()).or_default().push(line);
            }
            TxnOp::File { rel, data } => {
                let path = ng.join(rel);
                fault::fault("txn:file_write_err")?;
                fsx::atomic_write(&path, data)?;
            }
            TxnOp::FileDelete { rel } => {
                let path = ng.join(rel);
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Error::io(&path, e)),
                }
            }
        }
    }

    for (name, lines) in logs {
        let log_path = reflog_path(ng, name)?;
        fsx::ensure_dir(log_path.parent().unwrap_or(ng))?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| Error::io(&log_path, e))?;
        let payload = lines.concat();
        // Tag every append with the journal id so partial writes are
        // detectable and redo-duplicates are dedupable.
        f.write_all(payload.as_bytes())
            .map_err(|e| Error::io(&log_path, e))?;
        f.sync_all().map_err(|e| Error::io(&log_path, e))?;
    }
    fsx::fsync_dir(&txn_dir(ng))?;
    fsx::fsync_dir(&ng.join("refs"))?;
    let _ = limits;
    Ok(())
}

fn validate_ops(ops: &[TxnOp], limits: &Limits) -> Result<()> {
    if ops.is_empty() {
        return Err(Error::Invalid("empty transaction".into()));
    }
    if ops.len() > 10_000 {
        return Err(Error::Limit("transaction has too many ops".into()));
    }
    for op in ops {
        match op {
            TxnOp::Ref { name, log, .. } => {
                check_ref_name_system(name)?;
                if log.message.len() > 4096 {
                    return Err(Error::Limit("reflog message too long".into()));
                }
            }
            TxnOp::File { rel, data } => {
                check_file_op_rel(rel, limits)?;
                if data.len() as u64 > 1 << 20 {
                    return Err(Error::Limit(
                        "txn FILE op larger than 1 MiB (not a metadata write?)".into(),
                    ));
                }
            }
            TxnOp::FileDelete { rel } => {
                check_file_op_rel(rel, limits)?;
            }
        }
    }
    Ok(())
}

fn check_file_op_rel(rel: &str, limits: &Limits) -> Result<()> {
    fsx::check_rel_path(rel, limits.max_path_component)?;
    // Refuse to journal anything inside the object store or txn dir through
    // generic FILE/FDEL ops.
    if rel.starts_with("objects/") || rel.starts_with("txn/") {
        return Err(Error::Invalid(format!("FILE op may not target {rel:?}")));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ref file access (shared with refs.rs)
// ---------------------------------------------------------------------------

pub fn ref_path(ng: &Path, name: &str) -> Result<PathBuf> {
    check_ref_name_system(name)?;
    Ok(ng.join("refs").join(name))
}

pub fn reflog_path(ng: &Path, name: &str) -> Result<PathBuf> {
    check_ref_name_system(name)?;
    Ok(ng.join("logs").join("refs").join(name))
}

/// Read a ref's current value (None if absent). Malformed content is an error.
pub fn read_ref_raw(ng: &Path, name: &str) -> Result<Option<ObjectId>> {
    let path = ref_path(ng, name)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            let s = String::from_utf8(bytes).map_err(|_| {
                Error::Malformed(format!("ref file {} is not utf-8", path.display()))
            })?;
            let s = s.trim_end_matches('\n');
            if s.len() != ObjectId::HEX_LEN {
                return Err(Error::Malformed(format!(
                    "ref file {} has bad length {}",
                    path.display(),
                    s.len()
                )));
            }
            Ok(Some(ObjectId::from_hex(s)?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(&path, e)),
    }
}

/// A parsed reflog line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflogLine {
    pub old: Option<ObjectId>, // always None in v1 final-state journals
    pub new: Option<ObjectId>,
    pub ts_ms: i64,
    pub actor: Option<ObjectId>,
    pub txn_id: String,
    pub message: String,
}

/// Read a reflog, tolerating a torn last line and deduplicating (txn_id, ref)
/// repeats introduced by crash-redo.
pub fn read_reflog(ng: &Path, name: &str) -> Result<Vec<ReflogLine>> {
    let path = reflog_path(ng, name)?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(&path, e)),
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut out: Vec<ReflogLine> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        if f.len() != 6 {
            continue; // torn or foreign line — reflog is best-effort history
        }
        let Ok(new) = parse_oid_field(f[1]) else {
            continue;
        };
        let Ok(ts) = f[2].parse::<i64>() else {
            continue;
        };
        let Ok(actor) = parse_oid_field(f[3]) else {
            continue;
        };
        let Ok(txn) = String::from_utf8(base64::decode(f[4]).unwrap_or_default()) else {
            continue;
        };
        let Ok(msg) = String::from_utf8(base64::decode(f[5]).unwrap_or_default()) else {
            continue;
        };
        let key = format!("{txn}:{name}");
        if !seen.insert(key) {
            continue; // redo duplicate
        }
        out.push(ReflogLine {
            old: None,
            new,
            ts_ms: ts,
            actor,
            txn_id: txn,
            message: msg,
        });
    }
    Ok(out)
}
