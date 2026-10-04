# TEST_MATRIX.md — what is tested, where, and how to run it

Rule: a claim of "tested" requires an exact command + observed result.
Latest full run recorded at the bottom (updated each iteration).

## Commands

```bash
# Run these commands from the repository root; rustup reads rust-toolchain.toml.
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked                 # unit (src/**) + integration (tests/**)
cargo test --release --locked       # same suites, optimized (chaos uses this)
```

## Layer map

| Layer | Location | Covers | Status |
|---|---|---|---|
| Unit | `src/**/mod tests` | hex, varint (incl. non-minimal/truncation), fsx (atomic write, locks, stale reclaim, path traversal, symlink escape), ObjectId determinism, all 9 type codecs roundtrip + garbage rejection, envelope (bit-flip, truncation sweep, type mismatch, decompression bomb), ostore (roundtrip, idempotent put, corruption, misfiling, truncation, iter/prefix, limits, temp sweep), config parse/reject | ✅ iter 1 |
| Property | `tests/property_core.rs` | hex/varint/base64 roundtrips; blob/tree/snapshot/actor canonical roundtrips; total decoder (no panics on arbitrary bytes); envelope single-bit-flip detection; canonical uniqueness under re-sort; **diff reconstructs + deterministic** | ✅ iter 2–4 (11 suites × 256 cases) |
| Integration | `tests/txn_recovery.rs`, `tests/ops_snapshot.rs`, `tests/version.rs`, `tests/verify_gc.rs` | ref CRUD/CAS/reflog, txn atomicity, quarantine, snapshot/status/history/workspace cycles, checkout safety, VERSION sync, **verify corruption classes + read-only invariant, gc reachability/grace/forensics/debris** | ✅ iter 2–7 |
| E2E (CLI) | `tests/cli_e2e.rs` | full workflow, JSON envelopes, exit codes, discovery, determinism, debug logging, merge/rollback flows, **two-agent goal workflow**, **verify/gc/recover contract (exit 3, forensics)**, **import-git/export-git CLI contracts + error exits**, **serve/token/remote/push/pull/audit contracts + exit codes 2/3/5/7** | ✅ iter 3–9 (14 suites) |
| Concurrency | `tests/concurrency_refs.rs` | CAS races (one winner/version, reflog count equality), parallel multi-ref txns, concurrent object writes, concurrent open/recover vs writers | ✅ iter 2 (workspace races: iter 3) |
| Crash/failure injection | `tests/txn_recovery.rs` + `newgit-faultlab` | child-process aborts at 5 txn fault points; forward-recovery, partial-apply completion, reflog dedup, quarantine | ✅ iter 2 (ostore crash points: iter 3) |
| Chaos | `tests/chaos.rs` | 6 fixed xorshift64* seeds × 14–25 random ops (snapshot/ws-create/integrate/put-blob/txn-set) killed at random fault points; after EVERY step: auto-recovery, deep verify zero errors, refs resolve, status computes; end-of-seed gc + history walk | ✅ iter 7 (6 suites) |
| Fuzz-like | `tests/fuzz_parsers.rs` | 150k seeded prefix-anchored garbage inputs vs envelope/canonical/index/journal/config/hex/base64/ref-grammar/fast-export + **HTTP request framing, percent-decoding and all remote wire structs** (no panics, no OOM) | ✅ iter 7–9 (8 suites) |
| E2E (UI/MCP) | lib (`ui::tests`, `cli::mcp::tests`) + `tests/remote_e2e.rs` + 2 in `tests/cli_e2e.rs` | UI self-containment & XSS discipline (asserted against shipped bytes); UI served only with `--ui`, data-free shell, no-auth static vs role-gated data; `/v1/object` shapes (snapshot/tree/blob-b64/goal/change/evidence/proposal) + not-found/malformed; `/v1/diff` vs CLI rendering + spec forms + content cap; goals/changes(?goal)/proposals listings; MCP handshake/catalog(13)/ping, JSON-RPC error codes (-32700/-32600/-32601/-32602), argv mapping table, isError envelope, real child processes (`newgit ui` announcement + HTML over TCP; `newgit mcp` full session incl. snapshot-through-MCP and clean EOF exit) | ✅ iter 10 (4 lib + 2 remote + 2 cli suites) |
| E2E (remote) | `tests/remote_e2e.rs` + 2 in `tests/cli_e2e.rs` | REAL in-process server + REAL TCP client (no mocks): info/healthz anonymous, refs gating, role matrix (read/write/admin ⇒ 401/403 boundaries), bad token never downgrades, push→pull oid + object-universe equality, incremental push (0 objects on re-push), non-fast-forward refusal + wire CAS (one winner), dependency-order + corrupt-envelope rejection on put, batch/body limits, internal namespaces never cross, audit content + ordering, concurrent pushes to different refs, crash-mid-push leaves server clean and retry reuses orphans, negotiate superset + post-order, protocol-version and URL validation. CLI: `serve` port-0 announcement line, `token add/list` (no leaks), `remote add/list/remove`, `push`/`pull`/`audit` `--json` envelopes, exit codes 2/3/5/7, bind-conflict and not-a-repo errors | ✅ iter 9 (14 + 2 suites) |
| E2E (Git smart HTTP) | `tests/git_remote_e2e.rs` plus `remote::git_http::tests` | Ordinary Git 2.43.0 CLI over loopback against the live NewGit server: protocol v2 clone/ls-remote, v0 fetch, v1 pull; validates branch/tag refs, commit/tree, text and binary blob bytes; verifies authenticated single- and multi-branch pushes, empty-repo creation, post-push clone/fetch, ordinary single-branch deletion, atomic multi-branch deletion, post-delete fetch --prune and fresh clone, and a mixed projection result where Git accepts a deletion but the adapter returns HTTP 409 with unchanged NewGit refs/object inventory. Atomic delete plus forced non-fast-forward rejection also leaves refs/objects unchanged. Read-role denial, malformed packets, tag/non-fast-forward refusal, a deliberately tiny advertisement-response cap, hostile Git env/template-hook isolation, and unsupported protocol-header checks remain covered. | ✅ targeted suites: 3/3 |
| Process deadline | `src/util/process.rs`, `src/remote/http.rs` | A process-group timeout test proves the direct process and descendant are terminated/reaped; HTTP regression maps timed-out work to 504. Git smart-HTTP projection and upload-pack share a 120-second absolute deadline. | ✅ focused unit regressions |
| Compatibility | `tests/git_compat.rs` | REAL system-Git repos (branches, ordinary and four-parent octopus merges, annotated+light tags, binary, symlink, exec bit, Unicode, C-quoted UTF-8 names with quotes/backslashes, renames, empty commits and empty trees, distinct author/committer, remotes+notes refs, non-HEAD symbolic-ref omission, replace refs, Git-verified SSH-signed commit and tag, remote/notes blob refs omitted by fast-export, raw invalid-UTF-8 commit-message conversion, and a SHA-256 repository): import equality vs `ls-tree`/`cat-file`/`log`, import determinism, export round-trip (byte-identical blob SHAs + identity multiset + clean worktree), exact tested UTF-8 commit-message payloads via raw commit objects, U+0001 message refusal with zero refs moved + deep verify, symbolic-ref skip report, replace-ref skip with stored branch message/tree preserved despite replacement overlay, atomic non-commit-ref refusal for real lightweight/annotated blob/tree tags including orphan and tag-only sources, signed-commit signature-loss report by source SHA (fast-export and exported commit are unsigned; human/JSON CLI checked), SSH signed-tag source verification + stripped report + lightweight export/tree comparison, SHA-256 source-ID metadata plus semantic import/export fidelity, submodule refusal atomicity, empty repo, ref-move atomicity, export refusals, reimport stability; octopus test checks ordered parents, semantic merge tree, Git fsck, and deep NewGit integrity through Git→NewGit→Git→NewGit; empty-tree fixture compares tree ids, parent mapping, and path/mode/blob fidelity across both conversion legs | ✅ latest full debug/release validation: 312/312 tests; all 28 Git interoperability tests passed |
| Final audit (it12) | code review of it10/11 seams + README quickstart verbatim run | Findings→fixes→regressions: (1) /v1/diff internal-spec probe (`ws:`/`workspaces/*` resolvable remotely) → FIXED `check_wire_spec` 400-invalid + regression inside `internal_namespaces_never_cross_the_wire` (8 probe calls, both sides; honest ref+oid specs still work); (2) unbounded /v1/goals·changes·proposals responses → FIXED cap at `limits.max_batch_objects` + `truncated` flag (asserted false on small listings); (3) MCP positional values shaped like flags → FIXED `--` separator before every bare positional + dash-value cases (`--raw` oid, `--force` title/spec) in `argv_building_matches_cli_syntax`; (4) UI hash-route params traced: tampered `?ref=` is inert (matched against fetched refs only), goal-filter oids are hex-gated server-side; (5) README quickstart transcript executed VERBATIM end-to-end (goal→workspace→snapshot --goal→change→evidence record→tested gate→proposal→approve→integrate→achieved→verify 0 errors→history --goal) ✅ |
| Release/supply-chain | `scripts/dist.sh`, `scripts/sbom.py`, cargo-audit, dual-target rebuild | dist packaging + `sha256sum -c` over every file (RUN: 27/27 OK); SBOM determinism (`sbom.py \| diff SBOM.md -` RUN: clean); cargo-audit live DB (RUN 2026-10-04: 1290 advisories × 63 crates → 0 findings); reproducibility: two clean release builds, different target dirs (RUN: bit-identical, sha256 abcd51c8…); build.rs review of all 8 runtime crates w/ scripts (RUN: rustc probes only, no network) | ✅ iter 11 (commands recorded, not test-fns — rerun via CHANGELOG it11) |
| Performance | `src/bin/newgit-bench.rs` + docs/BENCHMARKS.md | put_blob / snapshot 1k+5k cold+warm / status cached+uncached / diff / history / integrate / verify / gc — real recorded numbers + regression policy | ✅ iter 7 (release re-check: iter 11) |

## Invariant test registry (each must have ≥1 permanent test)

| # | Invariant | Test(s) |
|---|---|---|
| I1 | identical content ⇒ identical id | `blob_identity_deterministic`, `deterministic_encoding` |
| I2 | objects immutable; put never overwrites differing content | `misfiled_object_detected`, `put_get_roundtrip` |
| I3 | any single-bit corruption is detected, never silently read | `corruption_detected`, `detects_bit_flip`, `truncated_file_detected` |
| I4 | decoders never panic on arbitrary bytes | `truncated_input_never_panics`, `truncated_envelopes_never_panic`, `decode_rejects_garbage`, `tests/fuzz_parsers.rs` (6 suites, 110k inputs) ✅ |
| I5 | decompression bombs bounded | `bomb_protection` |
| I6 | path safety: traversal/NUL/absolute/symlink escape rejected | `path_checks`, `symlink_escape_is_rejected` |
| I7 | locks exclusive; stale locks reclaimed only after timeout | `lock_is_exclusive`, `stale_lock_is_reclaimed` |
| I8 | atomic writes leave no partial files/debris | `atomic_write_is_durable_and_replaces` |
| I9 | config: unknown keys/versions rejected loudly | `config_rejects_unknown_and_bad` |
| I10 | set-fields strictly ascending ⇒ unique canonical form | `tree_roundtrip_and_ordering`, `decode_rejects_garbage` |
| I11 | reachable objects never deleted by GC | `gc_removes_unreachable_keeps_reachable`, `gc_keeps_reflog_referenced_objects`, `gc_preserves_chain_history`, `gc_never_touches_quarantine`, `gc_keeps_unreadable_objects_and_reports_missing_links`, chaos end-of-seed gc+history walk ✅ |
| I12 | transactions all-or-nothing across process death | `crash_before_journal_leaves_no_trace`, `crash_after_journal_commits_on_recovery`, `crash_mid_apply_finishes_on_recovery`, `crash_before_complete_marker_recovers_and_dedupes_reflog`, `repeated_recovery_is_stable` ✅ |
| I13 | integration atomic; failure leaves no partial state | `integrate_conflict_writes_nothing`, `integrate_three_way_merge_atomic_and_checked_out`, `integrate_concurrent_same_target_serializes` ✅ |
| I14 | verify detects every injected corruption class | `corrupt_object_detected`, `truncated_object_detected`, `misfiled_object_detected`, `bad_layout_detected`, `reflog_malformed_detected`, `ref_target_missing_detected`, `chain_tampering_detected`, `verify_never_modifies_the_repository` ✅ |
| I15 | evidence honesty flags survive roundtrip; claims ≠ facts | `change_lifecycle_honesty_gates`, evidence/proposal tests in `tests/ops_snapshot.rs` ✅ |
| I16 | git import/export preserves tested trees, history, modes, and valid UTF-8 commit-message payloads; unsupported message controls are refused before ref movement | `import_matches_git_content_exactly`, `export_roundtrip_matches_git`, `commit_message_roundtrip_preserves_exact_utf8_bytes`, `git_control_character_commit_message_is_refused_atomically` ✅ |
| I17 | git import is deterministic (same repo ⇒ same oids) | `import_is_deterministic` ✅ |
| I18 | unsupported git content (submodules) fails loudly and atomically — zero refs move | `submodule_import_is_refused_atomically` ✅ |
| I19 | export→reimport is a fixpoint on trees/messages | `reimport_after_export_is_stable` ✅ |
| I20 | remote store is link-closed: "server has X" ⇒ "server has closure(X)"; out-of-order/corrupt objects refused | `dependency_order_enforced_on_put`, `crash_mid_push_leaves_server_clean_and_retry_succeeds` ✅ |
| I21 | refs never move without authority: authn+authz before mutation, CAS over the wire, non-fast-forward refused | `roles_enforced_reader_writer_admin`, `push_cas_race_one_winner_clean_loser`, `info_anonymous_and_refs_gated`, cli_e2e exit-7 contracts ✅ |
| I22 | a remote can never inject an unverifiable or dangling object into a local repo | `push_pull_roundtrip_oid_equality` (object-universe equality + deep verify), pull-side envelope/id/dependency re-validation ✅ |
| I23 | no secret is ever emitted: token list, remote list, audit log, error messages | cli_e2e `remote_cli_push_pull_serve_token_audit`, `audit_log_records_who_what_result` ✅ |
| I24 | interrupted remote operations leave both repos verifiable and unchanged in visible state | `crash_mid_push_leaves_server_clean_and_retry_succeeds`, `push_cas_race_one_winner_clean_loser` ✅ |
| I25 | C-quoted UTF-8 Git paths preserve path names, modes, and blob ids across import/export | `quoted_utf8_git_paths_roundtrip_without_changing_names` ✅ |
| I26 | empty Git trees and their transitions survive import/export/reimport without changing mapped parents or populated-tree contents | `empty_git_trees_roundtrip_across_root_and_followup_commits` ✅ |
| I27 | Git import never silently turns an unsupported non-`HEAD` symbolic ref into an omitted ordinary ref | `non_head_symbolic_refs_are_reported_and_not_imported` ✅ |
| I28 | Git replace refs that NewGit cannot represent never silently rewrite imported ordinary branch history | `replace_refs_do_not_rewrite_imported_branch_history` ✅ |
| I29 | Git commit signature headers omitted by fast-export are reported by source object ID and are never described as preserved or verified | `signed_git_commit_signature_loss_is_reported` (Git-verified SSH signature, raw object vs stream/export, JSON and human CLI reports) ✅ |
| I30 | SHA-256 Git source object IDs import without width errors and semantic export retains refs/history/tree content | `sha256_git_import_export_roundtrips_semantically` plus SHA-256 parser/gitlink unit coverage ✅ |
| I31 | Git refs targeting non-commit objects that NewGit cannot export are refused before any refs move rather than silently omitted or imported into an asymmetric state | `non_commit_git_refs_are_refused_atomically` (real lightweight/annotated tags to reachable/orphan blobs and trees, plus a tag-only source; deep verify) ✅ |
| I32 | Git octopus merges retain every ordered parent and the merge tree through import, export, and reimport | `octopus_merge_preserves_parent_order_and_trees_across_roundtrip` (four-parent real-Git merge, semantic tree comparison, `fsck`, deep NewGit verification) ✅ |
| I33 | SSH-signed annotated tags are never claimed preserved: import reports tag-metadata loss, export uses a lightweight tag, and the tested target tree remains correct | `ssh_signed_annotated_tag_loss_is_reported_and_exported_as_lightweight` (Git-verified source signature, fast-export observation, Git `fsck`, deep NewGit verification) ✅ |
| I34 | Git namespace refs are reported and omitted rather than exposed as unrelated ordinary branches | `git_namespace_refs_are_reported_and_not_exported_as_branches` (Git 2.43 namespaced ls-remote sees virtual branch/tag/blob refs; fast-export omits the blob ref; scan reports all three; namespace-only commit objects are still processed; import/export skip and report refs; no flattened branch; Git `fsck`, deep verification) ✅ |
| I35 | Git refs in unsupported families omitted by `fast-export` are still reported and never imported | `omitted_unsupported_blob_refs_are_reported_from_the_ref_scan` (real remote-tracking and notes refs to blobs omitted by Git 2.43 fast-export; both reported, neither written; ordinary branch retained; deep verification) ✅ |
| I36 | Git commit messages that cannot be represented as UTF-8 are never silently presented as byte-preserved: import records the documented lossy text and marks its snapshot | `non_utf8_git_commit_message_is_lossily_converted_and_flagged` (raw Git commit object and fast-export preserve invalid bytes; import replacement text + `git_message_lossy=1`; exported message is the converted text; valid ancestor unmarked; deep verify) ✅ |
| I37 | Git smart-HTTP read and bounded branch-write behavior is established with real Git CLI tests; refused pushes do not mutate NewGit | `real_git_clone_fetch_pull_push_and_ls_remote_over_smart_http` and `real_git_initial_push_to_empty_newgit_repo_creates_main` (v2 clone/ls-remote, v0 fetch, v1 pull, authenticated create/fast-forward, empty-repository push, post-push clone/fetch, auth and refusal paths) ✅ |
| I38 | Ambient Git repository variables cannot redirect the temporary projection, and host Git template hooks cannot execute during projection generation | `ambient_git_environment_cannot_redirect_or_run_template_hooks` (hostile `GIT_DIR`/`GIT_WORK_TREE`/`GIT_TEMPLATE_DIR`; detached checkout; real `ls-remote` succeeds, sentinel bare refs stay unchanged, malicious hook marker absent) ✅ |
| I39 | Git operation deadline terminates descendant processes and reaps the direct child | `deadline_kills_and_reaps_child_process_group` ✅ |
| I40 | Git branch writes (create, update, delete) commit accepted canonical refs together; partial projection acceptance or a competing CAS winner cannot publish a partial ref set or loser objects | `real_git_clone_fetch_pull_push_and_ls_remote_over_smart_http`, `racing_multi_ref_pushes_publish_only_one_complete_ref_set`, `racing_ref_deletion_and_update_have_one_cas_winner` ✅ |
| I41 | A deletion accepted only in the disposable projection is not published when another requested ref is rejected; atomic and ordinary requests leave canonical refs/object inventory intact | `real_git_clone_fetch_pull_push_and_ls_remote_over_smart_http` (HTTP 409 partial result and atomic policy failure) ✅ |

## Latest recorded run

- Date: 2026-10-04 (authenticated smart-HTTP branch deletion; Git 2.43.0/Linux,
  Rust 1.99.0)
- `cargo test --locked` (debug): **313 passed; 0 failed** (137 lib unit,
  6 chaos, 16 cli_e2e, 4 concurrency_refs, 8 diff_engine, 8 fuzz_parsers,
  28 git_compat, 3 git_remote_e2e, 18 merge_integrate, 13 ops_snapshot,
  12 property_core, 16 remote_e2e, 14 txn_recovery, 20 verify_gc, 1 version,
  9 workflow)
- `cargo test --release --locked`: **313 passed; 0 failed** (same suites)
- Both full runs include **28/28** Git import/export tests, **3/3** live Git
  smart-HTTP tests, and **14/14** transaction/recovery tests. Real Git CLI
  coverage includes single and atomic multi-branch deletion, post-delete
  fetch-prune/fresh-clone checks, and ordinary/atomic rejection of a deletion
  paired with a forced non-fast-forward update. The transaction suite proves a
  same-ref delete-versus-update race has one CAS winner.
- `cargo fmt --all -- --check`, warnings-denied `cargo clippy --all-targets
  --locked`, `cargo build --release --locked`, SBOM drift, Git 2.43.0 /
  `ssh-keygen` prerequisites, and `git diff --check`: clean.
- The Ubuntu 24.04 CI job requires Git >=2.34.0 and `ssh-keygen` before tests,
  so its SSH-signature fixtures cannot silently skip on missing tools.
- Exact-SHA GitHub CI and CodeQL links are delivered with task completion after
  the single push, avoiding a follow-up state-only commit.
- Empty-tree compatibility commit
  `ba5eaed79cf778bf77d66fbea0bb6c0d2b46c6cb` passed [hosted CI run
  37203669979](https://github.com/kakarot700/newgit/actions/runs/37203669979)
  and [CodeQL run
  37203669978](https://github.com/kakarot700/newgit/actions/runs/37203669978),
  both on that exact SHA.
- Previous hosted checks passed on implementation SHA
  `52fa27a6a8d5cba4fbbdc87cc74acf31f06b84f2`: [CI run 37201393466](https://github.com/kakarot700/newgit/actions/runs/37201393466),
  [CodeQL run 37201393416](https://github.com/kakarot700/newgit/actions/runs/37201393416).
- The C-quoted UTF-8 pathname implementation SHA `b4e1ca5dd12b2d816fbd05f03416dc903a4a014a`
  passed [hosted CI run 37202630994](https://github.com/kakarot700/newgit/actions/runs/37202630994)
  and [CodeQL run 37202630998](https://github.com/kakarot700/newgit/actions/runs/37202630998)
  on the exact SHA.
