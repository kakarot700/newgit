//! `newgit mcp` — Model Context Protocol server over stdio (JSON-RPC 2.0,
//! newline-delimited). Iteration 10.
//!
//! Design rule: tools are THIN wrappers over the exact same CLI dispatch the
//! human/agent binary uses (`cli::call_json`) — one code path, no duplicated
//! command logic, identical validation and error categories. Errors are
//! returned as tool results with `isError:true` carrying the standard
//! `{ok:false,error:{category,message}}` envelope; protocol violations get
//! proper JSON-RPC error codes.
//!
//! Security: the MCP server inherits the privileges of whoever started it
//! (same as the CLI). `evidence record` executes an EXPLICIT command given
//! by the caller — it never executes repository content implicitly
//! (SECURITY_MODEL §3). No network access; stdio only.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::cli::args::{Args, COMMON_ALIASES};
use crate::cli::{call_json, Ctx, Output};
use crate::error::{Error, Result};

pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

pub(crate) fn cmd_mcp(ctx: &Ctx, tail: &[String]) -> Result<Output> {
    let a = Args::parse(tail, &[], COMMON_ALIASES)?;
    a.reject_unknown(&["json", "debug", "repo"])?;
    let repo: Option<PathBuf> = ctx.repo.clone();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(&line, repo.as_deref()) {
            let mut s = resp.to_string();
            s.push('\n');
            if out
                .write_all(s.as_bytes())
                .and_then(|_| out.flush())
                .is_err()
            {
                break; // reader gone
            }
        }
    }
    Ok(Output::Text(String::new()))
}

/// Process one JSON-RPC line; `None` means "no response" (notification).
fn handle_line(line: &str, repo: Option<&std::path::Path>) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(rpc_err(Value::Null, -32700, &format!("parse error: {e}")));
        }
    };
    if msg.is_array() {
        return Some(rpc_err(
            Value::Null,
            -32600,
            "batch requests are not supported (one message per line)",
        ));
    }
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = msg.get("id").is_none();
    if msg.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") {
        return Some(rpc_err(id, -32600, "jsonrpc must be \"2.0\""));
    }
    let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    crate::obs::event("mcp_request", &[("method", json!(method))]);

    match method {
        "initialize" => Some(rpc_ok(
            id,
            json!({
                "protocolVersion": params.get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(MCP_PROTOCOL_VERSION),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "newgit",
                    "title": "NewGit agent-native version control",
                    "version": crate::VERSION,
                },
                "instructions": "Tools mirror the newgit CLI 1:1 (status, history, cat, diff, \
                                 snapshot, verify, integrate, workspace, goal, change, evidence, \
                                 evaluation, proposal). Results are the CLI's --json `data` \
                                 payloads. Errors carry {category,message}; category `cas_failed` \
                                 means retry. Evidence honesty flags (deterministic, ai_generated) \
                                 must be preserved when relaying results — claims are not facts.",
            }),
        )),
        "notifications/initialized" | "initialized" => None,
        "ping" => Some(rpc_ok(id, json!({}))),
        "tools/list" => Some(rpc_ok(id, json!({ "tools": tools_list() }))),
        "tools/call" => {
            if is_notification {
                return None;
            }
            Some(call_tool(id, &params, repo))
        }
        _ => {
            if is_notification {
                None
            } else {
                Some(rpc_err(id, -32601, &format!("method not found: {method}")))
            }
        }
    }
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}
fn rpc_err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

// ---------------------------------------------------------------------------
// tool schemas
// ---------------------------------------------------------------------------

fn prop(t: &str, desc: &str) -> Value {
    json!({ "type": t, "description": desc })
}
fn tool(name: &str, description: &str, props: Vec<(&str, Value)>, required: Vec<&str>) -> Value {
    let mut properties = serde_json::Map::new();
    for (k, v) in props {
        properties.insert(k.to_string(), v);
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(properties),
            "required": required,
            "additionalProperties": false,
        }
    })
}

pub fn tools_list() -> Vec<Value> {
    vec![
        tool(
            "newgit_status",
            "Compare a workspace against its recorded position (clean/modified/added/deleted, head oid).",
            vec![("workspace", prop("string", "workspace name (default: main checkout)"))],
            vec![],
        ),
        tool(
            "newgit_history",
            "Walk snapshot history from a ref/oid (default HEAD); returns messages, authors, timestamps, parents.",
            vec![
                ("from", prop("string", "ref name or object id to start from")),
                ("limit", prop("integer", "max entries (default 20)")),
            ],
            vec![],
        ),
        tool(
            "newgit_cat",
            "Inspect any object by id (snapshot/tree/blob/actor/goal/change/evidence/evaluation/proposal) as JSON.",
            vec![
                ("oid", prop("string", "full object id (hex)")),
                ("raw", prop("boolean", "for blobs: return raw bytes (base64 in the JSON payload)")),
            ],
            vec!["oid"],
        ),
        tool(
            "newgit_diff",
            "Diff two snapshots/trees/refs (or a spec vs the live workspace). Machine-readable file list with renames, modes, binary flags.",
            vec![
                ("a", prop("string", "left spec (ref/oid/ws:name); omit for workspace position")),
                ("b", prop("string", "right spec; omit for live workspace")),
                ("name_only", prop("boolean", "only list paths")),
                ("context", prop("integer", "unified-diff context lines")),
                ("no_renames", prop("boolean", "disable rename detection")),
            ],
            vec![],
        ),
        tool(
            "newgit_snapshot",
            "Capture a workspace as an immutable snapshot object (the commit equivalent). Returns the new oid.",
            vec![
                ("message", prop("string", "snapshot message")),
                ("workspace", prop("string", "workspace name (default: main checkout)")),
            ],
            vec!["message"],
        ),
        tool(
            "newgit_verify",
            "Read-only integrity check (fsck) with stable issue codes; deep mode re-verifies digests and walks every link.",
            vec![("deep", prop("boolean", "verify digests + walk links"))],
            vec![],
        ),
        tool(
            "newgit_integrate",
            "Atomically integrate a snapshot/ref/workspace position into a workspace (fast-forward or 3-way merge; conflicts leave NO partial state).",
            vec![
                ("spec", prop("string", "snapshot oid, ref name, or ws:name")),
                ("workspace", prop("string", "target workspace (default: main checkout)")),
                ("message", prop("string", "merge snapshot message")),
                ("no_renames", prop("boolean", "disable rename detection")),
            ],
            vec!["spec"],
        ),
        tool(
            "newgit_workspace",
            "Manage isolated workspaces: create/list/show/discard.",
            vec![
                ("action", prop("string", "one of: create, list, show, discard")),
                ("name", prop("string", "workspace name (create/show/discard)")),
                ("base", prop("string", "create: base ref/oid")),
                ("force", prop("boolean", "discard: force")),
            ],
            vec!["action"],
        ),
        tool(
            "newgit_goal",
            "Goals: create/show/list/set-status. A goal is the intent multiple changes can address.",
            vec![
                ("action", prop("string", "one of: create, show, list, set-status")),
                ("title", prop("string", "create: goal title")),
                ("description", prop("string", "create: longer description")),
                ("id", prop("string", "show/set-status: goal id (hex prefix ok)")),
                ("status", prop("string", "set-status: proposed|in_progress|achieved|abandoned")),
            ],
            vec!["action"],
        ),
        tool(
            "newgit_change",
            "Changes: create (base→result snapshots)/show/list/set-status/attach-evidence. Status transitions are honesty-gated (e.g. `tested` requires evidence).",
            vec![
                ("action", prop("string", "one of: create, show, list, set-status, attach-evidence")),
                ("title", prop("string", "create: change title")),
                ("base", prop("string", "create: base snapshot spec")),
                ("result", prop("string", "create: result snapshot spec")),
                ("goal", prop("string", "create: goal id")),
                ("description", prop("string", "create: description")),
                ("id", prop("string", "show/set-status/attach-evidence: change id")),
                ("status", prop("string", "set-status value")),
                ("evidence_oid", prop("string", "attach-evidence: evidence object id")),
            ],
            vec!["action"],
        ),
        tool(
            "newgit_evidence",
            "Evidence: add (claim with honesty flags), show, or record (RUN an explicit command and capture its real output/exit status — never fabricates results).",
            vec![
                ("action", prop("string", "one of: add, show, record")),
                ("kind", prop("string", "e.g. unit_test, lint, build, benchmark")),
                ("verdict", prop("string", "add: pass|fail|inconclusive|not_applicable")),
                ("deterministic", prop("boolean", "add: machine-verified flag (honesty)")),
                ("target", prop("string", "change/goal id this evidence targets")),
                ("output", prop("string", "add: path to raw output file")),
                ("metrics", prop("string", "add: comma-separated k=v metrics")),
                ("oid", prop("string", "show: evidence object id")),
                ("workspace", prop("string", "record: workspace to run in")),
                ("command", json!({"type":"array","items":{"type":"string"},"description":"record: explicit argv to execute (after `--`)"})),
            ],
            vec!["action"],
        ),
        tool(
            "newgit_evaluation",
            "Evaluations: create (--ai flag marks AI opinions FOREVER), from-evidence (derive from a change's evidence), show.",
            vec![
                ("action", prop("string", "one of: create, from-evidence, show")),
                ("target", prop("string", "create: object being evaluated")),
                ("verdict", prop("string", "create: pass|fail|inconclusive|not_applicable")),
                ("ai", prop("boolean", "create: mark as AI-generated opinion (never a fact)")),
                ("dimensions", prop("string", "create: `name=verdict:note;…`")),
                ("change_id", prop("string", "from-evidence: change id")),
                ("oid", prop("string", "show: evaluation object id")),
            ],
            vec!["action"],
        ),
        tool(
            "newgit_proposal",
            "Proposals: create/show/list/approve/reject/close/integrate. Integration is atomic (position + goal/change chains in one transaction).",
            vec![
                ("action", prop("string", "one of: create, show, list, approve, reject, close, integrate")),
                ("title", prop("string", "create: proposal title")),
                ("change", prop("string", "create: change id")),
                ("rationale", prop("string", "create: rationale")),
                ("base", prop("string", "create: expected base snapshot")),
                ("evidence", json!({"type":"array","items":{"type":"string"},"description":"create: evidence oids"})),
                ("depends", json!({"type":"array","items":{"type":"string"},"description":"create: proposal ids this depends on"})),
                ("id", prop("string", "show/approve/reject/close/integrate: proposal id")),
                ("workspace", prop("string", "integrate: target workspace")),
            ],
            vec!["action"],
        ),
    ]
}

// ---------------------------------------------------------------------------
// tool dispatch → CLI argv
// ---------------------------------------------------------------------------

fn call_tool(id: Value, params: &Value, repo: Option<&std::path::Path>) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    if !args.is_object() {
        return rpc_err(id, -32602, "arguments must be an object");
    }
    let argv: Vec<String> = match build_argv(name, &args) {
        Ok(v) => v,
        Err(e) => return tool_result(id, Err(e)),
    };
    crate::obs::event("mcp_tool", &[("tool", json!(name)), ("argv", json!(argv))]);
    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    tool_result(id, call_json(repo, &argv_refs))
}

fn tool_result(id: Value, r: Result<Value>) -> Value {
    match r {
        Ok(data) => rpc_ok(
            id,
            json!({
                "content": [{ "type": "text", "text": data.to_string() }],
                "isError": false,
            }),
        ),
        Err(e) => {
            let env = json!({
                "ok": false,
                "error": { "category": e.category(), "message": e.to_string() },
            });
            rpc_ok(
                id,
                json!({
                    "content": [{ "type": "text", "text": env.to_string() }],
                    "isError": true,
                }),
            )
        }
    }
}

fn s(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}
fn b(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}
fn need(args: &Value, key: &str) -> Result<String> {
    s(args, key).ok_or_else(|| {
        Error::Invalid(format!(
            "missing required argument `{key}` (see tools/list schema)"
        ))
    })
}
fn i(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_i64())
        .map(|n| n.to_string())
}
fn arr(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn build_argv(name: &str, args: &Value) -> Result<Vec<String>> {
    // Layout rule (iteration-12 audit hardening): every BARE POSITIONAL
    // value (oid, spec, title, id, name) travels after a `--` separator, so
    // a value that happens to start with `-` can never be reinterpreted as
    // a flag by the CLI parser. Flag VALUES (--base x, -m x) are consumed
    // positionally by Args and are safe as-is. Action words stay directly
    // after the command family (family dispatch reads the raw token stream).
    let mut v: Vec<String> = vec![name.trim_start_matches("newgit_").to_string()];
    let mut pos: Vec<String> = Vec::new(); // appended after "--" at the end
    match name {
        "newgit_status" => {
            if let Some(w) = s(args, "workspace") {
                v.extend(["-w".into(), w]);
            }
        }
        "newgit_history" => {
            if let Some(f) = s(args, "from") {
                v.extend(["--from".into(), f]);
            }
            if let Some(n) = i(args, "limit") {
                v.extend(["-n".into(), n]);
            }
        }
        "newgit_cat" => {
            pos.push(need(args, "oid")?);
            if b(args, "raw") {
                v.push("--raw".into());
            }
        }
        "newgit_diff" => {
            if let Some(a) = s(args, "a") {
                pos.push(a);
                if let Some(bb) = s(args, "b") {
                    pos.push(bb);
                }
            } else if s(args, "b").is_some() {
                return Err(Error::Invalid("`b` requires `a`".into()));
            }
            if b(args, "name_only") {
                v.push("--name-only".into());
            }
            if let Some(c) = i(args, "context") {
                v.extend(["--context".into(), c]);
            }
            if b(args, "no_renames") {
                v.push("--no-renames".into());
            }
        }
        "newgit_snapshot" => {
            v.extend(["-m".into(), need(args, "message")?]);
            if let Some(w) = s(args, "workspace") {
                v.extend(["-w".into(), w]);
            }
        }
        "newgit_verify" => {
            if b(args, "deep") {
                v.push("--deep".into());
            }
        }
        "newgit_integrate" => {
            pos.push(need(args, "spec")?);
            if let Some(w) = s(args, "workspace") {
                v.extend(["-w".into(), w]);
            }
            if let Some(m) = s(args, "message") {
                v.extend(["-m".into(), m]);
            }
            if b(args, "no_renames") {
                v.push("--no-renames".into());
            }
        }
        "newgit_workspace" | "newgit_goal" | "newgit_change" | "newgit_evidence"
        | "newgit_evaluation" | "newgit_proposal" => {
            let action = need(args, "action")?;
            v.push(action.clone());
            match name {
                "newgit_workspace" => match action.as_str() {
                    "list" => {}
                    "create" => {
                        if let Some(base) = s(args, "base") {
                            v.extend(["--base".into(), base]);
                        }
                        pos.push(need(args, "name")?);
                    }
                    "show" => pos.push(need(args, "name")?),
                    "discard" => {
                        if b(args, "force") {
                            v.push("--force".into());
                        }
                        pos.push(need(args, "name")?);
                    }
                    other => return Err(bad_action(name, other)),
                },
                "newgit_goal" => match action.as_str() {
                    "list" => {}
                    "create" => {
                        if let Some(d) = s(args, "description") {
                            v.extend(["--description".into(), d]);
                        }
                        pos.push(need(args, "title")?);
                    }
                    "show" => pos.push(need(args, "id")?),
                    "set-status" => {
                        pos.push(need(args, "id")?);
                        pos.push(need(args, "status")?);
                    }
                    other => return Err(bad_action(name, other)),
                },
                "newgit_change" => match action.as_str() {
                    "list" => {
                        if let Some(g) = s(args, "goal") {
                            v.extend(["--goal".into(), g]);
                        }
                    }
                    "create" => {
                        v.extend(["--base".into(), need(args, "base")?]);
                        v.extend(["--result".into(), need(args, "result")?]);
                        if let Some(g) = s(args, "goal") {
                            v.extend(["--goal".into(), g]);
                        }
                        if let Some(d) = s(args, "description") {
                            v.extend(["--description".into(), d]);
                        }
                        pos.push(need(args, "title")?);
                    }
                    "show" => pos.push(need(args, "id")?),
                    "set-status" => {
                        pos.push(need(args, "id")?);
                        pos.push(need(args, "status")?);
                    }
                    "attach-evidence" => {
                        pos.push(need(args, "id")?);
                        pos.push(need(args, "evidence_oid")?);
                    }
                    other => return Err(bad_action(name, other)),
                },
                "newgit_evidence" => match action.as_str() {
                    "add" => {
                        v.extend(["--kind".into(), need(args, "kind")?]);
                        v.extend(["--verdict".into(), need(args, "verdict")?]);
                        if b(args, "deterministic") {
                            v.push("--deterministic".into());
                        }
                        if let Some(t) = s(args, "target") {
                            v.extend(["--target".into(), t]);
                        }
                        if let Some(o) = s(args, "output") {
                            v.extend(["--output".into(), o]);
                        }
                        if let Some(m) = s(args, "metrics") {
                            v.extend(["--metric".into(), m]);
                        }
                    }
                    "show" => pos.push(need(args, "oid")?),
                    "record" => {
                        if let Some(k) = s(args, "kind") {
                            v.extend(["--kind".into(), k]);
                        }
                        if let Some(t) = s(args, "target") {
                            v.extend(["--target".into(), t]);
                        }
                        if let Some(w) = s(args, "workspace") {
                            v.extend(["-w".into(), w]);
                        }
                        let cmd = arr(args, "command");
                        if cmd.is_empty() {
                            return Err(Error::Invalid(
                                "evidence record requires a non-empty `command` argv".into(),
                            ));
                        }
                        // the recorded command IS the positional tail
                        v.push("--".into());
                        v.extend(cmd);
                        return Ok(v);
                    }
                    other => return Err(bad_action(name, other)),
                },
                "newgit_evaluation" => match action.as_str() {
                    "create" => {
                        v.extend(["--target".into(), need(args, "target")?]);
                        v.extend(["--verdict".into(), need(args, "verdict")?]);
                        if b(args, "ai") {
                            v.push("--ai".into());
                        }
                        if let Some(d) = s(args, "dimensions") {
                            v.extend(["--dimension".into(), d]);
                        }
                    }
                    "from-evidence" => pos.push(need(args, "change_id")?),
                    "show" => pos.push(need(args, "oid")?),
                    other => return Err(bad_action(name, other)),
                },
                "newgit_proposal" => match action.as_str() {
                    "list" => {}
                    "create" => {
                        v.extend(["--change".into(), need(args, "change")?]);
                        if let Some(r) = s(args, "rationale") {
                            v.extend(["--rationale".into(), r]);
                        }
                        if let Some(bb) = s(args, "base") {
                            v.extend(["--base".into(), bb]);
                        }
                        let ev = arr(args, "evidence");
                        if !ev.is_empty() {
                            v.extend(["--evidence".into(), ev.join(",")]);
                        }
                        let dep = arr(args, "depends");
                        if !dep.is_empty() {
                            v.extend(["--depends".into(), dep.join(",")]);
                        }
                        pos.push(need(args, "title")?);
                    }
                    "show" | "approve" | "reject" | "close" => pos.push(need(args, "id")?),
                    "integrate" => {
                        if let Some(w) = s(args, "workspace") {
                            v.extend(["-w".into(), w]);
                        }
                        pos.push(need(args, "id")?);
                    }
                    other => return Err(bad_action(name, other)),
                },
                _ => unreachable!(),
            }
        }
        other => {
            return Err(Error::Invalid(format!(
                "unknown tool {other:?}; call tools/list for the catalog"
            )))
        }
    }
    if !pos.is_empty() {
        v.push("--".into());
        v.extend(pos);
    }
    Ok(v)
}

fn bad_action(tool: &str, action: &str) -> Error {
    Error::Invalid(format!(
        "{tool}: unsupported action {action:?} (see tools/list schema)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(msg: Value, repo: Option<&std::path::Path>) -> Value {
        handle_line(&msg.to_string(), repo).expect("response expected")
    }

    #[test]
    fn jsonrpc_handshake_and_catalog() {
        let init = line(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}),
            None,
        );
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(init["result"]["serverInfo"]["name"], "newgit");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        // notifications get NO response
        assert!(handle_line(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            None
        )
        .is_none());
        let ping = line(json!({"jsonrpc":"2.0","id":2,"method":"ping"}), None);
        assert_eq!(ping["result"], json!({}));
        let list = line(json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}), None);
        let tools = list["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 13);
        for t in tools {
            assert!(t["name"].as_str().unwrap().starts_with("newgit_"));
            assert_eq!(t["inputSchema"]["type"], "object");
            assert_eq!(t["inputSchema"]["additionalProperties"], false);
        }
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for must in [
            "newgit_status",
            "newgit_history",
            "newgit_cat",
            "newgit_diff",
            "newgit_snapshot",
            "newgit_verify",
            "newgit_integrate",
            "newgit_workspace",
            "newgit_goal",
            "newgit_change",
            "newgit_evidence",
            "newgit_evaluation",
            "newgit_proposal",
        ] {
            assert!(names.contains(&must), "missing tool {must}");
        }
    }

    #[test]
    fn protocol_errors_are_proper_jsonrpc() {
        // parse error
        let e = handle_line("{not json", None).unwrap();
        assert_eq!(e["error"]["code"], -32700);
        // batch
        let e = handle_line("[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]", None).unwrap();
        assert_eq!(e["error"]["code"], -32600);
        // bad version
        let e = line(json!({"jsonrpc":"1.0","id":1,"method":"ping"}), None);
        assert_eq!(e["error"]["code"], -32600);
        // unknown method
        let e = line(json!({"jsonrpc":"2.0","id":1,"method":"nope"}), None);
        assert_eq!(e["error"]["code"], -32601);
        // unknown method as notification: silent
        assert!(handle_line(r#"{"jsonrpc":"2.0","method":"nope"}"#, None).is_none());
        // unknown tool → tool-level error result (isError), not protocol error
        let r = line(
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"newgit_nope","arguments":{}}}),
            None,
        );
        assert_eq!(r["result"]["isError"], true);
        let body: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"]["category"], "invalid");
        // non-object arguments → -32602
        let e = line(
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"newgit_status","arguments":5}}),
            None,
        );
        assert_eq!(e["error"]["code"], -32602);
    }

    #[test]
    fn argv_building_matches_cli_syntax() {
        let cases: Vec<(Value, Vec<&str>)> = vec![
            (json!({"oid":"ab"}), vec!["cat", "--", "ab"]),
            (
                json!({"message":"m","workspace":"w1"}),
                vec!["snapshot", "-m", "m", "-w", "w1"],
            ),
            (
                json!({"from":"refs/main","limit":5}),
                vec!["history", "--from", "refs/main", "-n", "5"],
            ),
            (
                json!({"a":"x","b":"y","no_renames":true}),
                vec!["diff", "--no-renames", "--", "x", "y"],
            ),
            (
                json!({"action":"create","title":"t","base":"b","result":"r","goal":"g"}),
                vec![
                    "change", "create", "--base", "b", "--result", "r", "--goal", "g", "--", "t",
                ],
            ),
            (
                json!({"action":"record","kind":"unit_test","command":["cargo","test"]}),
                vec![
                    "evidence",
                    "record",
                    "--kind",
                    "unit_test",
                    "--",
                    "cargo",
                    "test",
                ],
            ),
            (
                json!({
                    "action":"create","title":"p","change":"c1",
                    "evidence":["e1","e2"],"depends":["d1"]
                }),
                vec![
                    "proposal",
                    "create",
                    "--change",
                    "c1",
                    "--evidence",
                    "e1,e2",
                    "--depends",
                    "d1",
                    "--",
                    "p",
                ],
            ),
            (
                json!({"action":"integrate","id":"p1","workspace":"w"}),
                vec!["proposal", "integrate", "-w", "w", "--", "p1"],
            ),
            (
                json!({"action":"create","target":"t","verdict":"pass","ai":true}),
                vec![
                    "evaluation",
                    "create",
                    "--target",
                    "t",
                    "--verdict",
                    "pass",
                    "--ai",
                ],
            ),
            // audit hardening: a positional value that looks like a flag is
            // inert because it travels after `--`
            (json!({"oid":"--raw"}), vec!["cat", "--", "--raw"]),
            (
                json!({"action":"create","title":"--force","base":"b","result":"r"}),
                vec![
                    "change", "create", "--base", "b", "--result", "r", "--", "--force",
                ],
            ),
            (
                json!({"spec":"--no-renames"}),
                vec!["integrate", "--", "--no-renames"],
            ),
        ];
        for (i, (args, want)) in cases.into_iter().enumerate() {
            let names = [
                "newgit_cat",
                "newgit_snapshot",
                "newgit_history",
                "newgit_diff",
                "newgit_change",
                "newgit_evidence",
                "newgit_proposal",
                "newgit_proposal",
                "newgit_evaluation",
                "newgit_cat",
                "newgit_change",
                "newgit_integrate",
            ];
            let got = build_argv(names[i], &args).unwrap();
            assert_eq!(
                got,
                want.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "case {i}"
            );
        }
        // missing required → Invalid naming the argument
        let e = build_argv("newgit_cat", &json!({})).unwrap_err();
        assert!(e.to_string().contains("oid"), "{e}");
        // bad action
        let e = build_argv("newgit_goal", &json!({"action":"frobnicate"})).unwrap_err();
        assert!(e.to_string().contains("frobnicate"), "{e}");
        // record without command
        assert!(build_argv("newgit_evidence", &json!({"action":"record"})).is_err());
        // b without a
        assert!(build_argv("newgit_diff", &json!({"b":"y"})).is_err());
    }

    #[test]
    fn tool_call_end_to_end_on_a_real_repo() {
        let dir = std::env::temp_dir().join(format!(
            "ngmcp-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        call_json(None, &["init", dir.to_str().unwrap()]).unwrap();
        std::fs::write(dir.join("f.txt"), b"hello mcp\n").unwrap();
        // snapshot via MCP path
        let r = line(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"newgit_snapshot","arguments":{"message":"mcp s1"}}}),
            Some(&dir),
        );
        assert_eq!(r["result"]["isError"], false, "{r}");
        // status via MCP
        let r = line(
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"newgit_status","arguments":{}}}),
            Some(&dir),
        );
        assert_eq!(r["result"]["isError"], false);
        let body: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["clean"], true);
        // history shows the snapshot
        let r = line(
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"newgit_history","arguments":{}}}),
            Some(&dir),
        );
        let body: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(body.to_string().contains("mcp s1"), "{body}");
        // failing tool call → isError with category
        let r = line(
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"newgit_cat","arguments":{"oid":"zz"}}}),
            Some(&dir),
        );
        assert_eq!(r["result"]["isError"], true);
        let body: Value =
            serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["ok"], false);
        assert!(!body["error"]["category"].as_str().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
