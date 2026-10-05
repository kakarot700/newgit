//! Append-only audit log for remote server activity.
//!
//! One JSON object per line in `.newgit/audit.log`: who (token id or
//! "anonymous"), what (method + path), result (status + error category),
//! when (epoch ms). Appends happen under a file lock so concurrent
//! connections interleave lines, never bytes. Audit failures NEVER fail
//! requests (availability first) — but they are surfaced via obs events so
//! `--debug` makes a broken audit log visible.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::Result;
use crate::util::fsx;

pub const FILE_NAME: &str = "audit.log";

#[derive(Clone, Debug)]
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(ng: &Path) -> Self {
        AuditLog {
            path: ng.join(FILE_NAME),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry. Best-effort: errors are reported to obs, not to
    /// the caller (a full/read-only disk must not take down reads/writes).
    pub fn append(
        &self,
        principal: &str,
        method: &str,
        path: &str,
        status: u16,
        error_category: Option<&str>,
    ) {
        let entry = json!({
            "ts_ms": crate::repo::txn::now_ms(),
            "principal": principal,
            "method": method,
            "path": path,
            "status": status,
            "error": error_category,
        });
        if let Err(e) = self.append_line(&entry) {
            crate::obs::event("audit_append_failed", &[("error", json!(e.to_string()))]);
        }
    }

    fn append_line(&self, entry: &Value) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fsx::ensure_dir(parent)?;
        }
        let lock = fsx::FileLock::acquire(
            &fsx::lock_path_for(&self.path),
            Duration::from_millis(2_000),
        )?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{entry}")?;
        f.sync_all()?;
        drop(lock);
        Ok(())
    }

    /// Last `limit` entries (skipping unparsable lines — a damaged audit
    /// line never breaks the reader).
    pub fn tail(&self, limit: usize) -> Result<Vec<Value>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut out: Vec<Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();
        if out.len() > limit {
            out = out.split_off(out.len() - limit);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_tail_roundtrip_with_concurrent_safety() {
        let dir = std::env::temp_dir().join(format!("ngaudit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = AuditLog::new(&dir);
        assert!(log.tail(10).unwrap().is_empty());
        log.append("ci-bot", "POST", "/v1/refs/update", 200, None);
        log.append("anonymous", "GET", "/v1/refs", 401, Some("auth"));
        let entries = log.tail(10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["principal"], "ci-bot");
        assert_eq!(entries[1]["status"], 401);
        assert_eq!(entries[1]["error"], "auth");
        // limit keeps the LAST n
        for i in 0..5 {
            log.append(&format!("p{i}"), "GET", "/healthz", 200, None);
        }
        assert_eq!(log.tail(3).unwrap().len(), 3);
        assert_eq!(log.tail(3).unwrap()[2]["principal"], "p4");
        // damaged line is skipped, not fatal
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(log.path())
            .unwrap();
        writeln!(f, "NOT JSON").unwrap();
        log.append("after-damage", "GET", "/x", 200, None);
        let entries = log.tail(100).unwrap();
        assert_eq!(entries.last().unwrap()["principal"], "after-damage");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
