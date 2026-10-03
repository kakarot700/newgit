# NewGit

**An agent-native version-control system.** Not "GitHub with a chatbot" —
a from-first-principles VCS for software written collaboratively by humans
and AI agents.

> Status: **0.1.0-dev, NOT PRODUCTION READY.** Under active autonomous
> development; see [RELEASE_READINESS.md](RELEASE_READINESS.md) for the
> honest gate checklist and [ROADMAP.md](ROADMAP.md) for the plan.

## Why

Git models *files and branches*. Modern software teams — mixed human and
agent — actually work in terms of **goals**, **alternative changes**,
**evidence**, and **integration decisions**. NewGit makes those first-class,
immutable, and auditable:

```
Goal → Workspaces → Changes (alternatives) → Evidence → Evaluations
     → Proposals → Integration → permanent, explainable history
```

AI is optional at every layer: NewGit is a complete VCS without it, and an
agent-collaboration substrate with it. Honesty is protocol-level: machine
test results and AI opinions are **different object fields** and can never be
confused (`deterministic`, `ai_generated`).

## Core properties

- **Immutable, content-addressed state** — SHA-256 identity; self-verifying
  on-disk envelope; corruption is always detected, never silent.
- **Crash-safe by construction** — atomic writes, WAL-journaled transactions,
  idempotent recovery; fault-injection tested.
- **Concurrency first-class** — lock+CAS refs, isolated workspaces, races and
  interrupted operations under test.
- **Secure by design** — no implicit execution of repo content, total parsers
  (no panics on malformed input), path-safety grammar, configurable resource
  limits, threat-model-driven tests.
- **Git-compatible where it matters** — import real git repos, export back
  (fast-export/fast-import), documented exact limitations.
- **Zero-rupee, self-hostable** — 5 small runtime dependencies, no cloud,
  no paid services, single static binary + optional built-in server/web UI.

## Quickstart (once iteration 3 lands)

```bash
newgit init myproject && cd myproject
newgit snapshot -m "initial state"
newgit goal create "Add OAuth authentication"
newgit workspace create ws-agent-a --goal <goal-id>
# ... work happens in the workspace directory ...
newgit change create --workspace ws-agent-a --goal <goal-id> -m "implement OAuth"
newgit evidence run --change <change-id> -- cargo test        # deterministic evidence
newgit proposal create --change <change-id>
newgit integrate --proposal <proposal-id>
newgit verify && newgit history
```

All commands support `--json` for agents and scripts; exit codes are stable
(see `src/error.rs::exit_code`).

## Documentation index

| Doc | What |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | product model + system layering |
| [docs/STORAGE_FORMAT.md](docs/STORAGE_FORMAT.md) | normative object/envelope/repo protocol |
| [SECURITY_MODEL.md](SECURITY_MODEL.md) · [THREAT_MODEL.md](THREAT_MODEL.md) | trust boundaries; per-threat mitigations+tests |
| [DECISIONS.md](DECISIONS.md) | architectural decision log |
| [ROADMAP.md](ROADMAP.md) · [PROJECT_STATE.md](PROJECT_STATE.md) | plan + live loop state |
| [TEST_MATRIX.md](TEST_MATRIX.md) | what is tested, how to run, invariant registry |
| [KNOWN_LIMITATIONS.md](KNOWN_LIMITATIONS.md) | honest bounds |
| [CHANGELOG.md](CHANGELOG.md) | per-iteration changes |
| [RELEASE_READINESS.md](RELEASE_READINESS.md) | production gate status |
| docs/CLI.md, docs/PROTOCOL.md, docs/AGENT_GUIDE.md, docs/GIT_MIGRATION.md, docs/TESTING.md, docs/BENCHMARKS.md, docs/DEPLOYMENT.md, docs/CONTRIBUTING.md, docs/TROUBLESHOOTING.md | land with their iterations |

## Building

```bash
rustup toolchain install  # pinned via rust-toolchain.toml (1.99.0)
cargo build --release     # binary: target/release/newgit
cargo test                # full suite
cargo clippy --all-targets -- -D warnings
```

## License

MIT OR Apache-2.0.
