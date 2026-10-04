# RELEASE_READINESS.md

**Current classification: NOT PRODUCTION READY** (iteration 9 of 12 complete — Git compatibility and remote protocol/auth landed; no web UI, release engineering, or CI yet).

Honest gate checklist; `[x]` only with evidence (test/command reference).

## ARCHITECTURE
- [x] coherent architecture — ARCHITECTURE.md, layering enforced by modules
- [x] documented invariants — TEST_MATRIX.md invariant registry; STORAGE_FORMAT.md normative
- [x] no unexplained architectural debt blocking release — DECISIONS.md D-001…D-007

## FUNCTIONALITY
- [x] core repository operations (init/open/discover/refs/txn/HEAD/actors) — iteration 2 ✅ (74 tests)
- [x] snapshots / status / history / workspaces — iteration 3 ✅ (13 ops + 7 e2e suites)
- [x] diff engine (line + tree, renames, binary, mode, unified + JSON) — iteration 4 ✅ (8 suites + property reconstruct)
- [x→partial] object model + store + corruption detection — iteration 1 ✅ (37 tests)
- [x] changes / goals / evidence / evaluations / proposals — iteration 6 ✅ (9 workflow suites + two-agent e2e)
- [x] integration (merge) + rollback — iteration 5 ✅ (18 suites + crash + race tests)
- [x] verification (fsck) — iteration 7 ✅ (`newgit verify [--deep]`, 20 verify_gc suites + chaos per-step)
- [x] Git compatibility path — iteration 8 ✅ (`import-git`/`export-git` via system git fast-export/fast-import; byte-identical blob round-trip + determinism + atomic refusal tested against real git; honest documented limits in docs/GIT_COMPAT.md — annotated tags lossy, no submodules)

## RELIABILITY
- [x] crash-safe object writes (tmp→fsync→rename→dir fsync) + fault hooks — iteration 1
- [x] crash recovery for transactions (forward recovery, quarantine, dedup) — iteration 2 ✅ (5 abort scenarios)
- [x] corruption detection (digest, misfiling, truncation, bombs) — iteration 1
- [x] concurrency testing: refs/txn races ✅ it2; integrate serialization ✅ it5; remote push races + concurrent connections ✅ it9
- [x] failure injection suite end-to-end — iterations 2–7 ✅ (30+ fault-point scenarios via newgit-faultlab)
- [x] recovery verification (chaos) — iteration 7 ✅ (tests/chaos.rs: 6 seeds × random ops × random kills; per-step deep-verify invariant)
- [x] safe gc — iteration 7 ✅ (non-destructive mark-sweep, D-014; `gc_*` suites + chaos end-of-seed)

## SECURITY
- [x] threat model — THREAT_MODEL.md (surfaces A–G; pending tests tracked)
- [x] path safety + parser hardening tests — iteration 1
- [ ] dependency review + SBOM + cargo-audit/deny — iteration 11
- [x] no unsafe code (`#![forbid(unsafe_code)]`), no implicit execution — DESIGN + iteration 1
- [x] access control (remote authn/authz) tested — iteration 9 ✅ (bearer tokens hashed at rest, roles read<write<admin, invalid-token-never-anonymous, authz before every mutation, audit log of every request incl. failures; tested in remote_e2e + cli_e2e exit-code contracts)

## QUALITY
- [x] unit tests (123) + integration/e2e (89, incl. 14 remote + 9 git-compat) + property (12) + verify/gc (20) + chaos (6) + fuzz-like (8) + misc (11) — iterations 1–9 (269 total)
- [x] integration/E2E/property/fuzz/chaos suites ✅ it2–7; regression discipline active (chaos seeds grow per bug found)

## PERFORMANCE
- [x] representative benchmarks — iteration 7 ✅ (src/bin/newgit-bench.rs + docs/BENCHMARKS.md real numbers; release re-check due it11)

## OPERABILITY
- [ ] structured diagnostics/logs, health checks, deployment docs — iterations 9–11

## DOCUMENTATION
- [x] README, ARCHITECTURE, STORAGE_FORMAT, SECURITY_MODEL, THREAT_MODEL — iteration 1
- [x→partial] CLI reference ✅ it3 (remote sections it9); protocol reference ✅ it9 (docs/PROTOCOL.md); git compat/migration ✅ it8 (docs/GIT_COMPAT.md); agent guide ✅ (docs/AGENT_WORKFLOW.md); contributor/testing/troubleshooting/deployment — iterations 10–11

## RELEASE
- [ ] clean reproducible build + artifacts + checksums — iteration 11
- [x] CI workflow defined — iteration 1 (`.github/workflows/ci.yml`)
- [ ] CI green on hosted runner — iteration 11
