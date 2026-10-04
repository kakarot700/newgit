# CHANGELOG

Format: Keep a Changelog. Versions follow semver once ≥1.0; 0.x = honest WIP.

## [Unreleased]

### Fixed
- `import-git` now inspects ref object types before invoking `fast-export` and
  refuses ordinary refs that target non-commit objects, naming the ref/type and
  leaving refs untouched. Git 2.43.0 silently omits lightweight blob/tree refs,
  while an annotated blob tag could previously import into a state that
  `export-git` rejected. Real-Git regressions cover lightweight/annotated tags
  to reachable and orphan blobs, tree tags, and a tag-only source.
- `import-git` now accepts 64-hex original object IDs from real Git SHA-256
  repositories, stores source commit IDs under format-neutral `git_oid` metadata
  (while retaining `git_sha1` for SHA-1 sources), and exports semantic history
  into a fresh repository using Git's default object format. A real-Git fixture
  checks commit identity metadata, refs, paths, modes, and blob bytes.
- `import-git` no longer persists `git fast-export --all`'s detached-HEAD
  pseudo-ref as an ordinary NewGit ref. `export-git` now preserves detached
  HEAD, including histories not reachable from any named ref, and deletes its
  temporary Git ref after checkout. Real-Git integration tests cover detached-
  only history, a detached tip ahead of a branch, and nested refs that collide
  with successive temporary-ref candidates. Export also refuses distinct
  NewGit refs that map to one Git ref rather than silently dropping one; a
  redundant-ancestor merge-parent round-trip pins parent ordering.
- `export-git` now preserves non-ASCII UTF-8 bytes when a pathname also needs
  Git C-quoting for quotes or backslashes. A real-Git import/export regression
  covers both sides of a rename and compares path names, modes, and blob IDs at
  every commit; it caught the previous `é` → `Ã©` pathname corruption.

### Added
- A real-Git regression compares raw commit-object message payloads before and
  after import/export and the imported `Snapshot.message`. It pins exact UTF-8
  bytes for leading/trailing blank lines, trailing spaces, CRLF, no final LF,
  and an empty message instead of relying on trimmed pretty-format output.
- Git messages containing unsupported control characters now fail with a
  commit-specific error naming the code point and explaining that refs have
  not been updated. A real Git commit containing U+0001 verifies the refusal is
  atomic and the NewGit repository remains verifiable.
- A real-Git round-trip now covers empty-tree history: an empty root commit, a
  populated commit, deletion back to the canonical empty tree, and a consecutive
  empty commit. Regression assertions compare exported tree IDs and mapped
  parents, then verify every path, mode, and blob after reimport.

## [0.1.0] - 2026-10-04

### Changed / Fixed (iteration 12 — final audit, 2026-10-04)
- **Audit finding FIXED (security)**: `POST /v1/diff` accepted specs that
  resolved internal namespaces (`workspaces/*`, `chains/*`) and the local
  `ws:<name>` shorthand through `diff::resolve_tree` — a read-role (or
  anonymous-read) client could probe workspace names/positions, violating
  the it9 "internal namespaces never cross the wire" invariant. New
  `check_wire_spec` guard refuses them with 400 `invalid`; regression tests
  added inside `internal_namespaces_never_cross_the_wire` (8 probe calls
  across both diff sides; honest ref+oid specs verified still working).
- **Audit finding FIXED (DoS)**: `/v1/goals|changes|proposals` responses
  were unbounded — large repos could produce arbitrarily large listings.
  Now capped at `limits.max_batch_objects` with an additive `truncated`
  flag on `ListData` (protocol doc updated).
- **Audit finding FIXED (robustness)**: MCP `build_argv` placed bare
  positional values (oids, specs, titles, ids) where a dash-leading value
  could be reinterpreted as a CLI flag. All positionals now travel after a
  `--` separator (flag values were already safe); dash-value cases added to
  the argv mapping test; live MCP smoke proves `-dashy title` works.
- README quickstart transcript executed VERBATIM against the built binary
  end-to-end (goal → workspace → snapshot --goal → change → evidence record
  → tested honesty gate → proposal → approve → atomic integrate → achieved
  → `verify` 0 errors → `history --goal`). UI hash-route inputs traced
  (tampered `?ref=` inert; goal-filter oids hex-gated server-side).
- Final gate re-runs on final code: 278/278 debug + 278/278 release,
  fmt/clippy clean, SBOM drift clean, cargo-audit 0 findings, cargo-deny
  all-ok, dual-target rebuild bit-identical (sha256 `30714184…`), dist
  tarball `bbafe611…` 28/28 checksums OK.
- **Hosted validation (2026-10-04, pre-tag commit `afa94c4`):** [CI run
  37182199247](https://github.com/kakarot700/newgit/actions/runs/37182199247)
  and [CodeQL run
  37182199239](https://github.com/kakarot700/newgit/actions/runs/37182199239)
  both passed on Ubuntu 24.04. CI passed format/lint, debug and release
  tests/build, dependency and SBOM checks, the second clean release build,
  distribution packaging, and checksum verification. The PR-only dependency
  review and tag-only release jobs did not run on this push event.
- **Readiness decision: PRODUCTION-CANDIDATE.** Hosted checks remove the
  former unexecuted-CI gate for this tested commit, but do not certify every
  production deployment; same-host reproducibility and platform/protocol
  limits remain. See RELEASE_READINESS.md and docs/COMPLETION_REPORT.md.

### Added (iteration 11 — 2026-10-04)
- **Release engineering**: `scripts/dist.sh` (dist dir + tarball +
  SHA256SUMS over every packaged file; verified with `sha256sum -c`),
  `scripts/sbom.py` → committed **SBOM.md** (deterministic, dateless;
  runtime/build-time/dev closures classified by where each crate actually
  runs: 21 / 7 / 34 crates; CI fails on drift), LICENSE-MIT +
  LICENSE-APACHE (Apache text fetched from apache.org, checksum matches the
  canonical `cfc7749b…`).
- **Reproducibility (real, same-host)**: two clean release builds in
  different target dirs → BIT-IDENTICAL binary (sha256 `abcd51c8…` twice);
  toolchain pinned (rust-toolchain.toml 1.99.0 = rustc in use); honest
  scope in KNOWN_LIMITATIONS #37.
- **Supply-chain audit (real, in-sandbox)**: `cargo audit` against the live
  RustSec DB — 1290 advisories, 63 locked crates, ZERO findings; every
  build.rs in the runtime closure (8 crates) read and classified (rustc
  probes only, no network) — recorded in THREAT_MODEL §G. `cargo deny
  check advisories bans licenses sources` — ALL OK, zero warnings
  (deny.toml committed: allow-list = exact SBOM license set, crates.io-only,
  ban list for git2/openssl/tokio/hyper/axum… per D-002/D-007).
- **CI hardened** (`.github/workflows/ci.yml` — defined; cannot execute on
  a hosted runner from this sandbox, recorded honestly): removed the FAKE
  `NEWGIT_CHAOS_ITERATIONS` knob (chaos is fixed-seed by design), SBOM
  drift gate, prebuilt cargo-audit (`--deny warnings`), cargo-deny action,
  dist job with in-CI reproducibility check + checksum verification +
  tag-release attachment via GITHUB_TOKEN (zero-rupee).
- **Benchmarks re-run** on the full it10 code base (release): all medians
  within 1.41× of the it7 baseline (shared-vCPU noise band; below the 2×
  gate) — new table recorded, old table ARCHIVED (never overwritten).
- **Docs completed**: docs/DEPLOYMENT.md (systemd unit with hardening,
  nginx/caddy TLS termination, honest client-side-TLS story — v1 CLI is
  http://-only, use an ssh tunnel, KL #27 expanded), docs/TESTING.md
  (layers, fault injection, the no-fake rules), docs/TROUBLESHOOTING.md
  (exit codes + symptom→fix, all claims source-verified),
  docs/CONTRIBUTING.md (change contract + code map); README index complete.

### Added (iteration 10 — 2026-10-04)
- **Web UI — embedded, single-file, read-only explorer (`newgit ui`,
  `serve --ui`)**: dashboard (info/limits/capabilities/refs), first-parent
  history with goal/change/workspace badges, universal object inspector
  (snapshots, lazy tree browser, blob text/hex preview ≤64 KiB, actors,
  goals, changes, evidence with DETERMINISTIC/NON-DETERMINISTIC badges,
  evaluations with a permanent **AI OPINION** badge, proposals), goals &
  proposals boards, side-by-side compare with colored unified diffs, audit
  log viewer. Zero external resources (no CDN/fonts/build step); token
  login kept in `sessionStorage`; ALL dynamic text via `textContent`
  (`innerHTML`/`eval`/`document.write` are CI-asserted absent — XSS-safe by
  construction); the served shell contains ZERO repository data.
- **Agent read endpoints on protocol v1** (docs/PROTOCOL.md):
  `POST /v1/object` (kind + links + data; blobs as `data_b64`+`size`),
  `POST /v1/diff` (specs a/b incl. refs and `ws:`; CLI-identical unified
  rendering, capped at 100 files), `GET /v1/goals`, `GET /v1/changes?goal=`,
  `GET /v1/proposals`; `/v1/info` capabilities extended (+`ui` when served).
- **`newgit mcp` — Model Context Protocol server over stdio** (JSON-RPC
  2.0, protocol 2024-11-05, zero new dependencies): 13 tools mirroring the
  CLI 1:1 through the SAME dispatch path (`cli::call_json`); tool failures
  return `isError:true` with the standard `{category,message}` envelope;
  malformed input never kills the server. `newgit ui` prints the UI URL;
  `serve --json` now reports `ui`/`ui_url`.
- **docs/AGENT_GUIDE.md** — the three interfaces (CLI `--json` / HTTP v1 /
  MCP), curl recipes, error-category contract, honesty rules, two-agent
  example. SECURITY_MODEL §8, THREAT_MODEL §E2 (UI/MCP surface), D-018,
  KNOWN_LIMITATIONS #33–36.
- Tests: 269 → **278** (UI self-containment + XSS discipline, UI serving &
  data-free shell vs real server, object/diff/listing endpoint shapes and
  gates, MCP handshake/catalog/error contract/argv mapping, `newgit ui` and
  `newgit mcp` as real child processes).

### Added (iteration 9 — 2026-10-04)
- **Remote protocol v1 + server + client (docs/PROTOCOL.md, D-017)** — new
  `src/remote/` layer, std-only, zero new dependencies:
  - `newgit serve [--bind host:port]` — hand-rolled total HTTP/1.1 server
    (thread-per-connection, bounded pool ⇒ 429, 30 s IO timeouts,
    Content-Length checked before reading, port-0 announcement line for
    scripts/tests). Endpoints: `/healthz`, `/v1/info`, `/v1/refs`,
    `/v1/have`, `/v1/negotiate`, `/v1/objects/get`, `/v1/objects/put`,
    `/v1/refs/update`, `/v1/audit` — CLI-identical `{ok,data|error}`
    envelopes, stable category→HTTP-status mapping, `X-NewGit-Protocol`
    version negotiation with actionable mismatch errors.
  - Auth: bearer tokens (32-byte OS-CSPRNG, SHA-256 at rest, 0600 file),
    roles read<write<admin, invalid token never downgrades to anonymous;
    `newgit token add|list|remove` (raw token printed exactly once, never
    listed). Append-only audit log (`.newgit/audit.log`) records every
    request including failures; readable via `/v1/audit` (admin) or
    `newgit audit`.
  - `newgit remote add|list|remove` (`.newgit/remotes.json`, tokens masked
    in listings), `newgit push` (negotiated incremental upload via batched
    have-probes; dependency-ordered batches; **non-fast-forward refusal**
    ⇒ exit 5 unless `--force`; refs move server-side in ONE CAS
    transaction), `newgit pull` (server-side closure-delta negotiation,
    per-object local re-validation of digest/id/dependencies, local refs
    move in one CAS transaction; HEAD/workspaces untouched — fetch
    semantics by design).
  - Server-side **link-closure invariant**: remote object writes are
    refused unless every dependency is already stored, which makes
    "have X ⇒ hold closure(X)" true and lets negotiation prune safely.
- Tests: `tests/remote_e2e.rs` (14 suites, real TCP, no mocks) · 2 CLI
  remote suites (serve child + full command contracts) · HTTP/wire-struct
  fuzz sweep (`fuzz_http_and_wire_json_never_panic`, 20k inputs) · 15 new
  lib unit tests (http framing, auth, audit, negotiate post-order).
- Docs: docs/PROTOCOL.md (normative v1 spec), CLI.md remote sections +
  remote exit-code table, THREAT_MODEL §D/§E rows now ✅ with test names,
  SECURITY_MODEL realized, TEST_MATRIX I20–I24, KNOWN_LIMITATIONS #27–32.

### Added (iteration 8 — 2026-10-04)
- **Git compatibility (D-007/D-016, docs/GIT_COMPAT.md)** — new `src/gitio/`
  layer, zero new dependencies, uses the *system git's own stream formats*:
  - `newgit import-git <git-repo>` — streams `git fast-export --all
    --full-tree --show-original-ids --signed-tags=strip` through a
    hand-rolled total parser (7 unit tests on real-git framing quirks);
    commits → Snapshots (author → Actor `git:<email>`; committer, original
    sha, tz, ordered merge parents preserved in extras), trees/blobs
    byte-exact, modes 100644/100755/120000 mapped; **all refs + HEAD move
    in one transaction**; deterministic oids; skipped namespaces and
    stripped annotated-tag metadata are reported, never silent.
  - `newgit export-git <target-dir>` — topological `git fast-import` stream
    with deterministic marks, piped into `git init` + `fast-import --done`;
    committer/first-parent metadata restored from extras; HEAD mirrored;
    working tree materialized and clean.
  - Round-trip verified against real git: byte-identical blob SHAs, modes,
    messages, author+committer identities/timestamps, first-parent lineage;
    reimport fixpoint on trees/messages.
  - Hard, loud limits: submodules abort import atomically (zero refs move);
    annotated/signed tag metadata stripped (reported); ms→s export precision
    loss documented.
- Tests: `tests/git_compat.rs` (9 suites vs REAL git 2.47 repos incl.
  merges, tags, binary, symlinks, exec bits, unicode, renames, empty
  commits, notes/remotes skipping, refusal contracts) · cli_e2e
  `import_export_git_cli` · fastexport fuzz sweep in `tests/fuzz_parsers.rs`.
- Docs: docs/GIT_COMPAT.md (mapping tables + guarantees + limits), CLI.md
  sections, THREAT_MODEL untrusted-import rows, KNOWN_LIMITATIONS #20–26.

### Added (iteration 7 — 2026-10-04)
- `newgit verify [--deep] [--json]` — read-only fsck with stable issue codes
  and error/warning severities: object layout/name/envelope/digest/misfiled/
  non-canonical, deep link walks (existence + type), refs + HEAD + reflog
  line format, chain head/prev-walk/cycle/type/root checks, workspace
  meta/files/position-ref/index-cache consistency, txn-dir leftovers, config
  parse. Errors ⇒ exit 3; crash debris classes are warnings (D-014).
- `newgit gc [--dry-run] [--force-now] [--json]` — strictly non-destructive
  mark-and-sweep: roots = HEAD + all refs + reflogs + workspace bases; mark
  follows extras.prev chain links; global txn lock held for the run; 24 h
  mtime grace window; corrupt/misfiled/quarantine/debris never deleted
  (kept_corrupt / quarantined counters); empty shards pruned + fsynced.
- `newgit recover [--json]` — explicit crash-recovery pass (also automatic
  on every open); reports redone/cleaned/quarantined/swept.
- Journal checkpointing (D-015): successful txns delete their journal;
  recovery deletes terminal-state journals — `txn/` stays bounded.
- Chaos suite (`tests/chaos.rs`): 6 fixed xorshift64* seeds × 14–25
  randomized ops (snapshot/workspace-create/integrate/put-blob/txn-set) each
  killed at random fault points in child processes; after EVERY step: open
  auto-recovers, deep verify has zero errors, all refs resolve, status
  computes; end-of-seed gc + full history walk. Found and fixed two real
  bugs (temp-debris misclassification, journal accumulation).
- Fuzz-like parser suite (`tests/fuzz_parsers.rs`): 110k seeded prefix-
  anchored garbage inputs across envelope/canonical/index/journal/config/
  hex/base64/ref-grammar parsers — no panics, no unbounded allocation (I4).
- Benchmarks (`src/bin/newgit-bench.rs`, zero extra deps) + real recorded
  numbers in docs/BENCHMARKS.md: put_blob 0.09 ms, snapshot 1k cold ~50 ms /
  warm 2.4 ms, 5k cold ~214 ms, status 1.9/4.5 ms cached/uncached, diff 1k
  ~1 ms, integrate 3-way ~5.3 ms, history 500 ~4 ms, verify --deep ~37 ms,
  gc 1000 orphans ~19 ms (sandbox Xeon 2.6 GHz, overlay FS).
- 33 new tests (20 verify_gc + 6 chaos + 6 fuzz + 1 cli e2e) — total 219.

### Changed (iteration 7)
- `verify` classifies object-store temp files (`*.tmp.*`) as warning-level
  crash debris (swept by recovery once stale), not layout errors.
- `workspace.files_missing` downgraded error → warning with a
  `newgit checkout -w <name>` repair hint (repairable debris, D-012/D-014).
- faultlab: unknown command now exits non-zero (was silently 0).

### Added (iteration 6 — 2026-10-04)
- Workflow entities on CAS-guarded version chains (`chains/<root>` refs,
  `extras.prev` audit links): goals, changes, proposals.
- Honesty gates: `tested` requires evidence; `proposal integrate` requires
  approval; state machines validated at every transition.
- Evidence: runner-recorded (`evidence record` executes the command,
  captures exit/output/duration; deterministic=true; verdict from exit
  status; output capped + truncation-flagged) and manual/opinion
  (`evidence add`, deterministic defaults false).
- Evaluations: deterministic aggregation from attached evidence
  (`evaluation from-evidence`, never AI-flagged) and explicit AI opinions
  (`evaluation create --ai`, dimensions supported).
- `proposal integrate`: position ref + proposal chain + change chain in ONE
  transaction; conflict ⇒ exit 5 with zero writes; fast-forward detection.
- CLI: goal/change/evidence/evaluation/proposal command families;
  `history --goal`; `workspace create --author`; docs/AGENT_WORKFLOW.md
  (real two-agent transcript).
- Crash tests: proposal-integrate killed before/after commit point; chain
  update concurrency (linearity invariant); evidence limits/signals.
- 20 new tests (total 186).

### Added (iteration 5 — 2026-10-04)
- Three-way merge engine: per-path resolution on flattened trees; exact
  rename tracking (rename+edit merges cleanly; rename/rename and
  rename/delete conflict); mode combining; modify/delete conflicts;
  dir/file collision detection; add/add with empty virtual base.
- diff3 content merge (Myers anchors); conflict blobs with
  ours/base/theirs markers stored for inspection (`merged_oid`).
- Ancestry: bounded, deterministic `is_ancestor` / `merge_base`.
- `newgit integrate` (atomic; up-to-date / fast-forward / merge; exit 5 on
  conflicts with nothing written), `newgit merge-tree` (dry run; exit 5 on
  conflicts in both text and JSON modes), `newgit rollback` (new snapshot,
  old tree; merge-role aware default target), `newgit checkout` (resync
  files; removes previously-materialized files absent from the target).
- Workspace checkout after integrate/rollback removes stale tracked files
  and prunes empty directories (never touches untracked content).
- 34 new tests: merge cases, integrate atomicity/races/crash (faultlab
  `integrate`), rollback, CLI flows, diff3 properties (total 176).

### Added (iteration 4 — 2026-10-04)
- Diff engine: Myers O(ND) line diff with bounded edit distance and
  exact coarse fallback; canonical opcodes; reconstruction property.
- Tree diff: added/deleted/modified/mode-change; two-stage deterministic
  rename detection (exact + similarity); binary detection; symlink diffs.
- Renderers: git-shaped unified diff (hunks, context merging, no-newline
  markers, rename/mode headers) + structured JSON hunks.
- CLI `newgit diff [<a> [<b>]]` with `-w`, `--name-only`, `--json`,
  `--context`, `--no-renames`, `--exit-code`.
- Read-only worktree capture (`capture_tree(save_index=false)`).
- 22 new tests (total 142 incl. property diff-reconstruct).

### Fixed (iteration 4)
- **Racily-clean index race** (D-011): entries whose mtime ≥ the index
  file's mtime are re-hashed; a same-tick file replacement (e.g. symlink
  swap) could previously reuse stale content ids. Caught by the symlink
  diff test; guarded by a unit test.

### Added (iteration 3 — 2026-10-04)
- `.newgitignore` engine (gitignore subset, deterministic, documented).
- Safe workspace walk with limits, symlink policy (record, never follow),
  filename validation, and skip-with-warning behavior.
- NGIX workspace index (status cache; corruption-safe by design).
- Workspaces: main + named, journaled create/discard, position refs,
  per-workspace locks, dirty-discard protection.
- Operations: snapshot (CAS-journaled), status, history (deterministic
  order), tree build/flatten (cycle-safe), checkout (symlink-traversal safe).
- CLI binary `newgit`: 11 commands, `--json` envelope, stable exit codes,
  `--debug` structured diagnostics; docs/CLI.md.
- Crash tests for snapshot and workspace creation; 45 new tests (total 120).

### Fixed (iteration 3)
- NGIX index double-length-prefix decode bug (caught by ops test — index
  reuse silently disabled; now regression-tested both levels).
- base64/cli-arg test-harness bugs (token splitting, fs limits vs OS limits).

### Added (iteration 2 — 2026-10-04)
- Repository facade: idempotent `init`, crash-recovering `open`, walk-up
  `discover`, HEAD (symbolic/detached) via journaled updates, actor registry
  with default-actor resolution, format-version gate.
- Refs: strict name grammar + reserved namespaces; CAS create/update/delete;
  prefix listing; reflog with txn-id deduplication.
- WAL transaction engine: global lock, recovery-first, pre-write CAS
  validation, RUNNING→apply→COMPLETE protocol, idempotent redo recovery,
  corrupt-journal quarantine.
- Lock reclamation by holder-pid liveness (crash-safe, D-008).
- `newgit-faultlab` crash-test harness binary; fault points across txn/ostore.
- base64 codec (RFC 4648) for journal/reflog text formats.
- 37 new tests: crash-recovery scenarios (child-process aborts), concurrency
  races, property-based codec invariants. Total: 74.

### Fixed (iteration 2)
- Journal REF op parser arity bug (5 vs 6 fields) caught by roundtrip test.
- Fault-point `name#N` matching (exact-name entries now fire correctly).
- base64 decoder now rejects mid-string padding (RFC-conformant).
- LockBusy deadlock after aborted child processes (D-008).

### Added (iteration 1 — 2026-10-03)
- Project bootstrap: Rust crate (lib+bin pending), pinned toolchain, CI workflow.
- Object model v1 (frozen protocol, docs/STORAGE_FORMAT.md):
  Blob, Tree, Snapshot, Actor, Goal, Change, Evidence, Evaluation, Proposal
  with canonical binary codecs and strict total decoders.
- SHA-256 object identity; deterministic-identity invariant tests.
- NGOB on-disk envelope: versioned, self-verifying, bomb-protected.
- Content-addressed object store: atomic crash-safe writes, verified reads,
  corruption/misfiling/truncation detection, prefix resolution, temp sweep.
- Filesystem primitives: atomic_write, O_EXCL locks with stale reclaim,
  bounded reads, path-safety grammar (traversal/NUL/drive/symlink escape).
- Fault-injection framework (`NEWGIT_FAULTS`) for crash-safety tests.
- Repository config & resource limits (`key = value`, fail-loud parsing).
- Error model with categories and stable CLI exit codes.
- Documentation baseline: README, ARCHITECTURE, STORAGE_FORMAT,
  SECURITY_MODEL, THREAT_MODEL, DECISIONS, ROADMAP, TEST_MATRIX,
  KNOWN_LIMITATIONS, RELEASE_READINESS, loop-state files.
- 37 unit tests; fmt/clippy(-D warnings) clean.
