//! Small deterministic utilities: hex, varints, and crash-safe filesystem
//! primitives (atomic writes, advisory locks, fsync, path safety checks).

pub mod fault;
pub mod fsx;
pub mod hex;
pub mod varint;
