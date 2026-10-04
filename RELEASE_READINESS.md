# RELEASE_READINESS.md

**Current classification: NOT PRODUCTION READY** (iteration 7 of 12 complete — no Git compatibility, no remote/auth yet).

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
- [ ] Git compatibility path — iteration 8

## RELIABILITY
- [x] crash-safe object writes (tmp→fsync→rename→dir fsync) + fault hooks — iteration 1
- [x] crash recovery for transactions (forward recovery, quarantine, dedup) — iteration 2 ✅ (5 abort scenarios)
- [x] corruption detection (digest, misfiling, truncation, bombs) — iteration 1
- [x→partial] concurrency testing: refs/txn races ✅ it2; integrate serialization ✅ it5; remote races due it9
- [x] failure injection suite end-to-end — iterations 2–7 ✅ (30+ fault-point scenarios via newgit-faultlab)
- [x] recovery verification (chaos) — iteration 7 ✅ (tests/chaos.rs: 6 seeds × random ops × random kills; per-step deep-verify invariant)
- [x] safe gc — iteration 7 ✅ (non-destructive mark-sweep, D-014; `gc_*` suites + chaos end-of-seed)

## SECURITY
- [x] threat model — THREAT_MODEL.md (surfaces A–G; pending tests tracked)
- [x] path safety + parser hardening tests — iteration 1
- [ ] dependency review + SBOM + cargo-audit/deny — iteration 11
- [x] no unsafe code (`#![forbid(unsafe_code)]`), no implicit execution — DESIGN + iteration 1
- [ ] access control (remote authn/authz) tested — iteration 9

## QUALITY
- [x] unit tests (101) + integration (49) + e2e (11) + property (12) + verify/gc (20) + chaos (6) + fuzz-like (6) — iterations 1–7 (219 total)
- [x] integration/E2E/property/fuzz/chaos suites ✅ it2–7; regression discipline active (chaos seeds grow per bug found)

## PERFORMANCE
- [x] representative benchmarks — iteration 7 ✅ (src/bin/newgit-bench.rs + docs/BENCHMARKS.md real numbers; release re-check due it11)

## OPERABILITY
- [ ] structured diagnostics/logs, health checks, deployment docs — iterations 9–11

## DOCUMENTATION
- [x] README, ARCHITECTURE, STORAGE_FORMAT, SECURITY_MODEL, THREAT_MODEL — iteration 1
- [x→partial] CLI reference ✅ it3; protocol reference, agent guide, migration guide, contributor/testing/troubleshooting/deployment — iterations 8–11

## RELEASE
- [ ] clean reproducible build + artifacts + checksums — iteration 11
- [x] CI workflow defined — iteration 1 (`.github/workflows/ci.yml`)
- [ ] CI green on hosted runner — iteration 11
