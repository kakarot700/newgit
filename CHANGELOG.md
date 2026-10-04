# CHANGELOG

Format: Keep a Changelog. Versions follow semver once ≥1.0; 0.x = honest WIP.

## [Unreleased]

### Added (iteration 4 — 2026-10-04)
- Diff engine: Myers O(ND) line diff with bounded edit distance and
  exact coarse fallback; canonical opcodes; reconstruction property.
- Tree diff: added/deleted/modified/mode-change; two-stage deterministic
  rename detection (exact + similarity); binary detection; symlink diffs.
- Renderers: git-shaped unified diff (hunks, context merging, no-newline
  markers, rename/mode headers) + structured JSON hunks.
- CLI `newgit diff [<a> [<b>]]` with `-w`, `--name-only`, `--json`,
  `--context`, `--no-renames`, `--exit-code`.
- Read-only worktree capture (`capture_tree(save_index=false)`).
- 22 new tests (total 142 incl. property diff-reconstruct).

### Fixed (iteration 4)
- **Racily-clean index race** (D-011): entries whose mtime ≥ the index
  file's mtime are re-hashed; a same-tick file replacement (e.g. symlink
  swap) could previously reuse stale content ids. Caught by the symlink
  diff test; guarded by a unit test.

### Added (iteration 3 — 2026-10-04)
- `.newgitignore` engine (gitignore subset, deterministic, documented).
- Safe workspace walk with limits, symlink policy (record, never follow),
  filename validation, and skip-with-warning behavior.
- NGIX workspace index (status cache; corruption-safe by design).
- Workspaces: main + named, journaled create/discard, position refs,
  per-workspace locks, dirty-discard protection.
- Operations: snapshot (CAS-journaled), status, history (deterministic
  order), tree build/flatten (cycle-safe), checkout (symlink-traversal safe).
- CLI binary `newgit`: 11 commands, `--json` envelope, stable exit codes,
  `--debug` structured diagnostics; docs/CLI.md.
- Crash tests for snapshot and workspace creation; 45 new tests (total 120).

### Fixed (iteration 3)
- NGIX index double-length-prefix decode bug (caught by ops test — index
  reuse silently disabled; now regression-tested both levels).
- base64/cli-arg test-harness bugs (token splitting, fs limits vs OS limits).

### Added (iteration 2 — 2026-10-04)
- Repository facade: idempotent `init`, crash-recovering `open`, walk-up
  `discover`, HEAD (symbolic/detached) via journaled updates, actor registry
  with default-actor resolution, format-version gate.
- Refs: strict name grammar + reserved namespaces; CAS create/update/delete;
  prefix listing; reflog with txn-id deduplication.
- WAL transaction engine: global lock, recovery-first, pre-write CAS
  validation, RUNNING→apply→COMPLETE protocol, idempotent redo recovery,
  corrupt-journal quarantine.
- Lock reclamation by holder-pid liveness (crash-safe, D-008).
- `newgit-faultlab` crash-test harness binary; fault points across txn/ostore.
- base64 codec (RFC 4648) for journal/reflog text formats.
- 37 new tests: crash-recovery scenarios (child-process aborts), concurrency
  races, property-based codec invariants. Total: 74.

### Fixed (iteration 2)
- Journal REF op parser arity bug (5 vs 6 fields) caught by roundtrip test.
- Fault-point `name#N` matching (exact-name entries now fire correctly).
- base64 decoder now rejects mid-string padding (RFC-conformant).
- LockBusy deadlock after aborted child processes (D-008).

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
