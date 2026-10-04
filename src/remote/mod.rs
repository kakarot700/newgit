//! Remote protocol v1: HTTP/1.1 + JSON, std-only server and client.
//!
//! * [`proto`] — wire types + protocol constants (docs/PROTOCOL.md).
//! * [`http`] — total request parser / response writer (fuzzed).
//! * [`auth`] — bearer tokens (SHA-256 at rest) + roles read<write<admin.
//! * [`audit`] — append-only JSONL audit log under `.newgit/`.
//! * [`negotiate`] — closure + dependency-first post-order with excludes.
//! * [`server`] — `newgit serve` (thread-per-connection, bounded).
//! * [`client`] — `newgit remote/push/pull`.
//!
//! Security posture (THREAT_MODEL §E): every parser total and capped, auth
//! failures never downgrade to anonymous, objects are self-verifying
//! envelopes re-validated on receipt, ref moves are CAS transactions on the
//! receiving side, and the server enforces the link-closure invariant on
//! every remote object write ("have X" ⇒ "hold closure(X)").

pub mod audit;
pub mod auth;
pub mod client;
pub mod http;
pub mod negotiate;
pub mod proto;
pub mod server;
