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

## Last fully green exact-SHA certification — 2026-10-05

The last source revision with a fully green six-target and package gate was `c9ab4681ead4dbbb647b35af7fffe9fc9e9d23d2`. [CI run 37271788464, attempt 2](https://github.com/kakarot700/newgit/actions/runs/37271788464) passed all six native jobs, shared formatting/lint/test/build, dependency/SBOM and advisory checks, and the reproducible release-package gate. [CodeQL run 37271788478](https://github.com/kakarot700/newgit/actions/runs/37271788478) passed on the same SHA. Attempt 1 of the same CI run failed two Windows x86_64 smart-HTTP E2Es (a shallow-fetch HTTP 400 and a connection abort); attempt 2 reran the full workflow on the identical SHA without source changes and passed. Both observations are retained.

| Target | Native job ID | Bundle SHA-256 |
|---|---:|---|
| Linux x86_64 | 111642229336 | `fc4147f4b93c0973c697cb655e496bef6996ef3c67e67154e2a0c82c67c648bc` |
| Linux ARM64 | 111642229312 | `9a10ce9d4476b76c4daf9a43728b57b7e66b7c9e559b9b8f41912b3c3449bb3a` |
| macOS x86_64 | 111642229167 | `18c736e0e3f330126cbc020c93ffffb0a9d625bbb4df1c52c0ee77bbd923ae6e` |
| macOS ARM64 | 111642229291 | `22b5aad16dc17ca857b94fe4a1db9cf0f141903762d77933e6fd7a77c9926142` |
| Windows x86_64 | 111642229231 | `6fbec440da5d907014354ab4a92b708f7aa40a7a8a20322b6f74a9c675ebe86b` |
| Windows ARM64 | 111642229268 | `dadab6aade8bccb42990f42ef96b9732636ecf8e5ac17c4647cb90a686dd023d` |

The downloaded bundles passed tar listing and extraction, source-commit/clean-tree/target/OS/architecture/toolchain metadata checks, and SHA-256 sidecar plus metadata comparison. The four Unix bundles preserved their executable permission; native jobs ran each staged binary's `--version` before upload. All metadata recorded the exact SHA above and a clean source checkout, Rust `1.99.0 (b940084d7 2026-09-28)`, Git `2.55.0` on Unix, and Git `2.55.0.windows.5` on Windows. The committed SBOM records a purpose for every direct dependency and its generator fails closed on rationale drift; dependency/SBOM checks passed. Attempt-2 CI and CodeQL logs contained no matches for the common credential formats scanned (a pattern scan, not a general proof of absence).

This certification applies to the exact source revision above. The tagged release-asset publication job was not run on the main-branch push; current published release archives remain Linux x86_64 GNU only.

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

The common format/lint/test/build job and dependency/SBOM checks passed. [CodeQL run 37259546815](https://github.com/kakarot700/newgit/actions/runs/37259546815) passed on the same SHA. Release packaging was skipped because four platform jobs failed. The Windows process-tree deadline regression passed in both Windows jobs despite their other test failures. These partial results do not establish support for their respective source SHAs; the later c9ab468 exact-SHA certification is recorded above.

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

The follow-up `ece651f` run below exercised the local correction on both Windows targets: all 28 `git_compat` tests passed on each. That earlier matrix still failed in a separate raw HTTP status-probe E2E; the Content-Length correction was exercised in later exact-SHA runs, culminating in the successful c9ab468 attempt-2 matrix above.

## Windows raw HTTP status-probe exact-SHA result (2026-10-05)

Commit `ece651fd69a7694830bc11097bdc5d46f91074c5` ran as [CI 37267038929](https://github.com/kakarot700/newgit/actions/runs/37267038929); [CodeQL 37267038890](https://github.com/kakarot700/newgit/actions/runs/37267038890) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 native jobs passed; the shared format/lint/test/build and dependency/SBOM jobs also passed. Both Windows jobs passed all 28 Git compatibility tests, including the prior checkout-policy correction, but failed the real-Git smart-HTTP E2E when `TcpStream::read_to_end` in the raw `/info/refs` status probes returned WSAECONNRESET (10054) while waiting for EOF. The logs did not record how many response bytes arrived. Reproducible release packaging was skipped.

The all-helper Content-Length reader and bounded `LockBusy` retries are included in exact-SHA `8927d36`. In both Windows jobs, the `concurrency_refs` (4 tests) and `git_remote_e2e` (7 tests) binaries passed before the later reserved-path assertion failed; those exact-SHA checks provide runtime evidence for those suites. The follow-up exact-SHA `45b3c5b` passed both suites again and passed the expanded path regression on both Windows targets, but failed later in the final workflow binary as recorded below. No full matrix or release package is certified.

## Windows raw-advertisement reset and lock-contention exact-SHA result (2026-10-05)

Commit `804ec488977b877fc6e5424e443bd78c99d1af19` ran as [CI 37268005334](https://github.com/kakarot700/newgit/actions/runs/37268005334); [CodeQL 37268005330](https://github.com/kakarot700/newgit/actions/runs/37268005330) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 native jobs passed, as did shared format/lint/test/build and dependency/SBOM checks. Windows x86_64 failed `parallel_multiref_transactions_all_succeed` because `txn::execute` returned documented `LockBusy` and the test unwrapped it; Windows ARM64 passed all 28 `git_compat` tests but failed the smart-HTTP E2E when `raw_git_receive_advertisement_with_protocol` used `read_to_end` and received WSAECONNRESET (10054). The x86_64 job stopped at the earlier concurrency failure before later integration binaries ran. Release packaging and tag publication were skipped.

The local test-only corrections now use one bounded Content-Length parser for raw status, receive-advertisement, POST-status, and Linux raw responses; it returns the exact declared body and rejects truncation. Multiref and concurrent-recovery writer stress operations retry `LockBusy` for at most eight attempts, retain final-state and atomic-pair assertions, and fail on other errors or exhaustion. Full local debug/release suites pass 324 tests each and warnings-denied Clippy passes on all six target triples. These results do not certify a native Windows run, a complete matrix, or a release package.

## Windows reserved-path assertion exact-SHA result (2026-10-05)

Commit `8927d36bd8ca7be5821fdbdede02ace8b3e999bb` ran as [CI 37268942512](https://github.com/kakarot700/newgit/actions/runs/37268942512); [CodeQL 37268942492](https://github.com/kakarot700/newgit/actions/runs/37268942492) passed on the same SHA. Linux x86_64/ARM64 and macOS x86_64/ARM64 passed, as did shared format/lint/test/build and dependency/SBOM checks. Both Windows targets passed formatting and Clippy, then failed the same `tests/ops_snapshot.rs::checkout_rejects_windows_reserved_paths_before_writing` assertion: `z:invalid.txt` returned `Invalid("absolute path not allowed: \"z:invalid.txt\"")`, while the test expected the later `Windows-reserved character` diagnostic. This is a test expectation mismatch, not acceptance of an invalid path: `checkout_tree` validates every entry before destination creation or writes. This failure occurred after both Windows jobs had already passed `concurrency_refs` (4 tests) and `git_remote_e2e` (7 tests). Reproducible packaging and tag publication were skipped.

The local regression checks the actual drive-prefix diagnostic, plus Windows-reserved `?`, trailing-dot, and `NUL` device-name cases, asserting an empty destination for each. Exact-SHA `45b3c5b` confirmed all these cases pass on both Windows architectures. Full local debug/release suites pass 324 tests each, and warnings-denied Clippy passes on all six target triples. At that checkpoint, the workflow signal-status test still needed hosted runtime verification; exact-SHA c9ab468 attempt 2 later passed both Windows jobs and the full matrix above. The 45b3c5b run itself skipped release packaging.

## Windows subprocess signal-status exact-SHA result (2026-10-05)

Commit `45b3c5b1817172cbd41c11136bcc1280401fb41c` ran as [CI 37269773137](https://github.com/kakarot700/newgit/actions/runs/37269773137); [CodeQL 37269773105](https://github.com/kakarot700/newgit/actions/runs/37269773105) passed on the same SHA. Linux x86_64/ARM64, macOS x86_64/ARM64, the shared format/lint/test/build job, and dependency/SBOM checks passed. Both Windows jobs passed `concurrency_refs` (4 tests), `git_remote_e2e` (7 tests), and `ops_snapshot` (16 tests including the reserved-path regression), then failed only `workflow::evidence_record_limits_and_signals` in the final integration-test binary (8 passed, 1 failed). The fixture's `sh -c "kill -9 $$"` result was `Fail` rather than the Unix-expected `Inconclusive`. The production implementation only receives a Unix signal value under `cfg(unix)`; on Windows it sets `signal=false` and treats an available nonzero child exit code as `Fail`. At this checkpoint, the local test asserted these platform-specific semantics but both Windows jobs still failed it. Exact-SHA c9ab468 attempt 2 later passed the test and all six native jobs, as recorded in the current certification above; `dist` and tag publication were skipped for this earlier run.

## Known platform boundaries

* Git tree modes preserve executable-bit metadata across import/export. Unix checkout applies executable permissions; Windows files do not have the same executable-bit behavior.
* Git symlink objects are preserved. Unix checkout creates native symlinks. Non-Unix checkout rejects a tree containing symlinks before it writes any files. Windows snapshot capture still depends on the host allowing symlink creation; the Git interoperability fixture stages the symlink in the index so the conversion test itself does not require that privilege.
* Windows filesystem paths reject reserved characters and device names, alternate-data-stream spellings, and trailing dots or spaces. Checkout preflights paths and detects case-folded collisions on Windows and macOS. This conservative check does not implement every filesystem's Unicode normalization or case-folding rule.
* Token files get best-effort mode `0600` on Unix. On Windows, their access control comes from the containing directory's inherited ACL; NewGit does not inspect or change that ACL.
* File contents are synced before rename, but directory fsync is best-effort. Locking now uses stable kernel advisory-lock files and releases on process exit; all concurrent processes must use the same lock protocol, and network-filesystem lock/atomicity behavior is not established. The adversarial smart-HTTP mid-transaction snapshot test is Linux-only. Exact-SHA c9ab468 passed all six native jobs and the dependent package gate; the certification above is limited to that tested source revision, and network-filesystem locking/durability remains unestablished.
* Git interoperability is exercised with the installed Git on each native runner when its job runs; local detailed baseline evidence is Git 2.43.0/Linux. The workflow requires Git 2.34.0 or newer for the SSH-signature fixtures, but that requirement is not a claim that every Git feature is compatible from that version onward.

## Latest successful native matrix evidence — Windows test-only correction on `b2fc43e` (2026-10-06)

The exact source SHA `b2fc43ee332a343ddfac444399dcdc8805e91352` has successful
native evidence for all six targets in [CI run 37361584544, attempt 3](https://github.com/kakarot700/newgit/actions/runs/37361584544/attempts/3). The native jobs completed their full suites, release builds, and staged-binary checks; the Windows jobs also ran the focused deadline regression first.

| Target | Native job ID | Result on `b2fc43e` |
|---|---:|---|
| Linux x86_64 | [111949257369](https://github.com/kakarot700/newgit/actions/runs/37361584544/job/111949257369) | Passed |
| Linux ARM64 | [111949257059](https://github.com/kakarot700/newgit/actions/runs/37361584544/job/111949257059) | Passed |
| macOS x86_64 | [111949257133](https://github.com/kakarot700/newgit/actions/runs/37361584544/job/111949257133) | Passed |
| macOS ARM64 | [111949257062](https://github.com/kakarot700/newgit/actions/runs/37361584544/job/111949257062) | Passed |
| Windows x86_64 | [111949256914](https://github.com/kakarot700/newgit/actions/runs/37361584544/job/111949256914) | Focused deadline test and full suite passed |
| Windows ARM64 | [111949256918](https://github.com/kakarot700/newgit/actions/runs/37361584544/job/111949256918) | Focused deadline test and full suite passed |

The Windows-focused tests and both native suites also passed on [attempt 5](https://github.com/kakarot700/newgit/actions/runs/37361584544/attempts/5); the exact-SHA [CodeQL run 37361584304](https://github.com/kakarot700/newgit/actions/runs/37361584304) passed. This is native platform evidence, not a green release gate: the overall CI run remained failed because the dependent reproducible-package job was cancelled without steps or logs on attempts 4 and 5. No package hashes or release artifacts are claimed for `b2fc43e`.

Standalone `actionlint` v1.7.7 does not recognize `macos-15-intel` and `windows-11-arm` in its bundled runner-label catalog and reports them as unknown. [GitHub's official runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) lists both as standard hosted labels; the GitHub-hosted jobs above also ran on these labels. An isolated temporary config suppressed only those two runner-label diagnostics, after which actionlint passed the remaining workflow checks. No runner targets or checked-in actionlint configuration were changed.

Subsequent verification on docs-only SHA `ea068dccdbabd2446786de6d6748cca8da8384b2` is not a passing six-target certification: [CI 37372516920 attempt 1](https://github.com/kakarot700/newgit/actions/runs/37372516920/attempts/1) had Windows real-Git transfer failures and no-step shared/CodeQL cancellations; [attempt 2](https://github.com/kakarot700/newgit/actions/runs/37372516920/attempts/2) passed shared checks, dependency/SBOM, Linux x86_64, and both macOS targets, but Windows x86_64/ARM64 failed partial-clone or shallow-fetch transfers with HTTP 408/reset, and ARM64 Linux exceeded a test-only response ceiling by 2.040549 ms. Both focused Windows deadline tests passed; [CodeQL attempt 2](https://github.com/kakarot700/newgit/actions/runs/37372516949/attempts/2) passed. The package job was skipped, not failed or passed. This test-only follow-up widens the ARM64-sensitive ceiling to 1 s; no production timeout, body cap, or server behavior changes. Do not certify a new source SHA until its exact native jobs pass.

## Exact-SHA status — test-only follow-up `3fc2270` (2026-10-06)

[CI 37376156693](https://github.com/kakarot700/newgit/actions/runs/37376156693)
attempts 1–3 do not certify a passing six-target matrix. Attempt 1 failed a
Windows ARM64 protected non-fast-forward transfer with HTTP 408; attempt 2 failed
Windows x86_64 protected-ref deletion with HTTP 408; attempt 3 failed x86_64
shallow-fetch deepening with HTTP 408 and ARM64 lazy fetch with a connection
reset while 49,148 body bytes remained. Both Windows-focused deadline tests
passed on every attempt. The four non-Windows targets passed on attempt 3;
shared checks and dependency/SBOM passed. `dist` was skipped after each native
matrix failure. [CodeQL 37376156803](https://github.com/kakarot700/newgit/actions/runs/37376156803)
passed. The follow-up changes only test portability/timing and documentation;
production behavior is unchanged. Release readiness remains pending.


## Current exact-SHA native evidence — Windows deadline follow-up (`c20ecff`)

On exact SHA `c20ecff6b5a28f98f7cb9a95fc0053563ab5c949`, [CI run 37380464977 attempt 2](https://github.com/kakarot700/newgit/actions/runs/37380464977/attempts/2) passed all six native targets and their full test/build/artifact steps. Both Windows jobs passed the focused request-deadline regression before the full suite: [Windows x86_64 job 112005430166](https://github.com/kakarot700/newgit/actions/runs/37380464977/job/112005430166) and [Windows ARM64 job 112005430010](https://github.com/kakarot700/newgit/actions/runs/37380464977/job/112005430010). Their complete suites, including real-Git force-push coverage, passed. Linux x86_64/ARM64 and macOS x86_64/ARM64 also passed in that run.

The shared checks, dependency/SBOM job, and dependent reproducible distribution-package gate passed on the same SHA; the exact-SHA [CodeQL run 37380465174](https://github.com/kakarot700/newgit/actions/runs/37380465174) passed. Attempt 1's force-push HTTP 408 remains in the record; the logs do not expose per-request idle duration, and the passing same-SHA retry is not used to erase or relabel that failure. The Windows-specific peer-EOF handling is test-only; production code and timeout policy are unchanged.
