# DECISIONS.md — architectural decision log

Format: context → decision → rationale → consequences. Newest first.

## D-019 · Git smart HTTP through a separate read-only upload-pack adapter (2026-10-04)
**Context:** The project needs ordinary Git clone/fetch/pull interoperability
without making NewGit's canonical SHA-256 object store depend on Git's pack
format or treating a compatibility projection as core storage.
**Decision:** Keep the NewGit JSON remote and object model authoritative. Add a
separate HTTP adapter for smart-HTTP upload-pack discovery and stateless RPC;
materialize a private temporary Git-format view from NewGit refs/objects per
request and delegate packet-line negotiation and pack production to the
installed `git upload-pack`. Isolate Git configuration/repository environment,
pass an explicit empty private template directory so host hooks are not copied,
use an OS-secure temporary directory, cap buffered bodies with `--max-body`,
and enforce a 120-second wall-clock deadline with process-tree termination.
Do not implement receive-pack/push, Git SSH, or GitHub/GitLab hosting behavior
in this slice.
**Consequences:** A real Git CLI can clone/fetch/pull/`ls-remote` from the
single-repository server (tested on Git 2.43.0/Linux; see
`tests/git_remote_e2e.rs`), while NewGit remains the source of truth. Full Git
view materialization repeats for every HTTP request, temporary disk and peak
RAM have no separate quota, concurrent-ref snapshot consistency is not
guaranteed, and Git writes remain unsupported. `tempfile` is promoted to a
runtime dependency solely for private temporary view allocation.

## D-010 · All ref mutations go through the transaction engine (Iteration 2)
**Context:** Single-ref updates could bypass journaling, creating two code
paths with different crash semantics.
**Decision:** Even one-ref updates execute as 1-op WAL transactions.
**Consequences:** Uniform recovery; slight overhead (journal write per ref
update) — acceptable, measurable in iteration 11 benchmarks.

## D-009 · Commit point = journal fsync; forward recovery (Iteration 2)
**Context:** A crash between journal write and application leaves intent
durable but effects missing. Undo-style recovery would need old-value
journals; redo needs only final state.
**Decision:** Journals record final state only. Once the RUNNING journal is
durable, the transaction WILL complete (recovery redoes application).
Application is idempotent; reflog lines carry the txn id and readers dedupe,
so redo re-appends are invisible. Corrupt journals are quarantined
(`*.journal.corrupt`), never applied, never deleted silently.
**Consequences:** Simple, testable all-or-nothing semantics; `execute()`
returns error before the journal only on CAS failure (no effects at all).

## D-008 · Lock reclamation by holder liveness (Iteration 2)
**Context:** `abort()`/SIGKILL never runs destructors, so O_EXCL lock files
outlive their holders and blocked recovery for the full stale timeout (5 min)
in tests — and would in production crashes too.
**Decision:** Lock files record `pid`/`time`. On contention, reclaim iff the
recorded pid is provably dead (Linux `/proc/<pid>` probe) or age exceeds the
stale timeout. Non-Linux platforms fall back to the timeout (conservative).
**Consequences:** Fast crash recovery; pid-reuse can only delay reclamation
(never steal a live lock). Tested: `dead_holder_lock_is_reclaimed`,
`live_holder_lock_is_not_stolen`.

## D-007 · Git interoperability via fast-export/fast-import (Iteration 1)
**Context:** Full git object-format compatibility (sha1 packs, deltas) would
require a large dependency (gitoxide) or months of work.
**Decision:** Import by parsing `git fast-export` streams; export by emitting
`git fast-import` streams, driving the system `git` binary. Detect git at
runtime; degrade with a clear error when absent.
**Rationale:** fast-export/import are stable, documented git interfaces;
zero new dependencies; honest semantics (full history, authors, messages,
modes, merge parents).
**Consequences:** Requires system git for interop commands (documented in
KNOWN_LIMITATIONS.md). Non-UTF-8 git paths rejected with explicit error
(NewGit paths are UTF-8 by protocol). Tag signatures are not carried over.

## D-006 · No staging area (Iteration 1)
**Context:** Git's index/staging split is a major source of user and agent
confusion and complicates concurrent-workspace semantics.
**Decision:** A workspace holds live file state; `newgit snapshot` captures
the entire workspace atomically. Partial capture is done by using multiple
workspaces, not a staging area.
**Rationale:** Matches the product model (workspace = one actor's experiment).
Simplifies crash safety (index is only a status cache, never semantics).
**Consequences:** No `git add -p` equivalent; documented in migration guide.

## D-005 · Evidence honesty flags (Iteration 1)
**Context:** Agents can fabricate "tests passed" claims.
**Decision:** `Evidence.deterministic: bool` and `Evaluation.ai_generated:
bool` are protocol fields. Deterministic evidence is meant to be produced by
a runner that executes the tool itself (CLI `newgit evidence run`), claimed
evidence by any actor. UI/eval layers must render them distinctly. The core
never upgrades a claim to a verified fact.
**Consequences:** Policy layer (proposals/integration) can require
deterministic evidence for certain kinds.

## D-004 · Canonical encodings are a frozen protocol (Iteration 1)
**Context:** Object identity depends on byte-level canonical form.
**Decision:** docs/STORAGE_FORMAT.md is normative. Set-like fields are stored
strictly ascending + unique; strings length-prefixed UTF-8; varints minimal
(non-minimal encodings rejected at decode); no trailing bytes anywhere.
Envelope: `NGOB` magic + version + type tag + LE lengths + zlib(level 6) +
trailing SHA-256. Changing any of this requires a new format version.
**Consequences:** Deterministic ids; corruption always detectable; decoders
must never panic (tested + fuzzed).

## D-003 · SHA-256 object ids (Iteration 1)
**Context:** Git uses SHA-1 (collision-attacked); SHA-256 is the conservative
choice and matches git's own SHA-256 mode direction.
**Decision:** All NewGit ids are SHA-256 over canonical bytes, displayed as
64-char lowercase hex.
**Consequences:** No cross-hash interop with git ids; import keeps a
sha1↔nid map file (documented).

## D-002 · Minimal dependency set (Iteration 1)
**Context:** Supply-chain security + build speed on 2 CPUs.
**Decision:** Runtime deps: `sha2`, `flate2` (pure-Rust `rust_backend`, no C),
`serde`+`serde_json`, `thiserror`, and `tempfile` (private temporary Git views;
added later by D-019). Dev deps: `proptest`. No CLI framework (hand-rolled
parser), no HTTP framework (hand-rolled HTTP/1.1 in iteration 9), no async
runtime.
**Rationale:** Every dependency is small, boring, and justified; fewer CVE
surfaces; reproducible builds easier.
**Consequences:** We own more code (parser, HTTP) — mitigated by dedicated
fuzz/property tests for each hand-rolled parser.

## D-001 · Rust, single crate + single binary (Iteration 1)
**Context:** Systems-language VCS engine; safety + performance + portability.
**Decision:** Rust stable (pinned 1.99.0), `#![forbid(unsafe_code)]`, one lib
crate + one `newgit` binary (workspace split deferred until build times hurt).
**Consequences:** No unsafe; compile-time enforcement of memory safety;
single artifact to ship.

## D-011 · Diff design: bounded Myers + racily-clean index guard (Iteration 4)

**Context.** The diff engine needs (a) exact, deterministic line diffs,
(b) memory safety on hostile/pathological inputs, (c) an index cache that
can never report stale content as current.

**Decision.**
1. Line diff = greedy Myers O(ND) with common prefix/suffix trimming and a
   bounded edit distance (`max_edit_distance`, default 1024 per file). When
   the bound is exceeded we emit a whole-file *replace* (still exact and
   reconstructible, just not minimal) and flag `edit_distance_capped` in
   JSON. Full-trace memory is therefore ≤ ~8 MiB per file. Hirschberg
   linear-space refinement is deferred until benchmarks justify it
   (KNOWN_LIMITATIONS #13).
2. Rename detection runs in two deterministic stages: exact (mode+oid)
   first, then similarity ≥50% via a cheap common-prefix/suffix line ratio
   (full Myers only for the *chosen* pair, at render time). Candidate
   scoring is skipped entirely when deleted×added pairs > `rename_pair_cap`
   (1000). Greedy assignment sorted by (score DESC, old path, new path) —
   same input ⇒ same output, no hash-map iteration order dependence.
3. Binary = NUL byte in first 8000 bytes (git-compatible rule). Binary
   files diff at metadata level only.
4. **Racily-clean guard**: the index cache is trusted only when
   size+mtime match AND file mtime < index-file mtime (git's rule). A file
   modified within the same timestamp tick as the index write would
   otherwise be invisible to status/snapshot; the symlink-diff test caught
   this in practice. Unit-tested in `repo::index::tests`.
5. `capture_tree(save_index)` factors the walk→hash→tree pipeline shared by
   snapshot (index-writing) and diff (read-only).

**Consequences.** Deterministic, memory-bounded diffs; correctness of
status/diff under coarse filesystem clocks; slight extra hashing for files
touched in the same tick as the last snapshot (acceptable, git-identical).

## D-012 · Merge semantics: atomic integrate, honest conflicts, no history rewriting (Iteration 5)

**Context.** Multiple actors (human + agents) must be able to work
concurrently and combine results without silent data loss or fake success.

**Decision.**
1. `integrate` is all-or-nothing at the position level: conflicts abort with
   exit 5 BEFORE any ref move or file change. (Merged-with-markers conflict
   blobs may be stored for inspection — content-addressed, unreachable,
   GC-able; they never affect semantics.)
2. Merge base = best common ancestor by (timestamp DESC, oid DESC) traversal;
   criss-cross histories with multiple maximal common ancestors pick one
   deterministically instead of git's recursive base-merge (documented
   limitation #15).
3. `parents` stays a canonically sorted set (D-004); merge ROLES live in
   `extras.merge_ours/merge_theirs` so rollback can undo "to our side"
   unambiguously.
4. Rollback never rewrites history: it creates a new snapshot with the old
   tree. Auditable, revertable, crash-safe like any snapshot.
5. Fast-forward moves the ref to the existing snapshot object (no empty
   merge commit), matching the "position" model.
6. Workspace file checkout after the durable commit is NOT journaled; a
   crash in between leaves the position ahead of the files — `status`
   reports it honestly and `newgit checkout` repairs it (tested with fault
   injection at integ:after_txn).
7. Content merge: diff3 over Myers anchors; symlinks and binaries never
   auto-merge; add/add uses an empty virtual base (git-compatible outcome);
   modify/delete and rename/delete always conflict. Exact-content rename
   tracking only (no similarity renames in merge v1).
8. Concurrent integrates on one workspace serialize on the workspace lock
   (lock acquired before reading the position); CAS remains the final guard.

**Consequences.** No silent conflict resolution, no lost work, deterministic
outputs, git-familiar semantics with explicitly documented deviations.

## D-013 · Mutable entities as CAS-guarded version chains (Iteration 6)

**Context.** Goals, Changes, and Proposals have lifecycles (status/state
transitions, growing evidence/approval lists), but the object store is
immutable and content-addressed.

**Decision.**
1. Entity identity = oid of its FIRST version (the chain root). The ref
   `chains/<root-hex>` points at the latest version and moves only via the
   txn engine with `Cas::Exactly(previous head)` — concurrent updates race
   safely (loser gets exit 4). Ref namespace added to the system grammar.
2. Every non-root version carries `extras["prev"] = <previous version>` —
   the full audit trail is reconstructible from the head alone; no version
   is ever edited or deleted in place.
3. State machines are validated at transition time (goal: open ⇄
   in_progress → achieved/abandoned with explicit reopen paths; change:
   draft→tested→proposed→integrated, any→abandoned; proposal:
   open→approved/rejected/closed, approved→integrated ONLY via
   `proposal integrate`).
4. Honesty gates: `change → tested` requires ≥1 attached Evidence object;
   `proposal integrate` requires state=approved; Evidence.deterministic is
   set true only by `evidence record` (runner-captured exit/output/
   duration); `evaluation from-evidence` aggregation is deterministic and
   always ai_generated=false; AI opinions must pass `--ai`.
5. `proposal integrate` commits position ref + proposal chain + change
   chain in ONE transaction (all-or-nothing); the merge itself is computed
   before the txn so conflicts abort with zero writes. Post-commit file
   checkout follows the integrate contract (status-visible, repairable via
   `newgit checkout`).
6. Evidence/Evaluation objects are append-only facts with NO chains —
   they are immutable observations, never edited.

**Consequences.** Full auditability (who changed a status, when, from what
to what — via chain + reflogs), safe concurrency, and machine-checkable
honesty rules without a database.

---

## D-014 · verify + gc: read-only fsck, strictly non-destructive gc (Iteration 7)

**Decision.** `newgit verify` is strictly read-only — it classifies and
reports (stable machine codes, `error`/`warning` severities), never repairs;
messages name the next command to run. Error ⇒ exit 3; warnings ⇒ exit 0
(debris that the engine tolerates by design: quarantined objects, orphan
workspace dirs, stale locks, temp-file debris, corrupt index caches).
Crash-debris classes are warnings, not corruption: a workspace whose files/
vanished between the create-txn and the (unjournaled) checkout is repairable
by `checkout` (same policy as D-012's post-commit checkout).

`newgit gc` is mark-and-sweep with four safety rules:
1. **Roots** = HEAD + every ref value (refs/, workspaces/, chains/) + every
   reflog OLD/NEW oid (audit history is never collected) + workspace
   base_oids. Mark follows every link type **including `extras.prev`** chain
   links (older Goal/Change/Proposal versions are reachable audit trail).
2. The **global txn lock is held for the whole run** (after a recovery pass),
   so refs cannot move between root collection and sweep. Object writes by
   concurrent processes don't take the txn lock — they are protected by a
   **24 h mtime grace window** (`--force-now` overrides; tests/single-user).
3. **Never destructive about anomalies**: only objects that decode cleanly
   AND match their file name are deleted. Unreadable/corrupt/misfiled files
   are kept and counted (`kept_corrupt`), quarantine (`*.corrupt`) is never
   touched, non-object debris is left for humans. A damaged repo does not
   block gc — missing links are counted (`missing_links`) and reported.
4. Deletion needs **no journaling**: removing provably-unreachable,
   fully-decodable objects cannot destroy reachable state; shard dirs are
   pruned when empty and fsynced.

`reachable()` is lenient by construction (missing objects are collected, not
fatal) so gc stays usable on damaged repos; `verify` remains the tool that
reports damage. Deep verify (`--deep`) re-encodes every object and compares
bytes (canonical-form drift detection) and walks every link (existence +
type).

**Consequences.** Operators get git-fsck/git-gc-equivalent guarantees with
stronger forensic preservation (corrupt data is never auto-deleted). gc
blocks writers for its duration — acceptable at current scale, documented in
KNOWN_LIMITATIONS #16.

## D-015 · Journal checkpointing: delete on success, recovery deletes terminal states (Iteration 7)

**Decision.** A successful transaction deletes its own journal after the
COMPLETE state is durably written (still under the txn lock). Recovery
deletes any journal found in a terminal state (COMPLETE/RECOVERED) and, after
redoing a RUNNING journal and marking it RECOVERED, deletes it too.

**Context.** Discovered by the iteration-7 e2e test: successful txns left
COMPLETE journals forever — unbounded txn-dir growth, every open/recover
rescanned dead journals, and `verify` correctly-but-noisily warned about
"unrecovered" journals that were in fact complete. The reflog (which carries
the txn id + message per ref update) is the durable audit trail; journal
content is redundant after apply.

**Crash-safety argument.** At any crash point the journal is either absent
(nothing committed), RUNNING (redo — idempotent apply + reflog txn-id dedup,
D-009), COMPLETE (durable + applied ⇒ safe to delete), or RECOVERED (redo
already durable ⇒ safe to delete). Deletion after the COMPLETE fsync can
only lose a redundant file.

**Consequences.** `txn/` stays bounded (journals exist only while live or
awaiting recovery); recovery scans shrink to O(pending); `verify`'s
journal_pending warning becomes meaningful (concurrent activity or genuinely
unrecovered crash). Tests upgraded to the stronger invariant: after
recovery, zero journal files remain.

## D-016 · Git interop specifics: stream via system git, one-transaction import, honest lossy points (Iteration 8)

Realizes D-007. Chosen after implementing and testing both directions:

- **Parser/emitter are hand-rolled, total, and dependency-free**; git is
  invoked only for its own `fast-export`/`fast-import` streams (never for
  network ops). If git is absent, only these two commands fail.
- **Import atomicity**: objects (idempotent, content-addressed) are written
  first; ALL refs + HEAD move in ONE txn. Any error (incl. submodule
  gitlink `160000`) ⇒ zero refs move. Rationale: a half-imported history is
  worse than none — agents must be able to retry safely.
- **Lossless-first-parent trick**: NewGit `Snapshot.parents` is a sorted set
  (frozen protocol), but git merges are ordered. Import stores
  `extras.git_parents_ordered` + `extras.git_sha1` (`--show-original-ids`);
  export replays exact `from`/`merge` order ⇒ round-trip lineage equality
  is tested, not hoped for.
- **Committer vs author**: git has both; NewGit snapshots have one Actor.
  Author becomes the Actor; committer lands in `git_committer_*` extras ONLY
  when it differs, and export restores it. Identity/timestamp multiset
  equality across round-trip is a test (`%an|%ae|%cn|%ce|%at|%ct|%s`).
- **Documented lossy points, loudly reported** (never silent): annotated/
  signed tag metadata stripped (no tag object type in NewGit) and listed in
  the import report; ms→s precision loss on export; non-UTF-8 messages
  lossy + flagged in extras; `refs/X` (non-heads/tags) exports as
  `refs/heads/X`.
- **Skipped namespaces** both directions (remotes/notes/replace/stash/
  bisect/worktree on import; workspaces//chains/ on export) — mirroring
  would leak internal state into git or duplicate remote-tracking refs.
- Rejected: gitoxide/libgit2 dependency (supply chain + weight, violates
  D-002); reimplementing SHA-1 object format (huge surface, zero benefit);
  silent best-effort submodule import (faking).

## D-017 · Remote protocol v1: JSON/HTTP on std, have-invariant negotiation, client-side FF guard (Iteration 9)

- **Hand-rolled HTTP/1.1 + JSON over std sockets** (no hyper/axum/tokio):
  the protocol needs exactly 9 endpoints with Content-Length framing; a
  total 300-line parser is auditable and fuzzable, and keeps D-002 (tiny
  dependency budget) intact. Cost: no keep-alive, no chunked, no HTTP/2 —
  accepted (KL #28).
- **Objects travel as self-verifying envelopes, re-validated on receipt**
  (digest + id + canonical form + dependency presence). Neither side trusts
  the other's oid claims; a corrupt or malicious peer cannot inject
  dangling/corrupt objects (tests: dependency_order, roundtrip oid equality).
- **Have-invariant**: remote `objects/put` refuses objects whose links are
  not yet stored ⇒ every store is link-closed ⇒ "peer has X" safely prunes
  negotiation at X without walking X's closure. This is what makes
  incremental push cheap (re-push = 0 objects) and divergence-safe (pruned
  oids need not exist locally).
- **Push = client-side non-fast-forward guard + server-side CAS txn**:
  the guard (server tip must be a local ancestor) gives git semantics
  (stale clones cannot clobber; exit 5 with "pull first"); the CAS
  `exactly(observed)` protects the observe→update window against
  concurrent pushers (one winner, exit 4). `--force` is explicit and
  audit-logged; overwritten objects are never destroyed (gc decides).
- **Pull = fetch semantics** (refs only, HEAD/workspaces untouched):
  NewGit's model integrates explicitly (workspaces, proposals, integrate);
  an implicit merge on pull would bypass the evidence/proposal flow.
- **Auth: random bearer tokens, SHA-256 at rest, roles read<write<admin.**
  No user database, no OAuth, no TLS in v1 (reverse proxy does TLS —
  zero-rupee, self-hostable). Invalid token ≠ anonymous (tested). Audit log
  is append-only, best-effort (availability first), secrets-free.
- Rejected: git smart-HTTP compatibility (huge surface, iteration 8 covers
  git interop via files); libp2p/custom TCP protocol (debuggability, proxy
  compatibility); async runtime (concurrency needs are modest: thread-per-
  connection with a bounded pool passed the concurrent-push tests).

## D-018 · Web UI = embedded data-free static shell; MCP = thin stdio wrapper over CLI dispatch; agent read endpoints on protocol v1 (Iteration 10)

- **Single-file UI, `include_str!`, zero JS dependencies.** No React, no
  build step, no CDN: the supply-chain surface stays exactly what D-002
  allows (nothing new), the binary ships the UI, and it works offline in
  sandboxed browsers. The file is hand-written vanilla JS (~60 KB) with a
  strict rendering discipline: `textContent` only, `innerHTML` never —
  asserted by a unit test against the shipped bytes, so XSS-safety is a
  CI-enforced invariant, not a convention.
- **UI is read-only by design (KL #33).** Serving `/` without auth is safe
  ONLY because the HTML carries zero repository data; every byte of data
  flows through the role-gated `/v1/*` API with the user's own bearer
  token. A browser write path would require a second authz/CSRF story for
  no gain — agents already have CLI/MCP/HTTP write paths. Bearer-header
  auth (never cookies) makes CSRF structurally impossible.
- **New read endpoints (`/v1/object`, `/v1/diff`, `/v1/goals|changes|
  proposals`) instead of client-side assembly.** Agents/UIs shouldn't have
  to know oid→shape mapping or walk trees by hand: `object` returns kind +
  links + data (blobs as b64+size — serde's `Vec<u8>` would be a giant
  number array); `diff` reuses `resolve_tree` + the CLI's OWN rendering
  (`render::file_header`/`render_content`) so wire unified text and CLI
  output cannot drift; listings reuse `workflow::list_entities` with a
  server-side `?goal=` filter. All additive to v1 (no version bump).
- **MCP = 13 thin tools over `cli::call_json`, NOT a parallel
  implementation.** One dispatch code path ⇒ identical validation, limits,
  honesty gates, and error categories everywhere; the MCP layer cannot
  drift from the CLI. JSON-RPC 2.0 on stdio, newline-delimited, std-only
  (serde_json already in budget): initialize/ping/tools list/call; tool
  errors are `isError:true` content carrying the standard envelope;
  protocol errors use proper -327xx/-326xx codes. No auth beyond process
  privileges (same trust model as the CLI, KL #36) — one server per agent
  user.
- Rejected: WASM/SPA framework UI (dependency sprawl, build complexity,
  supply chain); GraphQL/gRPC agent API (new deps, new surface — JSON/HTTP
  v1 already normative); MCP over HTTP+SSE (port exposure, auth story —
  stdio is the safe default); UI write operations (see above).
