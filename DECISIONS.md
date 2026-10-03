# DECISIONS.md — architectural decision log

Format: context → decision → rationale → consequences. Newest first.

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
`serde`+`serde_json`, `thiserror`. Dev deps: `proptest`, `tempfile`. No CLI
framework (hand-rolled parser), no HTTP framework (hand-rolled HTTP/1.1 in
iteration 9), no async runtime.
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
