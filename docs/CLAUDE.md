# NewGit in Claude Code

NewGit ships an MCP server over stdio, so Claude Code can drive a NewGit
repository directly — no extra dependency, plugin, or service. This page is
kept short by design; the full contract lives in
[AGENT_GUIDE.md](AGENT_GUIDE.md) §3–§3b.

## Setup

```bash
# in your NewGit repo
claude mcp add newgit -- newgit mcp
# or with explicit repo path
claude mcp add newgit -- newgit --repo /path/to/repo mcp
```

Check the server with `claude mcp list` or `claude mcp get newgit`; remove it
with `claude mcp remove newgit`. Run one server per repository and per agent:
it listens on stdio only, never on a port, and it has exactly the operating
system privileges of the user that started it.

## Tools

`tools/list` returns 13 tools, each a thin 1:1 wrapper over a CLI command:
`newgit_status`, `newgit_history`, `newgit_cat`, `newgit_diff`,
`newgit_snapshot`, `newgit_verify`, `newgit_integrate`, `newgit_workspace`,
`newgit_goal`, `newgit_change`, `newgit_evidence`, `newgit_evaluation`,
`newgit_proposal`. Arguments, CLI equivalents, and result shape are tabulated
in [AGENT_GUIDE.md](AGENT_GUIDE.md) §3.

## Honesty rules the server enforces

- `newgit_evidence` with `action: "record"` executes the argv you pass and
  stores the real exit code and output; `tested` status requires evidence.
- `newgit_evaluation` with `ai: true` is marked AI-generated permanently.
- Failures return `isError: true` carrying NewGit's standard
  `{"ok":false,"error":{"category","message"}}` envelope — branch on
  `category`, not on message text.
- `author` values are display metadata; identity is the OS user that spawned
  the server, not a claim in tool arguments.

## Scope

MCP is one of several interfaces (CLI `--json`, HTTP API v1, read-only Web UI)
that share the same implementation, validation, and error categories.
Claude Code is one MCP client among many — NewGit does not require any AI
model. This document claims no Git compatibility and no production readiness;
see [README](../README.md), [known limitations](../KNOWN_LIMITATIONS.md), and
[release readiness](../RELEASE_READINESS.md).
