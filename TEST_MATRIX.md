# TEST_MATRIX.md — what is tested, where, and how to run it

Rule: a claim of "tested" requires an exact command + observed result.
Latest full run recorded at the bottom (updated each iteration).

## Commands

```bash
export RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
cd /home/user/newgit
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                          # unit (src/**) + integration (tests/**)
cargo test --release                # same suites, optimized (chaos uses this)
```

## Layer map

| Layer | Location | Covers | Status |
|---|---|---|---|
| Unit | `src/**/mod tests` | hex, varint (incl. non-minimal/truncation), fsx (atomic write, locks, stale reclaim, path traversal, symlink escape), ObjectId determinism, all 9 type codecs roundtrip + garbage rejection, envelope (bit-flip, truncation sweep, type mismatch, decompression bomb), ostore (roundtrip, idempotent put, corruption, misfiling, truncation, iter/prefix, limits, temp sweep), config parse/reject | ✅ iter 1 |
| Property | `tests/property_core.rs` | hex/varint/base64 roundtrips; blob/tree/snapshot/actor canonical roundtrips; total decoder (no panics on arbitrary bytes); envelope single-bit-flip detection; canonical uniqueness under re-sort; **diff reconstructs + deterministic** | ✅ iter 2–4 (11 suites × 256 cases) |
| Integration | `tests/txn_recovery.rs`, `tests/ops_snapshot.rs`, `tests/version.rs`, `tests/verify_gc.rs` | ref CRUD/CAS/reflog, txn atomicity, quarantine, snapshot/status/history/workspace cycles, checkout safety, VERSION sync, **verify corruption classes + read-only invariant, gc reachability/grace/forensics/debris** | ✅ iter 2–7 |
| E2E (CLI) | `tests/cli_e2e.rs` | full workflow, JSON envelopes, exit codes, discovery, determinism, debug logging, merge/rollback flows, **two-agent goal workflow**, **verify/gc/recover contract (exit 3, forensics)**, **import-git/export-git CLI contracts + error exits** | ✅ iter 3–8 (12 suites) |
| Concurrency | `tests/concurrency_refs.rs` | CAS races (one winner/version, reflog count equality), parallel multi-ref txns, concurrent object writes, concurrent open/recover vs writers | ✅ iter 2 (workspace races: iter 3) |
| Crash/failure injection | `tests/txn_recovery.rs` + `newgit-faultlab` | child-process aborts at 5 txn fault points; forward-recovery, partial-apply completion, reflog dedup, quarantine | ✅ iter 2 (ostore crash points: iter 3) |
| Chaos | `tests/chaos.rs` | 6 fixed xorshift64* seeds × 14–25 random ops (snapshot/ws-create/integrate/put-blob/txn-set) killed at random fault points; after EVERY step: auto-recovery, deep verify zero errors, refs resolve, status computes; end-of-seed gc + history walk | ✅ iter 7 (6 suites) |
| Fuzz-like | `tests/fuzz_parsers.rs` | 130k seeded prefix-anchored garbage inputs vs envelope/canonical/index/journal/config/hex/base64/ref-grammar + **fast-export stream** parsers (no panics, no OOM); remote-protocol parsers join in iter 9 | ✅ iter 7–8 (7 suites) |
| Compatibility | `tests/git_compat.rs` | REAL system-git repos (branches, merges, annotated+light tags, binary, symlink, exec bit, unicode, renames, empty commits, distinct author/committer, remotes+notes refs): import equality vs `ls-tree`/`cat-file`/`log`, import determinism, export round-trip (byte-identical blob SHAs + identity multiset + clean worktree), submodule refusal atomicity, empty repo, ref-move atomicity, export refusals, reimport stability | ✅ iter 8 (9 suites) |
| Performance | `src/bin/newgit-bench.rs` + docs/BENCHMARKS.md | put_blob / snapshot 1k+5k cold+warm / status cached+uncached / diff / history / integrate / verify / gc — real recorded numbers + regression policy | ✅ iter 7 (release re-check: iter 11) |

## Invariant test registry (each must have ≥1 permanent test)

| # | Invariant | Test(s) |
|---|---|---|
| I1 | identical content ⇒ identical id | `blob_identity_deterministic`, `deterministic_encoding` |
| I2 | objects immutable; put never overwrites differing content | `misfiled_object_detected`, `put_get_roundtrip` |
| I3 | any single-bit corruption is detected, never silently read | `corruption_detected`, `detects_bit_flip`, `truncated_file_detected` |
| I4 | decoders never panic on arbitrary bytes | `truncated_input_never_panics`, `truncated_envelopes_never_panic`, `decode_rejects_garbage`, `tests/fuzz_parsers.rs` (6 suites, 110k inputs) ✅ |
| I5 | decompression bombs bounded | `bomb_protection` |
| I6 | path safety: traversal/NUL/absolute/symlink escape rejected | `path_checks`, `symlink_escape_is_rejected` |
| I7 | locks exclusive; stale locks reclaimed only after timeout | `lock_is_exclusive`, `stale_lock_is_reclaimed` |
| I8 | atomic writes leave no partial files/debris | `atomic_write_is_durable_and_replaces` |
| I9 | config: unknown keys/versions rejected loudly | `config_rejects_unknown_and_bad` |
| I10 | set-fields strictly ascending ⇒ unique canonical form | `tree_roundtrip_and_ordering`, `decode_rejects_garbage` |
| I11 | reachable objects never deleted by GC | `gc_removes_unreachable_keeps_reachable`, `gc_keeps_reflog_referenced_objects`, `gc_preserves_chain_history`, `gc_never_touches_quarantine`, `gc_keeps_unreadable_objects_and_reports_missing_links`, chaos end-of-seed gc+history walk ✅ |
| I12 | transactions all-or-nothing across process death | `crash_before_journal_leaves_no_trace`, `crash_after_journal_commits_on_recovery`, `crash_mid_apply_finishes_on_recovery`, `crash_before_complete_marker_recovers_and_dedupes_reflog`, `repeated_recovery_is_stable` ✅ |
| I13 | integration atomic; failure leaves no partial state | `integrate_conflict_writes_nothing`, `integrate_three_way_merge_atomic_and_checked_out`, `integrate_concurrent_same_target_serializes` ✅ |
| I14 | verify detects every injected corruption class | `corrupt_object_detected`, `truncated_object_detected`, `misfiled_object_detected`, `bad_layout_detected`, `reflog_malformed_detected`, `ref_target_missing_detected`, `chain_tampering_detected`, `verify_never_modifies_the_repository` ✅ |
| I15 | evidence honesty flags survive roundtrip; claims ≠ facts | `change_lifecycle_honesty_gates`, evidence/proposal tests in `tests/ops_snapshot.rs` ✅ |
| I16 | git import/export preserves trees, history, modes | `import_matches_git_content_exactly`, `export_roundtrip_matches_git` ✅ |
| I17 | git import is deterministic (same repo ⇒ same oids) | `import_is_deterministic` ✅ |
| I18 | unsupported git content (submodules) fails loudly and atomically — zero refs move | `submodule_import_is_refused_atomically` ✅ |
| I19 | export→reimport is a fixpoint on trees/messages | `reimport_after_export_is_stable` ✅ |

## Latest recorded run

- Date: 2026-10-04 (iteration 8)
- `cargo test`: **237 passed; 0 failed** (108 lib unit, 6 chaos, 12 cli_e2e,
  4 concurrency_refs, 8 diff_engine, 7 fuzz_parsers, 9 git_compat,
  18 merge_integrate, 13 ops_snapshot, 12 property_core, 10 txn_recovery,
  20 verify_gc, 1 version, 9 workflow)
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: clean
