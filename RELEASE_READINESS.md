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
- **Per-commit support evidence:** classify a target as supported only after its native job passes on that exact source SHA. The current six-target certification is recorded in [docs/PLATFORM_SUPPORT.md](docs/PLATFORM_SUPPORT.md); workflow configuration, cross-target compilation, and prior Linux-only runs do not establish other targets.
- [x] Latest six-target native evidence — exact source SHA `c20ecff6b5a28f98f7cb9a95fc0053563ab5c949`; all six native jobs passed on [CI run 37380464977 attempt 2](https://github.com/kakarot700/newgit/actions/runs/37380464977/attempts/2), including the focused deadline regression and complete suite on both Windows architectures. Shared tests/build and dependency/SBOM passed; [CodeQL 37380465174](https://github.com/kakarot700/newgit/actions/runs/37380465174) passed on the same SHA.
- [x] Current exact-SHA reproducible release-package gate — passed for `c20ecff`. The same CI attempt completed a clean second release build with matching binary hashes, packaged the distribution, and verified package checksums. The previously cancelled `b2fc43e` package attempts remain historical; they are superseded by this exact-SHA pass, not erased.
- [x] CI green on hosted runner — CI run 37182199247 on commit `afa94c4`, with applicable check, security, and reproducible-distribution jobs successful
- [x] CodeQL analysis — run 37182199239 completed successfully on the same commit
- [x] CodeQL on portability implementation and follow-up — runs 37259546815, 37260495354, 37263992598, 37264635990, 37265829698, 37267038890, 37268005330, 37268942492, 37269773105, 37271788478, and 37361584304 completed successfully on their respective exact SHAs; each future source-bearing change needs its own analysis.
- [x] tag-release workflow is configured to publish only on `v*` tags after the `dist` job; main-push preflight is not itself a test of tag publication

## Follow-up source-change evidence — receive-pack parser hardening

The bounded Git receive-pack parser change is a source-bearing follow-up after
`c9ab4681ead4dbbb647b35af7fffe9fc9e9d23d2`; that earlier exact-SHA matrix does
not certify this change. Local test and security-gate results are recorded in
`TEST_MATRIX.md`. The final combined implementation/docs commit's six-target
native CI, CodeQL, and packaging results are cited by exact SHA in the task
completion report; no state-only follow-up commit is used to retrofit evidence.

## Follow-up source-change evidence — bounded HTTP request parsing

The HTTP/1.1 bounded-line, framing, and accept-start 300-second request-deadline
implementation was published in `d272befba35db78452527641e0206b391d63c14f`,
after predecessor `c0dd16dbc85f19fd827db1988d17bca045ba1621`. The published SHA
remains unchanged. Exact-SHA verification exposed a Windows-only test oracle
issue; the follow-up `b2fc43ee332a343ddfac444399dcdc8805e91352` changes only the
test and adds a focused Windows CI step. Production server behavior is unchanged.
See `TEST_MATRIX.md` for all predecessor failures, five same-SHA CI attempts,
and the successful native/CodeQL/security results. The hosted `dist` package job
was cancelled without steps or logs on attempts 4 and 5, so this source revision
does not have a completed package gate. Keep release readiness pending; the
existing `v0.1.0` tag was not moved and no `v0.1.1` tag was created.

## Follow-up test-only evidence — deadline-test timing margin (2026-10-06)

The docs-only commit `ea068dccdbabd2446786de6d6748cca8da8384b2` was checked by
[CI run 37372516920](https://github.com/kakarot700/newgit/actions/runs/37372516920)
and [CodeQL run 37372516949](https://github.com/kakarot700/newgit/actions/runs/37372516949).
CI attempt 1 had Windows real-Git transfer failures; shared CI and CodeQL were
cancelled before any steps, dependency/SBOM passed, and the package job was
skipped. Attempt 2 passed shared checks, dependency/SBOM, Linux x86_64, and both
macOS targets. Windows x86_64/ARM64 failed full-suite real-Git transfer tests
with HTTP 408/reset during partial-clone or shallow operations; the focused
deadline regression passed on both. ARM64 Linux missed only a 550 ms test ceiling
by 2.040549 ms. The package job was skipped. CodeQL attempt 2 passed.

This combined follow-up widens only that test assertion to 1 second; the
HTTP 408, connection-close, deadline, worker-completion, and successful-normal-
request checks stay intact. Production code, limits, and dependencies are
unchanged. Local focused/debug/release/format/Clippy checks passed. This does not
clear the hosted release gate: keep readiness pending until all six native jobs,
CodeQL, and the exact current-SHA reproducible `dist` job actually pass. Do not
move `v0.1.0` or create a `v0.1.1` tag for a test-only fix.

## Exact-SHA outcome — test-only follow-up `3fc2270` (2026-10-06)

CI [37376156693](https://github.com/kakarot700/newgit/actions/runs/37376156693)
attempts 1–3 repeatedly failed Windows real-Git transfers despite passing the
focused deadline regression on both Windows architectures. Attempt 1 failed
Windows ARM64 protected non-fast-forward with HTTP 408; attempt 2 failed Windows
x86_64 protected deletion with HTTP 408; attempt 3 failed x86_64 shallow-fetch
deepening with HTTP 408 and ARM64 partial-clone lazy fetch with a connection
reset while 49,148 body bytes remained. The four non-Windows targets passed on
attempt 3; shared checks and dependency/SBOM passed. The dependent `dist` job was
skipped on all three attempts because the native matrix failed. CodeQL
[37376156803](https://github.com/kakarot700/newgit/actions/runs/37376156803)
passed.

These follow-ups change only Windows test portability and the test timing margin;
production behavior, timeout policy, limits, and dependencies are unchanged.
Release readiness remains **pending** because the exact-SHA native matrix did
not pass and the package gate did not run. Keep `v0.1.0` unchanged; do not create
`v0.1.1` until a complete exact-SHA release gate succeeds.


## Current exact-SHA release-gate result — Windows test-only follow-up (2026-10-06)

The older pending entries above accurately describe the state before the current rerun. The current documentation-only `main` SHA `c20ecff6b5a28f98f7cb9a95fc0053563ab5c949` passed all six native jobs, shared checks, security/SBOM, and the reproducible distribution-package gate on [CI run 37380464977 attempt 2](https://github.com/kakarot700/newgit/actions/runs/37380464977/attempts/2). [CodeQL run 37380465174](https://github.com/kakarot700/newgit/actions/runs/37380465174) also passed on that exact SHA. Thus the current exact-SHA hosted release gate is green; this does not change the project's **PRODUCTION-CANDIDATE** classification.

Attempt 1's Linux ARM64 timing-bound failure and Windows x86_64 real-Git force-push HTTP 408 remain documented in `TEST_MATRIX.md`. The Windows job log did not record a per-request elapsed time, so the 408 is not classified as a demonstrated 30-second idle timeout or as a confirmed CI flake. Attempt 2 on the same SHA passed both Windows focused deadline tests and full suites, including the force-push E2E. No production code, timeout, limit, dependency, or Cargo version changed. The published implementation SHA `d272befba35db78452527641e0206b391d63c14f`, existing `v0.1.0` tag, and release history remain untouched; no `v0.1.1` tag or release was created.
