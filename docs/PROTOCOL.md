# NewGit Remote Protocol v1

Status: **stable within 0.x** — additive changes only; breaking changes
require `PROTOCOL_VERSION = 2` and a migration note here.
Transport: HTTP/1.1, JSON bodies, `Content-Length` framing only (no chunked,
no keep-alive pipelining: every response closes the connection). No TLS in
v1 — deploy behind a TLS-terminating reverse proxy (nginx/caddy) for
encryption; the protocol is proxy-friendly plain HTTP.

```
newgit serve [--bind host:port] [--token-file P] [--allow-anonymous-read]
             [--max-body BYTES] [--max-threads N]
```

`--bind` accepts port `0` (ephemeral); the server then prints
`newgit serve: listening on http://HOST:PORT (protocol v1)` on stdout —
scripts and tests parse this line.

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
| internal/bug/io | 500 |
| chunked transfer-encoding | 501 |

Every response carries `X-NewGit-Protocol: 1`; clients send it too and
reject servers whose `/v1/info.protocol` differs (actionable upgrade hint).

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
 "capabilities":["have","negotiate","objects-get","objects-put","refs-update","audit"],
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

## Limits & abuse resistance (all tested)

- Request line ≤16 KiB; ≤128 headers / 64 KiB total; body: `Content-Length`
  required, checked against `--max-body` (default 64 MiB) BEFORE reading.
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
