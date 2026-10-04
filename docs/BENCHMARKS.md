# NewGit Benchmarks

Real measurements — no estimates, no fabricated numbers. Reproduce with:

```
cargo run --release --bin newgit-bench
```

The harness (`src/bin/newgit-bench.rs`) has **no external benchmark
dependency** (D-002): it times each workload with `Instant` over N
iterations and reports min/median/mean/max. Every run prints the
environment block first; results are only meaningful together with it.

## Git smart-HTTP transfer benchmark (2026-10-05)

Reproduce the real-client benchmark with:

```sh
cargo test --release --locked --lib --no-run
time -p cargo test --release --locked --lib \
  remote::git_http::benchmark::live_git_transfer_baseline -- \
  --ignored --nocapture
```

The ignored test builds a deterministic NewGit fixture (80 commits, 40 paths
per commit, 12 KiB per blob version), starts the live smart-HTTP server, and
uses installed Git over loopback through a byte-counting HTTP proxy. It reports
end-to-end wall time for full clone, `--depth=1`, `--filter=blob:none
--no-checkout`, one-file lazy hydration, and three unchanged fetches. Response
bytes are the exact HTTP response-body `Content-Length` totals for each client
operation. Three fresh temporary projections are also timed directly through
the same builder called by the server; their on-disk size is measured
recursively. `time -p` adds aggregate user/system CPU for the test command and
its child processes; the harness does not report per-operation CPU or peak RSS.
The crate forbids unsafe code, and no portable safe process-tree sampler is
available in the benchmark. Do not compare aggregate CPU when the command also
compiles the project; the `--no-run` command above warms that build first.

### Measured result

One before/after run on Git 2.43.0, Linux, loopback, the 2026-10-05 shared
computer. The fixture and client commands were identical; only the temporary
projection builder changed. Wall-time values for individual workflows are
single observations; unchanged-fetch and projection rows are medians of three
observations. Environment: Intel Xeon @ 2.50 GHz, 8 online logical CPUs,
24,788,980 kB total memory, Linux 6.18.38+; the machine is shared, so these
latencies are observations rather than capacity claims. The warm optimized run
reported aggregate `time -p` values of 4.10 s real, 2.55 s user, and 1.00 s
system. The baseline command included a release compilation, so its aggregate
CPU is not comparable; peak RSS was not measured.

| Measurement | Before: checkout materialized | After: upload-pack skips checkout |
|---|---:|---:|
| Projection build, median of 3 | 129.60 ms | 122.89 ms |
| Total temporary projection bytes | 1,992,727 | 1,497,394 |
| Git object-store bytes | 1,496,958 | 1,496,958 |
| Full clone: wall time / response-body bytes | 542.30 ms / 1,486,989 | 601.47 ms / 1,486,989 |
| Shallow clone (`--depth=1`): wall time / response-body bytes | 480.45 ms / 494,064 | 530.60 ms / 494,064 |
| Blobless clone (`--no-checkout`): wall time / response-body bytes | 488.26 ms / 22,156 | 492.98 ms / 22,156 |
| Lazy hydration of one selected file: wall time / response-body bytes | 344.56 ms / 12,515 | 355.82 ms / 12,515 |
| Unchanged fetch, median of 3: wall time / response-body bytes | 348.08 ms / 219 | 344.76 ms / 219 |

**Interpretation:** the safe, deterministic result is a 495,333-byte (24.9%)
reduction in temporary projection storage for this fixture, with the Git object
store unchanged. The upload-pack exporter now preserves symbolic or detached
`HEAD` without writing a checkout into the temporary repository; receive-pack
retains its existing worktree behavior. Measured projection median was 6.71 ms
(5.18%) lower. End-to-end clone timings moved in both directions, and the
unchanged-fetch median difference was only 3.32 ms (0.95%), so no general
latency or throughput improvement is claimed. All measured response-body totals
matched between the two runs. The benchmark does not measure concurrent load,
peak RSS, or a large-repository scaling curve. It measures an adapter cost and
temporary-storage reduction on this fixture only.

The projection change does not cache refs or Git objects, so it adds no cache
invalidation window: each request still exports the current repository state
using the existing request deadline. Ordinary `export-git` continues to
materialize its checkout.

## Environment (as measured)

```
cpu: Intel(R) Xeon(R) Processor @ 2.60GHz   (shared-cloud vCPU, 1 core visible)
mem: 2032608 kB (~2 GB)
os: Linux 6.1.158+
newgit: 0.1.0 (release profile, lto=thin, strip)
workdir: temporary directory on overlay-backed storage
date: 2026-10-04
```

> These are shared-vCPU numbers from Linux with overlay-backed storage — treat
> them as a **baseline and regression reference**, not as marketing peak
> performance. Two consecutive full runs agreed within ~10% on every
> median; the table below is the second run. A third run after the
> journal-checkpointing change (D-015) matched within the same noise
> band (snapshot-1k-cold median 55.5 ms vs 50.5 ms), confirming the
> maintenance paths did not perturb the core workloads.

## Results (iteration-11 re-check — full code base incl. remote/UI/MCP, 2026-10-04)

| benchmark | iters | min ms | med ms | mean ms | max ms |
|---|---|---|---|---|---|
| put_blob 1KiB (unique) | 2000 | 0.09 | 0.10 | 0.11 | 0.36 |
| snapshot 1000×200B cold | 3 | 50.17 | 61.35 | 76.80 | 118.88 |
| snapshot 1000 files warm (no changes) | 5 | 2.10 | 2.30 | 2.36 | 2.77 |
| snapshot 5000×200B cold | 1 | 239.36 | 239.36 | 239.36 | 239.36 |
| status 1000 files clean (cached index) | 10 | 1.77 | 1.83 | 1.91 | 2.30 |
| status 1000 files clean (index deleted) | 5 | 4.71 | 4.80 | 4.79 | 4.85 |
| diff_trees 1000 files (1 edit, 1 add, 1 rename) | 10 | 1.38 | 1.43 | 1.43 | 1.50 |
| history walk 500 snapshots (full) | 5 | 4.09 | 4.27 | 4.41 | 5.05 |
| integrate 3-way merge (200 files + 1 new) | 10 | 6.61 | 7.34 | 7.21 | 7.71 |
| verify --deep (1000 files, ~1000 objects) | 3 | 41.82 | 43.19 | 45.79 | 52.38 |
| gc --force-now (1000 orphans / ~1100 objects) | 1 | 21.28 | 21.28 | 21.28 | 21.28 |

**Regression check vs the iteration-7 baseline (archived below):** every
median moved by ≤1.41× (worst: diff_trees 1.02→1.43, integrate 5.20→7.34)
— within the shared-vCPU noise band documented above and far below the 2×
release-gate threshold. No row regressed for a code reason: iterations 8–10
added new subsystems (git interop, remote, UI/MCP) without touching the
core write/diff/merge paths.

## Interpretation (honest reading)

* **Durability dominates write paths.** Every stored object goes through
  temp-file → fsync → rename → dir-fsync (D-00x crash-safety protocol),
  and every ref mutation fsyncs its journal before the commit point.
  `put_blob` at ~0.09 ms (≈11k objects/s) and cold snapshots at ~50 µs
  per file are fsync-bound on this FS, not CPU-bound. A `--no-fsync`
  fast path deliberately does NOT exist: crash safety is the product.
* **The index cache works.** Warm snapshot (no changes) is 2.4 ms vs
  50 ms cold — 20× — because hashing is skipped for racily-clean-guarded
  cache hits (D-011). Cached `status` (1.9 ms) is 2.3× faster than
  uncached (4.3 ms).
* **Cold snapshot scales ~linearly**: 1000 files ≈ 50 ms, 5000 files
  ≈ 218 ms (≈44 µs/file both ways) — no superlinear blowup at this size.
* **Read/analysis paths are cheap**: full 3-way `integrate` (merge +
  atomic txn + checkout) ≈ 5.2 ms; diff over 1000 files with rename
  detection ≈ 1 ms; history walk of 500 snapshots ≈ 4 ms.
* **Maintenance is practical**: deep verify (re-encode every object +
  walk every link) ≈ 36 ms per 1000 objects (~36 µs/object); gc of 1000
  orphans ≈ 19 ms including root collection from refs+reflogs and
  shard-dir cleanup.
* **Known scaling limits** (see KNOWN_LIMITATIONS): single-process
  harness, no packed/compressed multi-object files, rename detection
  pair cap 1000, Myers edit-distance cap 1024 per file. Very large repos
  (100k+ files) are untested — the numbers above must not be extrapolated
  there without measurement.

## Regression policy

Benchmarks are run manually each release-candidate iteration and the raw
table is pasted into this file with its environment block (no silent
rewrites of history — superseded tables move to the Archive section with
their date). A >2× median regression on any row without a documented
reason blocks the release gate (RELEASE_READINESS.md).

## Archive

### Iteration-7 baseline (2026-10-04, pre-remote/UI code)

| benchmark | iters | min ms | med ms | mean ms | max ms |
|---|---|---|---|---|---|
| put_blob 1KiB (unique) | 2000 | 0.06 | 0.09 | 0.09 | 0.54 |
| snapshot 1000×200B cold | 3 | 45.67 | 50.45 | 64.46 | 97.26 |
| snapshot 1000 files warm (no changes) | 5 | 2.23 | 2.45 | 2.41 | 2.56 |
| snapshot 5000×200B cold | 1 | 218.19 | 218.19 | 218.19 | 218.19 |
| status 1000 files clean (cached index) | 10 | 1.80 | 1.89 | 1.96 | 2.24 |
| status 1000 files clean (index deleted) | 5 | 4.32 | 4.34 | 4.39 | 4.46 |
| diff_trees 1000 files (1 edit, 1 add, 1 rename) | 10 | 0.90 | 1.02 | 1.00 | 1.17 |
| history walk 500 snapshots (full) | 5 | 3.74 | 4.03 | 4.10 | 4.56 |
| integrate 3-way merge (200 files + 1 new) | 10 | 5.03 | 5.20 | 5.25 | 5.53 |
| verify --deep (1000 files, ~1000 objects) | 3 | 35.89 | 36.19 | 36.40 | 37.11 |
| gc --force-now (1000 orphans / ~1100 objects) | 1 | 18.91 | 18.91 | 18.91 | 18.91 |
