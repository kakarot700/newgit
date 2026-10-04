# NewGit

[![CI](https://github.com/kakarot700/newgit/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/kakarot700/newgit/actions/workflows/ci.yml)
[![CodeQL](https://github.com/kakarot700/newgit/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/kakarot700/newgit/actions/workflows/codeql.yml)

**Agent-native version control for software built collaboratively by people and AI agents.** NewGit makes goals, competing changes, evidence, evaluations, and integration decisions explicit and auditable alongside source history.

> **Status: Production Candidate (pre-1.0); not labeled Production Ready.** The project is under active development. See [release readiness](RELEASE_READINESS.md), [known limitations](KNOWN_LIMITATIONS.md), and the [roadmap](ROADMAP.md) for the evidence and boundaries behind this classification.

## Why NewGit?

Traditional version control records snapshots and their ancestry. In human–agent development, a team also needs to preserve *why* work was requested, which alternatives were considered, what evidence was collected, and who approved integration. NewGit models those as versioned data rather than relying only on branch names and commit-message conventions.

NewGit is **not a GitHub replacement** and does not claim full Git compatibility. Its own repository and remote formats are distinct; Git import/export is a documented interoperability path (see [Git compatibility](docs/GIT_COMPAT.md)).

## Core concepts

```text
Goal → Workspace → Change → Evidence → Evaluation → Proposal → Integration
```

- A **goal** records intended work.
- A **workspace** provides an isolated working area.
- A **change** captures a candidate result against a base snapshot; multiple alternatives may address one goal.
- **Evidence** records the result of an explicitly requested command or other supported check. It is distinct from an AI-generated evaluation.
- An **evaluation** records a review; AI-generated opinions remain marked as such.
- A **proposal** expresses an integration decision.
- **Integration** applies the selected proposal and updates related state transactionally.

The VCS is usable without an AI model. Agent workflows use the same repository operations and validation rules as the CLI.

## Features

The current implementation includes:

- A Rust CLI and a content-addressed object store using SHA-256 identifiers.
- Snapshots, workspaces, status/history, tree and line diffs, three-way merges, integration, and rollback.
- Goals, changes, evidence, evaluations, and proposals with compare-and-swap version chains and documented lifecycle checks.
- `verify` for structural integrity checks and a deliberately non-destructive `gc`.
- An optional self-hosted HTTP/1.1 + JSON remote with bearer-token roles and audit records, plus a separate Git smart-HTTP adapter for clone/fetch/pull/`ls-remote` and a bounded authenticated branch-push slice with multi-ref transactions.
- A read-only embedded Web UI, JSON CLI/API, and an MCP stdio server.
- Git repository import/export through the system `git` program's `fast-export`/`fast-import` streams, and ordinary Git clone/fetch/pull/`ls-remote` plus tested single- and multi-branch pushes against a NewGit server over smart HTTP.

See [architecture](ARCHITECTURE.md), the [CLI reference](docs/CLI.md), [protocol](docs/PROTOCOL.md), and the [agent guide](docs/AGENT_GUIDE.md) for details. Feature claims and their test coverage are mapped in [TEST_MATRIX.md](TEST_MATRIX.md).

## Architecture

NewGit is a single Rust crate. Its main layers are:

1. **Interfaces:** CLI (including `--json`), HTTP server/client, MCP stdio, and the embedded read-only UI.
2. **Operations:** snapshots, workspaces, history, diff/merge, workflow entities, verification, garbage collection, and Git import/export.
3. **Repository engine:** canonical object codecs, a verified object store, compare-and-swap refs, and journaled transactions with recovery.

The object format and repository layout are documented in [ARCHITECTURE.md](ARCHITECTURE.md) and [docs/STORAGE_FORMAT.md](docs/STORAGE_FORMAT.md). Architectural trade-offs are recorded in [DECISIONS.md](DECISIONS.md).

## Requirements

- Rust and Cargo. The repository pins its development toolchain in `rust-toolchain.toml` (currently Rust 1.99.0); `rustup` can install it automatically when entering the checkout. `Cargo.toml` declares Rust 1.80 as the package minimum, but CI uses the pinned toolchain rather than separately testing that minimum.
- A system `git` executable is needed for `import-git`/`export-git` and the Git smart-HTTP read adapter. Conversion requires Git 2.20 or newer; the live adapter's recorded end-to-end environment is Git 2.43.0 on Linux, and no broader minimum-version/platform matrix is claimed. Other NewGit operations do not use Git.

## Build and install

```bash
git clone https://github.com/kakarot700/newgit.git
cd newgit
cargo build --release --locked
cargo test --locked
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/newgit "$HOME/.local/bin/newgit"
export PATH="$HOME/.local/bin:$PATH"
newgit --help
```

Prebuilt Linux x86_64 GNU archives and SHA-256 sidecars are published on [GitHub Releases](https://github.com/kakarot700/newgit/releases). Verify the downloaded archive against its sidecar before extracting it.

The pinned toolchain is installed by `rustup` when Cargo first runs in the checkout. To verify the exact toolchain explicitly, run `rustup toolchain install 1.99.0 --component rustfmt --component clippy` first. The current CI and clean-checkout validation target Linux x86_64; other operating systems and architectures are not claimed as verified yet.

## Quick start

After building NewGit and adding it to `PATH`, try the basic repository workflow:

```bash
newgit init myproject
cd myproject
newgit snapshot -m "initial state"
newgit status
newgit history
newgit verify
```

A fuller workflow that connects a goal to an agent workspace, evidence, a human-approved proposal, and integration is shown below. The [agent workflow guide](docs/AGENT_WORKFLOW.md) contains a transcript and further explanation.

```bash
newgit actor set-default --id agent:qwen-coder --name "Qwen Coder"
newgit goal create "Add OAuth authentication"        # prints a goal id
newgit goal set-status <goal-id> in_progress
newgit workspace create ws-agent-a
# The agent works in .newgit/workspaces/ws-agent-a/files/...
newgit snapshot -w ws-agent-a -m "implement OAuth" --goal <goal-id>
newgit change create "implement OAuth" --base <initial-snap> --result <snap> --goal <goal-id>
newgit evidence record --kind unit_test --target <change-id> -w ws-agent-a -- cargo test
newgit change attach-evidence <change-id> <evidence-oid>
newgit change set-status <change-id> tested
newgit proposal create "Ship OAuth" --change <change-id>
newgit proposal approve <proposal-id> --author human:reviewer --author-name "Reviewer"
newgit proposal integrate <proposal-id>
newgit goal set-status <goal-id> achieved
newgit verify
newgit history --goal <goal-id>
```

Use the IDs returned by the preceding commands in place of the placeholders. `evidence record` runs only the command explicitly supplied after `--`; NewGit does not implicitly execute repository content.

## Git interoperability

`newgit import-git <git-repo-path>` and `newgit export-git <target-dir>` use the local Git executable and stream formats. A running NewGit server also exposes Git smart HTTP: ordinary Git `clone`, `fetch`, `pull`, and `ls-remote` work. Authenticated pushes support one or more branch creates, fast-forward updates, or deletions per request, including atomic multi-ref operations; accepted refs are committed together by NewGit's transaction engine. See [Git compatibility](docs/GIT_COMPAT.md), the [compatibility matrix](docs/GIT_COMPATIBILITY_MATRIX.md), and [Git smart HTTP protocol details](docs/PROTOCOL.md#git-smart-http-compatibility).

This is **not full Git compatibility**: the tested push slice requires a write-role bearer token and supports branch refs only; tags, signed pushes, and forced non-fast-forward updates are refused. The server advertises Git's `atomic` capability, backed by Git's all-or-none projection checks and one NewGit CAS transaction for accepted refs; an ordinary request partially accepted in the projection fails without canonical ref changes. Git-over-SSH is not implemented. Each HTTP request builds a temporary Git-format view from NewGit's canonical objects and refs; Git object IDs therefore belong to that projection, not NewGit's SHA-256 object namespace, and materialization is not incremental. Annotated-tag metadata is not representable, submodules are refused, and several ref/path/message edge cases are lossy or skipped with a report. See [known limitations](KNOWN_LIMITATIONS.md) before relying on interoperability.

## Remote and agent interfaces

A self-hosted NewGit server can exchange NewGit objects and refs through its JSON protocol and can serve Git clients over smart HTTP, including the bounded authenticated single-/multi-branch push path. The built-in server is plain HTTP; put it behind a TLS-terminating reverse proxy or use a tunnel over untrusted networks. The NewGit CLI remote client currently accepts `http://` URLs only; standard Git clients may use HTTPS through a TLS-terminating proxy. See [deployment](docs/DEPLOYMENT.md) and the [protocol specification](docs/PROTOCOL.md).

The embedded UI is read-only. MCP uses stdio and inherits the operating-system privileges of the process that starts it. Consult [SECURITY.md](SECURITY.md) and [THREAT_MODEL.md](THREAT_MODEL.md) when choosing a deployment model.

## Testing

From the repository root:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo test --release --locked
```

The suite includes unit, property, integration, CLI, remote, concurrency, crash-recovery, parser, chaos, UI/MCP, and real-Git interoperability tests. Additional supply-chain checks are run in GitHub Actions; see [docs/TESTING.md](docs/TESTING.md) and [TEST_MATRIX.md](TEST_MATRIX.md). Test results are evidence for the tested platform and commit, not a guarantee of correctness in every deployment.

## Security

Security boundaries, accepted risks, and deployment guidance are documented in [SECURITY.md](SECURITY.md), [SECURITY_MODEL.md](SECURITY_MODEL.md), and [THREAT_MODEL.md](THREAT_MODEL.md). The built-in remote speaks unencrypted HTTP and must not be exposed directly to untrusted networks. Report suspected vulnerabilities through the private reporting route described in `SECURITY.md` rather than opening a public issue with exploit details.

## Production status

**Production Candidate, not Production Ready.** The project has substantial automated tests and documented recovery, interoperability, and operational limits, but a green CI run or a release artifact does not certify every production use case. Evaluate the [release gates](RELEASE_READINESS.md), [known limitations](KNOWN_LIMITATIONS.md), and your own threat model before deployment.

## Documentation

- [Architecture](ARCHITECTURE.md) · [storage format](docs/STORAGE_FORMAT.md) · [decisions](DECISIONS.md)
- [CLI](docs/CLI.md) · [remote protocol](docs/PROTOCOL.md) · [agent interfaces](docs/AGENT_GUIDE.md)
- [Agent workflow](docs/AGENT_WORKFLOW.md) · [Git compatibility](docs/GIT_COMPAT.md)
- [Deployment](docs/DEPLOYMENT.md) · [testing](docs/TESTING.md) · [troubleshooting](docs/TROUBLESHOOTING.md)
- [Security model](SECURITY_MODEL.md) · [threat model](THREAT_MODEL.md) · [security reporting](SECURITY.md)
- [Project state](PROJECT_STATE.md) · [roadmap](ROADMAP.md) · [test matrix](TEST_MATRIX.md)
- [Known limitations](KNOWN_LIMITATIONS.md) · [release readiness](RELEASE_READINESS.md) · [changelog](CHANGELOG.md) · [SBOM](SBOM.md)

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) and the detailed [contributor guide](docs/CONTRIBUTING.md) before opening a pull request. Bug reports should include a minimal reproduction and the output of `newgit --version`; do not include repository credentials or private data.

## License

NewGit is dual-licensed under **MIT OR Apache-2.0**. You may choose either license; see [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). Third-party dependency license information is summarized in [SBOM.md](SBOM.md).
