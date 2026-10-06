//! NewGit JSON remote protocol plus a separate Git smart-HTTP adapter.
//!
//! * [`proto`] — wire types + protocol constants (docs/PROTOCOL.md).
//! * [`http`] — total request parser / response writer (fuzzed).
//! * [`auth`] — bearer tokens (SHA-256 at rest) + roles read<write<admin.
//! * [`audit`] — append-only JSONL audit log under `.newgit/`.
//! * [`negotiate`] — closure + dependency-first post-order with excludes.
//! * [`server`] — `newgit serve` (thread-per-connection, bounded).
//! * [`client`] — `newgit remote/push/pull`.
//! * [`git_http`] — ordinary Git upload-pack over smart HTTP.
//! * [`git_receive`] — bounded receive-pack for branch creates/fast-forward
//!   updates/deletes and lightweight-tag creates/deletes; accepted refs use the
//!   canonical transaction engine.
//!
//! Security posture (THREAT_MODEL §E): every parser total and capped, auth
//! failures never downgrade to anonymous, objects are self-verifying
//! envelopes re-validated on receipt, ref moves are CAS transactions on the
//! receiving side, and the server enforces the link-closure invariant on
//! every remote object write ("have X" ⇒ "hold closure(X)").

pub mod audit;
pub mod auth;
#[cfg(test)]
pub(crate) mod bench_timing;
pub mod client;
#[cfg(feature = "smart-http-diagnostics")]
pub(crate) mod diagnostics;
pub mod git_http;
pub mod git_receive;
pub mod http;
pub mod negotiate;
pub mod proto;
pub mod server;
