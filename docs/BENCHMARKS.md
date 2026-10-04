# NewGit Benchmarks

Real measurements — no estimates, no fabricated numbers. Reproduce with:

```
cargo run --release --bin newgit-bench
```

The harness (`src/bin/newgit-bench.rs`) has **no external benchmark
dependency** (D-002): it times each workload with `Instant` over N
iterations and reports min/median/mean/max. Every run prints the
environment block first; results are only meaningful together with it.

## Environment (as measured)

```
cpu: Intel(R) Xeon(R) Processor @ 2.60GHz   (shared-cloud vCPU, 1 core visible)
mem: 2032608 kB (~2 GB)
os: Linux 6.1.158+
newgit: 0.1.0 (release profile, lto=thin, strip)
workdir: /tmp (sandboxed overlay filesystem)
date: 2026-10-04
```

> These are sandbox numbers on a shared vCPU with an overlay FS — treat
> them as a **baseline and regression reference**, not as marketing peak
> performance. Two consecutive full runs agreed within ~10% on every
> median; the table below is the second run. A third run after the
> journal-checkpointing change (D-015) matched within the same noise
> band (snapshot-1k-cold median 55.5 ms vs 50.5 ms), confirming the
> maintenance paths did not perturb the core workloads.

## Results

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

(none yet — 2026-10-04 table above is the first recorded run)
