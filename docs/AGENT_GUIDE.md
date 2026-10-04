# NewGit Agent Guide — talking to a repository as a machine

NewGit exposes the SAME operations through three interfaces, all sharing one
implementation (`src/cli` dispatch → `src/ops`):

| Interface | For | Entry point |
|---|---|---|
| CLI `--json` | scripts, CI, any agent that can spawn a process | `newgit <cmd> --json` |
| HTTP API v1 | remote agents, services, browsers | `POST/GET http://host:port/v1/...` (docs/PROTOCOL.md) |
| MCP (stdio) | LLM agents via any MCP client | `newgit mcp` |

Nothing is interface-exclusive: an MCP tool is a thin wrapper over the CLI
dispatch, and the Web UI is a thin client of the HTTP API. Same validation,
same error categories, same honesty rules everywhere.

---

## 1. The model in 30 seconds

```
GOAL (what we want)
 └─ CHANGE (base snapshot → result snapshot; may carry EVIDENCE)
     └─ EVIDENCE (what actually ran: command, output, verdict, deterministic flag)
 └─ EVALUATION (opinion about a target; ai_generated flag is PERMANENT)
 └─ PROPOSAL (decision to integrate a change; approve/reject; integrate = 1 atomic txn)
SNAPSHOT = commit equivalent (immutable, content-addressed, workspace-scoped)
```

Rules agents MUST respect (enforced server-side, not etiquette):
- A change cannot become `tested` without attached evidence; a proposal
  requires the change to be `tested`/`proposed` first.
- `evidence record` runs YOUR explicit command and stores the REAL exit code
  and output. There is no way to record "pass" without a command that passed.
- `evaluation create --ai` marks an opinion as AI-generated forever. AI
  opinions are never presented as facts anywhere in the system.
- Actor identity is authenticated (token ⇒ principal), never a claim in a
  JSON body. `author` strings on objects are display metadata.

## 2. HTTP API quickstart (curl)

Server: `newgit serve --bind 0.0.0.0:8765 --token-file tokens.json`
(admin creates tokens: `newgit token add ci-bot --role write`).

Every request needs: `X-NewGit-Protocol: 1` and (unless the server allows
anonymous reads) `Authorization: Bearer <token>`. Responses are
`{"ok":true,"data":...}` or `{"ok":false,"error":{"category","message"}}`.

```bash
H='-H Content-Type:application/json -H X-NewGit-Protocol:1 -H "Authorization: Bearer $TOK"'

curl -s $H http://host:8765/v1/info                 # capabilities, limits, protocol
curl -s $H http://host:8765/v1/refs                  # all refs + oids (read role)
curl -s $H -d '{"oid":"<snapshot-oid>"}'      .../v1/object   # any object as JSON
curl -s $H -d '{"oid":"<blob-oid>"}'          .../v1/object   # blob ⇒ data_b64 + size
curl -s $H -d '{"a":"refs/main","b":"<oid>","content":true}' .../v1/diff
curl -s $H http://host:8765/v1/goals                 # every goal
curl -s $H "http://host:8765/v1/changes?goal=<goal-oid>"   # changes for one goal
curl -s $H http://host:8765/v1/proposals
curl -s $H -d '{"objects":[...]}'             .../v1/objects/put   # write role
curl -s $H -d '{"updates":[{"ref":"refs/main","old":null,"new":"<oid>"}]}' .../v1/refs/update
```

Error categories are stable strings (`invalid`, `malformed`, `not_found`,
`ref_not_found`, `auth`, `cas_failed`, `lock_busy`, `conflict`, `limit`,
`protocol`, ...) — branch on them, not on messages. `cas_failed` ⇒ re-read
and retry; `limit` ⇒ payload/concurrency cap hit (HTTP 413/429).

## 3. MCP quickstart

```bash
newgit --repo /path/to/repo mcp        # stdio, newline-delimited JSON-RPC 2.0
```

Handshake (any MCP client does this for you):

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"newgit_status","arguments":{}}}
```

13 tools, all mirroring CLI commands 1:1:

| Tool | CLI equivalent | Notes |
|---|---|---|
| `newgit_status` | `status [-w ws]` | |
| `newgit_history` | `history [--from][-n]` | |
| `newgit_cat` | `cat <oid> [--raw]` | raw blobs come back base64 |
| `newgit_diff` | `diff [a [b]] ...` | file list w/ renames+binary flags |
| `newgit_snapshot` | `snapshot -m msg` | mutation |
| `newgit_verify` | `verify [--deep]` | read-only fsck |
| `newgit_integrate` | `integrate <spec>` | atomic merge/FF |
| `newgit_workspace` | `workspace create/list/show/discard` | |
| `newgit_goal` | `goal create/show/list/set-status` | |
| `newgit_change` | `change create/show/list/set-status/attach-evidence` | honesty gates apply |
| `newgit_evidence` | `evidence add/show/record` | `record` EXECUTES the given argv — that is the point: real output, real exit code |
| `newgit_evaluation` | `evaluation create/from-evidence/show` | `ai:true` is permanent |
| `newgit_proposal` | `proposal create/show/list/approve/reject/close/integrate` | |

Tool results are the CLI's `--json` `data` payload as text content. NewGit
errors come back as `isError:true` with the standard
`{"ok":false,"error":{category,message}}` envelope — protocol-level JSON-RPC
errors (-32700/-32601/...) only for malformed RPC.

Security note: the MCP server has the privileges of the process that started
it (same as the CLI). Run one server per repository/per agent, under that
agent's OS user. It listens on stdio only — never on a port.

## 4. Web UI (read-only explorer)

```bash
newgit ui --bind 0.0.0.0:8765 [--token-file tokens.json] [--allow-anonymous-read]
```

Opens a single self-contained HTML page at `/` (no CDN, no build step, works
offline). Login is a bearer token — stored in `sessionStorage` only, sent as
`Authorization` header (never a cookie ⇒ CSRF is structurally impossible).
The UI renders the goal → change → evidence → proposal → integration flow,
snapshot history, a universal object inspector, side-by-side diffs, and the
audit log. It performs NO mutations by design (see KNOWN_LIMITATIONS #33);
writes stay in the CLI/MCP/API flow where authz is uniform.

## 5. Two agents, one goal (worked example)

See `docs/AGENT_WORKFLOW.md` for the full narrated example (agents Ada and
Bee both address goal "make word-count deterministic", each producing
changes + evidence + proposals, compared by evaluation, one integrated
atomically). The short version:

```bash
newgit goal create "make word-count deterministic"            # → goal oid G
# agent A (workspace a):
newgit workspace create a --base refs/main
newgit -w a snapshot -m "sort before count"                   # → result SA
newgit change create "sort-first" --base refs/main --result $SA --goal $G
newgit evidence record --kind unit_test --target $CH_A -w a -- cargo test
newgit change set-status $CH_A tested
newgit proposal create "ship sort-first" --change $CH_A
# agent B: same shape in workspace b → CH_B, proposal P_B
newgit evaluation from-evidence $CH_A ; newgit evaluation from-evidence $CH_B
newgit proposal integrate $P_A -w main    # atomic; goal + change chains move in one txn
newgit proposal close $P_B   # superseded by P_A (rationale lives in evaluations)
```
