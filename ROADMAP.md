# ROADMAP.md — NewGit

Bounded vertical slices; each ends with tests green + docs updated + commit.
Status: `[ ]` todo · `[~]` in progress · `[x]` done (with test evidence).

## Iteration 1 — Core object/storage layer  [x]
- ObjectId (SHA-256), canonical codecs for 9 object types, NGOB envelope.
- ObjectStore: atomic writes, verified reads, corruption detection, prefix resolution.
- fsx primitives (atomic write, locks, path safety), fault injection, config/limits.
- 37 unit tests; clippy/fmt clean.

## Iteration 2 — Repository skeleton, refs, transactions  [x]
- `.newgit/` layout, Repo init/open/discover, HEAD handling, actor registry. ✅
- Refs with validation, lock+CAS updates, reflog (txn-id dedup). ✅
- Journaled transactions (write-ahead, idempotent redo), 5 crash-recovery
  scenarios via child-process aborts, 4 concurrency suites. ✅ (74 tests total)

## Iteration 3 — Workspaces, snapshots, status, CLI foundation  [x]
- Workspace create/list/show/discard; safe walk (limits, symlink policy,
  ignores); tree building; snapshot/status/history ops. ✅
- CLI binary `newgit` (11 commands, `--json`, stable exit codes, --debug
  JSONL); docs/CLI.md. ✅
- E2E subprocess tests (7 suites) + ops integration (13) + crash tests for
  snapshot/workspace-create. ✅ (120 tests total)

## Iteration 4 — Diff engine  [x]
- Myers O(ND) line diff (bounded edit distance, coarse-exact fallback),
  unified + JSON renderers, binary/symlink/mode handling. ✅
- Two-stage deterministic rename detection (exact + similarity ≥50%). ✅
- CLI `newgit diff` (worktree vs position, snapshot↔snapshot, refs, ws:). ✅
- Property tests: reconstruction + determinism; 21 new tests (total 141). ✅
- Found & fixed: racily-clean index race (D-011). ✅

## Iteration 5 — Merge/integration engine  [x]
- 3-way tree merge: per-path resolution, exact rename tracking, mode
  combining, modify/delete + rename/rename + rename/delete + dir/file
  conflicts; diff3 content merge with marker blobs stored for inspection. ✅
- Ancestry: is_ancestor + deterministic merge_base (bounded traversal). ✅
- `integrate` (atomic; up-to-date/fast-forward/merge; conflict ⇒ exit 5,
  nothing written), `merge-tree` dry run (exit 5 on conflicts), `rollback`
  (new snapshot with old tree; merge-role aware), `checkout` (repair/resync
  with tracked-file removal). ✅
- Crash tests: integ:before_txn (nothing changes), integ:after_txn (ref
  durable, stale files visible to status, checkout repairs). ✅
- Concurrency: same-target integrates serialize on workspace lock; exactly
  one merge snapshot; reflog counted. ✅
- 35 new tests (total 176). ✅


## Iteration 6 — Goals, Changes, Evidence, Evaluations, Proposals  [x]
- Version chains (chains/ refs, CAS, prev-links) + validated state
  machines + honesty gates (tested-requires-evidence, approve-before-
  integrate). ✅
- Evidence: runner-recorded (deterministic, exit-code verdicts, capped
  output) vs opinions (flagged); Evaluations: aggregation vs --ai. ✅
- Atomic proposal integrate (position + 2 chains in one txn) with
  conflict-abort and crash tests. ✅
- CLI families + history --goal; docs/AGENT_WORKFLOW.md real two-agent
  transcript; 20 new tests (total 186). ✅


## Iteration 7 — verify + gc + chaos  [x]
- `newgit verify [--deep]` (fsck): objects (layout/name/envelope/digest/
  misfiled/canonicality + deep link walk), refs/HEAD/reflogs, chains
  (head/prev/cycle/type), workspaces (meta/files/refs/index cache), txn dir,
  config; stable issue codes; errors ⇒ exit 3; strictly read-only (D-014). ✅
- `newgit gc [--dry-run|--force-now]`: roots = HEAD + refs + reflogs +
  workspace bases; extras.prev-aware mark; txn lock held; 24 h grace window;
  never deletes corrupt/misfiled/quarantine/debris; `newgit recover` CLI. ✅
- Journal checkpointing (D-015): txn dir bounded; found by e2e test. ✅
- Chaos suite: 6 seeds × 14–25 random ops × random fault-point child kills;
  per-step invariants (auto-recovery, deep verify clean, refs resolve,
  status computes); end-of-seed gc + history walk. Found 2 real bugs. ✅
- Fuzz-like parser firehose (110k inputs, 7 parsers, no panics). ✅
- Benchmarks: `newgit-bench` (no deps) + real numbers in docs/BENCHMARKS.md
  (pulled forward from iteration 11). ✅
- 33 new tests (total 219); fmt/clippy clean. ✅

## Iteration 8 — Git compatibility  [x] ✅ 2026-10-04
- DONE: `src/gitio/` (total fast-export parser + deterministic fast-import
  emitter), `newgit import-git` / `export-git` (--json, reports, obs events).
- DONE: 9 `tests/git_compat.rs` suites vs REAL git repos + cli_e2e contract
  + parser fuzz sweep; round-trip: byte-identical blob SHAs, identity/
  timestamp multiset equality, first-parent lineage, reimport fixpoint.
- DONE: docs/GIT_COMPAT.md mapping + limits; D-016; THREAT_MODEL import
  rows; KNOWN_LIMITATIONS #20–26. Submodules refused atomically; annotated
  tags stripped + reported; ms→s export precision loss documented.
- `newgit import-git` (parse `git fast-export`), `newgit export-git` (emit `git fast-import`).
- Compatibility tests against real git repos (history, trees, modes, messages).
- Migration guide + exact documented limitations.

## Iteration 9 — Remote protocol + server  [x] ✅ 2026-10-04
- DONE: docs/PROTOCOL.md (normative v1); std-only HTTP/1.1 server
  (`newgit serve`, bounded threads, timeouts, size caps, port-0 announcement).
- DONE: negotiated push/pull (have-probe BFS + closure-delta, dependency-
  ordered batches, incremental: re-push sends 0 objects), refs CAS in one
  server-side transaction, non-fast-forward refusal (git semantics).
- DONE: bearer tokens hashed at rest + roles read/write/admin + audit log
  (`newgit token`, `newgit audit`); invalid tokens never downgrade.
- DONE: 14 remote_e2e suites (real TCP) + 2 CLI suites + HTTP/wire fuzz
  sweep; THREAT_MODEL §E fully realized with test names; D-017.

## Iteration 10 — Web UI + agent API + MCP  [ ]
- Embedded single-file UI: dashboard, history, changes, goals, proposals, evidence,
  comparison, verification; communicates GOAL→CHANGE→EVIDENCE→PROPOSAL→INTEGRATION.
- Agent integration guide; `newgit mcp` stdio JSON-RPC server (stretch).

## Iteration 11 — Performance, release engineering, docs completion  [ ]
- Benchmark harness + docs/BENCHMARKS.md (init/snapshot/status/diff/verify/gc/import).
- Reproducible-ish releases: dist script, sha256 checksums, SBOM (cargo metadata).
- Full docs set (see README index), CI hardening, cargo-audit/deny in CI.

## Iteration 12 — Final forensic audit + readiness gate  [ ]
- Hostile review passes 2–8 per subsystem; security regression tests for all findings.
- Fill RELEASE_READINESS.md gates with evidence; classify honestly.
