# Testing NewGit

The test pyramid, how to run each layer, and what each layer is allowed to
prove. Test-count and invariant registry: TEST_MATRIX.md (source of truth).

## Gates (run before any commit)

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked             # debug profile: full suite
cargo test --release --locked   # includes timing/crash-sensitive suites
```

## Hosted verification

On 2026-10-04, the public main-branch preflight on commit `afa94c4` passed
[CI run 37182199247](https://github.com/kakarot700/newgit/actions/runs/37182199247)
and [CodeQL run 37182199239](https://github.com/kakarot700/newgit/actions/runs/37182199239)
on Ubuntu 24.04. CI passed formatting, Clippy, debug/release tests and build,
the committed-SBOM drift check, RustSec and cargo-deny checks, a second clean
release build with matching hashes, package creation, and archive checksums.
The PR-only dependency-review job and tag-only release job were skipped on
this push event; a version tag invokes a separate release workflow gated on
the functional, security, and distribution jobs.

The native `platform-matrix` job is defined for Linux x86_64 (`ubuntu-24.04`),
Linux ARM64 (`ubuntu-24.04-arm`), macOS Intel (`macos-15-intel`), macOS ARM64
(`macos-15`), Windows x86_64 (`windows-2025`), and Windows ARM64
(`windows-11-arm`). Each runner checks formatting, warnings-denied Clippy,
`cargo test --locked`, and `cargo build --release --locked`; distribution is
gated on all six. Each job stages its native CLI with a SHA-256 file and JSON
metadata for target, OS, architecture, source commit, Rust, and Git versions.
These runner definitions are not themselves test evidence: consult the exact
commit's Actions run and do not call a target verified unless its native job
completed successfully. Git E2E behavior and platform-specific edge cases are
limited to what each job actually executes.

The C-quoted UTF-8 Git-path implementation, commit
`b4e1ca5dd12b2d816fbd05f03416dc903a4a014a`, passed [GitHub CI run
37202630994](https://github.com/kakarot700/newgit/actions/runs/37202630994) and
[CodeQL run 37202630998](https://github.com/kakarot700/newgit/actions/runs/37202630998)
on that exact SHA. CI passed the dependency/SBOM checks, formatting, Clippy,
debug and release tests, release build, reproducible package, and checksums.

An independent clean clone at commit `b648079` also passed the locked build,
all 278 debug and release tests, the README install/CLI quick start, an agent
workflow that exercised the explicit `proposal approve` CLI step under a test
reviewer identity and then integrated the proposal, plus a real-Git
import/export smoke test. These are results for the tested Linux x86_64
environment, not a claim that every operating system or deployment is verified.

## Layers

| Layer | Where | Runs how |
|---|---|---|
| Unit | `src/**/mod tests` | in-process; lib internals incl. UI-asset assertions and MCP protocol unit tests |
| Property | `tests/property_core.rs` | proptest: codec roundtrips, canonical-order invariants |
| Integration | `tests/{ops_snapshot,diff_engine,merge_integrate,workflow,verify_gc,version}.rs` | real repos in tempdirs |
| E2E (CLI) | `tests/cli_e2e.rs` | spawns the REAL built binary; asserts stdout/stderr/exit codes/disk state |
| E2E (remote) | `tests/remote_e2e.rs` | REAL in-process server + REAL TCP client — no mocks anywhere |
| E2E (UI/MCP) | lib tests + `tests/cli_e2e.rs` | UI shell served over TCP; MCP driven as a child process over stdio |
| Compatibility | `tests/git_compat.rs` | REAL system `git` (fast-export/fast-import round-trips, including C-quoted UTF-8 pathnames) |
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

1. **No fake passes.** Record the exact command, commit, environment, and result
   for every gate. Mark hosted CI or platform checks pending until the actual
   run completes; never infer a green result from local equivalents.
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
