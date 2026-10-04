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
| Secrets in output/logs | never print token material; audit excludes secrets | (it9) pending |

## E. Remote protocol (it9)

| Threat | Mitigation | Test |
|---|---|---|
| Unauthenticated mutation | bearer tokens (hashed at rest), roles, authz before mutation | pending(it9) |
| Malformed HTTP/JSON | strict parser with size/count limits; total decoder | fuzz suite pending(it9) |
| Object smuggling (bad id/content) | every received object digest-verified before store; type/limit checks | pending(it9) |
| Ref force-push / history rewrite | CAS with expected-old; admin-only force flag; audit-logged | pending(it9) |
| DoS (huge bodies, slowloris) | request size caps, read timeouts, bounded threads | pending(it9) |
| SSRF (remote URLs) | client connects only to operator-provided host:port; no redirects followed | pending(it9) |
| Confused deputy (server acts with client privileges) | per-request auth context; no ambient credentials; audit ties actions to token role | pending(it9) |
| Replay | CAS semantics make replays no-ops or CAS failures | pending(it9) |

## F. Agent/AI-specific

| Threat | Mitigation | Test |
|---|---|---|
| Fabricated "tests passed" evidence | `deterministic` flag; runner-produced evidence vs claims; policy can require deterministic | (it6) pending |
| AI opinion presented as fact | `ai_generated` on evaluations; UI renders distinctly | (it6/10) pending |
| Malicious agent identity spoofing (display name "human:alice") | identity ≠ authority; remote authz decides; pubkey signatures roadmap | documented; sigs (roadmap) |
| Agent resource abuse (fork bombs of workspaces/objects) | configurable limits on workspace count/objects per txn; GC | (it7) pending |
| Prompt-injected agent instructed to exfiltrate | out of scope for VCS core; NewGit gives no host access beyond repo dir; documented in AGENT_GUIDE | documented |

## G. Supply chain

| Threat | Mitigation |
|---|---|
| Dependency CVEs / typosquats | 5 runtime deps, all mainstream; Cargo.lock committed; cargo-audit + cargo-deny in CI (it11); SBOM generated |
| Malicious build scripts | none of the runtime deps run postinstall-equivalents beyond standard build.rs of flate2/sha2 (no network); reviewed in it11 audit |
| Toolchain drift | rust-toolchain.toml pins exact version |
