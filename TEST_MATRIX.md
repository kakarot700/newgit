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
| Property | `tests/property_core.rs` | hex/varint/base64 roundtrips; blob/tree/snapshot/actor canonical roundtrips; total decoder (no panics on arbitrary bytes); envelope single-bit-flip detection; canonical uniqueness under re-sort | ✅ iter 2 (10 suites × 256 cases) |
| Integration | `tests/txn_recovery.rs`, `tests/version.rs` | ref CRUD/CAS/reflog, txn atomicity, quarantine, VERSION sync | ✅ iter 2 |
| E2E (CLI) | `tests/cli_*.rs` | subprocess CLI → repo → output/exit codes | iter 3+ |
| Concurrency | `tests/concurrency_refs.rs` | CAS races (one winner/version, reflog count equality), parallel multi-ref txns, concurrent object writes, concurrent open/recover vs writers | ✅ iter 2 (workspace races: iter 3) |
| Crash/failure injection | `tests/txn_recovery.rs` + `newgit-faultlab` | child-process aborts at 5 txn fault points; forward-recovery, partial-apply completion, reflog dedup, quarantine | ✅ iter 2 (ostore crash points: iter 3) |
| Chaos | `tests/chaos.rs` | seeded random op+crash sequences; `NEWGIT_CHAOS_ITERATIONS` | iter 7 |
| Fuzz-like | `tests/fuzz_*.rs` | mutated/random bytes vs all parsers (no panics, no OOM) | iter 7/9 |
| Compatibility | `tests/git_compat.rs` | real git repos via system git | iter 8 |
| Performance | `src/bin/newgit-bench.rs` + docs/BENCHMARKS.md | realistic workloads | iter 11 |

## Invariant test registry (each must have ≥1 permanent test)

| # | Invariant | Test(s) |
|---|---|---|
| I1 | identical content ⇒ identical id | `blob_identity_deterministic`, `deterministic_encoding` |
| I2 | objects immutable; put never overwrites differing content | `misfiled_object_detected`, `put_get_roundtrip` |
| I3 | any single-bit corruption is detected, never silently read | `corruption_detected`, `detects_bit_flip`, `truncated_file_detected` |
| I4 | decoders never panic on arbitrary bytes | `truncated_input_never_panics`, `truncated_envelopes_never_panic`, `decode_rejects_garbage` (+ fuzz suite iter 7) |
| I5 | decompression bombs bounded | `bomb_protection` |
| I6 | path safety: traversal/NUL/absolute/symlink escape rejected | `path_checks`, `symlink_escape_is_rejected` |
| I7 | locks exclusive; stale locks reclaimed only after timeout | `lock_is_exclusive`, `stale_lock_is_reclaimed` |
| I8 | atomic writes leave no partial files/debris | `atomic_write_is_durable_and_replaces` |
| I9 | config: unknown keys/versions rejected loudly | `config_rejects_unknown_and_bad` |
| I10 | set-fields strictly ascending ⇒ unique canonical form | `tree_roundtrip_and_ordering`, `decode_rejects_garbage` |
| I11 | reachable objects never deleted by GC | iter 7 |
| I12 | transactions all-or-nothing across process death | `crash_before_journal_leaves_no_trace`, `crash_after_journal_commits_on_recovery`, `crash_mid_apply_finishes_on_recovery`, `crash_before_complete_marker_recovers_and_dedupes_reflog`, `repeated_recovery_is_stable` ✅ |
| I13 | integration atomic; failure leaves no partial state | iter 5 |
| I14 | verify detects every injected corruption class | iter 7 |
| I15 | evidence honesty flags survive roundtrip; claims ≠ facts | iter 6 |
| I16 | git import/export preserves trees, history, modes | iter 8 |

## Latest recorded run

- Date: 2026-10-04 (iteration 2)
- `cargo test`: **74 passed; 0 failed** (49 lib unit, 10 txn_recovery,
  4 concurrency_refs, 10 property_core, 1 version)
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: clean
