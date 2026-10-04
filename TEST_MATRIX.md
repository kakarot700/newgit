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
| Compatibility | `tests/git_compat.rs` | REAL system-Git repos (branches, merges, annotated+light tags, binary, symlink, exec bit, Unicode, renames, empty commits, distinct author/committer, remotes+notes refs): import equality vs `ls-tree`/`cat-file`/`log`, import determinism, export round-trip (byte-identical blob SHAs + identity multiset + clean worktree), exact tested UTF-8 commit-message payloads via raw commit objects, U+0001 message refusal with zero refs moved + deep verify, submodule refusal atomicity, empty repo, ref-move atomicity, export refusals, reimport stability | ✅ iter 8 + compatibility continuation (16 tests) |
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

## Latest recorded run

- Date: 2026-10-04 (Git compatibility continuation; Git 2.43.0)
- `cargo test --locked` (debug): **285 passed; 0 failed** (128 lib unit,
  6 chaos, 16 cli_e2e, 4 concurrency_refs, 8 diff_engine, 8 fuzz_parsers,
  16 git_compat, 18 merge_integrate, 13 ops_snapshot, 12 property_core,
  16 remote_e2e, 10 txn_recovery, 20 verify_gc, 1 version, 9 workflow)
- `cargo test --release --locked`: **285 passed; 0 failed** (same suites)
- Focused `cargo test --locked --test git_compat`: **16 passed; 0 failed**
- `cargo fmt --check`: clean
- `cargo clippy --all-targets --locked -- -D warnings`: clean
- `cargo build --release --locked`: clean
- `python3 scripts/sbom.py | diff -u SBOM.md -`: clean (no dependency drift)
- `git diff --check`: clean
- Hosted CI and CodeQL are run after push; record their exact commit/run links in
  `PROJECT_STATE.md` when results are available.
