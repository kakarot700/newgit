# ARCHITECTURE.md — NewGit

## 1. Product model

NewGit is an **agent-native version-control system**. Its primitives are
designed for software produced collaboratively by humans and AI agents:

```
                 GOAL  ("Add OAuth authentication")
                   │
     ┌─────────────┼─────────────┐
 WORKSPACE A   WORKSPACE B   WORKSPACE C        (isolated, concurrent)
     │             │             │
 CHANGE A      CHANGE B      CHANGE C           (alternative implementations)
     │             │             │
 EVIDENCE…     EVIDENCE…     EVIDENCE…          (tests, scans, benchmarks,
     │             │             │              reviews — honest flags)
     └──────┬──────┴──────┬──────┘
       EVALUATION    EVALUATION                 (aggregated judgements)
             │             │
        PROPOSAL      PROPOSAL                  (reviewable integration asks)
             └────┬────────┘
          INTEGRATION (merge)                   (atomic, journaled)
                  │
              SNAPSHOT                          (immutable project state,
                  │                              permanent history)
```

* **Snapshot** — immutable project state: parents, root tree, author (Actor),
  timestamp, message, optional workspace/change/goal links.
* **Change** — first-class delta: base snapshot → result snapshot, author,
  goal link, status, evidence links. Multiple Changes may address one Goal
  (alternatives), which the model records permanently.
* **Workspace** — an isolated checkout + mutable state where one actor works;
  cheap to create, compare, checkpoint, discard.
* **Actor** — human/agent/process identity metadata (optionally with pubkey).
  Display metadata is *never* treated as authenticated identity.
* **Evidence** — a recorded observation about a Change (unit tests, security
  scans, benchmarks, reviews…). Carries `deterministic` honesty flag.
* **Evaluation** — aggregated judgement over dimensions; `ai_generated` flag
  keeps AI opinions distinguishable from machine facts.
* **Proposal** — an integration request bundling change + rationale +
  evidence + approvals + state machine.

AI is an **extension, never a dependency**: every operation above works
without any AI involvement.

## 2. Layering (bottom → top)

```
┌────────────────────────────────────────────────────────┐
│ CLI (hand-rolled parser, --json, stable exit codes)    │
│ Web UI (embedded single file, `newgit ui`)  MCP (stdio)│
├────────────────────────────────────────────────────────┤
│ NewGit remote v1 (HTTP/1.1 + JSON, bearer auth,        │
│ object negotiation/batching, refs CAS, audit log)      │
│ Separate Git smart-HTTP adapter (read-only upload-pack)│
│ isolated temporary Git projection; NewGit stays canonical│
├────────────────────────────────────────────────────────┤
│ Ops: snapshot/status/history/diff/merge/integrate/     │
│ rollback/workspaces/goals/changes/evidence/proposals/  │
│ verify/gc/import-git/export-git                        │
├────────────────────────────────────────────────────────┤
│ Engine: diff (Myers), merge (3-way tree + diff3),      │
│ workspaces+index, transactions (WAL journal), refs,    │
│ object store (atomic, verified), Git import/export     │
├────────────────────────────────────────────────────────┤
│ Object model: 9 canonical types + NGOB envelope,       │
│ SHA-256 identity, strict decoders (never panic)        │
├────────────────────────────────────────────────────────┤
│ util: hex, varint, fsx (atomic write/locks/path safety)│
│ process deadlines/tree-kill; fault injection           │
└────────────────────────────────────────────────────────┘
```

Dependency policy: runtime deps are `sha2`, `flate2` (pure-Rust backend),
`serde`/`serde_json`, `thiserror`, and `tempfile` for securely allocated
per-request Git views — see DECISIONS.md D-002 and D-019. `forbid(unsafe_code)`.

## 3. Storage & durability

Spec: **docs/STORAGE_FORMAT.md** (normative).

* Content-addressed objects, immutable, self-verifying (trailing SHA-256).
* Writes: temp → fsync → rename → fsync(dir). Readers verify digest + id.
* Refs: small files updated under `O_EXCL` locks with compare-and-swap.
* Transactions: write-ahead journal with **idempotent redo recovery**;
  multi-ref updates are all-or-nothing even if the process dies mid-flight.
* Index: pure cache; deleting it is always safe (invariant).
* GC: mark from every registry root (refs, reflogs, active workspaces,
  goals/changes/proposals registries), grace period before deletion, runs
  under lock, interruption-safe.

## 4. Concurrency model

* Object writes: lock-free (content-addressed ⇒ idempotent).
* Ref updates: per-ref lock + CAS (`expect-old` → new), global txn lock for
  multi-ref operations; retry loops surface `RACE` exit code when exhausted.
* Workspaces: per-workspace lock; two actors never share one workspace.
* Remote: server serializes ref updates through the same txn machinery.

Each Git smart-HTTP request builds a fresh private projection. Before it reads
refs, `HEAD`, history, or reachable objects, the builder acquires the global
transaction/GC lock and runs journal recovery while holding it; the lock remains
held through the complete NewGit-to-Git export, then is released before Git
advertisement or upload-pack processing. Ref/metadata transactions, recovery,
repository default-`HEAD` initialization, and GC use the same lock. Object puts
remain lock-free because they add immutable content-addressed objects; an object
is not made reachable until a locked ref transaction publishes it. The
crate-private object deletion API is used by GC under that lock.

This gives each projection one committed canonical view, including when it must
redo a journal left partway through applying refs. It does not pin the separate
`info/refs` and stateless upload-pack HTTP requests to the same generation: a
mutation between them may make both requests individually consistent but
different. No projection cache or generation counter is implemented. The guard
is exclusive, so projections serialize with other projections as well as
writers and GC; see `docs/BENCHMARKS.md` for the observed contention cost.

## 5. Failure philosophy

* Fail loudly, never silently: corruption, unknown config keys, non-minimal
  encodings, trailing bytes, path escapes ⇒ hard errors.
* Crash anywhere ⇒ recovery on next open (journal redo, temp sweep).
* Every injected fault point is named, documented, and covered by a test.

## 6. Security posture (summary; full docs in SECURITY_MODEL.md / THREAT_MODEL.md)

* No implicit execution of repository content. Smart-HTTP upload-pack and the
  bounded receive-pack write path invoke system Git only against private
  temporary projections with system/global Git configuration and
  repository-redirection variables isolated; receive-pack is used only after
  write-role authorization, and accepted canonical changes go through the
  transaction engine.
* All external input length/type/shape-validated before use; decoders total.
* Path safety enforced at every filesystem boundary (walk, checkout, join).
* Evidence honesty protocol: claimed vs deterministic results distinguished.
* Resource limits configurable and enforced (sizes, counts, depth, requests).

## 7. Maintenance layer (verify · gc · recover)

```
verify (read-only fsck)          gc (mark & sweep)                recover (WAL)
  objects: layout/name/            roots: HEAD + refs +             redo RUNNING journals
    envelope/digest/misfiled/        reflogs + ws bases             (idempotent apply,
    canonical (--deep re-encode)   mark: all links incl.             reflog txn-id dedup)
  links: existence + type           extras.prev chains            delete terminal journals
  refs/HEAD/reflog grammar        lock: global txn lock            (checkpointing, D-015)
  chains: head/prev/cycle/type    grace: 24 h mtime window         quarantine corrupt
  workspaces: meta/files/refs/    NEVER deletes: corrupt,           journals; sweep stale
    index cache                     misfiled, quarantine,           object temp files
  txn dir + config                  non-object debris
  ⇒ coded Issues (error|warn)     ⇒ GcReport counters
```

Design rules (D-014/D-015): verify never mutates; gc is strictly
non-destructive about anything it cannot fully decode; crash debris classes
are warnings with repair hints, not corruption; journals exist only while a
transaction is live or awaiting recovery.
