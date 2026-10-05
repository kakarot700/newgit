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

## Iteration 10 — Web UI + agent API + MCP  [x] ✅ 2026-10-04
- DONE: embedded single-file UI (`src/ui/index.html`, `include_str!`, zero external
  resources, XSS-safe by construction — textContent only, CI-asserted): dashboard,
  first-parent history with goal/change/workspace badges, universal object inspector
  (snapshot/tree browser/blob preview + all 5 workflow types), goals & proposals
  views, side-by-side compare with unified diffs, audit log; login via bearer token
  in sessionStorage; model-flow strip GOAL→CHANGE→EVIDENCE→PROPOSAL→INTEGRATION;
  AI evaluations render with a permanent "AI OPINION" badge. Read-only by design (KL #33).
- DONE: agent read endpoints on protocol v1 — `POST /v1/object` (kind+links+data,
  blobs b64), `POST /v1/diff` (CLI-identical unified rendering, UNIFIED_CAP=100),
  `GET /v1/goals|changes(?goal=)|proposals`; capabilities extended; served under
  `--ui`/`newgit ui` (static `/` route, data-free shell, no auth on the shell only).
- DONE: `newgit mcp` — stdio JSON-RPC 2.0 MCP server (protocol 2024-11-05):
  13 tools mirroring the CLI 1:1 through the identical dispatch path
  (`cli::call_json`); tool errors carry NewGit categories; zero new deps.
- DONE: docs/AGENT_GUIDE.md (curl recipes, MCP quickstart, two-agent example);
  PROTOCOL.md/CLI.md/SECURITY_MODEL §8/THREAT_MODEL §E2 updated; D-018.
- DONE: +9 tests (278 total): UI static-shell + XSS-discipline assertions,
  endpoint shapes/gates/filters vs real servers, MCP handshake/catalog/argv
  mapping/error contract, `newgit ui` + `newgit mcp` as real child processes.

## Iteration 11 — Performance, release engineering, docs completion  [x] ✅ 2026-10-04
- DONE: benchmark re-run on full code base (release): worst median drift 1.41× vs it7
  baseline — inside noise band, below 2× gate; new table recorded, it7 table archived.
- DONE: dist script (tarball + SHA256SUMS over every file, `sha256sum -c` verified);
  SBOM generator + committed SBOM.md (21 runtime / 7 build-time / 34 dev crates,
  deterministic — CI drift gate); LICENSE-MIT + LICENSE-APACHE added.
- DONE: reproducibility check — two clean release builds, bit-identical binary
  (sha256 abcd51c8…; same-host scope, KL #37).
- DONE: recorded local cargo-audit run: 1290 advisories × 63 crates → zero findings;
  cargo-deny RUN: advisories/bans/licenses/sources all ok; all 8 runtime
  build.rs scripts read & classified (no network); deny.toml committed.
- DONE: CI hardened (fake chaos knob removed; SBOM drift + audit + deny + dist +
  reproducibility jobs) — hosted execution was still pending at this historical checkpoint.
- DONE: docs set complete — DEPLOYMENT.md (systemd/nginx/caddy/tunnel), TESTING.md,
  TROUBLESHOOTING.md, CONTRIBUTING.md; README index updated.

## Iteration 12 — Final forensic audit + readiness gate  [x] ✅ 2026-10-04
- DONE: hostile review of the newest seams (remote it10 endpoints, UI routes/hash params, MCP
  argv, audit/listing bounds) + README quickstart run VERBATIM end-to-end against the real
  binary. 3 findings → 3 fixes → 3 regression tests (TEST_MATRIX "Final audit (it12)" row):
  diff internal-spec probe (check_wire_spec), unbounded listings (cap + truncated flag),
  MCP flag-shaped positionals (`--` separator). Nothing unfixed, nothing undocumented.
- DONE: all gates re-run on FINAL code: fmt ✓, clippy -D warnings ✓, 278/278 debug ✓,
  278/278 release ✓ (chaos/fault/fuzz included), SBOM drift ✓, cargo-audit ✓ 0 findings,
  cargo-deny ✓ all-ok, dual-target rebuild bit-identical (sha256 30714184…), dist 28/28 ✓.
- DONE: RELEASE_READINESS.md filled gate-by-gate; evidence-based classification:
  **PRODUCTION-CANDIDATE** — at implementation completion, hosted CI had not yet run;
  the current publication result is recorded in RELEASE_READINESS.md and
  docs/COMPLETION_REPORT.md.

## Iteration 13 — Committed smart-HTTP projection snapshots  [x] ✅ 2026-10-05
- DONE: every advertisement/upload-pack projection now acquires the shared
  transaction/GC lock, replays committed journal recovery under it, and holds it
  through refs/HEAD/history/object export. Default HEAD initialization is locked;
  GC uses the crate-private deletion method; Linux lock reclamation no longer
  steals an old lock from a live recorded PID.
- DONE: live Linux race test pauses a two-ref transaction after its first apply,
  starts direct projection readers, raw HTTP advertisement/upload-pack readers,
  and real Git `ls-remote`, proves they block, kills the writer, then verifies
  recovery yields the complete refs and packs. No cache or generation counter.
- DONE: real-Git 80/800 release benchmark records lock wait/hold plus concurrent
  reader/writer contention; exclusive-reader serialization and non-Linux/network
  filesystem boundaries are documented.
- DONE: `TEST_MATRIX.md`, `ARCHITECTURE.md`, `DECISIONS.md`, security/threat
  models, limitations, compatibility matrix, and changelog record the new
  invariant and residual cross-request/platform boundaries.

## Iteration 14 — Cross-platform hardening and native CI evidence  [~] · 2026-10-05
- IMPLEMENTED: Replace the `/dev/urandom` bearer-token source with the portable OS
  CSPRNG; isolate Git subprocesses with a real empty config file on every host; add
  Windows path/reserved-name checks and checkout preflight; refuse unsupported
  non-Unix symlink checkout before writes; add case-collision and Windows
  process-tree regressions.
- IMPLEMENTED: Add native GitHub Actions jobs for Linux x86_64/ARM64, macOS x86_64/
  ARM64, and Windows x86_64/ARM64. Each job runs fmt, warnings-denied Clippy,
  the full test suite, release build, stages and runs the native CLI, then
  uploads a target-named tar bundle containing its binary, SHA-256, and build
  metadata. `dist` is gated on the complete matrix.
- HOSTED RESULT: First exact-SHA run `37259546829` on `de72d2c72f883626dbe0abc82774d112f421d219` passed both Linux targets, dependency/SBOM checks, and the common format/lint/test/build job; CodeQL `37259546815` passed. Follow-up `eeb6e09351531f6320543c6775adbe02ed69797e` exposed a Linux ARM64 stale-reclaimer race, age-fallback test assumptions on macOS/Windows, and Windows ARM64 `NUL` handling. Kernel-lock commit `e3dd818462e338185f2a8cc9a5dd6f740067dbd8` passed Linux and macOS but failed the Windows lock-sidecar test; CI `37263992593` failed and CodeQL `37263992598` passed. Follow-up `6990dcc4387990b75711dbaa56c5f1e21efc105d` passed Linux x64/ARM64 and macOS ARM64 but failed both Windows CLI E2Es and macOS x64 contention; CI `37264636052` failed and CodeQL `37264635990` passed. Latest `ef77bff94c76b48d1295d9c2a1989ce5c184e3a4` passed Linux x64/ARM64 and macOS x64/ARM64 but failed Windows x64/ARM64: each reported eight Git-compatibility failures, seven caused by CRLF/dirty-worktree assertions and one by a quoted filename forbidden on Windows. CI `37265829724` failed; CodeQL `37265829698` passed; dependency/SBOM passed and release packaging was skipped.
- CURRENT FIX: Replace unlinkable O_EXCL PID/age locks with stable `fs4` kernel advisory locks; sidecar contents are never read/written; symlink/nonregular paths are rejected. Isolate Git with a real empty config file; Windows ref listing now joins native path components with `/`. Exported Git repositories explicitly pin local `core.autocrlf=false` to prevent default host-wide CRLF conversion without modifying user/global config; file-specific `.gitattributes` rules still apply. The Windows Unicode fixture now uses legal filenames; quote/backslash-specific names remain Unix-only. The Git E2E also checks canonical blob bytes, and the contention test retries the documented bounded `LockBusy`. Artifact testing found and fixed lost POSIX execute permissions: CI now runs the staged binary and uploads a permission-preserving tar bundle. Linux debug/release suites pass 324 tests each (0 failures, 1 ignored); formatting and warnings-denied Clippy pass on Linux plus all five non-host targets; 20/20 mid-apply repeats and 5/5 six-seed chaos-suite repeats pass; RustSec, cargo-deny, SBOM drift, and the Linux staged/tar-extracted artifact smoke test pass. The latest Windows export-policy correction is local only; exact native retest is pending.
- BOUNDARY: Linux local checks do not establish the other five hosts. Per-target
  support is evidenced only by that target's successful native job on the exact pushed
  commit; Unicode path normalization, Windows ACLs, mixed-version lock protocol,
  network-filesystem locking, and platform-specific directory durability remain documented.
