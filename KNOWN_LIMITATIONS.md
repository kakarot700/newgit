# KNOWN_LIMITATIONS.md

Honest, current list. Anything not listed here that fails is a bug — report it.

## Core model
1. **UTF-8 paths only.** Non-UTF-8 filenames (legal in git/unix) are rejected
   with an explicit error, including during git import. No surrogate-escape
   mapping yet.
2. **No staging area.** Snapshots capture whole workspaces (DECISIONS D-006).
3. **Symlinks are stored, never followed.** A symlink is a blob containing its
   target string; checkout recreates the link (unix). No submodule/subrepo
   concept in v1.
4. **No object signing yet.** Actor pubkeys are metadata; signature
   verification is roadmap (SECURITY_MODEL §2).
5. **Timestamps are wall-clock claims**, not trusted ordering. History order
   is defined by parent edges, not times.
6. **Merge fan-in ≤ 64 parents** (protocol limit, deliberate).

## Storage / performance
7. One file per object (no packfiles yet). Large repos (millions of objects)
   will be inode-heavy; packfile-style bundling is a post-v1 optimization
   gated on benchmark evidence (iteration 11).
8. zlib level 6 fixed; no delta compression between objects.
9. Status/diff are O(worktree) with an index cache (racily-clean mtime guard
   included); `.newgitignore` implements a documented gitignore *subset*
   (no `[]` character-class negation corner cases beyond `[!a-z]`, no
   backslash escapes).

## Interop
10. Git interop requires the **system git binary** (fast-export/fast-import).
    Tag signatures, git notes, LFS pointers, and non-UTF-8 paths are not
    carried across; import records what it skipped in `extras`.
11. No GitHub/GitLab protocol compatibility (smart HTTP) — NewGit speaks its
    own documented protocol (iteration 9).

## Remote (arrives iteration 9)
12. Transport security relies on a TLS-terminating reverse proxy; the built-in
    server speaks plain HTTP/1.1 and MUST NOT be exposed to hostile networks
    directly until TLS support or proxy setup is documented per deployment.

## Diff (arrives iteration 4)
13. Line diff uses Myers with a bounded edit distance (default 1024 per
    file). Beyond the cap the output falls back to a whole-file replace —
    always correct and reconstructible, but not minimal. Rename similarity
    uses a cheap prefix/suffix heuristic for candidate scoring (full Myers
    only for the chosen pair) and is capped at 1000 candidate pairs; beyond
    that only exact-content renames are detected.

## Process
14. Benchmarks are measured on modest hardware (2 vCPU / 2 GB) — relative
    numbers, not marketing numbers (docs/BENCHMARKS.md).
