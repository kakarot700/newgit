# KNOWN_LIMITATIONS.md

Honest, current list. Anything not listed here that fails is a bug — report it.

## Core model
1. **UTF-8 paths only.** Non-UTF-8 filenames (legal in git/unix) are rejected
   with an explicit error, including during git import. Git import/export
   round-trips the tested C-quoted UTF-8 subset (Unicode combined with quotes
   and backslashes; `tests/git_compat.rs::quoted_utf8_git_paths_roundtrip_without_changing_names`),
   but this does not establish behavior for arbitrary path bytes, control
   characters, or every operating system. No surrogate-escape mapping yet.
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
    Annotated/signed-tag metadata and Git notes are not preserved; skipped refs
    and reported tag-metadata loss are surfaced by the import report. Symbolic
    refs outside `HEAD` are unsupported by NewGit's direct-ref model; import
    discovers and reports them because `fast-export --all` omits them. Replace
    refs and physical `refs/namespaces/*` refs are skipped and reported on
    import; export also skips namespace-shaped NewGit refs rather than mapping
    them to ordinary branches. The `for-each-ref` pre-scan seeds reports for
    every configured skipped family, including remote-tracking and notes refs
    with blob targets that Git 2.43.0/Linux `fast-export` omits; a real fixture
    verifies those reports and namespaced `ls-remote` visibility. Namespace-only
    commit history may still be
    streamed/imported as unreachable objects, so this is not a confidentiality
    boundary. The namespace pre-scan and `fast-export` are separate commands;
    concurrent source-ref mutation is not synchronized. Git commit signature headers are not
    preserved because `fast-export` omits them. Import scans original commit
    objects and reports affected source IDs in `signed_commits_stripped`, but
    does not preserve or verify the signature. One Git-verified SSH-signed
    commit confirms this behavior on Git 2.43.0/Linux; other signature types and
    Git versions are untested. Detecting signatures requires a separate
    streaming `cat-file --batch` pass over exported commit headers; its
    large-history performance impact has not been benchmarked. SHA-256 Git
    repositories are accepted and semantically round-trip in one Git 2.43.0/Linux
    fixture; export uses the destination Git's default object format and does not
    preserve source object IDs. Cross-version/platform coverage is untested.
    Import disables replacement-object substitution during `fast-export` so an omitted overlay cannot silently
    rewrite ordinary branch history. This imports stored objects, not Git's
    replacement-aware view, and is tested for one replacement commit on Git
    2.43.0/Linux. Git LFS
    service semantics are not implemented. Symbolic-ref discovery and
    `fast-export` are separate commands, so concurrent source-ref changes are
    not synchronized. Before `fast-export`, import refuses ordinary refs
    targeting non-commit objects, including tags to blobs/trees; Git 2.43.0 can
    otherwise omit lightweight non-commit refs or import annotated blob tags that
    the snapshot-only exporter cannot round-trip. Real-Git tests verify atomic
    refusal for lightweight/annotated blob/tree tags and a tag-only source on
    Git 2.43.0/Linux; nested annotated-tag chains and other versions/platforms
    are not separately tested. Non-UTF-8 paths are rejected by NewGit's UTF-8 path model
    (limitation 1), not silently converted.
11. Git smart HTTP supports upload-pack and a **narrow receive-pack slice**:
    a real Git 2.43.0/Linux loopback test covers `clone --depth=1`,
    `fetch --deepen=1`, `fetch --unshallow`, then ordinary fetch and pull.
    Shallow negotiation changes only the transfer boundary: each request still
    materializes the complete temporary Git projection, and this does not
    establish support on other Git versions or platforms. Separate real Git CLI
    tests cover write-authenticated branch creates, fast-forward
    and forced non-fast-forward updates, and deletions, plus lightweight tag creates/deletions per request,
    including an initial branch push to an empty repository. Existing tags cannot
    be retargeted (even with `--force`). Annotated pushes are refused before
    canonical import or object promotion with the tested diagnostic that NewGit
    has no Git tag-object or per-ref metadata representation; the test verifies
    refs and object inventory remain unchanged. Flattening would discard the
    tagger, message, and any signature. The server
    advertises Git's `atomic` capability: tests verify atomic multi-ref changes
    and policy rejection that leave canonical refs and object inventory
    unchanged. All accepted canonical refs share one NewGit journaled transaction;
    an ordinary request that Git accepts only in part fails at the HTTP boundary
    without committing canonical refs. Standard Git CLI tests show ordinary
    stale pushes and mismatched leases are rejected, while matching
    `--force-with-lease` and `--force` updates succeed. Each update's old Git tip
    must map to the current canonical tip and is checked again by locked CAS
    before object promotion. The receive-pack wire command carries no force flag,
    so a custom write-authenticated client can send the same non-fast-forward
    command without `--force`; explicit-force intent is not server-verifiable.
    A receive-pack `version=2` request falls back to conventional v0 framing;
    protocol-v2 push itself is not implemented. Signed pushes, receive-pack v2
    framing, Git-over-SSH, and
    GitHub/GitLab hosting features are not implemented. Every request rebuilds a temporary
    Git view from NewGit's canonical objects/refs. If a process or storage
    failure interrupts promotion of immutable objects, unreachable objects may
    remain, but the transactional ref set is not published to an incomplete
    graph. See `docs/PROTOCOL.md` and `docs/GIT_COMPATIBILITY_MATRIX.md`.

## Remote transports and Git smart HTTP
12. Transport security relies on a TLS-terminating reverse proxy; the built-in
    server speaks plain HTTP/1.1 and MUST NOT be exposed to hostile networks
    directly; TLS must be terminated by a trusted reverse proxy or traffic
    must remain on a trusted private network. See `docs/DEPLOYMENT.md`.

## Diff
13. Line diff uses Myers with a bounded edit distance (default 1024 per
    file). Beyond the cap the output falls back to a whole-file replace —
    always correct and reconstructible, but not minimal. Rename similarity
    uses a cheap prefix/suffix heuristic for candidate scoring (full Myers
    only for the chosen pair) and is capped at 1000 candidate pairs; beyond
    that only exact-content renames are detected.

## Merge
14. Merge base selection picks the maximal common ancestor by
    (timestamp, oid) — criss-cross merges with several merge bases do not
    get git's "recursive" virtual-base treatment. Rename tracking in merges
    is exact-content only (no similarity-based rename detection during
    merge; the diff engine has it for display). Conflict resolution is
    manual: NewGit stores the marker-annotated blob and expects a human or
    agent to edit + snapshot it (no rerere-style reuse yet).

## Maintenance (verify / gc)
16. `gc` holds the global transaction lock for its whole run — writers block
    until it finishes. Bounded by repo size (mark + sweep are linear in
    objects); at very large scale gc would need incremental/partial modes.
17. The gc grace window is a fixed 24 h constant (protects concurrent
    in-flight object writes); it is not repo-configurable yet. `--force-now`
    disables it (tests, single-user repos).
18. `verify` checks structural integrity (digests, canonical form, link
    existence + types, ref/chain/reflog/workspace grammar) but NOT semantic
    history rules: past chain transitions are not re-validated against the
    state machines, timestamp monotonicity is not enforced, and there is no
    cross-check that reflog actor ids exist. Also no repair mode by design
    (D-014) — messages point at the fixing command.

## Process
15. Benchmarks are measured on modest hardware (2 vCPU / 2 GB) — relative
    numbers, not marketing numbers (docs/BENCHMARKS.md).
19. The chaos suite is deterministic (6 fixed seeds, 14–25 ops each) — a
    regression net, not an open-ended fuzzer; seeds are added when bugs are
    found (regression capture). Fuzz-like parser tests are likewise seeded
    firehoses, not coverage-guided.
20. `import-git` caches per-commit tree state (needed for incremental
    fast-export streams and parent inheritance): RAM grows with total
    distinct paths × commits-in-flight, not with blob bytes (blobs stream).
    Very large monorepo histories can be memory-hungry; a streaming
    `--full-tree`-only mode could trade CPU for RAM later.
21. Git **annotated/signed tags** import as plain refs: tagger, tag message,
    and signature are not preserved; the tag ref is listed in the import
    report. NewGit has no tag object type; export rebuilds lightweight tags
    only. A Git-verified SSH-signed tag fixture shows that Git 2.43.0/Linux
    retains SSH signature bytes in `fast-export --signed-tags=strip` tag data;
    NewGit discards them when flattening the tag. NewGit does not verify tag
    signatures. OpenPGP/X.509 and other Git versions/platforms are untested.
22. Git **submodules (gitlinks, mode 160000) are refused**: the whole import
    aborts atomically with an actionable error (zero refs move). No partial
    or faked submodule support.
23. Export loses **sub-second timestamp precision** (git stores whole
    seconds); import is exact at git's own precision.
24. **No efficient direct NewGit-object/Git-remote bridge**: import/export
    conversion is whole-history, and both smart-HTTP adapters rematerialize
    the complete Git view for each discovery/request. Git's ordinary
    upload-pack negotiation works at the wire/transfer layer, but does not
    avoid that server-side export work.
25. Non-UTF-8 git commit messages become lossy-converted and are flagged
    (`extras.git_message_lossy`); a real Git plumbing fixture verifies the raw
    source bytes, fast-export payload, replacement text, marker, and lossy export
    (`tests/git_compat.rs::non_utf8_git_commit_message_is_lossily_converted_and_flagged`).
    Valid UTF-8 message payloads are preserved
    byte-for-byte for the tested cases (including leading/trailing whitespace,
    CRLF, missing final LF, and empty messages; see
    `tests/git_compat.rs::commit_message_roundtrip_preserves_exact_utf8_bytes`).
    Import refuses commit messages containing C0 control characters other than
    LF/CR/TAB or DEL because the NewGit text model does not permit them; the
    U+0001 case is regression-tested and confirms no refs move. Git ref names
    violating NewGit's stricter ref grammar are skipped and reported. Export
    refuses distinct NewGit ref names that map to the same Git ref (for
    example, `main` and `refs/main`) rather than silently overwriting one.
26. Git import/export requires a **system git ≥ ~2.20** on PATH. The live
    smart-HTTP adapter also requires system Git; its recorded CLI test uses
    Git 2.43.0/Linux, and no minimum version/platform matrix is established.
    SHA-256 import additionally requires a Git build with SHA-256 repository
    support. Native NewGit operations work without Git installed.
27. Remote protocol v1 is **plain HTTP** — no TLS, no request signing.
    Deploy behind a TLS-terminating reverse proxy (documented); tokens
    travel as bearer credentials, so an unencrypted network exposes them.
    Loopback-only default bind reflects this. The CLI client accepts
    `http://host[:port]` URLs ONLY — `validate_url` rejects `https://`
    outright rather than pretending to verify a certificate it cannot;
    cross-host CLI usage goes through a tunnel (docs/DEPLOYMENT.md §2.3).
    The TLS proxy terminates for browser/API clients; the built-in server
    never sees encrypted traffic.
28. Wire encoding is JSON + base64 (~33% payload inflation, JSON parse
    cost). Fine at v1 scale; binary framing is a v2 candidate. No
    keep-alive/pipelining: one request per connection.
29. One server process serves ONE repository (the one it was started in);
    no multi-repo routing or URL-path routing. Git smart HTTP is exposed at
    the server root for upload-pack and the documented bounded receive-pack
    branch/lightweight-tag create-delete slice; other push policies remain refused.
30. `push` non-fast-forward checking and negotiation walk object closures
    client-side (RAM/CPU proportional to reachable history, like verify/gc);
    every connection reopens the repo (recovery scan). Acceptable at v1
    scale; documented for huge repos.
31. No transfer resume: an interrupted push/pull restarts the batch stream
    (objects already stored are skipped via negotiation, so retries are
    cheap but not free).
32. NewGit's own `pull` never touches HEAD or workspaces (fetch semantics by
    design); there is no remote-side merge — integrate locally and explicitly.
    Separately, standard Git clients can read from the server over smart HTTP
    and can use the bounded branch-push slice described above; each HTTP
    request rebuilds the temporary Git view.
33. The Web UI is READ-ONLY by design (iteration 10): it explores
    goals/changes/evidence/proposals/history/diffs/audit but performs no
    mutations — writes stay in the CLI/MCP/API flow where authz and audit
    are uniform. Rationale: a browser write path would need its own
    CSRF/session story for zero gain (agents already have first-class
    APIs). A write-capable UI is a post-1.0 candidate.
34. The UI is one hand-written HTML file (~60 KB, no framework, no build
    step). That is deliberate (supply chain: zero JS dependencies; works
    offline) but it means no component ecosystem, no virtualized lists —
    history/object views cap at 200 rows client-side.
35. `/v1/diff` unified content is capped at 100 modified non-binary files
    per response (`UNIFIED_CAP`); the machine-readable `diff.files[]` list
    is never capped — clients needing more unified text diff pairwise.
36. The MCP server has no authentication of its own: whoever can write to
    its stdin has the privileges of the spawning process (same trust model
    as the CLI). Run one `newgit mcp` per agent under that agent's OS user.
    No MCP resources/prompts primitives (tools only); protocol version
    2024-11-05; batches unsupported.
37. Build reproducibility is verified **same-host, same-toolchain** (two
    clean release builds → bit-identical binary, sha256 recorded in
    THREAT_MODEL §G). Cross-host byte-equality is NOT claimed: it would
    require pinned rustc AND identical source paths AND a controlled
    environment (nix/buildinfo) — checksums published per release are the
    verification story instead.
38. `cargo audit` / `cargo deny` results (zero findings, all four checks
    ok — local preflight 2026-10-04) are SNAPSHOTS against the advisory DB at
    that time; advisories are published continuously. GitHub-hosted CI now
    reruns the SBOM, advisory, ban, license, and source checks on pushes. CI
    run 37182199247 on commit `afa94c4` passed those gates plus format, lint,
    debug/release tests, a second clean release build, package creation, and
    checksum verification. CodeQL run 37182199239 on that commit completed
    successfully. These results apply to the tested commit/platform; see the
    Actions page for current status and do not infer future advisory coverage.
