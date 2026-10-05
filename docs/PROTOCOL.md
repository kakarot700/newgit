# NewGit Remote Protocol v1

Status: **stable within 0.x** — additive changes only; breaking changes
require `PROTOCOL_VERSION = 2` and a migration note here.
Transport: HTTP/1.1, JSON bodies, `Content-Length` framing only (no chunked,
no keep-alive pipelining: every response closes the connection). No TLS in
v1 — deploy behind a TLS-terminating reverse proxy (nginx/caddy) for
encryption; the protocol is proxy-friendly plain HTTP.

```
newgit serve [--bind host:port] [--token-file P] [--allow-anonymous-read]
             [--max-body BYTES] [--max-threads N] [--ui]
             [--protect-ref refs/heads/NAME | refs/tags/NAME]...
newgit ui    [...same flags...]        # serve with --ui forced on; prints the UI URL
```

`--bind` accepts port `0` (ephemeral); the server then prints
`newgit serve: listening on http://HOST:PORT (protocol v1)` on stdout —
scripts and tests parse this line.

## Git smart-HTTP compatibility

This is a **separate transport adapter**, not an extension of the NewGit JSON
protocol described below. Git clients use the ordinary smart-HTTP discovery
request `GET /info/refs?service=git-upload-pack` and stateless
`POST /git-upload-pack` exchanges for reads. For writes, they use
`GET /info/refs?service=git-receive-pack` and `POST /git-receive-pack`.
Each request authenticates with `Authorization: Bearer <token>`; upload-pack
follows read/anonymous-read policy, while receive-pack requires a write-role
token on both discovery and POST. Both adapters use a private temporary
Git-format projection; NewGit remains canonical storage.

The installed `git upload-pack` produces read advertisements and pack
responses. For a push, the installed `git receive-pack` validates the
stateless request and pack in a disposable projection. A separate staging
import maps previously exported Git commit IDs back to canonical NewGit
snapshots, validates the resulting object closure, then checks every changed
target-ref compare-and-swap under NewGit's transaction lock before promoting
immutable objects and committing the ref set. All accepted refs are journaled
in one transaction and move all-or-nothing; a process or storage failure during
object promotion can leave unreferenced immutable objects, but cannot publish
refs to an incomplete object graph. Routine auth, packet, policy, partial
projection-result, and stale-CAS rejection occurs before object promotion.

Git protocol versions 0, 1, and 2 are passed to upload-pack after validating
the `Git-Protocol` header. HTTP advertisement framing is provided by the
adapter for v0/v1; v2 uses Git's capability advertisement. Successful
responses use Git's binary `application/x-git-upload-pack-advertisement` or
`application/x-git-upload-pack-result` content types, not NewGit JSON
envelopes or `X-NewGit-Protocol` headers. The ordinary Git CLI integration
test exercises v2 `clone`/`ls-remote`, v0 `fetch`, and v1 `pull` against the
local NewGit-backed server, and verifies refs, commit/tree behavior, and blob
bytes. The recorded environment is Git 2.43.0 on Linux; no wider version or
platform matrix is claimed.

A separate Git 2.43.0/Linux loopback regression verifies `clone --depth=1`
creates a one-commit shallow checkout, `fetch --deepen=1` extends its history,
and `fetch --unshallow` restores the full history. After unshallowing, ordinary
`fetch` and `pull --ff-only` still advance the branch and worktree. This is
standard Git upload-pack shallow negotiation; NewGit retains its complete
canonical history, and each request still materializes the full temporary Git
projection before Git negotiates the transferred pack. Other Git versions and
platforms are not established by this test. Evidence:
`tests/git_remote_e2e.rs::real_git_shallow_clone_deepen_unshallow_and_pull_over_smart_http`.

The read adapter also supports the bounded partial-clone filter `blob:none`.
It enables Git's upload-pack filter capability in protected command-scope
configuration, denies other filter types, and allows lazy object-ID requests
only for objects reachable from an exported ref. With Git 2.43.0/Linux,
`git clone --filter=blob:none --no-checkout <url> repo` creates promisor-remote
metadata and omits blob objects; an ordinary `git -C repo checkout <branch>`
then fetches the checked-out file blob on demand, leaving older history blobs
missing locally. This follows Git's [partial-clone design](https://git-scm.com/docs/partial-clone)
and [`git-upload-pack` configuration](https://git-scm.com/docs/git-upload-pack).
It is a client transfer/object-store optimization only: clone and each lazy
fetch still materialize the complete temporary Git projection, so the server
does not avoid full export work or temporary storage. The regression proves
object omission and hydration, not byte-level network savings; other filters,
Git versions, and platforms are not established. Git documents that the
`uploadpack.allowReachableSHA1InWant` reachability calculation can be
computationally expensive ([Git 2.43 configuration reference](https://git-scm.com/docs/git-config/2.43.0)); the existing 120-second request deadline bounds
elapsed time but does not remove that cost. Partial clone is therefore a
client-side transfer/object-store option, not a server performance optimization.
Evidence:
`tests/git_remote_e2e.rs::real_git_partial_clone_omits_blobs_and_lazily_fetches_checkout_content`.

The write-side receive-pack adapter accepts protocol versions 0 and 1. For
version 1, the server delegates the standard `version 1` advertisement packet
and receive-pack exchange to the installed Git executable, passing the validated
version on both discovery and POST requests. A real Git 2.43.0 test verifies the
v1 advertisement and completes an atomic branch push. A request for receive-pack protocol version 2 is answered and processed with
the conventional v0 framing, matching Git 2.43 receive-pack behavior. This is a
fallback, not v2 push support; other unsupported versions are refused.

**Bounded write policy:** receive-pack accepts branch creates, fast-forward or
forced non-fast-forward updates, and deletions under `refs/heads/*`, plus lightweight tag creates or
deletions under `refs/tags/*`; this includes a first branch push to an empty
repository. Tags must directly target commit objects. Existing tags cannot be
retargeted, even with `--force`, because NewGit stores tags only as refs to
snapshots; this tag policy is enforced separately from branch non-fast-forward
handling. Incoming annotated tags resolve to Git `tag` objects and are refused
before canonical import or object promotion. The concrete diagnostic is
`annotated Git tag <ref> resolves to a tag object; NewGit has no Git tag-object
or per-ref metadata representation, so tagger/message/signature data cannot be
preserved`; accepting one as a lightweight tag would silently flatten that
metadata. Other ref namespaces are refused. Every requested operation must be accepted by Git in
the disposable projection before canonical refs move; if Git accepts only a
subset, the adapter returns HTTP 409 and commits none of the NewGit refs. For
explicit `git push --atomic`, Git's `receive-pack` projection enforces all-or-none
policy validation, and NewGit commits all accepted canonical refs in one
CAS-guarded journal transaction; if any operation is rejected or any CAS check
fails, no canonical refs move. The server advertises Git's `atomic` capability
to match those guarantees. An ordinary non-atomic request that Git accepts only
in part is instead rejected with HTTP 409. The Git receive-pack command carries
the old and new object IDs but no `--force`/`--force-with-lease` indicator.
Standard Git clients reject an unforced stale update and a mismatched lease
locally; a matching lease or `--force` can send the same update command. NewGit
requires its old object ID to map to the canonical old tip, then rechecks that
tip with CAS under the transaction lock before promoting objects. A custom
write-authenticated client can submit the same non-fast-forward wire command
without a force flag, so explicit force intent cannot be enforced server-side.
Operators can additionally repeat `--protect-ref <exact-git-ref>` to require
the `admin` role for every effective create, update, or delete of the named
branch or tag. This is opt-in and exact-name only: wildcard/prefix patterns and
unsupported or non-NewGit-representable names are rejected during server
configuration; an identical old/new oid is a no-op. The complete parsed update
list is checked before constructing the disposable Git projection or importing
objects. Since `refs/heads/tags/X` and `refs/tags/X` map to the same canonical
NewGit ref, an incoming wire name with that same canonical destination shares
the protection gate; NewGit cannot store separate policy targets for aliases.
A protected change by a valid write-role token returns HTTP 403
(invalid/missing credentials remain 401); a denied mixed or atomic push leaves
all canonical refs and objects unchanged. Reads and unprotected refs retain
their existing authorization behavior. Evidence: unit role/ref tests,
`tests/cli_e2e.rs::serve_rejects_wildcard_protection_even_when_followed_by_an_exact_ref`,
and `tests/git_remote_e2e.rs::real_git_protected_refs_require_admin_and_reject_mixed_pushes_before_promotion`.
Signed pushes remain refused. Receive-pack protocol version 2 falls back to v0
rather than enabling v2 push framing; other unsupported versions are refused. A
branch such as `refs/heads/main` maps
to NewGit's `refs/main`; `refs/tags/v1` maps to NewGit's `refs/tags/v1`. Unmapped
names must pass NewGit's ref-name validation. Actual Git 2.43.0/Linux tests cover
branch and tag creation/deletion, protocol-v1 atomic branch creation, a `protocol.version=2` atomic push via v0 fallback, atomic branch-plus-tag creation, unauthorized
and invalid-tag rejection without canonical mutation, ordinary stale-push and
mismatched-lease rejection, matching `--force-with-lease` and `--force` success,
forced atomic-batch rejection without canonical mutation, and post-force
clone/fetch; other versions/platforms are not claimed.

The hand-rolled receive-pack command envelope is deliberately bounded: every
pkt-line is at most 65,520 bytes including its header, and a request may update
at most 256 refs (larger command lists return HTTP 413, independently of
`--max-body`). Only the first command may carry the NUL-delimited capability
list; one leading space after that NUL is accepted because Git 2.43.0 emits it.
Create/update requests require a packfile, including a valid empty pack when
all objects are already present; deletion-only requests carry no packfile.
Git itself validates pack format, checksum, and object contents in the
disposable projection. The configured request body cap remains 64 MiB by
default. Evidence: `src/remote/git_receive.rs::tests::fuzz_receive_pack_parser_never_panics_or_accepts_unbounded_command_sets` and `tests/git_remote_e2e.rs::real_git_clone_fetch_pull_push_and_ls_remote_over_smart_http` (Git 2.43.0/Linux).

The adapter materializes the full Git view independently for every discovery
and POST request. Git's pack negotiation can reduce transferred bytes, but it
does not avoid that full-history export. Both inbound request bodies and
outbound buffered pack/advertisement responses are capped by `--max-body`
(64 MiB by default); an oversized response is rejected. A single discovery or
upload-pack or receive-pack request has a 120-second wall-clock budget across
projection generation and Git subprocess work; deadline expiry returns HTTP
504 and force-terminates the Git process group. Temporary disk usage and peak
RAM are not separately quota-limited. Use Git bearer auth with an HTTP header
(for example, Git's `http.extraHeader`) or explicitly enable anonymous reads.
The server itself speaks plain HTTP; terminate TLS at a trusted reverse proxy
for remote networks. See [deployment](DEPLOYMENT.md) and
[compatibility evidence](GIT_COMPATIBILITY_MATRIX.md).

Protocol behavior follows Git's specifications for
[smart HTTP](https://git-scm.com/docs/gitprotocol-http),
[protocol v2](https://git-scm.com/docs/gitprotocol-v2),
[pack negotiation](https://git-scm.com/docs/gitprotocol-pack),
[push pack protocol](https://git-scm.com/docs/pack-protocol), and
[`git upload-pack`](https://git-scm.com/docs/git-upload-pack) /
[`git receive-pack`](https://git-scm.com/docs/git-receive-pack) commands.
The implementation delegates wire and pack details to the installed Git
executable rather than maintaining an independent pack implementation.

## Envelope

Every response body is JSON, identical in shape to the CLI envelope:

```json
{"ok": true,  "data": { ... }}
{"ok": false, "error": {"category": "...", "message": "..."}}
```

Categories are the CLI's stable set (`usage, invalid, malformed, protocol,
auth, cas_failed, lock_busy, conflict, limit, not_found, ref_not_found, io,
verify, config, bug`). HTTP status mapping (server side, single source of
truth `http::status_for_error`):

| condition | status |
|---|---|
| success | 200 |
| malformed/invalid/protocol (bad JSON, bad oid hex, bad request) | 400 |
| missing/invalid token | 401 |
| valid token, insufficient role | 403 |
| unknown endpoint / unknown object / unknown want | 404 |
| wrong HTTP method on a known endpoint | 405 |
| CAS failure, lock busy, non-fast-forward | 409 |
| batch/body over limit | 413 |
| server busy (thread cap) | 429 |
| Git adapter operation deadline | 504 |
| internal/bug/io | 500 |
| chunked transfer-encoding | 501 |

Every NewGit JSON response carries `X-NewGit-Protocol: 1`; NewGit JSON clients
send it too and reject servers whose `/v1/info.protocol` differs (actionable
upgrade hint). Git smart-HTTP binary responses do not use this NewGit header.

## Authentication & roles

`Authorization: Bearer <token>`. Tokens are high-entropy random strings
(32 bytes OS-CSPRNG, base64); the server stores only SHA-256 hex digests
(`.newgit/tokens.json` by default, 0600). Roles: `read < write < admin`.

| endpoint | minimum role |
|---|---|
| `GET /healthz`, `GET /v1/info` | none (anonymous) |
| `GET /v1/refs`, `POST /v1/have`, `POST /v1/negotiate`, `POST /v1/objects/get` | read (anonymous iff `--allow-anonymous-read`) |
| `POST /v1/objects/put`, `POST /v1/refs/update` | write |
| `GET /v1/audit` | admin |

Hard rules (tested): a present-but-invalid token is **never** downgraded to
anonymous (401 even on anonymous endpoints); role failures are 403; token
management is CLI-only (`newgit token add|list|remove`) — no endpoint ever
accepts or returns raw tokens; `token list` never prints them either.

Every request — including 401/403/409 failures — is appended to
`.newgit/audit.log` (JSON lines: `ts_ms, principal, method, path, status,
error`), readable via `GET /v1/audit?limit=N` (admin) or `newgit audit`.
Audit append failures never fail requests (availability first) but surface
as obs events under `--debug`.

## Endpoints

### `GET /healthz` → `"healthy"`
### `GET /v1/info`

```json
{"product":"newgit","version":"0.1.0","protocol":1,"head":"ref: refs/main",
 "capabilities":["have","negotiate","objects-get","objects-put","refs-update","audit",
                "object","diff","goals","changes","proposals"],   // +"ui" when --ui
 "limits":{"max_batch_objects":4096,"max_request_bytes":67108864}}
```

### `GET /v1/refs` → `{head, refs:[{name,oid}]}`

All refs EXCEPT internal namespaces (`workspaces/*`, `chains/*`) — those are
also invisible to `refs.list()` by grammar, so the filter is defense in
depth. Oids are full lowercase hex.

### `POST /v1/have` `{oids:[hex]}` → `{have:[hex]}`

Answers which listed oids the server stores. Batch cap:
`limits.max_batch_objects`.

**Have-invariant (load-bearing):** the server's object store is always
*link-closed* — `objects/put` refuses any object whose links are not already
stored. Therefore "server has X" implies "server has closure(X)", which lets
clients prune negotiation at X without walking it. (Local writes maintain
the same invariant by construction: snapshots/trees are written after their
dependencies.)

### `POST /v1/negotiate` `{have:[hex], want:[hex]}` → `{send:[hex]}`

`send = reachable(want) − reachable(have)`, in **dependency-first
post-order** (blobs → trees → snapshots), computed server-side. Haves the
server does not store are ignored — semantics are a sound SUPERSET (a
client may be offered objects it already has; it can never be offered too
few). Unknown `want` ⇒ 404. Used by `pull`.

### `POST /v1/objects/get` `{ids:[hex]}` → `{objects:[{data_b64}]}`

Same order as `ids`. `data_b64` is the base64 of the full **envelope**
bytes (`NGOB` v1: compressed canonical object + SHA-256 digest + id) —
self-verifying on receipt. Missing object ⇒ 404. Batch cap applies.
v1 note: base64 inflates payloads ~33%; acceptable for v1 simplicity,
binary framing is a v2 candidate.

### `POST /v1/objects/put` `{objects:[{data_b64}]}` → `{stored:N, oids:[hex]}`

Per object the server: b64-decodes → `envelope::verify` (digest + id +
canonical-form checks, size caps) → decodes → **enforces the have-invariant**
(every link already stored, else 400 `"missing dependency …"` — this is what
makes out-of-order or smuggled-dangling writes impossible) → stores
atomically (idempotent: re-putting an identical object is a no-op success;
different bytes under the same id fail digest/id checks).

Sequential all-until-first-error semantics: on failure, objects stored
earlier in the batch REMAIN as unreachable orphans — harmless (gc fodder,
24 h grace), because **refs, not objects, are the atomicity boundary** (a
crash between put and refs/update leaves zero visible change; retry reuses
the orphans — tested `crash_mid_push_leaves_server_clean_and_retry_succeeds`).

### `POST /v1/refs/update` `{updates:[{name,cas,new,message?}]}` → `{updated,txn_id}`

ALL updates apply in ONE journaled transaction (crash-safe, all-or-nothing;
identical engine as local ref writes). Per update:

- `name` — must pass the system ref grammar; internal namespaces
  (`workspaces/*`, `chains/*`) are REFUSED remotely (400).
- `cas` — `{"kind":"any"}` (force) or `{"kind":"exactly","old":hex|null}`
  (null = must not exist). Any CAS failure ⇒ 409 `cas_failed`, nothing moves.
- `new` — hex oid (must already be stored, else 400) or `null` (delete).
- Reflog: each move records `remote update by <principal>[: message]`.

### Read endpoints (iteration 10) — object / diff / workflow listings

All read-role gated (or anonymous when `--allow-anonymous-read`). They exist
so agents and the embedded UI never need to guess oid→shape mapping:

#### `POST /v1/object` `{oid:hex}` → `{oid,kind,links,data?|data_b64?+size?}`

- `kind` ∈ `blob|tree|snapshot|actor|goal|change|evidence|evaluation|proposal`.
- Non-blobs: `data` = the decoded object struct (same serde shape as
  `newgit cat --json`, minus the envelope); `links` = every oid the object
  references (`verify::object_links`, the single source of link truth).
- Blobs: `data_b64` (raw bytes, base64) + `size` — never a JSON number array.
  UI previews cap at 64 KiB client-side; the endpoint itself has no cap
  beyond `max_request_bytes` on the response path.
- Unknown oid ⇒ 404 `not_found`; bad hex ⇒ 400 `malformed`.

#### `POST /v1/diff` `{a,b,content?,context?,no_renames?}` → `{a_root,b_root,diff,unified}`

- `a`/`b` accept a (user-namespace) ref name or an oid/hex-prefix. The
  local-only shorthands are REFUSED with 400 (`invalid`): `ws:<name>` and
  internal namespaces (`workspaces/*`, `chains/*`) never cross the wire —
  the diff endpoint must not become a probe channel for them (same
  invariant as `/v1/refs` and `refs/update`). Unknown ⇒ 404.
- `diff` = the full `TreeDiff` JSON (identical to `newgit diff --json`:
  files[] with kind/path/old_path/modes/oids/binary/similarity + rename
  detection unless `no_renames`).
- `content:true` additionally returns `unified:[{path,unified}]` — the SAME
  rendering code as the CLI (`render::file_header` + `render::render_content`),
  capped at 100 modified non-binary files per response (`UNIFIED_CAP`) to
  bound payload size. `content` omitted/false ⇒ `unified:[]`.

#### `GET /v1/goals` · `GET /v1/changes[?goal=<hex>]` · `GET /v1/proposals`

→ `{entities:[{oid,data}],truncated}` — `data` is the wrapped object in
`{"type","data"}` form. `changes?goal=` filters server-side (invalid hex ⇒
400; matching-nothing ⇒ empty list, NOT 404). Same enumeration as
`goal list`/`change list --goal`/`proposal list`. Listings are capped at
`limits.max_batch_objects` entities; when cut, `truncated:true` (clients
narrow with `?goal=` or paginate locally by oid).

### Static UI route (iteration 10)

When the server is started with `--ui` (`newgit ui`), `GET /` and
`GET /index.html` return the embedded single-file Web UI
(`text/html; charset=utf-8`, no auth required — the file contains ZERO
repository data; all data flows through the role-gated `/v1/*` endpoints
with the user's own bearer token). `/v1/info` gains capability `"ui"`.
Without `--ui`, `/` is a normal 404. The UI never mutates (read-only by
design, KNOWN_LIMITATIONS #33); bearer-header auth (no cookies) makes CSRF
structurally impossible.

## Client operations

### `newgit push <remote> [refs…] [--all] [--force]`

1. `GET /v1/info` (protocol check) and `GET /v1/refs` (observe tips).
2. **Non-fast-forward guard** (unless `--force`): for each ref where the
   server tip differs, the server tip must EXIST locally and be an ancestor
   of the local tip (closure membership) — else exit 5 `conflict` with an
   actionable "pull first" message. Stale clones cannot clobber (git
   semantics); the wire CAS additionally protects the observe→update window
   against concurrent pushers (one winner, loser exits 4 `cas_failed`).
3. Negotiation: BFS from local tips, probing `POST /v1/have` in batches;
   descent STOPS at server-held oids (have-invariant). Send set =
   local `post_order(tips, exclude=server-held)` — dependencies first,
   nothing the server already has (incremental push tested: no-op push
   sends 0 objects).
4. `POST /v1/objects/put` in dependency-ordered batches.
5. `POST /v1/refs/update` — one transaction, CAS `exactly(observed tip)`
   (or `any` with `--force`).

Default ref selection: the ref HEAD points at; `--all` = every local ref
outside internal namespaces. Detached HEAD without explicit refs ⇒ usage
error.

### `newgit pull <remote> [refs…]`

1. `GET /v1/refs`; selection = all non-internal refs (or the listed names).
2. `POST /v1/negotiate` with `have` = all local ref tips + HEAD.
3. `POST /v1/objects/get` in batches; each object re-validated locally
   (envelope verify + id match + dependency presence) before storing —
   a malicious/broken server cannot inject corrupt or dangling objects.
4. Local refs move in ONE transaction, CAS `exactly(local tip observed at
   start)` — a concurrent local writer wins; the pull exits 4 and can be
   retried. **HEAD and workspaces are NOT touched** — `pull` is NewGit's
   `fetch`: integrate explicitly (`newgit integrate`, workspaces) so agents
   keep full control of working state.

### `newgit remote add <name> <url> [--token T] | list | remove <name>`

Stored in `.newgit/remotes.json` (0600; raw token — same trust model as
`~/.git-credentials`, documented). `list` masks tokens (`token=set|none`).
URLs: `http://host[:port]` only; `https://` and URL paths are rejected with
actionable errors (v1: TLS at the proxy, one repo per server).

## Limits & abuse resistance

- Request line ≤16 KiB; ≤128 headers / 64 KiB total; body: `Content-Length`
  required, checked against `--max-body` (default 64 MiB) BEFORE reading.
- Git smart-HTTP pack/advertisement responses are buffered and capped by the
  same `--max-body` value; each Git request has a separate 120-second total
  deadline, returning 504 on expiry. The response cap and deadline are tested;
  temporary disk/peak-memory quotas are not implemented.
- Batch caps: `max_batch_objects` (default 4096) for have/get/put;
  `negotiate` allows up to max(batch,10 000) oid arguments.
- Thread cap (`--max-threads`, default 32) ⇒ 429 beyond; per-connection
  read/write timeouts 30 s; client connect 10 s, read 300 s, write 60 s.
- Parsers are total and fuzzed (`fuzz_http_and_wire_json_never_panic`,
  20k seeded prefix-anchored inputs).
- Objects travel as self-verifying envelopes; ids are recomputed from
  content on both ends — no endpoint trusts a claimed oid.

## Versioning

`PROTOCOL_VERSION = 1` lives in `src/remote/proto.rs`; `/v1/` path prefix +
info field + header all carry it. Clients hard-fail on mismatch with an
upgrade hint (tested). v2 candidates (not promised): binary object framing,
keep-alive, incremental negotiate cursors, multi-repo servers, TLS.
