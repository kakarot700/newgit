# Git Compatibility (iteration 8)

NewGit interoperates with real git through the **system git's own stream
formats** (`git fast-export` / `git fast-import`) — D-007. NewGit never
reimplements git's object format, never invokes network git operations, and
adds no dependencies for this. If `git` is not on PATH, `import-git` /
`export-git` fail with a clear error; everything else works without git.

```
newgit import-git <git-repo-path>     # git → NewGit (into the current repo)
newgit export-git <target-dir>        # NewGit → git (target must be empty/absent)
```

## Import mapping (git → NewGit)

| git concept | NewGit representation | fidelity |
|---|---|---|
| blob | `Blob` object, byte-exact | exact |
| tree | `Tree` objects (built per commit) | exact |
| commit | `Snapshot` object | exact (see timestamps) |
| commit message | `Snapshot.message` (UTF-8) | exact; non-UTF-8 → lossy + `extras.git_message_lossy=1` |
| author `Name <email>` | `Actor` object: `display_name=Name`, `extras.email`, `id="git:<email>"`, kind Human | exact; same person ⇒ same Actor oid |
| author date + tz | `timestamp_ms` (seconds×1000), `tz_offset_min` | exact to the second |
| committer (when ≠ author) | `extras.git_committer_{name,email,ts_ms,tz}` | exact (restored on export) |
| original commit sha | `extras.git_sha1` (via `--show-original-ids`) | exact |
| first-parent order of merges | `extras.git_parents_ordered` (NewGit `parents` is a sorted set by protocol) | exact (restored on export) |
| mode 100644 / 100755 / 120000 | `EntryMode::File / Executable / Symlink` | exact |
| `refs/heads/*`, `refs/tags/*` | same ref names | exact |
| other `refs/*` | same ref names (if the ref grammar accepts them) | exact |
| lightweight tag | ref → target snapshot | exact |
| annotated tag | ref → target snapshot; **tagger + message stripped**, listed in the import report | lossy (documented) |
| signed tag | signature stripped (`--signed-tags=strip`), then as annotated | lossy (documented) |
| HEAD (symbolic / detached) | NewGit HEAD, moved **in the same transaction** as the refs | exact |

**Atomicity.** Objects are written first (content-addressed, idempotent);
then ALL refs + HEAD move in ONE transaction. A crash or any error mid-import
leaves orphan objects (gc fodder) and unmoved refs — never a half-imported
history. A failed import (e.g. submodule) moves zero refs — tested.

**Determinism.** Importing the same git repository into two fresh NewGit
repos yields **identical object ids** for every ref — tested
(`import_is_deterministic`).

**Skipped namespaces** (reported in `refs_skipped`): `refs/remotes/*`,
`refs/notes/*`, `refs/replace/*`, `refs/stash`, `refs/bisect/*`,
`refs/worktree/*`. NewGit-internal namespaces (`workspaces/*`, `chains/*`)
are never exported.

## Export mapping (NewGit → git)

| NewGit | git | notes |
|---|---|---|
| Snapshot chain | commits via `git fast-import` | parents-before-children topological stream |
| `refs/heads/*`, `refs/tags/*` | pass through | annotated tags cannot be rebuilt (metadata was stripped at import) → lightweight |
| `refs/X` (other) | `refs/heads/X` | e.g. `refs/main` → `refs/heads/main` |
| bare ref names | `refs/heads/<name>` | |
| Actor | `author`/`committer` lines | email from `extras.email`, else `exported@newgit.local`; empty display name → `NewGit User` |
| `timestamp_ms`, `tz_offset_min` | author date/tz | **sub-second precision is lost** (git stores whole seconds) |
| `git_committer_*` extras | committer lines | restored exactly when present; otherwise committer = author |
| `git_parents_ordered` extra | `from` + `merge` lines | git first-parent lineage preserved exactly |
| trees/blobs | byte-exact | round-tripped blob **git SHAs are identical** — tested |
| HEAD symbolic/detached | `symbolic-ref` / detached checkout | working tree materialized via `reset --hard`; exported repo is clean |

Export streams (no full-repo buffering of blob data) and is deterministic:
same repository ⇒ same marks, same stream bytes. `feature done`/`done`
framing makes a truncated stream a loud fast-import error. Ref tips are
finally pinned with `reset` commands, so fully-shared branch histories are
correct.

**Round-trip guarantee (tested):** git repo → import → export → git repo
preserves: commit count, every ref's full tree (paths, modes, **blob SHAs**),
all commit messages, author AND committer identities and timestamps, and
first-parent lineage (`export_roundtrip_matches_git`). Re-importing the
exported repo produces identical trees and messages (`reimport_after_export_
is_stable`); snapshot oids differ because `git_sha1`/committer metadata
necessarily reference the new git objects.

## Hard limitations (loud, never silent)

1. **Submodules (gitlinks, mode 160000) abort the import** with an actionable
   error; zero refs move (tested). NewGit has no gitlink concept.
2. **Annotated/signed tag metadata** (tagger, message, signature) is not
   representable — NewGit has no tag object type. Refs survive; metadata is
   listed in the report.
3. **Timestamps**: git stores whole seconds — NewGit milliseconds lose
   sub-second precision on export (import is exact; git has no sub-second).
4. **Memory**: import caches per-commit tree states (for incremental streams
   and parent inheritance); gigantic histories are RAM-bounded
   (KNOWN_LIMITATIONS #20). Blob payloads stream and are not cached.
5. **No incremental sync**: import/export are whole-history operations;
   there is no fetch/pull negotiation with git remotes (that is what the
   NewGit remote protocol — iteration 9 — is for).
6. Non-UTF-8 commit messages become lossy-converted and flagged; git refs
   whose names violate NewGit's stricter ref grammar are skipped and
   reported.
7. `git` binary must be available and modern enough for `fast-export
   --full-tree --show-original-ids` (git ≥ 2.20; tested against 2.47).

## CLI details

Both commands support `--json` (`{ok:true,data:<report>}`), `--repo/-C`, and
emit `obs` events with `--debug`. Reports include every mapped ref, every
skipped ref, and every stripped tag — an import/export never loses something
without telling you.

Exit codes: 0 success · 2 usage/invalid source or target (not a git repo,
occupied target, submodule, non-snapshot ref) · standard codes otherwise.
