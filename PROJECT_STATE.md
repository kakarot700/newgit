# PROJECT_STATE.md — NewGit autonomous build loop

> Machine-and-human readable continuation state. Updated at the end of every
> iteration. If context is lost, resume from this file.

## Current status

- **Phase:** Iteration 1 COMPLETE — core object/storage layer landed.
- **Classification:** NOT PRODUCTION READY (early core; see RELEASE_READINESS.md).
- **Last full verification:** `cargo fmt --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` 37/37 ✓ (see TEST_MATRIX.md for exact commands).

## Environment / how to resume

```bash
# Toolchain lives outside the repo snapshot:
export RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
cd /home/user/newgit
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

Rust 1.99.0 stable (pinned in rust-toolchain.toml). System `git` available
(used later by git interop tests). 2 CPUs, 2 GB RAM — keep test parallelism
modest; avoid heavyweight dev-dependencies.

## Repository layout (as of now)

```
Cargo.toml            # deps: sha2, flate2(rust backend), serde, serde_json, thiserror; dev: proptest, tempfile
rust-toolchain.toml
src/
  lib.rs              # crate root, VERSION, #![forbid(unsafe_code)]
  error.rs            # Error enum, categories, stable exit codes
  util/{hex,varint,fsx,fault}.rs   # codecs, atomic writes, locks, fault injection
  object/{id,types,envelope}.rs    # ObjectId, 9 object types w/ canonical codec, NGOB envelope
  repo/{config,ostore}.rs          # Limits/RepoConfig, content-addressed store
tests/                # (integration tests arrive from iteration 2)
docs/                 # STORAGE_FORMAT.md (normative)
.github/workflows/ci.yml
```

## What exists and works (verified by tests)

- SHA-256 object identity; invariant "identical content ⇒ identical id" tested.
- Canonical binary codec for: Blob, Tree, Snapshot, Actor, Goal, Change,
  Evidence, Evaluation, Proposal. Malformed-input rejection tested (no panics).
- NGOB on-disk envelope: magic, version, type tag, lengths, zlib payload,
  trailing SHA-256 digest. Bit-flip/truncation/misfiling/bomb tests pass.
- ObjectStore: atomic put (tmp→fsync→rename→dir fsync), verified get,
  iter, prefix resolution, temp-debris sweep, blob/file size limits.
- fsx: atomic_write, FileLock (O_EXCL, pid+time content, stale reclaim),
  read_limited, path safety (traversal/NUL/drive-letter/symlink-escape checks).
- Fault injection (`NEWGIT_FAULTS`, modes abort/error) for crash tests.
- RepoConfig/Limits: `key = value` config, unknown keys rejected, version gate.

## Failing tests

None.

## Current task (next iteration)

**Iteration 2: repository skeleton + refs + transactions + recovery.**
Completion condition:
1. `Repo::init/open` with `.newgit/` layout, HEAD, config, actors registry.
2. Refs module: create/read/update/delete with lock+CAS, invalid-name rejection.
3. Journaled transaction engine: multi-ref atomic updates; kill-at-fault-point
   tests prove redo recovery leaves refs all-or-nothing.
4. Property tests for varint/hex/ObjectId already exist; add ref name rules.
5. Docs: ARCHITECTURE.md updated; commits coherent.

## Next tasks (ordered)

3. Workspaces + tree building from filesystem + snapshot/status/history ops + CLI foundation (`init`, `snapshot`, `status`, `history`, `log`, `cat`, `hash-object`).
4. Diff engine (Myers line diff, rename detection, binary handling, JSON+unified output).
5. Merge engine (3-way tree + diff3 content merge, conflicts, integrate/rollback).
6. Goals/Changes/Actors/Evidence/Evaluations/Proposals ops + CLI.
7. verify (fsck) + gc + reflog + chaos/failure-injection suite.
8. Git import/export via fast-export/fast-import + compatibility tests.
9. Remote protocol (HTTP/1.1, JSON v1) server+client, auth, audit.
10. Web UI served by remote server; MCP/agent-API docs.
11. Benchmarks + BENCHMARKS.md; release engineering (dist script, checksums);
    full docs set; final forensic audit; readiness gate.

## Important decisions (full log in DECISIONS.md)

- Rust, minimal dependencies, no frameworks; hand-rolled CLI parser and HTTP.
- SHA-256 ids; canonical encodings are a frozen protocol (docs/STORAGE_FORMAT.md).
- Objects immutable; set-like fields strictly ascending for unique canonical form.
- Evidence has `deterministic` flag; AI opinions must be flagged (`ai_generated`).
- Git interop via system git fast-export/fast-import (no gitoxide dependency).
- No staging area: workspaces hold live state; `snapshot` captures whole workspace.

## Commands

```bash
cargo test                     # unit + integration
cargo test --release           # perf-sensitive suites (chaos)
NEWGIT_CHAOS_ITERATIONS=500 cargo test --release --test chaos
```
