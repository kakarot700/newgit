# SECURITY_MODEL.md — NewGit

## 1. Trust boundaries

| Boundary | Untrusted side | Enforcement |
|---|---|---|
| Filesystem repo content | objects, refs, config, indexes written by other/older/buggy processes | envelope digest verification on **every** read; strict decoders; config version/unknown-key rejection |
| Workspace files | user/agent-written files, symlinks, names | walk-time path validation; symlink policy (recorded as symlink blobs, never followed out of root); size/count limits |
| CLI input | arguments, prefixes, names | ref/path/name grammars; limit checks; no shell evaluation anywhere |
| Remote wire | HTTP requests, JSON bodies, object batches | size limits, strict JSON schemas, per-object digest verification, refs CAS, authn/authz before any mutation |
| Git interop | foreign repos via fast-export; Git smart-HTTP clients via upload-pack and bounded receive-pack | streamed parsing with limits; path/name validation identical to native; fixed Git argv (never a shell); private temporary projection with isolated Git config and cleared repository-redirection environment; write-role authorization, optional exact-ref admin authorization before projection/import, and transactional canonical ref updates |
| Evidence/evaluation | claims by any actor | honesty flags (`deterministic`, `ai_generated`); policy layer distinguishes them; core never upgrades claims |

## 2. Identity model

* **Display metadata** (Actor objects): who *says* they did something.
* **Authenticated identity**: only via cryptographic verification (pubkey in
  Actor + signatures; remote bearer tokens hashed at rest). A string like
  `agent=Claude` is metadata, never authority.
* Local repos have no ambient authentication: whoever can write `.newgit/`
  can write history (same threat model as git). The remote layer is where
  authz lives (roles: read/write/admin per token).

## 3. Non-execution principle

NewGit **never executes repository content**: no hooks, no filters, no
smudge/clean, no eval of config. The system `git` process is used for import,
export, and smart HTTP. The remote adapter runs isolated exporter
commands and `git upload-pack` with fixed argument vectors, empty global
config and template directories, system config disabled, and Git
repository-redirection variables cleared; only its private temporary
projection is served. Receive-pack is used only after write-role authorization,
runs on this private view, and promotes accepted changes through NewGit
transactions. Operators may configure exact protected Git refs; each effective
create/update/delete of one requires an admin-role token, checked across the
parsed update list before projection, import, or canonical object promotion.
These protections are opt-in and do not alter read/advertisement authorization.
Agents get no host access through NewGit beyond the repository
directory they are pointed at.

## 4. Input hardening rules (implemented + tested)

1. All decoders are total: no panics on any byte sequence (fuzz-tested).
2. Length prefixes checked against remaining input before allocation
   (parse-bomb protection); decompression bounded by configured `max_raw`.
3. Canonical-form strictness: non-minimal varints, trailing bytes, unsorted
   sets, duplicate parents ⇒ hard errors.
4. Path grammar: no absolute paths, `..`, `.`, NUL, control chars, drive
   letters; per-component length caps; symlink-escape check on join. Windows
   filesystem paths additionally reject reserved characters/device names and
   trailing dots/spaces; checkout preflights all tree paths and case-folded
   collisions on Windows/default macOS targets before writing.
5. Config: unknown keys/versions rejected (fail loudly, not silently ignore).
6. Locks: `fs4` whole-file OS advisory locks on stable `.lock` files. The kernel
   releases ownership when the owning process/handle exits; NewGit never reads
   or writes sidecar contents, acquisition waits at most `lock_wait_ms`, and
   paths are never unlinked. Verification ignores legacy owner text. All
   cooperating processes must use this same lock protocol.
7. Smart-HTTP snapshot: projection construction obtains the global transaction/
   GC lock, replays any committed incomplete journal while holding it, and keeps
   it through the complete refs/HEAD/history/object export. Initialization and
   GC share this lock, and direct object deletion is no longer a public bypass.
   A Linux black-box regression pauses a real transaction after its first ref
   apply, proves direct and HTTP readers wait, kills the holder, then verifies
   journal recovery precedes complete Git advertisements and pack responses.
8. Scope: each HTTP request has its own committed snapshot; no protocol token
   pins advertisement and upload-pack to one generation. The exclusive guard
   serializes projections, writers, and GC. Arbitrary filesystem edits outside
   NewGit's APIs are not coordinated. `lock_wait_ms` defaults to 10 seconds;
   an immediate open after a crash can acquire the released OS lock and recover.
   The legacy config key `lock_stale_s` is accepted as an alias for
   `temp_file_grace_s`, which governs abandoned object-temp cleanup only.
   Concurrent operation across binaries using different lock protocols is not
   supported; stop older processes before upgrading. Network-filesystem
   lock/atomicity semantics are not established.

### Platform-specific guarantees and limits

* Bearer tokens use `getrandom`'s OS CSPRNG on every target; failure is loud
  and never falls back to a weaker source. Token digests, not raw tokens, are
  persisted. The token file receives best-effort mode `0600` on Unix; Windows
  inherits the parent directory's ACL, which NewGit does not inspect or tighten.
* Git subprocess deadlines use a fresh process group plus `kill` on Unix and
  `taskkill /T /F` on Windows. Separate platform-gated tests exercise descendant
  cleanup; each native runner must pass its own test before that platform is
  treated as verified.
* Windows checkout rejects unsupported symlinks before writing any paths.
  Windows path aliases/reserved names are rejected; case-collision preflight
  is conservative but does not model every filesystem's Unicode normalization
  or case-folding rules.
* File contents are synced before rename. Directory sync is best-effort because
  not all target filesystems/platforms support opening and syncing directories;
  power-loss durability therefore depends on the host filesystem semantics.

## 5. Resource limits (configurable, `.newgit/config`)

blob size, object size, stored size, tree entries, path component length,
walk depth, lock wait/stale, remote batch object count, HTTP request size.
Defaults chosen for laptops; servers should tighten `max_request_bytes`.

## 6. Secrets hygiene

* No secret is ever logged; audit entries contain ids/names, never tokens.
* Tokens are stored **hashed** (SHA-256) server-side; CLI never echoes them.
* Error messages quote user input only where needed and never file contents.

## 7. Known accepted risks (see THREAT_MODEL.md for full model)

* Local write access ⇒ history rewrite (mitigation: remote-side protections,
  verify, signed objects roadmap).
* zlib decompression cost bounded by `max_raw` but CPU cost of inflating
  ~limit bytes remains (DoS vector for anonymous servers ⇒ require auth).
* Git receive-pack is delegated pack validation, not pack parsing: NewGit
  validates a bounded command envelope (pkt-lines ≤65,520 bytes; at most 256
  ref updates). The existing configurable HTTP body cap (64 MiB by default)
  remains the outer request bound; Git validates pack version, checksum, and
  object contents in the disposable projection.

## 8. Agent interfaces: Web UI + MCP (iteration 10)

* **UI = data-free static shell.** `/` serves one embedded HTML file with no
  auth; it contains zero repository data (asserted in tests). All data flows
  through role-gated `/v1/*` endpoints with the user's own bearer token.
* **XSS-safe by construction.** The UI never uses `innerHTML`,
  `document.write`, or `eval`; every dynamic node is built with
  `textContent` (unit-tested against the shipped file). Hostile commit
  messages/filenames/evidence output are inert text.
* **Token hygiene in the browser.** `sessionStorage` only (dies with the
  tab); no cookies ⇒ no ambient authority ⇒ CSRF is structurally
  impossible; the UI issues no mutations anyway.
* **MCP = thin wrapper, same code path.** `newgit mcp` dispatches through
  `cli::call_json` — the exact CLI validation, limits, and error categories;
  the MCP layer adds no privileges and no bypasses. It inherits the spawning
  user's privileges (documented, KL #36) and binds stdio only.
* **No new dependencies** for either surface (D-002 budget unchanged).
