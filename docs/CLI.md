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

### `newgit integrate <spec> [-w <ws>] [-m <msg>] [--time <ms>] [--author <id>] [--no-renames]`
Atomically integrate another snapshot into a workspace position:
* target already contained ⇒ `up_to_date` (exit 0),
* position is an ancestor of target ⇒ **fast-forward** (ref moves, files
  checked out),
* otherwise a **3-way merge** (base = best common ancestor): clean merges
  create a two-parent merge snapshot (roles recorded in `extras.merge_ours` /
  `extras.merge_theirs` because `parents` is a canonically sorted set) and
  check the merged tree out into the workspace; conflicts ⇒ **exit 5**,
  no ref move and no file changes. Conflict details: use `merge-tree`.

### `newgit merge-tree <ours> <theirs> [--base <b>] [--json] [--no-renames]`
Dry-run merge. Prints the merged tree id, applied renames, and conflicts
(text or JSON). **Exit 0 when clean, 5 when conflicted** (script-friendly).
Content conflicts include `merged_oid`: a stored blob with diff3-style
markers (`<<<<<<< ours` / `||||||| base` / `=======` / `>>>>>>> theirs`)
ready to inspect with `newgit cat <merged_oid> --raw`, edit, and snapshot
as the resolution.

### `newgit rollback [-w <ws>] [--to <spec>] [-m <msg>]`
Rollback = a NEW snapshot carrying an OLD tree; history is never rewritten
(the rollback itself is auditable: parents, message, `extras.op=rollback`).
Default target: `extras.merge_ours` for merge snapshots (undo the merge),
otherwise the sole parent. Files are re-checked-out.

### `newgit checkout [-w <ws>]`
Resynchronize workspace files with the position snapshot. Removes files
NewGit previously materialized that the target tree no longer contains
(tracked-file removal, like git switch); never touches files NewGit has not
tracked. Also the repair path after a crash between an integrate commit and
its file checkout.

### Workflow entities (goals / changes / evidence / evaluations / proposals)
Full worked example: **docs/AGENT_WORKFLOW.md**. Entity ids are the oid of
the first version; updates create new versions on a CAS-protected chain
(`chains/<id>` ref) — concurrent updates fail with exit 4 (retry). Prefixes
≥4 hex chars resolve when unambiguous.

* `goal create <title> [--description D] [--time ms]` · `goal show|list` ·
  `goal set-status <id> open|in_progress|achieved|abandoned`
  (transitions validated: achieved/abandoned only reopen via
  in_progress/open respectively).
* `change create <title> --base <spec> --result <spec> [--goal <id>]` ·
  `change show <id>` · `change list [--goal <id>]` ·
  `change set-status <id> draft|tested|proposed|integrated|abandoned`
  (**honesty gate:** `tested` requires attached evidence) ·
  `change attach-evidence <id> <evidence-oid>`.
* `evidence record [--kind K] [--target <id>] [-w ws] -- <cmd> [args…]` —
  NewGit runs the command itself and records exit code, capped combined
  output (blob), duration; `deterministic=true`, verdict from exit status.
* `evidence add --kind K --verdict pass|fail|inconclusive|not_applicable
  [--deterministic] [--target id] [--output file] [--metric k=v,…]` —
  manual/opinion evidence (`deterministic` defaults false; claim it only
  for tool-verified facts).
* `evidence show <oid>`.
* `evaluation create --target <id> --verdict V [--ai]
  [--dimension name=verdict[:note];…]` — `--ai` marks AI-generated opinion
  (never silently mixed with deterministic verdicts).
* `evaluation from-evidence <change-id>` — deterministic aggregation:
  all pass ⇒ pass, any fail ⇒ fail, else inconclusive; one dimension per
  evidence kind (worst verdict wins; opinions labeled).
* `evaluation show <oid>`.
* `proposal create <title> --change <id> [--rationale R] [--base <spec>]
  [--evidence oid,…] [--depends id,…]` (change must be tested/proposed) ·
  `proposal show|list` · `proposal approve|reject|close <id>` ·
  `proposal integrate <id> [-w ws]` — requires `approved`; moves position
  ref + proposal chain + change chain in ONE transaction; conflicts ⇒
  exit 5 with nothing written.
* `history --goal <id>` filters history to goal-tagged snapshots (combine
  with `-w`/`--from`).

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

## Maintenance commands (iteration 7)

### `newgit verify [--deep] [--json]`

Read-only filesystem integrity check (fsck). Never modifies anything
(D-014). Prints a coded issue list (`object.corrupt`, `ref.target_missing`,
`chain.cycle`, `workspace.orphan_dir`, …) with `error`/`warning` severities.

* Exit 0 when there are no **errors** (warnings are tolerable debris with
  repair hints in the message); exit 3 when any error is found.
* `--deep` additionally re-encodes every object and compares bytes
  (canonical-form drift) and walks every object link (existence + type).
* `--json`: `{ok:true, data:{objects_checked, refs_checked, chains_checked,
  workspaces_checked, quarantined, issues:[{code, severity, detail, oid?,
  path?}]}}` — the report is always the payload; the **exit code** carries
  pass/fail.
* Warning classes (exit stays 0): quarantined objects, orphan workspace
  dirs, missing workspace `files/` (hint: `checkout -w <name>`), corrupt
  index caches (hint: safe to delete), leftover locks, pending journals,
  object-store temp debris.

### `newgit gc [--dry-run] [--force-now] [--json]`

Strictly non-destructive mark-and-sweep garbage collection.

* Roots: HEAD, every ref (incl. `workspaces/*` positions and `chains/*`
  heads), every reflog OLD/NEW oid, workspace `base_oid`s. Reachability
  follows **all** links, including `extras.prev` chain history — audit
  trails are never collected.
* Holds the global txn lock for the run (writers block briefly); runs a
  recovery pass first.
* Unreachable objects **younger than 24 h** are kept (`kept_young`) to
  protect concurrent in-flight writes; `--force-now` disables the grace
  window (tests / single-user repos).
* Never deletes anything it cannot fully decode and identify: corrupt or
  misfiled files stay for forensics (`kept_corrupt`), quarantine
  (`*.corrupt`) is untouchable, non-object debris is left alone.
* `--dry-run` reports without deleting. `--json` returns the full
  `GcReport` (`live_objects, deleted_objects, freed_bytes, kept_young,
  kept_corrupt, quarantined, missing_links, deleted_oids`).

### `newgit recover [--json]`

Explicit crash-recovery pass (also runs automatically on every repository
open): redoes RUNNING journals (idempotent), checkpoint-deletes terminal
journals, quarantines unparsable journals, sweeps stale object temp files.
JSON: `{redone:[ids], quarantined:[names], cleaned:N, temp_files_swept:N}`.

## import-git <git-repo-path>

Imports a real git repository into the current NewGit repo via the system
git's `fast-export` stream (D-007/D-016; full mapping + limitations in
docs/GIT_COMPAT.md). Objects are written first, then ALL refs + HEAD move in
ONE transaction — a failed import (e.g. a submodule/gitlink ⇒ exit 2 with an
actionable error) moves zero refs. Deterministic: the same git repo imported
into fresh NewGit repos yields identical object ids. Skipped namespaces
(`refs/remotes/*`, `refs/notes/*`, `refs/replace/*`, `refs/stash`,
`refs/bisect/*`, `refs/worktree/*`) and stripped annotated/signed tag
metadata are listed in the report — nothing is lost silently.

Flags: `--json` (`{ok,data:{commits,blobs,trees,actors,refs_imported,
refs_skipped,annotated_tags_stripped,head}}`), `--repo/-C`, `--debug`.

## export-git <target-dir>

Exports the NewGit repo to a git repository at `<target-dir>` (must be
absent or empty) via `git init` + `git fast-import`. Round-trip guarantee
(tested against real git): byte-identical blob SHAs, modes, messages,
author AND committer identities/timestamps, first-parent lineage, working
tree materialized and clean. Sub-second timestamps lose precision (git is
whole-second). NewGit-internal namespaces (`workspaces/*`, `chains/*`) are
skipped; non-snapshot ref targets ⇒ exit 2; `refs/X` (non heads/tags) maps
to `refs/heads/X`.

Flags: `--json` (`{ok,data:{commits,blobs,refs_exported,refs_skipped,head,
target}}`), `--repo/-C`, `--debug`.

## Coming in later iterations

`remote`/`serve`/`push`/`pull` (it9) · `ui` (it10).

## Agent usage notes

* Prefer `--json` for everything; parse `ok`/`error.category`, never stderr text.
* On exit code 4, retry the operation (CAS races are normal under concurrency).
* Pin deterministic snapshots in tests with `--time` and `--author`.
* `newgit cat`/`hash-object` let agents address content without filesystem access.
