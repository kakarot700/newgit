//! NewGit remote server — HTTP/1.1 + JSON protocol v1 (std only, D-002).
//!
//! Thread-per-connection with a bounded pool; every connection reopens the
//! repo (cheap: recovery is idempotent, the txn lock serializes ref writes).
//! Auth is bearer-token (SHA-256 at rest, roles read<write<admin); every
//! request is audit-logged. All limits are enforced BEFORE allocation
//! (Content-Length cap, batch cap, body cap). No TLS in v1 — run behind a
//! reverse proxy for encryption (documented in docs/PROTOCOL.md).

use std::collections::HashSet;
use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::object::envelope;
use crate::object::types::Object;
use crate::object::ObjectId;
use crate::ops::verify;
use crate::remote::audit::AuditLog;
use crate::remote::auth::{self, Principal, Role, TokenFile};
use crate::remote::http::{self, Request};
use crate::remote::negotiate;
use crate::remote::proto::*;
use crate::repo::txn::{self, Cas, RefLogEntry, TxnOp};
use crate::repo::Repo;
use crate::util::base64;

pub const DEFAULT_BIND: &str = "127.0.0.1:8787";
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// `host:port` (port 0 ⇒ ephemeral; actual port via handle.addr()).
    pub bind: String,
    pub repo_root: PathBuf,
    /// JSON token file (see remote::auth). Missing/empty ⇒ no tokens.
    pub token_file: PathBuf,
    /// Allow unauthenticated READ endpoints (info is always anonymous).
    pub allow_anonymous_read: bool,
    /// Hard cap on request body bytes (Content-Length checked pre-read).
    pub max_body: u64,
    /// Max concurrent connection threads; beyond ⇒ 503.
    pub max_threads: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: DEFAULT_BIND.into(),
            repo_root: PathBuf::from("."),
            token_file: PathBuf::from(".newgit/tokens.json"),
            allow_anonymous_read: false,
            max_body: 64 * 1024 * 1024,
            max_threads: 32,
        }
    }
}

/// A running server (used by tests and by `newgit serve`).
#[derive(Debug)]
pub struct ServerHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
    /// Signal the accept loop to stop and wait for it (in-flight connection
    /// threads finish on their own; they are bounded by IO_TIMEOUT).
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Bind and spawn the accept loop; returns immediately.
pub fn spawn(cfg: ServerConfig) -> Result<ServerHandle> {
    let listener = TcpListener::bind(&cfg.bind)
        .map_err(|e| Error::Config(format!("cannot bind {}: {e}", cfg.bind)))?;
    let addr = listener
        .local_addr()
        .map_err(|e| Error::Bug(format!("local_addr: {e}")))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| Error::Bug(format!("set_nonblocking: {e}")))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let join = std::thread::Builder::new()
        .name("newgit-serve-accept".into())
        .spawn(move || accept_loop(listener, cfg, stop2))
        .map_err(|e| Error::Bug(format!("spawn accept loop: {e}")))?;
    Ok(ServerHandle {
        addr,
        stop,
        join: Some(join),
    })
}

fn accept_loop(listener: TcpListener, cfg: ServerConfig, stop: Arc<AtomicBool>) {
    let active = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                if active.load(Ordering::Relaxed) >= cfg.max_threads {
                    let mut w = BufWriter::new(stream);
                    let body = envelope_err(429, "limit", "server busy: too many connections");
                    let _ = http::write_response(&mut w, 429, "application/json", &body);
                    continue;
                }
                active.fetch_add(1, Ordering::Relaxed);
                let cfg = cfg.clone();
                let active_conn = active.clone();
                let res = std::thread::Builder::new()
                    .name("newgit-serve-conn".into())
                    .spawn(move || {
                        handle_conn(stream, &cfg);
                        active_conn.fetch_sub(1, Ordering::Relaxed);
                    });
                if res.is_err() {
                    active.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break, // listener dead
        }
    }
}

fn envelope_ok(data: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "ok": true, "data": data })).unwrap_or_default()
}

fn envelope_err(status_hint: u16, category: &str, message: &str) -> Vec<u8> {
    let _ = status_hint;
    serde_json::to_vec(&json!({
        "ok": false,
        "error": { "category": category, "message": message }
    }))
    .unwrap_or_default()
}

fn handle_conn(stream: TcpStream, cfg: &ServerConfig) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let out_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut r = BufReader::new(stream);
    let mut w = BufWriter::new(out_stream);

    // Repo + tokens are loaded per connection (cheap, always fresh).
    let repo = match Repo::open(&cfg.repo_root) {
        Ok(repo) => repo,
        Err(e) => {
            let body = envelope_err(500, e.category(), &e.to_string());
            let _ = http::write_response(&mut w, 500, "application/json", &body);
            return;
        }
    };
    let tokens = auth::load(&cfg.token_file).unwrap_or_default();
    let audit = AuditLog::new(repo.ng());

    let req = match http::read_request(&mut r, cfg.max_body) {
        Ok(req) => req,
        Err((status, msg)) => {
            let category = if status == 413 { "limit" } else { "protocol" };
            audit.append("pre-auth", "?", &msg, status, Some(category));
            let body = envelope_err(status, category, &msg);
            let _ = http::write_response(&mut w, status, "application/json", &body);
            return;
        }
    };
    crate::obs::event(
        "remote_request",
        &[
            ("peer", json!(peer)),
            ("method", json!(req.method)),
            ("path", json!(req.path)),
        ],
    );

    let (status, body, principal_id, category) = route(&repo, &tokens, cfg, &req);
    audit.append(
        &principal_id,
        &req.method,
        &req.path,
        status,
        category.as_deref(),
    );
    let _ = http::write_response(&mut w, status, "application/json", &body);
}

/// Route one request. Returns (status, body, principal-id, error-category).
fn route(
    repo: &Repo,
    tokens: &TokenFile,
    cfg: &ServerConfig,
    req: &Request,
) -> (u16, Vec<u8>, String, Option<String>) {
    // Authenticate once; a bad token is a hard 401 for EVERY endpoint
    // (including anonymous ones — presenting invalid credentials is an
    // error, not a downgrade).
    let principal = match tokens.authenticate(req.header("authorization")) {
        Ok(p) => p,
        Err(e) => {
            return (
                http::status_for_error(&e),
                envelope_err(401, e.category(), &e.to_string()),
                "bad-token".into(),
                Some(e.category().to_string()),
            )
        }
    };
    let who = principal
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "anonymous".into());

    let known_paths: &[&str] = &[
        "/healthz",
        "/v1/info",
        "/v1/refs",
        "/v1/have",
        "/v1/negotiate",
        "/v1/objects/get",
        "/v1/objects/put",
        "/v1/refs/update",
        "/v1/audit",
    ];
    if !known_paths.contains(&req.path.as_str()) {
        return (
            404,
            envelope_err(404, "protocol", &format!("unknown endpoint {}", req.path)),
            who,
            Some("protocol".into()),
        );
    }

    // Per-endpoint (method, role) policy.
    let policy: (&str, Role) = match req.path.as_str() {
        "/healthz" => ("GET", Role::Read), // anonymous allowed below
        "/v1/info" => ("GET", Role::Read), // anonymous allowed below
        "/v1/refs" | "/v1/have" | "/v1/negotiate" | "/v1/objects/get" => ("", Role::Read),
        "/v1/objects/put" | "/v1/refs/update" => ("POST", Role::Write),
        "/v1/audit" => ("GET", Role::Admin),
        _ => unreachable!("known_paths checked above"),
    };
    let anonymous_ok = matches!(req.path.as_str(), "/healthz" | "/v1/info")
        || (cfg.allow_anonymous_read && policy.1 == Role::Read);
    match (&req.method[..], principal.as_ref()) {
        ("GET", _) if policy.0.is_empty() || policy.0 == "GET" => {}
        ("POST", _) if policy.0.is_empty() || policy.0 == "POST" => {}
        _ => {
            return (
                405,
                envelope_err(
                    405,
                    "protocol",
                    &format!("{} not allowed on {}", req.method, req.path),
                ),
                who,
                Some("protocol".into()),
            )
        }
    }
    if !anonymous_ok && !auth::authorize(principal.as_ref(), policy.1) {
        let status = if principal.is_none() { 401 } else { 403 };
        return (
            status,
            envelope_err(
                status,
                "auth",
                &format!("{} requires role {:?} or higher", req.path, policy.1),
            ),
            who,
            Some("auth".into()),
        );
    }

    let result = dispatch(repo, cfg, req, principal.as_ref());
    match result {
        Ok(data) => (200, envelope_ok(data), who, None),
        Err(e) => {
            let status = http::status_for_error(&e);
            (
                status,
                envelope_err(status, e.category(), &e.to_string()),
                who,
                Some(e.category().to_string()),
            )
        }
    }
}

fn dispatch(
    repo: &Repo,
    cfg: &ServerConfig,
    req: &Request,
    principal: Option<&Principal>,
) -> Result<Value> {
    let limits = repo.limits();
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/healthz") => Ok(json!("healthy")),
        ("GET", "/v1/info") => {
            let head = head_string(repo)?;
            Ok(serde_json::to_value(InfoData {
                product: "newgit".into(),
                version: crate::VERSION.into(),
                protocol: PROTOCOL_VERSION,
                head,
                capabilities: vec![
                    "have".into(),
                    "negotiate".into(),
                    "objects-get".into(),
                    "objects-put".into(),
                    "refs-update".into(),
                    "audit".into(),
                ],
                limits: LimitsInfo {
                    max_batch_objects: limits.max_batch_objects,
                    max_request_bytes: cfg.max_body.min(limits.max_request_bytes),
                },
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("GET", "/v1/refs") => {
            let all = repo.refs.list(None)?;
            let refs: Vec<RefEntry> = all
                .into_iter()
                .filter(|(name, _)| !is_internal_ref(name))
                .map(|(name, oid)| RefEntry {
                    name,
                    oid: oid.to_hex(),
                })
                .collect();
            Ok(serde_json::to_value(RefsData {
                head: head_string(repo)?,
                refs,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/have") => {
            let r: HaveReq = body_json(req)?;
            let oids = parse_oid_batch(&r.oids, limits.max_batch_objects)?;
            let have: Vec<String> = oids
                .into_iter()
                .filter(|o| repo.objects.contains(o))
                .map(|o| o.to_hex())
                .collect();
            Ok(serde_json::to_value(HaveData { have }).map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/negotiate") => {
            let r: NegotiateReq = body_json(req)?;
            let batch_cap = limits.max_batch_objects.max(10_000);
            let have = parse_oid_batch(&r.have, batch_cap)?;
            let want = parse_oid_batch(&r.want, batch_cap)?;
            // Haves the server does not store are ignored (superset
            // semantics — the client may receive extra objects, never
            // fewer than needed).
            let have_stored: Vec<ObjectId> = have
                .into_iter()
                .filter(|o| repo.objects.contains(o))
                .collect();
            for w in &want {
                if !repo.objects.contains(w) {
                    return Err(Error::NotFound(*w));
                }
            }
            let exclude = negotiate::closure(repo, &have_stored);
            let exclude: HashSet<ObjectId> = exclude.into_iter().collect();
            let send = negotiate::post_order(repo, &want, &exclude)?;
            Ok(serde_json::to_value(NegotiateData {
                send: send.iter().map(|o| o.to_hex()).collect(),
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/objects/get") => {
            let r: ObjectsGetReq = body_json(req)?;
            let ids = parse_oid_batch(&r.ids, limits.max_batch_objects)?;
            let mut objects = Vec::with_capacity(ids.len());
            for oid in &ids {
                let raw =
                    std::fs::read(repo.objects.path_for(oid)).map_err(|_| Error::NotFound(*oid))?;
                objects.push(ObjectWire {
                    data_b64: base64::encode(&raw),
                });
            }
            Ok(serde_json::to_value(ObjectsData { objects })
                .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/objects/put") => {
            let r: ObjectsPutReq = body_json(req)?;
            if r.objects.len() > limits.max_batch_objects {
                return Err(Error::Limit(format!(
                    "batch of {} objects exceeds max_batch_objects {}",
                    r.objects.len(),
                    limits.max_batch_objects
                )));
            }
            let mut stored = Vec::new();
            for (i, obj) in r.objects.iter().enumerate() {
                let bytes = base64::decode(&obj.data_b64)
                    .map_err(|e| Error::Malformed(format!("object {i}: bad base64: {e}")))?;
                // Self-verifying envelope: digest + id checked here...
                let (canonical, oid) = envelope::verify(&bytes, limits.max_object_bytes)
                    .map_err(|e| Error::Malformed(format!("object {i}: {e}")))?;
                // ...and the link-closure invariant: dependencies must
                // already be stored (batches are dependency-ordered; an
                // earlier object in THIS batch counts as stored).
                let decoded = Object::from_canonical(&canonical)
                    .map_err(|e| Error::Malformed(format!("object {i}: {e}")))?;
                for link in verify::object_links(&decoded) {
                    if !repo.objects.contains(&link) {
                        return Err(Error::Protocol(format!(
                            "object {i} ({oid}): missing dependency {link} — \
                             send dependencies first"
                        )));
                    }
                }
                let got = repo.objects.put_canonical(&canonical)?;
                if got != oid {
                    return Err(Error::Bug(format!("object {i}: put_canonical id mismatch")));
                }
                stored.push(oid.to_hex());
            }
            Ok(serde_json::to_value(PutData {
                stored: stored.len(),
                oids: stored,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("POST", "/v1/refs/update") => {
            let r: RefsUpdateReq = body_json(req)?;
            if r.updates.is_empty() {
                return Err(Error::Invalid("refs/update with zero updates".into()));
            }
            if r.updates.len() > limits.max_batch_objects {
                return Err(Error::Limit("too many ref updates in one request".into()));
            }
            let who = principal.map(|p| p.id.as_str()).unwrap_or("anonymous");
            let mut ops = Vec::with_capacity(r.updates.len());
            let mut names = Vec::new();
            for (i, u) in r.updates.iter().enumerate() {
                crate::repo::refs::check_ref_name_system(&u.name)
                    .map_err(|e| Error::InvalidRef(format!("update {i} ({}): {e}", u.name)))?;
                if is_internal_ref(&u.name) {
                    return Err(Error::InvalidRef(format!(
                        "update {i}: {} is a NewGit-internal namespace and cannot be updated remotely",
                        u.name
                    )));
                }
                let cas =
                    match &u.cas {
                        CasWire::Any => Cas::Any,
                        CasWire::Exactly { old } => Cas::Exactly(match old {
                            None => None,
                            Some(hex) => Some(ObjectId::from_hex(hex).map_err(|e| {
                                Error::InvalidRef(format!("update {i} cas.old: {e}"))
                            })?),
                        }),
                    };
                let new = match &u.new {
                    None => None,
                    Some(hex) => {
                        let oid = ObjectId::from_hex(hex)
                            .map_err(|e| Error::InvalidRef(format!("update {i} new: {e}")))?;
                        if !repo.objects.contains(&oid) {
                            return Err(Error::Protocol(format!(
                                "update {i}: target {oid} is not stored on the server \
                                 (upload objects before moving refs)"
                            )));
                        }
                        Some(oid)
                    }
                };
                let mut msg = format!("remote update by {who}");
                if let Some(extra) = &u.message {
                    msg.push_str(": ");
                    msg.push_str(extra);
                }
                ops.push(TxnOp::Ref {
                    name: u.name.clone(),
                    cas,
                    new,
                    log: RefLogEntry::system(msg),
                });
                names.push(u.name.clone());
            }
            // ONE transaction: any CAS failure ⇒ nothing moves (409).
            let report = txn::execute(repo.ng(), ops, limits)?;
            Ok(serde_json::to_value(UpdateData {
                updated: names,
                txn_id: report.txn_id,
            })
            .map_err(|e| Error::Bug(e.to_string()))?)
        }
        ("GET", "/v1/audit") => {
            let limit: usize = req
                .query_get("limit")
                .map(|s| s.parse().unwrap_or(100))
                .unwrap_or(100)
                .min(10_000);
            let entries = AuditLog::new(repo.ng()).tail(limit)?;
            Ok(serde_json::to_value(AuditData { entries })
                .map_err(|e| Error::Bug(e.to_string()))?)
        }
        _ => Err(Error::Protocol(format!(
            "{} {} not supported",
            req.method, req.path
        ))),
    }
}

fn head_string(repo: &Repo) -> Result<String> {
    Ok(match repo.read_head()? {
        crate::repo::Head::Symbolic(name) => format!("ref: {name}"),
        crate::repo::Head::Detached(oid) => oid.to_hex(),
    })
}

fn body_json<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T> {
    if req.body.is_empty() {
        return Err(Error::Invalid("request body required".into()));
    }
    serde_json::from_slice(&req.body)
        .map_err(|e| Error::Malformed(format!("invalid JSON body: {e}")))
}

fn parse_oid_batch(hexes: &[String], cap: usize) -> Result<Vec<ObjectId>> {
    if hexes.len() > cap {
        return Err(Error::Limit(format!(
            "batch of {} exceeds cap {cap}",
            hexes.len()
        )));
    }
    hexes
        .iter()
        .map(|h| ObjectId::from_hex(h).map_err(|e| Error::Malformed(format!("oid {h:?}: {e}"))))
        .collect()
}

/// Convenience for tests/CLI: spawn + a writable "listening" announcement.
pub fn listening_line(addr: SocketAddr) -> String {
    format!("newgit serve: listening on http://{addr} (protocol v{PROTOCOL_VERSION})")
}
