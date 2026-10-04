# PROJECT_STATE.md — NewGit autonomous build loop

> Machine-and-human readable continuation state. Updated at the end of every
> iteration. If context is lost, resume from this file.

## Current status

- **Phase:** Iteration 6 COMPLETE — goals/changes/evidence/evaluations/proposals + two-agent workflow.
- **Classification:** NOT PRODUCTION READY (no verify/gc/remotes yet; see RELEASE_READINESS.md).
- **Last full verification:** `cargo fmt --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` **186/186** ✓ (101 unit + 10 cli-e2e + 4 concurrency + 8 diff + 18 merge + 13 ops + 12 property + 10 txn-recovery + 1 version + 9 workflow).

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
  diff/{myers,render,mod}.rs     # line diff, tree diff, rename detection, unified+JSON render
  merge/{diff3,base,mod}.rs      # 3-way content merge, LCA/ancestry, tree merge
  ops/integrate.rs               # atomic integrate, rollback, checkout_position
  ops/workflow.rs                # goals/changes/evidence/evaluations/proposals (chains)
  cli/workflow_cmds.rs           # workflow CLI command families
  cli/{mod,args}.rs + main.rs      # newgit binary: --json, stable exit codes
  obs.rs                            # structured stderr diagnostics
  bin/newgit-faultlab.rs           # crash-test harness child process
tests/{common,txn_recovery,concurrency_refs,property_core,version,ops_snapshot,diff_engine,merge_integrate,workflow,cli_e2e}.rs
docs/{STORAGE_FORMAT,CLI,AGENT_WORKFLOW}.md
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


## What iteration 4 added (verified)

- Myers O(ND) line diff with prefix/suffix trimming, bounded edit distance
  (cap 1024/file; coarse whole-file replace fallback that stays exact).
- Canonical opcodes (Equal/Delete/Insert/Replace) + reconstruction property
  (reconstruct(a,b,ops) == b for random inputs) + determinism property.
- Tree diff: added/deleted/modified/mode-change classification; rename
  detection in two deterministic stages (exact oid ⇒ 100%, then prefix/
  suffix similarity ≥50% with bounded candidate pairs, greedy with
  (score, old, new) tiebreak); binary detection (NUL in first 8000 bytes);
  symlink target diffs shown as text.
- Renderers: git-shaped unified output (@@ hunks, context merging at gap
  ≤ 2·context, "\ No newline at end of file" markers, rename/mode headers)
  and structured JSON hunks.
- CLI `newgit diff [<a> [<b>]] [-w ws] [--name-only] [--json] [--context N]
  [--no-renames] [--exit-code]`; specs: refs, snapshot/tree oids (prefix ok),
  ws:<name>; omitted b ⇒ live workspace (read-only capture).
- Racily-clean guard (D-011): index entries with mtime ≥ index-file mtime
  are re-hashed — closes the same-tick modification race (caught by the
  symlink diff test; regression-tested).
- capture_tree(save_index=false): read-only worktree capture for diff/status.


## What iteration 5 added (verified)

- 3-way tree merge with exact-rename tracking, mode combining, and honest
  conflict taxonomy (content/opaque/modify-delete/rename-rename/
  rename-delete/mode/dir-file); conflict blobs with diff3 markers stored
  for inspection (merged_oid).
- diff3 content merge on Myers anchors + property tests (determinism,
  trivial-case agreement).
- Bounded deterministic ancestry: is_ancestor, merge_base (LCA by
  (ts DESC, oid DESC); criss-cross documented limitation #14).
- ops: integrate (atomic up-to-date/fast-forward/merge; conflict ⇒ nothing
  written), rollback (new snapshot + old tree; merge_ours-aware),
  checkout_position (resync + stale-tracked-file removal + empty-dir prune).
- CLI: integrate, merge-tree (exit 5 on conflicts, text+JSON), rollback,
  checkout; help texts updated.
- Crash tests: integ:before_txn ⇒ position unchanged; integ:after_txn ⇒
  ref durable, status honestly dirty, `checkout` repairs.
- Concurrency test: same-target integrates serialize on the workspace lock
  (lock acquired BEFORE position read); exactly one merge snapshot; reflog
  length asserted.
- Merge roles in extras (merge_ours/merge_theirs) because parents is a
  canonically sorted set (D-004/D-012).


## What iteration 6 added (verified)

- Mutable entities as CAS-guarded version chains (D-013): chains/<root-hex>
  refs, extras.prev audit links, validated state machines for goal/change/
  proposal lifecycles.
- Honesty gates (tested): tested-requires-evidence; approve-before-integrate;
  integrated only via proposal integrate.
- Evidence: `evidence record` runs commands itself (exit code → verdict,
  capped output blob, duration metric, truncation flag, signal →
  inconclusive); `evidence add` for opinions (deterministic=false default).
- Evaluations: deterministic aggregation (from-evidence; all-pass/any-fail/
  worst-per-dimension; opinion-labeled notes) vs explicit --ai opinions.
- proposal integrate: one txn moves position ref + proposal chain + change
  chain; conflict ⇒ exit 5 zero writes; ff detection; post-commit checkout.
- CLI: goal/change/evidence/evaluation/proposal families, history --goal
  (respects -w/--from), workspace create --author.
- docs/AGENT_WORKFLOW.md: REAL transcript of two agents on one goal
  (qwen-coder multiplication vs claude-coder addition), evidence-recorded,
  AI opinion flagged, proposal approved+integrated, goal achieved.
- Tests: 9 workflow suites (incl. chain-linearity concurrency, crash
  atomicity at proposal:before/after_txn, evidence truncation/signal,
  honesty gates) + two-agent e2e.

## Current task (next iteration)

**Iteration 7: verify (fsck) + gc + chaos/failure-injection suite.**
Completion condition:
1. `newgit verify [--deep] [--json]`: object digests, envelope integrity,
   ref targets exist + type-correct, chain heads resolvable + prev-links
   walk, tree acyclicity + entry validation, snapshot parent existence,
   workspace meta/index/ref consistency, orphan workspace debris, leftover
   journals/locks, quarantine inventory; exit 3 on any problem; repair
   suggestions in messages (no silent auto-repair except documented sweeps).
2. `newgit gc [--dry-run]`: reachability from refs + chains + workspace
   positions + reflogs (bounded window) + HEAD; unreachable objects
   removed atomically (mark-sweep with grace period); never runs during
   active txns; quarantine dir compacted only when empty-of-active.
3. Chaos suite: randomized op sequences (snapshot/integrate/discard/
   rollback) × random fault points × child-process kills, then verify +
   status consistency assertions (property-style, seeded, reproducible).
4. Benchmarks scaffold (criterion-free: `newgit-bench` bin or #[ignore]
   tests recording to docs/BENCHMARKS.md) for: snapshot 1k/10k files,
   status cached/uncached, diff sizes, integrate throughput.
5. Docs + state updates; commit.

## Next tasks (ordered)

7. verify (fsck) + gc + chaos/failure-injection suite.
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
