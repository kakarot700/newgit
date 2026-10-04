//! Remote protocol v1 — wire types shared by server and client.
//!
//! JSON-over-HTTP/1.1 (docs/PROTOCOL.md). Every response body uses the same
//! envelope as the CLI: `{ok:true,data:...}` or `{ok:false,error:{category,
//! message}}`. Oids travel as full lowercase hex strings. Objects travel as
//! base64-encoded *envelope* bytes (self-verifying: digest + id checked by
//! the receiver before anything touches the store).

use serde::{Deserialize, Serialize};

/// Protocol version negotiated via `X-NewGit-Protocol` and `/v1/info`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Header carrying the protocol version on requests AND responses.
pub const HDR_PROTOCOL: &str = "x-newgit-protocol";

/// Namespaces never listed by `/v1/refs` and never pushed/pulled by the
/// default ref selection (NewGit-internal state).
pub fn is_internal_ref(name: &str) -> bool {
    name.starts_with("workspaces/") || name.starts_with("chains/")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LimitsInfo {
    pub max_batch_objects: usize,
    pub max_request_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InfoData {
    pub product: String,
    pub version: String,
    pub protocol: u32,
    /// `"ref: <name>"` or a detached oid hex.
    pub head: String,
    pub capabilities: Vec<String>,
    pub limits: LimitsInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefEntry {
    pub name: String,
    pub oid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefsData {
    pub head: String,
    pub refs: Vec<RefEntry>,
}

/// `POST /v1/have` — "which of these oids does the server store?"
/// Server invariant: every stored object's links are also stored (remote
/// `objects/put` enforces dependency presence), so "have X" implies the
/// server holds X's full closure — clients may stop negotiation at X.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HaveReq {
    pub oids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HaveData {
    pub have: Vec<String>,
}

/// `POST /v1/negotiate` — server computes `reachable(want) − reachable(have)`
/// in dependency-first order. Haves the server does not store are ignored
/// (sound superset semantics: the client receives MORE, never less).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NegotiateReq {
    pub have: Vec<String>,
    pub want: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NegotiateData {
    /// Oids to fetch, dependencies before dependents.
    pub send: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectWire {
    /// base64 of the full envelope bytes (NGOB v1).
    pub data_b64: String,
}

/// `POST /v1/objects/get`
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectsGetReq {
    pub ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectsData {
    /// Same order as the request ids.
    pub objects: Vec<ObjectWire>,
}

/// `POST /v1/objects/put` — sequential, dependency-order batches. On first
/// invalid object the batch aborts (400 + index); objects stored before the
/// failure remain as harmless orphans (gc fodder) — refs, not objects, are
/// the atomicity boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectsPutReq {
    pub objects: Vec<ObjectWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PutData {
    pub stored: usize,
    pub oids: Vec<String>,
}

/// Compare-and-swap selector for a ref update.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CasWire {
    /// Any current value (including absence) — force update.
    Any,
    /// Must currently equal `old` (`null` = must not exist).
    Exactly { old: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefUpdateWire {
    pub name: String,
    pub cas: CasWire,
    /// `null` deletes the ref.
    pub new: Option<String>,
    /// Optional reflog message suffix.
    pub message: Option<String>,
}

/// `POST /v1/refs/update` — ALL updates apply in one transaction
/// (all-or-nothing; any CAS failure ⇒ nothing moves, HTTP 409).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefsUpdateReq {
    pub updates: Vec<RefUpdateWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateData {
    pub updated: Vec<String>,
    pub txn_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditData {
    pub entries: Vec<serde_json::Value>,
}

/// `POST /v1/object` — one object as typed JSON (the UI/agent view).
/// Blobs are special-cased: `data_b64` + `size` instead of a byte array.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectReq {
    pub oid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ObjectData {
    pub oid: String,
    /// "blob" | "tree" | "snapshot" | "actor" | "goal" | "change" |
    /// "evidence" | "evaluation" | "proposal"
    pub kind: String,
    /// Every oid this object links to (dependency order NOT implied).
    pub links: Vec<String>,
    /// Typed payload (`Object` serde form minus blobs); null for blobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// Blob payload only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_b64: Option<String>,
    /// Blob raw size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// `POST /v1/diff` — tree diff between two snapshots/trees/refs (specs are
/// resolved exactly like `newgit diff <a> <b>`), optionally with unified
/// content diffs for modified text files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffReq {
    pub a: String,
    pub b: String,
    #[serde(default)]
    pub content: bool,
    #[serde(default)]
    pub context: Option<usize>,
    #[serde(default)]
    pub no_renames: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnifiedFile {
    pub path: String,
    pub unified: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffData {
    pub a_root: String,
    pub b_root: String,
    /// `TreeDiff` serde form: {files:[{kind,path,old_path,old_mode,new_mode,
    /// old_oid,new_oid,binary,similarity}], rename_detection}.
    pub diff: serde_json::Value,
    /// Present only when `content` was requested: unified text per modified
    /// text file (capped server-side).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unified: Vec<UnifiedFile>,
}

/// `GET /v1/goals|changes|proposals` — workflow entity listings (same data
/// the CLI `goal list`/`change list`/`proposal list` commands show).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityEntry {
    pub oid: String,
    /// Typed object JSON (`Object` serde form).
    pub data: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListData {
    pub entities: Vec<EntityEntry>,
}

/// Client-side report for `newgit push`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PushReport {
    pub remote: String,
    pub url: String,
    pub refs_pushed: Vec<String>,
    pub objects_sent: usize,
    pub bytes_sent: u64,
    pub had_probe_requests: usize,
}

/// Client-side report for `newgit pull`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PullReport {
    pub remote: String,
    pub url: String,
    pub refs_updated: Vec<String>,
    pub refs_up_to_date: Vec<String>,
    pub objects_received: usize,
    pub bytes_received: u64,
    pub txn_id: Option<String>,
}
