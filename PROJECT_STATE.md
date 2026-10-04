# PROJECT_STATE.md — NewGit autonomous build loop

> Machine-and-human readable continuation state. Updated at the end of every
> iteration. If context is lost, resume from this file.

## Current status

- **Phase:** Iteration 8 COMPLETE — Git compatibility: `src/gitio/` (total fast-export parser + deterministic fast-import emitter), `newgit import-git`/`export-git`, 9 real-git compat suites, docs/GIT_COMPAT.md, D-016 (on top of iteration 7: verify/gc/recover/chaos/fuzz/benchmarks).
- **Classification:** NOT PRODUCTION READY (no remote/auth, web UI, or release engineering yet; see RELEASE_READINESS.md).
- **Last full verification:** `cargo fmt --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` **237/237** ✓ (108 unit + 6 chaos + 12 cli-e2e + 4 concurrency + 8 diff + 7 fuzz + 9 git-compat + 18 merge + 13 ops + 12 property + 10 txn-recovery + 20 verify-gc + 1 version + 9 workflow). Benchmarks in docs/BENCHMARKS.md (real runs, release).

## Environment / how to resume

```bash
# Toolchain lives outside the repo snapshot:
export RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
cd /home/user/newgit
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

Rust 1.99.0 stable (pinned in rust-toolchain.toml). System `git` available
(used later by git interop tests). 2 CPUs, 2 GB RAM — keep test parallelism
modest; avoid heavyweight dev-dependencies.

## Repository layout (as of now)

```
Cargo.toml            # deps: sha2, flate2(rust backend), serde, serde_json, thiserror; dev: proptest, tempfile
rust-toolchain.toml
src/
  lib.rs              # crate root, VERSION, #![forbid(unsafe_code)]
  error.rs            # Error enum, categories, stable exit codes
  util/{hex,varint,fsx,fault}.rs   # codecs, atomic writes, locks, fault injection
  object/{id,types,envelope}.rs    # ObjectId, 9 object types w/ canonical codec, NGOB envelope
  repo/{config,ostore,refs,txn,mod}.rs  # store, refs (CAS+reflog), WAL transactions, Repo facade
  repo/{ignore,index,walk,workspace}.rs  # .newgitignore engine, NGIX cache, safe walk, workspaces
  ops/{tree,snapshot,checkout,status,history}.rs  # core operations
  diff/{myers,render,mod}.rs     # line diff, tree diff, rename detection, unified+JSON render
  merge/{diff3,base,mod}.rs      # 3-way content merge, LCA/ancestry, tree merge
  ops/integrate.rs               # atomic integrate, rollback, checkout_position
  ops/workflow.rs                # goals/changes/evidence/evaluations/proposals (chains)
  ops/verify.rs                  # fsck: coded Issues, deep link walk, reachable() for gc
  ops/gc.rs                      # non-destructive mark&sweep (D-014), grace window
  cli/workflow_cmds.rs           # workflow CLI command families
  cli/{mod,args}.rs + main.rs      # newgit binary: --json, stable exit codes
  obs.rs                            # structured stderr diagnostics
  bin/newgit-faultlab.rs           # crash-test harness child process
  bin/newgit-bench.rs              # benchmark harness (no bench deps)
tests/{common,txn_recovery,concurrency_refs,property_core,version,ops_snapshot,diff_engine,merge_integrate,workflow,cli_e2e,verify_gc,chaos,fuzz_parsers}.rs
docs/{STORAGE_FORMAT,CLI,AGENT_WORKFLOW,BENCHMARKS}.md
docs/                 # STORAGE_FORMAT.md (normative)
.github/workflows/ci.yml
```

## What exists and works (verified by tests)

- SHA-256 object identity; invariant "identical content ⇒ identical id" tested.
- Canonical binary codec for: Blob, Tree, Snapshot, Actor, Goal, Change,
  Evidence, Evaluation, Proposal. Malformed-input rejection tested (no panics).
- NGOB on-disk envelope: magic, version, type tag, lengths, zlib payload,
  trailing SHA-256 digest. Bit-flip/truncation/misfiling/bomb tests pass.
- ObjectStore: atomic put (tmp→fsync→rename→dir fsync), verified get,
  iter, prefix resolution, temp-debris sweep, blob/file size limits.
- fsx: atomic_write, FileLock (O_EXCL, pid+time content, stale reclaim),
  read_limited, path safety (traversal/NUL/drive-letter/symlink-escape checks).
- Fault injection (`NEWGIT_FAULTS`, modes abort/error) for crash tests.
- RepoConfig/Limits: `key = value` config, unknown keys rejected, version gate.

## Failing tests

None.

## What iteration 2 added (verified)

- Repo facade: init (idempotent), open (runs crash recovery), discover (walk-up),
  HEAD (symbolic/detached, txn-journaled updates), actor registry + default
  actor resolution (config → env → anonymous), format-version gate.
- Refs: strict name grammar (reserved namespaces, forbidden chars), CAS updates,
  delete, list/prefix-walk, reflog with txn-id dedup (crash-redo safe).
- WAL transaction engine: global txn lock, recovery-first, CAS preconditions
  before any write, RUNNING journal → apply (idempotent) → COMPLETE marker;
  commit point = journal fsync (forward recovery); corrupt journals quarantined.
- FileLock reclaims locks of provably-dead holders (pid liveness via /proc on
  Linux; stale timeout otherwise) — crash-tested with aborted child processes.
- Fault points: txn:before_journal, txn:after_journal, txn:apply#<i>,
  txn:before_complete, txn:after_complete, txn:ref_write_err, txn:file_write_err,
  ostore:before_write/after_tmp_write/before_rename/after_rename/after_read.
- Tests: 5 crash-recovery scenarios via faultlab child aborts; 4 concurrency
  suites (CAS races ⇒ exactly one winner/version + reflog count equality;
  parallel multi-ref txns; concurrent object writes; concurrent recovery);
  10 property suites (codecs, canonical-form invariants, bit-flip detection).

## What iteration 3 added (verified)

- .newgitignore engine (gitignore subset: wildcards, **, anchoring, negation,
  dir-only; last rule wins; pruning only without negations).
- Safe workspace walk: limits (depth/component/size), symlink record-don't-follow,
  control-char/NUL filename rejection, non-UTF-8 names skipped with warnings,
  .newgit/.git skipped, special files skipped with warnings.
- NGIX index cache (status accelerator; corrupt/missing index rebuilds silently —
  invariant-tested).
- Workspaces: main (repo root) + named (.newgit/workspaces/<name>/files),
  journaled create (ref+meta atomic), discard with dirty-refusal + --force,
  per-workspace op locks, position refs workspaces/<name>.
- Snapshot op: walk → hash (index fast path) → build_tree → Snapshot object →
  CAS ref update in one txn; deterministic with --time/--author.
- Checkout: FreshWorkspace/Overwrite modes, refuses symlink-component
  traversal, restores exec bits + symlinks, feeds index.
- Status: added/modified/deleted classification, list truncation, warnings.
- History: deterministic newest-first traversal (ts DESC, oid DESC).
- CLI `newgit`: init/status/snapshot/history/log/cat/hash-object/workspace/
  actor/config/version/help; --json envelope; stable exit codes; --debug JSONL
  diagnostics on stderr; hand-rolled arg parser with typo rejection.
- Crash tests: snap:before_txn (harmless), snap:after_txn (committed),
  workspace-create txn crash (ref/meta converge).
- E2E: 7 subprocess suites incl. full workflow, JSON errors, exit codes,
  cross-repo snapshot determinism.


## What iteration 4 added (verified)

- Myers O(ND) line diff with prefix/suffix trimming, bounded edit distance
  (cap 1024/file; coarse whole-file replace fallback that stays exact).
- Canonical opcodes (Equal/Delete/Insert/Replace) + reconstruction property
  (reconstruct(a,b,ops) == b for random inputs) + determinism property.
- Tree diff: added/deleted/modified/mode-change classification; rename
  detection in two deterministic stages (exact oid ⇒ 100%, then prefix/
  suffix similarity ≥50% with bounded candidate pairs, greedy with
  (score, old, new) tiebreak); binary detection (NUL in first 8000 bytes);
  symlink target diffs shown as text.
- Renderers: git-shaped unified output (@@ hunks, context merging at gap
  ≤ 2·context, "\ No newline at end of file" markers, rename/mode headers)
  and structured JSON hunks.
- CLI `newgit diff [<a> [<b>]] [-w ws] [--name-only] [--json] [--context N]
  [--no-renames] [--exit-code]`; specs: refs, snapshot/tree oids (prefix ok),
  ws:<name>; omitted b ⇒ live workspace (read-only capture).
- Racily-clean guard (D-011): index entries with mtime ≥ index-file mtime
  are re-hashed — closes the same-tick modification race (caught by the
  symlink diff test; regression-tested).
- capture_tree(save_index=false): read-only worktree capture for diff/status.


## What iteration 5 added (verified)

- 3-way tree merge with exact-rename tracking, mode combining, and honest
  conflict taxonomy (content/opaque/modify-delete/rename-rename/
  rename-delete/mode/dir-file); conflict blobs with diff3 markers stored
  for inspection (merged_oid).
- diff3 content merge on Myers anchors + property tests (determinism,
  trivial-case agreement).
- Bounded deterministic ancestry: is_ancestor, merge_base (LCA by
  (ts DESC, oid DESC); criss-cross documented limitation #14).
- ops: integrate (atomic up-to-date/fast-forward/merge; conflict ⇒ nothing
  written), rollback (new snapshot + old tree; merge_ours-aware),
  checkout_position (resync + stale-tracked-file removal + empty-dir prune).
- CLI: integrate, merge-tree (exit 5 on conflicts, text+JSON), rollback,
  checkout; help texts updated.
- Crash tests: integ:before_txn ⇒ position unchanged; integ:after_txn ⇒
  ref durable, status honestly dirty, `checkout` repairs.
- Concurrency test: same-target integrates serialize on the workspace lock
  (lock acquired BEFORE position read); exactly one merge snapshot; reflog
  length asserted.
- Merge roles in extras (merge_ours/merge_theirs) because parents is a
  canonically sorted set (D-004/D-012).


## What iteration 6 added (verified)

- Mutable entities as CAS-guarded version chains (D-013): chains/<root-hex>
  refs, extras.prev audit links, validated state machines for goal/change/
  proposal lifecycles.
- Honesty gates (tested): tested-requires-evidence; approve-before-integrate;
  integrated only via proposal integrate.
- Evidence: `evidence record` runs commands itself (exit code → verdict,
  capped output blob, duration metric, truncation flag, signal →
  inconclusive); `evidence add` for opinions (deterministic=false default).
- Evaluations: deterministic aggregation (from-evidence; all-pass/any-fail/
  worst-per-dimension; opinion-labeled notes) vs explicit --ai opinions.
- proposal integrate: one txn moves position ref + proposal chain + change
  chain; conflict ⇒ exit 5 zero writes; ff detection; post-commit checkout.
- CLI: goal/change/evidence/evaluation/proposal families, history --goal
  (respects -w/--from), workspace create --author.
- docs/AGENT_WORKFLOW.md: REAL transcript of two agents on one goal
  (qwen-coder multiplication vs claude-coder addition), evidence-recorded,
  AI opinion flagged, proposal approved+integrated, goal achieved.
- Tests: 9 workflow suites (incl. chain-linearity concurrency, crash
  atomicity at proposal:before/after_txn, evidence truncation/signal,
  honesty gates) + two-agent e2e.

## What iteration 7 added (verified)

- `newgit verify [--deep] [--json]` — read-only fsck (D-014): object
  layout/name/envelope/digest/misfiled/noncanonical (+deep re-encode &
  link walks), refs/HEAD/reflog-line grammar, chains (head/prev/cycle/
  type/root-prev), workspaces (meta/files/position-ref/index cache),
  txn dir (locks, pending journals), config. Stable issue codes;
  errors ⇒ exit 3; crash debris ⇒ warnings with repair hints;
  `verify_never_modifies_the_repository` enforces read-only.
- `newgit gc [--dry-run] [--force-now] [--json]` — strictly non-destructive
  mark&sweep: roots = HEAD + all refs + ALL reflog oids + workspace
  base_oids; mark follows extras.prev chain links; global txn lock held;
  24h mtime grace window; corrupt/misfiled/quarantine/non-object debris
  NEVER deleted (kept_corrupt/quarantined counters); missing_links reported
  on damaged repos instead of failing; empty shards pruned + fsynced.
- `newgit recover [--json]` — explicit recovery pass + journal
  CHECKPOINTING (D-015): successful txns delete their journal; recovery
  deletes terminal-state journals. Found by the new e2e test — before the
  fix, COMPLETE journals accumulated forever and every open rescanned them.
- Chaos suite (tests/chaos.rs): 6 fixed xorshift64* seeds × 14–25 random
  ops (snapshot/ws-create/integrate/put-blob/txn-set) each killed at random
  fault points in child processes; after EVERY step: open auto-recovers,
  deep verify zero errors, all refs resolve, status computes; end-of-seed:
  gc cleans debris, full history walk survives, repo still usable.
  Found 2 real bugs (temp-debris misclassified as error; journal buildup).
- Fuzz-like parsers (tests/fuzz_parsers.rs): 110k seeded prefix-anchored
  garbage inputs vs envelope/canonical/index/journal/config/hex/base64/
  ref-grammar — no panics, no unbounded allocation (I4).
- Benchmarks: src/bin/newgit-bench.rs (zero extra deps) + docs/BENCHMARKS.md
  with REAL numbers (put_blob 0.09ms; snapshot 1k cold ~50ms / warm 2.4ms;
  5k cold ~214ms; status 1.9/4.5ms; diff ~1ms; integrate ~5.3ms; history
  500 ~4ms; verify deep ~37ms; gc 1000 orphans ~19ms) + regression policy.
- CLI e2e: verify/gc/recover contract test (exit codes, JSON envelopes,
  forensics preservation). 33 new tests total.

## Iteration 8 outcome (facts for resume)

- `src/gitio/fastexport.rs`: Event::Blob/Commit/Tag/Reset/Meta; total parser
  (`Parser::new(BufRead)`, `from_child_stdout`); FileOp incl. Gitlink (raw
  40-hex `M` source = submodule); MAX_DATA 2 GiB; 7 unit tests pin real-git
  framing quirks (exact data lengths, optional trailing LF, deleteall under
  --full-tree, tag block order, light-tag reset form).
- `src/gitio/import.rs`: `import_git(repo,&Path)->ImportReport`; streams
  `git fast-export --all --full-tree --show-original-ids --signed-tags=strip
  --tag-of-filtered-object=drop`; actors cached (name,email)→Actor
  `git:<email>`; snapshot extras: git_sha1, git_committer_* (only when ≠
  author), git_parents_ordered (>1 parent), git_message_lossy; modes
  100644/100755/120000→File/Executable/Symlink; 160000/Gitlink→Err (atomic
  refusal); skip_ref(): remotes|notes|replace|stash|bisect|worktree (skips
  recorded from Commit/Tag/Reset branches, deduped); ALL refs + HEAD in ONE
  txn (TxnOp::File{rel:"HEAD"}); child exit status checked after stream.
- `src/gitio/export.rs`: `export_git(repo,&Path)->ExportReport`; target must
  be absent/empty; skips workspaces//chains/; non-snapshot ref→Err; topo DFS
  (two HashSets); deterministic marks (blobs 1..B by topo+sorted-path,
  commits B+1..); per-ref D+M diff vs first parent; final `reset <ref> from
  :mark` pins tips; pipes to `git init --quiet` + `git fast-import --quiet
  --done --export-marks=<tmp>` (the --export-marks= form MUST be one argv
  entry — splitting it caused exit 129); marks→sha map; HEAD symbolic→
  `git symbolic-ref` + `reset --hard --quiet`, detached→`reset --hard <sha>`;
  map_ref_name: heads/tags pass through, other refs/X→refs/heads/X, bare→
  refs/heads/name; C-quoting only for control/quote/backslash.
- CLI: `import-git <git-repo>` / `export-git <target-dir>` (--json envelope =
  serde report; text mode lists per-ref mappings, skipped refs, stripped
  annotated tags; help section "Git interop"). NOTE: `cat` still resolves
  hex prefixes only, NOT ref names (cli_e2e takes tip oid via `history
  --from <ref> --json`).
- Verified guarantees (tests/git_compat.rs, 9/9 vs real git 2.47): import
  content equality (ls-tree/cat-file/log per commit: paths, modes, bytes,
  messages, author, tz, parent counts) · import determinism · export
  round-trip: byte-identical blob SHAs + `%an|%ae|%cn|%ce|%at|%ct|%s`
  multiset equality + first-parent lineage + clean worktree + symlink on
  disk · native-repo export · submodule refusal atomic · empty repo ·
  import-into-used-repo atomic ref move · export refusals (dirty target,
  non-snapshot ref) · reimport-after-export fixpoint on trees/messages.
- Test gotchas learned: git `%B` appends record-terminating newline
  (trim_end both sides); `rev-list --all` includes notes/remotes commits
  (use `--branches --tags`); `git checkout -f` does NOT materialize a
  fast-imported worktree (`reset --hard` does); snapshot workspace names
  cannot contain '/'; exec-bit recipe = add → update-index --chmod=+x →
  commit; bind `git_out()` String to a local before `.split_whitespace()`
  (E0716).
- Fuzz: `fuzz_fastexport_parser_never_panics` added (20k prefix-anchored
  inputs, 4 real-framing prefixes, bounded drain).

## Current task (next iteration)

**Iteration 9: Remote protocol + server (HTTP/1.1, JSON v1, std-only).**
Completion condition:
1. `docs/PROTOCOL.md`: versioned JSON-over-HTTP v1 spec — endpoints
   (`/v1/info`, `/v1/objects` batch get/put with digests, `/v1/refs` list +
   CAS update txn, `/v1/snapshot` push/pull negotiation by oid sets,
   `/v1/history`), envelope `{ok,data|error}` identical to CLI, version
   header `X-NewGit-Version`, explicit capability negotiation.
2. `src/remote/`: hand-rolled HTTP/1.1 server on std TcpListener (no
   framework): request-line/headers parser (total, capped), body limits,
   timeout, keep-alive optional; thread-per-connection with bounded pool.
3. Auth: bearer tokens (SHA-256 hashed at rest in `.newgit/config` or
   tokens file), roles read|write|admin mapped to endpoint classes; audit
   log (who/what/when/result) as reflog-style append-only file. No TLS in
   v1 (documented; recommend reverse proxy) — zero-rupee.
4. Client: `newgit remote add/list/remove`, `newgit push <remote>`,
   `newgit pull <remote>` — negotiation: send local oid set digest → server
   responds missing/wanted lists → batched object transfer (base64 or
   binary+digest per object), refs moved via server-side txn (CAS);
   atomicity + crash-safety reuse existing txn engine on both ends.
5. Security: threat-model section E realized — parser fuzz joins
   tests/fuzz_parsers.rs; request-size caps; no path/JSON injection;
   authz tested (read cannot write; bad token exit 7); loops only to
   127.0.0.1 in tests.
6. Tests: tests/remote_e2e.rs — spin server on ephemeral port in-process;
   clone-equivalent pull into fresh repo (oid equality), push CAS race
   (one winner), auth failures, oversized request refusal, crash mid-push
   (server restart → verify clean); cli_e2e remote commands --json.
7. Docs + state updates; commit.

## Next tasks (ordered)

9. Remote protocol (HTTP/1.1, JSON v1) server+client, auth, audit. ← CURRENT
10. Web UI served by remote server; MCP/agent-API docs.
11. Release engineering (dist script, checksums, reproducible build);
    benchmark re-run + full docs set; dependency audit/SBOM.
12. Final forensic audit; production-readiness gate decision.

## Important decisions (full log in DECISIONS.md)

- Rust, minimal dependencies, no frameworks; hand-rolled CLI parser and HTTP.
- SHA-256 ids; canonical encodings are a frozen protocol (docs/STORAGE_FORMAT.md).
- Objects immutable; set-like fields strictly ascending for unique canonical form.
- Evidence has `deterministic` flag; AI opinions must be flagged (`ai_generated`).
- Git interop via system git fast-export/fast-import (no gitoxide dependency).
- No staging area: workspaces hold live state; `snapshot` captures whole workspace.

## Commands

```bash
cargo test                     # unit + integration
cargo test --release           # perf-sensitive suites (chaos)
NEWGIT_CHAOS_ITERATIONS=500 cargo test --release --test chaos
```
