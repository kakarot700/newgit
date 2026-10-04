# THREAT_MODEL.md — NewGit (STRIDE-flavored, per attack surface)

Each threat: vector → impact → mitigation → test that proves it.
"TEST: pending(itN)" means the regression test lands in iteration N.

## A. Malicious/corrupt repository content (objects)

| Threat | Mitigation | Test |
|---|---|---|
| Bit-rot / silent corruption | SHA-256 digest in envelope + id-vs-path check on every read | `corruption_detected`, `misfiled_object_detected` ✅ |
| Truncated object file | length checks; inflate-to-exact-length | `truncated_file_detected` ✅ |
| Decompression bomb (tiny file → huge memory) | `max_raw` bound pre- and mid-inflate | `bomb_protection` ✅ |
| Parser panics on malformed objects | total decoders; prefix-vs-remaining checks | `decode_rejects_garbage`, truncation sweeps ✅ + fuzz suite (it7) |
| Cross-type confusion (blob decoded as tree) | type tag in envelope + canonical header, cross-checked | `type_mismatch_detected` ✅ |
| Non-canonical encodings (id aliasing) | minimal varints, sorted sets, no trailing bytes rejected | ✅ roundtrip/garbage tests + property tests (it2) |
| Malicious git repo via `import-git`: path traversal in fast-export paths (`../`, absolute, NUL) | every path through `fsx::check_rel_path` grammar before any store write | `git_compat` + path-grammar fuzz ✅ |
| Malicious fast-export stream: bogus lengths/marks/framing | total hand-rolled parser, 2 GiB `data` cap, bounded marks, malformed ⇒ clean Err; parser never trusts git's byte counts beyond caps | 7 parser unit tests + `fuzz_fastexport_parser_never_panics` ✅ |
| Ref-name injection from git refs (`refs/remotes/..`, control chars) | skip-list for foreign namespaces + NewGit ref grammar re-check on every imported name | `git_compat` (notes/remotes skipped) ✅ |
| Smuggled gitlinks/submodules | mode `160000` and raw-sha `M` lines refused loudly; import aborts atomically (zero refs move) | `submodule_import_is_refused_atomically` ✅ |
| Runaway child git process | child status checked after stream; import runs read-only git commands only (`rev-parse`, `fast-export`) — no network, no config execution beyond git's own repo load | manual review + cli_e2e error contracts ✅ |

## B. Filesystem / workspace attacks

| Threat | Mitigation | Test |
|---|---|---|
| Path traversal (`../../etc/passwd`) in names/refs/checkout | path grammar enforced at every join | `path_checks` ✅ |
| Symlink escape during walk/checkout | never follow symlinks (stored as symlink blobs); parent canonicalization containment check | `symlink_escape_is_rejected` ✅; walk tests (it3) |
| NUL bytes / control chars / unicode tricks in names | byte-level validation; UTF-8 requirement; homoglyphs are *display* risk only (documented) | ✅ name tests; chaos suite (it7) |
| Absolute paths / drive letters (Windows export) | rejected in grammar | ✅ |
| TOCTOU on file reads (file swapped mid-read) | content hashed from the bytes actually read; index is a cache; snapshot atomicity via tmp+rename | crash tests (it2/3) |
| Resource exhaustion via huge files/many files | configurable limits; enforced before allocation | `limits_enforced` ✅; walk limits (it3) |
| Stale lock DoS | bounded wait + stale reclaim with logging | `stale_lock_is_reclaimed` ✅ |

## C. Concurrency / crash

| Threat | Mitigation | Test |
|---|---|---|
| Process killed mid-write (object) | tmp+fsync+rename; debris swept by recovery; verify classifies temp debris as warning | `ostore:*` fault tests (it2/3), `temp_debris_is_warning_and_survives_gc`, chaos ✅ |
| Kill mid-transaction (multi-ref) | WAL journal + idempotent redo on open; checkpoint-delete after COMPLETE (D-015) | `txn_recovery.rs` (10 suites) ✅ |
| Racing ref updates | global txn lock + CAS with expected-old | `concurrency_refs.rs` ✅ |
| Double integration / duplicate ops | CAS on proposal state + snapshot parents; idempotent object puts; reflog txn-id dedup | `merge_integrate.rs` race suites ✅ |
| GC vs concurrent writer | txn lock held for whole gc; 24 h mtime grace window protects in-flight object writes; only fully-decodable unreachable objects deleted | `gc_grace_window_keeps_young_objects`, chaos per-step invariants ✅ |
| GC/verify against damaged repo | gc keeps corrupt/misfiled/quarantine files (forensics), reports missing_links; verify never mutates | `gc_keeps_unreadable_objects...`, `verify_never_modifies_the_repository` ✅ |
| Local DoS via long gc | gc duration linear in object count, lock bounded by lock_wait_ms for clients; documented in KNOWN_LIMITATIONS #16 | bench: gc 1000 orphans ≈ 19 ms ✅ |

## D. CLI / local interface

| Threat | Mitigation | Test |
|---|---|---|
| Command/shell injection | no shell anywhere; fixed argv for git subprocess | git_compat tests (it8) |
| Ambiguous id prefixes | ≥4 hex, unique-match requirement, loud ambiguity error | `iter_and_prefix` ✅ |
| Arg parsing bombs (huge values) | limits at parse; no unbounded allocation | (it3) pending |
| Secrets in output/logs | never print token material; audit excludes secrets | cli_e2e token/remote-list no-leak assertions ✅ |

## E. Remote protocol (it9 — REALIZED)

| Threat | Mitigation | Test |
|---|---|---|
| Unauthenticated mutation | bearer tokens (SHA-256 at rest), roles read<write<admin, authz checked before every mutation; invalid token never downgrades to anonymous | `roles_enforced_reader_writer_admin`, `info_anonymous_and_refs_gated`, cli_e2e exit-7 contracts ✅ |
| Malformed HTTP/JSON | total parser: request-line/header/body caps checked pre-allocation, chunked refused, wire structs strictly typed | `fuzz_http_and_wire_json_never_panic` + http unit tests ✅ |
| Object smuggling (bad id/content) | envelope digest+id re-verified on receipt (both directions); **link-closure invariant**: put refused unless all dependencies stored ⇒ no dangling objects can enter | `dependency_order_enforced_on_put`, `push_pull_roundtrip_oid_equality` ✅ |
| Ref force-push / history rewrite | client non-fast-forward guard (exit 5) + wire CAS `exactly(old)` in one transaction; `--force` is explicit, writer-role only, audit-logged; overwritten objects never destroyed | `push_cas_race_one_winner_clean_loser` ✅ |
| DoS (huge bodies, slowloris) | Content-Length cap pre-read (413), batch caps, 30 s IO timeouts, bounded thread pool (429), client-side timeouts | `limits_enforced_batch_and_body` ✅ |
| Token/secret leakage | raw tokens printed once at creation; never listed, logged, or put in error messages; audit records ids only; token file 0600 | cli_e2e token/remote list assertions, `audit_log_records_who_what_result` ✅ |
| Internal-state exfiltration/injection via refs | `workspaces/*` + `chains/*` invisible in listings (user ref grammar) AND explicitly refused by remote refs/update | `internal_namespaces_never_cross_the_wire` ✅ |
| Interrupted transfer corruption | objects idempotent + refs atomic (one txn) on BOTH ends; crash between phases leaves orphans (gc fodder), zero visible change | `crash_mid_push_leaves_server_clean_and_retry_succeeds` ✅ |
| SSRF (remote URLs) | client issues exactly ONE request per call to the operator-provided host:port; the HTTP client never follows redirects (no Location handling in src/remote/client.rs) | by construction ✅ |
| Confused deputy (server acts with client privileges) | per-request auth context; no ambient credentials; audit ties actions to token role | `roles_enforced_reader_writer_admin` ✅ |
| Replay | CAS semantics make replays no-ops or CAS failures (`exactly(old)` after a move always fails) | `push_cas_race_one_winner_clean_loser` ✅ |

### E2. Web UI + MCP surface (it10 — REALIZED)

| Threat | Mitigation | Test |
|---|---|---|
| XSS via hostile repo content (commit messages, filenames, evidence output) | UI renders ALL dynamic content with `textContent`/`createTextNode` (`el()` helper); `innerHTML`/`document.write`/`eval` never appear in the file; served HTML contains zero repository data | `ui_is_self_contained_and_xss_disciplined`, `ui_is_served_only_when_enabled_and_carries_no_data` ✅ |
| Token theft from browser | bearer token in `sessionStorage` only (cleared on tab close; never localStorage/cookies); no external resources can read it (nothing external is loaded) | static assertions above ✅ |
| CSRF | auth is an `Authorization` header set by JS, never a cookie ⇒ browsers cannot ambient-authorize requests; UI performs no mutations at all | by construction ✅ |
| Data-free shell exposure | `/` requires no auth but serves ONLY the static HTML; every data endpoint stays role-gated (`/v1/object` etc. refuse anonymous when anon-read is off) | `ui_is_served_only_when_enabled_and_carries_no_data` ✅ |
| MCP command execution | `evidence record` executes ONLY an explicit caller-provided argv (same as CLI); never repository content; server has exactly the privileges of its spawning user and listens on stdio only (no port) | `mcp_speaks_jsonrpc_over_stdio`, SECURITY_MODEL §3 ✅ |
| MCP protocol abuse | total JSON-RPC handling: parse errors -32700, batches refused, unknown tools/methods are errors not crashes; tool failures never kill the server | `protocol_errors_are_proper_jsonrpc`, `mcp_speaks_jsonrpc_over_stdio` (malformed line, then ping still answers) ✅ |

## F. Agent/AI-specific

| Threat | Mitigation | Test |
|---|---|---|
| Fabricated "tests passed" evidence | `deterministic` flag; `evidence record` captures REAL command/exit/output; change `tested` gate requires attached evidence; proposal gate requires tested/proposed | `change_lifecycle_honesty_gates`, `evidence_record_limits_and_signals`, `object_diff_and_workflow_endpoints` (proposal refused pre-`tested`) ✅ |
| AI opinion presented as fact | `ai_generated` on evaluations is permanent; aggregation labels opinions as opinions; Web UI renders evaluations with an "AI OPINION" badge + explicit "never a fact" notice (src/ui/index.html) | `evaluation_targets_and_ai_flag` ✅ (UI badge: static asset, asserted data-free) |
| Malicious agent identity spoofing (display name "human:alice") | identity ≠ authority; remote authz decides; pubkey signatures roadmap | documented; sigs (roadmap) |
| Agent resource abuse (fork bombs of workspaces/objects) | configurable limits on workspace count/objects per txn; GC | (it7) pending |
| Prompt-injected agent instructed to exfiltrate | out of scope for VCS core; NewGit gives no host access beyond repo dir; documented in AGENT_GUIDE | documented |

## G. Supply chain

| Threat | Mitigation |
|---|---|
| Dependency CVEs / typosquats | 5 direct runtime deps (21-crate runtime closure), all mainstream; Cargo.lock committed; **cargo-audit RUN in-sandbox 2026-10-04: 1290 RustSec advisories vs 63 locked crates → zero findings**; `cargo audit --deny warnings` + cargo-deny 0.20.2 BOTH RUN in-sandbox (deny: advisories/bans/licenses/sources all ok, zero warnings; deny.toml: license allow-list = exact SBOM set, crates.io-only sources, framework/git-crate ban list) and gate CI; SBOM.md committed with a CI drift check |
| Malicious build scripts | all 8 runtime-closure crates with build.rs AUDITED by reading the vendored sources (2026-10-04): crc32fast, generic-array, libc, serde, serde_core, serde_json, thiserror, zmij — every process invocation is `rustc --version`/feature probing (libc additionally probes freebsd-version/emcc on non-Linux targets); NO network access in any build script |
| Toolchain drift | rust-toolchain.toml pins exact version (1.99.0); release builds verified bit-identical across clean target dirs on same host+toolchain (sha256 abcd51c8…, 2026-10-04) |
| Artifact tampering | dist tarball + SHA256SUMS.txt over every packaged file; CI verifies checksums post-package and attaches artifacts to tag releases |
