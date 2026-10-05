# NewGit Storage Format v1 — normative specification

Status: **frozen for format_version = 1**. Any change requires a new version
and a migration path. Implementations MUST reject unknown versions, tags,
non-minimal varints, and trailing bytes. Decoders MUST NOT panic on any
input (tested via fuzz/property suites).

## 1. Object identity

```
id(object) = SHA-256(canonical(object))            # 32 bytes, hex-displayed
canonical(object) = type_tag:u8 || varint(body_len) || body
```

Identical content MUST produce identical ids (deterministic-identity invariant).

## 2. Primitive encodings

| Type | Encoding |
|---|---|
| `u8` | one byte |
| `bool` | `0x00` false / `0x01` true; other values invalid |
| `varint` | unsigned LEB128, **minimal** (non-minimal rejected; max 10 bytes) |
| `svarint` | zigzag then varint |
| `bytes` | `varint(len) || raw` |
| `str` | `bytes`, validated UTF-8 |
| `oid` | 32 raw bytes |
| `oid?` | `bool` then `oid` if true |
| `str?` | `bool` then `str` if true |
| `oid[]` | `varint(count)` then `count × oid` |
| `map` | `varint(count)` then `count × (str key, str value)`, keys **strictly ascending**, no duplicates |

### Type tags

| Tag | Type | Tag | Type |
|---|---|---|---|
| 1 | Blob | 6 | Change |
| 2 | Tree | 7 | Evidence |
| 3 | Snapshot | 8 | Evaluation |
| 4 | Actor | 9 | Proposal |
| 5 | Goal | | |

### Enums (u8)

* EntryMode: 0 File, 1 Executable, 2 Symlink, 3 Tree
* ActorKind: 0 Human, 1 Agent, 2 Process, 3 Anonymous
* GoalStatus: 0 Open, 1 InProgress, 2 Achieved, 3 Abandoned
* ChangeStatus: 0 Draft, 1 Tested, 2 Proposed, 3 Integrated, 4 Abandoned
* Verdict: 0 Pass, 1 Fail, 2 Inconclusive, 3 NotApplicable
* ProposalState: 0 Open, 1 Approved, 2 Rejected, 3 Integrated, 4 Closed

### Limits (protocol-level)

`MAX_NAME_LEN=255`, `MAX_MESSAGE_LEN=65536`, `MAX_TITLE_LEN=512`,
`MAX_DESC_LEN=65536`, `MAX_ID_STR_LEN=512`, `MAX_TOOL_LEN=256`,
`MAX_COMMAND_LEN=8192`, `MAX_EXTRAS=256`, `MAX_EXTRA_KEY=256`,
`MAX_EXTRA_VALUE=8192`, `MAX_METRICS=256`, `MAX_DIMENSIONS=256`,
`MAX_LIST=2^20`, `MAX_PARENTS=64`, `MAX_PUBKEY=4096`,
`MAX_WORKSPACE_NAME=255`. Repository-level limits (blob size etc.) are
configuration, not protocol (see §5).

## 3. Object bodies

### Blob (1)
Body = raw bytes. Opaque; no UTF-8 or size requirement at protocol level.

### Tree (2)
```
varint(count) || count × ( EntryMode:u8 || str name || oid )
```
Entries MUST be strictly ascending by `name` (byte order), unique.
Names: non-empty, ≤255 bytes, UTF-8, no `/`, no NUL, not `.` or `..`,
no control characters. Subtrees use mode 3 and MUST reference a Tree object.

### Snapshot (3)
```
oid[] parents || oid root || oid author || svarint timestamp_ms ||
svarint tz_offset_min || str message || str? workspace ||
oid? change || oid? goal || map extras
```
`parents[0]` is the primary lineage parent; parents unique; ≤64.
`tz_offset_min` ∈ [-1440, 1440]. Root MUST be a Tree; author MUST be an Actor.

### Actor (4)
```
ActorKind:u8 || str id || str display_name || str tool || str tool_version ||
bool has_pubkey [ str algo || bytes key ] || map extras
```
Actor `id` convention: `kind:name` (e.g. `human:alice`, `agent:acme-v2`).
Presence of a pubkey is metadata, **not** verification (see SECURITY_MODEL.md).

### Goal (5)
```
str title || str description || oid creator || GoalStatus:u8 ||
svarint created_ms || svarint updated_ms || map extras
```
`updated_ms ≥ created_ms` required.

### Change (6)
```
oid base || oid result || oid author || oid? goal || str title ||
str description || ChangeStatus:u8 || svarint created_ms ||
svarint updated_ms || oid[] evidence || map extras
```
`base ≠ result` required; `evidence` strictly ascending + unique.

### Evidence (7)
```
oid producer || oid? target || str kind || Verdict:u8 || bool deterministic ||
str tool || str tool_version || str command || oid? output ||
map metrics || svarint created_ms || map extras
```
`deterministic=true` MUST only be set by a runner that executed the tool.

### Evaluation (8)
```
oid target || oid evaluator || bool ai_generated || Verdict:u8 ||
varint(n) || n × ( str dimension || Verdict:u8 || str note ) ||
svarint created_ms || map extras
```
Dimensions strictly ascending by name, unique.

### Proposal (9)
```
oid change || str title || str rationale || oid author || oid base ||
oid[] evidence || oid[] depends_on ||
varint(n) || n × ( oid approver || svarint ts ) ||
ProposalState:u8 || svarint created_ms || svarint updated_ms || map extras
```
`evidence`/`depends_on` strictly ascending + unique; approvals strictly
ascending by (approver, ts), unique.

## 4. On-disk envelope ("NGOB")

```
offset size field
0      4    magic "NGOB"
4      1    format version = 1
5      1    type tag
6      8    raw_len   (u64 LE) — canonical length
14     8    stored_len (u64 LE) — compressed length
22     n    zlib payload (RFC1950, level 6)
22+n   32   digest = SHA-256(canonical) = object id
```

Readers MUST: check magic/version/tag; check `stored_len == n`; enforce
configured `max_raw` **before and during** inflation (bomb protection);
recompute SHA-256 and compare with the digest AND with the requested id /
file path. Any mismatch = corruption error; never silently accept or repair.

Storage path: `objects/<hex[0..2]>/<hex[2..64]>`. Writes: temp file in the
same directory → write → fsync → rename → fsync(dir). Temps match
`*.tmp.<pid>.<nanos>.<ctr>` and are swept when stale.

Coordination uses a whole-file OS advisory lock on a stable sidecar named by
appending `.lock` to the protected path. The sidecar persists after use; new
sidecars are empty, and NewGit never reads or writes their contents. Owner text
left by older versions may remain but is ignored. The kernel releases ownership
when the owning process/handle exits. Symlink and special-file sidecars are
rejected; NewGit never unlinks a lock sidecar, and concurrent processes must
use the same lock protocol. `lock_wait_ms` bounds acquisition. The legacy config key
`lock_stale_s` is accepted as an alias for `temp_file_grace_s`, which controls
stale object-temp cleanup only; it no longer controls lock reclamation.

## 5. Repository layout (v1)

```
<workdir>/.newgit/
  config            # key = value; format_version = 1 (required)
  HEAD              # "ref: refs/<name>" or a bare snapshot oid
  objects/…         # object store (§4)
  refs/<name>       # 64-hex oid + "\n"; names: [A-Za-z0-9._/-], rules below
  logs/refs/<name>  # reflog: "<old> <new> <ts_ms> <actor_oid> <message>\n"
  txn/              # transaction journals (§6)
  actors/<id-hash>.oid  # actor registry (id string → oid mapping files)
  workspaces/<name>/    # per-workspace metadata + index (§7)
  git-map/          # optional sha1↔nid maps from git import
```

Ref name rules: segments of `[A-Za-z0-9._-]` joined by `/`; no empty
segments, no leading/trailing `/`, no `..`, no `~ ^ : ? * [ \`, no segment
`.lock`, no trailing `.lock`, no `@{`, ≤255 bytes total. Reserved prefixes:
`heads` not required; `logs`, `txn`, `workspaces`, `remotes`, `meta` are
reserved for system use and rejected for user refs except `remotes/<remote>/*`
(managed by the remote layer).

## 6. Transaction journal format

```
txn/<ts_ms>-<pid>-<rand>.journal
  line 1: "NEWGIT-TXN v1"
  line 2: "state=RUNNING|COMPLETE|RECOVERED"
  then one op per line:
    "REF <name> <old-oid-or-ZERO> <new-oid-or-ZERO>"     # ZERO = create/delete
    "REFLOG <name> <old> <new> <actor> <message-b64>"
    "FILE <path-relative-to-.newgit> <sha256-of-new-content-b64>"
  final line: "END"
```

Protocol: acquire global txn lock → write journal (RUNNING) → fsync →
materialize all new contents as temp files → fsync → rename each → fsync dirs
→ append reflog lines → rewrite header state=COMPLETE → fsync → **delete the
journal (checkpoint)** → release lock.
Recovery on open: any RUNNING journal is **redone** from its recorded final
state (renames are idempotent), marked RECOVERED, then deleted; COMPLETE or
RECOVERED journals (crash between commit and checkpoint-delete) are deleted
without re-apply. This yields all-or-nothing semantics across process death
and keeps `txn/` bounded: a journal exists only while a transaction is live
or a crash is awaiting recovery (D-015).

## 7. Workspace index (status cache)

```
workspaces/<name>/meta    # key = value: base_ref, base_oid, created_ms, actor
workspaces/<name>/index   # binary: "NGIX" v1 || varint(n) || n × ( str path || oid || varint size || varint mtime_secs || varint mtime_nanos || u8 mode )
```
The index is a **cache** only: it may be deleted and rebuilt from a snapshot
at any time; no operation may treat it as authoritative (invariant).
