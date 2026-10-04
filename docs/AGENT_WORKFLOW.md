# Agent workflow example: two agents, one goal, compared by evidence

This is a **real transcript** (captured 2026-10-04, newgit v0.1.0, ids
shortened only where marked `…`). It shows the flow NewGit exists for:
multiple agents address the same Goal with alternative Changes; NewGit
records *runner-produced* Evidence (never "trust me" strings); an
Evaluation layer keeps deterministic results and AI opinions visibly
separate; a Proposal is approved and integrated **atomically**.

## 0. Setup

```console
$ newgit init
initialized newgit repository in /tmp/awf/.newgit
$ printf 'def solve(x):\n    return None\n' > app.py
$ newgit snapshot -m "skeleton: solve() stub" --time 1700000000000
snapshot 4fadd8ab4007 on main (refs/main)
  files: 1 (hashed 1, reused 0)
```

## 1. The goal

```console
$ newgit goal create "Implement solve() = x*2" --description "Two agents attempt this; best evidence wins."
created goal c7271a80af97…
$ newgit goal set-status c7271a80af97… in_progress
goal c7271a80af97 → in_progress (version 99f89159c128…)
```

## 2. Two agents, two isolated workspaces

```console
$ newgit workspace create agent-1 --author agent:qwen-coder
created workspace agent-1
  dir: /tmp/awf/.newgit/workspaces/agent-1/files
  position: 4fadd8ab4007…
$ newgit workspace create agent-2 --author agent:claude-coder
created workspace agent-2 …

# each agent edits ONLY its own workspace directory
$ printf 'def solve(x):\n    return x * 2\n' > .newgit/workspaces/agent-1/files/app.py
$ printf 'def solve(x):\n    return x + x\n' > .newgit/workspaces/agent-2/files/app.py

$ newgit snapshot -w agent-1 -m "agent-1: multiplication" --time 1700000001000 \
      --author agent:qwen-coder --goal c7271a80af97…
snapshot b7be8dede528 on agent-1 (workspaces/agent-1)
$ newgit snapshot -w agent-2 -m "agent-2: addition" --time 1700000001000 \
      --author agent:claude-coder --goal c7271a80af97…
snapshot fb8bd997d738 on agent-2 (workspaces/agent-2)

$ newgit workspace list
 agent-1   position=b7be8dede528   base=4fadd8ab4007
 agent-2   position=fb8bd997d738   base=4fadd8ab4007
*main      position=4fadd8ab4007   base=-
```

Snapshots carry `--goal` (and optionally `--change`) links, so every piece
of work is traceable to the goal it addresses.

## 3. Changes: first-class objects (multiple per goal)

```console
$ newgit change create "agent-1: multiplication" \
      --base 4fadd8ab4007… --result b7be8dede528… --goal c7271a80af97…
created change ca286d9b4f59…
$ newgit change create "agent-2: addition" \
      --base 4fadd8ab4007… --result fb8bd997d738… --goal c7271a80af97…
created change 23d357a32d5a…
$ newgit change list --goal c7271a80af97…
23d357a32d5a [draft] agent-2: addition (goal c7271a80af97)
ca286d9b4f59 [draft] agent-1: multiplication (goal c7271a80af97)
```

## 4. Honesty gate #1 — no "tested" without evidence

```console
$ newgit change set-status ca286d9b4f59… tested
newgit: error[invalid]: invalid value: change has no evidence — attach
evidence before marking tested (see `newgit evidence record`)   # exit 2
```

## 5. Runner-recorded evidence (deterministic by construction)

`evidence record` runs the command **itself**, capturing exit code,
combined stdout+stderr (capped, truncation flagged), and duration. The
verdict is derived from the exit status — an agent cannot type "pass".

```console
$ newgit evidence record --kind unit_test --target ca286d9b4f59… -w agent-1 -- \
      sh -c "grep -qF 'x * 2' app.py"
evidence c3d6d53c940e recorded: verdict=pass exit=0 duration=2ms bytes=15

$ newgit change attach-evidence ca286d9b4f59… c3d6d53c940e…
attached evidence c3d6d53c940e to change ca286d9b4f59 (version 12718d232e0e…)
$ newgit change set-status ca286d9b4f59… tested
change ca286d9b4f59 → tested (version 1aea11112334…)
```

Opinion evidence (human or AI review) is allowed but MUST be recorded with
`deterministic=false` — `evidence add` defaults to false and the UI/JSON
render the two kinds distinctly.

## 6. Evaluation: deterministic aggregation vs AI opinion

```console
$ newgit evaluation from-evidence ca286d9b4f59…
evaluation 5f0c5b892d1c…: verdict=pass (aggregated from 1 evidence)
#   ^ ai_generated=false always; verdict computed: all pass ⇒ pass,
#     any fail ⇒ fail, else inconclusive

$ newgit evaluation create --target 23d357a32d5a… --verdict inconclusive --ai \
      --dimension "correctness=pass:x+x equals x*2 for all reals" \
      --dimension "intent=inconclusive:goal literally says x*2"
added evaluation 7959c2655f0b… (ai-generated)
```

Both agents' work is now *comparable*: agent-1 has a deterministic pass;
agent-2 has an AI opinion marked inconclusive against the goal's letter.
Nothing here claims agent-2 "failed" — the flags say exactly what each
judgement is.

## 7. Proposal → approval → atomic integration

```console
$ newgit proposal create "Integrate agent-1 solve()" --change ca286d9b4f59… \
      --rationale "Deterministic evidence passes; matches goal statement literally."
created proposal d7ac42bf800f…

# Honesty gate #2: no integration without approval
$ newgit proposal integrate d7ac42bf800f…
newgit: error[invalid]: proposal must be approved before integration
(state: open)                                                  # exit 2

$ newgit proposal approve d7ac42bf800f… --author human:reviewer --author-name "Reviewer"
proposal d7ac42bf800f → approved (version ef13edf73cae…)

$ newgit proposal integrate d7ac42bf800f…
proposal d7ac42bf800f integrated into main
  proposal version: fa7c42dcdfc7…
  change version: 5fa09f053f2c…
  result: fast-forward 4fadd8ab4007 → b7be8dede528
```

The integration moved **three things in one journaled transaction**:
workspace position ref, proposal chain (→ `integrated`), change chain
(→ `integrated`). A crash mid-way recovers all-or-nothing (tested with
fault injection at `proposal:before_txn` / `proposal:after_txn`). Had main
diverged, this would have been a 3-way merge; conflicts abort before any
write (exit 5).

## 8. Aftermath

```console
$ cat app.py
def solve(x):
    return x * 2
$ newgit status
workspace: main @ b7be8dede528
clean: no changes
$ newgit goal set-status c7271a80af97… achieved
goal c7271a80af97 → achieved (version 974f17b1aa79…)
$ newgit history -n 3
b7be8dede528 2023-11-14T22:13:21Z agent:qwen-coder agent-1: multiplication
4fadd8ab4007 2023-11-14T22:13:20Z user skeleton: solve() stub
$ newgit history --goal c7271a80af97… --json   # every snapshot linked to the goal
$ newgit change list --goal c7271a80af97…      # agent-2's alternative is preserved
23d357a32d5a [tested] agent-2: addition (goal c7271a80af97)
ca286d9b4f59 [integrated] agent-1: multiplication (goal c7271a80af97)
```

Agent-2's change is **not destroyed** — it remains a tested alternative
implementation, comparable and integrable later (`newgit rollback` can
also undo the integration without rewriting history).

## Notes for agent integrators

* Use `--json` everywhere and parse `ok` / `error.category`; exit 4 = retry
  (CAS race), exit 5 = conflict, exit 2 = fix your usage.
* Prefer `evidence record` over `evidence add`: only the runner sets
  `deterministic=true`.
* Pin `--time` in tests for byte-reproducible snapshots.
* Never fabricate identities: `--author` is display metadata; authenticated
  identity arrives with remote auth (iteration 9) and is kept separate
  (SECURITY_MODEL §4).
