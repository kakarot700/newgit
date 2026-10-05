# Platform support

NewGit does not claim that it works on every operating system. Platform status is tied to native test evidence for a specific source revision; a target list in workflow YAML or a cross-compiled binary is not a passing test.

## Native CI targets

| Operating system | Architecture | GitHub-hosted runner | Native checks |
|---|---|---|---|
| Linux | x86_64 | `ubuntu-24.04` | Formatting, warnings-denied Clippy, full debug suite, release build, checksummed CI binary |
| Linux | ARM64 | `ubuntu-24.04-arm` | Formatting, warnings-denied Clippy, full debug suite, release build, checksummed CI binary |
| macOS | Intel x86_64 | `macos-15-intel` | Formatting, warnings-denied Clippy, full debug suite, release build, checksummed CI binary |
| macOS | Apple silicon ARM64 | `macos-15` | Formatting, warnings-denied Clippy, full debug suite, release build, checksummed CI binary |
| Windows | x86_64 | `windows-2025` | Formatting, warnings-denied Clippy, full debug suite, release build, checksummed CI binary |
| Windows | ARM64 | `windows-11-arm` | Formatting, warnings-denied Clippy, full debug suite, release build, checksummed CI binary |

The authoritative evidence for a commit is its actual native matrix run in [GitHub Actions](https://github.com/kakarot700/newgit/actions/workflows/ci.yml). A platform is **SUPPORTED for that commit** only when its native job completes successfully. A target that has not passed that job is **NOT TESTED** for that commit; a successful cross-target `cargo check` or Clippy run is useful compile evidence, not native runtime evidence. The separate release-package job waits for every matrix job. Each native upload is a target-named `.tar.gz` bundle containing the executable binary, SHA-256 sidecar, and metadata naming the source commit, clean-checkout state, target, operating system, architecture, Rust compiler, and Git version. CI executes the staged binary before upload; the tar layer preserves POSIX execute permissions when extracted.

Current tagged release archives are Linux x86_64 GNU only. The native per-target CI binaries are verification artifacts and are not currently published as release archives.

## First exact-SHA matrix result (2026-10-05)

The first six-target run was for commit `de72d2c72f883626dbe0abc82774d112f421d219`: [CI run 37259546829](https://github.com/kakarot700/newgit/actions/runs/37259546829). These results apply to that SHA only; its test-only follow-up is recorded next.

| Target | Result on `de72d2c` |
|---|---|
| Linux x86_64 | Passed; artifact SHA-256 and metadata verified |
| Linux ARM64 | Passed; artifact SHA-256 and metadata verified |
| macOS Intel x86_64 | Failed in the chaos suite: test retried recovery before the configured age-only stale-lock threshold |
| macOS Apple silicon ARM64 | Failed in the chaos suite: same stale-lock-age test assumption |
| Windows x86_64 | Failed in the chaos suite: same stale-lock-age test assumption |
| Windows ARM64 | Failed in `upload_pack_projection_preserves_head_and_objects_without_checkout` at Git `symbolic-ref HEAD`; the follow-up captured `fatal: unable to access 'NUL': Invalid argument` |

The common format/lint/test/build job and dependency/SBOM checks passed. [CodeQL run 37259546815](https://github.com/kakarot700/newgit/actions/runs/37259546815) passed on the same SHA. Release packaging was skipped because four platform jobs failed. The Windows process-tree deadline regression passed in both Windows jobs despite their other test failures. Until a later exact-SHA matrix passes, do not treat these partial results as broad cross-platform support evidence.

## Follow-up exact-SHA matrix result (2026-10-05)

The test-only follow-up commit `eeb6e09351531f6320543c6775adbe02ed69797e` ran as [CI run 37260495371](https://github.com/kakarot700/newgit/actions/runs/37260495371); its [CodeQL run 37260495354](https://github.com/kakarot700/newgit/actions/runs/37260495354) passed. The shared format/lint/test/build job and dependency/SBOM checks passed; release packaging was skipped.

| Target | Result on `eeb6e093` |
|---|---|
| Linux x86_64 | Passed |
| Linux ARM64 | Failed in the adversarial mid-apply recovery test: concurrent stale-lock reclaimers unlinked/recreated the lock and a journal disappeared during recovery (`NotFound`) |
| macOS x86_64 and ARM64 | Failed three of six chaos seeds because crash recovery retried before the 300-second age-based fallback |
| Windows x86_64 | Failed three of six chaos seeds for the same age-fallback assumption |
| Windows ARM64 | Failed the Git projection test because Git treated `NUL` as a pathname (`fatal: unable to access 'NUL': Invalid argument`) |

The Linux ARM64 failure revealed a production check-then-unlink race, not merely a test-timing issue. The current working-tree follow-up replaces O_EXCL stale reclamation with stable kernel advisory locks and uses real empty Git config files. Its native exact-SHA retest is pending; none of the targets is newly certified by this uncommitted fix.

## Kernel-lock exact-SHA matrix result (2026-10-05)

Commit `e3dd818462e338185f2a8cc9a5dd6f740067dbd8` ran as [CI 37263992593](https://github.com/kakarot700/newgit/actions/runs/37263992593); [CodeQL 37263992598](https://github.com/kakarot700/newgit/actions/runs/37263992598) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 native jobs passed. Both Windows jobs failed the lock exclusivity test because it tried to open/read the sidecar while an exclusive Windows lock handle was live; the test was corrected to inspect the persistent empty sidecar after unlock. Shared dependency/SBOM checks passed; release packaging was skipped.

## Windows ref-list and macOS contention exact-SHA matrix result (2026-10-05)

Commit `6990dcc4387990b75711dbaa56c5f1e21efc105d` ran as [CI 37264636052](https://github.com/kakarot700/newgit/actions/runs/37264636052); [CodeQL 37264635990](https://github.com/kakarot700/newgit/actions/runs/37264635990) passed on the same SHA. Linux x86_64/ARM64 and macOS ARM64 native jobs passed. Windows x86_64/ARM64 failed the CLI E2E because (1) the test asserted LF bytes in a Git worktree that used CRLF, and (2) nested Windows paths were stringified with backslashes, so valid refs were rejected and pulls updated none. macOS x86_64 failed `concurrent_recovery_and_writes` when `Repo::open` returned the documented bounded `LockBusy` error after ten seconds of sustained writer contention and the test unwrapped it. The pending candidate joins ref path components with `/`, asserts canonical Git blob bytes, and retries `LockBusy` in the stress reader while retaining checks for other errors. Dependency/SBOM checks passed; release packaging was skipped. These results do not certify the pending candidate.

## Windows Git checkout-policy exact-SHA result (2026-10-05)

Commit `ef77bff94c76b48d1295d9c2a1989ce5c184e3a4` ran as [CI 37265829724](https://github.com/kakarot700/newgit/actions/runs/37265829724); [CodeQL 37265829698](https://github.com/kakarot700/newgit/actions/runs/37265829698) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 native jobs passed. Both Windows jobs failed eight `git_compat` tests: seven clean-worktree or byte assertions observed CRLF output, and one fixture attempted to create a quoted filename, which Windows rejects. The shared format/lint/test/build and dependency/SBOM jobs passed; reproducible release packaging was skipped.

The follow-up `ece651f` run below exercised the local correction on both Windows targets: all 28 `git_compat` tests passed on each. That later matrix still failed in a separate raw HTTP status-probe E2E; the Content-Length status-reader correction for that probe remains pending hosted verification.

## Windows raw HTTP status-probe exact-SHA result (2026-10-05)

Commit `ece651fd69a7694830bc11097bdc5d46f91074c5` ran as [CI 37267038929](https://github.com/kakarot700/newgit/actions/runs/37267038929); [CodeQL 37267038890](https://github.com/kakarot700/newgit/actions/runs/37267038890) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 native jobs passed; the shared format/lint/test/build and dependency/SBOM jobs also passed. Both Windows jobs passed all 28 Git compatibility tests, including the prior checkout-policy correction, but failed the real-Git smart-HTTP E2E when `TcpStream::read_to_end` in the raw `/info/refs` status probes returned WSAECONNRESET (10054) while waiting for EOF. The logs did not record how many response bytes arrived. Reproducible release packaging was skipped.

The status-probe framing correction was included in exact-SHA `804ec488`; the Windows ARM64 E2E progressed past those probes and then exposed a separate raw receive-advertisement helper still using `read_to_end`. The current local generic reader covers all raw HTTP helpers, returns the declared body, and fails on truncation; it does not change server runtime behavior. The accompanying bounded `LockBusy` retry change is test-only. Both fixes pass local Linux tests and cross-target Clippy but remain unhosted.

## Windows raw-advertisement reset and lock-contention exact-SHA result (2026-10-05)

Commit `804ec488977b877fc6e5424e443bd78c99d1af19` ran as [CI 37268005334](https://github.com/kakarot700/newgit/actions/runs/37268005334); [CodeQL 37268005330](https://github.com/kakarot700/newgit/actions/runs/37268005330) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 native jobs passed, as did shared format/lint/test/build and dependency/SBOM checks. Windows x86_64 failed `parallel_multiref_transactions_all_succeed` because `txn::execute` returned documented `LockBusy` and the test unwrapped it; Windows ARM64 passed all 28 `git_compat` tests but failed the smart-HTTP E2E when `raw_git_receive_advertisement_with_protocol` used `read_to_end` and received WSAECONNRESET (10054). The x86_64 job stopped at the earlier concurrency failure before later integration binaries ran. Release packaging and tag publication were skipped.

The local test-only corrections now use one bounded Content-Length parser for raw status, receive-advertisement, POST-status, and Linux raw responses; it returns the exact declared body and rejects truncation. Multiref and concurrent-recovery writer stress operations retry `LockBusy` for at most eight attempts, retain final-state and atomic-pair assertions, and fail on other errors or exhaustion. Full local debug/release suites pass 324 tests each and warnings-denied Clippy passes on all six target triples. These results do not certify a native Windows run, a complete matrix, or a release package.

## Known platform boundaries

* Git tree modes preserve executable-bit metadata across import/export. Unix checkout applies executable permissions; Windows files do not have the same executable-bit behavior.
* Git symlink objects are preserved. Unix checkout creates native symlinks. Non-Unix checkout rejects a tree containing symlinks before it writes any files. Windows snapshot capture still depends on the host allowing symlink creation; the Git interoperability fixture stages the symlink in the index so the conversion test itself does not require that privilege.
* Windows filesystem paths reject reserved characters and device names, alternate-data-stream spellings, and trailing dots or spaces. Checkout preflights paths and detects case-folded collisions on Windows and macOS. This conservative check does not implement every filesystem's Unicode normalization or case-folding rule.
* Token files get best-effort mode `0600` on Unix. On Windows, their access control comes from the containing directory's inherited ACL; NewGit does not inspect or change that ACL.
* File contents are synced before rename, but directory fsync is best-effort. Locking now uses stable kernel advisory-lock files and releases on process exit; all concurrent processes must use the same lock protocol, and network-filesystem lock/atomicity behavior is not established. The adversarial smart-HTTP mid-transaction snapshot test is Linux-only. The stable-lock implementation has partial native evidence, but no single exact-SHA matrix has passed all six targets; full platform certification remains gated.
* Git interoperability is exercised with the installed Git on each native runner when its job runs; local detailed baseline evidence is Git 2.43.0/Linux. The workflow requires Git 2.34.0 or newer for the SSH-signature fixtures, but that requirement is not a claim that every Git feature is compatible from that version onward.
