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
| Property | `tests/property_*.rs` | codec invariants via proptest | iter 2+ |
| Integration | `tests/*.rs` | repo workflows | iter 2+ |
| E2E (CLI) | `tests/cli_*.rs` | subprocess CLI → repo → output/exit codes | iter 3+ |
| Concurrency | `tests/concurrency.rs` | races on refs/txn/workspaces | iter 2/5 |
| Crash/failure injection | `tests/crash_*.rs` | `NEWGIT_FAULTS` kill points; recovery invariants | iter 2+ |
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
| I12 | transactions all-or-nothing across process death | iter 2 |
| I13 | integration atomic; failure leaves no partial state | iter 5 |
| I14 | verify detects every injected corruption class | iter 7 |
| I15 | evidence honesty flags survive roundtrip; claims ≠ facts | iter 6 |
| I16 | git import/export preserves trees, history, modes | iter 8 |

## Latest recorded run

- Date: 2026-10-03 (iteration 1)
- `cargo test`: **37 passed; 0 failed** (lib) + 0 doc tests
- `cargo clippy --all-targets -- -D warnings`: clean
- `cargo fmt --check`: clean
