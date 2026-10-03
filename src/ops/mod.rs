//! High-level operations on a repository (snapshot, status, history,
//! checkout, tree building). Each op is a pure function of Repo + explicit
//! inputs — the CLI and the remote server share this layer.

pub mod checkout;
pub mod history;
pub mod snapshot;
pub mod status;
pub mod tree;
