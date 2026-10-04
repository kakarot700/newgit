# NewGit CLI reference (v0.1 — grows each iteration)

```
newgit [--repo <path> | -C <path>] [--json] [--debug] <command> [args]
```

* `--repo/-C` — operate on an explicit repository root (must contain `.newgit`);
  otherwise the repo is discovered by walking up from the cwd.
* `--json` — machine-readable envelope on stdout:
  `{"ok":true,"data":…}` or `{"ok":false,"error":{"category","message"}}`.
* `--debug` (or `NEWGIT_LOG=json`) — structured JSON-lines events on **stderr**
  (op id, timings). Never contains file contents or secrets.

## Exit codes (stable API)

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | usage error (bad args, invalid names, unknown command/flag) |
| 3 | repository state error (not a repo, object not found, corruption, verify failure) |
| 4 | concurrency race (CAS failed / lock busy) — **retry** |
| 5 | merge/integration conflict or unsafe destructive op (e.g. dirty workspace discard) |
| 6 | resource limit exceeded |
| 7 | auth/permission failure |
| 70 | internal invariant violation (bug — please report with `--debug` output) |

## Commands (iteration 3 scope)

### `newgit init [<dir>]`
Create `.newgit/` (idempotent; existing config/HEAD preserved).

### `newgit status [-w <workspace>] [-n <N> | --all]`
Compare the workspace's live files with its position snapshot.
Lists added/modified/deleted (truncated to N=50 unless `--all`).

### `newgit snapshot -m <msg> [-w <ws>] [--time <unix-ms>] [--tz <minutes>] [--author <actor-id> [--author-name <display>]] [--goal <oid>] [--change <oid>]`
Capture the whole workspace as an immutable snapshot and move its position
ref with compare-and-swap (concurrent snapshot ⇒ exit 4, retry).
`--time/--tz` give deterministic identities for tests/imports (default: now, UTC).
There is **no staging area** — see DECISIONS.md D-006.

### `newgit history [--from <ref|oid>] [-w <ws>] [-n <N>]` (alias: `log`)
Newest-first deterministic walk (timestamp DESC, oid DESC tiebreak) across
all parents. Default N=20; `-n 0` = unlimited.

### `newgit cat <oid|prefix> [--raw]`
Inspect any object (JSON). `--raw` streams blob bytes to stdout (only for blobs).
Prefixes: ≥4 hex chars, must be unambiguous.

### `newgit hash-object <file> [--write]`
Compute the blob id of a file; `--write` stores it.

### `newgit diff [<a> [<b>]] [-w <ws>] [--name-only] [--json] [--context <N>] [--no-renames] [--exit-code]`
Compare two states. Specs: ref name, snapshot/tree oid (prefix ok),
`ws:<name>` (workspace position). With no `<b>` (or no args) the second
side is the **live workspace** (read-only capture). No args at all ⇒
position vs live workspace.
* `--name-only` — just paths.
* `--json` — `{a,b,rename_detection,files:[…]}`; text files carry structured
  `hunks`; `edit_distance_capped:true` marks coarse fallbacks.
* `--exit-code` — exit 1 when differences exist (git-compatible; the only
  command using exit 1).
Renames: exact-content first, then ≥50% similarity (deterministic greedy).
Binary files (NUL in first 8000 bytes) diff at metadata level only.

### `newgit workspace <create|list|show|discard>`
See `newgit help workspace`. Workspaces are isolated concurrent work areas;
`main` is the repository root. Non-main workspaces live in
`.newgit/workspaces/<name>/files/` with position ref `workspaces/<name>`.
`discard` refuses (exit 5) when unsnapshotted changes exist, unless `--force`.

### `newgit actor show | set-default --id <actor-id> [--name <display>]`
Identity metadata. Actor id convention: `human:<name>`, `agent:<name>`,
`process:<name>`; unknown prefixes become `anonymous`.
Resolution order for operations: repo config → `NEWGIT_ACTOR_ID`/`NEWGIT_ACTOR_NAME` env → `anonymous:local`.

### `newgit config show`
Repository configuration and resource limits (`key = value` format).

### `newgit version` · `newgit help [<topic>]`

## Coming in later iterations

`integrate`/`rollback`/merge (it5) · `goal`/`change`/`evidence`/
`proposal`/`verify`/`gc` (it6–7) · `import-git`/`export-git` (it8) ·
`remote`/`serve`/`push`/`pull` (it9) · `ui` (it10).

## Agent usage notes

* Prefer `--json` for everything; parse `ok`/`error.category`, never stderr text.
* On exit code 4, retry the operation (CAS races are normal under concurrency).
* Pin deterministic snapshots in tests with `--time` and `--author`.
* `newgit cat`/`hash-object` let agents address content without filesystem access.
