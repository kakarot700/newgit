# RELEASE_READINESS.md

**Current classification: NOT PRODUCTION READY** (iteration 2 of 12 complete).

Honest gate checklist; `[x]` only with evidence (test/command reference).

## ARCHITECTURE
- [x] coherent architecture — ARCHITECTURE.md, layering enforced by modules
- [x] documented invariants — TEST_MATRIX.md invariant registry; STORAGE_FORMAT.md normative
- [x] no unexplained architectural debt blocking release — DECISIONS.md D-001…D-007

## FUNCTIONALITY
- [x] core repository operations (init/open/discover/refs/txn/HEAD/actors) — iteration 2 ✅ (74 tests)
- [ ] snapshots / status / history — iteration 3
- [x→partial] object model + store + corruption detection — iteration 1 ✅ (37 tests)
- [ ] changes / goals / evidence / proposals — iteration 6
- [ ] workspaces — iteration 3
- [ ] integration (merge) + rollback — iteration 5
- [ ] verification (fsck) — iteration 7
- [ ] Git compatibility path — iteration 8

## RELIABILITY
- [x] crash-safe object writes (tmp→fsync→rename→dir fsync) + fault hooks — iteration 1
- [x] crash recovery for transactions (forward recovery, quarantine, dedup) — iteration 2 ✅ (5 abort scenarios)
- [x] corruption detection (digest, misfiling, truncation, bombs) — iteration 1
- [x→partial] concurrency testing: refs/txn races ✅ iteration 2; integration races due iteration 5
- [ ] failure injection suite end-to-end — iterations 2–7
- [ ] recovery verification (chaos) — iteration 7

## SECURITY
- [x] threat model — THREAT_MODEL.md (surfaces A–G; pending tests tracked)
- [x] path safety + parser hardening tests — iteration 1
- [ ] dependency review + SBOM + cargo-audit/deny — iteration 11
- [x] no unsafe code (`#![forbid(unsafe_code)]`), no implicit execution — DESIGN + iteration 1
- [ ] access control (remote authn/authz) tested — iteration 9

## QUALITY
- [x] unit tests (37) — iteration 1
- [ ] integration / E2E / property / fuzz / regression suites — iterations 2–9

## PERFORMANCE
- [ ] representative benchmarks — iteration 11

## OPERABILITY
- [ ] structured diagnostics/logs, health checks, deployment docs — iterations 9–11

## DOCUMENTATION
- [x] README, ARCHITECTURE, STORAGE_FORMAT, SECURITY_MODEL, THREAT_MODEL — iteration 1
- [ ] CLI reference, protocol reference, agent guide, migration guide, contributor/testing/troubleshooting/deployment — iterations 3–11

## RELEASE
- [ ] clean reproducible build + artifacts + checksums — iteration 11
- [x] CI workflow defined — iteration 1 (`.github/workflows/ci.yml`)
- [ ] CI green on hosted runner — iteration 11
