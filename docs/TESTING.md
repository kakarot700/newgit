# Testing NewGit

The test pyramid, how to run each layer, and what each layer is allowed to
prove. Test-count and invariant registry: TEST_MATRIX.md (source of truth).

## Gates (run all three before any commit)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # debug profile: full suite (~30 s)
```

Timing/crash-sensitive suites additionally run in release in CI:

```bash
cargo test --release --locked
```

## Layers

| Layer | Where | Runs how |
|---|---|---|
| Unit | `src/**/mod tests` | in-process; lib internals incl. UI-asset assertions and MCP protocol unit tests |
| Property | `tests/property_core.rs` | proptest: codec roundtrips, canonical-order invariants |
| Integration | `tests/{ops_snapshot,diff_engine,merge_integrate,workflow,verify_gc,version}.rs` | real repos in tempdirs |
| E2E (CLI) | `tests/cli_e2e.rs` | spawns the REAL built binary; asserts stdout/stderr/exit codes/disk state |
| E2E (remote) | `tests/remote_e2e.rs` | REAL in-process server + REAL TCP client — no mocks anywhere |
| E2E (UI/MCP) | lib tests + `tests/cli_e2e.rs` | UI shell served over TCP; MCP driven as a child process over stdio |
| Compatibility | `tests/git_compat.rs` | REAL system `git` (fast-export/fast-import round-trips) |
| Concurrency | `tests/concurrency_refs.rs`, races inside merge/remote suites | threads + CAS assertions (exactly one winner) |
| Crash/fault injection | `tests/txn_recovery.rs` + `src/bin/newgit-faultlab.rs` | child processes killed at injected fault points |
| Chaos | `tests/chaos.rs` | 6 FIXED seeds × random op sequences × random kills; per-step `verify --deep` invariant |
| Fuzz-like | `tests/fuzz_parsers.rs` | 150k seeded prefix-anchored garbage inputs vs every parser — must never panic |
| Performance | `src/bin/newgit-bench.rs` | recorded in docs/BENCHMARKS.md; 2× median regression blocks release |

## Fault injection

The engine has fault points (`NEWGIT_FAULTS=<point>[,<point>…]`,
`NEWGIT_FAULT_MODE=abort|error`). `abort` simulates power loss
(`std::process::abort` at the point); `error` simulates an I/O failure
returned to the caller. `newgit-faultlab` is the child-process harness the
txn_recovery tests drive. Fault points are part of the crash-safety proof —
removing one requires removing its tests AND arguing the invariant still
holds (they never are; see DECISIONS D-009/D-015).

## Rules this project holds itself to

1. **No fake passes.** A test that cannot run (e.g. CI-only workflows, no
   hosted runner in the sandbox) is DOCUMENTED as not executed — never
   claimed green.
2. **No weakened tests.** Failing tests get fixed at the root cause; a test
   is only edited when the test itself encodes a wrong expectation, with the
   reason in the commit message.
3. **Real subsystems.** Remote tests use real sockets; git tests use real
   git; MCP tests use real child processes. Mocks are not used to declare
   production readiness.
4. **Every bug grows a regression test**, and chaos seeds are extended when
   a crash-recovery bug is found (seeds are fixed constants — reruns are
   deterministic).
5. Invariants (I1…I24+ in TEST_MATRIX.md) each name the tests that enforce
   them; an invariant without a test is a lie and gets deleted or tested.
