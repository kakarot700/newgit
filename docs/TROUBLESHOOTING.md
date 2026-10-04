# Troubleshooting

Symptom → diagnosis → fix. Error categories are stable strings (`--json`
`error.category`); exit codes are a stable API (docs/CLI.md). When in doubt:
rerun with `--json --debug` and read the category, not the prose.

## Exit codes (quick table)

| Code | Meaning | First move |
|---|---|---|
| 0 | success | — |
| 2 | usage error | check the command against `newgit help` / docs/CLI.md |
| 3 | repository state error | `newgit verify --deep`; see below |
| 4 | concurrency race (`cas_failed` / `lock_busy`) | **retry** — this is normal under concurrency |
| 5 | merge conflict / unsafe destructive op | resolve the conflict; or pass the explicit force flag after reading it |
| 6 | resource limit exceeded | raise the limit in `.newgit/config` or split the operation |
| 7 | auth/permission failure | token missing/expired/role too low; `newgit token list` |
| 70 | internal invariant violation | this is a bug: report with `--debug` output + repo state |

## Common situations

### "not a newgit repository"
You are not under a directory containing `.newgit/` (discovery walks up
from CWD). Fix: `cd` into the project or pass `--repo <path>`. Note that
`newgit init <dir>` takes the path as a POSITIONAL argument — `--repo`
selects an EXISTING repository, it does not create one.

### Exit 4 storms (`cas_failed` / `lock_busy`)
Two writers raced; exactly one won (by design). Retry the loser. If retries
never settle, something is holding a stale lock: locks are `<path>.lock`
files next to their target (e.g. `.newgit/refs/refs/main.lock`) and
self-reclaim after `lock_stale_s` (default 300 s) via holder-liveness
checks — wait, or find the runaway process (`newgit verify` flags lock
files and unrecovered journals in `.newgit/txn/`). Never delete lock files
by hand while any newgit process runs.

### Push rejected (exit 5, "non-fast-forward")
The remote ref moved since you observed it. `newgit pull <remote>` then
integrate/merge locally, then push. `--force` exists, requires writer role,
is audit-logged, and does NOT destroy the overwritten objects (gc decides
later, 24 h grace minimum).

### `verify` reports issues (exit 3)
Read the stable issue codes — `verify` is strictly read-only and never
"fixes" anything:
- **corrupt object / digest mismatch**: the file on disk disagrees with its
  content-addressed id. Restore the object from a backup/remote (objects are
  immutable, so any good copy is THE copy); `gc` will never delete corrupt
  data (forensics preserved).
- **missing link**: an object references an oid that is absent. If this is a
  server repo, a peer push was interrupted mid-batch — re-push (objects are
  idempotent). Locally: restore from backup; `verify --deep` names every
  affected object.
- **quarantined journal** (`txn/*.journal.corrupt`): recovery refused a
  damaged journal and preserved it. The repo state is whatever the last
  COMMITTED journal says; inspect the quarantine file before deleting.

### Crash / kill -9 mid-operation
Do nothing special: the next `newgit` command runs recovery automatically
(forward-recovery from journals, idempotent application, dedup via reflog
txn ids). This is tested by fault injection AND chaos suites — if you ever
observe a post-crash inconsistency, that is a P0 bug: keep the repo, run
`newgit verify --deep --json`, report.

### Server won't start
- `Address already in use`: another process holds the port; `--bind :0`
  picks a free port and prints it.
- Token file unreadable/malformed: the server fails fast BEFORE binding —
  `.newgit/tokens.json` (mode 0600) is `{"tokens":[{"id","sha256","role"}]}`;
  hashes only, never raw tokens. Easiest fix: `newgit token remove <id>` +
  re-add (raw token shown once).
- Clients get `auth` (exit 7): the raw token is shown exactly ONCE at
  `token add` time — if lost, `token remove <id>` and add a new one.

### Remote protocol mismatch
`X-NewGit-Protocol` disagreement produces an actionable error naming both
versions. Server and client come from the same binary — upgrade both ends
together; protocol v1 is additive-only within 0.x (docs/PROTOCOL.md).

### Web UI shows the login loop
401 on every call ⇒ token invalid/expired (re-add), or the server does not
allow anonymous and you checked "browse anonymously". The UI never stores
anything except the token in sessionStorage — clearing site data resets it.

### MCP client can't reach tools
`newgit mcp` speaks newline-delimited JSON-RPC on STDIO — one message per
line, no HTTP. A client that sends batch arrays gets -32600 (unsupported).
Verify manually:

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | newgit mcp
```

expect one JSON line back with `serverInfo.name == "newgit"`.

### Disk usage grows
Objects are never overwritten; unreachable ones accumulate as gc fodder.
`newgit gc --dry-run` shows what WOULD go; `newgit gc` collects objects
unreachable from refs+reflogs+workspace bases with a 24 h grace window.
Reflogs and audit logs are roots by design (forensics) — they grow slowly
and are plain text you may rotate ONLY with an understanding that verify
chains and gc roots depend on them.
