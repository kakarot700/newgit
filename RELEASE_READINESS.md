# RELEASE_READINESS.md

**Current classification: NOT PRODUCTION READY** (iteration 11 of 12 complete — everything through release engineering has landed; what remains is iteration 12's final forensic audit and the readiness decision itself. Current blockers are listed per-gate below — most notably CI has never executed on a hosted runner from this environment.)

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
- [x] dependency review + SBOM + cargo-audit/deny — iteration 11 ✅ (SBOM.md committed w/ CI drift gate; cargo-audit RUN: 1290 advisories × 63 crates → zero findings; cargo-deny 0.20.2 RUN: advisories/bans/licenses/sources all ok, zero warnings; all 8 runtime build.rs scripts read — rustc probes only, no network; KL #38: snapshots vs that day's DB, CI re-runs both)
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
- [x] CI workflow defined — iteration 1 (`.github/workflows/ci.yml`)
- [ ] CI green on hosted runner — BLOCKED from sandbox (no GitHub remote); workflow hardened it11 (fake knob removed, SBOM-drift/audit/deny/dist/repro jobs); EVERY gate the CI runs has been executed locally in-sandbox (fmt, clippy, tests debug+release, cargo-audit, cargo-deny, SBOM drift, dual-target repro build, dist+`sha256sum -c`) — only the Actions glue (cache/artifacts/gh-release) is unexercised (KL #38)
