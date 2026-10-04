# NewGit — Completion Report (build loop iterations 1–12)

Date: 2026-10-04 · Version: 0.1.0 · Classification: **PRODUCTION-CANDIDATE**
(evidence in RELEASE_READINESS.md; reasoning in §8 below)

NewGit is an **agent-native version-control system**: content-addressed and
crash-safe like git, but with goals, changes, evidence, evaluations and
proposals as first-class immutable objects — so "what we wanted", "what
changed", "what actually ran" and "what was decided" are versioned data,
not commit-message folklore. It is written in Rust (single crate, single
binary, `#![forbid(unsafe_code)]`), with 5 runtime dependencies, zero
cloud/paid services, and three machine interfaces (CLI `--json`, HTTP API
v1, MCP stdio) plus an embedded read-only Web UI.

---

## 1. Architecture summary

```
CLI (--json, stable exit codes) · Web UI (embedded single file) · MCP (stdio JSON-RPC)
        └──────────── all four call the SAME ops layer (cli::call_json / /v1/* / tools) ─┘
Ops: snapshot/status/history · diff · merge/integrate/rollback · workspaces ·
     goals/changes/evidence/evaluations/proposals (CAS version chains) · verify · gc ·
     git import/export
Engine: Myers diff + rename detection · 3-way tree/content merge (diff3) · WAL-journaled
     transactions (fsync commit point, forward recovery, quarantine) · CAS refs + reflog ·
     atomic verified object store · fast-export/fast-import git streams
Object model: 9 canonical types, SHA-256 ids, NGOB envelope (compressed canonical +
     digest + id — self-verifying), strict total decoders (fuzz: never panic)
util: hex/varint/base64, atomic writes, liveness-checked locks, path-safety grammar,
     fault-injection hooks
```

Full detail: ARCHITECTURE.md (layering), docs/STORAGE_FORMAT.md (normative
on-disk protocol), DECISIONS.md (D-001…D-018: every consequential choice
with rejected alternatives).

## 2. Feature inventory (all implemented, all tested — TEST_MATRIX.md maps each to suites)

* **Object store**: deterministic SHA-256 ids; immutable writes; corruption
  always detected (digest, misfiling, truncation, bombs); atomic
  tmp→fsync→rename→dir-fsync; crash-safe by construction.
* **Snapshots/workspaces**: no staging area — isolated workspaces hold live
  state; `snapshot` captures atomically; `status` diff-against-position with
  a racily-clean-guarded index cache; first-parent `history` with filters.
* **Changes/Goals/Evidence/Evaluations/Proposals**: mutable entities as
  CAS-guarded version chains; multiple changes per goal; honesty gates
  (`tested` requires attached evidence; proposals require tested/proposed);
  `evidence record` runs an EXPLICIT command and stores the real exit code
  and output; `ai_generated` on evaluations is permanent — opinions are
  never rendered as facts (CLI, API, MCP, and the UI's AI-OPINION badge).
* **Merge/integration**: fast-forward + 3-way merges, rename-aware, honest
  conflict reporting (no partial state ever), atomic proposal integration
  (position + goal/change chains in ONE journaled transaction), rollback.
* **Maintenance**: `verify [--deep]` read-only fsck with stable issue codes;
  strictly non-destructive `gc` (24 h grace, reflog/audit as roots, corrupt
  data preserved for forensics); automatic crash recovery + `recover`.
* **Git compatibility** (docs/GIT_COMPAT.md): `import-git`/`export-git` via
  the system git's own fast-export/fast-import — byte-identical blob
  round-trip (tested against real git repos incl. merges/branches), atomic
  ref switch, deterministic export; lossy points documented, never faked.
* **Remote protocol v1** (docs/PROTOCOL.md, normative): HTTP/1.1+JSON,
  bearer tokens (SHA-256 at rest, roles read<write<admin, invalid token
  never downgrades), append-only audit log of every request incl. failures,
  have-invariant negotiation (link-closed stores), incremental push/pull,
  client-side FF guard + server-side CAS in one transaction, resource
  limits everywhere (413/429), agent read endpoints (`/v1/object`,
  `/v1/diff` with CLI-identical unified rendering, goal/change/proposal
  listings with server-side filter + truncation flag).
* **Web UI** (`newgit ui`): embedded single-file explorer (no CDN/build
  step/npm), data-free static shell, token login in sessionStorage,
  XSS-safe by construction (textContent-only, CI-asserted), read-only:
  dashboard, history, universal object inspector (incl. tree browser and
  blob preview), goals/proposals boards, diff compare, audit viewer.
* **MCP** (`newgit mcp`): stdio JSON-RPC 2.0 (protocol 2024-11-05), 13
  tools mirroring the CLI 1:1 through the identical dispatch path; total
  error handling; dash-safe argv generation.
* **CLI quality**: `--json` everywhere (`{ok,data|error{category,message}}`),
  stable exit codes (0/2/3/4/5/6/7/70), stable error categories, help
  system, discovery, `--debug` structured stderr events (NEWGIT_LOG).

## 3. Test inventory — 278 automated tests, 0 failures (debug AND release)

| Suite group | Count | What it proves |
|---|---|---|
| lib unit (`cargo test --lib`) | 128 | codecs, store, refs, txn, ops, diff/merge engines, protocol wire, HTTP parser, UI-asset invariants, MCP protocol/argv units |
| cli_e2e (real binary) | 16 | workflows, envelopes, exit codes, ui/mcp child processes |
| remote_e2e (real TCP) | 16 | role matrix, push/pull equality, CAS races, limits, crash-mid-push, internal-namespace confinement (incl. diff-spec probing), object/diff/listing endpoints |
| workflow | 9 | honesty gates, chain CAS under concurrency, atomic proposal integration + crash atomicity, evidence record limits, AI-flag permanence |
| merge_integrate | 18 | FF/3-way/conflicts/renames/rollback + races |
| verify_gc | 20 | fsck codes, gc reachability/grace/forensics |
| ops_snapshot / diff_engine | 21 | snapshot/status/history/workspaces; Myers/rename/binary/property-reconstruct |
| property_core (proptest) | 12 | codec roundtrips, canonical-order invariants |
| txn_recovery + faultlab | 10 | kill-at-fault-point child processes; forward recovery; quarantine |
| chaos | 6 | fixed seeds × random ops × random kills; deep-verify after every step |
| fuzz_parsers | 8 | 150k seeded garbage inputs vs every parser — never panics |
| git_compat | 9 | real system-git round-trips |
| concurrency_refs / version | 5 | CAS one-winner races; format/version guards |

Invariants I1–I24+ are individually mapped to enforcing tests
(TEST_MATRIX.md registry). Regression discipline: every audit finding grew
a test (it12: 3 findings → 3 fixes → regressions in 3 existing suites).

## 4. Security review summary (SECURITY_MODEL.md, THREAT_MODEL.md §A–G + §E2)

* **Posture**: no unsafe code; no implicit execution of repository content
  anywhere (evidence record executes only explicit caller-provided argv);
  total parsers (fuzz-proven); path-safety grammar; identity ≠ authority
  (authenticated principals decide, display strings never do); secrets
  hygiene (tokens hashed at rest, shown once, never logged; audit stores
  ids only).
* **Audit outcomes (real runs, 2026-10-04)**: `cargo audit` — 1290 RustSec
  advisories × 63 locked crates → **0 findings**; `cargo deny check
  advisories bans licenses sources` → **all ok, zero warnings**; every
  build.rs in the 21-crate runtime closure read and classified (rustc
  probes only; no network); SBOM committed with CI drift gate.
* **Final-iteration hostile review found and FIXED** (all with regression
  tests): diff-endpoint internal-namespace probing; unbounded listing
  responses; MCP flag-shaped positional values.
* **UI/MCP surface**: XSS-safe by construction (asserted against shipped
  bytes); data-free unauthenticated shell; bearer-header auth ⇒ CSRF
  structurally impossible; MCP has no privileges beyond its spawning user
  and binds stdio only.
* Accepted risks are enumerated, not hidden (THREAT_MODEL §7 rows; KL list).

## 5. Benchmarks (docs/BENCHMARKS.md — real runs, release, shared Linux vCPU)

put_blob 1KiB ≈ 0.10 ms (≈10k obj/s) · snapshot 1000 files: 61 ms cold
(min 50), 2.3 ms warm · status: 1.8 ms cached / 4.8 ms cold · diff_trees
1000 files w/ rename detection: 1.4 ms · history 500: 4.3 ms · 3-way
integrate (200 files): 7.3 ms · verify --deep 1000 objects: 43 ms · gc 1000
orphans: 21 ms · snapshot scales ~linearly (5000 files ≈ 239 ms).
Iteration-11 re-check vs the iteration-7 baseline: worst median drift
1.41× — inside the documented shared-vCPU noise band, below the 2× release
gate; baseline archived, never overwritten. Durability dominates write
paths by design (fsync per object/ref; no `--no-fsync` exists — crash
safety is the product).

## 6. Git compatibility status (docs/GIT_COMPAT.md)

Import: real git repos (branches, merges, tags, binary, unicode paths) →
byte-identical blobs, deterministic snapshots, atomic ref switch; export:
fast-import stream git accepts unchanged; sha1 of original commits recorded
in snapshot extras (`git_sha1`) and UI badges. Honest lossy points:
annotated tags → lightweight (+extras), author/committer collapse, no
submodules, skipped namespaces mirrored — documented, tested, never faked.
No git reimplementation, no gitoxide/libgit2 (D-007/D-016).

## 7. Deployment (docs/DEPLOYMENT.md)

One static ~2.5 MB binary + one directory. `newgit serve` (loopback bind,
systemd unit with hardening provided), TLS via nginx/caddy reverse proxy
(configs provided) for browsers/API agents; the v1 CLI client is
plain-HTTP-only by explicit design (validate_url refuses `https://` rather
than faking certificate validation) — ssh-tunnel pattern documented.
`newgit ui` adds the explorer on the same port. Backups = rsync the repo
dir or push to a second server; content-addressing makes every copy
self-verifying (`newgit verify --deep`). Release artifacts:
`scripts/dist.sh` → tarball + SHA256SUMS over every file (28/28 verified);
binary bit-identical across clean rebuilds on same host+toolchain
(sha256 `30714184e3e4c9a2d4d28821b7cc1995171c259b07fc9ac7c98c5c7b822a4858`).

## 8. Known limitations (KNOWN_LIMITATIONS.md — 38 entries, all deliberate or documented)

Headlines: single-node per repo (no multi-primary); plain-HTTP v1 protocol
(TLS via proxy/tunnel; client-side TLS is v2); JSON+base64 wire (~33%
inflation; binary framing v2); UI read-only by design; MCP trust = spawning
process; reproducibility same-host scope; closure-walk RAM on huge repos;
no transfer resume. Nothing on the list is a surprise: each entry names its
rationale and, where applicable, the decision record.

## 9. Readiness decision

**PRODUCTION-CANDIDATE.** The former hosted-runner gate is no longer pending
for the runner-pinned source-bearing commit: [GitHub CI run
37182199247](https://github.com/kakarot700/newgit/actions/runs/37182199247)
and [CodeQL run
37182199239](https://github.com/kakarot700/newgit/actions/runs/37182199239)
both passed on Ubuntu 24.04 commit `afa94c4`. CI covered format/lint, debug
and release tests/build, SBOM and dependency security checks, a second clean
release build, package generation, and checksum verification. The project
remains pre-1.0 and limited by the documented platform, transport, and
interoperability boundaries; a green run does not certify every deployment.
See RELEASE_READINESS.md and KNOWN_LIMITATIONS.md for the current gates.

## 10. Exact commands + results (as run for this report, 2026-10-04)

```bash
cargo fmt --check                                # clean
cargo clippy --all-targets --locked -- -D warnings # clean (0 warnings)
cargo test --locked                              # 278 passed; 0 failed
cargo test --release --locked                    # 278 passed; 0 failed (chaos/fault/fuzz incl.)
python3 scripts/sbom.py | diff -u SBOM.md -      # clean (no dependency drift)
cargo audit                                      # 1290 advisories × 63 crates → 0 findings
cargo deny check advisories bans licenses sources # advisories ok, bans ok, licenses ok, sources ok
cargo build --release --locked                   # sha256 30714184e3e4c9a2…
CARGO_TARGET_DIR=<second-clean-target> cargo build --release --locked
                                                 # identical sha256 (bit-reproducible, same host)
bash scripts/dist.sh --skip-build                # dist tarball bbafe611…
(cd dist/newgit-0.1.0-x86_64-unknown-linux-gnu && sha256sum -c SHA256SUMS.txt)  # 28/28 OK
cargo run --release --bin newgit-bench           # docs/BENCHMARKS.md it11 table (real run)
# README quickstart executed VERBATIM end-to-end against the built binary:
#   … verify → "checked 19 objects, 5 refs, 3 chains, 2 workspaces — 0 errors, 0 warnings"
```

*Report generated by the autonomous build loop; every number above came
from a command actually executed in this workspace.*


## 11. Public publication and hosted validation (2026-10-04)

**Repository:** [https://github.com/kakarot700/newgit](https://github.com/kakarot700/newgit), public, default branch `main`. The archive's 12 reachable implementation commits remain in the published history; publication changes were appended without squashing or rewriting that history. Reflog-only abandoned snapshots were not published.

**Hosted checks on the runner-pinned source-bearing commit `afa94c4`** (Ubuntu 24.04):

- [CI run 37182199247](https://github.com/kakarot700/newgit/actions/runs/37182199247) — success. The applicable format/lint/test/build, dependency and SBOM, reproducibility, distribution, and checksum jobs passed. The pull-request-only dependency-review and tag-only release jobs were not applicable to this `push` event.
- [CodeQL run 37182199239](https://github.com/kakarot700/newgit/actions/runs/37182199239) — analysis completed successfully. This is a workflow result, not a claim that every possible code-scanning alert is absent.

**Independent clean-clone verification** at commit `b648079` used the public repository, not the source archive:

- `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, locked debug/release tests, release build, SBOM-drift check, and Actions lint passed.
- Debug: 278 passed, 0 failed across 19 test suites. Release: 278 passed, 0 failed across 19 test suites.
- The README's build/install/help/basic quick start was exercised using an isolated home; `newgit --version` reported `newgit 0.1.0`, and `init`, `snapshot`, `status`, `history`, and `verify` completed with 0 errors and 0 warnings.
- A real CLI agent workflow created a goal and workspace, recorded a command-backed passing evidence item, marked the change tested, invoked `proposal approve` under an explicit test reviewer identity, integrated the proposal, marked the goal achieved, and verified the repository. The command exited 0; `verify` reported 21 objects, 5 refs, 3 chains, 2 workspaces, no quarantined objects, and no issues.
- A real Git repository with 4 commits, `main`/`feature` branches, a merge, and a lightweight tag was imported and exported. Three refs were checked; trees, commit messages and per-ref commit counts matched, `HEAD` remained `main`, the exported worktree was clean, and NewGit `verify` reported 0 errors and 0 warnings.

**Security and operations:** Gitleaks v8.30.1 found no findings in the preserved Git history or the edited publication worktree. GitHub secret scanning with push protection, Dependabot alerts/security updates, private vulnerability reporting, and read-only-by-default Actions token permissions are enabled. The repository remains **Production Candidate**, not Production Ready: the supported prebuilt target is Linux x86_64 GNU, cross-host reproducibility is not claimed, and the self-hosted protocol v1 is plain HTTP unless protected by a reverse proxy or trusted tunnel. The versioned notes at `docs/releases/v0.1.0.md` document the scope and limitations; the actual tag and assets are surfaced by the public Releases page.
