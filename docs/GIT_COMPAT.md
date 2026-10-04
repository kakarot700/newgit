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
| commit message | `Snapshot.message` (UTF-8) | valid UTF-8 payload bytes are preserved exactly in tested cases, including leading/trailing whitespace, CRLF, and no final LF; C0 controls other than LF/CR/TAB and DEL are refused before refs move; non-UTF-8 → lossy + `extras.git_message_lossy=1` |
| author `Name <email>` | `Actor` object: `display_name=Name`, `extras.email`, `id="git:<email>"`, kind Human | exact; same person ⇒ same Actor oid |
| author date + tz | `timestamp_ms` (seconds×1000), `tz_offset_min` | exact to the second |
| committer (when ≠ author) | `extras.git_committer_{name,email,ts_ms,tz}` | exact (restored on export) |
| original commit sha | `extras.git_sha1` (via `--show-original-ids`) | exact |
| first-parent order of merges | `extras.git_parents_ordered` (NewGit `parents` is a sorted set by protocol) | exact (restored on export) |
| mode 100644 / 100755 / 120000 | `EntryMode::File / Executable / Symlink` | exact |
| empty Git tree | empty NewGit `Tree` object | tested for an empty root commit, returning to empty after deleting the only file, and a consecutive empty commit; exact Git tree id survives export in this fixture |
| Valid UTF-8 Git paths | NewGit tree path strings | UTF-8 bytes survive Git C-quoted escaping; tested with Unicode plus quotes/backslashes and a rename through import/export |
| `refs/heads/*`, `refs/tags/*` | same ref names | exact |
| other `refs/*` | same ref names (if the ref grammar accepts them) | exact |
| lightweight tag | ref → target snapshot | exact |
| annotated tag | ref → target snapshot; **tagger + message stripped**, listed in the import report | lossy (documented) |
| signed tag | signature stripped (`--signed-tags=strip`), then as annotated | lossy (documented) |
| Symbolic `HEAD` | NewGit symbolic HEAD, moved **in the same transaction** as refs | tested for an ordinary branch HEAD |
| Detached `HEAD` | NewGit direct snapshot HEAD, moved **in the same transaction** as refs | the `fast-export` pseudo-ref `HEAD` is not imported as a named ref; tested for detached-only and detached-ahead-of-branch histories |

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
| distinct names mapping to one Git ref | refused before target initialization | avoids silently overwriting one exported ref; error names both source refs and the mapped ref |
| Actor | `author`/`committer` lines | email from `extras.email`, else `exported@newgit.local`; empty display name → `NewGit User` |
| `timestamp_ms`, `tz_offset_min` | author date/tz | **sub-second precision is lost** (git stores whole seconds) |
| `git_committer_*` extras | committer lines | restored exactly when present; otherwise committer = author |
| `git_parents_ordered` extra | `from` + `merge` lines | git first-parent lineage preserved exactly |
| trees/blobs | byte-exact | round-tripped blob **git SHAs are identical** — tested |
| Symbolic `HEAD` | `symbolic-ref` | working tree materialized with `reset --hard` |
| Detached `HEAD` | detached checkout of its snapshot | a temporary fast-import ref carries otherwise-unreferenced history and is deleted after checkout; tested output has no leaked temporary ref |

Export streams (no full-repo buffering of blob data) and is deterministic:
same repository ⇒ same marks, same stream bytes. `feature done`/`done`
framing makes a truncated stream a loud fast-import error. Ref tips are
finally pinned with `reset` commands, so fully-shared branch histories are
correct. For detached HEAD, a temporary ref lets `fast-import` emit commits
that no named ref reaches; NewGit checks out the target commit in detached mode
and deletes that temporary ref before returning.

**Round-trip guarantee (tested):** git repo → import → export → git repo
preserves the tested commit count, ref trees (paths, modes, **blob SHAs**),
author/committer identities and timestamps, and first-parent lineage
(`export_roundtrip_matches_git`). Message content is compared in that fixture;
the separate `commit_message_roundtrip_preserves_exact_utf8_bytes` test checks
raw Git commit-object payload bytes at source and export, plus the imported
`Snapshot.message`, for leading/trailing blank lines, trailing spaces, CRLF, no
final LF, and an empty message. The companion
`git_control_character_commit_message_is_refused_atomically` case proves the
tested U+0001 message is rejected with no ref updates. These tests do not
establish lossless handling of non-UTF-8 messages, which are converted lossily
and flagged. `quoted_utf8_git_paths_roundtrip_without_changing_names` compares
Git tree path names, modes, and blob IDs for each commit across a rename whose
names contain non-ASCII UTF-8, quotes, and backslashes; the exported worktree
is clean. Re-importing the
exported repo produces identical tested trees and messages
(`reimport_after_export_is_stable`); snapshot oids differ because
`git_sha1`/committer metadata necessarily reference the new git objects.

Detached-`HEAD` semantics are separately covered by real-Git tests for a
detached-only repository, a detached successor while a named branch remains at
its earlier tip, and nested branch refs that force multiple temporary-ref
candidate rejections. These tests check NewGit's ref set, the exported Git ref
set, detached state, commit messages, file contents, and clean working trees;
they do not claim exact commit-object identity.

Export also refuses distinct NewGit ref names that map to the same Git ref
instead of silently overwriting one. Its regression fixture checks the collision
between `main` and `refs/main`, validates the mapped destination with Git's
`check-ref-format`, and verifies rejection before partial output. A separate
real-Git round-trip fixture checks ordered parents for a valid merge whose second
parent is already an ancestor of its first.

`empty_git_trees_roundtrip_across_root_and_followup_commits` constructs its
history with the Git CLI. It compares each source and exported tree object id,
checks source-parent to exported-parent mapping, verifies empty root trees in
NewGit, and checks every path, mode, and blob through a second NewGit import.
The fixture covers an empty root, one populated commit, deletion back to an
empty tree, and an empty follow-up commit; it also runs Git `fsck` and NewGit
deep verification. This evidence is limited to the exercised Git 2.43.0 Linux
environment and does not claim other Git versions or operating systems.

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
6. Non-UTF-8 commit messages become lossy-converted and flagged; non-UTF-8 Git
   paths are rejected by NewGit's UTF-8 path model. The tested C-quoted UTF-8
   subset does not establish behavior for arbitrary path bytes, control
   characters, or every platform. Git refs whose names violate NewGit's
   stricter ref grammar are skipped and reported.
7. `git` binary must be available and modern enough for `fast-export
   --full-tree --show-original-ids` (git ≥ 2.20; this suite runs against Git
   2.43.0 in the recorded environment).
8. Git commit messages containing C0 control characters other than LF/CR/TAB,
   or DEL, are refused: NewGit's text model does not permit them. The refusal
   includes the source commit id and code point and occurs before ref updates;
   U+0001 is regression-tested.

## CLI details

Both commands support `--json` (`{ok:true,data:<report>}`), `--repo/-C`, and
emit `obs` events with `--debug`. Reports include every mapped ref, every
skipped ref, and every stripped tag — an import/export never loses something
without telling you.

Exit codes: 0 success · 2 usage/invalid source or target (not a git repo,
occupied target, submodule, non-snapshot ref) · standard codes otherwise.
