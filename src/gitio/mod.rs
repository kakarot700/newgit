//! Git compatibility: import/export via the system git's stream formats
//! (D-007 — no git object-format reimplementation, no extra dependencies).
//!
//! * `fastexport` — total parser for `git fast-export` streams,
//! * `import`     — `newgit import-git <git-repo>`: streams a real git
//!   repository into this one (objects + refs, atomic ref switch),
//! * `export`     — `newgit export-git <dir>`: streams NewGit history into
//!   a real git repository via `git fast-import`.
//!
//! Mapping rules and honest limitations: docs/GIT_COMPAT.md.

pub mod export;
pub mod fastexport;
pub mod import;
