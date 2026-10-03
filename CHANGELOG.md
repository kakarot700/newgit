# CHANGELOG

Format: Keep a Changelog. Versions follow semver once ≥1.0; 0.x = honest WIP.

## [Unreleased]

### Added (iteration 1 — 2026-10-03)
- Project bootstrap: Rust crate (lib+bin pending), pinned toolchain, CI workflow.
- Object model v1 (frozen protocol, docs/STORAGE_FORMAT.md):
  Blob, Tree, Snapshot, Actor, Goal, Change, Evidence, Evaluation, Proposal
  with canonical binary codecs and strict total decoders.
- SHA-256 object identity; deterministic-identity invariant tests.
- NGOB on-disk envelope: versioned, self-verifying, bomb-protected.
- Content-addressed object store: atomic crash-safe writes, verified reads,
  corruption/misfiling/truncation detection, prefix resolution, temp sweep.
- Filesystem primitives: atomic_write, O_EXCL locks with stale reclaim,
  bounded reads, path-safety grammar (traversal/NUL/drive/symlink escape).
- Fault-injection framework (`NEWGIT_FAULTS`) for crash-safety tests.
- Repository config & resource limits (`key = value`, fail-loud parsing).
- Error model with categories and stable CLI exit codes.
- Documentation baseline: README, ARCHITECTURE, STORAGE_FORMAT,
  SECURITY_MODEL, THREAT_MODEL, DECISIONS, ROADMAP, TEST_MATRIX,
  KNOWN_LIMITATIONS, RELEASE_READINESS, loop-state files.
- 37 unit tests; fmt/clippy(-D warnings) clean.
