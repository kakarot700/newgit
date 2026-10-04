# Contributing to NewGit

NewGit is developed in bounded, honest iterations (see ROADMAP.md and the
commit history — one coherent vertical slice per commit). This document is
the contract for any change, human or agent.

## Non-negotiable rules

1. **No fake completion.** Never claim a test/benchmark/validation passed
   without running it. If something cannot run in your environment (e.g. CI
   on a hosted runner), say so explicitly in the docs you touch.
2. **No weakened tests.** Fix root causes. A test may only be edited when it
   encodes a wrong expectation — and the commit message must say why.
3. **Correctness over appearance.** A smaller rigorous system beats a large
   fake one. Remove code that isn't load-bearing.
4. **Dependencies need a decision.** The runtime budget is 5 crates
   (D-002): `sha2`, `flate2` (pure-Rust backend), `serde`, `serde_json`,
   `thiserror`. Adding one requires a DECISIONS.md entry, an SBOM
   regeneration, and passing `deny.toml` gates. `#![forbid(unsafe_code)]`
   never changes.
5. **Every bug grows a regression test**; crash-recovery bugs also grow a
   chaos seed or fault-point scenario (docs/TESTING.md).
6. **Docs are part of the change.** State files (PROJECT_STATE, ROADMAP,
   DECISIONS, TEST_MATRIX, SECURITY_MODEL, THREAT_MODEL, ARCHITECTURE,
   RELEASE_READINESS, KNOWN_LIMITATIONS, CHANGELOG) must be truthful AFTER
   your commit — they are the product's honesty record, not decoration.

## The gates (run before every commit)

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --release --locked
python3 scripts/sbom.py | diff -u SBOM.md -   # when Cargo.toml/lock changes
```

When `Cargo.toml` or `Cargo.lock` changes, regenerate the committed inventory
with `python3 scripts/sbom.py > SBOM.md`, review the resulting diff, and include
`SBOM.md` with the dependency update. The final command above verifies that the
committed inventory matches the lockfile.

## Code layout orientation

* `src/object/` — 9 canonical types + NGOB envelope; encodings are a FROZEN
  protocol (D-004): changing them changes object ids. Storage spec:
  docs/STORAGE_FORMAT.md (normative).
* `src/repo/` — store (atomic verified writes), refs (CAS + reflog), WAL
  transactions (D-009/D-015), config, index, workspaces.
* `src/ops/` — the operations layer (snapshot/status/history/integrate/
  workflow/verify/gc). CLI, remote server, MCP and tests all call THIS —
  never duplicate its logic in an interface layer.
* `src/diff/`, `src/merge/` — engines with total parsers (no panics ever —
  fuzz-tested).
* `src/remote/` — protocol v1 (docs/PROTOCOL.md is normative; additive
  changes only within v1).
* `src/cli/` — hand-rolled args (`args.rs`), dispatch (`mod.rs`),
  `call_json` (the programmatic entry MCP uses), command families.
* `src/ui/` — the embedded Web UI; the HTML file is asserted data-free and
  XSS-disciplined by tests — keep `textContent`, never `innerHTML`.
* Error handling: `src/error.rs` categories + exit codes are a STABLE API —
  adding is fine, renaming/removing is breaking.

## Commit style

One iteration/slice per commit: `Iteration N: <what landed>` followed by
bullets of WHAT and WHY, test-count delta, and gate results. History is
linear on `main` (no merge commits for development work).

## Where to look first

| Question | File |
|---|---|
| How does anything work end to end? | ARCHITECTURE.md |
| What does the loop do next? | PROJECT_STATE.md ("Current task") |
| Why is X the way it is? | DECISIONS.md |
| What is tested how? | TEST_MATRIX.md + docs/TESTING.md |
| What is known-broken/limited? | KNOWN_LIMITATIONS.md |
| What ships? | scripts/dist.sh + SBOM.md + RELEASE_READINESS.md |
