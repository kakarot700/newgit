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
| original Git commit object ID | `extras.git_oid` (via `--show-original-ids`); SHA-1 sources also retain legacy `extras.git_sha1` | exact source ID metadata; not the NewGit snapshot ID and not preserved in exported commit IDs |
| SHA-256 Git repository | same semantic snapshot/ref mapping as SHA-1 input; source commit IDs recorded in `git_oid` | real-Git semantic import/export fixture; destination uses `git init`'s default object format, so object IDs are not expected to match |
| signed commit | no signature field in NewGit; `signed_commits_stripped` reports source commit IDs whose `gpgsig` header was dropped by `fast-export` | lossy; the signature is neither preserved nor verified |
| first-parent order of merges | `extras.git_parents_ordered` (NewGit `parents` is a sorted set by protocol) | exact for tested ordinary, redundant-ancestor, and four-parent octopus merges; restored on export |
| mode 100644 / 100755 / 120000 | `EntryMode::File / Executable / Symlink` | exact |
| empty Git tree | empty NewGit `Tree` object | tested for an empty root commit, returning to empty after deleting the only file, and a consecutive empty commit; exact Git tree id survives export in this fixture |
| Valid UTF-8 Git paths | NewGit tree path strings | UTF-8 bytes survive Git C-quoted escaping; tested with Unicode plus quotes/backslashes and a rename through import/export |
| `refs/heads/*`, `refs/tags/*` | same ref names when they target commits | exact for tested commit refs; other object targets are refused |
| other `refs/*` | same ref names (if the ref grammar accepts them) | commit targets only; other object targets are refused |
| lightweight tag | ref → target snapshot | exact |
| annotated tag | ref → target snapshot; **tagger + message, including any signature block, are stripped** and the tag ref is listed in `annotated_tags_stripped` | lossy (documented; Git-verified SSH-signed tag regression) |
| signed tag | NewGit does not preserve or verify tag signatures; export can only make a lightweight tag | lossy; Git 2.43.0/Linux `fast-export --signed-tags=strip` retains the tested SSH signature bytes in tag data, which NewGit drops with the rest of the tag metadata; other signature formats/versions/platforms are untested |
| ref targeting a blob, tree, or other non-commit object | no NewGit ref is written | refused before `fast-export`, because NewGit's Git export path represents snapshot histories |
| Symbolic `HEAD` | NewGit symbolic HEAD, moved **in the same transaction** as refs | tested for an ordinary branch HEAD |
| Detached `HEAD` | NewGit direct snapshot HEAD, moved **in the same transaction** as refs | the `fast-export` pseudo-ref `HEAD` is not imported as a named ref; tested for detached-only and detached-ahead-of-branch histories |
| Non-`HEAD` symbolic refs | not imported; detected with `git for-each-ref` and listed in `refs_skipped` | unsupported: NewGit refs are direct object pointers; a real-Git fixture with two branch aliases confirms Git 2.43.0 `fast-export --all` omits them |
| Git namespace refs (`refs/namespaces/*`) | no NewGit ref | unsupported; the importer scans and reports these names even when `fast-export` omits them, while export skips and reports namespace-shaped NewGit refs rather than flattening them into branches |
| Git replace refs | `refs/replace/*` skipped and reported; `fast-export` runs with `GIT_NO_REPLACE_OBJECTS=1` | replacement overlays are unsupported; the test proves ordinary branch history is imported from stored objects rather than silently rewritten through a replacement commit |

**Atomicity.** Objects are written first (content-addressed, idempotent);
then ALL refs + HEAD move in ONE transaction. A crash or any error mid-import
leaves orphan objects (gc fodder) and unmoved refs — never a half-imported
history. A failed import (e.g. submodule) moves zero refs — tested.

**Determinism.** Importing the same git repository into two fresh NewGit
repos yields **identical object ids** for every ref — tested
(`import_is_deterministic`).

**Skipped namespaces** (reported in `refs_skipped`): `refs/remotes/*`,
`refs/notes/*`, `refs/namespaces/*`, `refs/replace/*`, `refs/stash`,
`refs/bisect/*`, and `refs/worktree/*`. Non-`HEAD` symbolic refs are separately
discovered and reported because `fast-export --all` omits them. The `for-each-ref`
pre-scan seeds reports for all configured skipped families too, so unsupported
refs with blob targets are still reported when `fast-export` omits them (tested
for remote-tracking and notes refs on Git 2.43.0/Linux). NewGit-internal
namespaces (`workspaces/*`, `chains/*`) are never exported.

Git namespace refs under `refs/namespaces/<namespace>/...` are unsupported and
deliberately skipped. A real Git 2.43.0/Linux fixture confirms that
`GIT_NAMESPACE=tenant git ls-remote` presents namespace entries as virtual refs,
while `fast-export --all` emits its physical full name. Since NewGit's generic
export mapping would turn that physical name into an unrelated ordinary branch,
the importer reports and omits it rather than silently changing its meaning;
the test also verifies the regular branch still round-trips and no mapped
namespace branch leaks into the export. The pre-scan reports a namespace ref to
a blob even though Git omits that ref from `fast-export`. This skip is not a
confidentiality boundary: commits reachable only from a namespace ref can still
be streamed and imported as unreferenced objects before the ref is omitted. The
pre-scan and `fast-export` are separate commands, so concurrent source-ref
mutation is not synchronized.

Replace-ref semantics are not imported. Since Git normally applies replacement
objects transparently during history traversal, the importer sets
`GIT_NO_REPLACE_OBJECTS=1` for `fast-export`: this avoids silently changing an
ordinary branch's stored commit/tree/message when `refs/replace/*` is skipped.
The regression fixture confirms this policy for one replacement commit on Git
2.43.0/Linux. Git commands that honor replacement refs can therefore display a
different effective history than the stored-object history NewGit imports.

Before starting `fast-export`, import inspects ordinary refs' direct or peeled
Git object types. Refs to objects other than commits are refused with the ref
name and target type. This prevents an annotated tag-to-blob from importing as
a NewGit ref that `export-git` cannot export, and prevents silent loss of
lightweight blob/tree refs that Git 2.43.0 `fast-export --all` omits. Git-internal
refs already listed as skipped, and non-`HEAD` symbolic refs, keep their existing
skip/report behavior. Real-Git refusal cases cover Git 2.43.0/Linux; nested
annotated-tag chains and other Git versions/platforms are not separately tested.

## Export mapping (NewGit → git)

| NewGit | git | notes |
|---|---|---|
| Snapshot chain | commits via `git fast-import` | parents-before-children topological stream |
| `refs/heads/*`, `refs/tags/*` | pass through | annotated tags cannot be rebuilt (metadata was stripped at import) → lightweight |
| `refs/X` (other) | `refs/heads/X` | e.g. `refs/main` → `refs/heads/main`; `refs/namespaces/*` is an explicit skip/report exception |
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
tested U+0001 message is rejected with no ref updates. The
`non_utf8_git_commit_message_is_lossily_converted_and_flagged` fixture uses Git
plumbing to create an invalid-UTF-8 commit message (Git 2.43.0's `git commit`
porcelain normalizes such input); `cat-file` and `fast-export` retain the raw
bytes, import converts them with UTF-8 replacement characters and sets
`extras.git_message_lossy=1`, and export writes only the converted text. This
proves the tested loss is flagged, not that non-UTF-8 message bytes are
preserved; other Git versions and platforms are not established.
`quoted_utf8_git_paths_roundtrip_without_changing_names` compares
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

`octopus_merge_preserves_parent_order_and_trees_across_roundtrip` creates a
four-parent octopus merge using real Git and checks the ordered parents in
NewGit's metadata, after export to Git, and after reimport. It also compares the
merge tree's paths, modes, and blob IDs, runs Git `fsck`, and deep-verifies both
NewGit repositories. This is semantic evidence from Git 2.43.0/Linux; it does
not claim commit-object identity or broad platform/version coverage.

`sha256_git_import_export_roundtrips_semantically` creates a real Git
`--object-format=sha256` repository, confirms 64-character original IDs in the
fast-export stream, checks that imported commits retain those IDs in
`extras.git_oid`, and compares every commit's paths, modes, and blob bytes after
export. The fresh export target uses Git's default object format, so this is a
semantic repository-equivalence test, not Git object-ID preservation.

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
   representable — NewGit has no tag object type. Refs survive; metadata loss
   is listed in the report. A real SSH-signed tag is verified by Git before
   import; Git 2.43.0/Linux retains its SSH signature payload in the
   `--signed-tags=strip` fast-export stream, but NewGit discards it when it
   flattens the annotated tag. Export produces a lightweight tag. NewGit does
   not verify the tag signature; other signature formats, Git versions, and
   platforms are not established by this test.
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
9. **Symbolic refs outside `HEAD` are unsupported.** NewGit has no symbolic-ref
   representation for ordinary refs. Import detects them with `for-each-ref`,
   reports their names in `refs_skipped`, and does not import them as direct
   refs; the tested branch aliases are omitted by Git 2.43.0 `fast-export --all`.
   The pre-scan and export are separate Git commands, so concurrent source-ref
   mutation is not synchronized or covered by this guarantee.
10. **Replace-ref overlays are unsupported.** `refs/replace/*` are skipped and
    reported. Import disables Git's replacement-object substitution while
    running `fast-export`, preserving stored ordinary-branch history rather than
    silently rewriting it through an omitted replacement ref. The Git-visible
    replacement-aware view is not reproduced; this behavior is regression-tested
    for one replacement commit on Git 2.43.0/Linux.
11. **Git commit signatures are not preserved or verified.** Git 2.43.0's
    `fast-export` omits commit `gpgsig` headers. Before updating refs, import
    checks the original commit objects and lists each affected source object ID
    in `signed_commits_stripped`; export consequently creates unsigned commits.
    A real, Git-verified SSH-signed commit proves this loss/reporting behavior on
    Git 2.43.0/Linux. OpenPGP signatures, alternate Git versions, and other
    signature-header variants are not established by that fixture.
12. **Git SHA-256 object IDs are accepted for import**, but source object IDs
    are metadata only and export initializes a fresh repository using Git's
    default object format. The semantic SHA-256 import/export fixture is limited
    to Git 2.43.0/Linux; cross-version and cross-platform behavior is untested.
13. **Refs to non-commit Git objects are refused before import**, including
    lightweight or annotated tags to blobs/trees. NewGit's Git export path
    represents snapshot histories; its importer does not silently accept a ref
    that fast-export omits or that later cannot be exported. Real-Git tests cover
    blob/tree tag targets and the no-commit tag-only case on Git 2.43.0/Linux.
14. **Git namespaces are not preserved.** `refs/namespaces/*` are reported and
    omitted on import, including refs to blobs that `fast-export` skips; export
    also reports and omits namespace-shaped NewGit refs rather than flattening
    them into branches. `fast-export` may still stream commits reachable only
    through those refs, leaving unreferenced objects in NewGit, so this is not a
    confidentiality boundary. The namespace pre-scan and `fast-export` are
    separate commands; concurrent source-ref mutation is not synchronized. The
    regression is limited to Git 2.43.0/Linux.

## CLI details

Both commands support `--json` (`{ok:true,data:<report>}`), `--repo/-C`, and
emit `obs` events with `--debug`. Import reports include mapped refs, skipped
refs, stripped tag metadata, and source commit IDs whose signatures were
stripped. This signature report identifies the affected commits; it does not
preserve signature bytes or establish cryptographic validity.

Exit codes: 0 success · 2 usage/invalid source or target (not a git repo,
occupied target, submodule, non-snapshot ref, non-commit Git ref) · standard
codes otherwise.
