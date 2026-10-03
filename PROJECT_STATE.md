# PROJECT_STATE.md — NewGit autonomous build loop

> Machine-and-human readable continuation state. Updated at the end of every
> iteration. If context is lost, resume from this file.

## Current status

- **Phase:** Iteration 3 COMPLETE — workspaces, snapshots, status, history, CLI foundation.
- **Classification:** NOT PRODUCTION READY (no diff/merge yet; see RELEASE_READINESS.md).
- **Last full verification:** `cargo fmt --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` **120/120** ✓ (75 unit + 7 cli-e2e + 4 concurrency + 13 ops + 10 property + 10 txn-recovery + 1 version).

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
  repo/{config,ostore,refs,txn,mod}.rs  # store, refs (CAS+reflog), WAL transactions, Repo facade
  repo/{ignore,index,walk,workspace}.rs  # .newgitignore engine, NGIX cache, safe walk, workspaces
  ops/{tree,snapshot,checkout,status,history}.rs  # core operations
  cli/{mod,args}.rs + main.rs      # newgit binary: --json, stable exit codes
  obs.rs                            # structured stderr diagnostics
  bin/newgit-faultlab.rs           # crash-test harness child process
tests/{common,txn_recovery,concurrency_refs,property_core,version,ops_snapshot,cli_e2e}.rs
docs/{STORAGE_FORMAT,CLI}.md
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

## What iteration 2 added (verified)

- Repo facade: init (idempotent), open (runs crash recovery), discover (walk-up),
  HEAD (symbolic/detached, txn-journaled updates), actor registry + default
  actor resolution (config → env → anonymous), format-version gate.
- Refs: strict name grammar (reserved namespaces, forbidden chars), CAS updates,
  delete, list/prefix-walk, reflog with txn-id dedup (crash-redo safe).
- WAL transaction engine: global txn lock, recovery-first, CAS preconditions
  before any write, RUNNING journal → apply (idempotent) → COMPLETE marker;
  commit point = journal fsync (forward recovery); corrupt journals quarantined.
- FileLock reclaims locks of provably-dead holders (pid liveness via /proc on
  Linux; stale timeout otherwise) — crash-tested with aborted child processes.
- Fault points: txn:before_journal, txn:after_journal, txn:apply#<i>,
  txn:before_complete, txn:after_complete, txn:ref_write_err, txn:file_write_err,
  ostore:before_write/after_tmp_write/before_rename/after_rename/after_read.
- Tests: 5 crash-recovery scenarios via faultlab child aborts; 4 concurrency
  suites (CAS races ⇒ exactly one winner/version + reflog count equality;
  parallel multi-ref txns; concurrent object writes; concurrent recovery);
  10 property suites (codecs, canonical-form invariants, bit-flip detection).

## What iteration 3 added (verified)

- .newgitignore engine (gitignore subset: wildcards, **, anchoring, negation,
  dir-only; last rule wins; pruning only without negations).
- Safe workspace walk: limits (depth/component/size), symlink record-don't-follow,
  control-char/NUL filename rejection, non-UTF-8 names skipped with warnings,
  .newgit/.git skipped, special files skipped with warnings.
- NGIX index cache (status accelerator; corrupt/missing index rebuilds silently —
  invariant-tested).
- Workspaces: main (repo root) + named (.newgit/workspaces/<name>/files),
  journaled create (ref+meta atomic), discard with dirty-refusal + --force,
  per-workspace op locks, position refs workspaces/<name>.
- Snapshot op: walk → hash (index fast path) → build_tree → Snapshot object →
  CAS ref update in one txn; deterministic with --time/--author.
- Checkout: FreshWorkspace/Overwrite modes, refuses symlink-component
  traversal, restores exec bits + symlinks, feeds index.
- Status: added/modified/deleted classification, list truncation, warnings.
- History: deterministic newest-first traversal (ts DESC, oid DESC).
- CLI `newgit`: init/status/snapshot/history/log/cat/hash-object/workspace/
  actor/config/version/help; --json envelope; stable exit codes; --debug JSONL
  diagnostics on stderr; hand-rolled arg parser with typo rejection.
- Crash tests: snap:before_txn (harmless), snap:after_txn (committed),
  workspace-create txn crash (ref/meta converge).
- E2E: 7 subprocess suites incl. full workflow, JSON errors, exit codes,
  cross-repo snapshot determinism.

## Current task (next iteration)

**Iteration 4: diff engine.**
Completion condition:
1. Myers O(ND) line diff + unified & JSON output; binary detection; rename
   detection (exact + similarity); mode-change reporting.
2. `newgit diff [a] [b] [--name-only|--json|--unified]` for snapshots,
   workspaces, and arbitrary trees.
3. Property tests: patch application reconstructs target; determinism.
4. Large-file guards (limits) + tests. Docs updated; commit.

## Next tasks (ordered)

5. Workspaces + tree building from filesystem + snapshot/status/history ops + CLI foundation (`init`, `snapshot`, `status`, `history`, `log`, `cat`, `hash-object`).
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
