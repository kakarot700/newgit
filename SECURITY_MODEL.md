# SECURITY_MODEL.md — NewGit

## 1. Trust boundaries

| Boundary | Untrusted side | Enforcement |
|---|---|---|
| Filesystem repo content | objects, refs, config, indexes written by other/older/buggy processes | envelope digest verification on **every** read; strict decoders; config version/unknown-key rejection |
| Workspace files | user/agent-written files, symlinks, names | walk-time path validation; symlink policy (recorded as symlink blobs, never followed out of root); size/count limits |
| CLI input | arguments, prefixes, names | ref/path/name grammars; limit checks; no shell evaluation anywhere |
| Remote wire | HTTP requests, JSON bodies, object batches | size limits, strict JSON schemas, per-object digest verification, refs CAS, authn/authz before any mutation |
| Git interop | foreign repos via fast-export; Git smart-HTTP clients via upload-pack | streamed parsing with limits; path/name validation identical to native; fixed Git argv (never a shell); remote read view is private and temporary with empty isolated Git config and cleared repository-redirection environment; receive-pack is disabled |
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
export, and read-only smart HTTP. The remote adapter runs isolated exporter
commands and `git upload-pack` with fixed argument vectors, empty global
config and template directories, system config disabled, and Git
repository-redirection variables cleared; only its private temporary
projection is served. Git receive-pack is not invoked. Agents get no host
access through NewGit beyond the repository
directory they are pointed at.

## 4. Input hardening rules (implemented + tested)

1. All decoders are total: no panics on any byte sequence (fuzz-tested).
2. Length prefixes checked against remaining input before allocation
   (parse-bomb protection); decompression bounded by configured `max_raw`.
3. Canonical-form strictness: non-minimal varints, trailing bytes, unsorted
   sets, duplicate parents ⇒ hard errors.
4. Path grammar: no absolute paths, `..`, `.`, NUL, control chars, drive
   letters; per-component length caps; symlink-escape check on join.
5. Config: unknown keys/versions rejected (fail loudly, not silently ignore).
6. Locks: `O_EXCL` creation; stale reclaim only past timeout, logged.

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
