# RELEASE_READINESS.md

**Current classification: PRODUCTION-CANDIDATE** (pre-1.0; 2026-10-04).

**Hosted publication preflight:** The runner-pinned source-bearing commit
`afa94c4` (`afa94c427d5530374422dd40c46ddb6fbe87c32c`) passed [GitHub CI run
37182199247](https://github.com/kakarot700/newgit/actions/runs/37182199247)
and [CodeQL run
37182199239](https://github.com/kakarot700/newgit/actions/runs/37182199239)
on Ubuntu 24.04. Applicable CI jobs passed formatting, Clippy, debug and
release tests/build, SBOM drift, RustSec and cargo-deny checks, a second clean
release build with matching hashes, distribution packaging, and archive
checksum verification. The PR-only dependency review and tag-only release jobs
did not run on that push event.

An independent clean clone at `b648079` passed the locked build, all 278 debug
and 278 release tests, README install/CLI quick start, an agent workflow that
exercised the explicit proposal-approval command under a test reviewer identity
and then integrated the proposal, and real-Git import/export smoke checks. The
pre-publication Gitleaks scan found zero findings. GitHub secret
scanning and push protection, Dependabot alerts/security updates, private
vulnerability reporting, and read-only-by-default Actions token permissions
are enabled.

**Decision:** Keep NewGit classified as **PRODUCTION-CANDIDATE**, not
Production Ready. Passing checks apply to the tested commits and Linux x86_64
runner; they do not certify every deployment. The project remains pre-1.0,
reproducibility is verified same-host only, and the self-hosted protocol v1 is
plain HTTP. See [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md),
[THREAT_MODEL.md](THREAT_MODEL.md), and the [versioned release notes](docs/releases/v0.1.0.md).
Each pushed commit and version tag has its own Actions run; consult the live
[Actions](https://github.com/kakarot700/newgit/actions) and
[Releases](https://github.com/kakarot700/newgit/releases) pages for current
commit/tag and artifact status.

Honest gate checklist; `[x]` only with evidence (test/command reference).

## ARCHITECTURE
- [x] coherent architecture — ARCHITECTURE.md, layering enforced by modules
- [x] documented invariants — TEST_MATRIX.md invariant registry; STORAGE_FORMAT.md normative
- [x] no unexplained architectural debt blocking release — DECISIONS.md D-001…D-007

## FUNCTIONALITY
- [x] core repository operations (init/open/discover/refs/txn/HEAD/actors) — iteration 2 ✅ (74 tests)
- [x] snapshots / status / history / workspaces — iteration 3 ✅ (13 ops + 7 e2e suites)
- [x] diff engine (line + tree, renames, binary, mode, unified + JSON) — iteration 4 ✅ (8 suites + property reconstruct)
- [x] object model + store + corruption detection — iteration 1 ✅ (37 tests) COMPLETED by it7 verify layer: `verify --deep` re-encodes every object + walks every link (20 verify_gc suites), corruption/truncation/misfiling/bomb detection tested, decoders fuzzed (150k inputs, never panic), chaos suites deep-verify after every step — no partial remains
- [x] changes / goals / evidence / evaluations / proposals — iteration 6 ✅ (9 workflow suites + two-agent e2e)
- [x] integration (merge) + rollback — iteration 5 ✅ (18 suites + crash + race tests)
- [x] verification (fsck) — iteration 7 ✅ (`newgit verify [--deep]`, 20 verify_gc suites + chaos per-step)
- [x] Git compatibility path — iteration 8 ✅ (`import-git`/`export-git` via system git fast-export/fast-import; byte-identical blob round-trip + determinism + atomic refusal tested against real git; honest documented limits in docs/GIT_COMPAT.md — annotated tags lossy, no submodules)
- [x] Web UI + agent API — iteration 10 ✅ (`newgit ui` embedded single-file read-only explorer: dashboard/history/object inspector/goals/changes/evidence/proposals/compare/audit; agent HTTP read endpoints `/v1/object`, `/v1/diff`, `/v1/goals|changes|proposals`; `newgit mcp` stdio JSON-RPC 2.0 with 13 tools sharing the CLI dispatch path; docs/AGENT_GUIDE.md; zero new dependencies; UI mutations deliberately out of scope — KL #33)

## RELIABILITY
- [x] crash-safe object writes (tmp→fsync→rename→dir fsync) + fault hooks — iteration 1
- [x] crash recovery for transactions (forward recovery, quarantine, dedup) — iteration 2 ✅ (5 abort scenarios)
- [x] corruption detection (digest, misfiling, truncation, bombs) — iteration 1
- [x] concurrency testing: refs/txn races ✅ it2; integrate serialization ✅ it5; remote push races + concurrent connections ✅ it9
- [x] failure injection suite end-to-end — iterations 2–7 ✅ (30+ fault-point scenarios via newgit-faultlab)
- [x] recovery verification (chaos) — iteration 7 ✅ (tests/chaos.rs: 6 seeds × random ops × random kills; per-step deep-verify invariant)
- [x] safe gc — iteration 7 ✅ (non-destructive mark-sweep, D-014; `gc_*` suites + chaos end-of-seed)

## SECURITY
- [x] threat model — THREAT_MODEL.md (surfaces A–G + §E2 UI/MCP; it9 pending rows resolved it10)
- [x] path safety + parser hardening tests — iteration 1
- [x] dependency review + SBOM + cargo-audit/deny — SBOM.md committed w/ CI drift gate; cargo-audit RUN 2026-10-05: 1,290 advisories × 64 locked crates → zero findings; cargo-deny 0.20.2: advisories/bans/licenses/sources all ok; runtime build-script audit remains recorded in THREAT_MODEL §G; CI re-runs both audits)
- [x] no unsafe code (`#![forbid(unsafe_code)]`), no implicit execution — DESIGN + iteration 1
- [x] access control (remote authn/authz) tested — iteration 9 ✅ (bearer tokens hashed at rest, roles read<write<admin, invalid-token-never-anonymous, authz before every mutation, audit log of every request incl. failures; tested in remote_e2e + cli_e2e exit-code contracts)

## QUALITY
- [x] unit tests (128) + integration/e2e (95, incl. 16 remote + 9 git-compat + 4 UI/MCP e2e) + property (12) + verify/gc (20) + chaos (6) + fuzz-like (8) + misc (9) — iterations 1–10 (278 total)
- [x] integration/E2E/property/fuzz/chaos suites ✅ it2–7; regression discipline active (chaos seeds grow per bug found)

## PERFORMANCE
- [x] representative benchmarks — iteration 7 ✅ + iteration 11 re-check ✅ (src/bin/newgit-bench.rs + docs/BENCHMARKS.md real numbers for the full code base; worst median drift 1.41× — inside the documented shared-vCPU noise band, below the 2× gate; it7 table archived, not overwritten)

## OPERABILITY
- [x] structured diagnostics/logs (obs JSONL stderr via `--debug`/`NEWGIT_LOG=1`), health checks (`/healthz`, `verify`) ✅ it1–9; audit log + `/v1/audit` ✅ it9; deployment docs ✅ it11 (docs/DEPLOYMENT.md: systemd + hardening, TLS reverse proxy, ssh-tunnel pattern for the http-only v1 client, backup/restore/limits/upgrade)

## DOCUMENTATION
- [x] README, ARCHITECTURE, STORAGE_FORMAT, SECURITY_MODEL, THREAT_MODEL — iteration 1
- [x] full docs set ✅ it11 — CLI reference (it3/9/10), protocol (it9/10), git compat (it8), agent guide (it10), DEPLOYMENT/TESTING/TROUBLESHOOTING/CONTRIBUTING (it11), SBOM (it11), benchmarks re-run (it11); README index complete

## RELEASE
- [x→partial] clean reproducible build + artifacts + checksums — iteration 11 ✅ (two clean release builds bit-identical same-host, sha256 recorded; scripts/dist.sh tarball + SHA256SUMS verified with `sha256sum -c`; cross-host reproducibility NOT claimed — KL #37)
- [x] Native CI matrix and release gate defined — `.github/workflows/ci.yml`; actions pinned, least-privilege permissions; Linux x86_64/ARM64, macOS Intel/ARM64, Windows x86_64/ARM64. Each native job runs fmt, Clippy, the full debug suite, a release build, executes its staged CLI, and uploads a target-specific tar bundle with a checksummed binary and OS/architecture/build metadata. `dist` is gated on all six. The current tagged release archives remain Linux x86_64 GNU; other native binaries are CI verification artifacts.
- **Per-commit support evidence:** classify a target as supported only after its native job passes on that exact source SHA. Workflow configuration, cross-target compilation, and prior Linux-only runs do not establish other targets; inspect the actual Actions result before release.
- [ ] Latest six-target exact-SHA matrix green — `e3dd818462e338185f2a8cc9a5dd6f740067dbd8` [CI 37263992593](https://github.com/kakarot700/newgit/actions/runs/37263992593) failed both Windows lock-sidecar tests; `6990dcc4387990b75711dbaa56c5f1e21efc105d` [CI 37264636052](https://github.com/kakarot700/newgit/actions/runs/37264636052) passed Linux x64/ARM64 and macOS ARM64 but failed Windows x64/ARM64 CLI E2Es and macOS x64 contention. `ef77bff94c76b48d1295d9c2a1989ce5c184e3a4` passed Linux x64/ARM64 and both macOS jobs but failed Windows x64/ARM64 Git compatibility tests on CRLF worktree expectations and an invalid quoted-path fixture; [CodeQL 37265829698](https://github.com/kakarot700/newgit/actions/runs/37265829698) passed. Latest `ece651fd69a7694830bc11097bdc5d46f91074c5` [CI 37267038929](https://github.com/kakarot700/newgit/actions/runs/37267038929) passed both Linux and both macOS jobs plus dependency/SBOM, but both Windows jobs failed the raw status-probe E2E when `read_to_end` surfaced WSAECONNRESET (10054); all 28 `git_compat` tests passed on each Windows target. [CodeQL 37267038890](https://github.com/kakarot700/newgit/actions/runs/37267038890) passed; reproducible packaging was skipped. The local Content-Length response reader must pass a new exact-SHA six-target matrix before the release gate can open.
- [x] CI green on hosted runner — CI run 37182199247 on commit `afa94c4`, with applicable check, security, and reproducible-distribution jobs successful
- [x] CodeQL analysis — run 37182199239 completed successfully on the same commit
- [x] CodeQL on portability implementation and follow-up — runs 37259546815, 37260495354, 37263992598, 37264635990, 37265829698, and 37267038890 completed successfully on their respective exact SHAs; the next pushed candidate still needs its own analysis.
- [x] tag-release workflow is configured to publish only on `v*` tags after the `dist` job; main-push preflight is not itself a test of tag publication
