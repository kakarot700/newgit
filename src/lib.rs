//! NewGit: an agent-native version-control system.
//!
//! Core design (see docs/ARCHITECTURE.md):
//! * content-addressed immutable objects (SHA-256, self-verifying envelope),
//! * snapshots (immutable project states) with explicit parent lineage,
//! * first-class Goals, Changes, Evidence, Evaluations, Proposals, Actors,
//! * isolated workspaces for concurrent human/agent work,
//! * journaled transactions for atomic, crash-safe ref updates,
//! * verification and garbage collection that can never lose reachable data.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod cli;
pub mod diff;
pub mod error;
pub mod object;
pub mod obs;
pub mod ops;
pub mod repo;
pub mod util;

pub use error::{Error, Result};
pub use object::ObjectId;

/// NewGit version (single source of truth; kept in sync with Cargo.toml by a
/// test in tests/version.rs).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
