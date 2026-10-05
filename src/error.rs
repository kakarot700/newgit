//! NewGit error model.
//!
//! Every error carries a category so the CLI can map failures to stable exit
//! codes and so structured logs can classify outcomes without string parsing.

use std::path::PathBuf;

use crate::object::ObjectId;

pub type Result<T> = std::result::Result<T, Error>;

/// Stable process exit codes for the CLI.
pub mod exit_code {
    pub const OK: i32 = 0;
    /// User/input error (bad arguments, invalid names, ...).
    pub const USAGE: i32 = 2;
    /// Repository state error (not a repo, corrupt object, verify failure).
    pub const REPO: i32 = 3;
    /// Concurrency error (lock busy, CAS race) — retryable.
    pub const RACE: i32 = 4;
    /// Merge/integration conflict.
    pub const CONFLICT: i32 = 5;
    /// Resource limit exceeded.
    pub const LIMIT: i32 = 6;
    /// Permission / authentication / authorization failure.
    pub const AUTH: i32 = 7;
    /// Internal invariant violation (bug).
    pub const BUG: i32 = 70;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error{path}: {source}", path = match path { Some(p) => format!(" at {}", p.display()), None => String::new() })]
    Io {
        path: Option<PathBuf>,
        source: std::io::Error,
    },

    #[error("not a newgit repository: {0}")]
    NotRepo(PathBuf),

    #[error("object not found: {0}")]
    NotFound(ObjectId),

    #[error("object {oid} is corrupt: {reason}")]
    Corrupt { oid: ObjectId, reason: String },

    #[error("malformed data: {0}")]
    Malformed(String),

    #[error("invalid value: {0}")]
    Invalid(String),

    #[error("invalid reference name: {0}")]
    InvalidRef(String),

    #[error("reference not found: {0}")]
    RefNotFound(String),

    #[error("concurrent modification of {0}; retry the operation")]
    CasFailed(String),

    #[error("lock busy: {0} (another newgit process is running?)")]
    LockBusy(String),

    #[error("verification failed: {0}")]
    Verify(String),

    #[error("merge conflict: {0}")]
    Conflict(String),

    #[error("limit exceeded: {0}")]
    Limit(String),

    #[error("authentication/authorization failure: {0}")]
    Auth(String),

    #[error("authorization denied: {0}")]
    Forbidden(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("git interoperability error: {0}")]
    Git(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("internal invariant violation: {0}")]
    Bug(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: Some(path.into()),
            source,
        }
    }

    pub fn is_missing_file(&self) -> bool {
        matches!(self, Error::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }

    /// Stable exit code for CLI use.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Io { source, .. } => match source.kind() {
                std::io::ErrorKind::NotFound => exit_code::REPO,
                std::io::ErrorKind::PermissionDenied => exit_code::AUTH,
                _ => exit_code::REPO,
            },
            Error::NotRepo(_)
            | Error::NotFound(_)
            | Error::Corrupt { .. }
            | Error::RefNotFound(_)
            | Error::Verify(_) => exit_code::REPO,
            Error::Malformed(_) | Error::Invalid(_) | Error::InvalidRef(_) | Error::Config(_) => {
                exit_code::USAGE
            }
            Error::CasFailed(_) | Error::LockBusy(_) => exit_code::RACE,
            Error::Conflict(_) => exit_code::CONFLICT,
            Error::Limit(_) => exit_code::LIMIT,
            Error::Auth(_) | Error::Forbidden(_) => exit_code::AUTH,
            Error::Protocol(_) | Error::Git(_) => exit_code::REPO,
            Error::Bug(_) => exit_code::BUG,
        }
    }

    /// Machine-readable category for structured logs/JSON output.
    pub fn category(&self) -> &'static str {
        match self {
            Error::Io { .. } => "io",
            Error::NotRepo(_) => "not_repo",
            Error::NotFound(_) => "not_found",
            Error::Corrupt { .. } => "corrupt",
            Error::Malformed(_) => "malformed",
            Error::Invalid(_) => "invalid",
            Error::InvalidRef(_) => "invalid_ref",
            Error::RefNotFound(_) => "ref_not_found",
            Error::CasFailed(_) => "cas_failed",
            Error::LockBusy(_) => "lock_busy",
            Error::Verify(_) => "verify",
            Error::Conflict(_) => "conflict",
            Error::Limit(_) => "limit",
            Error::Auth(_) | Error::Forbidden(_) => "auth",
            Error::Protocol(_) => "protocol",
            Error::Git(_) => "git",
            Error::Config(_) => "config",
            Error::Bug(_) => "bug",
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { path: None, source }
    }
}
