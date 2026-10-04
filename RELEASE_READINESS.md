# RELEASE_READINESS.md

**Current classification: NOT PRODUCTION READY** (iteration 6 of 12 complete).

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
- [ ] verification (fsck) — iteration 7
- [ ] Git compatibility path — iteration 8

## RELIABILITY
- [x] crash-safe object writes (tmp→fsync→rename→dir fsync) + fault hooks — iteration 1
- [x] crash recovery for transactions (forward recovery, quarantine, dedup) — iteration 2 ✅ (5 abort scenarios)
- [x] corruption detection (digest, misfiling, truncation, bombs) — iteration 1
- [x→partial] concurrency testing: refs/txn races ✅ it2; integrate serialization ✅ it5; remote races due it9
- [ ] failure injection suite end-to-end — iterations 2–7
- [ ] recovery verification (chaos) — iteration 7

## SECURITY
- [x] threat model — THREAT_MODEL.md (surfaces A–G; pending tests tracked)
- [x] path safety + parser hardening tests — iteration 1
- [ ] dependency review + SBOM + cargo-audit/deny — iteration 11
- [x] no unsafe code (`#![forbid(unsafe_code)]`), no implicit execution — DESIGN + iteration 1
- [ ] access control (remote authn/authz) tested — iteration 9

## QUALITY
- [x] unit tests (101) + integration (49) + e2e (9) + property (12) — iterations 1–5
- [x→partial] integration/E2E/property suites ✅ it2–3; fuzz + chaos due it7; regression discipline active

## PERFORMANCE
- [ ] representative benchmarks — iteration 11

## OPERABILITY
- [ ] structured diagnostics/logs, health checks, deployment docs — iterations 9–11

## DOCUMENTATION
- [x] README, ARCHITECTURE, STORAGE_FORMAT, SECURITY_MODEL, THREAT_MODEL — iteration 1
- [x→partial] CLI reference ✅ it3; protocol reference, agent guide, migration guide, contributor/testing/troubleshooting/deployment — iterations 8–11

## RELEASE
- [ ] clean reproducible build + artifacts + checksums — iteration 11
- [x] CI workflow defined — iteration 1 (`.github/workflows/ci.yml`)
- [ ] CI green on hosted runner — iteration 11
