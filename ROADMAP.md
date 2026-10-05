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

## Iteration 14 — Cross-platform hardening and native CI evidence  [x] · 2026-10-05
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
- HISTORICAL HOSTED RESULTS (through 45b3c5b): First exact-SHA run `37259546829` on `de72d2c72f883626dbe0abc82774d112f421d219` passed both Linux targets, dependency/SBOM checks, and the common format/lint/test/build job; CodeQL `37259546815` passed. Follow-up `eeb6e09351531f6320543c6775adbe02ed69797e` exposed a Linux ARM64 stale-reclaimer race, age-fallback test assumptions on macOS/Windows, and Windows ARM64 `NUL` handling. Kernel-lock commit `e3dd818462e338185f2a8cc9a5dd6f740067dbd8` passed Linux and macOS but failed the Windows lock-sidecar test; CI `37263992593` failed and CodeQL `37263992598` passed. Follow-up `6990dcc4387990b75711dbaa56c5f1e21efc105d` passed Linux x64/ARM64 and macOS ARM64 but failed both Windows CLI E2Es and macOS x64 contention; CI `37264636052` failed and CodeQL `37264635990` passed. Latest `ef77bff94c76b48d1295d9c2a1989ce5c184e3a4` passed Linux x64/ARM64 and macOS x64/ARM64 but failed Windows x64/ARM64: each reported eight Git-compatibility failures, seven caused by CRLF/dirty-worktree assertions and one by a quoted filename forbidden on Windows. CI `37265829724` failed; CodeQL `37265829698` passed; dependency/SBOM passed and release packaging was skipped. Commit `ece651fd69a7694830bc11097bdc5d46f91074c5` [CI `37267038929`](https://github.com/kakarot700/newgit/actions/runs/37267038929) passed both Linux and both macOS jobs plus dependency/SBOM; Windows x64/ARM64 each failed `real_git_clone_fetch_pull_push_and_ls_remote_over_smart_http` when the raw status probes surfaced WSAECONNRESET (10054) from `read_to_end`. Both Windows jobs passed all 28 `git_compat` tests. [CodeQL `37267038890`](https://github.com/kakarot700/newgit/actions/runs/37267038890) passed; release packaging was skipped. Latest `804ec488977b877fc6e5424e443bd78c99d1af19` [CI `37268005334`](https://github.com/kakarot700/newgit/actions/runs/37268005334) and [CodeQL `37268005330`](https://github.com/kakarot700/newgit/actions/runs/37268005330) ran on the exact SHA: Linux x64/ARM64 and macOS x64/ARM64 plus shared/dependency checks passed, but Windows x64 failed `parallel_multiref_transactions_all_succeed` on unhandled `LockBusy` and Windows ARM64 failed the raw receive-advertisement `read_to_end` on WSAECONNRESET; `dist` was skipped. Exact-SHA `8927d36bd8ca7be5821fdbdede02ace8b3e999bb` [CI `37268942512`](https://github.com/kakarot700/newgit/actions/runs/37268942512) passed Linux x64/ARM64, macOS x64/ARM64, shared CI and dependency/SBOM; [CodeQL `37268942492`](https://github.com/kakarot700/newgit/actions/runs/37268942492) passed. Both Windows jobs passed formatting/Clippy but failed the same reserved-path assertion: `z:invalid.txt` was rejected as an absolute/prefixed path, while the test expected the later Windows-character diagnostic. Release packaging and tag publication were skipped. The next exact-SHA run, `45b3c5b1817172cbd41c11136bcc1280401fb41c`, [CI 37269773137](https://github.com/kakarot700/newgit/actions/runs/37269773137), passed Linux x86_64/ARM64, macOS x86_64/ARM64, shared checks, and dependency/SBOM; [CodeQL 37269773105](https://github.com/kakarot700/newgit/actions/runs/37269773105) passed. Both Windows jobs passed `concurrency_refs` (4), `git_remote_e2e` (7), and `ops_snapshot` (16, including reserved-path preflight), then failed the final `workflow` binary because its signal assertion assumed Unix semantics. Release packaging and tag publication were skipped.
- COMPLETED FIXES: Replace unlinkable O_EXCL PID/age locks with stable `fs4` kernel advisory locks; sidecar contents are never read/written; symlink/nonregular paths are rejected. Isolate Git with a real empty config file; Windows ref listing now joins native path components with `/`. Exported Git repositories explicitly pin local `core.autocrlf=false` to prevent default host-wide CRLF conversion without modifying user/global config; file-specific `.gitattributes` rules still apply. The Windows Unicode fixture now uses legal filenames; quote/backslash-specific names remain Unix-only. The Git E2E also checks canonical blob bytes, and the contention test retries the documented bounded `LockBusy`. Artifact testing found and fixed lost POSIX execute permissions: CI now runs the staged binary and uploads a permission-preserving tar bundle. Linux debug/release suites pass 324 tests each (0 failures, 1 ignored); formatting and warnings-denied Clippy pass on Linux plus all five non-host targets; 20/20 mid-apply repeats and 5/5 six-seed chaos-suite repeats pass; RustSec, cargo-deny, SBOM drift, and the Linux staged/tar-extracted artifact smoke test pass. The export-policy/fixture correction passed all 28 `git_compat` tests on both Windows targets in `ece651f`; its raw status probes then exposed the EOF assumption. Exact-SHA `804ec488` exercised the status framing but exposed a second unframed receive-advertisement helper on Windows ARM64 and an unhandled bounded `LockBusy` in the Windows x64 multiref stress test. The current test-only fix uses one bounded Content-Length parser for all raw HTTP helpers and retries documented `LockBusy` up to eight attempts in multiref and recovery-writer stress operations while retaining final-state and atomic-pair assertions. Linux debug/release suites pass 324 tests each and cross-target warnings-denied Clippy passes. The current Windows-only regression separately tests drive-prefix rejection, a reserved `?`, trailing dot, and `NUL` device-name rejection while asserting an empty destination. Exact-SHA `8927d36` passed the Windows concurrency and smart-HTTP binaries before its later path-test expectation failure. Exact-SHA `45b3c5b` passed the expanded path regression and those lock/HTTP suites on both Windows architectures, then failed only the final workflow binary at the Unix-specific signal expectation. The platform-aware child-status test and all other integration tests passed in both Windows native jobs on exact SHA c9ab468 attempt 2.
- BOUNDARY: Linux local checks do not establish the other five hosts. Per-target
  support is evidenced only by that target's successful native job on the exact pushed
  commit; Unicode path normalization, Windows ACLs, mixed-version lock protocol,
  network-filesystem locking, and platform-specific directory durability remain documented.
- FINAL HOSTED CERTIFICATION: Exact source SHA `c9ab4681ead4dbbb647b35af7fffe9fc9e9d23d2` passed all six native CI jobs, the dependency/SBOM gate, CodeQL, and reproducible release packaging in [CI run 37271788464 attempt 2](https://github.com/kakarot700/newgit/actions/runs/37271788464) and [CodeQL run 37271788478](https://github.com/kakarot700/newgit/actions/runs/37271788478). All six tar bundles passed metadata/checksum/extraction checks; Unix execute modes were preserved. Attempt 1 had two Windows x86_64 smart-HTTP E2E failures; the full same-SHA retry passed without source changes. Per-target hashes and remaining boundaries are recorded in `docs/PLATFORM_SUPPORT.md`.

## Iteration 15 — Opt-in exact protected Git refs  [x] · 2026-10-05
- IMPLEMENTED: Add repeatable `newgit serve --protect-ref <exact-git-ref>` settings, empty by default. Effective create/update/delete changes require admin; writers retain access to unprotected refs, readers retain existing advertisement policy, and authenticated denials map to HTTP 403 while invalid credentials remain 401.
- HARDENED: Validate names as exact supported branch/tag refs; reject patterns and malformed/unrepresentable names. Preflight every parsed command before disposable Git projection/import or canonical object promotion; reject mixed/atomic pushes without refs or object-inventory changes. Protect mapped canonical aliases too (`refs/heads/tags/X` and `refs/tags/X`), because NewGit cannot persist them as separate refs.
- TESTED: Unit policy/grammar/status tests, repeated CLI flag rejection, and real Git 2.43.0/Linux smart-HTTP tests cover writer denial, admin branch/tag creates and deletes plus branch updates, no-op and neighboring-ref behavior, non-admin reads, exact 403, atomic all-or-none rejection, unchanged canonical inventory, and alias denial.
- LOCAL GATES: Rust 1.99.0 `cargo fmt --all --check`, warnings-denied host and five-target Clippy, debug and release suites (**330 passed, 0 failed, 1 ignored each**), release build, cargo-audit, cargo-deny, SBOM drift, and `git diff --check` passed. Native feature evidence is reported only against the final commit's exact CI/CodeQL SHA; the prior `bbdc3c3` matrix is not treated as feature verification.

## Iteration 16 — Adversarial receive-pack parser hardening  [x] · 2026-10-05
- IMPLEMENTED: Bound the hand-rolled command envelope at Git's 65,520-byte
  pkt-line maximum and 256 ref updates per request; require first-command
  NUL/capability framing (including Git 2.43.0's one post-NUL separator space)
  and enforce protocol-correct pack presence for updates versus deletion-only
  batches. Pack checksums, object validation, and reachability remain delegated
  to installed Git in the disposable projection; the HTTP body cap is unchanged.
- TESTED: Fixed-seed 20,000-input parser fuzz/mutation suite; direct boundary
  tests for truncation, capabilities, pack presence, max pkt-line and command
  count; live Git 2.43.0/Linux malformed request cases assert no canonical refs
  or objects change. The existing protected-ref writer-denial test now includes
  a valid empty pack and continues to prove HTTP 403 preflight.
- VALIDATION: Full debug/release tests, lint, audit/deny, SBOM, exact-SHA six-
  target native CI, CodeQL, and packaging results are reported against the
  implementation-and-docs commit; predecessor matrix evidence is not reused.

## Iteration 17 — Bounded HTTP request parsing and framing hardening  [x] · 2026-10-05
- IMPLEMENTED: Enforce the 16 KiB request-line, 64 KiB aggregate-header, and
  128 actual header-line limits during incremental reads. Validate HTTP token
  field names and decimal Content-Length; reject duplicate singleton fields and
  simultaneous Content-Length/Transfer-Encoding. Keep extension-field
  last-value-wins semantics and Content-Length-only body handling. A single
  300-second monotonic deadline starts at TCP accept and covers the complete
  request, including headers and the declared body; reads apply the lesser of
  remaining total time and the unchanged 30-second idle timeout.
- TESTED: Parser regressions cover oversized request/header lines, repeated
  extension headers exceeding the actual-field count, whitespace before `:`,
  duplicate consumed fields, CL+TE, signed Content-Length, and extension
  compatibility. Live TCP cases assert 400 plus connection close; real Git
  2.43.0 smart-HTTP clone/fetch/pull/push tests remain green. Short-budget live
  sockets drip request headers and a delayed Content-Length body, assert 408 and
  close/worker completion, and verify a normal request returns 200.
- LOCAL GATES: Debug and release suites each **340 passed, 0 failed, 1 ignored**
  across 20 binaries; formatting, warnings-denied host/five-target Clippy,
  release build, RustSec, cargo-deny, SBOM drift, and whitespace checks passed.
  Hosted support evidence is tied to the exact combined source/docs SHA.
- BOUNDARY: The fixed 300-second deadline is not operator-configurable and may
  reject unusually slow large transfers; a slow request can hold a worker for
  up to five minutes. Reverse proxies that buffer before connecting upstream
  must separately bound client header/body reception. These residuals are
  recorded in `KNOWN_LIMITATIONS.md` and `docs/DEPLOYMENT.md`.

## Iteration 18 — Windows deadline-test portability correction [package gate pending] · 2026-10-06
- PRESERVED: Published implementation SHA `d272befba35db78452527641e0206b391d63c14f` remains immutable. The normal fast-forward test-only follow-up is `b2fc43ee332a343ddfac444399dcdc8805e91352`; production code is unchanged.
- FIXED: Windows may return WSAETIMEDOUT (10060) on a final extra-byte read after the complete 408 response and worker join. The test skips only that post-join peer-EOF probe on Windows; response status, `Connection: close`, deadline bounds, worker termination/join, and normal request assertions remain. CI runs this regression on both Windows targets before the full native suite.
- LOCAL GATES: Debug/release each **340 passed, 0 failed, 1 ignored** across 20 binaries; formatting, host/five-target Clippy, RustSec, cargo-deny, SBOM drift, reproducible build, package archive, and checksum gates passed locally.
- WORKFLOW LINT: actionlint v1.7.7's bundled label catalog flags `macos-15-intel` and `windows-11-arm` as unknown. Official GitHub hosted-runner documentation lists both labels, and exact-SHA native jobs passed on them. A temporary exact per-file ignore for those two diagnostics allowed actionlint to validate the rest of the workflow; no target assignment or repository config changed.
- HOSTED EVIDENCE: On `b2fc43e`, both Windows focused tests and full suites passed, all six native jobs passed in attempts 3 and 5, dependency/SBOM passed in attempts 4 and 5, and CodeQL run 37361584304 passed. The overall workflow is not green: the hosted reproducible-package job was cancelled without steps/logs in attempts 4 and 5, so release readiness remains pending.
- VERSIONING: This changes no shipped behavior; no `v0.1.1` tag was created, the existing `v0.1.0` tag was not moved, and Cargo remains `0.1.0`.

## Iteration 19 — deadline-test timing margin refinement (2026-10-06)
- TEST-ONLY: ARM64 Linux CI measured the body-timeout response at 552.040549 ms against a 550 ms test ceiling. Widen that elapsed-time guard to 1 s; preserve the 408, close-header, completion/join, and normal-request assertions. Production behavior and limits are unchanged.
- LOCAL: Formatting, focused regression, host warnings-denied Clippy, and debug/release each passed; debug/release each recorded **340 passed, 0 failed, 1 ignored across 20 binaries**.
- EXACT-SHA HISTORY: On docs-only `ea068dcc`, CI attempt 1 had Windows real-Git transfer failures and shared/CodeQL jobs cancelled without steps; attempt 2 passed shared checks, dependency/SBOM, Linux x86_64, and both macOS targets, but Windows x86_64/ARM64 transfer tests failed with HTTP 408/reset and ARM64 Linux hit the 2 ms test-ceiling overrun. Both focused Windows deadline regressions passed. CodeQL attempt 2 passed. The hosted package job was skipped in both attempts.
- NEXT GATE: Run exact-SHA six-target CI, CodeQL, and distribution packaging for this combined test/docs follow-up. Keep release readiness pending until the package job is actually green; do not alter tags or publish a test-only patch release.
