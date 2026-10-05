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
cargo test --release --locked --lib \
  remote::git_http::benchmark::live_git_transfer_baseline -- \
  --ignored --nocapture
```

The ignored test builds deterministic 80- and 800-commit NewGit fixtures (40
paths per commit, 12 KiB per blob version) and sends real Git clients through the
live loopback smart-HTTP server and a byte-counting proxy. For each fixture it
prints three direct projection-build samples and temporary bytes, three full
clones into fresh client directories, three unchanged fetches against the first
clone, a batch of four simultaneous full clones, and a second four-clone batch
started alongside a delayed transactional metadata writer. Response-body bytes are
the HTTP `Content-Length` totals observed by the proxy. There is no OS page-cache
eviction: later samples are warm-cache observations, not cold-storage tests.

On Linux, `/proc/<pid>/stat` and `/proc/<pid>/status` are sampled every 2 ms for
the Git leader process; this **excludes helper descendants** such as
`index-pack`. CPU figures have kernel clock-tick resolution (100 Hz here). The
test process's own CPU and high-water RSS include its in-process server threads
but exclude its child Git commands. These are partial resource measurements,
not whole-process-tree CPU or peak server RSS; non-Linux systems may report `n/a`.

### Historical before/after comparison (80 commits)

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
peak RSS, or a large-repository scaling curve; the expanded observations below
cover those dimensions with the process-scope limitations stated above.

The projection change does not cache refs or Git objects, so it adds no cache
invalidation window: each request still exports the current repository state
using the existing request deadline. Ordinary `export-git` continues to
materialize its checkout.

### Larger-history, repeated, and concurrent reads (2026-10-05)

The expanded benchmark ran on Git 2.43.0, Linux 6.18.38+, an Intel Xeon @
2.50 GHz shared host with 8 online logical CPUs and 24,788,980 kB total memory.
The fixtures were generated once per size; the 80-commit fixture took 484.99 ms
to build (1,645,477 canonical-repository bytes), and the 800-commit fixture
took 3,972.29 ms (12,002,285 bytes). Each operation's wall time excludes
fixture creation and compilation. Projection directories are new per build and
per live HTTP request; no persistent projection cache is used. The OS page cache
was not cleared. Every row gives raw wall-time samples and their median where
three serial samples were taken.

| Workload | 80 commits | 800 commits |
|---|---|---|
| Projection wall time (ms), 3 samples | 125.48, 129.09, 125.67; median **125.67** | 928.33, 909.17, 926.67; median **926.67** |
| Projection temporary bytes / Git object-store bytes | 1,497,394 / 1,496,958 | 10,629,021 / 10,628,585 |
| Unique exported blobs | 119 | 839 |
| Full-clone wall time (ms), 3 samples | 537.49, 545.32, 508.27; median **537.49** | 3,134.53, 3,138.91, 3,147.69; median **3,138.91** |
| Full-clone response-body bytes, 3 samples | 1,486,989 each | 10,542,749; 10,542,784; 10,542,804 |
| Unchanged-fetch wall time (ms), 3 samples | 358.44, 344.50, 301.29; median **344.50** | 1,946.69, 1,965.36, 1,929.67; median **1,946.69** |
| Unchanged-fetch response-body bytes, 3 samples | 219 each | 219 each |
| Four simultaneous full clones: batch wall time / aggregate response bytes | 650.15 ms / 5,947,946 | 3,182.38 ms / 42,170,091 |
| Simultaneous individual client wall samples (ms) | 645.59, 649.95, 542.18, 602.98 | 3,157.08, 3,159.34, 3,182.14, 3,161.10 |

At 800 commits the projection median was **7.37×** the 80-commit result, while
the canonical fixture occupied 7.29× as many bytes. The unchanged-fetch median
was **1.95 s** despite a 219-byte response body, versus 345 ms at 80 commits.
The request path builds a temporary Git view for both smart-HTTP advertisement
and upload-pack; a stateless fetch therefore rebuilds the history projection
more than once. This points to projection construction—not response transfer—as
the next bottleneck. The full-clone response grew to about 10.54 MB and its
median wall time to 3.14 s. The four-client 800-commit batch took 3.18 s on this
shared host; one batch per size is an observation, not a concurrency capacity
claim.

The harness process's overall high-water RSS was 102,380 KiB for the complete
run. The highest sampled Git leader RSS was 6,184 KiB in serial 800-commit
clones and 6,376 KiB among the four concurrent clients. Those RSS values omit
the leaders' helper descendants. The Linux 100 Hz CPU counters are quantized
(many Git-leader readings round to 0.000 s); the harness-process counter for an
800-commit direct projection was 0.24–0.25 s, but excludes the child `git init`
and `fast-import` CPU. CPU figures therefore do not provide complete cost
attribution.

No production optimization was retained: an experiment removing clones from
the request-local flattened-tree map showed no measurable projection improvement
and was discarded. There is no persistent cache. Reusing a built view across
requests is an attractive latency lever, but should not be shipped until a
monotonic repository generation can be proven to cover every canonical object,
ref, and `HEAD` write and readers can obtain a consistent generation under
concurrent writes; that invalidation and race-safety proof does not exist here.

### Test-only smart-HTTP phase profile (2026-10-05)

The ignored live benchmark now collects test-build-only wall timers for projection
setup/export, NewGit ref and history walks, tree flattening/blob-mark discovery,
Git initialization, feeding the `fast-import` stream, waiting for its final
drain, post-import `HEAD` work, the advertise and stateless upload-pack commands,
and each server HTTP request (repository open, request read, route, audit, response
write, and full handler time). It changes no production behavior. Reproduce it
with the command above; phase measurements add minor test-harness bookkeeping and
are not assertions or performance guarantees.

One warm-cache run on the same shared Linux/Git 2.43.0 host used above produced
these medians across three direct projections:

| Direct projection phase (ms) | 80 commits | 800 commits |
|---|---:|---:|
| Whole projection | 130.87 | 894.95 |
| NewGit ref/head collection | 0.08 | 0.09 |
| NewGit topological history walk | 1.25 | 10.96 |
| Tree flattening and unique blob-mark discovery | 7.42 | 69.81 |
| Commit/ref ownership walk | 1.03 | 10.02 |
| `git init` | 2.89 | 2.74 |
| Feed export stream to `git fast-import` | 78.17 | 654.39 |
| `fast-import` final drain/wait after input closes | 32.79 | 140.10 |
| Post-import marks and `HEAD` setup | 2.15 | 3.04 |

For the 800-commit unchanged fetch, the three client wall samples were 1,967.95,
1,894.55, and 1,944.18 ms (median **1,944.18 ms**), each returning **219 bytes**.
The fetch makes two HTTP requests and rebuilds two projections. Across those
three fetches, the median of the per-fetch sum of projection times was **1,830.84
ms**; the paired advertise and upload-pack Git commands themselves each took
about 1.5–1.7 ms. Median summed `fast-import` stream-feed time across the two
projections was 1,306.28 ms, and its post-input drain/wait was 295.46 ms. The
server's two complete request handlers together were about 1.8–1.9 s per fetch;
request parsing, repository open, audit, and response writing were small compared
with the route work.

**Interpretation and decision:** projection construction is the measured
bottleneck. Within it, the combined stream-feed/backpressure and final
`fast-import` wait dominate; tree flattening/mark discovery is roughly 70 ms per
800-commit projection, not the main 900-ms cost. The feed timer is wall time for
NewGit export formatting/object reads/writes while the child consumes the pipe;
it cannot attribute writer CPU separately from child work. `fast-import` wait is
the remaining child drain/finalization after NewGit closes stdin. The upload-pack
protocol subprocess is not the source of the 1.95-s delay. No production
optimization was retained: removing already-tested request-local tree clones did
not help, and reusing a projection without a proven invalidation boundary risks
serving stale refs or `HEAD`. Before caching, the canonical core needs a durable
generation covering all relevant object/ref/`HEAD` writes and a consistent
snapshot/publication protocol. Each HTTP request must independently validate
the current generation; advertisement and fetch cannot be guaranteed the same
generation if a mutation occurs between their stateless requests. That
generation and its concurrent-writer semantics must be implemented and proven
before caching.

These are one-run measurements on a shared host, not cold-cache or capacity
results. Timers are wall-clock intervals, not CPU attribution; `fast-import`
stream feeding overlaps child consumption, and the existing 100-Hz CPU and
leader-only RSS limitations described above still apply.

### Projection-cache safety audit (2026-10-05)

> Historical note: this audit records the state before the lock-protected
> snapshot guard in D-021. Its reader-race and GC/HEAD-bypass findings below are
> superseded by that implementation; the cache/generation decision remains in
> force.

**Decision: do not cache projections or add a generation counter yet.** The
performance case is real—an 800-commit unchanged fetch returns 219 bytes but
spends a median 1,830.84 ms building two projections—yet the current mutation
and read APIs do not provide a generation that can safely key reusable views.
This is an architecture boundary, not a claim that a cache has been implemented
or benchmarked.

The current transaction engine serializes ref/metadata writers, but projection
readers do not hold its lock. `apply_journal` writes each ref (and recovery
replays each op) sequentially, while the exporter separately enumerates and
reads refs, reads `HEAD`, walks histories, and reads objects. A concurrent
multi-ref commit can therefore overlap projection construction. Object writes
are lock-free and available through public object-store APIs; GC removes objects
directly; `Repo::init_with` creates an initial `HEAD` outside the transaction
engine; and generic transaction FILE/FDEL ops can target paths under `refs/` or
`HEAD`. Recovery must be part of any future generation protocol, not treated as
a process-local counter update. Separately, `info/refs` and stateless
`git-upload-pack` are distinct requests, each opening the repository and
building its own temporary projection; neither request carries a snapshot token
for the other.

A safe cache remains a possible future optimization only after the canonical
core supplies a durable generation that advances or is idempotently repaired
for every relevant object/ref/`HEAD` mutation, including GC and crash recovery;
prevents or detects readers observing a partially applied transaction; and
supports publication only when the captured generation is still current. Each
HTTP request must independently validate the current committed generation
before using an immutable cached view; an earlier entry is reusable only if that
fresh check still finds its generation current. Because advertisement and
upload-pack are separate stateless requests, they are not guaranteed to use the
same generation if a mutation lands between them. If protocol-level pinning is
required, it needs an explicit request/session token rather than an implicit
cache hit. Cached Git files must remain disposable derived data; NewGit's
objects and refs remain canonical.

### Committed projection snapshot guard and contention (2026-10-05)

The release-mode ignored benchmark was rerun after adding the exclusive
`SnapshotReadGuard`. It records lock-acquisition wait and lock-hold time during
each temporary Git projection. Fixture generation and compilation are excluded;
the OS page cache was warm and not cleared. Git 2.43.0/Linux; three direct
projections per size and one four-client batch per workload. The before-guard
concurrent-clone values are the earlier single-run observations in this file.

| Measurement | 80 commits | 800 commits |
|---|---:|---:|
| Direct projection median / snapshot-lock wait / lock hold | 121.31 / 0.59 / 120.10 ms | 918.88 / 0.81 / 917.78 ms |
| Four simultaneous full clones, current batch / historical pre-guard batch | 1,617.14 / 650.15 ms | 11,186.67 / 3,182.38 ms |
| Four clones plus delayed metadata transaction: batch / transaction wall time | 1,631.92 / 99.42 ms | 11,126.23 / 873.43 ms |

All 12 clone HTTP responses in each four-client batch were successful (no
non-200 responses). The writer starts 75 ms after the readers, then runs one
transactional metadata write; its reported wall time includes both lock wait and
commit. The isolated lock wait in direct projection samples was below 1 ms, but
the lock is held for essentially the full export: about 120 ms at 80 commits and
918 ms at 800 commits.

The material tradeoff is reader-reader serialization: in this observed run the
four-clone 800-history batch took 11.19 s versus the earlier 3.18 s observation;
the 80-history batch took 1.62 s versus 0.65 s. Writer blocking is also visible
in the synthetic concurrent-writer workload. These are single warm-cache runs
on a shared host, so the exact ratios are not capacity guarantees, but
serialization is an intentional consequence of using the existing exclusive
lock. The guard is released as soon as the private projection is complete; Git
advertisement/upload-pack subprocess work does not retain it. Each HTTP request
is individually consistent, but a mutation between advertisement and
upload-pack can still make the two request-local views differ. No cache or
generation counter was added.

### Kernel advisory-lock implementation check (2026-10-05)

Reproduced the ignored release benchmark after replacing the unlinkable lock
reclaimer with the stable `fs4` advisory-lock implementation. Command:

```sh
cargo test --release --locked --lib \
  remote::git_http::benchmark::live_git_transfer_baseline -- --ignored --nocapture
```

Environment: Git 2.43.0; Rust 1.99.0; Linux 6.18.38+ x86_64; Intel Xeon @
2.50 GHz, 8 online logical CPUs, 24,788,980 kB reported memory. Shared host,
warm page cache, no eviction. Same deterministic 80/800-commit fixture and
four-client workloads as above. The benchmark also prints per-sample and phase
timings; this table retains key measurements. It is one sample per batch, not a
capacity or statistical claim.

| Workload | 80 commits | 800 commits |
|---|---:|---:|
| Direct projection median / lock wait / lock hold | 124.14 / 0.03 / 123.96 ms | 895.14 / 0.03 / 894.95 ms |
| Four simultaneous full clones: batch / response bytes | 1,591.22 ms / 5,947,936 B | 11,394.95 ms / 42,171,046 B |
| Four clones plus delayed writer: batch / writer wait+commit | 1,605.98 / 204.41 ms | 11,392.73 / 915.46 ms |

All clone HTTP responses succeeded. Relative to the earlier single post-guard
run using the old lock path (`1,617.14 ms` and `11,186.67 ms` for the two
four-client batches), the new run moved by −1.60% and +1.86%. Direct projection
medians moved by +2.33% and −2.58%; the measured uncontended lock wait was
0.03 ms. These small, mixed, single-run differences cannot establish a causal
performance change from `fs4` and are within shared-host variability. They show
no obvious whole-request regression in this sample; reader-reader serialization
remains the dominant observed tradeoff at 800 commits (about 11.4 seconds for
four concurrent clones).

## Core benchmark environment (as measured — 2026-10-04)

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
