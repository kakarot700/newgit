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


## Iteration 6 — Goals, changes, evidence, evaluations, proposals  [ ]
- Ops + CLI for the agent-native objects; relationships (goal→changes→evidence).
- Compare changes; evaluation aggregation; proposal approval state machine.
- Honesty invariants: deterministic vs claimed evidence distinguished everywhere.

## Iteration 7 — verify + gc + chaos  [ ]
- `newgit verify` (fsck): objects, refs, relationships, indexes, workspaces.
- Safe GC with grace period, reachability from all registries; interruption tests.
- Chaos suite: seeded random ops + crashes + malformed fixtures; regression capture.

## Iteration 8 — Git compatibility  [ ]
- `newgit import-git` (parse `git fast-export`), `newgit export-git` (emit `git fast-import`).
- Compatibility tests against real git repos (history, trees, modes, messages).
- Migration guide + exact documented limitations.

## Iteration 9 — Remote protocol + server  [ ]
- Versioned JSON/HTTP v1 protocol doc; minimal HTTP/1.1 server (std only).
- push/pull (object negotiation, batched transfer), refs CAS over the wire.
- Auth (hashed bearer tokens, roles), audit log, request limits; parser fuzz tests.

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
