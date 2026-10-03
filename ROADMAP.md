# ROADMAP.md — NewGit

Bounded vertical slices; each ends with tests green + docs updated + commit.
Status: `[ ]` todo · `[~]` in progress · `[x]` done (with test evidence).

## Iteration 1 — Core object/storage layer  [x]
- ObjectId (SHA-256), canonical codecs for 9 object types, NGOB envelope.
- ObjectStore: atomic writes, verified reads, corruption detection, prefix resolution.
- fsx primitives (atomic write, locks, path safety), fault injection, config/limits.
- 37 unit tests; clippy/fmt clean.

## Iteration 2 — Repository skeleton, refs, transactions  [ ]
- `.newgit/` layout, Repo init/open, HEAD handling, actor registry.
- Refs with validation, lock+CAS updates, reflog.
- Journaled transactions (write-ahead, idempotent redo), crash-recovery tests.

## Iteration 3 — Workspaces, snapshots, status, CLI foundation  [ ]
- Workspace create/list/discard/checkpoint; filesystem walk with limits+symlink safety.
- Tree building, `snapshot`, `status` (index-accelerated), `history`/`log`.
- CLI binary: hand-rolled parser, `--json`, exit codes, `init`, `hash-object`, `cat`.
- E2E tests through the CLI subprocess.

## Iteration 4 — Diff engine  [ ]
- Myers line diff, unified + JSON output, rename detection, binary/large-file handling.
- Property tests: patch reconstruction, determinism.

## Iteration 5 — Merge/integration engine  [ ]
- 3-way tree merge; diff3 content merge; conflict records; integrate/rollback.
- Atomic integration via transactions; interrupted-integration recovery tests.
- Concurrency/race tests (parallel integrates on same ref).

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
