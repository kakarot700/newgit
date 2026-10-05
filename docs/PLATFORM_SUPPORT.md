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

The authoritative evidence for a commit is its actual native matrix run in [GitHub Actions](https://github.com/kakarot700/newgit/actions/workflows/ci.yml). A platform is **SUPPORTED for that commit** only when its native job completes successfully. A target that has not passed that job is **NOT TESTED** for that commit; a successful cross-target `cargo check` or Clippy run is useful compile evidence, not native runtime evidence. The separate release-package job waits for every matrix job. Each native CI artifact includes the target binary, SHA-256 sidecar, and metadata naming the source commit, clean-checkout state, target, operating system, architecture, Rust compiler, and Git version.

Current tagged release archives are Linux x86_64 GNU only. The native per-target CI binaries are verification artifacts and are not currently published as release archives.

## First exact-SHA matrix result (2026-10-05)

The first six-target run was for commit `de72d2c72f883626dbe0abc82774d112f421d219`: [CI run 37259546829](https://github.com/kakarot700/newgit/actions/runs/37259546829). These results apply to that SHA only; a test-only follow-up is pending its own native run.

| Target | Result on `de72d2c` |
|---|---|
| Linux x86_64 | Passed; artifact SHA-256 and metadata verified |
| Linux ARM64 | Passed; artifact SHA-256 and metadata verified |
| macOS Intel x86_64 | Failed in the chaos suite: test retried recovery before the configured age-only stale-lock threshold |
| macOS Apple silicon ARM64 | Failed in the chaos suite: same stale-lock-age test assumption |
| Windows x86_64 | Failed in the chaos suite: same stale-lock-age test assumption |
| Windows ARM64 | Failed in `upload_pack_projection_preserves_head_and_objects_without_checkout` at the Git `symbolic-ref HEAD` check; the follow-up captures Git stderr for diagnosis |

The common format/lint/test/build job and dependency/SBOM checks passed. [CodeQL run 37259546815](https://github.com/kakarot700/newgit/actions/runs/37259546815) passed on the same SHA. Release packaging was skipped because four platform jobs failed. The Windows process-tree deadline regression passed in both Windows jobs despite their other test failures. Until a later exact-SHA matrix passes, do not treat these partial results as broad cross-platform support evidence.

## Known platform boundaries

* Git tree modes preserve executable-bit metadata across import/export. Unix checkout applies executable permissions; Windows files do not have the same executable-bit behavior.
* Git symlink objects are preserved. Unix checkout creates native symlinks. Non-Unix checkout rejects a tree containing symlinks before it writes any files. Windows snapshot capture still depends on the host allowing symlink creation; the Git interoperability fixture stages the symlink in the index so the conversion test itself does not require that privilege.
* Windows filesystem paths reject reserved characters and device names, alternate-data-stream spellings, and trailing dots or spaces. Checkout preflights paths and detects case-folded collisions on Windows and macOS. This conservative check does not implement every filesystem's Unicode normalization or case-folding rule.
* Token files get best-effort mode `0600` on Unix. On Windows, their access control comes from the containing directory's inherited ACL; NewGit does not inspect or change that ACL.
* File contents are synced before rename, but directory fsync is best-effort. Stale-lock reclamation uses recorded-PID liveness on Linux; other platforms rely on the configured age fallback. The adversarial smart-HTTP mid-transaction snapshot test is Linux-only, and network filesystems are not established.
* Git interoperability is exercised with the installed Git on each native runner when its job runs; local detailed baseline evidence is Git 2.43.0/Linux. The workflow requires Git 2.34.0 or newer for the SSH-signature fixtures, but that requirement is not a claim that every Git feature is compatible from that version onward.
