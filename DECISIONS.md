# DECISIONS.md — architectural decision log

Format: context → decision → rationale → consequences. Newest first.

## D-010 · All ref mutations go through the transaction engine (Iteration 2)
**Context:** Single-ref updates could bypass journaling, creating two code
paths with different crash semantics.
**Decision:** Even one-ref updates execute as 1-op WAL transactions.
**Consequences:** Uniform recovery; slight overhead (journal write per ref
update) — acceptable, measurable in iteration 11 benchmarks.

## D-009 · Commit point = journal fsync; forward recovery (Iteration 2)
**Context:** A crash between journal write and application leaves intent
durable but effects missing. Undo-style recovery would need old-value
journals; redo needs only final state.
**Decision:** Journals record final state only. Once the RUNNING journal is
durable, the transaction WILL complete (recovery redoes application).
Application is idempotent; reflog lines carry the txn id and readers dedupe,
so redo re-appends are invisible. Corrupt journals are quarantined
(`*.journal.corrupt`), never applied, never deleted silently.
**Consequences:** Simple, testable all-or-nothing semantics; `execute()`
returns error before the journal only on CAS failure (no effects at all).

## D-008 · Lock reclamation by holder liveness (Iteration 2)
**Context:** `abort()`/SIGKILL never runs destructors, so O_EXCL lock files
outlive their holders and blocked recovery for the full stale timeout (5 min)
in tests — and would in production crashes too.
**Decision:** Lock files record `pid`/`time`. On contention, reclaim iff the
recorded pid is provably dead (Linux `/proc/<pid>` probe) or age exceeds the
stale timeout. Non-Linux platforms fall back to the timeout (conservative).
**Consequences:** Fast crash recovery; pid-reuse can only delay reclamation
(never steal a live lock). Tested: `dead_holder_lock_is_reclaimed`,
`live_holder_lock_is_not_stolen`.

## D-007 · Git interoperability via fast-export/fast-import (Iteration 1)
**Context:** Full git object-format compatibility (sha1 packs, deltas) would
require a large dependency (gitoxide) or months of work.
**Decision:** Import by parsing `git fast-export` streams; export by emitting
`git fast-import` streams, driving the system `git` binary. Detect git at
runtime; degrade with a clear error when absent.
**Rationale:** fast-export/import are stable, documented git interfaces;
zero new dependencies; honest semantics (full history, authors, messages,
modes, merge parents).
**Consequences:** Requires system git for interop commands (documented in
KNOWN_LIMITATIONS.md). Non-UTF-8 git paths rejected with explicit error
(NewGit paths are UTF-8 by protocol). Tag signatures are not carried over.

## D-006 · No staging area (Iteration 1)
**Context:** Git's index/staging split is a major source of user and agent
confusion and complicates concurrent-workspace semantics.
**Decision:** A workspace holds live file state; `newgit snapshot` captures
the entire workspace atomically. Partial capture is done by using multiple
workspaces, not a staging area.
**Rationale:** Matches the product model (workspace = one actor's experiment).
Simplifies crash safety (index is only a status cache, never semantics).
**Consequences:** No `git add -p` equivalent; documented in migration guide.

## D-005 · Evidence honesty flags (Iteration 1)
**Context:** Agents can fabricate "tests passed" claims.
**Decision:** `Evidence.deterministic: bool` and `Evaluation.ai_generated:
bool` are protocol fields. Deterministic evidence is meant to be produced by
a runner that executes the tool itself (CLI `newgit evidence run`), claimed
evidence by any actor. UI/eval layers must render them distinctly. The core
never upgrades a claim to a verified fact.
**Consequences:** Policy layer (proposals/integration) can require
deterministic evidence for certain kinds.

## D-004 · Canonical encodings are a frozen protocol (Iteration 1)
**Context:** Object identity depends on byte-level canonical form.
**Decision:** docs/STORAGE_FORMAT.md is normative. Set-like fields are stored
strictly ascending + unique; strings length-prefixed UTF-8; varints minimal
(non-minimal encodings rejected at decode); no trailing bytes anywhere.
Envelope: `NGOB` magic + version + type tag + LE lengths + zlib(level 6) +
trailing SHA-256. Changing any of this requires a new format version.
**Consequences:** Deterministic ids; corruption always detectable; decoders
must never panic (tested + fuzzed).

## D-003 · SHA-256 object ids (Iteration 1)
**Context:** Git uses SHA-1 (collision-attacked); SHA-256 is the conservative
choice and matches git's own SHA-256 mode direction.
**Decision:** All NewGit ids are SHA-256 over canonical bytes, displayed as
64-char lowercase hex.
**Consequences:** No cross-hash interop with git ids; import keeps a
sha1↔nid map file (documented).

## D-002 · Minimal dependency set (Iteration 1)
**Context:** Supply-chain security + build speed on 2 CPUs.
**Decision:** Runtime deps: `sha2`, `flate2` (pure-Rust `rust_backend`, no C),
`serde`+`serde_json`, `thiserror`. Dev deps: `proptest`, `tempfile`. No CLI
framework (hand-rolled parser), no HTTP framework (hand-rolled HTTP/1.1 in
iteration 9), no async runtime.
**Rationale:** Every dependency is small, boring, and justified; fewer CVE
surfaces; reproducible builds easier.
**Consequences:** We own more code (parser, HTTP) — mitigated by dedicated
fuzz/property tests for each hand-rolled parser.

## D-001 · Rust, single crate + single binary (Iteration 1)
**Context:** Systems-language VCS engine; safety + performance + portability.
**Decision:** Rust stable (pinned 1.99.0), `#![forbid(unsafe_code)]`, one lib
crate + one `newgit` binary (workspace split deferred until build times hurt).
**Consequences:** No unsafe; compile-time enforcement of memory safety;
single artifact to ship.

## D-011 · Diff design: bounded Myers + racily-clean index guard (Iteration 4)

**Context.** The diff engine needs (a) exact, deterministic line diffs,
(b) memory safety on hostile/pathological inputs, (c) an index cache that
can never report stale content as current.

**Decision.**
1. Line diff = greedy Myers O(ND) with common prefix/suffix trimming and a
   bounded edit distance (`max_edit_distance`, default 1024 per file). When
   the bound is exceeded we emit a whole-file *replace* (still exact and
   reconstructible, just not minimal) and flag `edit_distance_capped` in
   JSON. Full-trace memory is therefore ≤ ~8 MiB per file. Hirschberg
   linear-space refinement is deferred until benchmarks justify it
   (KNOWN_LIMITATIONS #13).
2. Rename detection runs in two deterministic stages: exact (mode+oid)
   first, then similarity ≥50% via a cheap common-prefix/suffix line ratio
   (full Myers only for the *chosen* pair, at render time). Candidate
   scoring is skipped entirely when deleted×added pairs > `rename_pair_cap`
   (1000). Greedy assignment sorted by (score DESC, old path, new path) —
   same input ⇒ same output, no hash-map iteration order dependence.
3. Binary = NUL byte in first 8000 bytes (git-compatible rule). Binary
   files diff at metadata level only.
4. **Racily-clean guard**: the index cache is trusted only when
   size+mtime match AND file mtime < index-file mtime (git's rule). A file
   modified within the same timestamp tick as the index write would
   otherwise be invisible to status/snapshot; the symlink-diff test caught
   this in practice. Unit-tested in `repo::index::tests`.
5. `capture_tree(save_index)` factors the walk→hash→tree pipeline shared by
   snapshot (index-writing) and diff (read-only).

**Consequences.** Deterministic, memory-bounded diffs; correctness of
status/diff under coarse filesystem clocks; slight extra hashing for files
touched in the same tick as the last snapshot (acceptable, git-identical).

## D-012 · Merge semantics: atomic integrate, honest conflicts, no history rewriting (Iteration 5)

**Context.** Multiple actors (human + agents) must be able to work
concurrently and combine results without silent data loss or fake success.

**Decision.**
1. `integrate` is all-or-nothing at the position level: conflicts abort with
   exit 5 BEFORE any ref move or file change. (Merged-with-markers conflict
   blobs may be stored for inspection — content-addressed, unreachable,
   GC-able; they never affect semantics.)
2. Merge base = best common ancestor by (timestamp DESC, oid DESC) traversal;
   criss-cross histories with multiple maximal common ancestors pick one
   deterministically instead of git's recursive base-merge (documented
   limitation #15).
3. `parents` stays a canonically sorted set (D-004); merge ROLES live in
   `extras.merge_ours/merge_theirs` so rollback can undo "to our side"
   unambiguously.
4. Rollback never rewrites history: it creates a new snapshot with the old
   tree. Auditable, revertable, crash-safe like any snapshot.
5. Fast-forward moves the ref to the existing snapshot object (no empty
   merge commit), matching the "position" model.
6. Workspace file checkout after the durable commit is NOT journaled; a
   crash in between leaves the position ahead of the files — `status`
   reports it honestly and `newgit checkout` repairs it (tested with fault
   injection at integ:after_txn).
7. Content merge: diff3 over Myers anchors; symlinks and binaries never
   auto-merge; add/add uses an empty virtual base (git-compatible outcome);
   modify/delete and rename/delete always conflict. Exact-content rename
   tracking only (no similarity renames in merge v1).
8. Concurrent integrates on one workspace serialize on the workspace lock
   (lock acquired before reading the position); CAS remains the final guard.

**Consequences.** No silent conflict resolution, no lost work, deterministic
outputs, git-familiar semantics with explicitly documented deviations.
