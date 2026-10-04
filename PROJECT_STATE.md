# PROJECT_STATE.md — NewGit autonomous build loop

> Machine-and-human readable continuation state. Updated at the end of every
> iteration. If context is lost, resume from this file.

## Current status

- **Phase:** The 12 original implementation iterations and v0.1.0 publication are complete; incremental Git compatibility work continues. Current bounded milestone: prove empty-tree Git histories survive semantic import/export/reimport (recorded below).
- **Public repository:** [kakarot700/newgit](https://github.com/kakarot700/newgit), public, default branch `main`; the original 12 implementation commits remain in its history.
- **Classification:** **PRODUCTION-CANDIDATE**, pre-1.0 and not a blanket Production Ready certification.
- **Hosted verification:** the publication baseline passed GitHub CI run [37182199247](https://github.com/kakarot700/newgit/actions/runs/37182199247) and CodeQL run [37182199239](https://github.com/kakarot700/newgit/actions/runs/37182199239) on Ubuntu 24.04 commit `afa94c4`. The detached-HEAD/ref-integrity implementation commit `6ca3eec9b2e65b77e6e975127868bcec9079231a` was pushed to `main`; GitHub CI run [37200186462](https://github.com/kakarot700/newgit/actions/runs/37200186462) and CodeQL run [37200186384](https://github.com/kakarot700/newgit/actions/runs/37200186384) both completed successfully on that exact SHA.
- **Git message compatibility hosted validation:** implementation commit `52fa27a6a8d5cba4fbbdc87cc74acf31f06b84f2` passed GitHub [CI run 37201393466](https://github.com/kakarot700/newgit/actions/runs/37201393466) and [CodeQL run 37201393416](https://github.com/kakarot700/newgit/actions/runs/37201393416) on `main`.
- **Empty-tree compatibility hosted validation:** commit `ba5eaed79cf778bf77d66fbea0bb6c0d2b46c6cb` passed [CI run 37203669979](https://github.com/kakarot700/newgit/actions/runs/37203669979) and [CodeQL run 37203669978](https://github.com/kakarot700/newgit/actions/runs/37203669978), both on that exact SHA.
- **Clean-clone verification:** commit `b648079` built with `--locked`; all 278 debug and release tests passed, and the README install/CLI quick start, agent workflow, and real-Git import/export smoke checks passed.
- **Publication local verification (2026-10-04):** `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` (278 passed), `cargo build --release --locked`, `cargo test --release --locked` (278 passed), and the generated-SBOM drift check passed on the pinned Rust 1.99.0 toolchain.
- **Security controls and scans:** the pre-publication Gitleaks v8.30.1 scan found 0 findings across the then-current worktree and history; GitHub secret scanning/push protection, Dependabot alerts/security updates, and private vulnerability reporting are enabled. actionlint v1.7.12 found no workflow errors.
- **Publication deliverable:** the preserved development history, public repository, release decision, and completion/readiness report. The active compatibility continuation is tracked below.

## Previous Git compatibility milestone — detached `HEAD` and ref integrity (2026-10-04)

- **Evidence and defects:** real Git 2.43.0 probes showed `git fast-export --all` emits a pseudo-ref named exactly `HEAD` for detached checkouts. The importer previously persisted it as a regular NewGit ref, while export could omit detached-only history or fail to reproduce detached state. Adversarial review also confirmed that `main` and `refs/main` can alias one Git ref and one source ref was silently overwritten.
- **Implementation:** import treats the exact stream label as pseudo-ref-only while mapping detached `HEAD` directly in its atomic transaction. Export roots detached history at a temporary ref, checks out the target with `git checkout --detach`, and deletes the temporary ref. Its allocator avoids exact and slash-delimited ancestor/descendant collisions on every suffix attempt. Export now rejects multiple NewGit names mapping to the same Git ref before initializing the target.
- **Regression evidence:** real-Git tests cover detached-only history, a detached tip ahead of a branch, two successive nested temporary-ref candidates, and a valid redundant-ancestor merge whose ordered parents survive export. The mapped-ref alias test asserts the two mappings, validates the destination with `git check-ref-format`, and checks that export refuses before creating partial output. The suspected topological-sort failure was not reproduced; no sorter change was made.
- **Local verification:** `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, full `cargo test --locked` (283 passed), `cargo build --release --locked`, full `cargo test --release --locked` (283 passed), SBOM drift check, and `git diff --check` all pass. The focused `tests/git_compat.rs` suite contains 14 passing tests. The exact implementation SHA, push state, and hosted conclusions are recorded above.

## Previous Git compatibility milestone — exact tested UTF-8 messages and control-byte refusal (2026-10-04)

- **Evidence gap and attack:** the previous interoperability fixture compared
  messages with `trim_end()`, so it did not prove byte-exact preservation of
  significant leading/trailing whitespace or newline framing. An independent
  review also found that Git accepts U+0001 in a commit message while NewGit's
  snapshot text model rejects it; the generic validation error did not explain
  the Git boundary.
- **Regression:** `tests/git_compat.rs::commit_message_roundtrip_preserves_exact_utf8_bytes`
  creates commits through the Git CLI with leading/trailing blank lines,
  trailing spaces, CRLF, no final LF, and an empty message. It compares raw
  `git cat-file commit` message bytes to the fixture bytes, imported
  `Snapshot.message` bytes, and exported raw commit-object payloads. It does
  not use `%B` pretty-format output or normalize whitespace.
- **Implementation:** valid UTF-8 bytes are preserved for the tested message
  cases. Rather than relaxing core text validation or exposing control bytes to
  consumers, import now returns an explicit error containing the source commit
  and U+XXXX code point for C0 controls other than LF/CR/TAB and DEL. It refuses
  before the import ref transaction; a real Git U+0001 fixture confirms zero
  refs moved and a healthy repository.
- **Local verification:** all 16 `tests/git_compat.rs` cases pass with Git
  2.43.0; `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D
  warnings`, full debug and release suites (285 passed each), release build,
  SBOM drift check, and `git diff --check` all pass. Hosted CI and CodeQL checks
  both passed on implementation SHA `52fa27a6a8d5cba4fbbdc87cc74acf31f06b84f2`
  (runs are linked in Current status above).

## Previous Git compatibility milestone — C-quoted UTF-8 pathnames (2026-10-04)

- **Defect and reproduction:** a real Git 2.43.0 repository with a filename
  containing both non-ASCII UTF-8 and Git-quoted punctuation imported correctly,
  but `export-git` changed `café` to `cafÃ©` whenever `quote_path` entered its
  byte-at-a-time escaping branch. Blob identity stayed the same while the path
  changed, so a tree comparison exposed the data-loss bug.
- **Fix:** `src/gitio/export.rs::quote_path` now escapes the Git C-quote syntax
  while iterating Unicode scalar values, preserving valid UTF-8 rather than
  reinterpreting individual UTF-8 bytes as Unicode code points.
- **Regression evidence:** `tests/git_compat.rs::quoted_utf8_git_paths_roundtrip_without_changing_names`
  creates real Git commits for `café "quoted"\\name.txt` and a rename to
  `quoted "résumé"\\file.txt`. It confirms fast-export's octal UTF-8, quote,
  and backslash escapes, then compares Git `ls-tree -z` paths, modes, and blob
  IDs at each commit after NewGit import/export. The new test failed before the
  fix (`café` vs `cafÃ©`) and passes after; it also checks a clean exported
  worktree and deep NewGit verification.
- **Adversarial review and boundary:** an independent review found no material
  issue in the character-based encoder or fixture. This evidence covers tested
  valid UTF-8 names only; NewGit's non-UTF-8 path limitation remains, and the
  fixture does not claim broad control-character or cross-platform pathname
  compatibility.
- **Local verification:** Git 2.43.0, Rust 1.99.0; all 17 Git interoperability
  tests passed as part of both complete suites (286 debug and 286 release).
  `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
  `cargo build --release --locked`, SBOM drift check, and `git diff --check`
  all passed. Hosted GitHub CI [run 37202630994](https://github.com/kakarot700/newgit/actions/runs/37202630994)
  and CodeQL [run 37202630998](https://github.com/kakarot700/newgit/actions/runs/37202630998)
  both completed successfully on implementation SHA
  `b4e1ca5dd12b2d816fbd05f03416dc903a4a014a`.

## Current Git compatibility milestone — empty Git trees (2026-10-04)

- **Evidence gap:** the matrix distinguished an empty Git repository (no
  commits) from a commit whose tree is empty, but the latter was marked NOT
  TESTED. A real Git 2.43.0 probe showed the existing stream conversion can
  preserve a canonical empty root tree; the bounded work here establishes the
  repeatable regression contract rather than changing the core architecture.
- **Regression:** `tests/git_compat.rs::empty_git_trees_roundtrip_across_root_and_followup_commits`
  uses Git CLI commits for an empty root, a populated tree, deletion of its
  only file back to empty, and a consecutive empty commit. It compares source
  and exported Git tree IDs, source-parent to exported-parent mapping, NewGit
  root-tree entries, and every path/mode/blob through both conversion legs.
  It also runs `git fsck --full` and deep NewGit verification after import and
  reimport.
- **Independent attack:** review caught that the first test version compared
  only parent counts and checked only empty/non-empty status on final reimport.
  Both assertions were strengthened: mapped parent identities must match, and
  the complete reimported path/mode/blob set is compared with Git.
- **Boundary:** empty-tree handling is now SUPPORTED only for this tested
  semantic round-trip on Git 2.43.0/Linux. This does not claim general Git
  object-hash or cross-platform compatibility.
- **Local verification:** `cargo fmt --check`, `cargo clippy --all-targets
  --locked -- -D warnings`, full debug and release suites (**287 passed each**),
  `cargo build --release --locked`, SBOM drift check, and `git diff --check` all
  pass. `tests/git_compat.rs` has **18 passing tests**. Hosted CI and CodeQL
  passed on the exact pushed implementation SHA; a following documentation-only
  head is being checked separately.

## Local development setup

Run from the repository root. The checked-in toolchain file pins Rust 1.99.0;
`rustup` installs that toolchain when Cargo is first invoked in the checkout.
System Git 2.20 or newer is required for Git import/export and those tests.

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --release --locked
```

These are the reproducible contributor commands; they do not depend on a
machine-specific Cargo or Rustup directory.

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
  gitio/{fastexport,import,export}.rs  # git interop: total parser, atomic import, deterministic export (D-016)
  remote/{proto,http,auth,audit,negotiate,server,client}.rs  # protocol v1: wire types, HTTP, tokens/roles, audit, negotiation, server, push/pull, agent read endpoints (D-017/D-018)
  ui/{mod.rs,index.html}         # embedded Web UI: data-free static shell, XSS-safe by construction (D-018)
  cli/workflow_cmds.rs           # workflow CLI command families
  cli/remote_cmds.rs             # serve/ui/remote/push/pull/token/audit commands
  cli/mcp.rs                     # MCP stdio JSON-RPC 2.0 server: 13 tools → cli::call_json (D-018)
  cli/{mod,args}.rs + main.rs      # newgit binary: --json, stable exit codes, pub call_json
  obs.rs                            # structured stderr diagnostics
  bin/newgit-faultlab.rs           # crash-test harness child process
  bin/newgit-bench.rs              # benchmark harness (no bench deps)
tests/{common,txn_recovery,concurrency_refs,property_core,version,ops_snapshot,diff_engine,merge_integrate,workflow,cli_e2e,verify_gc,chaos,fuzz_parsers,git_compat,remote_e2e}.rs
  docs/{STORAGE_FORMAT,CLI,AGENT_WORKFLOW,AGENT_GUIDE,BENCHMARKS,GIT_COMPAT,PROTOCOL}.md
.github/workflows/ci.yml           # GitHub Actions checks, dependency security, reproducible packaging, and tag releases
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

## Iteration 9 outcome (facts for resume)

- `src/remote/proto.rs`: PROTOCOL_VERSION=1; HDR_PROTOCOL="x-newgit-protocol";
  is_internal_ref()=workspaces/|chains/; wire structs (serde): InfoData,
  RefsData/RefEntry, HaveReq/Data, NegotiateReq/Data{send}, ObjectWire{data_b64},
  ObjectsGetReq/ObjectsData, ObjectsPutReq/PutData, CasWire (tagged kind
  any|exactly{old:Option}), RefUpdateWire, RefsUpdateReq/UpdateData, AuditData,
  PushReport{remote,url,refs_pushed,objects_sent,bytes_sent,had_probe_requests},
  PullReport{...,refs_updated,refs_up_to_date,objects_received,bytes_received,txn_id}.
- `src/remote/http.rs`: total parser read_request(BufRead,max_body)->
  Result<Request,(u16,String)>; caps MAX_REQUEST_LINE 16K/128 headers/64K total;
  methods GET|POST|HEAD|PUT|DELETE else 405; HTTP/1.x only; origin-form paths
  only; percent_decode total ('+'→space, bad %XX literal, lossy utf8); chunked
  →501; CL required for body, cap→413; write_response(status,content_type,body)
  with CL+Connection:close+protocol header; status_for_error: Auth→401,
  CasFailed|LockBusy|Conflict→409, Limit→413, NotFound|RefNotFound|NotRepo→404,
  Invalid|Malformed|InvalidRef|Protocol→400, else 500. 9 unit tests.
- `src/remote/auth.rs`: Role read<write<admin (Ord); TokenFile{tokens:[{id,
  sha256 hex,role}]}; hash_token=sha256 hex; generate_token=32B /dev/urandom
  base64 (loud error if unavailable); load (missing→empty; malformed→Config;
  id/sha checks) / save (atomic_write+0600 unix); authenticate(None header)→
  Ok(None) anonymous, bad header→Err(Auth) NEVER downgrades; add() rejects dup
  id/dup token/bad id chars; authorize(p,required)=role>=required. 4 unit tests.
- `src/remote/audit.rs`: AuditLog::new(ng) → ng/audit.log; append(principal,
  method,path,status,error_category) JSONL under FileLock+fsync, BEST-EFFORT
  (failures→obs event only); tail(limit) skips damaged lines. 1 unit test.
- `src/remote/negotiate.rs`: closure(repo,roots)=verify::reachable().0;
  post_order(repo,roots,exclude:HashSet)->Vec dependencies-first, iterative
  DFS (oid,expanded) stack, exclude prunes WITHOUT reading (peer vouches
  closure), missing non-excluded object→Error::Protocol. 2 unit tests.
- verify.rs refactor: `pub fn object_links(&Object)->Vec<ObjectId>` is now the
  SINGLE SOURCE for links (incl. extras.prev chain links); reachable() uses it.
- `src/remote/server.rs`: ServerConfig{bind,repo_root,token_file,
  allow_anonymous_read,max_body 64MiB,max_threads 32}; spawn()→ServerHandle
  {addr(),port(),shutdown()}; nonblocking accept + 50ms poll; thread-per-conn
  bounded (AtomicUsize, over cap→429); IO_TIMEOUT 30s; repo+tokens reloaded
  PER CONNECTION; routes: GET /healthz,/v1/info (anon), /v1/refs+/v1/audit
  (GET) + POST /v1/have,/v1/negotiate,/v1/objects/get (read; anon iff
  allow_anonymous_read), POST /v1/objects/put,/v1/refs/update (write),
  GET /v1/audit (admin); unknown→404, wrong method→405; EVERY request
  audit-logged (principal id | "anonymous" | "bad-token"); envelope
  {ok,data|error{category,message}}.
  - objects/put: b64→envelope::verify(max_object_bytes)→from_canonical→
    object_links all contains (HAVE-INVARIANT: dependency-order enforced;
    violation→400 "missing dependency") → put_canonical; sequential, first
    error aborts batch (earlier objects remain = harmless orphans).
  - refs/update: check_ref_name_system + is_internal_ref REFUSED + new oid
    must be stored + ONE txn::execute with Cas mapping + RefLogEntry::system
    ("remote update by <principal>[: msg]"); CAS fail→409 cas_failed.
  - negotiate: have filtered to stored (superset semantics), want must all
    exist (else 404), send=post_order(want, exclude=closure(have)); batch cap
    max(max_batch_objects,10_000) for have/want args.
  - listening_line(addr) prints "newgit serve: listening on http://ADDR
    (protocol v1)" — CLI prints+flushes it (port-0 discovery contract, tested).
- `src/remote/client.rs`: REMOTES_FILE=remotes.json (0600, token raw inside —
  documented trust model); Remote{name,url,token}; load/save/add/remove/get;
  validate_url: http://host[:port] ONLY (https→Invalid w/ "plain HTTP" hint,
  paths refused, default port 80); Client::call(method,path,body)→data Value:
  one request per connection, connect 10s/read 300s/write 60s timeouts,
  read_response total (status line+headers+CL body; caps 64K head/2GiB body);
  error mapping by (status,category): 401|403→Auth, (409,cas_failed)→CasFailed,
  (409,lock_busy)→LockBusy, 409→Conflict, 413|limit→Limit, 404→RefNotFound,
  malformed→Malformed, auth→Auth, else Protocol.
  - push(repo,remote,refs,force): info (protocol check)→refs→NON-FF GUARD
    (unless force: server tip must exist locally AND ∈ closure(local tip),
    else Error::Conflict "non-fast-forward…pull first")→BFS have-probes
    (batched /v1/have; descent stops at server-held oids)→send=post_order
    (tips,exclude=known_server)→objects/put batches (raw envelope files b64)→
    refs/update ONE txn cas exactly(observed server tip)|any(force).
  - pull(repo,remote,filter): info→refs (internal filtered)→negotiate
    {have:local tips+head, want:selected}→objects/get batches→PER-OBJECT
    local re-validation (envelope::verify + id==requested + object_links all
    contains)→put_canonical→local txn refs move cas exactly(observed local
    tip), RefLogEntry "pull from remote…"; HEAD/workspaces UNTOUCHED (fetch
    semantics by design, D-017).
  - default_push_refs=HEAD symbolic name (detached→Invalid); all_push_refs=
    refs.list minus internal (list itself never returns internal names —
    user grammar filters them; double defense).
- CLI (`src/cli/remote_cmds.rs`, dispatch in mod.rs): serve [--bind
  --token-file --max-body --max-threads --allow-anonymous-read] (fails fast
  on bad token file BEFORE bind; prints listening line; blocks in 1h sleeps;
  --json prints {listening,protocol} then blocks); remote add|list|remove
  (list masks tokens "set|none"); push <remote> [refs…|--all] [--force];
  pull <remote> [refs…] (alias fetch); token add|list|remove [--role
  --token --token-file]; audit [-n N] (text: ts/principal/method path/
  status/error; json {entries,count}). Help section "Remote".
- Exit codes seen in tests: 0 ok · 2 usage (bad url/flags) · 3 protocol/repo
  (unreachable server, verify) · 4 cas_failed (wire race) · 5 conflict
  (non-FF) · 6 limit · 7 auth (401/403).
- Fuzz: fuzz_http_and_wire_json_never_panic (20k inputs, 4 HTTP prefixes,
  read_request+percent_decode+4 wire structs).
- Test gotchas learned: Repo::init does NOT create missing parent dirs
  (test helpers must create_dir_all); unborn workspaces have NO position ref
  (create one via refs.update with system grammar); refs.list() applies the
  USER grammar so workspaces/* refs are writable but invisible in listings;
  verify --json has no "errors" field (issues[] + exit-3 contract; use
  objects_checked); CasFailed category string is "cas_failed" (not "race");
  serve child stdout line parse: split("http://").nth(1) then split(' ');
  tempfile::TempDir IS a dev-dep (used by cli_e2e/remote_e2e).

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

## Iteration 10 outcome (facts for resume)

- **UI** (`src/ui/index.html`, `include_str!` via `src/ui/mod.rs`, `pub mod ui`):
  single file, zero external resources (CI-asserted: no http(s)://, innerHTML,
  eval, document.write, <link>, url()); dark theme; hash router `#/`,
  `#/history?ref=`, `#/object/<oid>`, `#/goals`, `#/proposals`,
  `#/compare?a=&b=`, `#/audit`; login = bearer token → sessionStorage
  `ng_token` (+ anonymous checkbox); model-flow strip
  GOAL→CHANGE→EVIDENCE→PROPOSAL→INTEGRATION; evidence shows
  DETERMINISTIC/NON-DETERMINISTIC badges; evaluations show purple "AI OPINION"
  badge + "opinion never fact" notice; blob preview ≤64 KiB (text/hex);
  history caps 200 rows client-side. READ-ONLY (KL #33).
- **Server** (`src/remote/server.rs`): `ServerConfig.ui` flag; static route
  `GET /`+`/index.html` (no auth, principal "ui-static", text/html) only when
  ui=true; new endpoints `POST /v1/object`, `POST /v1/diff`,
  `GET /v1/goals|changes|proposals` (read role; `changes?goal=<hex>`
  server-side filter, non-matching ⇒ empty list); capabilities +=
  object/diff/goals/changes/proposals (+`ui` conditional). Object endpoint:
  non-blobs `{oid,kind,links,data}` (links via `verify::object_links`), blobs
  `{oid,kind:"blob",data_b64,size,links:[]}`. Diff endpoint: specs via
  `diff::resolve_tree`; `content:true` ⇒ `unified:[{path,unified}]` rendered
  by the CLI's own `render::file_header`+`render_content`, UNIFIED_CAP=100
  modified non-binary files (KL #35). Wire types in `src/remote/proto.rs`:
  ObjectReq/ObjectData, DiffReq/DiffData/UnifiedFile, EntityEntry/ListData.
- **CLI**: `serve --ui` (default OFF); `newgit ui` = serve with --ui forced,
  prints second line `web UI: http://HOST:PORT/ ...`; `serve --json` data now
  `{listening,protocol,ui,ui_url}`. `cli::call_json(repo,args)` = PUBLIC
  programmatic dispatch returning the JSON data payload (Text→json string,
  Raw→b64) — MCP and tests share it.
- **MCP** (`src/cli/mcp.rs`): newline-delimited JSON-RPC 2.0 on stdio;
  protocolVersion echoes client (default 2024-11-05); initialize/ping/
  tools-list/tools-call; notifications silent; batches -32600, parse -32700,
  unknown method -32601, non-object arguments -32602; unknown tool ⇒
  isError tool result with category "invalid" (NOT a protocol error); NewGit
  failures ⇒ `{content:[{type:"text",text:<envelope>}],isError:true}`;
  13 tools newgit_{status,history,cat,diff,snapshot,verify,integrate,
  workspace,goal,change,evidence,evaluation,proposal} — `build_argv` maps to
  exact CLI syntax (unit-tested mapping table); EOF ⇒ exit 0.
- **Tests 269→278**: lib +5 (`ui_is_self_contained_and_xss_disciplined`,
  `jsonrpc_handshake_and_catalog`, `protocol_errors_are_proper_jsonrpc`,
  `argv_building_matches_cli_syntax`, `tool_call_end_to_end_on_a_real_repo`);
  remote_e2e +2 (`ui_is_served_only_when_enabled_and_carries_no_data`,
  `object_diff_and_workflow_endpoints` — fixtures built via `call_json`,
  honesty gate caught proposal-before-tested during writing); cli_e2e +2
  (`ui_command_serves_browser_shell`, `mcp_speaks_jsonrpc_over_stdio` — real
  child processes; parse the `http://ADDR` token from the announcement, the
  LAST whitespace token is `v1)`).
- **Docs**: docs/AGENT_GUIDE.md NEW (3 interfaces table, curl recipes, MCP
  quickstart, category contract, two-agent example); PROTOCOL.md (read
  endpoints + static route + serve --ui + capabilities); CLI.md (ui/mcp
  sections); SECURITY_MODEL §8; THREAT_MODEL §E2 (+3 it9 pending rows
  resolved honestly); KNOWN_LIMITATIONS #33–36; DECISIONS D-018; CHANGELOG;
  ROADMAP [x]10; RELEASE_READINESS updated.
- Gotchas learned: `writeln!(w, r#"{...}"#)` needs `"{}",` (format-string
  lint); `Vec<String> += [..]` doesn't compile (use extend); `tail.clone()`
  on `&[String]` clones the REFERENCE (use to_vec); cmd_init takes the dir
  as a POSITIONAL (ignores --repo for creation).

## Iteration 11 outcome (facts for resume)

- dist: `bash scripts/dist.sh [--skip-build]` → dist/newgit-0.1.0-x86_64-unknown-linux-gnu{,.tar.gz};
  SHA256SUMS.txt covers every packaged file (27 verified OK); tarball hash
  3578f85b…; dist.sh FAILS if LICENSE files missing; /dist/ gitignored.
- SBOM: `python3 scripts/sbom.py > SBOM.md` — deterministic (no date);
  classification via host-context walk of cargo metadata resolve graph
  (proc-macro crates + build-edge deps = build-time): 21 runtime, 7
  build-time (proc-macro2/quote/syn/unicode-ident/version_check/serde_derive/
  thiserror-impl), 34 dev-only. Drift gate: `sbom.py | diff -u SBOM.md -`.
- Reproducibility: the archived same-host check used two separate clean release
  target directories and recorded identical binary hashes; Rust 1.99.0 is
  pinned in rust-toolchain.toml; profile: lto=thin, debug=false, strip=true.
- cargo-audit 0.22.2 was installed for the original audit run; `cargo audit`
  reported 1290 advisories and 63 locked crates with ZERO findings. The local
  installation and advisory-database cache locations are intentionally omitted;
  hosted CI re-runs the audit against its current database.
- build.rs audit (vendored sources read): crc32fast(35L), generic-array(5L),
  libc(605L), serde(69L), serde_core(113L), serde_json(30L), thiserror(195L),
  zmij(45L) — all Command::new uses are rustc probes; libc also
  freebsd-version/emcc (non-Linux paths); NO TcpStream/reqwest/http anywhere.
- cargo-deny 0.20.2 installed + RUN: `check advisories bans licenses sources`
  → all ok, zero warnings (after trimming allow-list to exact SBOM set:
  MIT/Apache-2.0/0BSD/Unicode-3.0/Zlib/Unlicense).
- CI (rewritten): checks (fmt/clippy/test/test-release/build/artifact),
  security (SBOM drift, taiki-e/install-action@cargo-audit prebuilt,
  `cargo audit --deny warnings`, cargo-deny-action@v2), dist (repro check
  two target dirs, dist.sh, checksum verify, artifacts, tag → gh release
  via GITHUB_TOKEN). REMOVED fake NEWGIT_CHAOS_ITERATIONS env (chaos.rs has
  no such knob — fixed seeds by design). deny.toml: license allow-list
  (MIT/Apache/BSD/0BSD/Zlib/Unlicense/Unicode), version_check clarify
  (non-SPDX "MIT/Apache-2.0"), bans: git2/gix/libgit2-sys/openssl(-sys)/
  ring/tokio/hyper/axum/actix-web, sources: crates.io only, wildcards deny.
- Benchmarks it11 re-run (docs/BENCHMARKS.md): put_blob 0.10 / snapshot-1k
  cold 61.35 med (min 50.17) / warm 2.30 / status 1.83 cached / diff 1.43 /
  history 4.27 / integrate 7.34 / verify-deep 43.19 / gc 21.28 — worst
  drift 1.41× vs it7 (noise band; <2× gate); it7 table ARCHIVED below.
- Docs NEW: DEPLOYMENT.md (systemd hardening unit, nginx/caddy TLS, client
  is http://-ONLY — validate_url REJECTS https (client.rs:127) → ssh
  tunnel pattern; ops table: verify/gc/audit/backup=rsync-or-push/limits),
  TESTING.md (layers table, fault injection, 5 no-fake rules),
  TROUBLESHOOTING.md (exit codes, symptom→fix; locks are <path>.lock files,
  tokens.json = {"tokens":[{id,sha256,role}]}), CONTRIBUTING.md (rules,
  gates, code map). KL #37 (repro scope), #38 (advisory-database snapshot and
  hosted workflow pending at that checkpoint); KL #27 expanded (client-side TLS). THREAT_MODEL §G rewritten
  with run data + artifact-tampering row. README index complete + license
  files linked.
- NEWGIT_LOG=1 is the obs env var (NOT NEWGIT_DEBUG).

## Iteration 12 outcome (facts for resume)

- Audit scope executed: it10/11 seams (new server endpoints incl. route/policy
  table re-read, UI hash-router + api() param flow, MCP build_argv + JSON-RPC
  handling, listings/audit bounds) + README quickstart verbatim run + gate
  re-runs. Older subsystems (store/txn/refs/ops/diff/merge/workflow/verify/gc/
  gitio) were audited within their own iterations (it1–9 outcomes above) and
  are covered by 278 tests incl. chaos/fuzz/fault-injection re-run in BOTH
  profiles this iteration.
- Fixes landed (see CHANGELOG it12): check_wire_spec in src/remote/server.rs;
  ListData.truncated (serde default — additive); build_argv `--` layout rule
  (action words stay raw-first: family dispatch uses split_first, NOT Args).
- Final binary sha256 30714184e3e4c9a2d4d28821b7cc1995171c259b07fc9ac7c98c5c7b822a4858
  (bit-identical dual-target); dist bbafe61150867e5209576e8e4dd0e6dd9d34da2dae41fc230f6367b20ac26bba.
- Classification at implementation completion: PRODUCTION-CANDIDATE; hosted evidence was added during publication and is recorded in RELEASE_READINESS.md.
- 11 commits on main so far (one per iteration); this final commit makes 12.

## Publication record (2026-10-04)

The preserved NewGit history is published at
https://github.com/kakarot700/newgit. The hosted preflight CI and CodeQL runs
listed above passed on the runner-pinned source-bearing commit. The independent
clean-clone checks exercised the built CLI, the documented install path, the
human-approved agent workflow, and real Git import/export.

Repository settings enable issues, read-only-by-default Actions tokens,
secret scanning with push protection, Dependabot alerts/security updates, and
private vulnerability reporting. The CI release job is tag-only and depends
on the checks, security, and distribution jobs; the versioned notes are in
docs/releases/v0.1.0.md. Consult the public Releases and Actions pages for the
current tag and artifact status.

Future product work should start from the post-1.0 candidates in ROADMAP.md
(client-side TLS/protocol v2, UI mutations, packed objects, and horizontal
scale) and follow docs/CONTRIBUTING.md.

## Important decisions (full log in DECISIONS.md)

- Rust, minimal dependencies, no frameworks; hand-rolled CLI parser and HTTP.
- SHA-256 ids; canonical encodings are a frozen protocol (docs/STORAGE_FORMAT.md).
- Objects immutable; set-like fields strictly ascending for unique canonical form.
- Evidence has `deterministic` flag; AI opinions must be flagged (`ai_generated`).
- Git interop via system git fast-export/fast-import (no gitoxide dependency).
- No staging area: workspaces hold live state; `snapshot` captures whole workspace.

## Commands

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --release --locked
```
